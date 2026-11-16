//! v1.0.0 drafts of one-question text cards are dropped at start
//! (ADR-0009 §2, decision Р4). The start sequence of `app::run` is replayed
//! on the harness: reset after `recover`, then the notices.

mod support;

use std::sync::atomic::Ordering;

use bridge::core::forms;
use bridge::domain::{Awaiting, Draft, FormAnswer};
use bridge::store::{DeliveryStatus, ResetDraft, StoreOp};
use serde_json::{Value, json};
use support::harness::{Harness, TgCall};
use support::paperclip::*;
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

const NOTICE_A: &str = "🔄 Bridge обновлён: черновик ответа на PET-1 сброшен, текст не отправлен. Ответьте на вопрос обычным сообщением.";

fn text_card(id: &str) -> Value {
    card(
        id,
        "ask_user_questions",
        "pending",
        json!({"version": 1, "questionSet": {"questions": [
            {"id": "notes", "prompt": "Notes?", "required": true, "answerMode": "text"}]}}),
    )
}

fn form_card(id: &str) -> Value {
    card(
        id,
        "ask_user_questions",
        "pending",
        json!({"version": 1, "questionSet": {"questions": [
            {"id": "a", "prompt": "First?", "required": true, "answerMode": "text"},
            {"id": "b", "prompt": "Second?", "required": true, "answerMode": "text"}]}}),
    )
}

fn reason_card(id: &str) -> Value {
    let mut c = confirmation(id);
    c["payload"]["rejectRequiresReason"] = json!(true);
    c
}

fn draft(delivery_id: i64, awaiting: Awaiting, texts: &[&str]) -> Draft {
    let mut d = Draft::new(delivery_id);
    d.awaiting = Some(awaiting);
    d.answers.form = texts
        .iter()
        .map(|t| {
            Some(FormAnswer {
                option_ids: vec![],
                other_text: Some(t.to_string()),
            })
        })
        .collect();
    d.prompt_message_id = Some(4242);
    d
}

/// State of v1.0.0 with drafts A (text, sent), B (form), C (reject reason),
/// D (text, closed).
async fn v1_state() -> (MockServer, Harness, [i64; 4]) {
    let server = MockServer::start().await;
    serve(
        &server,
        &["a", "b", "c", "d"],
        vec![
            text_card("a"),
            form_card("b"),
            reason_card("c"),
            text_card("d"),
        ],
    )
    .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;
    let h = Harness::start(&server.uri(), &[]).await;
    h.sync().await;
    let ids = [
        h.delivery("a").await.id,
        h.delivery("b").await.id,
        h.delivery("c").await.id,
        h.delivery("d").await.id,
    ];
    h.ctx
        .store
        .set_status(ids[3], DeliveryStatus::Closed, Some("cancelled".into()))
        .await
        .unwrap();
    h.ctx
        .store
        .apply(vec![
            StoreOp::PutDraft(draft(ids[0], Awaiting::Text { question: 0 }, &["typed a"])),
            StoreOp::PutDraft(draft(ids[1], Awaiting::Text { question: 1 }, &["one"])),
            StoreOp::PutDraft(draft(
                ids[2],
                Awaiting::RejectReason { optional: false },
                &[],
            )),
            StoreOp::PutDraft(draft(ids[3], Awaiting::Text { question: 0 }, &["typed d"])),
        ])
        .await
        .unwrap();
    (server, h, ids)
}

async fn snapshot(h: &Harness, ids: &[i64; 4]) -> Vec<(i64, Option<Draft>)> {
    let mut out = Vec::new();
    for id in ids {
        let d = h.ctx.store.delivery(*id).await.unwrap().unwrap();
        out.push((d.generation, h.ctx.store.draft(*id).await.unwrap()));
    }
    out
}

async fn outbox_empty(h: &Harness, ids: &[i64; 4]) -> bool {
    for id in ids {
        if h.ctx.store.outbox_for(*id).await.unwrap().is_some() {
            return false;
        }
    }
    h.ctx.store.pending_outbox_count().await.unwrap() == 0
}

/// `app::run`: reset after `recover`, then the notice before `announce_restart`.
async fn start_steps(h: &Harness) -> Vec<ResetDraft> {
    let reset = h.ctx.store.reset_chat_text_drafts().await.unwrap();
    forms::announce_chat_text_reset(&h.ctx, &reset).await;
    reset
}

