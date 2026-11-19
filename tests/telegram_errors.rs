//! Classification of Bot API errors over HTTP (ADR-0010 §1.7): the table of
//! answers for `sendMessage`, `createForumTopic` and `reopenForumTopic`. The
//! texts of deleted/closed topic errors were not checked against the live
//! Bot API (docs/paperclip-api.md): the cases here are the mock's guess.

mod support;

use std::time::Duration;

use bridge::telegram::{HttpTelegramClient, TelegramApi, TelegramError};
use serde_json::json;
use support::proxy::ForwardProxy;
use support::{BOT_TOKEN, CHAT, telegram_config};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// One server answers every method with this error.
async fn client_for(
    code: u16,
    description: &str,
    retry_after: Option<u64>,
) -> (MockServer, ForwardProxy, HttpTelegramClient) {
    let telegram = MockServer::start().await;
    let mut body = json!({"ok": false, "error_code": code, "description": description});
    if let Some(r) = retry_after {
        body["parameters"] = json!({"retry_after": r});
    }
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(code).set_body_json(body))
        .mount(&telegram)
        .await;
    let proxy = ForwardProxy::start().await;
    let client = HttpTelegramClient::new(&telegram_config(&telegram.uri(), &proxy.url())).unwrap();
    (telegram, proxy, client)
}

fn secs(n: u64) -> TelegramError {
    TelegramError::RateLimited {
        retry_after: Duration::from_secs(n),
    }
}

#[tokio::test]
async fn send_message_errors_are_classified_by_code_and_text() {
    let not_found = TelegramError::ThreadNotFound;
    let closed = TelegramError::TopicClosed;
    let cases: Vec<(u16, &str, Option<u64>, TelegramError)> = vec![
        // Deleted topic: case, prefix and spelling do not matter on a 400.
        (
            400,
            "Bad Request: message thread not found",
            None,
            not_found.clone(),
        ),
        (
            400,
            "Bad Request: MESSAGE THREAD NOT FOUND",
            None,
            not_found.clone(),
        ),
        (400, "message thread not found", None, not_found.clone()),
        (400, "Bad Request: TOPIC_DELETED", None, not_found.clone()),
        (400, "bad request: topic_deleted", None, not_found.clone()),
        // Closed topic.
        (400, "Bad Request: TOPIC_CLOSED", None, closed.clone()),
        (400, "bad request: topic_closed", None, closed.clone()),
        (
            400,
            "Bad Request: TOPIC_CLOSED (extra)",
            None,
            closed.clone(),
        ),
        // Broken HTML.
        (
            400,
            "Bad Request: can't parse entities: Unsupported start tag",
            None,
            TelegramError::CantParseEntities,
        ),
        // Other 400 stay a plain bad request and are never "topic gone".
        (
            400,
            "Bad Request: chat not found",
            None,
            TelegramError::BadRequest {
                description: "Bad Request: chat not found".into(),
            },
        ),
        (
            400,
            "Bad Request: message is too long",
            None,
            TelegramError::BadRequest {
                description: "Bad Request: message is too long".into(),
            },
        ),
        // The code decides first: the same texts with another code.
        (
            403,
            "Forbidden: thread not found",
            None,
            TelegramError::Forbidden,
        ),
        (
            403,
            "Forbidden: bot was kicked from the supergroup chat",
            None,
            TelegramError::Forbidden,
        ),
        (
            401,
            "Unauthorized",
            None,
            TelegramError::Auth { status: 401 },
        ),
        (404, "Not Found", None, TelegramError::Auth { status: 404 }),
        (
            409,
            "Conflict: terminated by other getUpdates request",
            None,
            TelegramError::Conflict,
        ),
        (429, "Too Many Requests: thread not found", Some(5), secs(5)),
        (429, "Too Many Requests: retry after 12", Some(12), secs(12)),
        // `retry_after` is missing: wait one second, never zero.
        (429, "Too Many Requests", None, secs(1)),
        (
            500,
            "Internal Server Error: TOPIC_CLOSED",
            None,
            TelegramError::Transient { reason: "http_5xx" },
        ),
        (
            502,
            "Bad Gateway",
            None,
            TelegramError::Transient { reason: "http_5xx" },
        ),
    ];
    for (code, description, retry_after, expected) in cases {
        let (_telegram, _proxy, client) = client_for(code, description, retry_after).await;
        let got = client.send_message(CHAT, Some(7), "x", true).await;
        assert_eq!(got, Err(expected), "{code} {description:?}");
    }
}

#[tokio::test]
async fn topic_calls_use_the_same_classification() {
    let (_t, _p, client) = client_for(429, "Too Many Requests: retry after 30", Some(30)).await;
    assert_eq!(
        client.create_forum_topic(CHAT, "PET · Lead").await,
        Err(secs(30))
    );
    assert_eq!(client.reopen_forum_topic(CHAT, 7).await, Err(secs(30)));

    let (_t, _p, client) = client_for(400, "Bad Request: message thread not found", None).await;
    assert_eq!(
        client.reopen_forum_topic(CHAT, 7).await,
        Err(TelegramError::ThreadNotFound)
    );

    let (_t, _p, client) =
        client_for(403, "Forbidden: not enough rights to create a topic", None).await;
    assert_eq!(
        client.create_forum_topic(CHAT, "PET · Lead").await,
        Err(TelegramError::Forbidden)
    );

    let (_t, _p, client) = client_for(400, "Bad Request: CHAT_NOT_MODIFIED", None).await;
    assert!(matches!(
        client.create_forum_topic(CHAT, "x").await,
        Err(TelegramError::BadRequest { .. })
    ));
}

#[tokio::test]
async fn bad_request_description_is_cut_and_never_holds_the_token() {
    let long = format!("Bad Request: {}", "y".repeat(500));
    let (_t, _p, client) = client_for(400, &long, None).await;
    match client.send_message(CHAT, None, "x", true).await {
        Err(TelegramError::BadRequest { description }) => {
            assert!(description.chars().count() <= 200, "{}", description.len());
            assert!(!description.contains(BOT_TOKEN));
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn non_json_answers_are_classified_by_status() {
    for (code, body, transient) in [
        (502u16, "<html>Bad Gateway</html>", true),
        (200, "not json", true),
    ] {
        let telegram = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(format!("/bot{BOT_TOKEN}/sendMessage")))
            .respond_with(ResponseTemplate::new(code).set_body_string(body))
            .mount(&telegram)
            .await;
        let proxy = ForwardProxy::start().await;
        let client =
            HttpTelegramClient::new(&telegram_config(&telegram.uri(), &proxy.url())).unwrap();
        let err = client
            .send_message(CHAT, Some(1), "x", true)
            .await
            .unwrap_err();
        assert_eq!(err.is_transient(), transient, "{code}: {err:?}");
    }
}
