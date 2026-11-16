//! Governed confirmations end to end on API §6.6 fixtures (stage 6a).

mod support;

use std::sync::{Arc, Mutex};

use bridge::store::DeliveryStatus;
use serde_json::{Value, json};
use support::harness::Harness;
use support::paperclip::*;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PREVIEW: &str = "Creates issue \"Fix <login>\" in repo";
const ARGS: &str = "{\"title\":\"Fix login\",\"token\":\"[REDACTED]\"}";

fn in_one_hour() -> String {
    bridge::timeutil::format(bridge::timeutil::now() + time::Duration::hours(1))
}

fn tool_card(id: &str, risk: &str, hash: &str, expires_at: &str) -> Value {
    card(
        id,
        "request_confirmation",
        "pending",
        json!({
            "version": 1, "prompt": "Approve Create issue?", "acceptLabel": "Approve action",
            "rejectLabel": "Reject action", "rejectRequiresReason": false, "allowDeclineReason": true,
            "detailsMarkdown": "DETAILS-JSON-SHOULD-NOT-LEAK",
            "target": {"type": "custom", "key": format!("tool-action:{id}"), "revisionId": hash},
            "toolAction": {
                "version": 1, "actionRequestId": format!("action-{id}"), "invocationId": format!("inv-{id}"),
                "toolName": "github.create_issue_TECHNICAL", "toolDisplayName": "Create issue",
                "connectionId": "conn-secret-id", "applicationId": "app-secret-id", "appDisplayName": "GitHub",
                "risk": risk, "rememberActionScope": "REMEMBER-SCOPE-TEXT",
                "previewMarkdown": PREVIEW, "argumentsSummaryJson": ARGS,
                "argumentsHash": hash, "expiresAt": expires_at
            }
        }),
    )
}

fn accepted(c: &Value, tool_status: &str, extra: Value) -> Value {
    let mut ta = json!({"version": 1, "status": tool_status, "rememberedAction": false,
                        "errorMessage": "EXTERNAL-ERROR-MESSAGE"});
    if let (Some(o), Some(e)) = (ta.as_object_mut(), extra.as_object()) {
        o.extend(e.clone());
    }
    resolved(
        c.clone(),
        "accepted",
        Some("user-1"),
        None,
        json!({"version": 1, "outcome": "accepted", "toolAction": ta}),
    )
}

fn secret_card(id: &str) -> Value {
    card(
        id,
        "request_confirmation",
        "pending",
        json!({"version": 1, "prompt": "Bind secret?", "rejectRequiresReason": true,
               "secretProposal": {"proposalId": "p-1", "sourceSecretLabel": "GITHUB_TOKEN",
                                  "configPath": "env.GITHUB_TOKEN", "targetAgentId": "agent-1",
                                  "targetAgentName": "Developer", "justification": "JUSTIFICATION-TEXT",
                                  "expiresAt": in_one_hour()}}),
    )
}

/// A Paperclip whose card list changes when the card is accepted/rejected.
struct LiveCard {
    card: Arc<Mutex<Value>>,
}

impl LiveCard {
    async fn mount(server: &MockServer, c: Value) -> Self {
        let card = Arc::new(Mutex::new(c));
        let id = card.lock().unwrap()["id"].as_str().unwrap().to_string();
        Mock::given(method("GET"))
            .and(path(format!("/api/companies/{COMPANY}/attention")))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(attention(vec![attention_item(&id)])),
            )
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/api/companies/{COMPANY}/agents")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!([{"id": "agent-1", "name": "Developer"}])),
            )
            .mount(server)
            .await;
        let c2 = card.clone();
        Mock::given(method("GET"))
            .and(path(format!("/api/issues/{ISSUE}/interactions")))
            .respond_with(move |_: &wiremock::Request| {
                ResponseTemplate::new(200).set_body_json(json!([c2.lock().unwrap().clone()]))
            })
            .mount(server)
            .await;
        Self { card }
    }

    fn set(&self, v: Value) {
        *self.card.lock().unwrap() = v;
    }

    fn get(&self) -> Value {
        self.card.lock().unwrap().clone()
    }
}

