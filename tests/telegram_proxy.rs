//! Telegram goes only through `TELEGRAM_PROXY_URL`, never directly
//! (architecture §5.3), and the bot token never appears in errors or logs.

mod support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bridge::telegram::{HttpTelegramClient, TelegramApi, TelegramError};
use serde_json::json;
use support::proxy::ForwardProxy;
use support::{BOT_TOKEN, CHAT, mount_get_me, telegram_config};
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn closed_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

#[tokio::test]
async fn unavailable_proxy_means_zero_direct_requests() {
    let telegram = MockServer::start().await;
    mount_get_me(&telegram).await;
    for scheme in ["socks5h", "socks5", "http"] {
        let proxy = format!("{scheme}://127.0.0.1:{}", closed_port());
        let client = HttpTelegramClient::new(&telegram_config(&telegram.uri(), &proxy)).unwrap();
        for _ in 0..3 {
            let err = client.get_me().await.unwrap_err();
            assert!(err.is_transient(), "{scheme}: {err:?}");
            let err = client
                .get_updates(0, Duration::from_secs(1))
                .await
                .unwrap_err();
            assert!(err.is_transient(), "{scheme}: {err:?}");
        }
    }
    assert_eq!(
        telegram.received_requests().await.unwrap().len(),
        0,
        "no direct fallback"
    );
}

fn ok(result: serde_json::Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({"ok": true, "result": result}))
}

fn fail(code: u16, description: &str) -> ResponseTemplate {
    ResponseTemplate::new(code)
        .set_body_json(json!({"ok": false, "error_code": code, "description": description}))
}

#[tokio::test]
async fn every_request_goes_through_the_proxy() {
    let telegram = MockServer::start().await;
    mount_get_me(&telegram).await;
    let tg = |m: &str| path(format!("/bot{BOT_TOKEN}/{m}"));
    Mock::given(method("POST"))
        .and(tg("sendMessage"))
        .and(body_partial_json(
            json!({"chat_id": CHAT, "message_thread_id": 7, "parse_mode": "HTML"}),
        ))
        .respond_with(ok(
            json!({"message_id": 42, "chat": {"id": CHAT, "type": "supergroup"}, "date": 0}),
        ))
        .mount(&telegram)
        .await;
    Mock::given(method("POST"))
        .and(tg("sendMessage"))
        .and(body_partial_json(json!({"message_thread_id": 8})))
        .respond_with(fail(400, "Bad Request: message thread not found"))
        .mount(&telegram)
        .await;
    Mock::given(method("POST"))
        .and(tg("sendMessage"))
        .and(body_partial_json(json!({"message_thread_id": 9})))
        .respond_with(fail(400, "Bad Request: TOPIC_CLOSED"))
        .mount(&telegram)
        .await;
    Mock::given(method("POST"))
        .and(tg("createForumTopic"))
        .and(body_partial_json(
            json!({"chat_id": CHAT, "name": "PET · Lead"}),
        ))
        .respond_with(ok(
            json!({"message_thread_id": 77, "name": "PET · Lead", "icon_color": 1}),
        ))
        .mount(&telegram)
        .await;
    Mock::given(method("POST"))
        .and(tg("reopenForumTopic"))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({
            "ok": false, "error_code": 429, "description": "Too Many Requests: retry after 3",
            "parameters": {"retry_after": 3}
        })))
        .mount(&telegram)
        .await;
    Mock::given(method("POST"))
        .and(tg("getChat"))
        .respond_with(ok(
            json!({"id": CHAT, "type": "supergroup", "is_forum": true}),
        ))
        .mount(&telegram)
        .await;
    Mock::given(method("POST"))
        .and(tg("getChatMember"))
        .and(body_partial_json(json!({"chat_id": CHAT, "user_id": 1})))
        .respond_with(ok(
            json!({"status": "administrator", "can_manage_topics": true,
                               "user": {"id": 1, "is_bot": true}}),
        ))
        .mount(&telegram)
        .await;
    Mock::given(method("POST"))
        .and(tg("getUpdates"))
        .and(body_partial_json(
            json!({"offset": 5, "allowed_updates": ["message"]}),
        ))
        .respond_with(ok(json!([
            {"update_id": 5, "message": {"message_id": 1, "from": {"id": 111},
              "chat": {"id": CHAT, "type": "supergroup"}, "message_thread_id": 7,
              "is_topic_message": true, "text": "hi"}},
            {"update_id": 6, "edited_message": {"message_id": 1}}
        ])))
        .mount(&telegram)
        .await;

    let proxy = ForwardProxy::start().await;
    let client = HttpTelegramClient::new(&telegram_config(&telegram.uri(), &proxy.url())).unwrap();
    let bot = client.get_me().await.unwrap();
    assert_eq!(bot, 1);
    assert_eq!(
        client.send_message(CHAT, Some(7), "<b>hi</b>", true).await,
        Ok(42)
    );
    assert_eq!(
        client.send_message(CHAT, Some(8), "x", true).await,
        Err(TelegramError::ThreadNotFound)
    );
    assert_eq!(
        client.send_message(CHAT, Some(9), "x", true).await,
        Err(TelegramError::TopicClosed)
    );
    assert_eq!(client.create_forum_topic(CHAT, "PET · Lead").await, Ok(77));
    assert_eq!(
        client.reopen_forum_topic(CHAT, 9).await,
        Err(TelegramError::RateLimited {
            retry_after: Duration::from_secs(3)
        })
    );
    let chat = client.get_chat(CHAT).await.unwrap();
    assert!(chat.is_forum && chat.kind == "supergroup");
    let me = client.get_chat_member(CHAT, bot).await.unwrap();
    assert!(me.can_manage_topics && me.status == "administrator");
    let updates = client.get_updates(5, Duration::from_secs(1)).await.unwrap();
    assert_eq!(updates.len(), 2, "unknown update kinds keep their id");
    assert_eq!(
        updates[0].message.as_ref().unwrap().topic_thread_id(),
        Some(7)
    );
    assert_eq!(updates[1].update_id, 6);
    assert!(updates[1].message.is_none());

    let direct = telegram.received_requests().await.unwrap().len();
    assert_eq!(direct, 9);
    assert_eq!(proxy.requests(), direct, "all requests went via the proxy");
}

