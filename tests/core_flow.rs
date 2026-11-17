//! Integration (mock Paperclip + fake Telegram): delivery, callbacks,
//! outbox, reconcile, "resolved elsewhere" (architecture §12, stage 5).

mod support;

use std::time::Duration;

use bridge::store::DeliveryStatus;
use serde_json::json;
use support::harness::Harness;
use support::paperclip::*;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn accept_ok(server: &MockServer, id: &str) {
    Mock::given(method("POST"))
        .and(path(format!(
            "/api/issues/{ISSUE}/interactions/{id}/accept"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(resolved(
            confirmation(id),
            "accepted",
            Some("user-1"),
            None,
            json!({"version": 1, "outcome": "accepted"}),
        )))
        .mount(server)
        .await;
}

#[tokio::test]
async fn interaction_becomes_a_message_with_buttons() {
    let server = MockServer::start().await;
    serve(&server, &["card-1"], vec![confirmation("card-1")]).await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    let sent = h.tg.sent();
    assert_eq!(sent.len(), 1);
    let (_, html, kb) = &sent[0];
    assert!(html.contains("Подтверждение от Architect"), "{html}");
    assert!(html.contains("PET-1"));
    assert!(html.contains("Ship it?"));
    assert_eq!(Harness::buttons(kb), vec!["Approve", "Reject"]);
    assert_eq!(h.delivery("card-1").await.status, DeliveryStatus::Sent);

    // The next cycle does not deliver it again.
    h.sync().await;
    assert_eq!(h.tg.sent().len(), 1);
}

#[tokio::test]
async fn callback_sends_accept_once_and_updates_the_card() {
    let server = MockServer::start().await;
    serve(&server, &["card-1"], vec![confirmation("card-1")]).await;
    accept_ok(&server, "card-1").await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    let d = h.delivery("card-1").await;
    let accept_data = format!(
        "v1:ac:{}:{}",
        bridge::render::callback::to_base36(d.id as u64),
        d.generation
    );
    h.press(&accept_data).await;
    // Second press of the same (now stale) button.
    h.press(&accept_data).await;
    assert_eq!(h.delivery("card-1").await.status, DeliveryStatus::Answering);
    h.outbox().await;
    h.outbox().await;
    assert_eq!(posts(&server, "/accept").await, vec![json!({})]);
    let d = h.delivery("card-1").await;
    assert_eq!(d.status, DeliveryStatus::Answered);
    let (html, kb) = h.card("card-1").await;
    assert!(html.contains("✅ Ответ отправлен: Approve"), "{html}");
    assert!(kb.is_none());
    let answers = h.tg.answers();
    assert_eq!(answers[0].as_deref(), Some("Отправляю…"));
    assert_eq!(answers[1].as_deref(), Some("Ответ уже отправляется"));
}

#[tokio::test]
async fn duplicate_update_is_processed_once() {
    let server = MockServer::start().await;
    serve(&server, &["card-1"], vec![confirmation("card-1")]).await;
    accept_ok(&server, "card-1").await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    let u = h.press_button("card-1", "Approve").await;
    // Telegram re-delivers the same update (e.g. after a restart).
    let raw = json!({"update_id": u.update_id, "callback_query": {
        "id": "again", "from": {"id": 111}, "data": "v1:rj:1:0",
        "message": {"message_id": 1, "chat": {"id": 111, "type": "private"}}}});
    h.send_update(raw).await;
    h.outbox().await;
    assert_eq!(posts(&server, "/accept").await.len(), 1);
    assert!(posts(&server, "/reject").await.is_empty());
}

#[tokio::test]
async fn answered_in_ui_first_reports_resolved_elsewhere() {
    let server = MockServer::start().await;
    serve(&server, &["card-1"], vec![confirmation("card-1")]).await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    h.press_button("card-1", "Approve").await;

    // Meanwhile the card was rejected in the UI.
    serve(
        &server,
        &[],
        vec![resolved(
            confirmation("card-1"),
            "rejected",
            Some("user-1"),
            None,
            json!({"version": 1, "outcome": "rejected", "reason": "no"}),
        )],
    )
    .await;
    Mock::given(method("POST"))
        .and(path(format!("/api/issues/{ISSUE}/interactions/card-1/accept")))
        .respond_with(ResponseTemplate::new(409).set_body_json(
            json!({"error": "Interaction has already been resolved", "code": "interaction_already_resolved"}),
        ))
        .mount(&server)
        .await;
    h.outbox().await;
    let d = h.delivery("card-1").await;
    assert_eq!(d.status, DeliveryStatus::Closed);
    let (html, kb) = h.card("card-1").await;
    assert!(
        html.contains("Решено в Paperclip: отклонено — ваш ответ не отправлен"),
        "{html}"
    );
    assert!(kb.is_none());
    // Not retried.
    h.outbox().await;
    assert_eq!(posts(&server, "/accept").await.len(), 1);
}

#[tokio::test]
async fn lost_response_is_recognised_as_our_answer() {
    let server = MockServer::start().await;
    serve(&server, &["card-1"], vec![confirmation("card-1")]).await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    h.press_button("card-1", "Approve").await;
    // Our first request reached the server; the response was lost and the
    // retry gets 409. The card is accepted by a user, not by an agent.
    serve(
        &server,
        &[],
        vec![resolved(
            confirmation("card-1"),
            "accepted",
            Some("user-1"),
            None,
            json!({"version": 1, "outcome": "accepted"}),
        )],
    )
    .await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(409).set_body_json(
                json!({"error": "resolved", "code": "interaction_already_resolved"}),
            ),
        )
        .mount(&server)
        .await;
    h.outbox().await;
    assert_eq!(h.delivery("card-1").await.status, DeliveryStatus::Answered);
    assert!(h.card("card-1").await.0.contains("✅ Ответ отправлен"));
}

#[tokio::test]
async fn agent_resolution_is_not_ours() {
    let server = MockServer::start().await;
    serve(&server, &["card-1"], vec![confirmation("card-1")]).await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    h.press_button("card-1", "Approve").await;
    serve(
        &server,
        &[],
        vec![resolved(
            confirmation("card-1"),
            "accepted",
            None,
            Some("agent-2"),
            json!({"version": 1, "outcome": "accepted"}),
        )],
    )
    .await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(409).set_body_json(
                json!({"error": "resolved", "code": "interaction_already_resolved"}),
            ),
        )
        .mount(&server)
        .await;
    h.outbox().await;
    assert_eq!(h.delivery("card-1").await.status, DeliveryStatus::Closed);
    assert!(h.card("card-1").await.0.contains("ваш ответ не отправлен"));
}

