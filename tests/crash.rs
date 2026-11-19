//! Crash recovery with `fault-injection` (ADR-0010 §3.3, scenario 6):
//! (a) after POST, before `pc_outbox` commit → exactly one comment;
//! (b) after `sendMessage`, before `sent` → a duplicate is allowed, no loss;
//! (c) after reading comments, before the cursor commit → no loss, no
//! duplicates. Fault points are global: tests run one at a time.
#![cfg(feature = "fault-injection")]

mod support;

use bridge::core::CoreError;
use bridge::fault;
use bridge::store::PcStatus;
use support::harness::Harness;
use support::paperclip::PaperclipMock;

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn pet() -> (PaperclipMock, Harness, i64) {
    let pc = PaperclipMock::start().await;
    pc.company("company-pet", "PET")
        .agent("company-pet", "agent-lead", "PET Lead")
        .chat("company-pet", "issue-1", "PET-4", "agent-lead");
    let h = Harness::start(&pc, &[]).await;
    h.sync().await;
    let thread = h.tg.topic("PET · PET Lead").unwrap();
    (pc, h, thread)
}

#[tokio::test]
async fn crash_after_post_gives_exactly_one_comment() {
    let _g = SERIAL.lock().await;
    let (pc, h, thread) = pet().await;
    h.say(thread, "только один раз").await;
    fault::arm(fault::AFTER_HTTP_BEFORE_OUTBOX_COMMIT);
    assert!(matches!(h.try_outbox().await, Err(CoreError::Fault)));
    let h = h.restart().await;
    h.outbox().await;
    let posts = pc.posts();
    assert_eq!(posts.len(), 2, "the POST is repeated");
    assert_eq!(posts[0].client_request_id, posts[1].client_request_id);
    assert_eq!(pc.user_comments("issue-1").len(), 1, "one comment");
    assert_eq!(
        h.ctx.store.messages().await.unwrap()[0].status,
        PcStatus::Done
    );
}

#[tokio::test]
async fn crash_after_send_repeats_the_piece_without_loss() {
    let _g = SERIAL.lock().await;
    let (pc, h, thread) = pet().await;
    pc.agent_says("issue-1", "ответ");
    h.cycle().await;
    fault::arm(fault::AFTER_SEND_BEFORE_COMMIT);
    assert!(matches!(h.try_drain().await, Err(CoreError::Fault)));
    let h = h.restart().await;
    h.sync().await;
    assert_eq!(
        h.tg.in_topic(thread),
        ["ответ", "ответ"],
        "duplicate accepted"
    );
    assert_eq!(h.ctx.store.queue_depth().await.unwrap(), 0);
}

#[tokio::test]
async fn crash_before_cursor_commit_loses_and_duplicates_nothing() {
    let _g = SERIAL.lock().await;
    let (pc, h, thread) = pet().await;
    pc.agent_says("issue-1", "one");
    pc.agent_says("issue-1", "two");
    fault::arm(fault::AFTER_READ_BEFORE_CURSOR_COMMIT);
    assert!(matches!(h.try_cycle().await, Err(CoreError::Fault)));
    let h = h.restart().await;
    h.sync().await;
    h.sync().await;
    assert_eq!(h.tg.in_topic(thread), ["one", "two"]);
}