#[tokio::test]
async fn plain_text_is_sent_without_parse_mode() {
    let telegram = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!("/bot{BOT_TOKEN}/sendMessage")))
        .respond_with(ok(json!({"message_id": 1})))
        .mount(&telegram)
        .await;
    let proxy = ForwardProxy::start().await;
    let client = HttpTelegramClient::new(&telegram_config(&telegram.uri(), &proxy.url())).unwrap();
    client
        .send_message(CHAT, None, "<b>raw</b>", false)
        .await
        .unwrap();
    let req = &telegram.received_requests().await.unwrap()[0];
    let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
    assert!(body.get("parse_mode").is_none(), "{body}");
    assert!(body.get("message_thread_id").is_none(), "General: {body}");
    assert_eq!(body["text"], "<b>raw</b>");
}

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn token_is_not_in_errors_or_logs_on_connection_failure() {
    // Global subscriber: a thread-local one misses events once another test
    // thread cached the callsite interest.
    let capture = Capture::default();
    let writer = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_max_level(tracing::Level::DEBUG)
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::set_global_default(subscriber).unwrap();

    let proxy = format!("socks5h://user:hunter2@127.0.0.1:{}", closed_port());
    let client =
        HttpTelegramClient::new(&telegram_config("https://api.telegram.org", &proxy)).unwrap();
    let err = client
        .send_message(CHAT, Some(7), "secret question text", true)
        .await
        .unwrap_err();
    // What callers log: the error display and class.
    tracing::warn!(error = %err, class = err.class(), "telegram call failed");
    let rendered = format!("{err} {err:?}");
    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(
        logs.contains("telegram call failed"),
        "log was captured: {logs}"
    );
    for haystack in [&rendered, &logs] {
        assert!(!haystack.contains("TEST-placeholder-token"), "{haystack}");
        assert!(!haystack.contains("hunter2"), "{haystack}");
        assert!(!haystack.contains("api.telegram.org"), "{haystack}");
        assert!(!haystack.contains("secret question"), "{haystack}");
    }
}

#[test]
fn proxy_is_mandatory_in_the_type() {
    // TelegramConfig.proxy is a ProxyUrl, not an Option; an empty or
    // unsupported value cannot be constructed.
    assert!(bridge::config::ProxyUrl::parse("").is_err());
    assert!(bridge::config::ProxyUrl::parse("direct://").is_err());
}