#[tokio::test]
async fn paperclip_down_answer_waits_and_is_delivered_later() {
    let server = MockServer::start().await;
    serve(&server, &["card-1"], vec![confirmation("card-1")]).await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    h.press_button("card-1", "Approve").await;
    // Paperclip unavailable for a while.
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(3)
        .mount(&server)
        .await;
    accept_ok(&server, "card-1").await;
    for _ in 0..3 {
        h.outbox().await;
        assert_eq!(h.delivery("card-1").await.status, DeliveryStatus::Answering);
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    let texts = h.tg.texts();
    assert_eq!(
        texts
            .iter()
            .filter(|t| t.contains("Paperclip недоступен, ответ на PET-1 в очереди"))
            .count(),
        1,
        "{texts:?}"
    );
    h.outbox().await;
    assert_eq!(h.delivery("card-1").await.status, DeliveryStatus::Answered);
    assert_eq!(posts(&server, "/accept").await.len(), 4);
}

#[tokio::test]
async fn stale_generation_answers_outdated() {
    let server = MockServer::start().await;
    serve(&server, &["card-1"], vec![confirmation("card-1")]).await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    let d = h.delivery("card-1").await;
    h.press(&format!(
        "v1:ac:{}:9",
        bridge::render::callback::to_base36(d.id as u64)
    ))
    .await;
    h.press("garbage").await;
    h.press("v1:ac:zzzz:0").await;
    assert_eq!(
        h.tg.answers(),
        vec![
            Some("Устарело".into()),
            Some("Устарело".into()),
            Some("Устарело".into())
        ]
    );
    assert!(posts(&server, "/accept").await.is_empty());
    assert_eq!(h.delivery("card-1").await.status, DeliveryStatus::Sent);
}

#[tokio::test]
async fn card_resolved_in_ui_is_edited() {
    let server = MockServer::start().await;
    serve(&server, &["card-1"], vec![confirmation("card-1")]).await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    serve(
        &server,
        &[],
        vec![resolved(
            confirmation("card-1"),
            "accepted",
            Some("user-1"),
            None,
            json!({"version": 1, "outcome": "accepted"}),
        )],
    )
    .await;
    h.sync().await;
    assert_eq!(h.delivery("card-1").await.status, DeliveryStatus::Closed);
    let (html, kb) = h.card("card-1").await;
    assert!(html.contains("— Решено в Paperclip: принято"), "{html}");
    assert!(kb.is_none());
    // A late press on the old keyboard is harmless.
    h.press("v1:ac:1:0").await;
    assert_eq!(
        h.tg.answers().last().unwrap().as_deref(),
        Some("Уже решено")
    );
    assert!(posts(&server, "/accept").await.is_empty());
}

#[tokio::test]
async fn resolved_elsewhere_edit_can_be_disabled() {
    let server = MockServer::start().await;
    serve(&server, &["card-1"], vec![confirmation("card-1")]).await;
    let h = Harness::start(&server.uri(), &[("NOTIFY_RESOLVED_ELSEWHERE", "false")]).await;
    h.sync().await;
    serve(
        &server,
        &[],
        vec![resolved(
            confirmation("card-1"),
            "accepted",
            Some("user-1"),
            None,
            json!({"version": 1, "outcome": "accepted"}),
        )],
    )
    .await;
    h.sync().await;
    assert_eq!(h.delivery("card-1").await.status, DeliveryStatus::Closed);
    assert!(
        h.tg.calls()
            .iter()
            .all(|c| !matches!(c, support::harness::TgCall::Edit { .. }))
    );
}

#[tokio::test]
async fn reject_with_required_reason_waits_for_text() {
    let server = MockServer::start().await;
    let mut c = confirmation("card-1");
    c["payload"]["rejectRequiresReason"] = json!(true);
    serve(&server, &["card-1"], vec![c]).await;
    Mock::given(method("POST"))
        .and(path(format!(
            "/api/issues/{ISSUE}/interactions/card-1/reject"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    h.text("random text", None).await;
    assert!(
        h.tg.texts()
            .last()
            .unwrap()
            .contains("Сейчас нет вопросов, на которые можно ответить текстом")
    );
    h.press_button("card-1", "Reject").await;
    let prompts = h.tg.prompts();
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].1.contains("причину отказа"));
    let (html, kb) = h.card("card-1").await;
    assert!(html.contains("Укажите причину"));
    assert_eq!(Harness::buttons(&kb), vec!["Отмена"]);
    // The reason arrives as a reply to the prompt.
    h.text("Not now", Some(prompts[0].0)).await;
    h.outbox().await;
    assert_eq!(
        posts(&server, "/reject").await,
        vec![json!({"reason": "Not now"})]
    );
    assert!(
        h.card("card-1")
            .await
            .0
            .contains("✅ Ответ отправлен: Reject")
    );
}

#[tokio::test]
async fn optional_reason_and_cancel() {
    let server = MockServer::start().await;
    let mut c = confirmation("card-1");
    c["payload"]["allowDeclineReason"] = json!(true);
    serve(&server, &["card-1"], vec![c]).await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    h.press_button("card-1", "Reject").await;
    h.text("/cancel", None).await;
    let (_, kb) = h.card("card-1").await;
    assert_eq!(Harness::buttons(&kb), vec!["Approve", "Reject"]);
    h.press_button("card-1", "Reject").await;
    h.press_button("card-1", "Без причины").await;
    h.outbox().await;
    assert_eq!(posts(&server, "/reject").await, vec![json!({})]);
}

#[tokio::test]
async fn checkbox_selection_is_sent() {
    let server = MockServer::start().await;
    let c = card(
        "card-1",
        "request_checkbox_confirmation",
        "pending",
        json!({"version": 1, "prompt": "Pick", "options": [
            {"id": "a", "label": "Alpha"}, {"id": "b", "label": "Beta"}, {"id": "c", "label": "Gamma"}],
            "defaultSelectedOptionIds": ["a"], "minSelected": 1, "maxSelected": 2}),
    );
    serve(&server, &["card-1"], vec![c]).await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    h.press_button("card-1", "☐ 3. Gamma").await;
    // Max 2 reached: a third selection is refused.
    h.press_button("card-1", "☐ 2. Beta").await;
    assert_eq!(
        h.tg.answers().last().unwrap().as_deref(),
        Some("Можно выбрать не больше 2")
    );
    h.press_button("card-1", "☑ 1. Alpha").await;
    h.press_button("card-1", "☑ 3. Gamma").await;
    // Nothing selected: min 1 is enforced before sending.
    h.press_button("card-1", "Подтвердить").await;
    assert!(
        h.tg.answers()
            .last()
            .unwrap()
            .as_deref()
            .unwrap()
            .contains("от 1 до 2")
    );
    h.press_button("card-1", "☐ 2. Beta").await;
    h.press_button("card-1", "Подтвердить (выбрано 1)").await;
    h.outbox().await;
    assert_eq!(
        posts(&server, "/accept").await,
        vec![json!({"selectedOptionIds": ["b"]})]
    );
}

#[tokio::test]
async fn validation_error_returns_the_card_to_the_user() {
    let server = MockServer::start().await;
    serve(&server, &["card-1"], vec![confirmation("card-1")]).await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(422).set_body_json(json!({"error": "Something is <wrong>"})),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    accept_ok(&server, "card-1").await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    h.press_button("card-1", "Approve").await;
    h.outbox().await;
    assert_eq!(h.delivery("card-1").await.status, DeliveryStatus::Sent);
    let last = h.tg.texts().last().unwrap().clone();
    assert!(
        last.contains("Paperclip не принял ответ на PET-1: Something is &lt;wrong&gt;"),
        "{last}"
    );
    // The user can answer again.
    h.press_button("card-1", "Approve").await;
    h.outbox().await;
    assert_eq!(h.delivery("card-1").await.status, DeliveryStatus::Answered);
}

#[tokio::test]
async fn unsupported_kinds_are_notifications_without_buttons() {
    let server = MockServer::start().await;
    let c = card("card-1", "suggest_tasks", "pending", json!({"version": 1}));
    serve(&server, &["card-1"], vec![c]).await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    let sent = h.tg.sent();
    assert_eq!(sent.len(), 1);
    assert!(sent[0].1.contains("Ответьте в Paperclip: PET-1"));
    assert!(sent[0].2.is_none());
    assert_eq!(
        h.delivery("card-1").await.status,
        DeliveryStatus::Unsupported
    );
}

#[tokio::test]
async fn many_new_cards_get_a_summary_first() {
    let server = MockServer::start().await;
    let ids: Vec<String> = (0..12).map(|n| format!("card-{n:02}")).collect();
    let id_refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    serve(
        &server,
        &id_refs,
        ids.iter().map(|id| confirmation(id)).collect(),
    )
    .await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    let texts = h.tg.texts();
    assert_eq!(texts.len(), 13);
    assert!(texts[0].contains("Новых вопросов: 12"));
}

#[tokio::test]
async fn foreign_updates_are_ignored_silently() {
    let server = MockServer::start().await;
    serve(&server, &["card-1"], vec![confirmation("card-1")]).await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    let d = h.delivery("card-1").await;
    let data = format!(
        "v1:ac:{}:{}",
        bridge::render::callback::to_base36(d.id as u64),
        d.generation
    );
    h.send_update(json!({"update_id": 500, "callback_query": {
        "id": "x", "from": {"id": 999}, "data": data,
        "message": {"message_id": 1, "chat": {"id": 111, "type": "private"}}}}))
        .await;
    h.send_update(json!({"update_id": 501, "message": {
        "message_id": 2, "from": {"id": 999}, "chat": {"id": 999, "type": "private"}, "text": "/status"}}))
        .await;
    assert!(h.tg.answers().is_empty());
    assert_eq!(h.tg.sent().len(), 1, "nothing was sent to anyone");
    assert_eq!(h.ctx.store.telegram_offset().await.unwrap(), 502);
    assert_eq!(h.delivery("card-1").await.status, DeliveryStatus::Sent);
}

#[tokio::test]
async fn telegram_down_delivery_is_retried_next_cycle() {
    let server = MockServer::start().await;
    serve(&server, &["card-1"], vec![confirmation("card-1")]).await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.tg.fail_sends
        .store(true, std::sync::atomic::Ordering::SeqCst);
    h.sync().await;
    assert!(h.tg.sent().is_empty());
    assert_eq!(h.delivery("card-1").await.status, DeliveryStatus::Sending);
    h.tg.fail_sends
        .store(false, std::sync::atomic::Ordering::SeqCst);
    h.sync().await;
    assert_eq!(h.tg.sent().len(), 1);
    assert_eq!(h.delivery("card-1").await.status, DeliveryStatus::Sent);
}

#[tokio::test]
async fn non_text_messages_are_refused() {
    let server = MockServer::start().await;
    serve(&server, &[], vec![]).await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.send_update(json!({"update_id": 1, "message": {
        "message_id": 2, "from": {"id": 111}, "chat": {"id": 111, "type": "private"}, "photo": [{"file_id": "x"}]}}))
        .await;
    assert!(h.tg.texts()[0].contains("Файлы и голос не поддерживаются"));
}
