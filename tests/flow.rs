//! Main v2 flows on the harness (ADR-0010 §2): discovery and topics,
//! Paperclip → Telegram, Telegram → Paperclip, restarts, topic errors.
//! The full matrix of ADR-0010 §3.3 lives with the Test Engineer's suite.

mod support;

use bridge::core::inbound::client_request_id;
use bridge::store::{PcStatus, TopicState};
use bridge::telegram::TelegramError;
use serde_json::json;
use support::harness::{GROUP, Harness, USER};
use support::paperclip::{Forced, PaperclipMock};

const PET_LEAD: &str = "PET · PET Lead";

/// One company `PET` with a conversation `issue-1` with agent `PET Lead`.
async fn pet() -> PaperclipMock {
    let pc = PaperclipMock::start().await;
    pc.company("company-pet", "PET")
        .agent("company-pet", "agent-lead", "PET Lead")
        .chat("company-pet", "issue-1", "PET-4", "agent-lead");
    pc
}

async fn started(pc: &PaperclipMock) -> (Harness, i64) {
    let h = Harness::start(pc, &[]).await;
    h.sync().await;
    let thread = h.tg.topic(PET_LEAD).expect("topic created");
    (h, thread)
}

#[tokio::test]
async fn two_companies_get_their_topics_and_replies() {
    let pc = pet().await;
    pc.company("company-nn", "NN")
        .agent("company-nn", "agent-nn", "Lead")
        .chat("company-nn", "issue-2", "NN-1", "agent-nn");
    let (h, pet_thread) = started(&pc).await;
    let nn_thread = h.tg.topic("NN · Lead").expect("second topic");
    assert_eq!(h.tg.topics().len(), 2);
    assert!(h.tg.sent().is_empty(), "no history at discovery");

    pc.agent_says("issue-1", "**Готово**: план в документе");
    pc.agent_says("issue-2", "Привет из NN");
    h.sync().await;
    assert_eq!(
        h.tg.in_topic(pet_thread),
        ["<b>Готово</b>: план в документе"]
    );
    assert_eq!(h.tg.in_topic(nn_thread), ["Привет из NN"]);
    assert!(h.tg.sent().iter().all(|s| s.html));
}

#[tokio::test]
async fn history_before_discovery_is_not_forwarded() {
    let pc = pet().await;
    pc.agent_says("issue-1", "old 1");
    pc.user_says("issue-1", "old question");
    pc.agent_says("issue-1", "old 2");
    let (h, thread) = started(&pc).await;
    h.sync().await;
    assert!(h.tg.sent().is_empty());
    pc.agent_says("issue-1", "new");
    h.sync().await;
    assert_eq!(h.tg.in_topic(thread), ["new"]);
}

#[tokio::test]
async fn agent_replies_receipts_and_system_notices_go_to_telegram() {
    let pc = pet().await;
    pc.agent("company-pet", "agent-other", "Other");
    let (h, thread) = started(&pc).await;
    pc.user_says("issue-1", "from the UI");
    pc.system_says("issue-1", "Run failed");
    pc.comment(
        "issue-1",
        json!({"body": "someone else", "authorType": "agent", "authorAgentId": "agent-other"}),
        true,
    );
    pc.comment(
        "issue-1",
        json!({"body": "deleted", "authorType": "agent", "authorAgentId": "agent-lead",
               "deletedAt": "2026-11-19T10:00:00.000Z"}),
        true,
    );
    pc.receipt("issue-1", "Принято");
    pc.agent_says("issue-1", "Ответ");
    h.sync().await;
    assert_eq!(h.tg.in_topic(thread), ["⚠️ Run failed", "Принято", "Ответ"]);
}

#[tokio::test]
async fn service_notices_are_marked_and_replies_are_not() {
    let pc = pet().await;
    let (h, thread) = started(&pc).await;
    pc.service_notice(
        "issue-1",
        "run-failed",
        "ACP agent reported a terminal limit failure.\nYou've hit your session limit",
    );
    // A reply written by a run that failed later stays an ordinary reply.
    pc.run(
        "run-progress",
        "failed",
        Some("ACP agent reported a terminal limit failure."),
    );
    let mut progress = support::paperclip::agent_json("agent-lead", "Прогресс");
    progress["createdByRunId"] = json!("run-progress");
    pc.comment("issue-1", progress, true);
    // Unknown run (404): forwarded without the mark.
    pc.agent_says("issue-1", "Ответ");
    h.sync().await;
    assert_eq!(
        h.tg.in_topic(thread),
        [
            "⚠️ ACP agent reported a terminal limit failure.",
            "Прогресс",
            "Ответ"
        ]
    );
}

