//! Paperclip API behind a trait (architecture §2). Only `dto` knows the JSON.

pub mod dto;
pub mod http;
pub mod version;

use std::future::Future;
use std::time::Duration;

use crate::domain::{Agent, Chat, Comment, Company, HealthInfo, RunOutcome};

pub use http::HttpPaperclipClient;

/// Longest server error text kept (shown to the user in the topic).
pub const MESSAGE_MAX_CHARS: usize = 300;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PaperclipError {
    /// Timeout, connection error, `429`.
    #[error("transient error: {reason}")]
    Transient {
        reason: &'static str,
        retry_after: Option<Duration>,
    },
    /// `5xx`.
    #[error("server error {status}")]
    Server { status: u16 },
    /// `401`: the key is rejected.
    #[error("authorization failed (401)")]
    Auth,
    /// `403`. `message` is the server text, at most [`MESSAGE_MAX_CHARS`].
    #[error("forbidden")]
    Forbidden { message: String },
    /// `404`.
    #[error("not found")]
    NotFound { message: String },
    /// `409`.
    #[error("conflict")]
    Conflict { message: String },
    /// `400` / `422` and other `4xx`. The message is for the user, never logged.
    #[error("invalid request ({status})")]
    Invalid { status: u16, message: String },
    #[error("unexpected response")]
    Decode,
}

impl PaperclipError {
    /// Retry with backoff.
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Transient { .. } | Self::Server { .. })
    }

    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::Transient { retry_after, .. } => *retry_after,
            _ => None,
        }
    }

    /// Server text of a `4xx`, empty otherwise.
    pub fn message(&self) -> &str {
        match self {
            Self::Forbidden { message }
            | Self::NotFound { message }
            | Self::Conflict { message }
            | Self::Invalid { message, .. } => message,
            _ => "",
        }
    }

    /// Case-insensitive search in the server text.
    pub fn message_contains(&self, needle: &str) -> bool {
        self.message()
            .to_lowercase()
            .contains(&needle.to_lowercase())
    }

    /// `404 "Agent Chat is disabled"` (ADR-0010 §1.2).
    pub fn is_agent_chat_disabled(&self) -> bool {
        matches!(self, Self::NotFound { .. }) && self.message_contains("agent chat is disabled")
    }

    /// Short class for logs and `pc_outbox.last_error`.
    pub fn class(&self) -> String {
        match self {
            Self::Transient { reason, .. } => (*reason).to_string(),
            Self::Server { status } => format!("http_{status}"),
            Self::Auth => "auth_401".into(),
            Self::Forbidden { .. } => "http_403".into(),
            Self::NotFound { .. } => "http_404".into(),
            Self::Conflict { .. } => "http_409".into(),
            Self::Invalid { status, .. } => format!("http_{status}"),
            Self::Decode => "decode".into(),
        }
    }
}

pub type Result<T> = std::result::Result<T, PaperclipError>;

/// Everything the bridge calls in Paperclip with one board key
/// (docs/paperclip-api.md, ADR-0010 §1).
pub trait PaperclipApi: Send + Sync + 'static {
    fn health(&self) -> impl Future<Output = Result<HealthInfo>> + Send;

    /// `GET /api/companies?scope=accessible`: companies the key owner is a member of.
    fn companies(&self) -> impl Future<Output = Result<Vec<Company>>> + Send;

    fn agents(&self, company_id: &str) -> impl Future<Output = Result<Vec<Agent>>> + Send;

    /// `GET /api/companies/{id}/chats`: Agent Chat conversations of the key owner.
    fn chats(&self, company_id: &str) -> impl Future<Output = Result<Vec<Chat>>> + Send;

    /// `order=asc&limit=…[&after=…]`. The server returns the anchor itself
    /// again (ADR-0010 §1.4): callers drop already seen ids.
    fn comments_after(
        &self,
        issue_id: &str,
        after: Option<&str>,
        limit: u32,
    ) -> impl Future<Output = Result<Vec<Comment>>> + Send;

    /// `order=desc&limit=…`: the newest comments, newest first.
    fn comments_latest(
        &self,
        issue_id: &str,
        limit: u32,
    ) -> impl Future<Output = Result<Vec<Comment>>> + Send;

    /// `GET /api/heartbeat-runs/{id}`: status and error of the run that wrote
    /// a comment (service notices, docs/paperclip-api.md).
    fn run(&self, run_id: &str) -> impl Future<Output = Result<RunOutcome>> + Send;

    /// `POST /api/issues/{id}/comments` with `clientRequestId`. Returns the
    /// comment id. No `X-Paperclip-Run-Id`: this is the user writing.
    fn post_comment(
        &self,
        issue_id: &str,
        body: &str,
        client_request_id: &str,
    ) -> impl Future<Output = Result<Option<String>>> + Send;

    /// `POST /api/cli-auth/revoke-current`: revokes the key used for the request.
    fn revoke_current_key(&self) -> impl Future<Output = Result<()>> + Send;
}
