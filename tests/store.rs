//! State store v2 (ADR-0010 §2.4): migration from v1, atomic commits,
//! idempotency, recovery after a crash.

use bridge::store::*;

fn conversation(issue: &str, watermark: Option<&str>) -> NewConversation {
    NewConversation {
        issue_id: issue.into(),
        company_id: "company-1".into(),
        identifier: Some("PET-4".into()),
        agent_id: "agent-1".into(),
        agent_name: "PET Lead".into(),
        key_ref: "0:abcdef01".into(),
        watermark_comment_id: watermark.map(str::to_string),
        updated_at: Some("2026-11-19T10:00:00.000Z".into()),
        topic_name: "PET · PET Lead".into(),
    }
}

fn chunk(comment: &str, i: u32, n: u32) -> NewChunk {
    NewChunk {
        comment_id: comment.into(),
        chunk: i,
        chunks: n,
        html: format!("<b>{comment}/{i}</b>"),
        plain: format!("{comment}/{i}"),
    }
}

fn message(message_id: i64) -> NewPcEntry {
    NewPcEntry {
        chat_id: -100,
        message_id,
        issue_id: "issue-1".into(),
        client_request_id: format!("crid-{message_id}"),
        body: "secret user text".into(),
    }
}

async fn open() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("bridge.db")).unwrap();
    store.recover().await.unwrap();
    (dir, store)
}

#[tokio::test]
async fn v1_state_is_migrated_and_offset_kept() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bridge.db");
    {
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute_batch(
            "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE deliveries (id INTEGER PRIMARY KEY, interaction_id TEXT);
             CREATE TABLE delivery_messages (delivery_id INTEGER REFERENCES deliveries(id), chunk INTEGER, message_id INTEGER);
             CREATE TABLE drafts (delivery_id INTEGER PRIMARY KEY REFERENCES deliveries(id));
             CREATE TABLE outbox (id INTEGER PRIMARY KEY, delivery_id INTEGER REFERENCES deliveries(id));
             CREATE TABLE seen_runs (run_id TEXT PRIMARY KEY, seen_at TEXT NOT NULL);
             CREATE TABLE seen_done_issues (issue_id TEXT PRIMARY KEY, seen_at TEXT NOT NULL);
             INSERT INTO deliveries VALUES (1, 'card-1');
             INSERT INTO delivery_messages VALUES (1, 0, 77);
             INSERT INTO drafts VALUES (1);
             INSERT INTO outbox VALUES (1, 1);
             INSERT INTO meta VALUES ('telegram_offset', '4242'), ('self_user_id', 'u'), ('runs_baseline_at', 'x');
             PRAGMA user_version = 1;",
        )
        .unwrap();
    }
    let store = Store::open(&path).unwrap();
    assert_eq!(store.telegram_offset().await.unwrap(), 4242);
    drop(store);
    let c = rusqlite::Connection::open(&path).unwrap();
    let v: i64 = c
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, SCHEMA_VERSION);
    let tables: Vec<String> = c
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    assert_eq!(
        tables,
        [
            "companies",
            "conversations",
            "meta",
            "pc_outbox",
            "seen_comments",
            "tg_outbox"
        ]
    );
    let keys: Vec<String> = c
        .prepare("SELECT key FROM meta ORDER BY key")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    assert_eq!(keys, ["telegram_offset"]);
}

#[tokio::test]
async fn conversation_is_inserted_once_with_its_watermark_seen() {
    let (_d, s) = open().await;
    assert!(
        s.insert_conversation(conversation("issue-1", Some("c-0")))
            .await
            .unwrap()
    );
    assert!(
        !s.insert_conversation(conversation("issue-1", Some("c-9")))
            .await
            .unwrap()
    );
    let c = s.conversation("issue-1".into()).await.unwrap().unwrap();
    assert_eq!(c.watermark_comment_id.as_deref(), Some("c-0"));
    assert_eq!(c.cursor_comment_id.as_deref(), Some("c-0"));
    assert_eq!(c.topic_state, TopicState::Pending);
    assert_eq!(c.topic_thread_id, None);
    assert_eq!(
        s.seen_of("issue-1".into(), vec!["c-0".into(), "c-1".into()])
            .await
            .unwrap(),
        ["c-0"]
    );
    // Without comments the cursor starts empty.
    assert!(
        s.insert_conversation(conversation("issue-2", None))
            .await
            .unwrap()
    );
    let c2 = s.conversation("issue-2".into()).await.unwrap().unwrap();
    assert_eq!(c2.cursor_comment_id, None);
}

