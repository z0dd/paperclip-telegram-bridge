//! The real binary against mock Paperclip and mock Telegram behind a test
//! proxy: SIGTERM during a long poll and with an answer in flight exits 0
//! without losing the answer (architecture §7.5).

mod support;

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use bridge::store::{DeliveryStatus, OutboxStatus, Store};
use serde_json::json;
use support::paperclip::*;
use support::proxy::ForwardProxy;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TOKEN: &str = "123456:placeholder";

struct Env {
    dir: tempfile::TempDir,
    paperclip: MockServer,
    telegram: MockServer,
    proxy: ForwardProxy,
}

impl Env {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pc"), "pcp_board_placeholder\n").unwrap();
        std::fs::write(dir.path().join("tg"), format!("{TOKEN}\n")).unwrap();
        let telegram = MockServer::start().await;
        for m in ["setMyCommands", "editMessageText", "answerCallbackQuery"] {
            Mock::given(method("POST"))
                .and(path(format!("/bot{TOKEN}/{m}")))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(json!({"ok": true, "result": true})),
                )
                .mount(&telegram)
                .await;
        }
        Mock::given(method("POST"))
            .and(path(format!("/bot{TOKEN}/sendMessage")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true,
                "result": {"message_id": 77, "chat": {"id": 111, "type": "private"}, "date": 0}})))
            .mount(&telegram)
            .await;
        Self {
            dir,
            paperclip: MockServer::start().await,
            telegram,
            proxy: ForwardProxy::start().await,
        }
    }

    fn spawn(&self) -> Child {
        let d = self.dir.path();
        Command::new(assert_cmd::cargo::cargo_bin("bridge"))
            .arg("run")
            .env_clear()
            .env("PAPERCLIP_URL", self.paperclip.uri())
            .env("PAPERCLIP_COMPANY_IDS", COMPANY)
            .env("PAPERCLIP_TOKEN_FILE", d.join("pc"))
            .env("TELEGRAM_BOT_TOKEN_FILE", d.join("tg"))
            .env("TELEGRAM_CHAT_ID", "111")
            .env("TELEGRAM_USER_ID", "111")
            .env("TELEGRAM_PROXY_URL", self.proxy.url())
            .env("TELEGRAM_API_BASE_URL", self.telegram.uri())
            .env("TELEGRAM_LONG_POLL_TIMEOUT", "20s")
            .env("STATE_PATH", d.join("bridge.db"))
            .env("HEALTH_FILE", d.join("health.json"))
            .env("PAPERCLIP_POLL_INTERVAL", "1s")
            .env("SHUTDOWN_GRACE", "10s")
            .env("LOG_FORMAT", "text")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    }
}

fn sigterm(child: &Child) {
    // SAFETY: plain kill(2) on our own child process.
    let rc = unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
    assert_eq!(rc, 0);
}

async fn wait_exit(child: &mut Child, limit: Duration) -> std::process::ExitStatus {
    let t0 = Instant::now();
    loop {
        if let Some(s) = child.try_wait().unwrap() {
            return s;
        }
        assert!(t0.elapsed() < limit, "process did not exit in {limit:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn stderr(child: &mut Child) -> String {
    use std::io::Read;
    let mut s = String::new();
    if let Some(mut e) = child.stderr.take() {
        let _ = e.read_to_string(&mut s);
    }
    s
}

#[tokio::test(flavor = "multi_thread")]
async fn sigterm_during_long_poll_exits_zero() {
    let env = Env::new().await;
    serve(&env.paperclip, &[], vec![]).await;
    Mock::given(method("POST"))
        .and(path(format!("/bot{TOKEN}/getUpdates")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"ok": true, "result": []}))
                .set_delay(Duration::from_secs(20)),
        )
        .mount(&env.telegram)
        .await;
    let mut child = env.spawn();
    // Wait until the long poll is in progress.
    let t0 = Instant::now();
    while !env
        .telegram
        .received_requests()
        .await
        .unwrap()
        .iter()
        .any(|r| r.url.path().ends_with("/getUpdates"))
    {
        assert!(t0.elapsed() < Duration::from_secs(10), "no getUpdates");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let stopped_at = Instant::now();
    sigterm(&child);
    let status = wait_exit(&mut child, Duration::from_secs(8)).await;
    let log = stderr(&mut child);
    assert!(status.success(), "exit status {status:?}\n{log}");
    assert!(
        stopped_at.elapsed() < Duration::from_secs(5),
        "long poll was interrupted"
    );
    assert!(env.dir.path().join("health.json").exists());
    // Every Telegram request went through the proxy.
    assert_eq!(
        env.proxy.requests(),
        env.telegram.received_requests().await.unwrap().len()
    );
    assert!(!log.contains("placeholder"), "no secrets in logs:\n{log}");
}

#[tokio::test(flavor = "multi_thread")]
async fn sigterm_with_answer_in_flight_finishes_it() {
    let env = Env::new().await;
    serve(&env.paperclip, &["card-1"], vec![confirmation("card-1")]).await;
    Mock::given(method("GET"))
        .and(path("/api/health"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"status": "ok", "version": "test"})),
        )
        .mount(&env.paperclip)
        .await;
    Mock::given(method("POST"))
        .and(path(format!(
            "/api/issues/{ISSUE}/interactions/card-1/accept"
        )))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({}))
                .set_delay(Duration::from_secs(3)),
        )
        .mount(&env.paperclip)
        .await;
    // First poll: the user's press, after the card was delivered.
    Mock::given(method("POST"))
        .and(path(format!("/bot{TOKEN}/getUpdates")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(
                    json!({"ok": true, "result": [{"update_id": 10, "callback_query": {
                    "id": "q1", "from": {"id": 111}, "data": "v1:ac:1:0",
                    "message": {"message_id": 77, "chat": {"id": 111, "type": "private"}}}}]}),
                )
                .set_delay(Duration::from_secs(3)),
        )
        .up_to_n_times(1)
        .mount(&env.telegram)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/bot{TOKEN}/getUpdates")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"ok": true, "result": []}))
                .set_delay(Duration::from_secs(20)),
        )
        .mount(&env.telegram)
        .await;
    let mut child = env.spawn();
    let t0 = Instant::now();
    while posts(&env.paperclip, "/accept").await.is_empty() {
        assert!(
            t0.elapsed() < Duration::from_secs(20),
            "accept was not sent"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // The accept is in flight (3 s server delay): stop now.
    sigterm(&child);
    let status = wait_exit(&mut child, Duration::from_secs(15)).await;
    let log = stderr(&mut child);
    assert!(status.success(), "exit status {status:?}\n{log}");

    let store = Store::open(&env.dir.path().join("bridge.db")).unwrap();
    let d = store
        .delivery_by_interaction("card-1".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(d.status, DeliveryStatus::Answered, "{log}");
    let e = store.outbox_for(d.id).await.unwrap().unwrap();
    assert_eq!(e.status, OutboxStatus::Done);
    assert_eq!(store.telegram_offset().await.unwrap(), 11);
    assert_eq!(posts(&env.paperclip, "/accept").await.len(), 1);
}
