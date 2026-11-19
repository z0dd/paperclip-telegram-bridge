//! Issue comments and the rule which of them go to Telegram (ADR-0010 §1.6).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorType {
    Agent,
    User,
    System,
    Other,
}

impl AuthorType {
    pub fn parse(s: &str) -> Self {
        match s {
            "agent" => Self::Agent,
            "user" => Self::User,
            "system" => Self::System,
            _ => Self::Other,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    pub id: String,
    pub body: String,
    pub author_type: AuthorType,
    pub author_agent_id: Option<String>,
    /// `comment`, `fast_response`, …
    pub origin: Option<String>,
    pub created_at: String,
    pub deleted: bool,
}

impl Comment {
    pub fn is_fast_response(&self) -> bool {
        self.origin.as_deref() == Some("fast_response")
    }
}

/// The only place that decides what is mirrored into the topic: replies of
/// the conversation agent, Paperclip receipts included (they are written on
/// behalf of that agent). User and system comments stay in Paperclip
/// (ADR-0010 §6.3: system notices are not forwarded by default).
pub fn should_forward(c: &Comment, conversation_agent_id: &str) -> bool {
    !c.deleted
        && c.author_type == AuthorType::Agent
        && c.author_agent_id.as_deref() == Some(conversation_agent_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comment(author_type: AuthorType, agent: Option<&str>, origin: &str) -> Comment {
        Comment {
            id: "c".into(),
            body: "text".into(),
            author_type,
            author_agent_id: agent.map(str::to_string),
            origin: Some(origin.into()),
            created_at: "2026-11-19T10:00:00.000Z".into(),
            deleted: false,
        }
    }

    #[test]
    fn agent_replies_and_receipts_are_forwarded() {
        assert!(should_forward(
            &comment(AuthorType::Agent, Some("agent-1"), "comment"),
            "agent-1"
        ));
        let receipt = comment(AuthorType::Agent, Some("agent-1"), "fast_response");
        assert!(receipt.is_fast_response());
        assert!(should_forward(&receipt, "agent-1"));
    }

    #[test]
    fn user_system_foreign_and_deleted_comments_are_not() {
        assert!(!should_forward(
            &comment(AuthorType::User, None, "comment"),
            "agent-1"
        ));
        assert!(!should_forward(
            &comment(AuthorType::System, None, "comment"),
            "agent-1"
        ));
        assert!(!should_forward(
            &comment(AuthorType::Agent, Some("agent-2"), "comment"),
            "agent-1"
        ));
        assert!(!should_forward(
            &comment(AuthorType::Agent, None, "comment"),
            "agent-1"
        ));
        let mut deleted = comment(AuthorType::Agent, Some("agent-1"), "comment");
        deleted.deleted = true;
        assert!(!should_forward(&deleted, "agent-1"));
    }
}
