//! Questions and verdicts end to end (architecture §12, stage 6).

mod support;

use bridge::store::DeliveryStatus;
use serde_json::{Value, json};
use support::harness::Harness;
use support::paperclip::*;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn form(id: &str) -> Value {
    card(
        id,
        "ask_user_questions",
        "pending",
        json!({"version": 1, "questionSet": {
        "schema": "paperclip.question_set.v1", "title": "Setup", "description": "Please answer",
        "questions": [
            {"id": "license", "prompt": "Licence?", "required": true, "answerMode": "single_select",
             "options": [{"id": "apache2", "label": "Apache-2.0"}, {"id": "mit", "label": "MIT"}]},
            {"id": "langs", "prompt": "Languages?", "required": false, "answerMode": "multi_select",
             "options": [{"id": "rs", "label": "Rust"}, {"id": "go", "label": "Go"}, {"id": "py", "label": "Python"}]},
            {"id": "count", "prompt": "How many?", "required": true, "answerMode": "text",
             "textValidation": {"inputType": "integer", "minimum": 1, "maximum": 10}}
        ]}}),
    )
}

async fn respond_ok(server: &MockServer) {
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(server)
        .await;
}

#[tokio::test]
async fn full_form_with_back_validation_and_summary() {
    let server = MockServer::start().await;
    serve(&server, &["form-1"], vec![form("form-1")]).await;
    respond_ok(&server).await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    let (html, kb) = h.card("form-1").await;
    assert!(html.contains("Форма от Architect"), "{html}");
    assert!(html.contains("Вопрос 1/3"));
    assert_eq!(Harness::buttons(&kb), vec!["1. Apache-2.0", "2. MIT"]);

    h.press_button("form-1", "2. MIT").await;
    assert!(h.card("form-1").await.0.contains("Вопрос 2/3"));
    // Back to question 1: the answer is kept and marked.
    h.press_button("form-1", "◀ Назад").await;
    let (html, kb) = h.card("form-1").await;
    assert!(html.contains("Ваш ответ: MIT"), "{html}");
    assert!(Harness::buttons(&kb).contains(&"✓ 2. MIT".to_string()));
    h.press_button("form-1", "1. Apache-2.0").await;

    h.press_button("form-1", "☐ 3. Python").await;
    h.press_button("form-1", "☐ 1. Rust").await;
    h.press_button("form-1", "Далее ▶").await;
    assert!(h.card("form-1").await.0.contains("Вопрос 3/3"));
    // Required text question: no "next" until answered.
    assert!(!Harness::buttons(&h.card("form-1").await.1).contains(&"Готово ▶".to_string()));

    h.press_button("form-1", "✍️ Ответить текстом").await;
    let prompt = h.tg.prompts().last().unwrap().clone();
    h.text("eleven", Some(prompt.0)).await;
    assert!(h.tg.texts().last().unwrap().contains("Нужно целое число"));
    h.text("42", None).await;
    assert!(h.tg.texts().last().unwrap().contains("не больше 10"));
    h.text("7", None).await;

    let (html, kb) = h.card("form-1").await;
    assert!(html.contains("Проверьте ответы"), "{html}");
    assert!(html.contains("1) Licence? → Apache-2.0"));
    assert!(html.contains("2) Languages? → Rust, Python"));
    assert!(html.contains("3) How many? → «7»"));
    let buttons = Harness::buttons(&kb);
    assert_eq!(buttons[0], "Отправить");
    assert!(buttons.contains(&"Изменить 2".to_string()));

    // Edit question 2 from the summary.
    h.press_button("form-1", "Изменить 2").await;
    h.press_button("form-1", "☑ 3. Python").await;
    h.press_button("form-1", "Далее ▶").await;
    h.press_button("form-1", "Готово ▶").await;
    h.press_button("form-1", "Отправить").await;
    h.outbox().await;
    assert_eq!(
        posts(&server, "/respond").await,
        vec![json!({"answers": [
            {"questionId": "license", "optionIds": ["apache2"]},
            {"questionId": "langs", "optionIds": ["rs"]},
            {"questionId": "count", "optionIds": [], "otherText": "7"}
        ], "summaryMarkdown": null})]
    );
    assert_eq!(h.delivery("form-1").await.status, DeliveryStatus::Answered);
    assert!(h.card("form-1").await.0.contains("✅ Ответ отправлен"));
}