async fn all_bodies(server: &MockServer) -> String {
    server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .map(|r| String::from_utf8_lossy(&r.body).into_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

fn assert_no_leaks(h: &Harness) {
    for text in
        h.tg.texts()
            .iter()
            .chain(h.tg.calls().iter().filter_map(|c| match c {
                support::harness::TgCall::Edit { html, .. } => Some(html),
                _ => None,
            }))
    {
        for secret in [
            "DETAILS-JSON-SHOULD-NOT-LEAK",
            "github.create_issue_TECHNICAL",
            "conn-secret-id",
            "app-secret-id",
            "REMEMBER-SCOPE-TEXT",
            "EXTERNAL-ERROR-MESSAGE",
        ] {
            assert!(
                !text.contains(secret),
                "{secret} leaked into Telegram: {text}"
            );
        }
    }
}

#[tokio::test]
async fn write_action_accept_executes_and_follows_result() {
    let server = MockServer::start().await;
    let c = tool_card("ta-1", "write", "hash-1", &in_one_hour());
    let live = LiveCard::mount(&server, c.clone()).await;
    let live_card = live.card.clone();
    Mock::given(method("POST"))
        .and(path(format!(
            "/api/issues/{ISSUE}/interactions/ta-1/accept"
        )))
        .respond_with(move |_: &wiremock::Request| {
            let rec = accepted(&live_card.lock().unwrap(), "executing", json!({}));
            *live_card.lock().unwrap() = rec.clone();
            ResponseTemplate::new(200).set_body_json(rec)
        })
        .mount(&server)
        .await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    let (html, kb) = h.card("ta-1").await;
    assert!(
        html.contains("🛠 <b>Агент Developer просит выполнить действие</b> · PET-1"),
        "{html}"
    );
    assert!(html.contains("Сервис: GitHub · Действие: Create issue"));
    assert!(html.contains("Риск: ✏️ запись"));
    assert!(html.contains("Creates issue &quot;Fix &lt;login&gt;&quot; in repo"));
    assert!(html.contains("Действует до"));
    assert!(!html.contains("Аргументы"), "arguments hidden by default");
    assert_eq!(Harness::buttons(&kb), vec!["✅ Разрешить", "❌ Отклонить"]);

    h.press_button("ta-1", "✅ Разрешить").await;
    h.outbox().await;
    assert_eq!(posts(&server, "/accept").await, vec![json!({})]);
    assert_eq!(h.delivery("ta-1").await.status, DeliveryStatus::Following);
    assert!(
        h.card("ta-1")
            .await
            .0
            .contains("⏳ Разрешено, выполняется…")
    );

    // Later the action finishes; sync follows the status.
    live.set(accepted(
        &c,
        "executed",
        json!({"resultSummary": "Issue #42 created"}),
    ));
    h.sync().await;
    assert_eq!(h.delivery("ta-1").await.status, DeliveryStatus::Answered);
    assert!(
        h.card("ta-1")
            .await
            .0
            .contains("✅ Выполнено: Issue #42 created")
    );
    assert!(!all_bodies(&server).await.contains("rememberAction"));
    assert_no_leaks(&h);
}

#[tokio::test]
async fn accept_with_failed_result_and_repeat_is_one_effect() {
    let server = MockServer::start().await;
    let c = tool_card("ta-1", "write", "hash-1", &in_one_hour());
    let live = LiveCard::mount(&server, c.clone()).await;
    // The response of the first accept is lost (500 after the action ran).
    let live_card = live.card.clone();
    let failed = accepted(&c, "failed", json!({"errorCode": "upstream_404"}));
    Mock::given(method("POST"))
        .and(path(format!(
            "/api/issues/{ISSUE}/interactions/ta-1/accept"
        )))
        .respond_with(move |_: &wiremock::Request| {
            *live_card.lock().unwrap() = failed.clone();
            ResponseTemplate::new(500).set_body_json(json!({"error": "Internal server error"}))
        })
        .mount(&server)
        .await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    h.press_button("ta-1", "✅ Разрешить").await;
    h.outbox().await;
    // Reconciled by state: accepted by the user → ours, failed.
    assert_eq!(posts(&server, "/accept").await.len(), 1);
    let d = h.delivery("ta-1").await;
    assert_eq!(d.status, DeliveryStatus::Answered);
    assert!(
        h.card("ta-1")
            .await
            .0
            .contains("❌ Ошибка выполнения: upstream_404")
    );
    assert_eq!(live.get()["status"], "accepted");
    assert_no_leaks(&h);
}

#[tokio::test]
async fn conflict_without_code_and_500_are_not_retried_forever() {
    let server = MockServer::start().await;
    let c = tool_card("ta-1", "write", "hash-1", &in_one_hour());
    let _live = LiveCard::mount(&server, c).await;
    // The card stays pending and every accept fails: unclear outcome.
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(409)
                .set_body_json(json!({"error": "This review has already been resolved"})),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(500).set_body_json(json!({"error": "Internal server error"})),
        )
        .mount(&server)
        .await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    h.press_button("ta-1", "✅ Разрешить").await;
    for _ in 0..6 {
        h.outbox().await;
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    }
    assert_eq!(
        posts(&server, "/accept").await.len(),
        3,
        "at most 3 unclear attempts"
    );
    assert_eq!(h.delivery("ta-1").await.status, DeliveryStatus::Closed);
    assert!(
        h.card("ta-1")
            .await
            .0
            .contains("Не удалось, ответьте в Paperclip: PET-1")
    );
}

#[tokio::test]
async fn changed_arguments_or_expired_send_nothing() {
    let server = MockServer::start().await;
    let c = tool_card("ta-1", "write", "hash-1", &in_one_hour());
    let live = LiveCard::mount(&server, c).await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    // The agent re-created the action with other arguments.
    live.set(tool_card("ta-1", "write", "hash-2", &in_one_hour()));
    h.press_button("ta-1", "✅ Разрешить").await;
    assert_eq!(
        h.tg.answers().last().unwrap().as_deref(),
        Some("Действие изменилось или устарело")
    );
    h.outbox().await;
    assert!(posts(&server, "/accept").await.is_empty());
    assert_eq!(h.delivery("ta-1").await.status, DeliveryStatus::Closed);

    let server = MockServer::start().await;
    let past = bridge::timeutil::format(bridge::timeutil::now() - time::Duration::minutes(1));
    let _live = LiveCard::mount(&server, tool_card("ta-2", "write", "hash-1", &past)).await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    let (html, kb) = h.card("ta-2").await;
    assert!(html.contains("⌛ Истекло, решения не принято"));
    assert!(kb.is_none());
    assert!(posts(&server, "/accept").await.is_empty());
}

#[tokio::test]
async fn hash_changed_between_queue_and_send_is_not_sent() {
    let server = MockServer::start().await;
    let live = LiveCard::mount(
        &server,
        tool_card("ta-1", "write", "hash-1", &in_one_hour()),
    )
    .await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    h.press_button("ta-1", "✅ Разрешить").await;
    live.set(tool_card("ta-1", "write", "hash-2", &in_one_hour()));
    h.outbox().await;
    assert!(posts(&server, "/accept").await.is_empty());
    assert!(
        h.card("ta-1")
            .await
            .0
            .contains("Действие изменилось или устарело")
    );
}

async fn mount_approvals(server: &MockServer, status: &str, action_id: Option<&str>) {
    let mut payload =
        json!({"source": "tool_gateway", "invocationId": "inv-ta-1", "argumentsHash": "hash-1"});
    if let Some(a) = action_id {
        payload["actionRequestId"] = json!(a);
    }
    Mock::given(method("GET"))
        .and(path(format!("/api/issues/{ISSUE}/approvals")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"id": "ap-other", "type": "hire_agent", "status": "pending", "payload": {}},
            {"id": "ap-1", "type": "request_board_approval", "status": status, "payload": payload}
        ])))
        .mount(server)
        .await;
}

