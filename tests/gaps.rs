//! Gaps of the ADR-0010 §3.3 matrix on the harness: restart in both
//! directions in one scenario, no echo, `retry_after` on messages and
//! topics, limits of one Telegram message, idempotency of updates, start
//! checks and the topic error classifier end to end.

mod support;

use bridge::core::checks::{CheckError, check};
use bridge::core::inbound::client_request_id;
use bridge::render::{utf16_len, visible_len};
use bridge::store::PcStatus;
use bridge::telegram::{ChatInfo, MemberInfo, TelegramError};
use serde_json::json;
use std::time::Duration;
use support::harness::{FakeTelegram, GROUP, Harness, USER};
use support::paperclip::PaperclipMock;

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

#[tokio::test]
async fn restart_with_work_pending_in_both_directions_loses_and_repeats_nothing() {
    let (pc, h, thread) = pet().await;
    // Telegram → Paperclip: queued, not posted yet.
    h.say(thread, "вопрос").await;
    // Paperclip → Telegram: read and queued, not sent yet.
    pc.agent_says("issue-1", "ответ");
    h.cycle().await;
    assert!(pc.posts().is_empty());
    assert!(h.tg.in_topic(thread).is_empty());

    let h = h.restart().await;
    h.outbox().await;
    h.sync().await;
    assert_eq!(pc.posts().len(), 1);
    assert_eq!(pc.user_comments("issue-1").len(), 1);
    assert_eq!(h.tg.in_topic(thread), ["ответ"]);
    assert_eq!(h.tg.topics().len(), 1);

    // Another restart with nothing pending changes nothing.
    let h = h.restart().await;
    h.outbox().await;
    h.sync().await;
    h.sync().await;
    assert_eq!(pc.posts().len(), 1);
    assert_eq!(h.tg.in_topic(thread), ["ответ"]);
    assert_eq!(h.ctx.store.queue_depth().await.unwrap(), 0);

    // And the bridge still works in both directions afterwards.
    pc.agent_says("issue-1", "ещё");
    h.say(thread, "второй").await;
    h.outbox().await;
    h.sync().await;
    assert_eq!(h.tg.in_topic(thread), ["ответ", "ещё"]);
    assert_eq!(pc.user_comments("issue-1").len(), 2);
}

#[tokio::test]
async fn own_comment_is_not_echoed_back_to_telegram() {
    let (pc, h, thread) = pet().await;
    h.say(thread, "из Telegram").await;
    h.outbox().await;
    assert_eq!(pc.user_comments("issue-1").len(), 1);
    for _ in 0..3 {
        h.sync().await;
    }
    assert!(h.tg.in_topic(thread).is_empty(), "{:?}", h.tg.sent());
    assert_eq!(pc.posts().len(), 1);
    // Also after a restart the user comment is not read as news.
    let h = h.restart().await;
    h.sync().await;
    assert!(h.tg.in_topic(thread).is_empty());
}

#[tokio::test]
async fn same_text_in_two_messages_gives_two_comments_and_the_same_update_one() {
    let (pc, h, thread) = pet().await;
    let first = h.message(Some(thread), USER, GROUP, json!({"text": "ок"}));
    let u1 = h.update(first.clone()).await;
    let u2 = h.say(thread, "ок").await;
    // Telegram redelivers both updates (the offset was not saved).
    h.update(first).await;
    h.outbox().await;
    let posts = pc.posts();
    assert_eq!(posts.len(), 2);
    assert_ne!(posts[0].client_request_id, posts[1].client_request_id);
    let id = |u: &bridge::telegram::Update| {
        client_request_id(GROUP, u.message.as_ref().unwrap().message_id)
    };
    assert_eq!(
        posts[0].client_request_id.as_deref(),
        Some(id(&u1).as_str())
    );
    assert_eq!(
        posts[1].client_request_id.as_deref(),
        Some(id(&u2).as_str())
    );
    assert_eq!(pc.user_comments("issue-1").len(), 2);
}

#[tokio::test]
async fn retry_after_on_send_message_keeps_every_piece_in_order() {
    let (pc, h, thread) = pet().await;
    pc.agent_says("issue-1", "a");
    pc.agent_says("issue-1", "b");
    pc.agent_says("issue-1", "c");
    h.tg.st().send_errors.push_back(TelegramError::RateLimited {
        retry_after: Duration::from_secs(30),
    });
    h.cycle().await;
    h.drain().await;
    assert!(h.tg.in_topic(thread).is_empty(), "the queue is paused");
    assert_eq!(h.ctx.store.queue_depth().await.unwrap(), 3);
    h.drain().await; // `drain` clears the pause, as if the time passed
    assert_eq!(h.tg.in_topic(thread), ["a", "b", "c"]);
    assert_eq!(h.ctx.store.queue_depth().await.unwrap(), 0);
}

