//! Thin Bot API client on reqwest. There is exactly one constructor and it
//! always installs `Proxy::all(TELEGRAM_PROXY_URL)`; explicit proxy
//! configuration also disables reading proxy variables from the environment.
//!
//! The bot token is part of every URL, so reqwest errors are reduced to a
//! class and never formatted.

use std::time::Duration;

use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use serde_json::{Value, json};
use url::Url;

use super::{ChatInfo, MemberInfo, Result, TelegramApi, TelegramError, Update};
use crate::config::TelegramConfig;

pub struct HttpTelegramClient {
    http: reqwest::Client,
    base: Url,
    token: SecretString,
    request_timeout: Duration,
}

#[derive(Deserialize)]
struct ApiResponse {
    ok: bool,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error_code: Option<u16>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    parameters: Option<ResponseParameters>,
}

#[derive(Deserialize)]
struct ResponseParameters {
    #[serde(default)]
    retry_after: Option<u64>,
}

impl HttpTelegramClient {
    /// The only constructor. `cfg.proxy` is not optional.
    pub fn new(cfg: &TelegramConfig) -> std::result::Result<Self, String> {
        crate::logging::install_tls_provider();
        let proxy = reqwest::Proxy::all(cfg.proxy.expose())
            .map_err(|_| "TELEGRAM_PROXY_URL is not usable as a proxy".to_string())?;
        let http = reqwest::Client::builder()
            .proxy(proxy)
            .connect_timeout(cfg.connect_timeout)
            .user_agent(concat!(
                "paperclip-telegram-bridge/",
                env!("CARGO_PKG_VERSION")
            ))
            .build()
            .map_err(|_| "cannot build Telegram HTTP client".to_string())?;
        Ok(Self {
            http,
            base: cfg.api_base.clone(),
            token: cfg.token.clone(),
            request_timeout: cfg.request_timeout,
        })
    }

    fn method_url(&self, method: &str) -> Url {
        let mut url = self.base.clone();
        if let Ok(mut path) = url.path_segments_mut() {
            path.pop_if_empty()
                .push(&format!("bot{}", self.token.expose_secret()))
                .push(method);
        }
        url
    }

    async fn call(&self, method: &str, body: Value, timeout: Duration) -> Result<Value> {
        let resp = self
            .http
            .post(self.method_url(method))
            .json(&body)
            .timeout(timeout)
            .send()
            .await
            .map_err(map_reqwest_error)?;
        let status = resp.status().as_u16();
        let bytes = resp.bytes().await.map_err(map_reqwest_error)?;
        let parsed: Option<ApiResponse> = serde_json::from_slice(&bytes).ok();
        match parsed {
            Some(r) if r.ok => Ok(r.result.unwrap_or(Value::Null)),
            Some(r) => Err(classify(
                r.error_code.unwrap_or(status),
                r.description.unwrap_or_default(),
                r.parameters.and_then(|p| p.retry_after),
            )),
            None if status >= 500 => Err(TelegramError::Transient { reason: "http_5xx" }),
            None if (200..300).contains(&status) => Err(TelegramError::Decode),
            None => Err(classify(status, String::new(), None)),
        }
    }
}

fn map_reqwest_error(e: reqwest::Error) -> TelegramError {
    let e = e.without_url();
    let reason = if e.is_timeout() {
        "timeout"
    } else if e.is_connect() {
        "connect"
    } else {
        "request"
    };
    TelegramError::Transient { reason }
}

fn classify(code: u16, description: String, retry_after: Option<u64>) -> TelegramError {
    use super::topic_errors::{CANT_PARSE_ENTITIES, THREAD_NOT_FOUND, TOPIC_CLOSED, matches};
    match code {
        429 => TelegramError::RateLimited {
            retry_after: Duration::from_secs(retry_after.unwrap_or(1)),
        },
        400 if matches(&description, THREAD_NOT_FOUND) => TelegramError::ThreadNotFound,
        400 if matches(&description, TOPIC_CLOSED) => TelegramError::TopicClosed,
        400 if matches(&description, CANT_PARSE_ENTITIES) => TelegramError::CantParseEntities,
        400 => TelegramError::BadRequest {
            description: description.chars().take(200).collect(),
        },
        401 | 404 => TelegramError::Auth { status: code },
        403 => TelegramError::Forbidden,
        409 => TelegramError::Conflict,
        c if c >= 500 => TelegramError::Transient { reason: "http_5xx" },
        _ => TelegramError::BadRequest {
            description: description.chars().take(200).collect(),
        },
    }
}

