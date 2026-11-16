//! Telegram Bot API behind a trait. The only implementation always goes
//! through `TELEGRAM_PROXY_URL` (architecture §5.1).

pub mod http;
pub mod types;

use std::future::Future;
use std::time::Duration;

pub use http::HttpTelegramClient;
pub use types::*;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TelegramError {
    /// Timeout, connection or proxy error, `5xx`. Never contains the URL.
    #[error("transient error: {reason}")]
    Transient { reason: &'static str },
    #[error("rate limited for {retry_after:?}")]
    RateLimited { retry_after: Duration },
    /// `400 message is not modified` — harmless.
    #[error("message is not modified")]
    NotModified,
    /// `400 message to edit not found` — send a new message instead.
    #[error("message not found")]
    MessageNotFound,
    /// `400 query is too old` — callback can no longer be answered.
    #[error("callback query expired")]
    CallbackExpired,
    #[error("bad request")]
    BadRequest { description: String },
    /// `401` / `404`: the bot token is invalid.
    #[error("unauthorized ({status})")]
    Auth { status: u16 },
    /// `403`: e.g. the user blocked the bot.
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
            Self::NotModified => "not_modified",
            Self::MessageNotFound => "message_not_found",
            Self::CallbackExpired => "callback_expired",
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

    /// Returns the `message_id` of the sent message.
    fn send_message(
        &self,
        chat_id: i64,
        html: &str,
        markup: Option<&ReplyMarkup>,
    ) -> impl Future<Output = Result<i64>> + Send;

    fn edit_message_text(
        &self,
        chat_id: i64,
        message_id: i64,
        html: &str,
        keyboard: Option<&InlineKeyboard>,
    ) -> impl Future<Output = Result<()>> + Send;

    fn edit_reply_markup(
        &self,
        chat_id: i64,
        message_id: i64,
        keyboard: Option<&InlineKeyboard>,
    ) -> impl Future<Output = Result<()>> + Send;

    fn answer_callback_query(
        &self,
        callback_query_id: &str,
        text: Option<&str>,
    ) -> impl Future<Output = Result<()>> + Send;

    fn set_my_commands(
        &self,
        chat_id: i64,
        commands: &[BotCommand],
    ) -> impl Future<Output = Result<()>> + Send;

    fn get_me(&self) -> impl Future<Output = Result<()>> + Send;
}
