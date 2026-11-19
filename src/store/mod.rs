//! Persistent state in SQLite (ADR-0003, ADR-0010 §2.4). One connection lives
//! in a dedicated thread (actor); async code sends it closures over a channel.
//! Every public method is one transaction. `core` sees only these methods
//! and the model types, never rusqlite.

mod sqlite;

use std::path::Path;
use std::sync::mpsc;

use tokio::sync::oneshot;

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
}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        // rusqlite errors carry SQL and codes, never user data from parameters.
        StoreError::Sqlite(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// A company seen by one of the keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompanyRow {
    pub company_id: String,
    pub name: String,
    pub issue_prefix: String,
    pub key_ref: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopicState {
    /// Not created yet.
    Pending,
    Active,
    Closed,
    /// Deleted in Telegram: to be created again.
    Recreating,
}

impl TopicState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Active => "active",
            Self::Closed => "closed",
            Self::Recreating => "recreating",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "active" => Self::Active,
            "closed" => Self::Closed,
            "recreating" => Self::Recreating,
            _ => Self::Pending,
        }
    }
}

/// A conversation and its topic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conversation {
    pub issue_id: String,
    pub company_id: String,
    pub identifier: Option<String>,
    pub agent_id: String,
    pub agent_name: String,
    pub key_ref: String,
    pub discovered_at: String,
    pub watermark_comment_id: Option<String>,
    pub cursor_comment_id: Option<String>,
    /// `updatedAt` of the conversation at the last poll.
    pub last_updated_at: Option<String>,
    pub hot_until: Option<String>,
    pub last_swept_at: Option<String>,
    pub topic_thread_id: Option<i64>,
    pub topic_name: String,
    pub topic_state: TopicState,
    pub removed_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewConversation {
    pub issue_id: String,
    pub company_id: String,
    pub identifier: Option<String>,
    pub agent_id: String,
    pub agent_name: String,
    pub key_ref: String,
    /// Newest comment at discovery: nothing up to it is forwarded.
    pub watermark_comment_id: Option<String>,
    pub updated_at: Option<String>,
    pub topic_name: String,
}

/// One piece of an agent comment for Telegram.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewChunk {
    pub comment_id: String,
    pub chunk: u32,
    pub chunks: u32,
    pub html: String,
    pub plain: String,
}

/// Comments read in one pass over a conversation, committed together with
/// the cursor (ADR-0010 §2.6).
#[derive(Debug, Clone, Default)]
pub struct CommentBatch {
    pub issue_id: String,
    /// Every new comment id, forwarded or not.
    pub seen: Vec<String>,
    pub chunks: Vec<NewChunk>,
    /// New cursor; `None` keeps the current one.
    pub cursor: Option<String>,
    pub swept_at: String,
    /// Extends the hot window when set.
    pub hot_until: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TgChunk {
    pub id: i64,
    pub issue_id: String,
    pub comment_id: String,
    pub chunk: u32,
    pub chunks: u32,
    pub html: Option<String>,
    pub plain: Option<String>,
    /// Send without `parse_mode` (after `can't parse entities`).
    pub use_plain: bool,
    pub attempts: u32,
    /// Topic of the conversation at the time of reading.
    pub thread_id: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PcStatus {
    Pending,
    InFlight,
    Done,
    Rejected,
}

impl PcStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InFlight => "in_flight",
            Self::Done => "done",
            Self::Rejected => "rejected",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "pending" => Self::Pending,
            "in_flight" => Self::InFlight,
            "done" => Self::Done,
            _ => Self::Rejected,
        }
    }
}

/// A user message from Telegram on its way to Paperclip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PcEntry {
    pub id: i64,
    pub chat_id: i64,
    pub message_id: i64,
    pub update_id: i64,
    pub issue_id: String,
    pub client_request_id: String,
    /// `None` once finished (wiped).
    pub body: Option<String>,
    pub status: PcStatus,
    pub attempts: u32,
    pub last_error: Option<String>,
    pub comment_id: Option<String>,
    /// "In the queue" was already told to the user.
    pub notified: bool,
}

