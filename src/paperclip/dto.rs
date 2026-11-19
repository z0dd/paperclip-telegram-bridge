//! Paperclip JSON ⇄ domain. The only module that knows the wire format
//! (docs/paperclip-api.md). Parsing is lenient: unknown fields are ignored,
//! list items without the required fields are skipped.

use serde::Deserialize;
use serde_json::Value;

use crate::domain::*;

fn s(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|x| !x.is_empty())
}

/// Lists come either as a bare array or wrapped in an object.
pub fn list(v: &Value, keys: &[&str]) -> Option<Vec<Value>> {
    if let Some(a) = v.as_array() {
        return Some(a.clone());
    }
    keys.iter()
        .find_map(|k| v.get(*k).and_then(Value::as_array))
        .cloned()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorBody {
    #[serde(default)]
    pub error: Option<String>,
}

pub fn health(v: &Value) -> HealthInfo {
    HealthInfo {
        status: s(v, "status").unwrap_or_default(),
        version: s(v, "version"),
    }
}

pub fn companies(v: &Value) -> Option<Vec<Company>> {
    let items = list(v, &["companies", "items", "data"])?;
    Some(
        items
            .iter()
            .filter_map(|c| {
                let id = s(c, "id")?;
                Some(Company {
                    name: s(c, "name").unwrap_or_else(|| id.clone()),
                    issue_prefix: s(c, "issuePrefix").unwrap_or_default(),
                    status: s(c, "status"),
                    id,
                })
            })
            .collect(),
    )
}

pub fn agents(v: &Value) -> Option<Vec<Agent>> {
    let items = list(v, &["agents", "items", "data"])?;
    Some(
        items
            .iter()
            .filter_map(|a| {
                Some(Agent {
                    id: s(a, "id")?,
                    name: s(a, "name")
                        .or_else(|| s(a, "displayName"))
                        .unwrap_or_else(|| "агент".into()),
                })
            })
            .collect(),
    )
}

/// Conversations: issues with `conversationAgentId` (ADR-0010 §1.2).
pub fn chats(v: &Value) -> Option<Vec<Chat>> {
    let items = list(v, &["chats", "issues", "items", "data"])?;
    Some(
        items
            .iter()
            .filter_map(|i| {
                Some(Chat {
                    issue_id: s(i, "id")?,
                    agent_id: s(i, "conversationAgentId")?,
                    identifier: s(i, "identifier"),
                    user_id: s(i, "conversationUserId"),
                    updated_at: s(i, "updatedAt"),
                })
            })
            .collect(),
    )
}

pub fn comment(c: &Value) -> Option<Comment> {
    Some(Comment {
        id: s(c, "id")?,
        body: c
            .get("body")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        author_type: AuthorType::parse(c.get("authorType").and_then(Value::as_str).unwrap_or("")),
        author_agent_id: s(c, "authorAgentId"),
        origin: s(c, "origin"),
        created_by_run_id: s(c, "createdByRunId"),
        created_at: s(c, "createdAt").unwrap_or_default(),
        deleted: c.get("deletedAt").is_some_and(|d| !d.is_null()),
    })
}

/// `GET /api/heartbeat-runs/{id}`: a run without `status` is not a run.
pub fn run(v: &Value) -> Option<RunOutcome> {
    Some(RunOutcome {
        status: s(v, "status")?,
        error: s(v, "error"),
    })
}

pub fn comments(v: &Value) -> Option<Vec<Comment>> {
    let items = list(v, &["comments", "items", "data"])?;
    Some(items.iter().filter_map(comment).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn comment_kinds_are_parsed() {
        let v = json!([
            {"id": "c1", "body": "**hi**", "authorType": "agent", "authorAgentId": "a1",
             "origin": "comment", "createdByRunId": "r1",
             "createdAt": "2026-11-19T10:00:00.000Z", "deletedAt": null},
            {"id": "c2", "body": "Принято", "authorType": "agent", "authorAgentId": "a1",
             "origin": "fast_response", "fastResponseRequestId": "f1"},
            {"id": "c3", "body": "hello", "authorType": "user", "authorUserId": "u1",
             "clientRequestId": "x"},
            {"id": "c4", "body": "notice", "authorType": "system",
             "presentation": {"kind": "system_notice"}},
            {"id": "c5", "body": "gone", "authorType": "agent", "authorAgentId": "a1",
             "deletedAt": "2026-11-19T10:00:00.000Z"},
            {"body": "no id"}
        ]);
        let c = comments(&v).unwrap();
        assert_eq!(c.len(), 5);
        assert_eq!(c[0].author_type, AuthorType::Agent);
        assert_eq!(c[0].author_agent_id.as_deref(), Some("a1"));
        assert!(!c[0].deleted);
        assert!(c[1].is_fast_response());
        assert_eq!(c[0].created_by_run_id.as_deref(), Some("r1"));
        assert_eq!(c[1].created_by_run_id, None);
        assert_eq!(c[2].author_type, AuthorType::User);
        assert_eq!(c[3].author_type, AuthorType::System);
        assert!(c[4].deleted);
    }

    #[test]
    fn runs_are_parsed() {
        let r = run(&json!({"id": "r1", "status": "failed", "error": "boom\nmore"})).unwrap();
        assert_eq!(r.status, "failed");
        assert_eq!(r.error.as_deref(), Some("boom\nmore"));
        assert!(run(&json!({"error": "API route not found"})).is_none());
    }

    #[test]
    fn chats_require_a_conversation_agent() {
        let v = json!([
            {"id": "i1", "identifier": "PET-4", "conversationAgentId": "a1",
             "conversationUserId": "u1", "updatedAt": "2026-11-19T10:00:00.000Z"},
            {"id": "i2", "identifier": "PET-5", "conversationAgentId": null}
        ]);
        let c = chats(&v).unwrap();
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].identifier.as_deref(), Some("PET-4"));
        assert_eq!(c[0].user_id.as_deref(), Some("u1"));
    }

    #[test]
    fn companies_are_parsed() {
        let v = json!([{"id": "c1", "name": "PET", "issuePrefix": "PET", "status": "active"},
                       {"name": "no id"}]);
        let c = companies(&v).unwrap();
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].issue_prefix, "PET");
        assert!(c[0].is_active());
        assert!(companies(&json!({"error": "x"})).is_none());
    }
}
