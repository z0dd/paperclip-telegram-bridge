//! Persistent state in SQLite (architecture §3–4). One connection lives in a
//! dedicated thread (actor); async code sends it closures over a channel.
//! Every public method is one transaction. `core` sees only these methods
//! and the model types, never rusqlite.

mod sqlite;

use std::path::Path;
use std::sync::mpsc;

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use crate::domain::{Draft, Interaction, Resolution};

pub use sqlite::SCHEMA_VERSION;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("state database error: {0}")]
    Sqlite(String),
    #[error("state database failed the integrity check")]
    Corrupt,
    #[error("state database schema version {0} is newer than supported")]
    TooNew(i64),
    #[error("state store is closed")]
    Closed,
    #[error("state data could not be decoded")]
    Decode,
}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        // rusqlite errors carry SQL and codes, never user data from parameters.
        StoreError::Sqlite(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, StoreError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryStatus {
    New,
    Sending,
    Sent,
    Answering,
    Answered,
    /// Governed card accepted; following `toolAction.status`.
    Following,
    Closed,
    Unsupported,
}

impl DeliveryStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Sending => "sending",
            Self::Sent => "sent",
            Self::Answering => "answering",
            Self::Answered => "answered",
            Self::Following => "following",
            Self::Closed => "closed",
            Self::Unsupported => "unsupported",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "new" => Self::New,
            "sending" => Self::Sending,
            "sent" => Self::Sent,
            "answering" => Self::Answering,
            "answered" => Self::Answered,
            "following" => Self::Following,
            "closed" => Self::Closed,
            "unsupported" => Self::Unsupported,
            _ => return None,
        })
    }

    /// The card may still be resolved in Paperclip and is shown as open.
    pub fn is_open(self) -> bool {
        matches!(self, Self::Sent | Self::Answering | Self::Unsupported)
    }
}

/// What was shown to the user: the normalised card plus header data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub interaction: Interaction,
    pub issue_title: Option<String>,
    pub agent_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Delivery {
    pub id: i64,
    pub interaction_id: String,
    pub company_id: String,
    pub issue_id: String,
    pub issue_ref: String,
    pub kind: String,
    pub snapshot: Snapshot,
    pub status: DeliveryStatus,
    pub generation: i64,
    pub closed_reason: Option<String>,
    pub follow_until: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    /// `(chunk, message_id)` ordered by chunk.
    pub messages: Vec<(i64, i64)>,
}

impl Delivery {
    /// The message that carries the keyboard and gets edited.
    pub fn last_message_id(&self) -> Option<i64> {
        self.messages.last().map(|(_, m)| *m)
    }
}

#[derive(Debug, Clone)]
pub struct NewDelivery {
    pub interaction_id: String,
    pub company_id: String,
    pub issue_id: String,
    pub issue_ref: String,
    pub snapshot: Snapshot,
    pub status: DeliveryStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboxStatus {
    Pending,
    InFlight,
    Done,
    Abandoned,
}

impl OutboxStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InFlight => "in_flight",
            Self::Done => "done",
            Self::Abandoned => "abandoned",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "pending" => Self::Pending,
            "in_flight" => Self::InFlight,
            "done" => Self::Done,
            "abandoned" => Self::Abandoned,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct OutboxEntry {
    pub id: i64,
    pub delivery_id: i64,
    /// `None` after the entry is done (body is wiped).
    pub resolution: Option<Resolution>,
    pub status: OutboxStatus,
    pub attempts: u32,
    pub next_attempt_at: String,
    pub last_error: Option<String>,
    pub created_at: String,
}

/// State changes of one Telegram update, committed together with the offset.
#[derive(Debug, Clone)]
pub enum StoreOp {
    PutDraft(Draft),
    DeleteDraft(i64),
    /// Drops text-waiting of every other draft (one waiting at a time).
    ClearOtherTextAwaiting {
        keep: i64,
    },
    /// Queues the answer and moves the delivery to `answering`. Ignored if an
    /// entry already exists (at most one answer per card).
    Enqueue {
        delivery_id: i64,
        resolution: Resolution,
    },
    SetStatus {
        delivery_id: i64,
        status: DeliveryStatus,
        closed_reason: Option<String>,
    },
    BumpGeneration(i64),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Recovery {
    /// Outbox entries moved from `in_flight` back to `pending`.
    pub outbox_reset: usize,
    /// Deliveries left in `sending`: to be delivered again.
    pub sending: Vec<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitOutcome {
    Applied,
    /// `update_id < telegram_offset`: already processed.
    Duplicate,
}

type Job = Box<dyn FnOnce(&mut rusqlite::Connection) + Send>;

#[derive(Clone)]
pub struct Store {
    tx: mpsc::Sender<Job>,
}

pub mod meta {
    pub const TELEGRAM_OFFSET: &str = "telegram_offset";
    pub const RUNS_BASELINE_AT: &str = "runs_baseline_at";
    pub const DONE_BASELINE_AT: &str = "done_baseline_at";
    pub const STARTED_AT: &str = "started_at";
    pub const SELF_USER_ID: &str = "self_user_id";
}

impl Store {
    /// Opens (creating if needed), runs `integrity_check` and migrations.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = sqlite::open(path)?;
        let (tx, rx) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("state-store".into())
            .spawn(move || {
                let mut conn = conn;
                while let Ok(job) = rx.recv() {
                    job(&mut conn);
                }
                let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
            })
            .map_err(|e| StoreError::Sqlite(e.to_string()))?;
        Ok(Self { tx })
    }

    async fn call<R, F>(&self, f: F) -> Result<R>
    where
        R: Send + 'static,
        F: FnOnce(&mut rusqlite::Connection) -> Result<R> + Send + 'static,
    {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Box::new(move |c| {
                let _ = tx.send(f(c));
            }))
            .map_err(|_| StoreError::Closed)?;
        rx.await.map_err(|_| StoreError::Closed)?
    }

