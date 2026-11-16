use serde::{Deserialize, Serialize};

/// A heartbeat run, without `error` / `stderrExcerpt` (may contain secrets).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunInfo {
    pub id: String,
    pub agent_id: Option<String>,
    pub issue_id: Option<String>,
    pub status: String,
    pub error_code: Option<String>,
    pub created_at: Option<String>,
    pub finished_at: Option<String>,
    pub scheduled_retry_at: Option<String>,
}

impl RunInfo {
    /// `failed` and `timed_out` are reported; `cancelled` / `interrupted` are not.
    pub fn is_failure(&self) -> bool {
        matches!(self.status.as_str(), "failed" | "timed_out")
    }

    pub fn is_live(&self) -> bool {
        matches!(self.status.as_str(), "queued" | "running")
    }
}

/// A failure to report to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunFailure {
    pub run_id: String,
    pub company_id: String,
    pub agent_id: Option<String>,
    pub issue_id: Option<String>,
    pub issue_ref: Option<String>,
    pub status: String,
    pub error_code: Option<String>,
    pub at: Option<String>,
}