#[tokio::test]
async fn destructive_confirm_mode_two_steps_then_approval_then_accept() {
    let server = MockServer::start().await;
    let c = tool_card("ta-1", "destructive", "hash-1", &in_one_hour());
    let live = LiveCard::mount(&server, c.clone()).await;
    mount_approvals(&server, "pending", Some("action-ta-1")).await;
    Mock::given(method("POST"))
        .and(path("/api/approvals/ap-1/approve"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"applied": true})))
        .mount(&server)
        .await;
    let live_card = live.card.clone();
    Mock::given(method("POST"))
        .and(path(format!(
            "/api/issues/{ISSUE}/interactions/ta-1/accept"
        )))
        .respond_with(move |_: &wiremock::Request| {
            let rec = accepted(
                &live_card.lock().unwrap(),
                "executed",
                json!({"resultSummary": "Deleted"}),
            );
            *live_card.lock().unwrap() = rec.clone();
            ResponseTemplate::new(200).set_body_json(rec)
        })
        .mount(&server)
        .await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    assert!(h.card("ta-1").await.0.contains("Риск: 🔴 НЕОБРАТИМО"));

    h.press_button("ta-1", "✅ Разрешить").await;
    assert!(
        posts(&server, "/approve").await.is_empty(),
        "first press only opens step 2"
    );
    let (html, kb) = h.card("ta-1").await;
    assert!(html.contains("🔴 Подтвердите необратимое действие"));
    assert!(html.contains("Create issue · GitHub · PET-1"));
    assert_eq!(Harness::buttons(&kb), vec!["🔴 Да, выполнить", "Отмена"]);
    // Cancel returns the original keyboard.
    h.press_button("ta-1", "Отмена").await;
    assert_eq!(
        Harness::buttons(&h.card("ta-1").await.1),
        vec!["✅ Разрешить", "❌ Отклонить"]
    );

    h.press_button("ta-1", "✅ Разрешить").await;
    h.press_button("ta-1", "🔴 Да, выполнить").await;
    h.outbox().await;
    assert_eq!(
        posts(&server, "/approve").await,
        vec![json!({"decisionNote": "Approved via Telegram bridge"})]
    );
    assert_eq!(posts(&server, "/accept").await, vec![json!({})]);
    // Approval strictly before accept.
    let order: Vec<String> = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.method.as_str() == "POST")
        .map(|r| r.url.path().to_string())
        .collect();
    assert_eq!(
        order,
        vec![
            "/api/approvals/ap-1/approve".to_string(),
            format!("/api/issues/{ISSUE}/interactions/ta-1/accept")
        ]
    );
    assert!(h.card("ta-1").await.0.contains("✅ Выполнено: Deleted"));
    assert_no_leaks(&h);
}