    /// Checkpoints the WAL; the connection closes when the last handle drops.
    pub async fn checkpoint(&self) -> Result<()> {
        self.call(|c| {
            c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
            Ok(())
        })
        .await
    }

    pub async fn recover(&self) -> Result<Recovery> {
        self.call(sqlite::recover).await
    }

    pub async fn get_meta(&self, key: &'static str) -> Result<Option<String>> {
        self.call(move |c| sqlite::get_meta(c, key)).await
    }

    pub async fn set_meta(&self, key: &'static str, value: String) -> Result<()> {
        self.call(move |c| sqlite::set_meta(c, key, &value)).await
    }

    pub async fn telegram_offset(&self) -> Result<i64> {
        Ok(self
            .get_meta(meta::TELEGRAM_OFFSET)
            .await?
            .and_then(|v| v.parse().ok())
            .unwrap_or(0))
    }

    /// Skips an update without effects (foreign, unsupported, stale).
    pub async fn advance_offset(&self, update_id: i64) -> Result<CommitOutcome> {
        self.commit_update(update_id, Vec::new()).await
    }

    /// Applies `ops` and sets `telegram_offset = update_id + 1` atomically.
    pub async fn commit_update(&self, update_id: i64, ops: Vec<StoreOp>) -> Result<CommitOutcome> {
        self.call(move |c| sqlite::commit_update(c, Some(update_id), ops))
            .await
    }

    /// Applies `ops` in one transaction without touching the offset.
    pub async fn apply(&self, ops: Vec<StoreOp>) -> Result<()> {
        self.call(move |c| sqlite::commit_update(c, None, ops).map(|_| ()))
            .await
    }

    /// Inserts a delivery unless the interaction is already known.
    pub async fn insert_delivery(&self, d: NewDelivery) -> Result<Option<i64>> {
        self.call(move |c| sqlite::insert_delivery(c, &d)).await
    }

    pub async fn delivery(&self, id: i64) -> Result<Option<Delivery>> {
        self.call(move |c| sqlite::delivery(c, id)).await
    }

    pub async fn delivery_by_interaction(
        &self,
        interaction_id: String,
    ) -> Result<Option<Delivery>> {
        self.call(move |c| sqlite::delivery_by_interaction(c, &interaction_id))
            .await
    }

    pub async fn delivery_by_message(&self, message_id: i64) -> Result<Option<Delivery>> {
        self.call(move |c| sqlite::delivery_by_message(c, message_id))
            .await
    }

    pub async fn deliveries_with_status(
        &self,
        statuses: Vec<DeliveryStatus>,
    ) -> Result<Vec<Delivery>> {
        self.call(move |c| sqlite::deliveries_with_status(c, &statuses))
            .await
    }

    pub async fn known_interactions(&self, ids: Vec<String>) -> Result<Vec<String>> {
        self.call(move |c| sqlite::known_interactions(c, &ids))
            .await
    }

    pub async fn set_status(
        &self,
        delivery_id: i64,
        status: DeliveryStatus,
        closed_reason: Option<String>,
    ) -> Result<()> {
        self.apply(vec![StoreOp::SetStatus {
            delivery_id,
            status,
            closed_reason,
        }])
        .await
    }

    /// `sending` → `sent` with message ids (one transaction).
    pub async fn mark_sent(
        &self,
        delivery_id: i64,
        status: DeliveryStatus,
        messages: Vec<i64>,
    ) -> Result<()> {
        self.call(move |c| sqlite::mark_sent(c, delivery_id, status, &messages))
            .await
    }

    /// Replaces the id of the last chunk (message re-sent after "not found").
    pub async fn replace_last_message(&self, delivery_id: i64, message_id: i64) -> Result<()> {
        self.call(move |c| sqlite::replace_last_message(c, delivery_id, message_id))
            .await
    }

