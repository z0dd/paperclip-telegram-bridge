//! Telegram goes only through `TELEGRAM_PROXY_URL`, never directly
//! (architecture §5.3), and the bot token never appears in errors or logs.

mod support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bridge::telegram::{
    HttpTelegramClient, InlineKeyboard, ReplyMarkup, TelegramApi, TelegramError,
};
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

#[tokio::test]
async fn every_request_goes_through_the_proxy() {
    let telegram = MockServer::start().await;
    mount_get_me(&telegram).await;
    Mock::given(method("POST"))
        .and(path(format!("/bot{BOT_TOKEN}/sendMessage")))
        .and(body_partial_json(json!({"chat_id": CHAT, "parse_mode": "HTML"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true, "result": {"message_id": 42, "chat": {"id": CHAT, "type": "private"}, "date": 0}
        })))
        .mount(&telegram)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/bot{BOT_TOKEN}/getUpdates")))
        .and(body_partial_json(
            json!({"offset": 5, "allowed_updates": ["message", "callback_query"]}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true, "result": [
                {"update_id": 5, "message": {"message_id": 1, "from": {"id": 111}, "chat": {"id": 111, "type": "private"}, "text": "/help"}},
                {"update_id": 6, "edited_message": {"message_id": 1}},
                {"update_id": 7, "callback_query": {"id": "q", "from": {"id": 111}, "data": "v1:ac:1:0",
                  "message": {"message_id": 1, "chat": {"id": 111, "type": "private"}}}}
            ]
        })))
        .mount(&telegram)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/bot{BOT_TOKEN}/editMessageText")))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "ok": false, "error_code": 400, "description": "Bad Request: message is not modified"
        })))
        .mount(&telegram)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/bot{BOT_TOKEN}/answerCallbackQuery")))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({
            "ok": false, "error_code": 429, "description": "Too Many Requests: retry after 3",
            "parameters": {"retry_after": 3}
        })))
        .mount(&telegram)
        .await;

    let proxy = ForwardProxy::start().await;
    let client = HttpTelegramClient::new(&telegram_config(&telegram.uri(), &proxy.url())).unwrap();
    client.get_me().await.unwrap();
    let id = client
        .send_message(
            CHAT,
            "<b>hi</b>",
            Some(&ReplyMarkup::Inline(InlineKeyboard::default())),
        )
        .await
        .unwrap();
    assert_eq!(id, 42);
    let updates = client.get_updates(5, Duration::from_secs(1)).await.unwrap();
    assert_eq!(updates.len(), 3, "unknown update kinds keep their id");
    assert_eq!(updates[1].update_id, 6);
    assert!(updates[1].message.is_none() && updates[1].callback_query.is_none());
    assert_eq!(
        client.edit_message_text(CHAT, 1, "x", None).await,
        Err(TelegramError::NotModified)
    );
    assert_eq!(
        client.answer_callback_query("q", Some("ok")).await,
        Err(TelegramError::RateLimited {
            retry_after: Duration::from_secs(3)
        })
    );

    let direct = telegram.received_requests().await.unwrap().len();
    assert_eq!(direct, 5);
    assert_eq!(proxy.requests(), direct, "all requests went via the proxy");
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
    let capture = Capture::default();
    let writer = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_max_level(tracing::Level::DEBUG)
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let proxy = format!("socks5h://user:hunter2@127.0.0.1:{}", closed_port());
    let client =
        HttpTelegramClient::new(&telegram_config("https://api.telegram.org", &proxy)).unwrap();
    let err = client
        .send_message(CHAT, "secret question text", None)
        .await
        .unwrap_err();
    // What callers log: the error display and class.
    tracing::warn!(error = %err, class = err.class(), "telegram call failed");
    let rendered = format!("{err} {err:?}");
    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
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