#[tokio::test]
async fn restart_in_the_middle_of_a_form_continues() {
    let server = MockServer::start().await;
    serve(&server, &["form-1"], vec![form("form-1")]).await;
    respond_ok(&server).await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    h.press_button("form-1", "1. Apache-2.0").await;
    h.press_button("form-1", "Далее ▶").await;
    h.press_button("form-1", "✍️ Ответить текстом").await;

    let h = h.restart().await;
    bridge::core::forms::announce_restart(&h.ctx).await.unwrap();
    let notice = h.tg.texts().last().unwrap().clone();
    assert!(
        notice.contains("Bridge перезапущен, продолжаем PET-1: вопрос 3/3, жду текстовый ответ"),
        "{notice}"
    );
    // The first message after the restart lands in the waiting draft.
    h.text("3", None).await;
    h.press_button("form-1", "Отправить").await;
    h.outbox().await;
    let body = &posts(&server, "/respond").await[0];
    assert_eq!(
        body["answers"][0],
        json!({"questionId": "license", "optionIds": ["apache2"]})
    );
    assert_eq!(
        body["answers"][1],
        json!({"questionId": "count", "optionIds": [], "otherText": "3"})
    );
}

#[tokio::test]
async fn cancel_resets_the_draft_and_partial_form_is_not_sent() {
    let server = MockServer::start().await;
    serve(&server, &["form-1"], vec![form("form-1")]).await;
    respond_ok(&server).await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    h.press_button("form-1", "1. Apache-2.0").await;
    h.press_button("form-1", "Далее ▶").await;
    // Skip the text question via "Изменить" path: summary is not reachable,
    // so jump with a crafted Submit — refused.
    let d = h.delivery("form-1").await;
    let id36 = bridge::render::callback::to_base36(d.id as u64);
    h.press(&format!("v1:sb:{id36}:{}", d.generation)).await;
    assert!(posts(&server, "/respond").await.is_empty());
    h.press_button("form-1", "◀ Назад").await;
    h.press_button("form-1", "◀ Назад").await;
    let d = h.delivery("form-1").await;
    h.press(&format!("v1:cn:{id36}:{}", d.generation)).await;
    assert_eq!(
        h.tg.answers().last().unwrap().as_deref(),
        Some("Черновик сброшен")
    );
    assert!(h.ctx.store.draft(d.id).await.unwrap().is_none());
    let (html, kb) = h.card("form-1").await;
    assert!(html.contains("Вопрос 1/3"));
    assert_eq!(Harness::buttons(&kb), vec!["1. Apache-2.0", "2. MIT"]);
}

#[tokio::test]
async fn too_many_questions_is_only_a_notice() {
    let server = MockServer::start().await;
    let questions: Vec<Value> = (0..21)
        .map(|n| json!({"id": format!("q{n}"), "prompt": "?", "required": false, "answerMode": "text"}))
        .collect();
    let c = card(
        "form-1",
        "ask_user_questions",
        "pending",
        json!({"version": 1, "questionSet": {"questions": questions}}),
    );
    serve(&server, &["form-1"], vec![c]).await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    let (_, html, kb) = h.tg.sent()[0].clone();
    assert!(html.contains("Форма слишком большая для Telegram, ответьте в Paperclip: PET-1"));
    assert!(kb.is_none());
    assert_eq!(
        h.delivery("form-1").await.status,
        DeliveryStatus::Unsupported
    );
}

#[tokio::test]
async fn single_question_single_select_submits_on_press() {
    let server = MockServer::start().await;
    let c = card(
        "q-1",
        "ask_user_questions",
        "pending",
        json!({"version": 1, "questionSet": {"questions": [
            {"id": "pick", "prompt": "Pick one", "required": true, "answerMode": "single_select",
             "options": [{"id": "a", "label": "Option A", "description": "first"}, {"id": "b", "label": "Option B"}],
             "customAnswer": {"enabled": true}}]}}),
    );
    serve(&server, &["q-1"], vec![c]).await;
    respond_ok(&server).await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    let (html, kb) = h.card("q-1").await;
    assert!(html.contains("Вопрос от Architect"));
    assert!(html.contains("1. Option A — first"));
    assert_eq!(
        Harness::buttons(&kb),
        vec!["1. Option A", "2. Option B", "✍️ Свой ответ"]
    );
    h.press_button("q-1", "2. Option B").await;
    h.outbox().await;
    assert_eq!(
        posts(&server, "/respond").await,
        vec![
            json!({"answers": [{"questionId": "pick", "optionIds": ["b"]}], "summaryMarkdown": null})
        ]
    );
    assert!(
        h.card("q-1")
            .await
            .0
            .contains("✅ Ответ отправлен: Option B")
    );
}