#[tokio::test]
async fn new_conversation_gets_a_topic_without_restart() {
    let pc = pet().await;
    let (h, _) = started(&pc).await;
    pc.agent("company-pet", "agent-dev", "Developer");
    pc.chat("company-pet", "issue-3", "PET-9", "agent-dev");
    h.sync().await;
    let t =
        h.tg.topic("PET · Developer")
            .expect("topic for the new conversation");
    pc.agent_says("issue-3", "hello");
    h.sync().await;
    assert_eq!(h.tg.in_topic(t), ["hello"]);
}

#[tokio::test]
async fn telegram_message_becomes_one_comment() {
    let pc = pet().await;
    pc.st().auto_reply = true;
    let (h, thread) = started(&pc).await;
    let raw = h.message(Some(thread), USER, GROUP, json!({"text": "Сделай отчёт"}));
    let u = h.update(raw.clone()).await;
    h.outbox().await;
    let posts = pc.posts();
    assert_eq!(posts.len(), 1);
    let m = u.message.as_ref().unwrap();
    assert_eq!(
        posts[0].client_request_id.as_deref(),
        Some(client_request_id(GROUP, m.message_id).as_str())
    );
    assert_eq!(posts[0].body.as_deref(), Some("Сделай отчёт"));
    assert_eq!(posts[0].status, 201);

    // The same update again (crash before the offset was saved): no new POST.
    h.update(raw).await;
    h.outbox().await;
    assert_eq!(pc.posts().len(), 1);
    assert_eq!(pc.user_comments("issue-1").len(), 1);

    // The receipt (no updatedAt bump) and the reply arrive: the conversation is hot.
    h.sync().await;
    assert_eq!(
        h.tg.in_topic(thread),
        ["Принято, отвечу скоро.", "Ответ на: Сделай отчёт"]
    );
    let entry = &h.ctx.store.messages().await.unwrap()[0];
    assert_eq!(entry.status, PcStatus::Done);
    assert!(entry.body.is_none(), "body is wiped");
}

#[tokio::test]
async fn foreign_chat_user_general_and_media_are_not_forwarded() {
    let pc = pet().await;
    let (h, thread) = started(&pc).await;
    let foreign_chat = h.message(Some(thread), USER, -100999, json!({"text": "x"}));
    let foreign_user = h.message(Some(thread), 999, GROUP, json!({"text": "x"}));
    let general = h.message(None, USER, GROUP, json!({"text": "x"}));
    let photo = h.message(
        Some(thread),
        USER,
        GROUP,
        json!({"photo": [{"file_id": "f"}]}),
    );
    let unlinked = h.message(Some(4242), USER, GROUP, json!({"text": "x"}));
    let service = h.message(
        Some(thread),
        USER,
        GROUP,
        json!({"forum_topic_reopened": {}}),
    );
    for u in [
        foreign_chat,
        foreign_user,
        general,
        photo,
        unlinked,
        service,
    ] {
        h.update(u).await;
    }
    h.outbox().await;
    h.drain().await;
    assert!(pc.posts().is_empty());
    assert_eq!(h.ctx.store.telegram_offset().await.unwrap(), 7);
    // Silent for foreign chat and user; hints for General, media, unlinked topic.
    assert_eq!(h.tg.general().len(), 1, "{:?}", h.tg.sent());
    assert!(h.tg.general()[0].contains("тему"));
    let in_topic = h.tg.in_topic(thread);
    assert_eq!(in_topic.len(), 1);
    assert!(in_topic[0].contains("только текст"));
    assert_eq!(h.tg.in_topic(4242).len(), 1);
}

#[tokio::test]
async fn restart_keeps_topics_cursor_and_offset() {
    let pc = pet().await;
    let (h, thread) = started(&pc).await;
    pc.agent_says("issue-1", "one");
    h.sync().await;
    h.say(thread, "hi").await;
    h.outbox().await;
    let offset = h.ctx.store.telegram_offset().await.unwrap();
    let h = h.restart().await;
    h.sync().await;
    assert_eq!(h.tg.topics().len(), 1, "topic not created again");
    assert_eq!(h.ctx.store.telegram_offset().await.unwrap(), offset);
    pc.agent_says("issue-1", "two");
    h.sync().await;
    assert_eq!(
        h.tg.in_topic(thread),
        ["one", "two"],
        "no duplicates, no loss"
    );
    assert_eq!(pc.posts().len(), 1);
}

