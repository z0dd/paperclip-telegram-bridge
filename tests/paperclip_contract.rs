//! Contract tests of `HttpPaperclipClient` against wiremock: request shape
//! of the v2 endpoints (docs/paperclip-api.md, ADR-0010 §1) and error
//! classification. All ids are placeholders.

use std::time::Duration;

use bridge::domain::*;
use bridge::paperclip::{HttpPaperclipClient, PaperclipApi, PaperclipError};
use secrecy::SecretString;
use serde_json::{Value, json};
use wiremock::matchers::{body_json, header, method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

const ISSUE: &str = "issue-1";
const COMPANY: &str = "company-1";

fn client(server: &MockServer) -> HttpPaperclipClient {
    client_with_timeout(server, Duration::from_secs(5))
}

fn client_with_timeout(server: &MockServer, timeout: Duration) -> HttpPaperclipClient {
    HttpPaperclipClient::new(
        &server.uri().parse().unwrap(),
        SecretString::from("pcp_board_placeholder"),
        Duration::from_secs(2),
        timeout,
    )
    .unwrap()
}

#[tokio::test]
async fn health_is_parsed() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/health"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "ok", "version": "2026.1106.0",
            "deploymentMode": "authenticated", "deploymentExposure": "private"
        })))
        .mount(&server)
        .await;
    let h = client(&server).health().await.unwrap();
    assert_eq!(h.status, "ok");
    assert_eq!(h.version.as_deref(), Some("2026.1106.0"));
}

#[tokio::test]
async fn companies_use_scope_accessible_and_bearer_token() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/companies"))
        .and(query_param("scope", "accessible"))
        .and(header("authorization", "Bearer pcp_board_placeholder"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"id": "c-1", "name": "PET", "issuePrefix": "PET", "status": "active"},
            {"id": "c-2", "name": "Old", "issuePrefix": "OLD", "status": "archived"}
        ])))
        .expect(1)
        .mount(&server)
        .await;
    let list = client(&server).companies().await.unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].issue_prefix, "PET");
    assert!(list[0].is_active());
    assert!(!list[1].is_active());
}

#[tokio::test]
async fn chats_and_agents_are_parsed() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/api/companies/{COMPANY}/chats")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"id": ISSUE, "identifier": "PET-4", "title": "Chat with PET Lead",
             "conversationAgentId": "agent-1", "conversationUserId": "user-1",
             "conversationState": "waiting", "conversationSessionGeneration": 0,
             "updatedAt": "2026-11-19T13:56:56.463Z"}
        ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/api/companies/{COMPANY}/agents")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"id": "agent-1", "name": "PET Lead", "role": "ceo"}
        ])))
        .mount(&server)
        .await;
    let c = client(&server);
    let chats = c.chats(COMPANY).await.unwrap();
    assert_eq!(
        chats,
        vec![Chat {
            issue_id: ISSUE.into(),
            identifier: Some("PET-4".into()),
            agent_id: "agent-1".into(),
            user_id: Some("user-1".into()),
            updated_at: Some("2026-11-19T13:56:56.463Z".into()),
        }]
    );
    assert_eq!(c.agents(COMPANY).await.unwrap()[0].name, "PET Lead");
}

fn comment(id: &str) -> Value {
    json!({"id": id, "body": "text", "authorType": "agent", "authorAgentId": "agent-1",
           "origin": "comment", "createdAt": "2026-11-19T10:00:00.000Z", "deletedAt": null})
}

#[tokio::test]
async fn comments_cursor_and_latest_queries() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/api/issues/{ISSUE}/comments")))
        .and(query_param("order", "asc"))
        .and(query_param("after", "c-1"))
        .and(query_param("limit", "100"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!([comment("c-1"), comment("c-2")])),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/api/issues/{ISSUE}/comments")))
        .and(query_param("order", "asc"))
        .and(query_param_is_missing("after"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([comment("c-0")])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/api/issues/{ISSUE}/comments")))
        .and(query_param("order", "desc"))
        .and(query_param("limit", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([comment("c-9")])))
        .expect(1)
        .mount(&server)
        .await;
    let c = client(&server);
    let page = c.comments_after(ISSUE, Some("c-1"), 100).await.unwrap();
    assert_eq!(
        page.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
        ["c-1", "c-2"]
    );
    assert_eq!(page[0].author_type, AuthorType::Agent);
    assert_eq!(
        c.comments_after(ISSUE, None, 100).await.unwrap()[0].id,
        "c-0"
    );
    assert_eq!(c.comments_latest(ISSUE, 1).await.unwrap()[0].id, "c-9");
}

#[tokio::test]
async fn post_comment_sends_client_request_id_without_run_id() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!("/api/issues/{ISSUE}/comments")))
        .and(body_json(
            json!({"body": "Привет", "clientRequestId": "6b0c…-placeholder"}),
        ))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": "c-new", "body": "Привет", "authorType": "user", "authorUserId": "user-1",
            "clientRequestId": "6b0c…-placeholder"})))
        .expect(1)
        .mount(&server)
        .await;
    let id = client(&server)
        .post_comment(ISSUE, "Привет", "6b0c…-placeholder")
        .await
        .unwrap();
    assert_eq!(id.as_deref(), Some("c-new"));
    let reqs = server.received_requests().await.unwrap();
    assert!(!reqs[0].headers.contains_key("x-paperclip-run-id"));
}