#[tokio::test]
async fn destructive_second_step_expires_after_two_minutes() {
    let server = MockServer::start().await;
    let _live = LiveCard::mount(
        &server,
        tool_card("ta-1", "destructive", "hash-1", &in_one_hour()),
    )
    .await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    h.press_button("ta-1", "✅ Разрешить").await;
    let d = h.delivery("ta-1").await;
    let mut dr = h.ctx.store.draft(d.id).await.unwrap().unwrap();
    dr.updated_at = bridge::timeutil::format(bridge::timeutil::now() - time::Duration::minutes(3));
    h.ctx
        .store
        .apply(vec![bridge::store::StoreOp::PutDraft(dr)])
        .await
        .unwrap();
    // Re-rendered card shows the original keyboard again…
    bridge::core::cards::refresh(&h.ctx, d.id).await.unwrap();
    assert_eq!(
        Harness::buttons(&h.card("ta-1").await.1),
        vec!["✅ Разрешить", "❌ Отклонить"]
    );
    // …and a late "Да, выполнить" does nothing.
    let id36 = bridge::render::callback::to_base36(d.id as u64);
    h.press(&format!("v1:gc:{id36}:{}", d.generation)).await;
    assert_eq!(
        h.tg.answers().last().unwrap().as_deref(),
        Some("Время подтверждения истекло")
    );
    h.outbox().await;
    assert!(posts(&server, "/approve").await.is_empty());
    assert!(posts(&server, "/accept").await.is_empty());
}

