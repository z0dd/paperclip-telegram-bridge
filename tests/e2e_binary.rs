//! The real binary against the Paperclip mock and mock Telegram behind a
//! test proxy: SIGTERM during a long poll and with a message in flight exits 0
//! without losing the message (architecture §7.5); start checks and the
//! version check do not block the start.

mod support;

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use bridge::store::{PcStatus, Store};
use serde_json::json;
use support::paperclip::{KEY, PaperclipMock};
use support::proxy::ForwardProxy;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TOKEN: &str = "123456:placeholder";
const GROUP: &str = "-1001234567890";
const THREAD: i64 = 77;

struct Env {
    dir: tempfile::TempDir,
    paperclip: PaperclipMock,
    telegram: MockServer,
    proxy: ForwardProxy,
}

fn ok(result: serde_json::Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({"ok": true, "result": result}))
}

impl Env {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pc"), format!("{KEY}\n")).unwrap();
        std::fs::write(dir.path().join("tg"), format!("{TOKEN}\n")).unwrap();
        let telegram = MockServer::start().await;
        for (m, result) in [
            (
                "getMe",
                json!({"id": 1, "is_bot": true, "first_name": "bridge"}),
            ),
            (
                "getChat",
                json!({"id": -1001234567890_i64, "type": "supergroup", "is_forum": true}),
            ),
            (
                "getChatMember",
                json!({"status": "administrator", "can_manage_topics": true}),
            ),
            (
                "createForumTopic",
                json!({"message_thread_id": THREAD, "name": "PET · Lead"}),
            ),
            ("reopenForumTopic", json!(true)),
            (
                "sendMessage",
                json!({"message_id": 500, "date": 0,
                                   "chat": {"id": -1001234567890_i64, "type": "supergroup"}}),
            ),
        ] {
            Mock::given(method("POST"))
                .and(path(format!("/bot{TOKEN}/{m}")))
                .respond_with(ok(result))
                .mount(&telegram)
                .await;
        }
        Self {
            dir,
            paperclip: PaperclipMock::start().await,
            telegram,
            proxy: ForwardProxy::start().await,
        }
    }

    async fn long_poll(&self) {
        Mock::given(method("POST"))
            .and(path(format!("/bot{TOKEN}/getUpdates")))
            .respond_with(ok(json!([])).set_delay(Duration::from_secs(20)))
            .mount(&self.telegram)
            .await;
    }

    async fn requested(&self, suffix: &str) -> bool {
        self.telegram
            .received_requests()
            .await
            .unwrap()
            .iter()
            .any(|r| r.url.path().ends_with(suffix))
    }

    async fn wait_for(&self, suffix: &str) {
        let t0 = Instant::now();
        while !self.requested(suffix).await {
            assert!(t0.elapsed() < Duration::from_secs(10), "no {suffix}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    fn spawn(&self) -> Child {
        let d = self.dir.path();
        Command::new(assert_cmd::cargo::cargo_bin("bridge"))
            .arg("run")
            .env_clear()
            .env("PAPERCLIP_URL", self.paperclip.uri())
            .env("PAPERCLIP_TOKEN_FILE", d.join("pc"))
            .env("TELEGRAM_BOT_TOKEN_FILE", d.join("tg"))
            .env("TELEGRAM_CHAT_ID", GROUP)
            .env("TELEGRAM_ALLOWED_USER_IDS", "111")
            .env("TELEGRAM_PROXY_URL", self.proxy.url())
            .env("TELEGRAM_API_BASE_URL", self.telegram.uri())
            .env("TELEGRAM_LONG_POLL_TIMEOUT", "20s")
            .env("TELEGRAM_GROUP_MIN_INTERVAL", "100ms")
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
    env.long_poll().await;
    let mut child = env.spawn();
    env.wait_for("/getUpdates").await;
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
async fn sigterm_with_message_in_flight_finishes_it() {
    let env = Env::new().await;
    env.paperclip
        .company("company-pet", "PET")
        .agent("company-pet", "agent-lead", "Lead")
        .chat("company-pet", "issue-1", "PET-4", "agent-lead");
    env.paperclip.st().post_delay = Some(Duration::from_secs(3));
    // First poll (after the topic exists): the user's message in the topic.
    Mock::given(method("POST"))
        .and(path(format!("/bot{TOKEN}/getUpdates")))
        .respond_with(
            ok(json!([{"update_id": 10, "message": {
                "message_id": 5, "from": {"id": 111}, "date": 0,
                "chat": {"id": -1001234567890_i64, "type": "supergroup"},
                "message_thread_id": THREAD, "is_topic_message": true, "text": "Привет"}}]))
            .set_delay(Duration::from_secs(3)),
        )
        .up_to_n_times(1)
        .mount(&env.telegram)
        .await;
    env.long_poll().await;
    let mut child = env.spawn();
    let t0 = Instant::now();
    while env.paperclip.posts().is_empty() {
        assert!(
            t0.elapsed() < Duration::from_secs(20),
            "message was not posted"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // The POST is in flight (3 s server delay): stop now.
    sigterm(&child);
    let status = wait_exit(&mut child, Duration::from_secs(15)).await;
    let log = stderr(&mut child);
    assert!(status.success(), "exit status {status:?}\n{log}");

    let store = Store::open(&env.dir.path().join("bridge.db")).unwrap();
    let entries = store.messages().await.unwrap();
    assert_eq!(entries.len(), 1, "{log}");
    assert_eq!(entries[0].status, PcStatus::Done, "{log}");
    assert_eq!(store.telegram_offset().await.unwrap(), 11);
    assert_eq!(env.paperclip.posts().len(), 1);
    assert_eq!(env.paperclip.user_comments("issue-1").len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn start_checks_go_through_the_proxy() {
    let env = Env::new().await;
    env.long_poll().await;
    let mut child = env.spawn();
    env.wait_for("/getChatMember").await;
    sigterm(&child);
    let status = wait_exit(&mut child, Duration::from_secs(8)).await;
    let log = stderr(&mut child);
    assert!(status.success(), "exit status {status:?}");
    assert!(log.contains("telegram supergroup checks passed"), "{log}");
    assert_eq!(
        env.proxy.requests(),
        env.telegram.received_requests().await.unwrap().len(),
        "start checks must use the proxy"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_group_is_an_error_without_exit() {
    let env = Env::new().await;
    env.telegram.reset().await;
    Mock::given(method("POST"))
        .and(path(format!("/bot{TOKEN}/getMe")))
        .respond_with(ok(json!({"id": 1})))
        .mount(&env.telegram)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/bot{TOKEN}/getChat")))
        .respond_with(ok(
            json!({"id": -1001234567890_i64, "type": "supergroup", "is_forum": false}),
        ))
        .mount(&env.telegram)
        .await;
    env.long_poll().await;
    let mut child = env.spawn();
    env.wait_for("/getChat").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        child.try_wait().unwrap().is_none(),
        "the process keeps running"
    );
    sigterm(&child);
    let status = wait_exit(&mut child, Duration::from_secs(8)).await;
    let log = stderr(&mut child);
    assert!(status.success(), "exit status {status:?}");
    let line = log
        .lines()
        .find(|l| l.contains("telegram supergroup check failed"))
        .unwrap_or_else(|| panic!("{log}"));
    assert!(line.contains("ERROR") && line.contains("Topics"), "{line}");
}

/// Starts the binary with `/api/health` answering `health`, waits for the
/// first Telegram poll and returns the log.
async fn start_with_health(health: serde_json::Value) -> String {
    let env = Env::new().await;
    env.paperclip.st().health = health;
    env.long_poll().await;
    let mut child = env.spawn();
    env.wait_for("/getUpdates").await;
    sigterm(&child);
    let status = wait_exit(&mut child, Duration::from_secs(8)).await;
    let log = stderr(&mut child);
    assert!(status.success(), "exit status {status:?}\n{log}");
    log
}

fn warn_line<'a>(log: &'a str, message: &str) -> &'a str {
    log.lines()
        .find(|l| l.contains(message))
        .unwrap_or_else(|| panic!("no {message:?} in\n{log}"))
}

#[tokio::test(flavor = "multi_thread")]
async fn other_paperclip_version_warns_and_starts() {
    let log = start_with_health(json!({"status": "ok", "version": "2026.0101.0"})).await;
    let line = warn_line(&log, "paperclip version differs from the tested one");
    assert!(line.contains("WARN"), "{line}");
    assert!(line.contains("2026.0101.0"), "{line}");
    assert!(
        line.contains(bridge::paperclip::version::TESTED_PAPERCLIP_VERSION),
        "{line}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_paperclip_version_warns_and_starts() {
    let log = start_with_health(json!({"status": "ok"})).await;
    let line = warn_line(&log, "paperclip version is missing");
    assert!(line.contains("WARN"), "{line}");
}

#[tokio::test(flavor = "multi_thread")]
async fn unrecognized_paperclip_version_warns_and_starts() {
    let log = start_with_health(json!({"status": "ok", "version": "x"})).await;
    let line = warn_line(&log, "paperclip version is not recognized");
    assert!(line.contains("WARN"), "{line}");
}

#[tokio::test(flavor = "multi_thread")]
async fn tested_paperclip_version_is_info_only() {
    let version = bridge::paperclip::version::TESTED_PAPERCLIP_VERSION;
    let log = start_with_health(json!({"status": "ok", "version": version})).await;
    let line = warn_line(&log, "paperclip reachable");
    assert!(line.contains("INFO"), "{line}");
    assert!(!log.contains("paperclip version"), "{log}");
}