#[derive(Debug, Clone)]
pub struct NewPcEntry {
    pub chat_id: i64,
    pub message_id: i64,
    pub issue_id: String,
    pub client_request_id: String,
    pub body: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Recovery {
    /// `pc_outbox` entries moved from `in_flight` back to `pending`.
    pub pc_reset: usize,
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
    pub const STARTED_AT: &str = "started_at";
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

    /// Skips an update without effects (foreign, unsupported, hint only).
    pub async fn advance_offset(&self, update_id: i64) -> Result<CommitOutcome> {
        self.call(move |c| sqlite::commit_update(c, update_id, None))
            .await
    }

    /// Queues a user message and sets `telegram_offset = update_id + 1`
    /// atomically (ADR-0010 §2.5). A message already queued is not queued again.
    pub async fn enqueue_message(&self, update_id: i64, e: NewPcEntry) -> Result<CommitOutcome> {
        self.call(move |c| sqlite::commit_update(c, update_id, Some(e)))
            .await
    }

    pub async fn upsert_companies(&self, rows: Vec<CompanyRow>) -> Result<()> {
        self.call(move |c| sqlite::upsert_companies(c, &rows)).await
    }

    pub async fn conversations(&self) -> Result<Vec<Conversation>> {
        self.call(sqlite::conversations).await
    }

    pub async fn conversation(&self, issue_id: String) -> Result<Option<Conversation>> {
        self.call(move |c| sqlite::conversation(c, &issue_id)).await
    }

    pub async fn conversation_by_thread(&self, thread_id: i64) -> Result<Option<Conversation>> {
        self.call(move |c| sqlite::conversation_by_thread(c, thread_id))
            .await
    }

    /// Inserts a new conversation (topic `pending`) with its watermark marked
    /// as seen. Returns `false` if it was already known.
    pub async fn insert_conversation(&self, n: NewConversation) -> Result<bool> {
        self.call(move |c| sqlite::insert_conversation(c, &n)).await
    }

    /// Marks a conversation as gone from `/chats` (`Some`) or back (`None`).
    pub async fn set_removed(&self, issue_id: String, at: Option<String>) -> Result<()> {
        self.call(move |c| sqlite::set_removed(c, &issue_id, at.as_deref()))
            .await
    }

    /// New `updatedAt` seen; opens the hot window.
    pub async fn set_updated_at(
        &self,
        issue_id: String,
        updated_at: Option<String>,
        hot_until: String,
    ) -> Result<()> {
        self.call(move |c| sqlite::set_updated_at(c, &issue_id, updated_at.as_deref(), &hot_until))
            .await
    }

    /// Extends the hot window (never shortens it).
    pub async fn extend_hot(&self, issue_id: String, hot_until: String) -> Result<()> {
        self.call(move |c| sqlite::extend_hot(c, &issue_id, &hot_until))
            .await
    }

    /// Of `ids`, those already in `seen_comments` of the conversation.
    pub async fn seen_of(&self, issue_id: String, ids: Vec<String>) -> Result<Vec<String>> {
        self.call(move |c| sqlite::seen_of(c, &issue_id, &ids))
            .await
    }

    /// Seen ids, Telegram pieces and the cursor in one transaction.
    pub async fn commit_comments(&self, b: CommentBatch) -> Result<()> {
        self.call(move |c| sqlite::commit_comments(c, &b)).await
    }

    /// Drops `seen_comments` older than `before` beyond the newest `keep` of
    /// each conversation; the cursor anchor is always kept.
    pub async fn purge_seen(&self, before: String, keep: u32) -> Result<usize> {
        self.call(move |c| sqlite::purge_seen(c, &before, keep))
            .await
    }

    /// Oldest conversation whose topic is to be created.
    pub async fn next_topic_job(&self) -> Result<Option<Conversation>> {
        self.call(sqlite::next_topic_job).await
    }

    /// Topic created: `active` with the new thread id.
    pub async fn set_topic(&self, issue_id: String, thread_id: i64) -> Result<()> {
        self.call(move |c| sqlite::set_topic(c, &issue_id, thread_id))
            .await
    }

    /// The topic `thread_id` was deleted: recreate it (no-op if the
    /// conversation already moved to another topic).
    pub async fn topic_lost(&self, issue_id: String, thread_id: i64) -> Result<()> {
        self.call(move |c| sqlite::topic_lost(c, &issue_id, thread_id))
            .await
    }

    /// Next piece to send: oldest due, its topic active, nothing older pending
    /// in the same conversation.
    pub async fn next_chunk(&self, now: String) -> Result<Option<TgChunk>> {
        self.call(move |c| sqlite::next_chunk(c, &now)).await
    }

    /// Earliest `next_attempt_at` of pending pieces.
    pub async fn next_chunk_due(&self) -> Result<Option<String>> {
        self.call(sqlite::next_chunk_due).await
    }

    pub async fn chunk_sent(&self, id: i64, message_id: i64) -> Result<()> {
        self.call(move |c| sqlite::chunk_sent(c, id, message_id))
            .await
    }

    pub async fn chunk_use_plain(&self, id: i64) -> Result<()> {
        self.call(move |c| sqlite::chunk_use_plain(c, id)).await
    }

    pub async fn chunk_retry(&self, id: i64, attempts: u32, next_at: String) -> Result<()> {
        self.call(move |c| sqlite::chunk_retry(c, id, attempts, &next_at))
            .await
    }

    pub async fn chunk_abandon(&self, id: i64) -> Result<()> {
        self.call(move |c| sqlite::chunk_abandon(c, id)).await
    }

    /// Pieces waiting for Telegram.
    pub async fn queue_depth(&self) -> Result<u64> {
        self.call(sqlite::queue_depth).await
    }

    /// Due `pending` user messages, oldest first.
    pub async fn due_messages(&self, now: String) -> Result<Vec<PcEntry>> {
        self.call(move |c| sqlite::due_messages(c, &now)).await
    }

    pub async fn next_message_due(&self) -> Result<Option<String>> {
        self.call(sqlite::next_message_due).await
    }

    pub async fn message_in_flight(&self, id: i64) -> Result<()> {
        self.call(move |c| sqlite::message_in_flight(c, id)).await
    }

    /// `done`: body wiped, comment id kept.
    pub async fn message_done(&self, id: i64, comment_id: Option<String>) -> Result<()> {
        self.call(move |c| {
            sqlite::message_finish(c, id, PcStatus::Done, comment_id.as_deref(), None)
        })
        .await
    }

    /// `rejected`: body wiped, error class kept.
    pub async fn message_rejected(&self, id: i64, error: String) -> Result<()> {
        self.call(move |c| sqlite::message_finish(c, id, PcStatus::Rejected, None, Some(&error)))
            .await
    }

    pub async fn message_retry(
        &self,
        id: i64,
        attempts: u32,
        next_at: String,
        last_error: String,
        notified: bool,
    ) -> Result<()> {
        self.call(move |c| sqlite::message_retry(c, id, attempts, &next_at, &last_error, notified))
            .await
    }

    pub async fn message(&self, id: i64) -> Result<Option<PcEntry>> {
        self.call(move |c| sqlite::message(c, id)).await
    }

    pub async fn messages(&self) -> Result<Vec<PcEntry>> {
        self.call(sqlite::messages).await
    }

    /// All pieces, for tests and diagnostics.
    pub async fn chunks(&self) -> Result<Vec<(TgChunk, String)>> {
        self.call(sqlite::chunks).await
    }
}