#[tokio::test]
async fn own_answer_and_text_reply() {
    let server = MockServer::start().await;
    let c = card(
        "q-1",
        "ask_user_questions",
        "pending",
        json!({"version": 1, "questionSet": {"questions": [
            {"id": "pick", "prompt": "Pick one", "required": true, "answerMode": "single_select",
             "options": [{"id": "a", "label": "A"}], "customAnswer": {"enabled": true, "label": "Другое"}}]}}),
    );
    let t = card(
        "q-2",
        "ask_user_questions",
        "pending",
        json!({"version": 1, "questionSet": {"questions": [
            {"id": "notes", "prompt": "Notes?", "required": true, "answerMode": "text"}]}}),
    );
    serve(&server, &["q-1", "q-2"], vec![c, t]).await;
    respond_ok(&server).await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;

    h.press_button("q-1", "✍️ Другое").await;
    h.text("my own", None).await;
    let (html, kb) = h.card("q-1").await;
    assert!(html.contains("Отправить этот ответ? «my own»"), "{html}");
    assert_eq!(
        Harness::buttons(&kb),
        vec!["Отправить", "Изменить", "Отмена"]
    );
    h.press_button("q-1", "Отправить").await;

    // Text answer as a reply to the card message itself.
    let card_msg = h.delivery("q-2").await.last_message_id().unwrap();
    h.text("free text answer", Some(card_msg)).await;
    h.press_button("q-2", "Отправить").await;
    h.outbox().await;
    let bodies = posts(&server, "/respond").await;
    assert!(bodies.contains(&json!({"answers": [{"questionId": "pick", "optionIds": [], "otherText": "my own"}], "summaryMarkdown": null})));
    assert!(bodies.contains(&json!({"answers": [{"questionId": "notes", "optionIds": [], "otherText": "free text answer"}], "summaryMarkdown": null})));
}

#[tokio::test]
async fn legacy_free_text_option_is_sent_with_its_id() {
    let server = MockServer::start().await;
    let c = card(
        "q-1",
        "ask_user_questions",
        "pending",
        json!({"version": 1, "questions": [
            {"id": "q_single", "prompt": "Pick", "selectionMode": "single", "required": true, "allowOther": false,
             "options": [{"id": "a", "label": "A"}, {"id": "paperclip_custom_answer", "label": "Other", "freeText": true}]}]}),
    );
    serve(&server, &["q-1"], vec![c]).await;
    respond_ok(&server).await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    h.press_button("q-1", "✍️ Other").await;
    h.text("custom", None).await;
    h.press_button("q-1", "Отправить").await;
    h.outbox().await;
    assert_eq!(
        posts(&server, "/respond").await[0]["answers"][0],
        json!({"questionId": "q_single", "optionIds": ["paperclip_custom_answer"], "otherText": "custom"})
    );
}

#[tokio::test]
async fn server_validation_error_keeps_the_form() {
    let server = MockServer::start().await;
    serve(&server, &["form-1"], vec![form("form-1")]).await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(422).set_body_json(
            json!({"error": "/response/answers/count: must be a valid integer", "code": "invalid_question_response"}),
        ))
        .mount(&server)
        .await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    h.press_button("form-1", "2. MIT").await;
    h.press_button("form-1", "Далее ▶").await;
    h.press_button("form-1", "✍️ Ответить текстом").await;
    h.text("5", None).await;
    h.press_button("form-1", "Отправить").await;
    h.outbox().await;
    assert_eq!(h.delivery("form-1").await.status, DeliveryStatus::Sent);
    assert!(
        h.tg.texts()
            .last()
            .unwrap()
            .contains("must be a valid integer")
    );
    // The draft survived: the summary is shown again with the answers.
    let (html, kb) = h.card("form-1").await;
    assert!(html.contains("1) Licence? → MIT"), "{html}");
    assert_eq!(Harness::buttons(&kb)[0], "Отправить");
}

