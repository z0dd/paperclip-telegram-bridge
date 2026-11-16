//! Paperclip fixtures for wiremock (placeholders only).

use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

pub const COMPANY: &str = "company-1";
pub const ISSUE: &str = "issue-1";

pub fn card(id: &str, kind: &str, status: &str, payload: Value) -> Value {
    json!({
        "id": id, "companyId": COMPANY, "issueId": ISSUE,
        "kind": kind, "status": status,
        "title": "Card title",
        "createdByAgentId": "agent-1",
        "resolvedByAgentId": null, "resolvedByUserId": null,
        "addresseeAgentId": null, "addresseeUserId": null,
        "payload": payload, "result": null,
        "createdAt": format!("2026-11-15T10:00:00.{:0>3}Z", id.len()),
        "updatedAt": "2026-11-15T10:00:00.000Z"
    })
}

pub fn confirmation(id: &str) -> Value {
    card(
        id,
        "request_confirmation",
        "pending",
        json!({"version": 1, "prompt": "Ship it?", "acceptLabel": "Approve", "rejectLabel": "Reject",
               "rejectRequiresReason": false, "allowDeclineReason": false}),
    )
}

pub fn resolved(
    mut c: Value,
    status: &str,
    by_user: Option<&str>,
    by_agent: Option<&str>,
    result: Value,
) -> Value {
    c["status"] = json!(status);
    c["resolvedByUserId"] = json!(by_user);
    c["resolvedByAgentId"] = json!(by_agent);
    c["result"] = result;
    c
}

pub fn attention_item(interaction_id: &str) -> Value {
    json!({
        "id": format!("issue_thread_interaction:interaction:{interaction_id}"),
        "sourceKind": "issue_thread_interaction",
        "activityAt": "2026-11-15T11:59:00.000Z",
        "subject": {"kind": "interaction", "id": interaction_id, "status": "pending",
                    "metadata": {"issueId": ISSUE}},
        "relatedIssue": {"id": ISSUE, "identifier": "PET-1", "title": "Issue title"}
    })
}

pub fn attention(items: Vec<Value>) -> Value {
    json!({"companyId": COMPANY, "totalCount": items.len(), "nextCursor": null, "items": items})
}

/// Replaces all mocks: feed with `feed_ids`, issue interactions `cards`.
pub async fn serve(server: &MockServer, feed_ids: &[&str], cards: Vec<Value>) {
    server.reset().await;
    mount_base(server, feed_ids, cards).await;
}

pub async fn mount_base(server: &MockServer, feed_ids: &[&str], cards: Vec<Value>) {
    Mock::given(method("GET"))
        .and(path(format!("/api/companies/{COMPANY}/attention")))
        .respond_with(ResponseTemplate::new(200).set_body_json(attention(
            feed_ids.iter().map(|id| attention_item(id)).collect(),
        )))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/api/issues/{ISSUE}/interactions")))
        .respond_with(ResponseTemplate::new(200).set_body_json(Value::Array(cards)))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/api/companies/{COMPANY}/agents")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!([{"id": "agent-1", "name": "Architect"}])),
        )
        .mount(server)
        .await;
}

pub async fn posts(server: &MockServer, suffix: &str) -> Vec<Value> {
    server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.method.as_str() == "POST" && r.url.path().ends_with(suffix))
        .map(|r| serde_json::from_slice(&r.body).unwrap_or(Value::Null))
        .collect()
}
