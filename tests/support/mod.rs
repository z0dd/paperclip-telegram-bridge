//! Shared helpers for integration tests.
#![allow(dead_code)]

pub mod harness;
pub mod paperclip;
pub mod proxy;

use std::time::Duration;

use bridge::config::{ProxyUrl, TelegramConfig};
use bridge::telegram::{HttpTelegramClient, TelegramApi};
use secrecy::SecretString;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

pub const BOT_TOKEN: &str = "123456:TEST-placeholder-token";
pub const CHAT: i64 = 111;
pub const USER: i64 = 111;

pub fn telegram_config(api_base: &str, proxy: &str) -> TelegramConfig {
    TelegramConfig {
        token: SecretString::from(BOT_TOKEN),
        chat_id: CHAT,
        user_id: USER,
        proxy: ProxyUrl::parse(proxy).unwrap(),
        api_base: api_base.parse().unwrap(),
        long_poll_timeout: Duration::from_secs(1),
        request_timeout: Duration::from_secs(3),
        connect_timeout: Duration::from_secs(2),
    }
}

pub async fn mount_get_me(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path(format!("/bot{BOT_TOKEN}/getMe")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true, "result": {"id": 1, "is_bot": true, "first_name": "bridge"}
        })))
        .mount(server)
        .await;
}

/// Telegram half of the env-proxy test: the Telegram client uses only its
/// configured proxy, never the trap from the environment.
pub async fn env_proxy_telegram_checks(_trap: &proxy::CountingListener) {
    let telegram = MockServer::start().await;
    mount_get_me(&telegram).await;
    let fwd = proxy::ForwardProxy::start().await;
    let client = HttpTelegramClient::new(&telegram_config(&telegram.uri(), &fwd.url())).unwrap();
    client.get_me().await.unwrap();
    assert_eq!(fwd.requests(), 1);
    assert_eq!(telegram.received_requests().await.unwrap().len(), 1);
}
