//! Telegram Bot API behind a trait. The only implementation always goes
//! through `TELEGRAM_PROXY_URL` (architecture §5.1).

pub mod http;
pub mod types;

use std::future::Future;
use std::time::Duration;

pub use http::HttpTelegramClient;
pub use types::*;

/// Substrings of Bot API error descriptions that identify topic errors
/// (ADR-0010 §1.7). Compared case-insensitively. The texts for a deleted and a
/// closed topic are not documented by Telegram and were not checked live yet
/// (docs/paperclip-api.md): after a live check, only these lists change.
pub mod topic_errors {
    /// The topic was deleted: create it again.
    pub const THREAD_NOT_FOUND: &[&str] = &["thread not found", "topic_deleted"];
    /// The topic is closed: reopen it.
    pub const TOPIC_CLOSED: &[&str] = &["topic_closed"];
    /// Broken HTML entities: send the same piece as plain text.
    pub const CANT_PARSE_ENTITIES: &[&str] = &["can't parse entities"];

    pub fn matches(description: &str, markers: &[&str]) -> bool {
        let lower = description.to_lowercase();
        markers.iter().any(|m| lower.contains(&m.to_lowercase()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TelegramError {
    /// Timeout, connection or proxy error, `5xx`. Never contains the URL.
    #[error("transient error: {reason}")]
    Transient { reason: &'static str },
    #[error("rate limited for {retry_after:?}")]
    RateLimited { retry_after: Duration },
    /// The topic was deleted.
    #[error("message thread not found")]
    ThreadNotFound,
    /// The topic is closed.
    #[error("topic closed")]
    TopicClosed,
    /// `400 can't parse entities`: the HTML is broken.
    #[error("can't parse entities")]
    CantParseEntities,
    #[error("bad request")]
    BadRequest { description: String },
    /// `401` / `404`: the bot token is invalid.
    #[error("unauthorized ({status})")]
    Auth { status: u16 },
    /// `403`: the bot was removed from the group or has no rights.
    #[error("forbidden")]
    Forbidden,
    /// `409` on getUpdates: another instance or a webhook is active.
    #[error("conflict: another getUpdates consumer or a webhook is active")]
    Conflict,
    #[error("unexpected response")]
    Decode,
}

impl TelegramError {
    pub fn class(&self) -> &'static str {
        match self {
            Self::Transient { reason } => reason,
            Self::RateLimited { .. } => "rate_limited",
            Self::ThreadNotFound => "thread_not_found",
            Self::TopicClosed => "topic_closed",
            Self::CantParseEntities => "cant_parse_entities",
            Self::BadRequest { .. } => "bad_request",
            Self::Auth { .. } => "unauthorized",
            Self::Forbidden => "forbidden",
            Self::Conflict => "conflict",
            Self::Decode => "decode",
        }
    }

    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Transient { .. } | Self::Decode)
    }
}

pub type Result<T> = std::result::Result<T, TelegramError>;

pub trait TelegramApi: Send + Sync + 'static {
    fn get_updates(
        &self,
        offset: i64,
        timeout: Duration,
    ) -> impl Future<Output = Result<Vec<Update>>> + Send;

    /// Sends to a topic (`thread_id`) or to General (`None`). `html = false`
    /// sends plain text without `parse_mode`. Returns the `message_id`.
    fn send_message(
        &self,
        chat_id: i64,
        thread_id: Option<i64>,
        text: &str,
        html: bool,
    ) -> impl Future<Output = Result<i64>> + Send;

    /// Returns the `message_thread_id` of the new topic.
    fn create_forum_topic(
        &self,
        chat_id: i64,
        name: &str,
    ) -> impl Future<Output = Result<i64>> + Send;

    fn reopen_forum_topic(
        &self,
        chat_id: i64,
        thread_id: i64,
    ) -> impl Future<Output = Result<()>> + Send;

    /// Returns the bot user id.
    fn get_me(&self) -> impl Future<Output = Result<i64>> + Send;

    fn get_chat(&self, chat_id: i64) -> impl Future<Output = Result<ChatInfo>> + Send;

    fn get_chat_member(
        &self,
        chat_id: i64,
        user_id: i64,
    ) -> impl Future<Output = Result<MemberInfo>> + Send;
}
