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
    /// Heartbeat run that wrote the comment (`createdByRunId`).
    pub created_by_run_id: Option<String>,
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
/// behalf of that agent), and system notices (ADR-0010 §6.3, user decision in
/// PET-4: they come marked with ⚠️). User comments stay in Paperclip.
pub fn should_forward(c: &Comment, conversation_agent_id: &str) -> bool {
    if c.deleted {
        return false;
    }
    match c.author_type {
        AuthorType::Agent => c.author_agent_id.as_deref() == Some(conversation_agent_id),
        AuthorType::System => true,
        AuthorType::User | AuthorType::Other => false,
    }
}

/// Mark of a Paperclip service notice in the topic (user decision in PET-4).
pub const SERVICE_NOTICE_PREFIX: &str = "⚠️ ";
/// Service notices are one short line; longer comments are never looked up.
pub const SERVICE_NOTICE_MAX_CHARS: usize = 300;
/// Run statuses whose error text Paperclip posts into the conversation.
const FAILED_RUN_STATUSES: [&str; 3] = ["failed", "cancelled", "timed_out"];

/// The run behind a comment (`GET /api/heartbeat-runs/{id}`), only what the
/// service notice rule needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOutcome {
    pub status: String,
    pub error: Option<String>,
}

/// A system comment is a service notice by itself, no run lookup needed.
pub fn is_system_notice(c: &Comment) -> bool {
    c.author_type == AuthorType::System
}

/// Cheap pre-check before asking for the run: one short line written by a run.
pub fn may_be_service_notice(c: &Comment) -> bool {
    let body = c.body.trim();
    c.author_type == AuthorType::Agent
        && c.created_by_run_id.is_some()
        && !c.is_fast_response()
        && !body.is_empty()
        && !body.contains('\n')
        && body.chars().count() <= SERVICE_NOTICE_MAX_CHARS
}

/// A service notice (`ACP agent reported a terminal limit failure.`,
/// `cancelled`) carries the same author, origin and metadata as a reply of the
/// agent. It differs by its run: the run did not succeed and the comment
/// repeats the first line of the run error or the run status
/// (docs/paperclip-api.md). Case-insensitive.
pub fn is_service_notice(c: &Comment, run: &RunOutcome) -> bool {
    let status = run.status.to_lowercase();
    if !FAILED_RUN_STATUSES.contains(&status.as_str()) {
        return false;
    }
    let body = c.body.trim().to_lowercase();
    let first_error_line = run
        .error
        .as_deref()
        .and_then(|e| e.lines().next())
        .map(|l| l.trim().to_lowercase());
    body == status || first_error_line.as_deref() == Some(body.as_str())
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
            created_by_run_id: Some("run-1".into()),
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
    fn system_notices_are_forwarded() {
        let notice = comment(AuthorType::System, None, "comment");
        assert!(should_forward(&notice, "agent-1"));
        assert!(is_system_notice(&notice));
        assert!(!may_be_service_notice(&notice));
        let mut deleted = notice;
        deleted.deleted = true;
        assert!(!should_forward(&deleted, "agent-1"));
    }

    #[test]
    fn user_foreign_and_deleted_comments_are_not() {
        assert!(!should_forward(
            &comment(AuthorType::User, None, "comment"),
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

    fn run(status: &str, error: Option<&str>) -> RunOutcome {
        RunOutcome {
            status: status.into(),
            error: error.map(str::to_string),
        }
    }

    fn reply(body: &str) -> Comment {
        let mut c = comment(AuthorType::Agent, Some("agent-1"), "comment");
        c.body = body.into();
        c
    }

    #[test]
    fn service_notices_repeat_the_error_of_a_failed_run() {
        let limit = run(
            "failed",
            Some("ACP agent reported a terminal limit failure.\nYou've hit your session limit"),
        );
        let notice = reply("ACP agent reported a terminal limit failure.");
        assert!(may_be_service_notice(&notice));
        assert!(is_service_notice(&notice, &limit));
        let cancelled = run("cancelled", Some("Cancelled before issue reassignment"));
        assert!(is_service_notice(&reply("cancelled"), &cancelled));
    }

    #[test]
    fn ordinary_replies_are_not_service_notices() {
        let limit = run(
            "failed",
            Some("ACP agent reported a terminal limit failure."),
        );
        // A progress reply written by the run before it failed.
        assert!(!is_service_notice(
            &reply("Прогресс: ядро написано."),
            &limit
        ));
        // The same text from a successful run.
        let ok = run("succeeded", None);
        assert!(!is_service_notice(
            &reply("ACP agent reported a terminal limit failure."),
            &ok
        ));
        // Long or multi-line comments and receipts are never looked up.
        assert!(!may_be_service_notice(&reply("line one\nline two")));
        assert!(!may_be_service_notice(&reply(&"x".repeat(301))));
        let mut no_run = reply("cancelled");
        no_run.created_by_run_id = None;
        assert!(!may_be_service_notice(&no_run));
        let mut receipt = comment(AuthorType::Agent, Some("agent-1"), "fast_response");
        receipt.body = "Принято".into();
        assert!(!may_be_service_notice(&receipt));
    }
}