#[tokio::test]
async fn destructive_restart_between_steps_repeats_without_duplicate() {
    let server = MockServer::start().await;
    let c = tool_card("ta-1", "destructive", "hash-1", &in_one_hour());
    let live = LiveCard::mount(&server, c.clone()).await;
    // Approve succeeds, accept times out the first time (process "crashes").
    let approval_status = Arc::new(Mutex::new("pending".to_string()));
    let st = approval_status.clone();
    Mock::given(method("GET"))
        .and(path(format!("/api/issues/{ISSUE}/approvals")))
        .respond_with(move |_: &wiremock::Request| {
            ResponseTemplate::new(200).set_body_json(
                json!([{"id": "ap-1", "type": "request_board_approval",
                "status": st.lock().unwrap().clone(),
                "payload": {"source": "tool_gateway", "actionRequestId": "action-ta-1"}}]),
            )
        })
        .mount(&server)
        .await;
    let st = approval_status.clone();
    Mock::given(method("POST"))
        .and(path("/api/approvals/ap-1/approve"))
        .respond_with(move |_: &wiremock::Request| {
            *st.lock().unwrap() = "approved".into();
            ResponseTemplate::new(200).set_body_json(json!({"applied": true}))
        })
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!(
            "/api/issues/{ISSUE}/interactions/ta-1/accept"
        )))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    let live_card = live.card.clone();
    Mock::given(method("POST"))
        .and(path(format!(
            "/api/issues/{ISSUE}/interactions/ta-1/accept"
        )))
        .respond_with(move |_: &wiremock::Request| {
            let rec = accepted(&live_card.lock().unwrap(), "executed", json!({}));
            *live_card.lock().unwrap() = rec.clone();
            ResponseTemplate::new(200).set_body_json(rec)
        })
        .mount(&server)
        .await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    h.press_button("ta-1", "✅ Разрешить").await;
    h.press_button("ta-1", "🔴 Да, выполнить").await;
    h.outbox().await;
    assert_eq!(h.delivery("ta-1").await.status, DeliveryStatus::Answering);

    let h = h.restart().await;
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    h.outbox().await;
    assert_eq!(
        posts(&server, "/approve").await.len(),
        1,
        "approval not repeated once approved"
    );
    assert_eq!(posts(&server, "/accept").await.len(), 2);
    assert_eq!(h.delivery("ta-1").await.status, DeliveryStatus::Answered);
}