    /// Card resolved: closes the delivery, deletes its draft.
    pub async fn close_delivery(
        &self,
        delivery_id: i64,
        status: DeliveryStatus,
        reason: String,
    ) -> Result<()> {
        self.call(move |c| sqlite::close_delivery(c, delivery_id, status, &reason))
            .await
    }

    pub async fn update_snapshot(&self, delivery_id: i64, snapshot: Snapshot) -> Result<()> {
        self.call(move |c| sqlite::update_snapshot(c, delivery_id, &snapshot))
            .await
    }

    pub async fn set_following(&self, delivery_id: i64, until: String) -> Result<()> {
        self.call(move |c| sqlite::set_following(c, delivery_id, &until))
            .await
    }

    pub async fn draft(&self, delivery_id: i64) -> Result<Option<Draft>> {
        self.call(move |c| sqlite::draft(c, delivery_id)).await
    }

    pub async fn drafts(&self) -> Result<Vec<Draft>> {
        self.call(sqlite::drafts).await
    }

    /// The draft currently waiting for free text, if any.
    pub async fn text_awaiting_draft(&self) -> Result<Option<Draft>> {
        self.call(sqlite::text_awaiting_draft).await
    }

    pub async fn draft_by_prompt_message(&self, message_id: i64) -> Result<Option<Draft>> {
        self.call(move |c| sqlite::draft_by_prompt_message(c, message_id))
            .await
    }

    pub async fn set_prompt_message(&self, delivery_id: i64, message_id: i64) -> Result<()> {
        self.call(move |c| sqlite::set_prompt_message(c, delivery_id, message_id))
            .await
    }

    pub async fn outbox_for(&self, delivery_id: i64) -> Result<Option<OutboxEntry>> {
        self.call(move |c| sqlite::outbox_for(c, delivery_id)).await
    }

    /// Due `pending` entries, oldest first.
    pub async fn due_outbox(&self, now: String) -> Result<Vec<OutboxEntry>> {
        self.call(move |c| sqlite::due_outbox(c, &now)).await
    }

    pub async fn next_outbox_due(&self) -> Result<Option<String>> {
        self.call(sqlite::next_outbox_due).await
    }

    pub async fn pending_outbox_count(&self) -> Result<u64> {
        self.call(sqlite::pending_outbox_count).await
    }

    pub async fn outbox_in_flight(&self, id: i64) -> Result<()> {
        self.call(move |c| sqlite::outbox_set_status(c, id, OutboxStatus::InFlight))
            .await
    }

    /// Outbox done (body wiped) and delivery status in one transaction.
    pub async fn outbox_done(
        &self,
        id: i64,
        delivery_id: i64,
        status: DeliveryStatus,
        reason: Option<String>,
    ) -> Result<()> {
        self.call(move |c| {
            sqlite::outbox_finish(
                c,
                id,
                delivery_id,
                OutboxStatus::Done,
                status,
                reason.as_deref(),
            )
        })
        .await
    }

    /// Outbox abandoned and delivery status in one transaction.
    pub async fn outbox_abandon(
        &self,
        id: i64,
        delivery_id: i64,
        status: DeliveryStatus,
        reason: Option<String>,
    ) -> Result<()> {
        self.call(move |c| {
            sqlite::outbox_finish(
                c,
                id,
                delivery_id,
                OutboxStatus::Abandoned,
                status,
                reason.as_deref(),
            )
        })
        .await
    }

    /// Removes a finished entry so the card can be answered again (partial
    /// verdicts, validation errors).
    pub async fn outbox_remove(
        &self,
        id: i64,
        delivery_id: i64,
        status: DeliveryStatus,
    ) -> Result<()> {
        self.call(move |c| sqlite::outbox_remove(c, id, delivery_id, status))
            .await
    }

    pub async fn outbox_retry(
        &self,
        id: i64,
        attempts: u32,
        next_at: String,
        last_error: String,
    ) -> Result<()> {
        self.call(move |c| sqlite::outbox_retry(c, id, attempts, &next_at, &last_error))
            .await
    }

    pub async fn is_run_seen(&self, run_id: String) -> Result<bool> {
        self.call(move |c| sqlite::is_seen(c, "seen_runs", "run_id", &run_id))
            .await
    }

    pub async fn mark_runs_seen(&self, ids: Vec<String>) -> Result<()> {
        self.call(move |c| sqlite::mark_seen(c, "seen_runs", "run_id", &ids))
            .await
    }

    pub async fn is_done_issue_seen(&self, issue_id: String) -> Result<bool> {
        self.call(move |c| sqlite::is_seen(c, "seen_done_issues", "issue_id", &issue_id))
            .await
    }

    pub async fn mark_done_issues_seen(&self, ids: Vec<String>) -> Result<()> {
        self.call(move |c| sqlite::mark_seen(c, "seen_done_issues", "issue_id", &ids))
            .await
    }

    /// Drops `seen_runs` rows older than `before`.
    pub async fn purge_seen(&self, before: String) -> Result<usize> {
        self.call(move |c| sqlite::purge_seen(c, &before)).await
    }
}
