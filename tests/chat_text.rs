//! One-question text cards answered by a plain chat message (ADR-0009 §1,
//! decisions Р1–Р3). Row numbers refer to the ADR table.

mod support;

use bridge::render::{Callback, Op};
use bridge::store::DeliveryStatus;
use serde_json::{Value, json};
use support::harness::Harness;
use support::paperclip::*;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const QUEUED: &str = "📨 Ответ на PET-1 принят и поставлен в очередь отправки.";
const NO_QUESTION: &str =
    "Сейчас нет вопросов, на которые можно ответить текстом. Сообщение не отправлено.";

fn text_card(id: &str, validation: Option<Value>) -> Value {
    let mut q = json!({"id": "notes", "prompt": "Notes?", "required": true, "answerMode": "text"});
    if let Some(v) = validation {
        q["textValidation"] = v;
    }
    card(
        id,
        "ask_user_questions",
        "pending",
        json!({"version": 1, "questionSet": {"questions": [q]}}),
    )
}

fn option_card(id: &str) -> Value {
    card(
        id,
        "ask_user_questions",
        "pending",
        json!({"version": 1, "questionSet": {"questions": [
            {"id": "pick", "prompt": "Pick one", "required": true, "answerMode": "single_select",
             "options": [{"id": "a", "label": "A"}], "customAnswer": {"enabled": true, "label": "Другое"}}]}}),
    )
}

async fn respond_ok(server: &MockServer) {
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(server)
        .await;
}

fn answer(text: &str) -> Value {
    json!({"answers": [{"questionId": "notes", "optionIds": [], "otherText": text}],
           "summaryMarkdown": null})
}

async fn post_count(server: &MockServer) -> usize {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.method.as_str() == "POST")
        .count()
}

fn last_text(h: &Harness) -> String {
    h.tg.texts().last().cloned().unwrap_or_default()
}

#[tokio::test]
async fn card_has_no_text_button_only_a_hint() {
    let server = MockServer::start().await;
    let c = text_card("q-1", Some(json!({"inputType": "integer", "minimum": 1})));
    serve(&server, &["q-1"], vec![c]).await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    let (html, kb) = h.card("q-1").await;
    assert!(html.contains("Notes?"), "{html}");
    assert!(
        html.contains("Ответьте обычным сообщением в этот чат (или reply на эту карточку)"),
        "{html}"
    );
    assert!(html.contains("Формат: целое число, не меньше 1"), "{html}");
    assert!(kb.is_none(), "{kb:?}");
}

#[tokio::test]
async fn reply_to_card_is_queued_at_once() {
    // Rows 6 and 18 (no repeated status message after the send).
    let server = MockServer::start().await;
    serve(&server, &["q-1"], vec![text_card("q-1", None)]).await;
    respond_ok(&server).await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    let card_msg = h.delivery("q-1").await.last_message_id().unwrap();
    h.text("  my answer  ", Some(card_msg)).await;

    let d = h.delivery("q-1").await;
    assert_eq!(d.status, DeliveryStatus::Answering);
    assert!(h.ctx.store.draft(d.id).await.unwrap().is_none());
    assert!(h.ctx.store.outbox_for(d.id).await.unwrap().is_some());
    assert_eq!(last_text(&h), QUEUED);
    assert!(h.tg.prompts().is_empty(), "no force-reply prompt");
    assert!(
        h.card("q-1").await.1.is_none(),
        "no buttons while answering"
    );
    assert_eq!(post_count(&server).await, 0, "only through the outbox");

    h.outbox().await;
    assert_eq!(posts(&server, "/respond").await, vec![answer("my answer")]);
    assert_eq!(h.delivery("q-1").await.status, DeliveryStatus::Answered);
    assert!(h.card("q-1").await.0.contains("✅ Ответ отправлен"));
    assert_eq!(h.tg.texts().iter().filter(|t| *t == QUEUED).count(), 1);
}

#[tokio::test]
async fn message_without_reply_answers_the_only_card() {
    // Row 12.
    let server = MockServer::start().await;
    serve(
        &server,
        &["q-1", "c-1"],
        vec![text_card("q-1", None), confirmation("c-1")],
    )
    .await;
    respond_ok(&server).await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    h.text("plain answer", None).await;
    assert_eq!(last_text(&h), QUEUED);
    h.outbox().await;
    assert_eq!(
        posts(&server, "/respond").await,
        vec![answer("plain answer")]
    );
    assert!(posts(&server, "/accept").await.is_empty());
    assert_eq!(h.delivery("c-1").await.status, DeliveryStatus::Sent);
}