impl TelegramApi for HttpTelegramClient {
    async fn get_updates(&self, offset: i64, timeout: Duration) -> Result<Vec<Update>> {
        let body = json!({
            "offset": offset,
            "timeout": timeout.as_secs(),
            "allowed_updates": ["message"],
        });
        let v = self
            .call("getUpdates", body, timeout + Duration::from_secs(10))
            .await?;
        let raw = v.as_array().ok_or(TelegramError::Decode)?;
        // Unknown update shapes are kept with their id so that offset advances.
        Ok(raw
            .iter()
            .filter_map(|u| {
                serde_json::from_value::<Update>(u.clone())
                    .ok()
                    .or_else(|| {
                        Some(Update {
                            update_id: u.get("update_id")?.as_i64()?,
                            message: None,
                        })
                    })
            })
            .collect())
    }

    async fn send_message(
        &self,
        chat_id: i64,
        thread_id: Option<i64>,
        text: &str,
        html: bool,
    ) -> Result<i64> {
        let mut body = json!({
            "chat_id": chat_id,
            "text": text,
            "link_preview_options": {"is_disabled": true},
        });
        if html {
            body["parse_mode"] = json!("HTML");
        }
        if let Some(t) = thread_id {
            body["message_thread_id"] = json!(t);
        }
        let v = self.call("sendMessage", body, self.request_timeout).await?;
        v.get("message_id")
            .and_then(Value::as_i64)
            .ok_or(TelegramError::Decode)
    }

    async fn create_forum_topic(&self, chat_id: i64, name: &str) -> Result<i64> {
        let body = json!({"chat_id": chat_id, "name": name});
        let v = self
            .call("createForumTopic", body, self.request_timeout)
            .await?;
        v.get("message_thread_id")
            .and_then(Value::as_i64)
            .ok_or(TelegramError::Decode)
    }

    async fn reopen_forum_topic(&self, chat_id: i64, thread_id: i64) -> Result<()> {
        let body = json!({"chat_id": chat_id, "message_thread_id": thread_id});
        self.call("reopenForumTopic", body, self.request_timeout)
            .await
            .map(|_| ())
    }

    async fn get_me(&self) -> Result<i64> {
        let v = self.call("getMe", json!({}), self.request_timeout).await?;
        v.get("id")
            .and_then(Value::as_i64)
            .ok_or(TelegramError::Decode)
    }

    async fn get_chat(&self, chat_id: i64) -> Result<ChatInfo> {
        let v = self
            .call("getChat", json!({"chat_id": chat_id}), self.request_timeout)
            .await?;
        Ok(ChatInfo {
            kind: v
                .get("type")
                .and_then(Value::as_str)
                .ok_or(TelegramError::Decode)?
                .to_string(),
            is_forum: v.get("is_forum").and_then(Value::as_bool).unwrap_or(false),
        })
    }

    async fn get_chat_member(&self, chat_id: i64, user_id: i64) -> Result<MemberInfo> {
        let v = self
            .call(
                "getChatMember",
                json!({"chat_id": chat_id, "user_id": user_id}),
                self.request_timeout,
            )
            .await?;
        Ok(MemberInfo {
            status: v
                .get("status")
                .and_then(Value::as_str)
                .ok_or(TelegramError::Decode)?
                .to_string(),
            can_manage_topics: v
                .get("can_manage_topics")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification() {
        assert_eq!(
            classify(429, "Too Many Requests: retry after 5".into(), Some(5)),
            TelegramError::RateLimited {
                retry_after: Duration::from_secs(5)
            }
        );
        assert_eq!(
            classify(400, "Bad Request: message thread not found".into(), None),
            TelegramError::ThreadNotFound
        );
        assert_eq!(
            classify(400, "Bad Request: MESSAGE THREAD NOT FOUND".into(), None),
            TelegramError::ThreadNotFound
        );
        assert_eq!(
            classify(400, "Bad Request: TOPIC_DELETED".into(), None),
            TelegramError::ThreadNotFound
        );
        assert_eq!(
            classify(400, "Bad Request: TOPIC_CLOSED".into(), None),
            TelegramError::TopicClosed
        );
        assert_eq!(
            classify(400, "bad request: topic_closed".into(), None),
            TelegramError::TopicClosed
        );
        assert_eq!(
            classify(
                400,
                "Bad Request: can't parse entities: unsupported start tag".into(),
                None
            ),
            TelegramError::CantParseEntities
        );
        assert!(matches!(
            classify(400, "Bad Request: chat not found".into(), None),
            TelegramError::BadRequest { .. }
        ));
        assert_eq!(
            classify(401, "Unauthorized".into(), None),
            TelegramError::Auth { status: 401 }
        );
        assert_eq!(
            classify(404, "Not Found".into(), None),
            TelegramError::Auth { status: 404 }
        );
        assert_eq!(
            classify(409, "Conflict".into(), None),
            TelegramError::Conflict
        );
        assert_eq!(
            classify(
                403,
                "Forbidden: bot was kicked from the supergroup chat".into(),
                None
            ),
            TelegramError::Forbidden
        );
        assert!(classify(502, String::new(), None).is_transient());
    }
}
