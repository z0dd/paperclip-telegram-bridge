//! Contract tests of `HttpPaperclipClient` against wiremock, fixtures from
//! docs/paperclip-api.md. All ids are placeholders.

use std::time::Duration;

use bridge::domain::*;
use bridge::paperclip::{HttpPaperclipClient, PaperclipApi, PaperclipError};
use secrecy::SecretString;
use serde_json::{Value, json};
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const ISSUE: &str = "issue-1";
const CARD: &str = "card-1";

fn client(server: &MockServer) -> HttpPaperclipClient {
    client_with_timeout(server, Duration::from_secs(5))
}

fn client_with_timeout(server: &MockServer, timeout: Duration) -> HttpPaperclipClient {
    HttpPaperclipClient::new(
        &server.uri().parse().unwrap(),
        SecretString::from("pcp_board_placeholder"),
        Duration::from_secs(2),
        timeout,
        timeout * 2,
    )
    .unwrap()
}

fn card(kind: &str, status: &str, payload: Value, result: Value) -> Value {
    json!({
        "id": CARD, "companyId": "company-1", "issueId": ISSUE,
        "kind": kind, "status": status,
        "continuationPolicy": "none",
        "title": "Card title",
        "createdByAgentId": "agent-1", "createdByUserId": null,
        "resolvedByAgentId": null, "resolvedByRunId": null, "resolvedByUserId": null,
        "payload": payload, "result": result,
        "createdAt": "2026-11-15T17:28:52.897Z", "updatedAt": "2026-11-15T17:28:52.897Z"
    })
}

async fn one_card(payload_kind: &str, payload: Value) -> Interaction {
    one_card_with_result(payload_kind, payload, Value::Null).await
}

