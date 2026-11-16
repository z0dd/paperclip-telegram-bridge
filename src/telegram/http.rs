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

use super::{BotCommand, InlineKeyboard, ReplyMarkup, Result, TelegramApi, TelegramError, Update};
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
    let lower = description.to_ascii_lowercase();
    match code {
        429 => TelegramError::RateLimited {
            retry_after: Duration::from_secs(retry_after.unwrap_or(1)),
        },
        400 if lower.contains("message is not modified") => TelegramError::NotModified,
        400 if lower.contains("message to edit not found")
            || lower.contains("message_id_invalid")
            || lower.contains("message can't be edited") =>
        {
            TelegramError::MessageNotFound
        }
        400 if lower.contains("query is too old") || lower.contains("query id is invalid") => {
            TelegramError::CallbackExpired
        }
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

fn markup_json(markup: Option<&ReplyMarkup>) -> Option<Value> {
    match markup? {
        ReplyMarkup::Inline(kb) => Some(serde_json::to_value(kb).unwrap_or(Value::Null)),
        ReplyMarkup::ForceReply => Some(json!({"force_reply": true, "selective": true})),
    }
}

fn keyboard_json(kb: Option<&InlineKeyboard>) -> Value {
    match kb {
        Some(kb) => serde_json::to_value(kb).unwrap_or(Value::Null),
        // An empty keyboard removes the buttons.
        None => json!({"inline_keyboard": []}),
    }
}

impl TelegramApi for HttpTelegramClient {
    async fn get_updates(&self, offset: i64, timeout: Duration) -> Result<Vec<Update>> {
        let body = json!({
            "offset": offset,
            "timeout": timeout.as_secs(),
            "allowed_updates": ["message", "callback_query"],
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
                            callback_query: None,
                        })
                    })
            })
            .collect())
    }

    async fn send_message(
        &self,
        chat_id: i64,
        html: &str,
        markup: Option<&ReplyMarkup>,
    ) -> Result<i64> {
        let mut body = json!({
            "chat_id": chat_id,
            "text": html,
            "parse_mode": "HTML",
            "link_preview_options": {"is_disabled": true},
        });
        if let Some(m) = markup_json(markup) {
            body["reply_markup"] = m;
        }
        let v = self.call("sendMessage", body, self.request_timeout).await?;
        v.get("message_id")
            .and_then(Value::as_i64)
            .ok_or(TelegramError::Decode)
    }

    async fn edit_message_text(
        &self,
        chat_id: i64,
        message_id: i64,
        html: &str,
        keyboard: Option<&InlineKeyboard>,
    ) -> Result<()> {
        let body = json!({
            "chat_id": chat_id,
            "message_id": message_id,
            "text": html,
            "parse_mode": "HTML",
            "link_preview_options": {"is_disabled": true},
            "reply_markup": keyboard_json(keyboard),
        });
        self.call("editMessageText", body, self.request_timeout)
            .await
            .map(|_| ())
    }

    async fn edit_reply_markup(
        &self,
        chat_id: i64,
        message_id: i64,
        keyboard: Option<&InlineKeyboard>,
    ) -> Result<()> {
        let body = json!({
            "chat_id": chat_id,
            "message_id": message_id,
            "reply_markup": keyboard_json(keyboard),
        });
        self.call("editMessageReplyMarkup", body, self.request_timeout)
            .await
            .map(|_| ())
    }

    async fn answer_callback_query(
        &self,
        callback_query_id: &str,
        text: Option<&str>,
    ) -> Result<()> {
        let mut body = json!({"callback_query_id": callback_query_id});
        if let Some(t) = text {
            body["text"] = json!(t.chars().take(200).collect::<String>());
        }
        self.call("answerCallbackQuery", body, self.request_timeout)
            .await
            .map(|_| ())
    }

    async fn set_my_commands(&self, chat_id: i64, commands: &[BotCommand]) -> Result<()> {
        let body = json!({
            "commands": commands,
            "scope": {"type": "chat", "chat_id": chat_id},
        });
        self.call("setMyCommands", body, self.request_timeout)
            .await
            .map(|_| ())
    }

    async fn get_me(&self) -> Result<()> {
        self.call("getMe", json!({}), self.request_timeout)
            .await
            .map(|_| ())
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
            classify(400, "Bad Request: message is not modified: x".into(), None),
            TelegramError::NotModified
        );
        assert_eq!(
            classify(400, "Bad Request: message to edit not found".into(), None),
            TelegramError::MessageNotFound
        );
        assert_eq!(
            classify(
                400,
                "Bad Request: query is too old and response timeout expired".into(),
                None
            ),
            TelegramError::CallbackExpired
        );
        assert!(matches!(
            classify(400, "Bad Request: can't parse entities".into(), None),
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
            classify(403, "Forbidden: bot was blocked".into(), None),
            TelegramError::Forbidden
        );
        assert!(classify(502, String::new(), None).is_transient());
    }
}
