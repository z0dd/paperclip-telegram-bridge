//! State store: durability across restarts, exactly-once update effects,
//! recovery of interrupted work (architecture §3.1, §4.2, §4.3).

use bridge::domain::*;
use bridge::store::*;

fn interaction(id: &str) -> Interaction {
    Interaction {
        id: id.into(),
        company_id: "company-1".into(),
        issue_id: "issue-1".into(),
        kind: InteractionKind::RequestConfirmation,
        status: InteractionStatus::Pending,
        title: Some("t".into()),
        created_by_agent_id: Some("agent-1".into()),
        created_at: "2026-11-15T10:00:00.000Z".into(),
        resolved_by_user_id: None,
        resolved_by_agent_id: None,
        addressee_agent_id: None,
        addressee_user_id: None,
        body: InteractionBody::Confirmation(Confirmation {
            prompt: "Ok?".into(),
            details: None,
            accept_label: None,
            reject_label: None,
            reject_requires_reason: false,
            allow_decline_reason: false,
        }),
        result: None,
    }
}

fn new_delivery(id: &str) -> NewDelivery {
    NewDelivery {
        interaction_id: id.into(),
        company_id: "company-1".into(),
        issue_id: "issue-1".into(),
        issue_ref: "PET-1".into(),
        snapshot: Snapshot {
            interaction: interaction(id),
            issue_title: Some("Issue".into()),
            agent_name: Some("Architect".into()),
        },
        status: DeliveryStatus::New,
    }
}

fn accept() -> Resolution {
    Resolution::Accept {
        selected_option_ids: None,
    }
}

#[tokio::test]
async fn state_survives_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bridge.db");
    let did = {
        let store = Store::open(&path).unwrap();
        let did = store
            .insert_delivery(new_delivery("card-1"))
            .await
            .unwrap()
            .unwrap();
        store
            .mark_sent(did, DeliveryStatus::Sent, vec![10, 11])
            .await
            .unwrap();
        let mut draft = Draft::new(did);
        draft.step = 2;
        draft.awaiting = Some(Awaiting::Text { question: 2 });
        draft.answers.form = vec![
            Some(FormAnswer {
                option_ids: vec!["a".into()],
                other_text: None,
            }),
            None,
        ];
        store
            .commit_update(7, vec![StoreOp::PutDraft(draft)])
            .await
            .unwrap();
        store.checkpoint().await.unwrap();
        did
    };
    let store = Store::open(&path).unwrap();
    assert_eq!(store.telegram_offset().await.unwrap(), 8);
    let d = store.delivery(did).await.unwrap().unwrap();
    assert_eq!(d.status, DeliveryStatus::Sent);
    assert_eq!(d.messages, vec![(0, 10), (1, 11)]);
    assert_eq!(d.last_message_id(), Some(11));
    assert_eq!(d.snapshot.agent_name.as_deref(), Some("Architect"));
    assert_eq!(d.snapshot.interaction, interaction("card-1"));
    let draft = store.draft(did).await.unwrap().unwrap();
    assert_eq!(draft.step, 2);
    assert_eq!(draft.awaiting, Some(Awaiting::Text { question: 2 }));
    assert_eq!(draft.answers.form.len(), 2);
    assert_eq!(
        store.delivery_by_message(11).await.unwrap().unwrap().id,
        did
    );
    assert_eq!(
        store
            .text_awaiting_draft()
            .await
            .unwrap()
            .unwrap()
            .delivery_id,
        did
    );
}

#[tokio::test]
async fn duplicate_update_has_one_effect() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("bridge.db")).unwrap();
    let did = store
        .insert_delivery(new_delivery("card-1"))
        .await
        .unwrap()
        .unwrap();
    let ops = || {
        vec![
            StoreOp::Enqueue {
                delivery_id: did,
                resolution: accept(),
            },
            StoreOp::BumpGeneration(did),
        ]
    };
    assert_eq!(
        store.commit_update(100, ops()).await.unwrap(),
        CommitOutcome::Applied
    );
    assert_eq!(
        store.commit_update(100, ops()).await.unwrap(),
        CommitOutcome::Duplicate
    );
    assert_eq!(
        store.commit_update(99, ops()).await.unwrap(),
        CommitOutcome::Duplicate
    );
    let d = store.delivery(did).await.unwrap().unwrap();
    assert_eq!(d.generation, 1, "effects applied once");
    assert_eq!(d.status, DeliveryStatus::Answering);
    assert_eq!(store.telegram_offset().await.unwrap(), 101);
    assert_eq!(store.pending_outbox_count().await.unwrap(), 1);
}

#[tokio::test]
async fn at_most_one_outbox_entry_per_card() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("bridge.db")).unwrap();
    let did = store
        .insert_delivery(new_delivery("card-1"))
        .await
        .unwrap()
        .unwrap();
    for uid in [1, 2] {
        store
            .commit_update(
                uid,
                vec![StoreOp::Enqueue {
                    delivery_id: did,
                    resolution: if uid == 1 {
                        accept()
                    } else {
                        Resolution::Reject { reason: None }
                    },
                }],
            )
            .await
            .unwrap();
    }
    let entry = store.outbox_for(did).await.unwrap().unwrap();
    assert_eq!(entry.resolution, Some(accept()), "first answer wins");
    assert_eq!(store.pending_outbox_count().await.unwrap(), 1);
}

#[tokio::test]
async fn duplicate_interaction_is_not_inserted_twice() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("bridge.db")).unwrap();
    assert!(
        store
            .insert_delivery(new_delivery("card-1"))
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        store
            .insert_delivery(new_delivery("card-1"))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store
            .known_interactions(vec!["card-1".into(), "card-2".into()])
            .await
            .unwrap(),
        vec!["card-1"]
    );
}