#[tokio::test]
async fn retry_after_on_two_topics_creates_each_once() {
    let pc = PaperclipMock::start().await;
    pc.company("company-pet", "PET")
        .agent("company-pet", "agent-lead", "PET Lead")
        .agent("company-pet", "agent-dev", "Developer")
        .chat("company-pet", "issue-1", "PET-4", "agent-lead")
        .chat("company-pet", "issue-2", "PET-5", "agent-dev");
    let h = Harness::start(&pc, &[]).await;
    for _ in 0..2 {
        h.tg.st()
            .topic_errors
            .push_back(TelegramError::RateLimited {
                retry_after: Duration::from_secs(30),
            });
    }
    h.cycle().await;
    for _ in 0..4 {
        h.drain().await;
    }
    let names: Vec<String> = h.tg.topics().into_iter().map(|(_, n)| n).collect();
    assert_eq!(names.len(), 2, "{names:?}");
    assert!(names.contains(&PET_LEAD.to_string()));
    assert!(names.contains(&"PET · Developer".to_string()));
}

/// Messages of a topic after a comment of this size.
async fn sent_for(body: &str) -> Vec<String> {
    let (pc, h, thread) = pet().await;
    pc.agent_says("issue-1", body);
    h.sync().await;
    h.tg.in_topic(thread)
}

#[tokio::test]
async fn messages_around_the_limit_fit_telegram_and_keep_every_character() {
    for n in [3499usize, 3500, 3501, 4095, 4096, 4097, 7000] {
        let parts = sent_for(&"a".repeat(n)).await;
        assert!(!parts.is_empty());
        assert!(
            parts.iter().all(|p| visible_len(p) <= 4096),
            "n={n}: {:?}",
            parts.iter().map(|p| visible_len(p)).collect::<Vec<_>>()
        );
        let total: usize = parts.iter().map(|p| p.matches('a').count()).sum();
        assert_eq!(total, n, "no loss, no duplicates");
    }
}

#[tokio::test]
async fn emoji_and_cyrillic_are_counted_in_utf16_and_never_cut_in_half() {
    // Two UTF-16 units each: 6000 units, more than one message.
    let emoji = "😀".repeat(3000);
    let parts = sent_for(&emoji).await;
    assert!(parts.len() > 1);
    assert!(parts.iter().all(|p| utf16_len(p) <= 4096));
    assert_eq!(parts.concat(), emoji);

    // One unit each, but two bytes: bytes must not be mistaken for units.
    let ru = "я".repeat(4000);
    let parts = sent_for(&ru).await;
    assert!(parts.iter().all(|p| utf16_len(p) <= 4096));
    assert_eq!(parts.concat(), ru);
}

#[tokio::test]
async fn escaped_characters_count_as_one_and_pieces_do_not_cut_entities() {
    // `<` becomes `&lt;` in HTML (4 chars) but is one character for Telegram.
    let body = "<".repeat(5000);
    let parts = sent_for(&body).await;
    assert!(parts.len() > 1);
    for p in &parts {
        assert!(visible_len(p) <= 4096);
        let opened = p.matches('&').count();
        let closed = p.matches(';').count();
        assert_eq!(
            opened,
            closed,
            "entity cut in half: {}",
            &p[..p.len().min(40)]
        );
        assert!(!p.contains('<'), "raw `<` in HTML");
    }
    let total: usize = parts.iter().map(|p| p.matches("&lt;").count()).sum();
    assert_eq!(total, 5000);
}

#[tokio::test]
async fn markup_is_escaped_and_not_executed_in_telegram() {
    let parts = sent_for("<script>alert(1)</script> & **ok**").await;
    assert_eq!(
        parts,
        ["&lt;script&gt;alert(1)&lt;/script&gt; &amp; <b>ok</b>"]
    );
}

#[tokio::test]
async fn bold_lines_keep_their_tags_balanced_across_pieces() {
    let body = (0..120)
        .map(|i| format!("**bold {i}** and `code {i}` and _it {i}_"))
        .collect::<Vec<_>>()
        .join("\n");
    let parts = sent_for(&body).await;
    assert!(parts.len() > 1);
    for p in &parts {
        assert!(visible_len(p) <= 4096);
        for tag in ["b", "code", "i"] {
            assert_eq!(
                p.matches(&format!("<{tag}>")).count(),
                p.matches(&format!("</{tag}>")).count(),
                "<{tag}> unbalanced"
            );
        }
    }
}