#[tokio::test]
async fn topics_are_created_lost_and_recreated() {
    let (_d, s) = open().await;
    s.insert_conversation(conversation("issue-1", None))
        .await
        .unwrap();
    let job = s.next_topic_job().await.unwrap().unwrap();
    assert_eq!(job.issue_id, "issue-1");
    s.set_topic("issue-1".into(), 101).await.unwrap();
    assert!(s.next_topic_job().await.unwrap().is_none());
    let c = s.conversation_by_thread(101).await.unwrap().unwrap();
    assert_eq!(c.topic_state, TopicState::Active);
    // A stale "lost" for another thread does nothing.
    s.topic_lost("issue-1".into(), 99).await.unwrap();
    assert!(s.next_topic_job().await.unwrap().is_none());
    s.topic_lost("issue-1".into(), 101).await.unwrap();
    let job = s.next_topic_job().await.unwrap().unwrap();
    assert_eq!(job.topic_state, TopicState::Recreating);
    assert!(s.conversation_by_thread(101).await.unwrap().is_none());
    // Removed conversations get no topic.
    s.set_removed("issue-1".into(), Some("2026-11-19T10:00:00.000Z".into()))
        .await
        .unwrap();
    assert!(s.next_topic_job().await.unwrap().is_none());
}

#[tokio::test]
async fn pieces_cursor_and_seen_commit_together_and_keep_order() {
    let (_d, s) = open().await;
    s.insert_conversation(conversation("issue-1", Some("c-0")))
        .await
        .unwrap();
    let batch = CommentBatch {
        issue_id: "issue-1".into(),
        seen: vec!["c-1".into(), "c-2".into()],
        chunks: vec![chunk("c-1", 0, 2), chunk("c-1", 1, 2), chunk("c-2", 0, 1)],
        cursor: Some("c-2".into()),
        swept_at: "2026-11-19T10:00:00.000Z".into(),
        hot_until: Some("2099-01-01T00:00:00.000Z".into()),
    };
    s.commit_comments(batch.clone()).await.unwrap();
    // The same batch again (crash before the cursor was read back) adds nothing.
    s.commit_comments(batch).await.unwrap();
    assert_eq!(s.queue_depth().await.unwrap(), 3);
    let c = s.conversation("issue-1".into()).await.unwrap().unwrap();
    assert_eq!(c.cursor_comment_id.as_deref(), Some("c-2"));
    assert_eq!(c.hot_until.as_deref(), Some("2099-01-01T00:00:00.000Z"));

    let now = "2099-12-31T00:00:00.000Z".to_string();
    // No topic yet: nothing to send.
    assert!(s.next_chunk(now.clone()).await.unwrap().is_none());
    s.set_topic("issue-1".into(), 101).await.unwrap();
    let first = s.next_chunk(now.clone()).await.unwrap().unwrap();
    assert_eq!((first.comment_id.as_str(), first.chunk), ("c-1", 0));
    assert_eq!(first.thread_id, Some(101));
    // A piece waiting for retry blocks the later pieces of its conversation.
    s.chunk_retry(first.id, 1, "2099-12-31T01:00:00.000Z".into())
        .await
        .unwrap();
    assert!(s.next_chunk(now.clone()).await.unwrap().is_none());
    // The later pieces are due already: the sender wakes up for them.
    let due = s.next_chunk_due().await.unwrap().unwrap();
    assert!(due.as_str() < "2099-12-31T01:00:00.000Z", "{due}");
    s.chunk_retry(first.id, 1, now.clone()).await.unwrap();
    let again = s.next_chunk(now.clone()).await.unwrap().unwrap();
    assert_eq!(again.id, first.id);
    assert_eq!(again.attempts, 1);
    s.chunk_sent(first.id, 5001).await.unwrap();
    let second = s.next_chunk(now.clone()).await.unwrap().unwrap();
    assert_eq!((second.comment_id.as_str(), second.chunk), ("c-1", 1));
    s.chunk_use_plain(second.id).await.unwrap();
    let plain = s.next_chunk(now.clone()).await.unwrap().unwrap();
    assert!(plain.use_plain);
    s.chunk_abandon(second.id).await.unwrap();
    let third = s.next_chunk(now.clone()).await.unwrap().unwrap();
    assert_eq!(third.comment_id, "c-2");
    s.chunk_sent(third.id, 5002).await.unwrap();
    assert_eq!(s.queue_depth().await.unwrap(), 0);
    // Delivered text is not kept.
    for (c, status) in s.chunks().await.unwrap() {
        assert!(c.html.is_none() && c.plain.is_none(), "{status}");
    }
}

#[tokio::test]
async fn duplicate_update_queues_one_message_and_offset_moves_once() {
    let (_d, s) = open().await;
    assert_eq!(
        s.enqueue_message(10, message(1)).await.unwrap(),
        CommitOutcome::Applied
    );
    assert_eq!(s.telegram_offset().await.unwrap(), 11);
    assert_eq!(
        s.enqueue_message(10, message(1)).await.unwrap(),
        CommitOutcome::Duplicate
    );
    // Same Telegram message under a new update id: still one entry.
    s.enqueue_message(11, message(1)).await.unwrap();
    assert_eq!(s.messages().await.unwrap().len(), 1);
    assert_eq!(s.advance_offset(5).await.unwrap(), CommitOutcome::Duplicate);
    assert_eq!(s.advance_offset(12).await.unwrap(), CommitOutcome::Applied);
    assert_eq!(s.telegram_offset().await.unwrap(), 13);
}