#[tokio::test]
async fn first_start_resets_only_text_card_drafts() {
    let (server, h, ids) = v1_state().await;
    let before = snapshot(&h, &ids).await;
    let calls_before = h.tg.calls().len();

    let reset = start_steps(&h).await;
    assert_eq!(
        reset,
        vec![ResetDraft {
            delivery_id: ids[0],
            issue_ref: "PET-1".into()
        }]
    );
    let after = snapshot(&h, &ids).await;
    // A and D: draft deleted, generation +1.
    for i in [0, 3] {
        assert!(after[i].1.is_none(), "draft {i} deleted");
        assert_eq!(after[i].0, before[i].0 + 1, "gen {i}");
    }
    // B and C: rows unchanged.
    for i in [1, 2] {
        assert_eq!(after[i], before[i], "draft {i} untouched");
    }
    assert!(outbox_empty(&h, &ids).await);
    assert!(posts(&server, "").await.is_empty(), "no POST to Paperclip");

    // Telegram: one re-render and one notice, only for A.
    let a_msg = h.delivery("a").await.last_message_id().unwrap();
    let new_calls: Vec<TgCall> = h.tg.calls()[calls_before..].to_vec();
    assert_eq!(new_calls.len(), 2, "{new_calls:?}");
    match &new_calls[0] {
        TgCall::Edit { id, html, keyboard } => {
            assert_eq!(*id, a_msg);
            assert!(
                html.contains("Ответьте обычным сообщением в этот чат"),
                "{html}"
            );
            assert!(keyboard.is_none());
        }
        other => panic!("expected edit of A, got {other:?}"),
    }
    match &new_calls[1] {
        TgCall::Send { html, .. } => assert_eq!(html, NOTICE_A),
        other => panic!("expected the notice, got {other:?}"),
    }
    assert!(!h.tg.texts().iter().any(|t| t.contains("typed")));

    // The restart notice no longer mentions the reset draft.
    forms::announce_restart(&h.ctx).await.unwrap();
    let restart: Vec<String> =
        h.tg.texts()
            .into_iter()
            .filter(|t| t.contains("Bridge перезапущен"))
            .collect();
    assert_eq!(restart.len(), 2, "B and C only: {restart:?}");

    // The text answer goes straight to the queue.
    h.text("fresh answer", Some(a_msg)).await;
    h.outbox().await;
    assert_eq!(
        posts(&server, "/a/respond").await,
        vec![
            json!({"answers": [{"questionId": "notes", "optionIds": [], "otherText": "fresh answer"}],
                    "summaryMarkdown": null})
        ]
    );
}

#[tokio::test]
async fn second_start_changes_nothing() {
    let (server, h, ids) = v1_state().await;
    start_steps(&h).await;
    let h = h.restart().await;
    let before = snapshot(&h, &ids).await;
    let calls_before = h.tg.calls().len();

    let reset = start_steps(&h).await;
    assert!(reset.is_empty());
    assert_eq!(snapshot(&h, &ids).await, before);
    assert!(outbox_empty(&h, &ids).await);
    assert_eq!(h.tg.calls().len(), calls_before, "no edit or send");
    assert!(posts(&server, "").await.is_empty());
}

#[tokio::test]
async fn failed_rerender_does_not_stop_the_start() {
    let (server, h, ids) = v1_state().await;
    let old = h.delivery("a").await;
    // Card message gone and Telegram unreachable for the notice.
    h.tg.fail_edits.store(true, Ordering::SeqCst);
    h.tg.fail_sends.store(true, Ordering::SeqCst);
    let reset = start_steps(&h).await;
    assert_eq!(reset.len(), 1);
    h.tg.fail_edits.store(false, Ordering::SeqCst);
    h.tg.fail_sends.store(false, Ordering::SeqCst);

    assert!(h.ctx.store.draft(ids[0]).await.unwrap().is_none());
    assert_eq!(
        h.delivery("a").await.generation,
        old.generation + 1,
        "state is committed"
    );

    // An old button of A is stale and re-renders the card.
    let data =
        bridge::render::Callback::new(bridge::render::Op::Submit, old.id, old.generation).encode();
    h.press(&data).await;
    assert_eq!(h.tg.answers().last().unwrap().as_deref(), Some("Устарело"));
    let (html, kb) = h.card("a").await;
    assert!(
        html.contains("Ответьте обычным сообщением в этот чат"),
        "{html}"
    );
    assert!(kb.is_none());

    // The next update is handled as usual.
    let a_msg = h.delivery("a").await.last_message_id().unwrap();
    h.text("after failure", Some(a_msg)).await;
    assert_eq!(
        h.tg.texts().last().unwrap(),
        "📨 Ответ на PET-1 принят и поставлен в очередь отправки."
    );
    h.outbox().await;
    assert_eq!(posts(&server, "/a/respond").await.len(), 1);
    assert_eq!(posts(&server, "").await.len(), 1);
}