#[tokio::test]
async fn destructive_without_approval_asks_to_decide_in_ui() {
    let server = MockServer::start().await;
    let _live = LiveCard::mount(
        &server,
        tool_card("ta-1", "destructive", "hash-1", &in_one_hour()),
    )
    .await;
    Mock::given(method("GET"))
        .and(path(format!("/api/issues/{ISSUE}/approvals")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    h.press_button("ta-1", "✅ Разрешить").await;
    h.press_button("ta-1", "🔴 Да, выполнить").await;
    h.outbox().await;
    assert!(posts(&server, "/accept").await.is_empty());
    assert_eq!(h.delivery("ta-1").await.status, DeliveryStatus::Closed);
    assert!(
        h.card("ta-1")
            .await
            .0
            .contains("Не удалось одобрить из Telegram, решите в Paperclip: PET-1")
    );
}

#[tokio::test]
async fn approval_found_by_invocation_id_fallback() {
    let server = MockServer::start().await;
    let c = tool_card("ta-1", "destructive", "hash-1", &in_one_hour());
    let live = LiveCard::mount(&server, c).await;
    mount_approvals(&server, "pending", None).await;
    Mock::given(method("POST"))
        .and(path("/api/approvals/ap-1/approve"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;
    let live_card = live.card.clone();
    Mock::given(method("POST"))
        .and(path(format!(
            "/api/issues/{ISSUE}/interactions/ta-1/accept"
        )))
        .respond_with(move |_: &wiremock::Request| {
            let rec = accepted(&live_card.lock().unwrap(), "executed", json!({}));
            *live_card.lock().unwrap() = rec.clone();
            ResponseTemplate::new(200).set_body_json(rec)
        })
        .mount(&server)
        .await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    h.press_button("ta-1", "✅ Разрешить").await;
    h.press_button("ta-1", "🔴 Да, выполнить").await;
    h.outbox().await;
    assert_eq!(posts(&server, "/approve").await.len(), 1);
    assert_eq!(h.delivery("ta-1").await.status, DeliveryStatus::Answered);
}

#[tokio::test]
async fn ui_only_mode_has_no_buttons_and_sends_nothing_but_write_works() {
    let server = MockServer::start().await;
    let d_card = tool_card("ta-1", "destructive", "hash-1", &in_one_hour());
    let w_card = tool_card("ta-22", "write", "hash-9", &in_one_hour());
    serve(&server, &["ta-1", "ta-22"], vec![d_card, w_card.clone()]).await;
    Mock::given(method("POST"))
        .and(path(format!(
            "/api/issues/{ISSUE}/interactions/ta-22/accept"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(accepted(
            &w_card,
            "executed",
            json!({}),
        )))
        .mount(&server)
        .await;
    let h = Harness::start(&server.uri(), &[("GOVERNED_DESTRUCTIVE_MODE", "ui_only")]).await;
    h.sync().await;
    let (html, kb) = h.card("ta-1").await;
    assert!(
        html.contains("Необратимое действие: решение только в Paperclip, PET-1"),
        "{html}"
    );
    assert!(kb.is_none());
    // Even a forged callback sends nothing.
    let d = h.delivery("ta-1").await;
    let id36 = bridge::render::callback::to_base36(d.id as u64);
    h.press(&format!("v1:ac:{id36}:{}", d.generation)).await;
    h.press(&format!("v1:gc:{id36}:{}", d.generation)).await;
    h.outbox().await;
    assert!(posts(&server, "/ta-1/accept").await.is_empty());
    assert!(posts(&server, "/approve").await.is_empty());

    h.press_button("ta-22", "✅ Разрешить").await;
    h.outbox().await;
    assert_eq!(posts(&server, "/ta-22/accept").await.len(), 1);
    assert_eq!(h.delivery("ta-22").await.status, DeliveryStatus::Answered);
}

#[tokio::test]
async fn reject_destructive_rejects_linked_approval() {
    let server = MockServer::start().await;
    let c = tool_card("ta-1", "destructive", "hash-1", &in_one_hour());
    let live = LiveCard::mount(&server, c.clone()).await;
    mount_approvals(&server, "pending", Some("action-ta-1")).await;
    let live_card = live.card.clone();
    Mock::given(method("POST"))
        .and(path(format!(
            "/api/issues/{ISSUE}/interactions/ta-1/reject"
        )))
        .respond_with(move |_: &wiremock::Request| {
            let rec = resolved(
                live_card.lock().unwrap().clone(),
                "rejected",
                Some("user-1"),
                None,
                json!({"version": 1, "outcome": "rejected", "reason": "REASON-TEXT"}),
            );
            *live_card.lock().unwrap() = rec.clone();
            ResponseTemplate::new(200).set_body_json(rec)
        })
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/approvals/ap-1/reject"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    h.press_button("ta-1", "❌ Отклонить").await;
    assert_eq!(
        Harness::buttons(&h.card("ta-1").await.1),
        vec!["Без причины", "Отмена"]
    );
    h.text("REASON-TEXT", None).await;
    h.outbox().await;
    assert_eq!(
        posts(&server, "/ta-1/reject").await,
        vec![json!({"reason": "REASON-TEXT"})]
    );
    assert_eq!(
        posts(&server, "/ap-1/reject").await,
        vec![json!({"decisionNote": "Rejected via Telegram bridge"})]
    );
    assert!(
        posts(&server, "/ap-other/reject").await.is_empty(),
        "unrelated approvals untouched"
    );
    assert!(h.card("ta-1").await.0.contains("✅ Отклонено"));
}

#[tokio::test]
async fn write_reject_without_reason() {
    let server = MockServer::start().await;
    let c = tool_card("ta-1", "write", "hash-1", &in_one_hour());
    let live = LiveCard::mount(&server, c).await;
    let live_card = live.card.clone();
    Mock::given(method("POST"))
        .and(path(format!(
            "/api/issues/{ISSUE}/interactions/ta-1/reject"
        )))
        .respond_with(move |_: &wiremock::Request| {
            let rec = resolved(
                live_card.lock().unwrap().clone(),
                "rejected",
                Some("user-1"),
                None,
                json!({"version": 1, "outcome": "rejected"}),
            );
            *live_card.lock().unwrap() = rec.clone();
            ResponseTemplate::new(200).set_body_json(rec)
        })
        .mount(&server)
        .await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    h.press_button("ta-1", "❌ Отклонить").await;
    h.press_button("ta-1", "Без причины").await;
    h.outbox().await;
    assert_eq!(posts(&server, "/reject").await, vec![json!({})]);
    assert!(posts(&server, "/approvals").await.is_empty());
}

#[tokio::test]
async fn secret_proposal_bind_and_failed_binding() {
    let server = MockServer::start().await;
    let c = secret_card("sp-1");
    let live = LiveCard::mount(&server, c.clone()).await;
    let live_card = live.card.clone();
    Mock::given(method("POST"))
        .and(path(format!("/api/issues/{ISSUE}/interactions/sp-1/accept")))
        .respond_with(move |_: &wiremock::Request| {
            let rec = resolved(
                live_card.lock().unwrap().clone(),
                "accepted",
                Some("user-1"),
                None,
                json!({"version": 1, "outcome": "accepted",
                       "secretProposal": {"version": 1, "status": "failed", "errorCode": "http_403"}}),
            );
            *live_card.lock().unwrap() = rec.clone();
            ResponseTemplate::new(200).set_body_json(rec)
        })
        .mount(&server)
        .await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    let (html, kb) = h.card("sp-1").await;
    assert!(
        html.contains("🔑 <b>Агент Developer просит доступ к секрету</b>"),
        "{html}"
    );
    assert!(html.contains("Секрет: GITHUB_TOKEN (существующий секрет компании)"));
    assert!(html.contains("Получит: агент Developer, как env.GITHUB_TOKEN"));
    assert!(html.contains("Обоснование: JUSTIFICATION-TEXT"));
    assert_eq!(Harness::buttons(&kb), vec!["✅ Привязать", "❌ Отклонить"]);
    h.press_button("sp-1", "✅ Привязать").await;
    h.outbox().await;
    assert_eq!(posts(&server, "/accept").await, vec![json!({})]);
    assert!(
        h.card("sp-1")
            .await
            .0
            .contains("❌ Привязка не создана: http_403")
    );
    assert_eq!(
        posts(&server, "/accept").await.len(),
        1,
        "failed binding is not repeated"
    );
}

#[tokio::test]
async fn secret_proposal_reject_requires_reason() {
    let server = MockServer::start().await;
    let live = LiveCard::mount(&server, secret_card("sp-1")).await;
    let live_card = live.card.clone();
    Mock::given(method("POST"))
        .and(path(format!("/api/issues/{ISSUE}/interactions/sp-1/reject")))
        .respond_with(move |_: &wiremock::Request| {
            let rec = resolved(
                live_card.lock().unwrap().clone(),
                "rejected",
                Some("user-1"),
                None,
                json!({"version": 1, "outcome": "rejected", "secretProposal": {"version": 1, "status": "rejected"}}),
            );
            *live_card.lock().unwrap() = rec.clone();
            ResponseTemplate::new(200).set_body_json(rec)
        })
        .mount(&server)
        .await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    h.press_button("sp-1", "❌ Отклонить").await;
    // No "without reason" option, and an empty reason is refused.
    assert_eq!(Harness::buttons(&h.card("sp-1").await.1), vec!["Отмена"]);
    h.text("   ", None).await;
    h.outbox().await;
    assert!(posts(&server, "/reject").await.is_empty());
    h.text("Not this agent", None).await;
    h.outbox().await;
    assert_eq!(
        posts(&server, "/reject").await,
        vec![json!({"reason": "Not this agent"})]
    );
}

#[tokio::test]
async fn governed_disabled_is_notification_only() {
    let server = MockServer::start().await;
    serve(
        &server,
        &["ta-1", "sp-1"],
        vec![
            tool_card("ta-1", "write", "hash-1", &in_one_hour()),
            secret_card("sp-1"),
        ],
    )
    .await;
    let h = Harness::start(&server.uri(), &[("GOVERNED_APPROVALS_ENABLED", "false")]).await;
    h.sync().await;
    for id in ["ta-1", "sp-1"] {
        let (html, kb) = h.card(id).await;
        assert!(html.contains("Решите в Paperclip: PET-1"), "{html}");
        assert!(kb.is_none());
        assert_eq!(h.delivery(id).await.status, DeliveryStatus::Unsupported);
    }
}

#[tokio::test]
async fn arguments_shown_only_with_flag_and_card_fits_one_message() {
    let server = MockServer::start().await;
    let mut c = tool_card("ta-1", "write", "hash-1", &in_one_hour());
    c["payload"]["toolAction"]["previewMarkdown"] = json!("p".repeat(5000));
    c["payload"]["toolAction"]["argumentsSummaryJson"] =
        json!(format!("{{\"a\":\"{}\"}}", "x".repeat(5000)));
    serve(&server, &["ta-1"], vec![c]).await;
    let h = Harness::start(&server.uri(), &[("GOVERNED_SHOW_ARGUMENTS", "true")]).await;
    h.sync().await;
    let sent = h.tg.sent();
    assert_eq!(sent.len(), 1, "always one message");
    let html = &sent[0].1;
    assert!(
        bridge::render::visible_len(html) <= 3500,
        "{}",
        bridge::render::visible_len(html)
    );
    assert!(html.contains("⚠️ Описание сокращено"));
    assert!(html.contains("…полностью — в Paperclip, PET-1"));
    assert!(html.contains("<pre>"));
}

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Global capture for this test binary: a thread-local subscriber misses
/// events once another thread cached the callsite interest.
fn capture() -> &'static Capture {
    static CAPTURE: std::sync::OnceLock<Capture> = std::sync::OnceLock::new();
    CAPTURE.get_or_init(|| {
        let c = Capture::default();
        let writer = c.clone();
        let subscriber = tracing_subscriber::fmt()
            .json()
            .with_max_level(tracing::Level::DEBUG)
            .with_writer(move || writer.clone())
            .finish();
        let _ = tracing::subscriber::set_global_default(subscriber);
        c
    })
}

#[tokio::test]
async fn logs_contain_decisions_but_no_content() {
    let capture = capture();
    let server = MockServer::start().await;
    let c = tool_card("ta-1", "write", "hash-1", &in_one_hour());
    let live = LiveCard::mount(&server, c).await;
    let live_card = live.card.clone();
    Mock::given(method("POST"))
        .respond_with(move |_: &wiremock::Request| {
            let rec = resolved(
                live_card.lock().unwrap().clone(),
                "rejected",
                Some("user-1"),
                None,
                json!({"version": 1, "outcome": "rejected"}),
            );
            *live_card.lock().unwrap() = rec.clone();
            ResponseTemplate::new(200).set_body_json(rec)
        })
        .mount(&server)
        .await;
    let h = Harness::start(&server.uri(), &[("GOVERNED_SHOW_ARGUMENTS", "true")]).await;
    h.sync().await;
    h.press_button("ta-1", "❌ Отклонить").await;
    h.text("REASON-TEXT", None).await;
    h.outbox().await;
    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(logs.contains("decision from telegram queued"), "{logs}");
    assert!(logs.contains("\"interaction_id\":\"ta-1\""));
    assert!(logs.contains("\"risk\":\"write\""));
    assert!(logs.contains("\"update_id\""));
    for secret in [
        "REASON-TEXT",
        "Creates issue",
        "Fix login",
        "[REDACTED]",
        "pcp_board_placeholder",
    ] {
        assert!(!logs.contains(secret), "{secret} leaked into logs");
    }
}
