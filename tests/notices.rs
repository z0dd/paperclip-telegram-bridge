//! The ⚠️ rule on the harness (`domain::is_service_notice` /
//! `is_system_notice`, docs/paperclip-api.md §6.1): which Paperclip comments
//! reach the topic marked as service notices and which stay ordinary replies.

mod support;

use serde_json::json;
use support::harness::Harness;
use support::paperclip::{Forced, PaperclipMock, agent_json};

const PET_LEAD: &str = "PET · PET Lead";

async fn pet() -> (PaperclipMock, Harness, i64) {
    let pc = PaperclipMock::start().await;
    pc.company("company-pet", "PET")
        .agent("company-pet", "agent-lead", "PET Lead")
        .chat("company-pet", "issue-1", "PET-4", "agent-lead");
    let h = Harness::start(&pc, &[]).await;
    h.sync().await;
    let thread = h.tg.topic(PET_LEAD).expect("topic created");
    (pc, h, thread)
}

/// A comment of the conversation agent written by `run_id`.
fn run_comment(pc: &PaperclipMock, body: &str, run_id: &str) {
    let mut c = agent_json("agent-lead", body);
    c["createdByRunId"] = json!(run_id);
    pc.comment("issue-1", c, true);
}

#[tokio::test]
async fn system_comment_is_marked_without_a_run_lookup() {
    let (pc, h, thread) = pet().await;
    pc.system_says("issue-1", "Agent run was cancelled by the board");
    h.sync().await;
    assert_eq!(
        h.tg.in_topic(thread),
        ["⚠️ Agent run was cancelled by the board"]
    );
    assert_eq!(pc.run_lookups().await, 0, "a system comment needs no run");
}

#[tokio::test]
async fn failed_run_notice_is_marked_whatever_the_status_or_case() {
    let (pc, h, thread) = pet().await;
    // The comment repeats the run status.
    pc.run("run-cancelled", "cancelled", None);
    run_comment(&pc, "cancelled", "run-cancelled");
    // The status of the run is in another case than the comment.
    pc.run("run-timeout", "TIMED_OUT", None);
    run_comment(&pc, "timed_out", "run-timeout");
    // The comment repeats the first line of the error, spaces around.
    pc.run(
        "run-failed",
        "failed",
        Some("  Limit reached  \nsecond line"),
    );
    run_comment(&pc, "limit reached", "run-failed");
    h.sync().await;
    assert_eq!(
        h.tg.in_topic(thread),
        ["⚠️ cancelled", "⚠️ timed_out", "⚠️ limit reached"]
    );
}

#[tokio::test]
async fn ordinary_reply_of_a_failed_run_is_not_marked() {
    let (pc, h, thread) = pet().await;
    pc.run(
        "run-a",
        "failed",
        Some("ACP agent reported a terminal limit failure."),
    );
    // Only the second line of the error is not the notice.
    pc.run("run-b", "failed", Some("first line\nUseful answer"));
    // A reply that only contains the error text is not the error line.
    pc.run("run-c", "cancelled", None);
    run_comment(&pc, "Готово, вот план", "run-a");
    run_comment(&pc, "Useful answer", "run-b");
    run_comment(&pc, "Готово: cancelled в UI не было", "run-c");
    h.sync().await;
    assert_eq!(
        h.tg.in_topic(thread),
        [
            "Готово, вот план",
            "Useful answer",
            "Готово: cancelled в UI не было"
        ]
    );
}

#[tokio::test]
async fn text_equal_to_the_error_of_a_successful_run_is_not_marked() {
    let (pc, h, thread) = pet().await;
    for (i, status) in ["succeeded", "running", "queued"].iter().enumerate() {
        let run = format!("run-{i}");
        pc.run(&run, status, Some("same text"));
        run_comment(&pc, "same text", &run);
    }
    h.sync().await;
    assert_eq!(
        h.tg.in_topic(thread),
        ["same text", "same text", "same text"]
    );
}

#[tokio::test]
async fn run_lookup_failure_forwards_without_the_mark_and_loses_nothing() {
    let (pc, h, thread) = pet().await;
    let text = "ACP agent reported a terminal limit failure.";
    pc.fail_run("run-500", Forced::new(500, json!({"error": "boom"})));
    // 200 without `status`: not a run.
    pc.st()
        .runs
        .insert("run-bad".into(), json!({"id": "run-bad"}));
    run_comment(&pc, text, "run-500");
    run_comment(&pc, text, "run-bad");
    pc.agent_says("issue-1", "after the errors");
    h.sync().await;
    assert_eq!(h.tg.in_topic(thread), [text, text, "after the errors"]);
    assert!(pc.run_lookups().await >= 2);
    // Delivered once: the next passes do not repeat anything.
    h.sync().await;
    h.sync().await;
    assert_eq!(h.tg.in_topic(thread).len(), 3);
}

#[tokio::test]
async fn long_multiline_and_run_less_comments_are_never_looked_up() {
    let (pc, h, thread) = pet().await;
    let long = "x".repeat(301);
    pc.run("run-long", "failed", Some(&long));
    run_comment(&pc, &long, "run-long");
    pc.run("run-multi", "failed", Some("line one"));
    run_comment(&pc, "line one\nline two", "run-multi");
    // No `createdByRunId` at all.
    pc.comment(
        "issue-1",
        json!({"body": "failed", "authorType": "agent", "authorAgentId": "agent-lead",
               "origin": "comment"}),
        true,
    );
    // A receipt is an ordinary message even when it says "failed".
    pc.run("run-r", "failed", Some("failed"));
    pc.receipt("issue-1", "failed");
    h.sync().await;
    assert_eq!(
        h.tg.in_topic(thread),
        [long.as_str(), "line one\nline two", "failed", "failed"]
    );
    assert_eq!(pc.run_lookups().await, 0);
}

#[tokio::test]
async fn notice_and_replies_keep_their_order_in_one_pass() {
    let (pc, h, thread) = pet().await;
    pc.agent_says("issue-1", "first");
    pc.service_notice("issue-1", "run-x", "Run failed\ndetails");
    pc.system_says("issue-1", "system");
    pc.agent_says("issue-1", "last");
    h.sync().await;
    assert_eq!(
        h.tg.in_topic(thread),
        ["first", "⚠️ Run failed", "⚠️ system", "last"]
    );
}
