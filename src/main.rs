use std::collections::HashMap;
use std::process::ExitCode;

use bridge::config::{Config, Env, HealthConfig};
use bridge::{app, health, logging, shutdown, timeutil};
use tokio_util::sync::CancellationToken;

const USAGE: &str = "usage: bridge [run|check-config|healthcheck]";

/// Exit codes: 0 — ok, 1 — runtime failure or unhealthy, 2 — invalid configuration or usage.
fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None | Some("run") => run(),
        Some("check-config") => check_config(),
        Some("healthcheck") => healthcheck(),
        Some("-h" | "--help" | "help") => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some(_) => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn load_config() -> Result<Config, ExitCode> {
    Config::from_process_env().map_err(|e| {
        eprintln!("configuration error: {e}");
        ExitCode::from(2)
    })
}

fn check_config() -> ExitCode {
    match load_config() {
        Ok(cfg) => {
            println!(
                "configuration OK: {} compan{}, telegram proxy scheme {}",
                cfg.paperclip.company_ids.len(),
                if cfg.paperclip.company_ids.len() == 1 {
                    "y"
                } else {
                    "ies"
                },
                cfg.telegram.proxy.scheme()
            );
            ExitCode::SUCCESS
        }
        Err(code) => code,
    }
}

fn healthcheck() -> ExitCode {
    let vars: HashMap<String, String> = std::env::vars().collect();
    let cfg = match HealthConfig::from_env(&Env::new(&vars)) {
        Ok(c) => c,
        Err(e) => {
            println!("unhealthy: configuration error: {e}");
            return ExitCode::FAILURE;
        }
    };
    match health::check(&cfg.file, cfg.max_stale, timeutil::now()) {
        Ok(()) => {
            println!("healthy");
            ExitCode::SUCCESS
        }
        Err(reason) => {
            println!("unhealthy: {reason}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> ExitCode {
    let cfg = match load_config() {
        Ok(c) => c,
        Err(code) => return code,
    };
    logging::init(&cfg.log);
    logging::install_tls_provider();
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!(error = %e, "cannot start async runtime");
            return ExitCode::FAILURE;
        }
    };
    rt.block_on(async move {
        let token = CancellationToken::new();
        if let Err(e) = shutdown::install(token.clone()) {
            tracing::error!(error = %e, "cannot install signal handlers");
            return ExitCode::FAILURE;
        }
        tracing::info!(
            companies = cfg.paperclip.company_ids.len(),
            proxy_scheme = cfg.telegram.proxy.scheme(),
            governed_enabled = cfg.governed.enabled,
            destructive_mode = ?cfg.governed.destructive_mode,
            "bridge starting"
        );
        match app::run(cfg, token).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                tracing::error!(error = %e, "bridge failed");
                ExitCode::FAILURE
            }
        }
    })
}