fn verdicts_card(decided: Option<Value>) -> Value {
    let mut c = card(
        "v-1",
        "request_item_verdicts",
        "pending",
        json!({"version": 1, "prompt": "Review items", "items": [
            {"id": "i1", "label": "First"}, {"id": "i2", "label": "Second"}, {"id": "i3", "label": "Third"}],
            "verdicts": ["approve", "reject"], "requireReasonOn": ["reject"]}),
    );
    if let Some(r) = decided {
        c["result"] = r;
    }
    c
}

#[tokio::test]
async fn verdicts_partial_then_complete() {
    let server = MockServer::start().await;
    serve(&server, &["v-1"], vec![verdicts_card(None)]).await;
    let partial = verdicts_card(Some(
        json!({"version": 1, "outcome": "resolved", "complete": false,
        "items": [{"id": "i1", "verdict": "approve"}]}),
    ));
    Mock::given(method("POST"))
        .and(path(format!(
            "/api/issues/{ISSUE}/interactions/v-1/verdicts"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(partial))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    let mut done = verdicts_card(None);
    done["status"] = json!("answered");
    Mock::given(method("POST"))
        .and(path(format!(
            "/api/issues/{ISSUE}/interactions/v-1/verdicts"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(done))
        .mount(&server)
        .await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    let (html, kb) = h.card("v-1").await;
    assert!(html.contains("Проверка от Architect"));
    assert!(html.contains("1. First: —"));
    assert_eq!(
        Harness::buttons(&kb),
        vec![
            "✅ 1",
            "❌ 1",
            "✅ 2",
            "❌ 2",
            "✅ 3",
            "❌ 3",
            "Одобрить все"
        ]
    );

    h.press_button("v-1", "✅ 1").await;
    h.press_button("v-1", "Отправить (1)").await;
    h.outbox().await;
    assert_eq!(
        posts(&server, "/verdicts").await,
        vec![json!({"verdicts": [{"id": "i1", "verdict": "approve"}]})]
    );
    assert_eq!(h.delivery("v-1").await.status, DeliveryStatus::Sent);
    let (html, kb) = h.card("v-1").await;
    assert!(
        html.contains("1. First: ✅ одобрено (отправлено)"),
        "{html}"
    );
    assert!(!Harness::buttons(&kb).contains(&"✅ 1".to_string()));

    // Reject needs a reason.
    h.press_button("v-1", "❌ 2").await;
    assert!(
        h.tg.prompts()
            .last()
            .unwrap()
            .1
            .contains("Причина для пункта 2")
    );
    h.text("broken", None).await;
    h.press_button("v-1", "✅ 3").await;
    h.press_button("v-1", "Отправить (2)").await;
    h.outbox().await;
    assert_eq!(
        posts(&server, "/verdicts").await[1],
        json!({"verdicts": [{"id": "i2", "verdict": "reject", "reason": "broken"}, {"id": "i3", "verdict": "approve"}]})
    );
    assert_eq!(h.delivery("v-1").await.status, DeliveryStatus::Answered);
    assert!(h.card("v-1").await.0.contains("✅ Вердикты отправлены"));
}

#[tokio::test]
async fn verdicts_bulk_approve() {
    let server = MockServer::start().await;
    let mut c = verdicts_card(None);
    c["payload"]["requireReasonOn"] = json!([]);
    serve(&server, &["v-1"], vec![c]).await;
    let mut done = verdicts_card(None);
    done["status"] = json!("answered");
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(done))
        .mount(&server)
        .await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    h.press_button("v-1", "Одобрить все").await;
    h.press_button("v-1", "Отправить (3)").await;
    h.outbox().await;
    assert_eq!(
        posts(&server, "/verdicts").await,
        vec![json!({"verdicts": [
            {"id": "i1", "verdict": "approve"}, {"id": "i2", "verdict": "approve"}, {"id": "i3", "verdict": "approve"}]})]
    );
}
