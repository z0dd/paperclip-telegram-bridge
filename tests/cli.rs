//! CLI behaviour: exit codes of `check-config` and `healthcheck`.

use assert_cmd::Command;
use bridge::health::{HealthState, write_file};
use bridge::timeutil;

fn bridge() -> Command {
    let mut cmd = Command::cargo_bin("bridge").unwrap();
    cmd.env_clear();
    cmd
}

struct Secrets {
    dir: tempfile::TempDir,
}

impl Secrets {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pc"), "pcp_board_placeholder\n").unwrap();
        std::fs::write(dir.path().join("tg"), "123456:placeholder\n").unwrap();
        Self { dir }
    }

    fn apply(&self, cmd: &mut Command) {
        cmd.env("PAPERCLIP_URL", "http://paperclip.internal:3100")
            .env("PAPERCLIP_TOKEN_FILE", self.dir.path().join("pc"))
            .env("TELEGRAM_BOT_TOKEN_FILE", self.dir.path().join("tg"))
            .env("TELEGRAM_CHAT_ID", "-1001234567890")
            .env("TELEGRAM_ALLOWED_USER_IDS", "123456789");
    }
}

#[test]
fn check_config_fails_without_proxy() {
    let s = Secrets::new();
    let mut cmd = bridge();
    s.apply(&mut cmd);
    let out = cmd.arg("check-config").assert().code(2);
    let stderr = String::from_utf8_lossy(&out.get_output().stderr).into_owned();
    assert!(stderr.contains("TELEGRAM_PROXY_URL"), "{stderr}");
}

#[test]
fn run_fails_without_proxy_before_any_network() {
    let s = Secrets::new();
    let mut cmd = bridge();
    s.apply(&mut cmd);
    cmd.arg("run").timeout(std::time::Duration::from_secs(10));
    cmd.assert().code(2);
}

#[test]
fn check_config_ok_does_not_print_secrets() {
    let s = Secrets::new();
    let mut cmd = bridge();
    s.apply(&mut cmd);
    cmd.env("TELEGRAM_PROXY_URL", "socks5h://user:hunter2@xray:1080");
    let out = cmd.arg("check-config").assert().success();
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.get_output().stdout),
        String::from_utf8_lossy(&out.get_output().stderr)
    );
    assert!(all.contains("socks5h"), "{all}");
    for secret in ["hunter2", "xray", "placeholder", "paperclip.internal"] {
        assert!(!all.contains(secret), "{secret} leaked: {all}");
    }
}

#[test]
fn v1_configuration_is_refused_with_a_migration_hint() {
    let s = Secrets::new();
    for (var, hint) in [
        ("PAPERCLIP_COMPANY_IDS", "discovered automatically"),
        ("TELEGRAM_USER_ID", "TELEGRAM_ALLOWED_USER_IDS"),
    ] {
        let mut cmd = bridge();
        s.apply(&mut cmd);
        cmd.env("TELEGRAM_PROXY_URL", "socks5h://xray:1080")
            .env(var, "123");
        let out = cmd.arg("check-config").assert().code(2);
        let stderr = String::from_utf8_lossy(&out.get_output().stderr).into_owned();
        assert!(stderr.contains(var) && stderr.contains(hint), "{stderr}");
    }
}

#[test]
fn obsolete_v1_variables_only_warn() {
    let s = Secrets::new();
    let mut cmd = bridge();
    s.apply(&mut cmd);
    cmd.env("TELEGRAM_PROXY_URL", "socks5h://xray:1080")
        .env("NOTIFY_RUN_FAILURES", "true");
    let out = cmd.arg("check-config").assert().success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(stdout.contains("NOTIFY_RUN_FAILURES"), "{stdout}");
    assert!(stdout.contains("configuration OK"), "{stdout}");
}

#[test]
fn unknown_subcommand_is_usage_error() {
    bridge().arg("frobnicate").assert().code(2);
}

#[test]
fn healthcheck_exit_codes() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("health.json");

    // No file yet → unhealthy.
    bridge()
        .env("HEALTH_FILE", &file)
        .arg("healthcheck")
        .assert()
        .code(1);

    let now = timeutil::now_string();
    let mut state = HealthState {
        pid: 1,
        started_at: now.clone(),
        written_at: now.clone(),
        last_telegram_ok_at: Some(now.clone()),
        last_paperclip_ok_at: Some(now),
        last_discovery_ok_at: None,
        paperclip_auth_ok: true,
        telegram_group_ok: true,
        telegram_send_ok: true,
        agent_chat_ok: true,
        queue_depth: 0,
    };
    write_file(&file, &state).unwrap();
    bridge()
        .env("HEALTH_FILE", &file)
        .arg("healthcheck")
        .assert()
        .success();

    state.written_at = timeutil::format(timeutil::now() - time::Duration::minutes(5));
    write_file(&file, &state).unwrap();
    let out = bridge()
        .env("HEALTH_FILE", &file)
        .arg("healthcheck")
        .assert()
        .code(1);
    assert!(String::from_utf8_lossy(&out.get_output().stdout).contains("stale"));
}

fn revoke_cmd(secrets: &Secrets, url: &str) -> Command {
    let mut cmd = bridge();
    cmd.env("PAPERCLIP_URL", url)
        .env("PAPERCLIP_TOKEN_FILE", secrets.dir.path().join("pc"))
        .arg("revoke-paperclip-key");
    cmd
}

#[tokio::test(flavor = "multi_thread")]
async fn revoke_paperclip_key_calls_revoke_current() {
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/cli-auth/revoke-current"))
        .and(header("authorization", "Bearer pcp_board_placeholder"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"revoked": true})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let s = Secrets::new();
    let mut cmd = revoke_cmd(&s, &server.uri());
    let out = tokio::task::spawn_blocking(move || cmd.assert().success().get_output().clone())
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("revoked"));
    // Only the revoke config is needed: no Telegram variables were set.
}

#[tokio::test(flavor = "multi_thread")]
async fn revoke_failure_is_reported_without_secrets() {
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(401).set_body_json(serde_json::json!({"error": "Unauthorized"})),
        )
        .mount(&server)
        .await;
    let s = Secrets::new();
    let mut cmd = revoke_cmd(&s, &server.uri());
    let out = tokio::task::spawn_blocking(move || cmd.assert().code(1).get_output().clone())
        .await
        .unwrap();
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(all.contains("auth_401"), "{all}");
    assert!(!all.contains("pcp_board_placeholder"));
    assert!(!all.contains("127.0.0.1"));
}