#[tokio::test]
async fn in_flight_message_returns_to_pending_after_a_crash() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bridge.db");
    let s = Store::open(&path).unwrap();
    s.recover().await.unwrap();
    s.enqueue_message(1, message(1)).await.unwrap();
    let e = s
        .due_messages("2099-01-01T00:00:00.000Z".into())
        .await
        .unwrap()
        .remove(0);
    s.message_in_flight(e.id).await.unwrap();
    assert!(
        s.due_messages("2099-01-01T00:00:00.000Z".into())
            .await
            .unwrap()
            .is_empty()
    );
    drop(s);
    let s = Store::open(&path).unwrap();
    let rec = s.recover().await.unwrap();
    assert_eq!(rec.pc_reset, 1);
    let e = s
        .due_messages("2099-01-01T00:00:00.000Z".into())
        .await
        .unwrap()
        .remove(0);
    assert_eq!(e.status, PcStatus::Pending);
    assert_eq!(e.body.as_deref(), Some("secret user text"));
}

#[tokio::test]
async fn finished_messages_wipe_the_body() {
    let (_d, s) = open().await;
    s.enqueue_message(1, message(1)).await.unwrap();
    s.enqueue_message(2, message(2)).await.unwrap();
    let due = s
        .due_messages("2099-01-01T00:00:00.000Z".into())
        .await
        .unwrap();
    s.message_retry(
        due[0].id,
        1,
        "2099-06-01T00:00:00.000Z".into(),
        "http_502".into(),
        true,
    )
    .await
    .unwrap();
    let e = s.message(due[0].id).await.unwrap().unwrap();
    assert_eq!((e.attempts, e.notified), (1, true));
    assert_eq!(e.last_error.as_deref(), Some("http_502"));
    // The other message is due now, the retried one later.
    let next = s.next_message_due().await.unwrap().unwrap();
    assert!(next.as_str() < "2099-06-01T00:00:00.000Z", "{next}");
    s.message_done(due[0].id, Some("c-5".into())).await.unwrap();
    s.message_rejected(due[1].id, "http_409".into())
        .await
        .unwrap();
    let all = s.messages().await.unwrap();
    assert_eq!(all[0].status, PcStatus::Done);
    assert_eq!(all[0].comment_id.as_deref(), Some("c-5"));
    assert_eq!(all[1].status, PcStatus::Rejected);
    assert!(all.iter().all(|e| e.body.is_none()));
}

#[tokio::test]
async fn seen_purge_keeps_cursor_and_newest() {
    let (_d, s) = open().await;
    s.insert_conversation(conversation("issue-1", Some("c-0")))
        .await
        .unwrap();
    let ids: Vec<String> = (1..=5).map(|i| format!("c-{i}")).collect();
    s.commit_comments(CommentBatch {
        issue_id: "issue-1".into(),
        seen: ids.clone(),
        cursor: Some("c-5".into()),
        swept_at: "2026-11-19T10:00:00.000Z".into(),
        ..CommentBatch::default()
    })
    .await
    .unwrap();
    // Everything is "old"; keep the newest 2 plus cursor and watermark.
    let purged = s
        .purge_seen("2999-01-01T00:00:00.000Z".into(), 2)
        .await
        .unwrap();
    let mut all = ids.clone();
    all.insert(0, "c-0".into());
    let left = s.seen_of("issue-1".into(), all).await.unwrap();
    assert!(left.contains(&"c-5".to_string()), "{left:?}");
    assert!(left.contains(&"c-0".to_string()), "{left:?}");
    assert_eq!(purged + left.len(), 6);
    // Nothing is older than the cutoff: nothing goes.
    assert_eq!(
        s.purge_seen("2000-01-01T00:00:00.000Z".into(), 0)
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn hot_window_only_grows() {
    let (_d, s) = open().await;
    s.insert_conversation(conversation("issue-1", None))
        .await
        .unwrap();
    s.extend_hot("issue-1".into(), "2026-11-19T12:00:00.000Z".into())
        .await
        .unwrap();
    s.extend_hot("issue-1".into(), "2026-11-19T11:00:00.000Z".into())
        .await
        .unwrap();
    s.set_updated_at(
        "issue-1".into(),
        Some("2026-11-19T10:30:00.000Z".into()),
        "2026-11-19T10:40:00.000Z".into(),
    )
    .await
    .unwrap();
    let c = s.conversation("issue-1".into()).await.unwrap().unwrap();
    assert_eq!(c.hot_until.as_deref(), Some("2026-11-19T12:00:00.000Z"));
    assert_eq!(
        c.last_updated_at.as_deref(),
        Some("2026-11-19T10:30:00.000Z")
    );
}

#[tokio::test]
async fn corrupt_database_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bridge.db");
    std::fs::write(
        &path,
        b"this is not a sqlite database at all, just garbage bytes",
    )
    .unwrap();
    assert!(Store::open(&path).is_err());
}

#[tokio::test]
async fn schema_is_versioned_and_pragmas_are_set() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bridge.db");
    drop(Store::open(&path).unwrap());
    let conn = rusqlite::Connection::open(&path).unwrap();
    let v: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, SCHEMA_VERSION);
    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    conn.execute_batch("PRAGMA user_version = 99").unwrap();
    drop(conn);
    assert!(matches!(Store::open(&path), Err(StoreError::TooNew(99))));
}
