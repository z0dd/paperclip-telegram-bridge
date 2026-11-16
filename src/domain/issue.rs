use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueSummary {
    pub id: String,
    pub identifier: Option<String>,
    pub title: String,
    pub status: String,
    pub priority: Option<String>,
    pub assignee_agent_id: Option<String>,
    pub updated_at: Option<String>,
}

impl IssueSummary {
    /// Lower is more urgent; unknown priorities go last.
    pub fn priority_rank(&self) -> u8 {
        match self.priority.as_deref() {
            Some("critical" | "urgent") => 0,
            Some("high") => 1,
            Some("medium") => 2,
            Some("low") => 3,
            _ => 4,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agent {
    pub id: String,
    pub name: String,
}

/// One element of the company `attention` feed relevant to the bridge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttentionItem {
    Interaction {
        interaction_id: String,
        issue_id: String,
        issue_ref: Option<String>,
        issue_title: Option<String>,
        activity_at: Option<String>,
    },
    FailedRun {
        run_id: String,
        agent_id: Option<String>,
        issue_id: Option<String>,
        issue_ref: Option<String>,
        error_code: Option<String>,
        activity_at: Option<String>,
    },
    /// Board approvals and unknown kinds: not delivered as cards.
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthInfo {
    pub status: String,
    pub version: Option<String>,
}