#[tokio::test]
async fn topic_error_texts_end_to_end_on_the_real_classifier_outcomes() {
    // The mock returns the typed errors the classifier produces for the
    // texts of tests/telegram_errors.rs; here the bridge's reaction to each.
    let (pc, h, thread) = pet().await;
    pc.agent_says("issue-1", "one");
    h.cycle().await;
    h.tg.st()
        .send_errors
        .push_back(TelegramError::ThreadNotFound);
    h.drain().await;
    h.drain().await;
    assert_eq!(h.tg.topics().len(), 2, "recreated after ThreadNotFound");
    let new_thread = h.tg.topic(PET_LEAD).unwrap();
    assert_ne!(new_thread, thread);
    assert_eq!(h.tg.in_topic(new_thread), ["one"]);

    pc.agent_says("issue-1", "two");
    h.cycle().await;
    h.tg.st().send_errors.push_back(TelegramError::TopicClosed);
    h.drain().await;
    h.drain().await;
    assert_eq!(h.tg.st().reopened, [new_thread]);
    assert_eq!(h.tg.in_topic(new_thread), ["one", "two"]);
    assert_eq!(h.tg.topics().len(), 2, "reopened, not recreated");
}

#[tokio::test]
async fn service_notice_to_a_deleted_topic_goes_to_general() {
    let (_pc, h, thread) = pet().await;
    h.tg.st().deleted.insert(thread);
    h.ctx.notify(Some(thread), "notice");
    h.drain().await;
    assert!(
        h.tg.general().contains(&"notice".to_string()),
        "{:?}",
        h.tg.sent()
    );
}

fn tg(kind: &str, forum: bool, status: &str, manage: bool) -> FakeTelegram {
    let t = FakeTelegram::default();
    {
        let mut st = t.st();
        st.chat = ChatInfo {
            kind: kind.into(),
            is_forum: forum,
        };
        st.member = MemberInfo {
            status: status.into(),
            can_manage_topics: manage,
        };
    }
    t
}

#[tokio::test]
async fn start_checks_accept_only_a_forum_supergroup_with_an_admin_bot() {
    assert_eq!(
        check(&tg("supergroup", true, "administrator", true), GROUP).await,
        Ok(())
    );
    for (t, what) in [
        (tg("group", true, "administrator", true), "not a supergroup"),
        (tg("private", false, "administrator", true), "private chat"),
        (
            tg("supergroup", false, "administrator", true),
            "topics are off",
        ),
        (tg("supergroup", true, "member", false), "not an admin"),
        (tg("supergroup", true, "restricted", false), "restricted"),
        (
            tg("supergroup", true, "administrator", false),
            "no manage topics",
        ),
    ] {
        match check(&t, GROUP).await {
            Err(CheckError::Setup(reason)) => assert!(!reason.is_empty(), "{what}"),
            other => panic!("{what}: {other:?}"),
        }
    }
}

#[test]
fn start_check_errors_split_into_setup_and_try_again() {
    for e in [
        TelegramError::Auth { status: 401 },
        TelegramError::Forbidden,
        TelegramError::BadRequest {
            description: "chat not found".into(),
        },
    ] {
        assert!(matches!(CheckError::from(e), CheckError::Setup(_)));
    }
    for e in [
        TelegramError::Transient { reason: "timeout" },
        TelegramError::RateLimited {
            retry_after: Duration::from_secs(3),
        },
        TelegramError::Decode,
    ] {
        assert_eq!(CheckError::from(e.clone()), CheckError::Unavailable(e));
    }
}

#[tokio::test]
async fn rejected_message_is_not_retried_and_does_not_block_the_next_one() {
    let (pc, h, thread) = pet().await;
    pc.fail_post(
        "issue-1",
        support::paperclip::Forced::new(409, json!({"error": "Task is paused. Resume it."})),
    );
    h.say(thread, "first").await;
    h.say(thread, "second").await;
    h.outbox().await;
    h.drain().await;
    let entries = h.ctx.store.messages().await.unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].status, PcStatus::Rejected);
    assert_eq!(entries[1].status, PcStatus::Done);
    assert_eq!(pc.user_comments("issue-1").len(), 1);
}
