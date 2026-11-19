use std::collections::HashMap;
use std::process::ExitCode;

use bridge::config::{Config, Env, HealthConfig, RevokeConfig};
use bridge::{app, health, logging, shutdown, timeutil};
use tokio_util::sync::CancellationToken;

const USAGE: &str = "usage: bridge [run|check-config|healthcheck|revoke-paperclip-key]";

/// Exit codes: 0 — ok, 1 — runtime failure or unhealthy, 2 — invalid configuration or usage.
fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None | Some("run") => run(),
        Some("check-config") => check_config(),
        Some("healthcheck") => healthcheck(),
        Some("revoke-paperclip-key") => revoke_paperclip_key(),
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
            logging::install_tls_provider();
            // Builds the clients without any network call: proves the proxy
            // URL is usable by the HTTP stack.
            if let Err(e) =
                bridge::telegram::HttpTelegramClient::new(&cfg.telegram).and_then(|_| {
                    bridge::paperclip::HttpPaperclipClient::from_config(&cfg.paperclip).map(|_| ())
                })
            {
                eprintln!("configuration error: {e}");
                return ExitCode::from(2);
            }
            for var in &cfg.ignored_vars {
                println!("warning: {var} is no longer used in v2, remove it");
            }
            println!(
                "configuration OK: {} Paperclip key{}, {} allowed Telegram user{}, telegram proxy scheme {}",
                cfg.paperclip.tokens.len(),
                if cfg.paperclip.tokens.len() == 1 {
                    ""
                } else {
                    "s"
                },
                cfg.telegram.allowed_user_ids.len(),
                if cfg.telegram.allowed_user_ids.len() == 1 {
                    ""
                } else {
                    "s"
                },
                cfg.telegram.proxy.scheme()
            );
            ExitCode::SUCCESS
        }
        Err(code) => code,
    }
}

/// Revokes the board key from `PAPERCLIP_TOKEN_FILE` via
/// `POST /api/cli-auth/revoke-current` (runbook: leak or suspicion).
fn revoke_paperclip_key() -> ExitCode {
    let vars: HashMap<String, String> = std::env::vars().collect();
    let cfg = match RevokeConfig::from_env(&Env::new(&vars)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("configuration error: {e}");
            return ExitCode::from(2);
        }
    };
    logging::install_tls_provider();
    let client = match bridge::paperclip::HttpPaperclipClient::new(
        &cfg.url,
        cfg.token,
        cfg.connect_timeout,
        cfg.request_timeout,
    ) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: cannot start async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    use bridge::paperclip::PaperclipApi;
    match rt.block_on(client.revoke_current_key()) {
        Ok(()) => {
            println!(
                "Paperclip key revoked. Create a new key and update the secret file before restarting the bridge."
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!(
                "revocation failed: {} — revoke the key in the Paperclip UI",
                e.class()
            );
            ExitCode::FAILURE
        }
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
            version = env!("CARGO_PKG_VERSION"),
            paperclip_keys = cfg.paperclip.tokens.len(),
            allowed_users = cfg.telegram.allowed_user_ids.len(),
            proxy_scheme = cfg.telegram.proxy.scheme(),
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