#[tokio::test]
async fn several_cards_ask_for_a_reply_and_send_nothing() {
    // Row 13.
    let server = MockServer::start().await;
    serve(
        &server,
        &["q-1", "q-2"],
        vec![text_card("q-1", None), text_card("q-2", None)],
    )
    .await;
    respond_ok(&server).await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    h.text("which one?", None).await;
    assert_eq!(
        last_text(&h),
        "Открыто несколько вопросов: PET-1. Ответьте reply на нужную карточку."
    );
    for id in ["q-1", "q-2"] {
        let d = h.delivery(id).await;
        assert_eq!(d.status, DeliveryStatus::Sent);
        assert!(h.ctx.store.draft(d.id).await.unwrap().is_none());
        assert!(h.ctx.store.outbox_for(d.id).await.unwrap().is_none());
    }
    h.outbox().await;
    assert_eq!(post_count(&server).await, 0);

    // A reply picks the card.
    let card_msg = h.delivery("q-2").await.last_message_id().unwrap();
    h.text("second", Some(card_msg)).await;
    h.outbox().await;
    assert_eq!(posts(&server, "/respond").await, vec![answer("second")]);
    assert!(posts(&server, "/q-2/respond").await.len() == 1);
    assert_eq!(h.delivery("q-1").await.status, DeliveryStatus::Sent);
}

#[tokio::test]
async fn several_cards_in_different_issues_are_listed() {
    let server = MockServer::start().await;
    serve(&server, &["q-1"], vec![text_card("q-1", None)]).await;
    // A second issue with its own text card.
    let mut other = text_card("q-9", None);
    other["issueId"] = json!("issue-2");
    let mut feed = attention(vec![attention_item("q-1"), attention_item("q-9")]);
    feed["items"][1]["subject"]["metadata"]["issueId"] = json!("issue-2");
    feed["items"][1]["relatedIssue"] =
        json!({"id": "issue-2", "identifier": "PET-2", "title": "Other"});
    server.reset().await;
    Mock::given(method("GET"))
        .and(path(format!("/api/companies/{COMPANY}/attention")))
        .respond_with(ResponseTemplate::new(200).set_body_json(feed))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/issues/issue-2/interactions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([other])))
        .mount(&server)
        .await;
    mount_base(&server, &[], vec![text_card("q-1", None)]).await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    assert_eq!(h.delivery("q-9").await.issue_ref, "PET-2");
    h.text("which one?", None).await;
    // Cards are listed in delivery order.
    let text = last_text(&h);
    assert!(
        text == "Открыто несколько вопросов: PET-1, PET-2. Ответьте reply на нужную карточку."
            || text
                == "Открыто несколько вопросов: PET-2, PET-1. Ответьте reply на нужную карточку.",
        "{text}"
    );
    assert_eq!(post_count(&server).await, 0);
}

#[tokio::test]
async fn no_suitable_card_sends_nothing_to_paperclip() {
    // Row 14: no task, comment or answer is created.
    let server = MockServer::start().await;
    serve(
        &server,
        &["c-1", "q-1"],
        vec![confirmation("c-1"), option_card("q-1")],
    )
    .await;
    respond_ok(&server).await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    let before = server.received_requests().await.unwrap().len();
    h.text("hello there", None).await;
    assert_eq!(last_text(&h), NO_QUESTION);
    h.outbox().await;
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        before,
        "not a single request to Paperclip"
    );
    for id in ["c-1", "q-1"] {
        let d = h.delivery(id).await;
        assert_eq!(d.status, DeliveryStatus::Sent);
        assert!(h.ctx.store.draft(d.id).await.unwrap().is_none());
    }
}

