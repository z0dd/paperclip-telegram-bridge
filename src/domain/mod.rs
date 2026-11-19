//! Domain types. Depend neither on Paperclip JSON nor on Telegram.

pub mod comment;

pub use comment::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthInfo {
    pub status: String,
    pub version: Option<String>,
}

/// A company visible to a board key (`GET /api/companies?scope=accessible`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Company {
    pub id: String,
    pub name: String,
    pub issue_prefix: String,
    pub status: Option<String>,
}

impl Company {
    /// Archived companies are not polled.
    pub fn is_active(&self) -> bool {
        self.status.as_deref() != Some("archived")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agent {
    pub id: String,
    pub name: String,
}

/// An Agent Chat conversation of the key owner (`GET /api/companies/{id}/chats`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chat {
    pub issue_id: String,
    pub identifier: Option<String>,
    pub agent_id: String,
    pub user_id: Option<String>,
    pub updated_at: Option<String>,
}