#[tokio::test]
async fn deleted_topic_is_recreated_and_the_reply_delivered() {
    let pc = pet().await;
    let (h, thread) = started(&pc).await;
    h.tg.st().deleted.insert(thread);
    pc.agent_says("issue-1", "after delete");
    h.sync().await;
    let topics = h.tg.topics();
    assert_eq!(topics.len(), 2);
    let new_thread = topics[1].0;
    assert_eq!(topics[1].1, PET_LEAD);
    assert_eq!(h.tg.in_topic(new_thread), ["after delete"]);
    let c = h
        .ctx
        .store
        .conversation("issue-1".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(c.topic_thread_id, Some(new_thread));
    assert_eq!(c.topic_state, TopicState::Active);
    // Messages from the new topic reach the conversation.
    h.say(new_thread, "still here").await;
    h.outbox().await;
    assert_eq!(pc.posts().len(), 1);
}

#[tokio::test]
async fn closed_topic_is_reopened() {
    let pc = pet().await;
    let (h, thread) = started(&pc).await;
    h.tg.st().closed.insert(thread);
    pc.agent_says("issue-1", "after close");
    h.sync().await;
    assert_eq!(h.tg.st().reopened, [thread]);
    assert_eq!(h.tg.in_topic(thread), ["after close"]);
    assert_eq!(h.tg.topics().len(), 1);
}

#[tokio::test]
async fn broken_html_falls_back_to_plain_text() {
    let pc = pet().await;
    let (h, thread) = started(&pc).await;
    h.tg.st().reject_html = true;
    pc.agent_says("issue-1", "**bold** text");
    h.sync().await;
    let sent = h.tg.sent();
    assert_eq!(sent.len(), 1);
    assert!(!sent[0].html);
    assert_eq!(sent[0].text, "**bold** text");
    assert_eq!(sent[0].thread, Some(thread));
}

#[tokio::test]
async fn long_reply_is_split_in_order() {
    let pc = pet().await;
    let h = Harness::start(&pc, &[("MAX_MESSAGE_CHUNKS", "20")]).await;
    h.sync().await;
    let thread = h.tg.topic(PET_LEAD).unwrap();
    let body = (0..3000)
        .map(|i| format!("w{i}"))
        .collect::<Vec<_>>()
        .join(" ");
    pc.agent_says("issue-1", &body);
    h.sync().await;
    let parts = h.tg.in_topic(thread);
    assert!(parts.len() > 1, "{}", parts.len());
    assert_eq!(parts.join(" ").split_whitespace().count(), 3000);
    assert!(parts[0].starts_with("w0 "));
    assert!(parts.iter().all(|p| p.encode_utf16().count() <= 4096));
}

#[tokio::test]
async fn very_long_reply_is_cut_to_max_message_chunks() {
    let pc = pet().await;
    let (h, thread) = started(&pc).await;
    let body = (0..6000)
        .map(|i| format!("w{i}"))
        .collect::<Vec<_>>()
        .join(" ");
    pc.agent_says("issue-1", &body);
    h.sync().await;
    let parts = h.tg.in_topic(thread);
    assert_eq!(parts.len(), 3, "MAX_MESSAGE_CHUNKS default");
    assert!(parts[2].contains("полностью — в Paperclip"));
}

#[tokio::test]
async fn anchor_returned_again_gives_no_duplicates_in_both_server_modes() {
    for inclusive in [true, false] {
        let pc = pet().await;
        pc.st().anchor_inclusive = inclusive;
        let (h, thread) = started(&pc).await;
        for i in 0..3 {
            pc.agent_says("issue-1", &format!("m{i}"));
            h.sync().await;
            h.sync().await;
        }
        assert_eq!(
            h.tg.in_topic(thread),
            ["m0", "m1", "m2"],
            "inclusive={inclusive}"
        );
    }
}

#[tokio::test]
async fn vanished_anchor_is_recovered_from_the_newest_page() {
    let pc = pet().await;
    let (h, thread) = started(&pc).await;
    let first = pc.agent_says("issue-1", "first");
    h.sync().await;
    pc.delete_comment("issue-1", &first);
    pc.agent_says("issue-1", "second");
    pc.agent_says("issue-1", "third");
    h.sync().await;
    assert_eq!(h.tg.in_topic(thread), ["first", "second", "third"]);
    h.sync().await;
    assert_eq!(h.tg.in_topic(thread).len(), 3);
}

#[tokio::test]
async fn paused_conversation_is_reported_in_the_topic() {
    let pc = pet().await;
    let (h, thread) = started(&pc).await;
    pc.fail_post(
        "issue-1",
        Forced::new(
            409,
            json!({"error": "Task is paused. Resume it before sending a message."}),
        ),
    );
    h.say(thread, "hello").await;
    h.outbox().await;
    h.drain().await;
    let e = &h.ctx.store.messages().await.unwrap()[0];
    assert_eq!(e.status, PcStatus::Rejected);
    let texts = h.tg.in_topic(thread);
    assert_eq!(texts.len(), 1);
    assert!(texts[0].contains("на паузе"), "{texts:?}");
}

#[tokio::test]
async fn paperclip_outage_keeps_the_message_and_tells_once() {
    let pc = pet().await;
    let (h, thread) = started(&pc).await;
    for _ in 0..3 {
        pc.fail_post("issue-1", Forced::new(502, json!({})));
    }
    h.say(thread, "hello").await;
    for _ in 0..4 {
        h.outbox().await;
        // Backoff never waits less than retry::MIN_DELAY.
        tokio::time::sleep(bridge::retry::MIN_DELAY + std::time::Duration::from_millis(20)).await;
    }
    h.drain().await;
    let e = &h.ctx.store.messages().await.unwrap()[0];
    assert_eq!(e.status, PcStatus::Done, "{e:?} {:?}", pc.posts());
    assert_eq!(pc.user_comments("issue-1").len(), 1);
    let queued: Vec<String> =
        h.tg.in_topic(thread)
            .into_iter()
            .filter(|t| t.contains("в очереди"))
            .collect();
    assert_eq!(queued.len(), 1, "told once (RETRY_NOTIFY_AFTER=2)");
}

#[tokio::test]
async fn agent_chat_disabled_is_reported_once_and_unhealthy() {
    let pc = pet().await;
    pc.fail_chats(
        "company-pet",
        Forced::new(404, json!({"error": "Agent Chat is disabled"})),
    );
    let h = Harness::start(&pc, &[]).await;
    h.sync().await;
    h.sync().await;
    assert_eq!(h.tg.general().len(), 1);
    assert!(h.tg.general()[0].contains("Agent Chat"));
    assert!(!h.ctx.health.snapshot().agent_chat_ok);
    pc.heal_chats("company-pet");
    h.sync().await;
    assert!(h.ctx.health.snapshot().agent_chat_ok);
    assert!(h.tg.topic(PET_LEAD).is_some());
}

#[tokio::test]
async fn rejected_key_is_reported_and_unhealthy() {
    let pc = pet().await;
    let (h, _) = started(&pc).await;
    pc.revoke(support::paperclip::KEY);
    h.sync().await;
    h.sync().await;
    let general = h.tg.general();
    assert_eq!(general.len(), 1, "{general:?}");
    assert!(general[0].contains("401"));
    assert!(!h.ctx.health.snapshot().paperclip_auth_ok);
}

#[tokio::test]
async fn removed_conversation_unlinks_its_topic() {
    let pc = pet().await;
    let (h, thread) = started(&pc).await;
    pc.remove_chat("issue-1");
    h.sync().await;
    let c = h
        .ctx
        .store
        .conversation("issue-1".into())
        .await
        .unwrap()
        .unwrap();
    assert!(c.removed_at.is_some());
    h.say(thread, "anyone?").await;
    h.outbox().await;
    h.drain().await;
    assert!(pc.posts().is_empty());
    assert!(h.tg.in_topic(thread)[0].contains("не связана"));
}

#[tokio::test]
async fn several_keys_see_their_companies() {
    let pc = pet().await;
    pc.st()
        .companies
        .push(json!({"id": "company-nn", "name": "NN", "issuePrefix": "NN", "status": "active"}));
    pc.key(
        "pcp_second_placeholder",
        support::paperclip::USER,
        &["company-nn"],
    )
    .agent("company-nn", "agent-nn", "Lead")
    .chat("company-nn", "issue-2", "NN-1", "agent-nn");
    let h = Harness::start_keys(
        &pc,
        &[support::paperclip::KEY, "pcp_second_placeholder"],
        &[],
    )
    .await;
    h.sync().await;
    let nn = h.tg.topic("NN · Lead").expect("topic of the second key");
    assert!(h.tg.topic(PET_LEAD).is_some());
    h.say(nn, "to NN").await;
    h.outbox().await;
    assert_eq!(pc.user_comments("issue-2").len(), 1);
}

#[tokio::test]
async fn topic_creation_waits_for_retry_after() {
    let pc = pet().await;
    let h = Harness::start(&pc, &[]).await;
    h.tg.st()
        .topic_errors
        .push_back(TelegramError::RateLimited {
            retry_after: std::time::Duration::from_secs(30),
        });
    h.cycle().await;
    h.drain().await;
    assert!(h.tg.topics().is_empty());
    h.drain().await; // `drain` clears the pause, as if the time passed
    assert_eq!(h.tg.topics().len(), 1);
}