async fn error_for(status: u16, body: Value) -> PaperclipError {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(&server)
        .await;
    client(&server)
        .post_comment(ISSUE, "x", "id")
        .await
        .unwrap_err()
}

#[tokio::test]
async fn errors_keep_the_server_text_for_classification() {
    let e = error_for(
        409,
        json!({"error": "Task is paused. Resume it before sending a message."}),
    )
    .await;
    assert!(matches!(e, PaperclipError::Conflict { .. }));
    assert!(e.message_contains("PAUSED"));
    let e = error_for(
        403,
        json!({"error": "Only the conversation owner can send messages to this chat"}),
    )
    .await;
    assert!(matches!(e, PaperclipError::Forbidden { .. }));
    let e = error_for(404, json!({"error": "Agent Chat is disabled"})).await;
    assert!(e.is_agent_chat_disabled());
    let e = error_for(404, json!({"error": "Issue not found"})).await;
    assert!(!e.is_agent_chat_disabled());
    let e = error_for(422, json!({"error": "x".repeat(1000)})).await;
    assert!(matches!(e, PaperclipError::Invalid { status: 422, .. }));
    assert_eq!(e.message().chars().count(), 300);
    assert_eq!(
        error_for(401, json!({"error": "Unauthorized"})).await,
        PaperclipError::Auth
    );
    assert_eq!(
        error_for(503, json!({})).await,
        PaperclipError::Server { status: 503 }
    );
    let e = error_for(500, json!({"error": "secret stack"})).await;
    assert_eq!(e.message(), "", "5xx text is not kept");
}

#[tokio::test]
async fn rate_limit_carries_retry_after() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("Retry-After", "7")
                .set_body_json(json!({"error": "Too many requests"})),
        )
        .mount(&server)
        .await;
    let e = client(&server).chats(COMPANY).await.unwrap_err();
    assert!(e.is_transient());
    assert_eq!(e.retry_after(), Some(Duration::from_secs(7)));
}

#[tokio::test]
async fn malformed_lists_are_decode_errors() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"unexpected": true})))
        .mount(&server)
        .await;
    assert_eq!(
        client(&server).chats(COMPANY).await.unwrap_err(),
        PaperclipError::Decode
    );
}

#[tokio::test]
async fn timeout_is_transient() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"status": "ok"}))
                .set_delay(Duration::from_secs(3)),
        )
        .mount(&server)
        .await;
    let e = client_with_timeout(&server, Duration::from_millis(300))
        .health()
        .await
        .unwrap_err();
    assert_eq!(
        e,
        PaperclipError::Transient {
            reason: "timeout",
            retry_after: None
        }
    );
}

#[tokio::test]
async fn unreachable_paperclip_is_transient_and_leaks_nothing() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let c = HttpPaperclipClient::new(
        &format!("http://127.0.0.1:{port}").parse().unwrap(),
        SecretString::from("pcp_board_placeholder"),
        Duration::from_secs(1),
        Duration::from_secs(1),
    )
    .unwrap();
    let e = c.companies().await.unwrap_err();
    assert!(e.is_transient());
    let text = format!("{e} {e:?}");
    assert!(!text.contains("127.0.0.1"), "{text}");
    assert!(!text.contains("pcp_board"), "{text}");
}

#[tokio::test]
async fn revoke_current_key() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/cli-auth/revoke-current"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"revoked": true})))
        .expect(1)
        .mount(&server)
        .await;
    client(&server).revoke_current_key().await.unwrap();
}

#[tokio::test]
async fn base_url_with_path_prefix_is_respected() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/paperclip/api/health"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status": "ok"})))
        .expect(1)
        .mount(&server)
        .await;
    let c = HttpPaperclipClient::new(
        &format!("{}/paperclip", server.uri()).parse().unwrap(),
        SecretString::from("x"),
        Duration::from_secs(1),
        Duration::from_secs(1),
    )
    .unwrap();
    c.health().await.unwrap();
}