#[tokio::test]
async fn invalid_text_keeps_the_question_open() {
    // Rows 7 and 12 with `textValidation` (Р3).
    let server = MockServer::start().await;
    let c = text_card(
        "q-1",
        Some(json!({"inputType": "integer", "minimum": 1, "maximum": 10})),
    );
    serve(&server, &["q-1"], vec![c]).await;
    respond_ok(&server).await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    let card_msg = h.delivery("q-1").await.last_message_id().unwrap();

    h.text("eleven", Some(card_msg)).await;
    assert_eq!(
        last_text(&h),
        "Нужно целое число. Вопрос остаётся открытым — отправьте ответ ещё раз."
    );
    h.text("42", None).await;
    assert_eq!(
        last_text(&h),
        "Число должно быть не больше 10. Вопрос остаётся открытым — отправьте ответ ещё раз."
    );
    h.text("   ", None).await;
    assert_eq!(
        last_text(&h),
        "Ответ не может быть пустым. Вопрос остаётся открытым — отправьте ответ ещё раз."
    );
    let d = h.delivery("q-1").await;
    assert_eq!(d.status, DeliveryStatus::Sent);
    assert!(h.ctx.store.draft(d.id).await.unwrap().is_none());
    assert!(h.ctx.store.outbox_for(d.id).await.unwrap().is_none());
    assert_eq!(post_count(&server).await, 0);

    h.text("7", None).await;
    h.outbox().await;
    assert_eq!(posts(&server, "/respond").await, vec![answer("7")]);
}

#[tokio::test]
async fn waiting_scenario_takes_the_text_first() {
    // Rows 5, 11 (Р2) and the reply-beats-waiting rule.
    let server = MockServer::start().await;
    let mut c = confirmation("c-1");
    c["payload"]["rejectRequiresReason"] = json!(true);
    serve(&server, &["c-1", "q-1"], vec![c, text_card("q-1", None)]).await;
    respond_ok(&server).await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    h.press_button("c-1", "Reject").await;

    // A reply to the text card wins over the waiting reason…
    let card_msg = h.delivery("q-1").await.last_message_id().unwrap();
    h.text("card answer", Some(card_msg)).await;
    assert_eq!(last_text(&h), QUEUED);
    // …and does not reset the waiting.
    let c1 = h.delivery("c-1").await;
    assert!(
        h.ctx
            .store
            .draft(c1.id)
            .await
            .unwrap()
            .and_then(|d| d.awaiting)
            .is_some()
    );

    // Without a reply the text is the reject reason.
    h.text("not now", None).await;
    h.outbox().await;
    assert_eq!(
        posts(&server, "/reject").await,
        vec![json!({"reason": "not now"})]
    );
    assert_eq!(
        posts(&server, "/respond").await,
        vec![answer("card answer")]
    );
}

#[tokio::test]
async fn waiting_without_reply_does_not_answer_the_text_card() {
    // Row 11: the only text card is not touched while a form step waits.
    let server = MockServer::start().await;
    serve(
        &server,
        &["q-1", "q-2"],
        vec![option_card("q-1"), text_card("q-2", None)],
    )
    .await;
    respond_ok(&server).await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    h.press_button("q-1", "✍️ Другое").await;
    h.text("own", None).await;
    assert!(
        h.card("q-1")
            .await
            .0
            .contains("Отправить этот ответ? «own»")
    );
    let d = h.delivery("q-2").await;
    assert_eq!(d.status, DeliveryStatus::Sent);
    assert!(h.ctx.store.outbox_for(d.id).await.unwrap().is_none());
}

#[tokio::test]
async fn reply_to_an_unsuitable_card_works_as_before() {
    // Row 8: the text does not go to the only text card.
    let server = MockServer::start().await;
    serve(
        &server,
        &["q-1", "q-2", "c-1"],
        vec![
            option_card("q-1"),
            text_card("q-2", None),
            confirmation("c-1"),
        ],
    )
    .await;
    respond_ok(&server).await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;

    let msg = h.delivery("c-1").await.last_message_id().unwrap();
    h.text("for the confirmation", Some(msg)).await;
    assert!(last_text(&h).contains("Не жду текста"), "{}", last_text(&h));

    let msg = h.delivery("q-1").await.last_message_id().unwrap();
    h.text("own via reply", Some(msg)).await;
    let (html, kb) = h.card("q-1").await;
    assert!(
        html.contains("Отправить этот ответ? «own via reply»"),
        "{html}"
    );
    assert_eq!(
        Harness::buttons(&kb),
        vec!["Отправить", "Изменить", "Отмена"]
    );
    let d = h.delivery("q-2").await;
    assert_eq!(d.status, DeliveryStatus::Sent);
    assert!(h.ctx.store.outbox_for(d.id).await.unwrap().is_none());
    h.outbox().await;
    assert_eq!(post_count(&server).await, 0);
}