#[tokio::test]
async fn crash_in_flight_and_sending_are_recovered() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bridge.db");
    let (sending, answered) = {
        let store = Store::open(&path).unwrap();
        // Crash after the outbox entry went in flight, before the result commit.
        let answered = store
            .insert_delivery(new_delivery("card-1"))
            .await
            .unwrap()
            .unwrap();
        store
            .commit_update(
                1,
                vec![StoreOp::Enqueue {
                    delivery_id: answered,
                    resolution: accept(),
                }],
            )
            .await
            .unwrap();
        let e = store.outbox_for(answered).await.unwrap().unwrap();
        store.outbox_in_flight(e.id).await.unwrap();
        // Crash between "sending" and the commit of message ids.
        let sending = store
            .insert_delivery(new_delivery("card-2"))
            .await
            .unwrap()
            .unwrap();
        store
            .set_status(sending, DeliveryStatus::Sending, None)
            .await
            .unwrap();
        (sending, answered)
        // Store dropped without any cleanup = crash.
    };
    let store = Store::open(&path).unwrap();
    let rec = store.recover().await.unwrap();
    assert_eq!(rec.outbox_reset, 1);
    assert_eq!(rec.sending, vec![sending]);
    let due = store
        .due_outbox(bridge::timeutil::now_string())
        .await
        .unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].delivery_id, answered);
    assert_eq!(due[0].status, OutboxStatus::Pending);
    assert_eq!(due[0].resolution, Some(accept()), "answer is not lost");
}

#[tokio::test]
async fn finished_outbox_wipes_body_and_draft() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("bridge.db")).unwrap();
    let did = store
        .insert_delivery(new_delivery("card-1"))
        .await
        .unwrap()
        .unwrap();
    store
        .commit_update(
            1,
            vec![
                StoreOp::PutDraft(Draft::new(did)),
                StoreOp::Enqueue {
                    delivery_id: did,
                    resolution: Resolution::Reject {
                        reason: Some("private reason".into()),
                    },
                },
            ],
        )
        .await
        .unwrap();
    let e = store.outbox_for(did).await.unwrap().unwrap();
    store
        .outbox_done(e.id, did, DeliveryStatus::Answered, None)
        .await
        .unwrap();
    let e = store.outbox_for(did).await.unwrap().unwrap();
    assert_eq!(e.status, OutboxStatus::Done);
    assert_eq!(e.resolution, None);
    assert!(store.draft(did).await.unwrap().is_none());
    assert_eq!(
        store.delivery(did).await.unwrap().unwrap().status,
        DeliveryStatus::Answered
    );
}

#[tokio::test]
async fn retry_schedule_and_removal() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("bridge.db")).unwrap();
    let did = store
        .insert_delivery(new_delivery("card-1"))
        .await
        .unwrap()
        .unwrap();
    store
        .apply(vec![StoreOp::Enqueue {
            delivery_id: did,
            resolution: accept(),
        }])
        .await
        .unwrap();
    let e = store.outbox_for(did).await.unwrap().unwrap();
    store
        .outbox_retry(e.id, 3, "2999-01-01T00:00:00Z".into(), "timeout".into())
        .await
        .unwrap();
    assert!(
        store
            .due_outbox(bridge::timeutil::now_string())
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store.next_outbox_due().await.unwrap().as_deref(),
        Some("2999-01-01T00:00:00Z")
    );
    let e = store.outbox_for(did).await.unwrap().unwrap();
    assert_eq!((e.attempts, e.last_error.as_deref()), (3, Some("timeout")));
    store
        .outbox_remove(e.id, did, DeliveryStatus::Sent)
        .await
        .unwrap();
    assert!(store.outbox_for(did).await.unwrap().is_none());
    assert_eq!(
        store.delivery(did).await.unwrap().unwrap().status,
        DeliveryStatus::Sent
    );
}

#[tokio::test]
async fn only_one_text_waiting_draft() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("bridge.db")).unwrap();
    let a = store
        .insert_delivery(new_delivery("card-1"))
        .await
        .unwrap()
        .unwrap();
    let b = store
        .insert_delivery(new_delivery("card-2"))
        .await
        .unwrap()
        .unwrap();
    let mut da = Draft::new(a);
    da.awaiting = Some(Awaiting::RejectReason { optional: false });
    store.apply(vec![StoreOp::PutDraft(da)]).await.unwrap();
    let mut db = Draft::new(b);
    db.awaiting = Some(Awaiting::Text { question: 0 });
    store
        .apply(vec![
            StoreOp::PutDraft(db),
            StoreOp::ClearOtherTextAwaiting { keep: b },
        ])
        .await
        .unwrap();
    assert_eq!(store.draft(a).await.unwrap().unwrap().awaiting, None);
    assert_eq!(
        store
            .text_awaiting_draft()
            .await
            .unwrap()
            .unwrap()
            .delivery_id,
        b
    );
}

#[tokio::test]
async fn seen_runs_are_tracked_and_purged() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("bridge.db")).unwrap();
    store
        .mark_runs_seen(vec!["run-1".into(), "run-2".into()])
        .await
        .unwrap();
    store.mark_runs_seen(vec!["run-1".into()]).await.unwrap();
    assert!(store.is_run_seen("run-1".into()).await.unwrap());
    assert!(!store.is_run_seen("run-3".into()).await.unwrap());
    store
        .mark_done_issues_seen(vec!["issue-1".into()])
        .await
        .unwrap();
    assert_eq!(
        store
            .purge_seen("2999-01-01T00:00:00Z".into())
            .await
            .unwrap(),
        2
    );
    assert!(!store.is_run_seen("run-1".into()).await.unwrap());
    assert!(store.is_done_issue_seen("issue-1".into()).await.unwrap());
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
