//! Crash injection (feature `fault-injection`): a crash between a network
//! side effect and the state commit loses nothing and never duplicates an
//! answer in Paperclip (architecture §4.2, §12 stage 4).
#![cfg(feature = "fault-injection")]

mod support;

use bridge::fault;
use bridge::store::DeliveryStatus;
use serde_json::json;
use support::harness::Harness;
use support::paperclip::*;
use tokio::sync::Mutex;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Fault points are process-global: run these tests one at a time.
static SERIAL: Mutex<()> = Mutex::const_new(());

#[tokio::test]
async fn crash_after_send_before_commit_redelivers_the_card() {
    let _guard = SERIAL.lock().await;
    let server = MockServer::start().await;
    serve(&server, &["card-1"], vec![confirmation("card-1")]).await;
    let h = Harness::start(&server.uri(), &[]).await;
    fault::arm(fault::AFTER_SEND_BEFORE_COMMIT);
    assert!(
        h.try_sync().await.is_ok(),
        "delivery errors are logged, not fatal"
    );
    assert_eq!(h.tg.sent().len(), 1, "message went out");
    assert_eq!(h.delivery("card-1").await.status, DeliveryStatus::Sending);

    let h = h.restart().await;
    h.sync().await;
    // Documented trade-off: the card may be shown twice, it is never lost.
    assert_eq!(h.tg.sent().len(), 2);
    let d = h.delivery("card-1").await;
    assert_eq!(d.status, DeliveryStatus::Sent);
    assert_eq!(d.last_message_id(), Some(h.tg.sent()[1].0));
}

#[tokio::test]
async fn crash_after_http_before_outbox_commit_sends_once() {
    let _guard = SERIAL.lock().await;
    let server = MockServer::start().await;
    serve(&server, &["card-1"], vec![confirmation("card-1")]).await;
    // The first accept succeeds on the server…
    Mock::given(method("POST"))
        .and(path(format!(
            "/api/issues/{ISSUE}/interactions/card-1/accept"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    h.press_button("card-1", "Approve").await;
    fault::arm(fault::AFTER_HTTP_BEFORE_OUTBOX_COMMIT);
    assert!(h.try_outbox().await.is_err(), "crash injected");

    // …and after the restart the repeated accept gets 409: the card is
    // already accepted by the key owner, so the bridge reports success.
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
        .and(path(format!(
            "/api/issues/{ISSUE}/interactions/card-1/accept"
        )))
        .respond_with(
            ResponseTemplate::new(409).set_body_json(
                json!({"error": "resolved", "code": "interaction_already_resolved"}),
            ),
        )
        .mount(&server)
        .await;
    let h = h.restart().await;
    h.outbox().await;
    let d = h.delivery("card-1").await;
    assert_eq!(d.status, DeliveryStatus::Answered);
    assert!(h.card("card-1").await.0.contains("✅ Ответ отправлен"));
    // Exactly one POST after the restart; nothing else queued.
    assert_eq!(posts(&server, "/accept").await.len(), 1);
    assert_eq!(h.ctx.store.pending_outbox_count().await.unwrap(), 0);
}

#[tokio::test]
async fn crash_before_commit_of_update_loses_nothing() {
    let _guard = SERIAL.lock().await;
    let server = MockServer::start().await;
    serve(&server, &["card-1"], vec![confirmation("card-1")]).await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    let offset = h.ctx.store.telegram_offset().await.unwrap();
    // Telegram delivers the press, but the process dies before the commit:
    // nothing is stored, the offset stays, Telegram re-sends the update.
    let h = h.restart().await;
    assert_eq!(h.ctx.store.telegram_offset().await.unwrap(), offset);
    h.press_button("card-1", "Approve").await;
    h.outbox().await;
    assert_eq!(posts(&server, "/accept").await.len(), 1);
    assert_eq!(h.delivery("card-1").await.status, DeliveryStatus::Answered);
}

fn text_card() -> serde_json::Value {
    card(
        "q-1",
        "ask_user_questions",
        "pending",
        json!({"version": 1, "questionSet": {"questions": [
            {"id": "notes", "prompt": "Notes?", "required": true, "answerMode": "text"}]}}),
    )
}

#[tokio::test]
async fn chat_text_answer_survives_a_restart_and_is_sent_once() {
    // ADR-0009 §1, row 18: the answer is durable once the update is committed.
    let _guard = SERIAL.lock().await;
    let server = MockServer::start().await;
    serve(&server, &["q-1"], vec![text_card()]).await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    h.text("survives", None).await;
    assert_eq!(h.delivery("q-1").await.status, DeliveryStatus::Answering);
    let queued = h.tg.texts().len();

    // Process dies before the outbox ran.
    let h = h.restart().await;
    bridge::core::forms::announce_restart(&h.ctx).await.unwrap();
    h.outbox().await;
    assert_eq!(
        posts(&server, "/respond").await,
        vec![
            json!({"answers": [{"questionId": "notes", "optionIds": [], "otherText": "survives"}],
                    "summaryMarkdown": null})
        ]
    );
    assert_eq!(h.delivery("q-1").await.status, DeliveryStatus::Answered);
    // No second "accepted" status after the restart.
    assert!(
        h.tg.texts()[queued..]
            .iter()
            .all(|t| !t.contains("принят и поставлен"))
    );
}

#[tokio::test]
async fn chat_text_crash_after_http_reconciles_without_a_second_answer() {
    let _guard = SERIAL.lock().await;
    let server = MockServer::start().await;
    serve(&server, &["q-1"], vec![text_card()]).await;
    Mock::given(method("POST"))
        .and(path(format!(
            "/api/issues/{ISSUE}/interactions/q-1/respond"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    let msg = h.delivery("q-1").await.last_message_id().unwrap();
    h.text("in flight", Some(msg)).await;
    fault::arm(fault::AFTER_HTTP_BEFORE_OUTBOX_COMMIT);
    assert!(h.try_outbox().await.is_err(), "crash injected");

    serve(
        &server,
        &[],
        vec![resolved(
            text_card(),
            "answered",
            Some("user-1"),
            None,
            json!({"version": 1, "answers": [{"questionId": "notes", "optionIds": [], "otherText": "in flight"}]}),
        )],
    )
    .await;
    Mock::given(method("POST"))
        .and(path(format!(
            "/api/issues/{ISSUE}/interactions/q-1/respond"
        )))
        .respond_with(
            ResponseTemplate::new(409).set_body_json(
                json!({"error": "resolved", "code": "interaction_already_resolved"}),
            ),
        )
        .mount(&server)
        .await;
    let h = h.restart().await;
    h.outbox().await;
    assert_eq!(h.delivery("q-1").await.status, DeliveryStatus::Answered);
    // Exactly one POST after the restart, answered by 409 and reconciled.
    assert_eq!(posts(&server, "/respond").await.len(), 1);
    assert_eq!(h.ctx.store.pending_outbox_count().await.unwrap(), 0);
}