#[tokio::test]
async fn repeated_update_is_ignored() {
    // Row 3.
    let server = MockServer::start().await;
    serve(&server, &["q-1"], vec![text_card("q-1", None)]).await;
    respond_ok(&server).await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    let u = h.text_update("once", None);
    h.send_update(u.clone()).await;
    h.send_update(u).await;
    h.outbox().await;
    assert_eq!(posts(&server, "/respond").await, vec![answer("once")]);
    assert_eq!(h.tg.texts().iter().filter(|t| *t == QUEUED).count(), 1);
}

#[tokio::test]
async fn second_text_after_the_answer_is_not_sent() {
    // Row 15.
    let server = MockServer::start().await;
    serve(&server, &["q-1"], vec![text_card("q-1", None)]).await;
    respond_ok(&server).await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    let card_msg = h.delivery("q-1").await.last_message_id().unwrap();
    h.text("first", Some(card_msg)).await;
    h.text("second", Some(card_msg)).await;
    assert_eq!(
        last_text(&h),
        "Карточка PET-1 уже закрыта или ответ отправлен."
    );
    h.text("third", None).await;
    assert_eq!(last_text(&h), NO_QUESTION);
    h.outbox().await;
    h.text("fourth", Some(card_msg)).await;
    assert_eq!(
        last_text(&h),
        "Карточка PET-1 уже закрыта или ответ отправлен."
    );
    h.outbox().await;
    assert_eq!(posts(&server, "/respond").await, vec![answer("first")]);
}

#[tokio::test]
async fn old_text_button_is_stale() {
    // Row 16.
    let server = MockServer::start().await;
    serve(&server, &["q-1"], vec![text_card("q-1", None)]).await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    let d = h.delivery("q-1").await;
    for op in [Op::Text, Op::Submit, Op::Next, Op::Edit] {
        let data = Callback::new(op, d.id, d.generation).with_a(0).encode();
        h.press(&data).await;
        assert_eq!(h.tg.answers().last().unwrap().as_deref(), Some("Устарело"));
    }
    let d = h.delivery("q-1").await;
    assert!(h.ctx.store.draft(d.id).await.unwrap().is_none());
    assert!(h.tg.prompts().is_empty());
    assert!(h.card("q-1").await.1.is_none());
}

#[tokio::test]
async fn server_rejection_reopens_the_question() {
    // Row 17: 422 returns the card, a new message answers again.
    let server = MockServer::start().await;
    serve(&server, &["q-1"], vec![text_card("q-1", None)]).await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(422).set_body_json(
            json!({"error": "/response/answers/notes: does not match pattern", "code": "invalid_question_response"}),
        ))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    respond_ok(&server).await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    h.text("bad", None).await;
    h.outbox().await;
    assert_eq!(h.delivery("q-1").await.status, DeliveryStatus::Sent);
    assert!(last_text(&h).contains("does not match pattern"));
    assert!(h.card("q-1").await.1.is_none());
    h.text("good", None).await;
    h.outbox().await;
    assert_eq!(
        posts(&server, "/respond").await,
        vec![answer("bad"), answer("good")]
    );
    assert_eq!(h.delivery("q-1").await.status, DeliveryStatus::Answered);
}

#[tokio::test]
async fn multi_question_form_still_uses_the_text_button() {
    let server = MockServer::start().await;
    let c = card(
        "f-1",
        "ask_user_questions",
        "pending",
        json!({"version": 1, "questionSet": {"questions": [
            {"id": "a", "prompt": "First?", "required": true, "answerMode": "text"},
            {"id": "b", "prompt": "Second?", "required": true, "answerMode": "text"}]}}),
    );
    serve(&server, &["f-1"], vec![c]).await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    assert_eq!(
        Harness::buttons(&h.card("f-1").await.1),
        vec!["✍️ Ответить текстом"]
    );
    // A plain message is not an answer to a form.
    h.text("loose", None).await;
    assert_eq!(last_text(&h), NO_QUESTION);
    assert!(
        h.ctx
            .store
            .draft(h.delivery("f-1").await.id)
            .await
            .unwrap()
            .is_none()
    );
}