async fn one_card_with_result(payload_kind: &str, payload: Value, result: Value) -> Interaction {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/api/issues/{ISSUE}/interactions")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([card(
            payload_kind,
            "pending",
            payload,
            result
        )])))
        .mount(&server)
        .await;
    let mut list = client(&server).interactions(ISSUE).await.unwrap();
    assert_eq!(list.len(), 1);
    list.remove(0)
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
async fn sends_bearer_token_and_no_run_id_header() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/health"))
        .and(header("authorization", "Bearer pcp_board_placeholder"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status": "ok"})))
        .expect(1)
        .mount(&server)
        .await;
    client(&server).health().await.unwrap();
    let reqs = server.received_requests().await.unwrap();
    assert!(!reqs[0].headers.contains_key("x-paperclip-run-id"));
}

fn attention_body(items: Vec<Value>, total: u64, next: Option<&str>) -> Value {
    json!({
        "companyId": "company-1", "generatedAt": "2026-11-15T12:00:00.000Z",
        "totalCount": total, "deskBadgeCount": total, "nextCursor": next,
        "items": items
    })
}

fn attention_interaction(id: &str) -> Value {
    json!({
        "id": format!("issue_thread_interaction:interaction:{id}"),
        "sourceKind": "issue_thread_interaction",
        "activityAt": "2026-11-15T11:59:00.000Z",
        "decisionVerbs": ["accept", "reject"],
        "subject": {"kind": "interaction", "id": id, "status": "pending", "title": "…",
                    "metadata": {"kind": "request_confirmation", "issueId": ISSUE, "createdByAgentId": "agent-1"}},
        "relatedIssue": {"id": ISSUE, "identifier": "PET-1", "title": "Issue title"},
        "resolverAudience": {"effectivePolicy": "human_only"}
    })
}

#[tokio::test]
async fn attention_maps_all_source_kinds() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/companies/company-1/attention"))
        .and(query_param("limit", "100"))
        .and(query_param("sort", "activity"))
        .respond_with(ResponseTemplate::new(200).set_body_json(attention_body(
            vec![
                attention_interaction("card-1"),
                json!({
                    "id": "failed_run:run:run-9", "sourceKind": "failed_run",
                    "activityAt": "2026-11-15T11:00:00.000Z",
                    "subject": {"kind": "run", "id": "run-9",
                                "metadata": {"agentId": "agent-1", "issueId": ISSUE, "errorCode": "adapter_failed",
                                             "error": "secret stderr must not be read"}},
                    "relatedIssue": {"id": ISSUE, "identifier": "PET-1", "title": "Issue"}
                }),
                json!({"id": "approval:approval-1", "sourceKind": "approval",
                       "subject": {"kind": "approval", "id": "approval-1"}}),
                json!({"id": "x", "sourceKind": "something_new"}),
            ],
            4,
            None,
        )))
        .mount(&server)
        .await;
    let items = client(&server).attention("company-1").await.unwrap();
    assert_eq!(items.len(), 4);
    assert_eq!(
        items[0],
        AttentionItem::Interaction {
            interaction_id: "card-1".into(),
            issue_id: ISSUE.into(),
            issue_ref: Some("PET-1".into()),
            issue_title: Some("Issue title".into()),
            activity_at: Some("2026-11-15T11:59:00.000Z".into()),
        }
    );
    match &items[1] {
        AttentionItem::FailedRun {
            run_id, error_code, ..
        } => {
            assert_eq!(run_id, "run-9");
            assert_eq!(error_code.as_deref(), Some("adapter_failed"));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(items[2], AttentionItem::Other);
    assert_eq!(items[3], AttentionItem::Other);
}

#[tokio::test]
async fn attention_follows_cursor_and_restarts_on_stale_cursor() {
    let server = MockServer::start().await;
    // Stale cursor → 400; first page is requested again.
    Mock::given(method("GET"))
        .and(path("/api/companies/company-1/attention"))
        .and(query_param("cursor", "stale"))
        .respond_with(ResponseTemplate::new(400).set_body_json(
            json!({"error": "Attention cursor no longer matches the filtered feed"}),
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/companies/company-1/attention"))
        .and(query_param("cursor", "page2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(attention_body(
            vec![attention_interaction("card-2")],
            2,
            None,
        )))
        .mount(&server)
        .await;
    // First page: first call returns a stale cursor, later calls a good one.
    Mock::given(method("GET"))
        .and(path("/api/companies/company-1/attention"))
        .respond_with(ResponseTemplate::new(200).set_body_json(attention_body(
            vec![attention_interaction("card-1")],
            2,
            Some("stale"),
        )))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/companies/company-1/attention"))
        .respond_with(ResponseTemplate::new(200).set_body_json(attention_body(
            vec![attention_interaction("card-1")],
            2,
            Some("page2"),
        )))
        .mount(&server)
        .await;
    let items = client(&server).attention("company-1").await.unwrap();
    let ids: Vec<_> = items
        .iter()
        .filter_map(|i| match i {
            AttentionItem::Interaction { interaction_id, .. } => Some(interaction_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(ids, vec!["card-1", "card-2"]);
    assert_eq!(server.received_requests().await.unwrap().len(), 4);
}

#[tokio::test]
async fn question_set_form_is_parsed() {
    let i = one_card(
        "ask_user_questions",
        json!({
            "version": 1,
            "questionSet": {
                "schema": "paperclip.question_set.v1",
                "title": "Form", "description": "Long description", "submitLabel": "Send",
                "questions": [
                    {"id": "license", "prompt": "Licence?", "required": true, "answerMode": "single_select",
                     "options": [{"id": "apache2", "label": "Apache-2.0", "description": "permissive"}, {"id": "mit", "label": "MIT"}],
                     "customAnswer": {"enabled": true, "label": "Other"}},
                    {"id": "langs", "prompt": "Languages?", "required": false, "answerMode": "multi_select",
                     "options": [{"id": "rs", "label": "Rust"}, {"id": "go", "label": "Go"}]},
                    {"id": "notes", "prompt": "Notes", "helpText": "free", "required": true, "answerMode": "text",
                     "textValidation": {"minLength": 2, "maxLength": 10, "inputType": "integer", "minimum": 1, "maximum": 99}}
                ]
            },
            "questions": [{"id": "license", "prompt": "Licence?", "selectionMode": "single", "required": true,
                           "allowOther": true, "options": []}]
        }),
    )
    .await;
    assert_eq!(i.kind, InteractionKind::AskUserQuestions);
    let InteractionBody::Questions(form) = i.body else {
        panic!()
    };
    assert_eq!(form.title.as_deref(), Some("Form"));
    assert_eq!(form.questions.len(), 3);
    let q0 = &form.questions[0];
    assert_eq!(q0.mode, AnswerMode::Single);
    assert!(q0.required);
    assert_eq!(q0.options.len(), 2);
    assert_eq!(
        q0.custom_answer.as_ref().unwrap().label.as_deref(),
        Some("Other")
    );
    assert_eq!(form.questions[1].mode, AnswerMode::Multi);
    let q2 = &form.questions[2];
    assert_eq!(q2.mode, AnswerMode::Text);
    let tv = q2.text_validation.as_ref().unwrap();
    assert_eq!(tv.input_type, InputType::Integer);
    assert_eq!((tv.min_length, tv.max_length), (Some(2), Some(10)));
    assert_eq!((tv.minimum, tv.maximum), (Some(1.0), Some(99.0)));
}

#[tokio::test]
async fn legacy_questions_are_parsed() {
    let i = one_card(
        "ask_user_questions",
        json!({
            "version": 1,
            "questions": [
                {"id": "q_single", "prompt": "Pick", "selectionMode": "single", "required": true, "allowOther": false,
                 "options": [{"id": "a", "label": "A"}, {"id": "paperclip_custom_answer", "label": "Other", "freeText": true}]},
                {"id": "q_multi", "prompt": "Many", "selectionMode": "multi", "required": false, "allowOther": true,
                 "options": [{"id": "x", "label": "X"}]},
                {"id": "q_text", "prompt": "Text", "selectionMode": "single", "required": true, "allowOther": false,
                 "options": [{"id": "paperclip_text_answer", "label": "Answer", "freeText": true}]}
            ]
        }),
    )
    .await;
    let InteractionBody::Questions(form) = i.body else {
        panic!()
    };
    let q = &form.questions;
    assert_eq!(q[0].mode, AnswerMode::Single);
    assert_eq!(q[0].options.len(), 1);
    assert_eq!(
        q[0].custom_answer.as_ref().unwrap().option_id.as_deref(),
        Some("paperclip_custom_answer")
    );
    assert_eq!(q[1].mode, AnswerMode::Multi);
    assert!(q[1].custom_answer.as_ref().unwrap().option_id.is_none());
    assert_eq!(q[2].mode, AnswerMode::Text);
    assert!(q[2].options.is_empty());
}

#[tokio::test]
async fn confirmation_checkbox_and_verdicts_are_parsed() {
    let c = one_card(
        "request_confirmation",
        json!({"version": 1, "prompt": "Ok?", "acceptLabel": "Approve", "rejectLabel": "Reject",
               "rejectRequiresReason": true, "allowDeclineReason": true, "detailsMarkdown": "details",
               "supersedeOnUserComment": false}),
    )
    .await;
    assert_eq!(
        c.body,
        InteractionBody::Confirmation(Confirmation {
            prompt: "Ok?".into(),
            details: Some("details".into()),
            accept_label: Some("Approve".into()),
            reject_label: Some("Reject".into()),
            reject_requires_reason: true,
            allow_decline_reason: true,
        })
    );

    let cb = one_card(
        "request_checkbox_confirmation",
        json!({"version": 1, "prompt": "Pick", "options": [{"id": "a", "label": "A"}, {"id": "b", "label": "B"}, {"id": "c", "label": "C"}],
               "defaultSelectedOptionIds": ["a"], "minSelected": 1, "maxSelected": 2}),
    )
    .await;
    let InteractionBody::Checkbox(cb) = cb.body else {
        panic!()
    };
    assert_eq!(cb.default_selected, vec!["a"]);
    assert_eq!((cb.min_selected, cb.max_selected), (Some(1), Some(2)));

    let v = one_card_with_result(
        "request_item_verdicts",
        json!({"version": 1, "prompt": "Review", "items": [{"id": "i1", "label": "One"}, {"id": "i2", "label": "Two"}],
               "verdicts": ["approve", "reject"], "requireReasonOn": ["reject"], "allowBulkApprove": false}),
        json!({"version": 1, "outcome": "resolved", "complete": false,
               "items": [{"id": "i1", "verdict": "approve", "resolvedByUserId": "user-1"}]}),
    )
    .await;
    let InteractionBody::Verdicts(v) = v.body else {
        panic!()
    };
    assert_eq!(v.verdicts, vec![Verdict::Approve, Verdict::Reject]);
    assert_eq!(v.require_reason_on, vec![Verdict::Reject]);
    assert!(!v.allow_bulk_approve);
    assert_eq!(v.decided, vec![("i1".to_string(), Verdict::Approve)]);

    let all = one_card(
        "request_item_verdicts",
        json!({"version": 1, "prompt": "Review", "items": [{"id": "i1", "label": "One"}]}),
    )
    .await;
    let InteractionBody::Verdicts(all) = all.body else {
        panic!()
    };
    assert_eq!(all.verdicts, Verdict::ALL.to_vec());
    assert!(all.allow_bulk_approve);
}

fn tool_action_payload(risk: &str) -> Value {
    json!({
        "version": 1, "prompt": "Approve Create issue?", "acceptLabel": "Approve action",
        "rejectLabel": "Reject action", "rejectRequiresReason": false, "allowDeclineReason": true,
        "detailsMarkdown": "```json\n{\"token\":\"secret\"}\n```",
        "target": {"type": "custom", "key": "tool-action:action-1", "revisionId": "hash-1"},
        "toolAction": {
            "version": 1, "actionRequestId": "action-1", "invocationId": "inv-1",
            "toolName": "github.create_issue", "toolDisplayName": "Create issue",
            "connectionId": "conn-1", "applicationId": "app-1", "appDisplayName": "GitHub",
            "risk": risk, "rememberActionScope": "always", "previewMarkdown": "Creates an issue",
            "argumentsSummaryJson": "{\"title\":\"x\"}", "argumentsHash": "hash-1",
            "expiresAt": "2026-11-15T18:04:00.000Z"
        }
    })
}

#[tokio::test]
async fn governed_cards_are_parsed() {
    let t = one_card("request_confirmation", tool_action_payload("destructive")).await;
    assert!(t.is_governed());
    let InteractionBody::ToolAction(ta) = t.body else {
        panic!()
    };
    assert_eq!(ta.risk, Risk::Destructive);
    assert_eq!(ta.action_request_id, "action-1");
    assert_eq!(ta.invocation_id.as_deref(), Some("inv-1"));
    assert_eq!(ta.tool_display_name, "Create issue");
    assert_eq!(ta.app_display_name.as_deref(), Some("GitHub"));
    assert_eq!(ta.arguments_hash, "hash-1");
    assert_eq!(ta.target_revision_id.as_deref(), Some("hash-1"));
    let dbg = format!("{ta:?}");
    assert!(!dbg.contains("github.create_issue"), "toolName is not kept");
    assert!(!dbg.contains("conn-1"));

    let s = one_card(
        "request_confirmation",
        json!({"version": 1, "prompt": "Bind?", "rejectRequiresReason": true,
               "secretProposal": {"proposalId": "p-1", "sourceSecretLabel": "GITHUB_TOKEN",
                                  "configPath": "env.GITHUB_TOKEN", "targetAgentId": "agent-1",
                                  "targetAgentName": "Developer", "justification": "needs it",
                                  "expiresAt": "2026-11-29T17:04:00.000Z"}}),
    )
    .await;
    let InteractionBody::SecretProposal(sp) = s.body else {
        panic!()
    };
    assert_eq!(sp.source_secret_label, "GITHUB_TOKEN");
    assert_eq!(sp.config_path, "env.GITHUB_TOKEN");
    assert_eq!(sp.target_agent_name.as_deref(), Some("Developer"));
}

#[tokio::test]
async fn unsupported_kinds_are_marked() {
    for (kind, payload, reason) in [
        (
            "suggest_tasks",
            json!({"version": 1}),
            UnsupportedReason::SuggestTasks,
        ),
        (
            "connection_intent",
            json!({"version": 1}),
            UnsupportedReason::ConnectionIntent,
        ),
        (
            "request_confirmation",
            json!({"version": 1, "prompt": "Connect", "connectionAuthorization": {"provider": "x"}}),
            UnsupportedReason::ConnectionAuthorization,
        ),
        ("brand_new_kind", json!({}), UnsupportedReason::UnknownKind),
        (
            "request_checkbox_confirmation",
            json!({"version": 1, "prompt": "no options"}),
            UnsupportedReason::Malformed,
        ),
        (
            "request_confirmation",
            json!({"version": 1, "toolAction": {"risk": "write"}}),
            UnsupportedReason::Malformed,
        ),
    ] {
        let i = one_card(kind, payload).await;
        assert_eq!(i.body, InteractionBody::Unsupported { reason }, "{kind}");
    }
}

#[tokio::test]
async fn resolved_result_is_parsed() {
    let server = MockServer::start().await;
    let mut c = card(
        "request_confirmation",
        "accepted",
        tool_action_payload("write"),
        json!({"version": 1, "outcome": "accepted",
               "toolAction": {"version": 1, "status": "executed", "rememberedAction": false,
                              "resultSummary": "Row 42 added", "resultHref": null, "errorCode": null,
                              "errorMessage": "never shown", "updatedAt": "2026-11-15T12:00:00.000Z"}}),
    );
    c["resolvedByUserId"] = json!("user-1");
    Mock::given(method("GET"))
        .and(path(format!("/api/issues/{ISSUE}/interactions")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([c])))
        .mount(&server)
        .await;
    let i = client(&server).interactions(ISSUE).await.unwrap().remove(0);
    assert_eq!(i.status, InteractionStatus::Accepted);
    assert_eq!(i.resolved_by_user_id.as_deref(), Some("user-1"));
    let r = i.result.unwrap();
    assert_eq!(r.outcome.as_deref(), Some("accepted"));
    let ta = r.tool_action.unwrap();
    assert_eq!(ta.status, "executed");
    assert!(ta.is_final());
    assert_eq!(ta.result_summary.as_deref(), Some("Row 42 added"));
}

async fn expect_post(server: &MockServer, action: &str, body: Value) {
    Mock::given(method("POST"))
        .and(path(format!(
            "/api/issues/{ISSUE}/interactions/{CARD}/{action}"
        )))
        .and(body_json(body))
        .respond_with(ResponseTemplate::new(200).set_body_json(card(
            "request_confirmation",
            "accepted",
            json!({"version": 1, "prompt": "x"}),
            json!({"version": 1, "outcome": "accepted"}),
        )))
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn resolution_request_bodies_match_contract() {
    let server = MockServer::start().await;
    expect_post(
        &server,
        "respond",
        json!({"answers": [
            {"questionId": "license", "optionIds": ["apache2"]},
            {"questionId": "notes", "optionIds": [], "otherText": "free text"}
        ], "summaryMarkdown": null}),
    )
    .await;
    expect_post(&server, "accept", json!({"selectedOptionIds": ["a", "c"]})).await;
    expect_post(&server, "reject", json!({"reason": "no"})).await;
    expect_post(
        &server,
        "verdicts",
        json!({"verdicts": [{"id": "i1", "verdict": "approve"}, {"id": "i2", "verdict": "reject", "reason": "bad"}]}),
    )
    .await;
    let c = client(&server);
    let rec = c
        .resolve(
            ISSUE,
            CARD,
            &Resolution::Respond {
                answers: vec![
                    QuestionAnswer {
                        question_id: "license".into(),
                        option_ids: vec!["apache2".into()],
                        other_text: None,
                    },
                    QuestionAnswer {
                        question_id: "notes".into(),
                        option_ids: vec![],
                        other_text: Some("free text".into()),
                    },
                ],
            },
        )
        .await
        .unwrap();
    assert_eq!(rec.unwrap().status, InteractionStatus::Accepted);
    c.resolve(
        ISSUE,
        CARD,
        &Resolution::Accept {
            selected_option_ids: Some(vec!["a".into(), "c".into()]),
        },
    )
    .await
    .unwrap();
    c.resolve(
        ISSUE,
        CARD,
        &Resolution::Reject {
            reason: Some("no".into()),
        },
    )
    .await
    .unwrap();
    c.resolve(
        ISSUE,
        CARD,
        &Resolution::Verdicts {
            verdicts: vec![
                ItemVerdictAnswer {
                    id: "i1".into(),
                    verdict: Verdict::Approve,
                    reason: None,
                },
                ItemVerdictAnswer {
                    id: "i2".into(),
                    verdict: Verdict::Reject,
                    reason: Some("bad".into()),
                },
            ],
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn governed_accept_sends_empty_body_without_remember_action() {
    let server = MockServer::start().await;
    expect_post(&server, "accept", json!({})).await;
    expect_post(&server, "reject", json!({"reason": "nope"})).await;
    let c = client(&server);
    c.governed_accept(ISSUE, CARD).await.unwrap();
    c.governed_reject(ISSUE, CARD, Some("nope")).await.unwrap();
    for req in server.received_requests().await.unwrap() {
        let body = String::from_utf8_lossy(&req.body).into_owned();
        assert!(!body.contains("rememberAction"), "{body}");
    }
}

async fn error_for(status: u16, body: Value) -> PaperclipError {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(&server)
        .await;
    client(&server)
        .resolve(
            ISSUE,
            CARD,
            &Resolution::Accept {
                selected_option_ids: None,
            },
        )
        .await
        .unwrap_err()
}

#[tokio::test]
async fn conflict_codes_are_classified() {
    for code in [
        "interaction_already_resolved",
        "interaction_superseded",
        "interaction_issue_closed",
        "interaction_stale_target",
    ] {
        let e = error_for(
            409,
            json!({"error": "Interaction has already been resolved", "code": code}),
        )
        .await;
        assert_eq!(e, PaperclipError::Resolved { code: code.into() });
        assert!(!e.is_transient());
    }
    let e = error_for(
        409,
        json!({"error": "This review has already been resolved"}),
    )
    .await;
    assert_eq!(e, PaperclipError::Conflict);
    assert!(e.is_transient());
}

#[tokio::test]
async fn auth_and_audience_errors_are_classified() {
    for code in [
        "interaction_human_only",
        "interaction_addressee_mismatch",
        "interaction_creator_excluded",
        "interaction_governed_action_denied",
    ] {
        let e = error_for(403, json!({"error": "denied", "code": code, "details": {}})).await;
        assert_eq!(e, PaperclipError::AudienceDenied { code: code.into() });
    }
    assert_eq!(
        error_for(403, json!({"error": "Board access required"})).await,
        PaperclipError::Auth { status: 403 }
    );
    assert_eq!(
        error_for(401, json!({"error": "Unauthorized"})).await,
        PaperclipError::Auth { status: 401 }
    );
    assert_eq!(
        error_for(
            404,
            json!({"error": "Interaction not found", "code": "interaction_not_found"})
        )
        .await,
        PaperclipError::NotFound
    );
}

#[tokio::test]
async fn validation_and_server_errors_are_classified() {
    let e = error_for(
        422,
        json!({"error": "A decline reason is required for this confirmation"}),
    )
    .await;
    match &e {
        PaperclipError::Invalid {
            status: 422,
            message,
            ..
        } => assert!(message.contains("decline reason")),
        other => panic!("{other:?}"),
    }
    assert!(
        !e.to_string().contains("decline"),
        "Display must not carry server text"
    );
    let e = error_for(
        422,
        json!({"error": "/response/answers/q_text: must be at most 10", "code": "invalid_question_response"}),
    )
    .await;
    assert!(
        matches!(e, PaperclipError::Invalid { code: Some(ref c), .. } if c == "invalid_question_response")
    );
    assert!(matches!(
        error_for(400, json!({"error": "Validation error", "details": []})).await,
        PaperclipError::Invalid { status: 400, .. }
    ));
    let e = error_for(500, json!({"error": "Internal server error"})).await;
    assert_eq!(e, PaperclipError::Server { status: 500 });
    assert!(e.is_transient());
    assert_eq!(
        error_for(503, json!({})).await,
        PaperclipError::Server { status: 503 }
    );
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
    let e = client(&server)
        .issues("company-1", &["todo"], 10)
        .await
        .unwrap_err();
    assert!(e.is_transient());
    assert_eq!(e.retry_after(), Some(Duration::from_secs(7)));
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
        Duration::from_secs(1),
    )
    .unwrap();
    let e = c.attention("company-1").await.unwrap_err();
    assert!(e.is_transient());
    let text = format!("{e} {e:?}");
    assert!(!text.contains("127.0.0.1"), "{text}");
    assert!(!text.contains("pcp_board"), "{text}");
}

#[tokio::test]
async fn approvals_are_listed_and_approved() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/api/issues/{ISSUE}/approvals")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"id": "ap-1", "type": "request_board_approval", "status": "pending",
             "payload": {"title": "t", "source": "tool_gateway", "invocationId": "inv-1",
                         "actionRequestId": "action-1", "tool": "x", "risk": "destructive", "argumentsHash": "hash-1"}},
            {"id": "ap-2", "type": "hire_agent", "status": "pending", "payload": {}}
        ])))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/approvals/ap-1/approve"))
        .and(body_json(
            json!({"decisionNote": "Approved via Telegram bridge"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"applied": true})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/approvals/ap-1/reject"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"applied": true})))
        .expect(1)
        .mount(&server)
        .await;
    let c = client(&server);
    let list = c.issue_approvals(ISSUE).await.unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].action_request_id.as_deref(), Some("action-1"));
    assert_eq!(list[0].source.as_deref(), Some("tool_gateway"));
    c.approve_approval("ap-1", "Approved via Telegram bridge")
        .await
        .unwrap();
    c.reject_approval("ap-1", "Rejected via Telegram bridge")
        .await
        .unwrap();
}

#[tokio::test]
async fn issues_runs_agents_and_revoke() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/companies/company-1/issues"))
        .and(query_param("status", "todo,backlog"))
        .and(query_param("view", "compact"))
        .and(query_param("limit", "20"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"id": "i-1", "identifier": "PET-20", "title": "Do it", "status": "todo", "priority": "high",
             "assigneeAgentId": "agent-1", "updatedAt": "2026-11-15T10:00:00.000Z"}
        ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/companies/company-1/heartbeat-runs"))
        .and(query_param("limit", "100"))
        .and(query_param("summary", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"id": "run-1", "agentId": "agent-1", "status": "failed", "errorCode": "adapter_failed",
             "error": "do not read", "stderrExcerpt": "do not read", "createdAt": "2026-11-15T10:00:00.000Z",
             "finishedAt": "2026-11-15T10:01:00.000Z", "scheduledRetryAt": null,
             "contextSnapshot": {"issueId": "i-1"}}
        ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/companies/company-1/live-runs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"id": "run-2", "agentId": "agent-1", "issueId": "i-1", "status": "running"}
        ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/companies/company-1/agents"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!([{"id": "agent-1", "name": "Developer", "role": "engineer"}])),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/cli-auth/revoke-current"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"revoked": true})))
        .expect(1)
        .mount(&server)
        .await;
    let c = client(&server);
    let issues = c
        .issues("company-1", &["todo", "backlog"], 20)
        .await
        .unwrap();
    assert_eq!(issues[0].identifier.as_deref(), Some("PET-20"));
    assert_eq!(issues[0].priority_rank(), 1);
    let runs = c.heartbeat_runs("company-1", 100).await.unwrap();
    assert!(runs[0].is_failure());
    assert_eq!(runs[0].issue_id.as_deref(), Some("i-1"));
    assert!(!format!("{:?}", runs[0]).contains("do not read"));
    assert!(c.live_runs("company-1").await.unwrap()[0].is_live());
    assert_eq!(c.agents("company-1").await.unwrap()[0].name, "Developer");
    c.revoke_current_key().await.unwrap();
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
        Duration::from_secs(1),
    )
    .unwrap();
    c.health().await.unwrap();
}
