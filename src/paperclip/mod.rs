//! Paperclip API behind a trait (architecture §2). Only `dto` knows the JSON.

pub mod dto;
pub mod http;
pub mod version;

use std::future::Future;
use std::time::Duration;

use crate::domain::{
    Agent, Approval, AttentionItem, HealthInfo, Interaction, IssueSummary, Resolution, RunInfo,
};

pub use http::HttpPaperclipClient;

/// `409` codes meaning "the card is already resolved" (API §7).
pub const RESOLVED_CODES: [&str; 4] = [
    "interaction_already_resolved",
    "interaction_superseded",
    "interaction_issue_closed",
    "interaction_stale_target",
];

/// `403` codes that are permanent for one card, not a key problem (architecture §7.2).
pub const AUDIENCE_CODES: [&str; 4] = [
    "interaction_human_only",
    "interaction_addressee_mismatch",
    "interaction_creator_excluded",
    "interaction_governed_action_denied",
];

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
    /// `409` without a resolution code.
    #[error("conflict without code")]
    Conflict,
    /// `409` with an `interaction_*` code: permanent outcome, reconcile.
    #[error("already resolved ({code})")]
    Resolved { code: String },
    /// `401`, or `403` without an audience code.
    #[error("authorization failed ({status})")]
    Auth { status: u16 },
    /// `403` with an audience code: this key may not answer this card.
    #[error("audience denied ({code})")]
    AudienceDenied { code: String },
    #[error("not found")]
    NotFound,
    /// `400` / `422`. `message` is the server text for the user, never logged.
    #[error("invalid request ({status})")]
    Invalid {
        status: u16,
        code: Option<String>,
        message: String,
    },
    #[error("unexpected response")]
    Decode,
}

impl PaperclipError {
    /// Retry with backoff (for ordinary cards and reads).
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            Self::Transient { .. } | Self::Server { .. } | Self::Conflict
        )
    }

    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::Transient { retry_after, .. } => *retry_after,
            _ => None,
        }
    }

    /// Short class for logs and `outbox.last_error`.
    pub fn class(&self) -> String {
        match self {
            Self::Transient { reason, .. } => (*reason).to_string(),
            Self::Server { status } => format!("http_{status}"),
            Self::Conflict => "http_409".into(),
            Self::Resolved { code } => code.clone(),
            Self::Auth { status } => format!("auth_{status}"),
            Self::AudienceDenied { code } => code.clone(),
            Self::NotFound => "http_404".into(),
            Self::Invalid { status, .. } => format!("http_{status}"),
            Self::Decode => "decode".into(),
        }
    }
}

pub type Result<T> = std::result::Result<T, PaperclipError>;

/// Everything the bridge calls in Paperclip (API §2, "фактически используемые операции").
pub trait PaperclipApi: Send + Sync + 'static {
    fn health(&self) -> impl Future<Output = Result<HealthInfo>> + Send;

    /// First page of the company attention feed (`limit=100&sort=activity`),
    /// continued by cursor when `totalCount` is larger; a stale cursor (`400`)
    /// restarts from the first page.
    fn attention(
        &self,
        company_id: &str,
    ) -> impl Future<Output = Result<Vec<AttentionItem>>> + Send;

    /// All interactions of an issue, pending and resolved.
    fn interactions(&self, issue_id: &str)
    -> impl Future<Output = Result<Vec<Interaction>>> + Send;

    /// `respond` / `accept` / `reject` / `verdicts` for ordinary cards.
    /// Returns the updated record when the server sends one.
    fn resolve(
        &self,
        issue_id: &str,
        interaction_id: &str,
        resolution: &Resolution,
    ) -> impl Future<Output = Result<Option<Interaction>>> + Send;

    /// Governed accept: body `{}` (never `rememberAction`), long timeout.
    fn governed_accept(
        &self,
        issue_id: &str,
        interaction_id: &str,
    ) -> impl Future<Output = Result<Option<Interaction>>> + Send;

    fn governed_reject(
        &self,
        issue_id: &str,
        interaction_id: &str,
        reason: Option<&str>,
    ) -> impl Future<Output = Result<Option<Interaction>>> + Send;

    fn issue_approvals(&self, issue_id: &str)
    -> impl Future<Output = Result<Vec<Approval>>> + Send;

    fn approve_approval(
        &self,
        approval_id: &str,
        decision_note: &str,
    ) -> impl Future<Output = Result<()>> + Send;

    fn reject_approval(
        &self,
        approval_id: &str,
        decision_note: &str,
    ) -> impl Future<Output = Result<()>> + Send;

    fn issues(
        &self,
        company_id: &str,
        statuses: &[&str],
        limit: u32,
    ) -> impl Future<Output = Result<Vec<IssueSummary>>> + Send;

    fn heartbeat_runs(
        &self,
        company_id: &str,
        limit: u32,
    ) -> impl Future<Output = Result<Vec<RunInfo>>> + Send;

    fn live_runs(&self, company_id: &str) -> impl Future<Output = Result<Vec<RunInfo>>> + Send;

    fn agents(&self, company_id: &str) -> impl Future<Output = Result<Vec<Agent>>> + Send;

    /// `POST /api/cli-auth/revoke-current`: revokes the key used for the request.
    fn revoke_current_key(&self) -> impl Future<Output = Result<()>> + Send;
}
