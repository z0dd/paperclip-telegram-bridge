//! SQL for the state store. Migrations are tracked in `PRAGMA user_version`.

use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, Row, params};

use super::*;
use crate::timeutil;

pub const SCHEMA_VERSION: i64 = 2;

const MIGRATIONS: &[&str] = &[
    // 1: v1 schema (cards), unchanged: a v1 state file migrates step by step.
    r#"
    CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);

    CREATE TABLE deliveries (
      id              INTEGER PRIMARY KEY,
      interaction_id  TEXT NOT NULL UNIQUE,
      company_id      TEXT NOT NULL,
      issue_id        TEXT NOT NULL,
      issue_ref       TEXT NOT NULL,
      kind            TEXT NOT NULL,
      snapshot_json   TEXT NOT NULL,
      status          TEXT NOT NULL CHECK (status IN
                       ('new','sending','sent','answering','answered','following','closed','unsupported')),
      gen             INTEGER NOT NULL DEFAULT 0,
      closed_reason   TEXT,
      follow_until    TEXT,
      created_at      TEXT NOT NULL,
      updated_at      TEXT NOT NULL
    );
    CREATE INDEX deliveries_status ON deliveries(status);

    CREATE TABLE delivery_messages (
      delivery_id INTEGER NOT NULL REFERENCES deliveries(id),
      chunk       INTEGER NOT NULL,
      message_id  INTEGER NOT NULL,
      PRIMARY KEY (delivery_id, chunk)
    );
    CREATE INDEX delivery_messages_message ON delivery_messages(message_id);

    CREATE TABLE drafts (
      delivery_id       INTEGER PRIMARY KEY REFERENCES deliveries(id),
      step              INTEGER NOT NULL,
      answers_json      TEXT NOT NULL,
      awaiting          TEXT,
      prompt_message_id INTEGER,
      updated_at        TEXT NOT NULL
    );

    CREATE TABLE outbox (
      id              INTEGER PRIMARY KEY,
      delivery_id     INTEGER NOT NULL UNIQUE REFERENCES deliveries(id),
      action          TEXT NOT NULL,
      body_json       TEXT,
      status          TEXT NOT NULL CHECK (status IN ('pending','in_flight','done','abandoned')),
      attempts        INTEGER NOT NULL DEFAULT 0,
      next_attempt_at TEXT NOT NULL,
      last_error      TEXT,
      created_at      TEXT NOT NULL,
      updated_at      TEXT NOT NULL
    );
    CREATE INDEX outbox_due ON outbox(status, next_attempt_at);

    CREATE TABLE seen_runs (run_id TEXT PRIMARY KEY, seen_at TEXT NOT NULL);
    CREATE TABLE seen_done_issues (issue_id TEXT PRIMARY KEY, seen_at TEXT NOT NULL);
    "#,
    // 2: v2 (ADR-0010 §2.4). Card state is not needed: v1 tables are dropped,
    // `meta.telegram_offset` is kept.
    r#"
    DROP TABLE IF EXISTS drafts;
    DROP TABLE IF EXISTS outbox;
    DROP TABLE IF EXISTS delivery_messages;
    DROP TABLE IF EXISTS deliveries;
    DROP TABLE IF EXISTS seen_runs;
    DROP TABLE IF EXISTS seen_done_issues;
    DELETE FROM meta WHERE key IN ('runs_baseline_at', 'done_baseline_at', 'self_user_id');

    CREATE TABLE companies (
      company_id        TEXT PRIMARY KEY,
      name              TEXT NOT NULL,
      issue_prefix      TEXT NOT NULL,
      key_ref           TEXT NOT NULL,
      last_seen_at      TEXT NOT NULL,
      unavailable_since TEXT
    );

    CREATE TABLE conversations (
      issue_id             TEXT PRIMARY KEY,
      company_id           TEXT NOT NULL,
      identifier           TEXT,
      agent_id             TEXT NOT NULL,
      agent_name           TEXT NOT NULL,
      key_ref              TEXT NOT NULL,
      discovered_at        TEXT NOT NULL,
      watermark_comment_id TEXT,
      cursor_comment_id    TEXT,
      last_updated_at      TEXT,
      hot_until            TEXT,
      last_swept_at        TEXT,
      topic_thread_id      INTEGER UNIQUE,
      topic_name           TEXT NOT NULL,
      topic_state          TEXT NOT NULL CHECK (topic_state IN ('pending','active','closed','recreating')),
      removed_at           TEXT
    );

    CREATE TABLE seen_comments (
      issue_id   TEXT NOT NULL,
      comment_id TEXT NOT NULL,
      created_at TEXT NOT NULL,
      PRIMARY KEY (issue_id, comment_id)
    );

    CREATE TABLE tg_outbox (
      id                  INTEGER PRIMARY KEY,
      issue_id            TEXT NOT NULL,
      comment_id          TEXT NOT NULL,
      chunk               INTEGER NOT NULL,
      chunks              INTEGER NOT NULL,
      html                TEXT,
      plain               TEXT,
      use_plain           INTEGER NOT NULL DEFAULT 0,
      status              TEXT NOT NULL CHECK (status IN ('pending','sending','sent','abandoned')),
      attempts            INTEGER NOT NULL DEFAULT 0,
      next_attempt_at     TEXT NOT NULL,
      telegram_message_id INTEGER,
      created_at          TEXT NOT NULL,
      updated_at          TEXT NOT NULL,
      UNIQUE (comment_id, chunk)
    );
    CREATE INDEX tg_outbox_pending ON tg_outbox(status, issue_id, id);

    CREATE TABLE pc_outbox (
      id                INTEGER PRIMARY KEY,
      chat_id           INTEGER NOT NULL,
      message_id        INTEGER NOT NULL,
      update_id         INTEGER NOT NULL,
      issue_id          TEXT NOT NULL,
      client_request_id TEXT NOT NULL,
      body              TEXT,
      status            TEXT NOT NULL CHECK (status IN ('pending','in_flight','done','rejected')),
      attempts          INTEGER NOT NULL DEFAULT 0,
      next_attempt_at   TEXT NOT NULL,
      last_error        TEXT,
      comment_id        TEXT,
      notified          INTEGER NOT NULL DEFAULT 0,
      created_at        TEXT NOT NULL,
      updated_at        TEXT NOT NULL,
      UNIQUE (chat_id, message_id)
    );
    CREATE INDEX pc_outbox_due ON pc_outbox(status, next_attempt_at);
    "#,
];

pub fn open(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(Duration::from_millis(5000))?;
    // temp_store=MEMORY: the root filesystem is read-only, only /state is writable.
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = FULL;
         PRAGMA foreign_keys = ON;
         PRAGMA temp_store = MEMORY;",
    )?;
    let check: String = conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    if check != "ok" {
        return Err(StoreError::Corrupt);
    }
    migrate(&conn)?;
    Ok(conn)
}

fn migrate(conn: &Connection) -> Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version > SCHEMA_VERSION {
        return Err(StoreError::TooNew(version));
    }
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(version as usize) {
        let target = i as i64 + 1;
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(sql)?;
        tx.execute_batch(&format!("PRAGMA user_version = {target};"))?;
        tx.commit()?;
    }
    Ok(())
}

fn now() -> String {
    timeutil::now_string()
}

pub fn get_meta(c: &Connection, key: &str) -> Result<Option<String>> {
    Ok(
        c.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
            .optional()?,
    )
}

pub fn set_meta(c: &Connection, key: &str, value: &str) -> Result<()> {
    c.execute(
        "INSERT INTO meta(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

pub fn recover(c: &mut Connection) -> Result<Recovery> {
    let tx = c.transaction()?;
    let ts = now();
    let pc_reset = tx.execute(
        "UPDATE pc_outbox SET status = 'pending', updated_at = ?1 WHERE status = 'in_flight'",
        [&ts],
    )?;
    tx.execute(
        "UPDATE tg_outbox SET status = 'pending', updated_at = ?1 WHERE status = 'sending'",
        [&ts],
    )?;
    if get_meta(&tx, meta::STARTED_AT)?.is_none() {
        set_meta(&tx, meta::STARTED_AT, &ts)?;
    }
    tx.commit()?;
    Ok(Recovery { pc_reset })
}

pub fn commit_update(
    c: &mut Connection,
    update_id: i64,
    entry: Option<NewPcEntry>,
) -> Result<CommitOutcome> {
    let tx = c.transaction()?;
    let offset: i64 = get_meta(&tx, meta::TELEGRAM_OFFSET)?
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    if update_id < offset {
        return Ok(CommitOutcome::Duplicate);
    }
    if let Some(e) = entry {
        let ts = now();
        tx.execute(
            "INSERT OR IGNORE INTO pc_outbox(chat_id, message_id, update_id, issue_id,
                 client_request_id, body, status, attempts, next_attempt_at, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', 0, ?7, ?7, ?7)",
            params![
                e.chat_id,
                e.message_id,
                update_id,
                e.issue_id,
                e.client_request_id,
                e.body,
                ts
            ],
        )?;
    }
    set_meta(&tx, meta::TELEGRAM_OFFSET, &(update_id + 1).to_string())?;
    tx.commit()?;
    Ok(CommitOutcome::Applied)
}

pub fn upsert_companies(c: &mut Connection, rows: &[CompanyRow]) -> Result<()> {
    let tx = c.transaction()?;
    let ts = now();
    for r in rows {
        tx.execute(
            "INSERT INTO companies(company_id, name, issue_prefix, key_ref, last_seen_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(company_id) DO UPDATE SET name = excluded.name,
                issue_prefix = excluded.issue_prefix, key_ref = excluded.key_ref,
                last_seen_at = excluded.last_seen_at",
            params![r.company_id, r.name, r.issue_prefix, r.key_ref, ts],
        )?;
    }
    tx.commit()?;
    Ok(())
}

const CONVERSATION_COLS: &str = "issue_id, company_id, identifier, agent_id, agent_name, key_ref,
    discovered_at, watermark_comment_id, cursor_comment_id, last_updated_at, hot_until,
    last_swept_at, topic_thread_id, topic_name, topic_state, removed_at";

fn conversation_row(r: &Row<'_>) -> rusqlite::Result<Conversation> {
    let state: String = r.get(14)?;
    Ok(Conversation {
        issue_id: r.get(0)?,
        company_id: r.get(1)?,
        identifier: r.get(2)?,
        agent_id: r.get(3)?,
        agent_name: r.get(4)?,
        key_ref: r.get(5)?,
        discovered_at: r.get(6)?,
        watermark_comment_id: r.get(7)?,
        cursor_comment_id: r.get(8)?,
        last_updated_at: r.get(9)?,
        hot_until: r.get(10)?,
        last_swept_at: r.get(11)?,
        topic_thread_id: r.get(12)?,
        topic_name: r.get(13)?,
        topic_state: TopicState::parse(&state),
        removed_at: r.get(15)?,
    })
}

fn query_conversations(
    c: &Connection,
    where_sql: &str,
    p: impl rusqlite::Params,
) -> Result<Vec<Conversation>> {
    let mut stmt = c.prepare(&format!(
        "SELECT {CONVERSATION_COLS} FROM conversations WHERE {where_sql}"
    ))?;
    Ok(stmt
        .query_map(p, conversation_row)?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn conversations(c: &mut Connection) -> Result<Vec<Conversation>> {
    query_conversations(c, "1 = 1 ORDER BY discovered_at, issue_id", [])
}

pub fn conversation(c: &mut Connection, issue_id: &str) -> Result<Option<Conversation>> {
    Ok(query_conversations(c, "issue_id = ?1", [issue_id])?.pop())
}

pub fn conversation_by_thread(c: &mut Connection, thread_id: i64) -> Result<Option<Conversation>> {
    Ok(query_conversations(c, "topic_thread_id = ?1", [thread_id])?.pop())
}

pub fn insert_conversation(c: &mut Connection, n: &NewConversation) -> Result<bool> {
    let tx = c.transaction()?;
    let ts = now();
    let inserted = tx.execute(
        "INSERT OR IGNORE INTO conversations(issue_id, company_id, identifier, agent_id,
             agent_name, key_ref, discovered_at, watermark_comment_id, cursor_comment_id,
             last_updated_at, last_swept_at, topic_name, topic_state)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8, ?9, ?7, ?10, 'pending')",
        params![
            n.issue_id,
            n.company_id,
            n.identifier,
            n.agent_id,
            n.agent_name,
            n.key_ref,
            ts,
            n.watermark_comment_id,
            n.updated_at,
            n.topic_name
        ],
    )? > 0;
    if inserted && let Some(w) = &n.watermark_comment_id {
        tx.execute(
            "INSERT OR IGNORE INTO seen_comments(issue_id, comment_id, created_at) VALUES (?1, ?2, ?3)",
            params![n.issue_id, w, ts],
        )?;
    }
    tx.commit()?;
    Ok(inserted)
}

pub fn set_removed(c: &mut Connection, issue_id: &str, at: Option<&str>) -> Result<()> {
    c.execute(
        "UPDATE conversations SET removed_at = ?2 WHERE issue_id = ?1",
        params![issue_id, at],
    )?;
    Ok(())
}

pub fn set_updated_at(
    c: &mut Connection,
    issue_id: &str,
    updated_at: Option<&str>,
    hot_until: &str,
) -> Result<()> {
    c.execute(
        "UPDATE conversations SET last_updated_at = ?2,
            hot_until = MAX(COALESCE(hot_until, ''), ?3) WHERE issue_id = ?1",
        params![issue_id, updated_at, hot_until],
    )?;
    Ok(())
}

pub fn extend_hot(c: &mut Connection, issue_id: &str, hot_until: &str) -> Result<()> {
    c.execute(
        "UPDATE conversations SET hot_until = MAX(COALESCE(hot_until, ''), ?2) WHERE issue_id = ?1",
        params![issue_id, hot_until],
    )?;
    Ok(())
}

pub fn seen_of(c: &mut Connection, issue_id: &str, ids: &[String]) -> Result<Vec<String>> {
    let mut stmt =
        c.prepare_cached("SELECT 1 FROM seen_comments WHERE issue_id = ?1 AND comment_id = ?2")?;
    let mut out = Vec::new();
    for id in ids {
        if stmt.exists(params![issue_id, id])? {
            out.push(id.clone());
        }
    }
    Ok(out)
}

pub fn commit_comments(c: &mut Connection, b: &CommentBatch) -> Result<()> {
    let tx = c.transaction()?;
    let ts = now();
    {
        let mut seen = tx.prepare_cached(
            "INSERT OR IGNORE INTO seen_comments(issue_id, comment_id, created_at) VALUES (?1, ?2, ?3)",
        )?;
        for id in &b.seen {
            seen.execute(params![b.issue_id, id, ts])?;
        }
        let mut chunk = tx.prepare_cached(
            "INSERT OR IGNORE INTO tg_outbox(issue_id, comment_id, chunk, chunks, html, plain,
                 status, attempts, next_attempt_at, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', 0, ?7, ?7, ?7)",
        )?;
        for ch in &b.chunks {
            chunk.execute(params![
                b.issue_id,
                ch.comment_id,
                ch.chunk,
                ch.chunks,
                ch.html,
                ch.plain,
                ts
            ])?;
        }
    }
    tx.execute(
        "UPDATE conversations SET cursor_comment_id = COALESCE(?2, cursor_comment_id),
            last_swept_at = ?3,
            hot_until = CASE WHEN ?4 IS NULL THEN hot_until
                             ELSE MAX(COALESCE(hot_until, ''), ?4) END
         WHERE issue_id = ?1",
        params![b.issue_id, b.cursor, b.swept_at, b.hot_until],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn purge_seen(c: &mut Connection, before: &str, keep: u32) -> Result<usize> {
    let tx = c.transaction()?;
    let n = tx.execute(
        "DELETE FROM seen_comments WHERE rowid IN (
           SELECT s.rowid FROM (
             SELECT rowid, issue_id, comment_id, created_at,
                    ROW_NUMBER() OVER (PARTITION BY issue_id ORDER BY created_at DESC, rowid DESC) AS rn
             FROM seen_comments) s
           LEFT JOIN conversations c ON c.issue_id = s.issue_id
           WHERE s.created_at < ?1 AND s.rn > ?2
             AND (c.cursor_comment_id IS NULL OR c.cursor_comment_id != s.comment_id)
             AND (c.watermark_comment_id IS NULL OR c.watermark_comment_id != s.comment_id))",
        params![before, keep],
    )?;
    tx.commit()?;
    Ok(n)
}

pub fn next_topic_job(c: &mut Connection) -> Result<Option<Conversation>> {
    Ok(query_conversations(
        c,
        "topic_state IN ('pending', 'recreating') AND removed_at IS NULL
         ORDER BY discovered_at, issue_id LIMIT 1",
        [],
    )?
    .pop())
}

pub fn set_topic(c: &mut Connection, issue_id: &str, thread_id: i64) -> Result<()> {
    c.execute(
        "UPDATE conversations SET topic_thread_id = ?2, topic_state = 'active' WHERE issue_id = ?1",
        params![issue_id, thread_id],
    )?;
    Ok(())
}

pub fn topic_lost(c: &mut Connection, issue_id: &str, thread_id: i64) -> Result<()> {
    c.execute(
        "UPDATE conversations SET topic_thread_id = NULL, topic_state = 'recreating'
         WHERE issue_id = ?1 AND topic_thread_id = ?2",
        params![issue_id, thread_id],
    )?;
    Ok(())
}

const CHUNK_COLS: &str = "o.id, o.issue_id, o.comment_id, o.chunk, o.chunks, o.html, o.plain,
    o.use_plain, o.attempts, c.topic_thread_id";

fn chunk_row(r: &Row<'_>) -> rusqlite::Result<TgChunk> {
    Ok(TgChunk {
        id: r.get(0)?,
        issue_id: r.get(1)?,
        comment_id: r.get(2)?,
        chunk: r.get(3)?,
        chunks: r.get(4)?,
        html: r.get(5)?,
        plain: r.get(6)?,
        use_plain: r.get::<_, i64>(7)? != 0,
        attempts: r.get::<_, i64>(8)?.max(0) as u32,
        thread_id: r.get(9)?,
    })
}

pub fn next_chunk(c: &mut Connection, now: &str) -> Result<Option<TgChunk>> {
    Ok(c.query_row(
        &format!(
            "SELECT {CHUNK_COLS} FROM tg_outbox o JOIN conversations c ON c.issue_id = o.issue_id
             WHERE o.status = 'pending' AND o.next_attempt_at <= ?1
               AND c.topic_state = 'active' AND c.topic_thread_id IS NOT NULL
               AND NOT EXISTS (SELECT 1 FROM tg_outbox p
                               WHERE p.issue_id = o.issue_id AND p.status = 'pending' AND p.id < o.id)
             ORDER BY o.id LIMIT 1"
        ),
        [now],
        chunk_row,
    )
    .optional()?)
}

pub fn next_chunk_due(c: &mut Connection) -> Result<Option<String>> {
    Ok(c.query_row(
        "SELECT MIN(next_attempt_at) FROM tg_outbox WHERE status = 'pending'",
        [],
        |r| r.get(0),
    )?)
}

pub fn chunk_sent(c: &mut Connection, id: i64, message_id: i64) -> Result<()> {
    // The text is the agent's reply: not kept once delivered.
    c.execute(
        "UPDATE tg_outbox SET status = 'sent', telegram_message_id = ?2, html = NULL, plain = NULL,
            updated_at = ?3 WHERE id = ?1",
        params![id, message_id, now()],
    )?;
    Ok(())
}

pub fn chunk_use_plain(c: &mut Connection, id: i64) -> Result<()> {
    c.execute(
        "UPDATE tg_outbox SET use_plain = 1, updated_at = ?2 WHERE id = ?1",
        params![id, now()],
    )?;
    Ok(())
}

pub fn chunk_retry(c: &mut Connection, id: i64, attempts: u32, next_at: &str) -> Result<()> {
    c.execute(
        "UPDATE tg_outbox SET attempts = ?2, next_attempt_at = ?3, updated_at = ?4 WHERE id = ?1",
        params![id, attempts, next_at, now()],
    )?;
    Ok(())
}

pub fn chunk_abandon(c: &mut Connection, id: i64) -> Result<()> {
    c.execute(
        "UPDATE tg_outbox SET status = 'abandoned', html = NULL, plain = NULL, updated_at = ?2
         WHERE id = ?1",
        params![id, now()],
    )?;
    Ok(())
}

pub fn queue_depth(c: &mut Connection) -> Result<u64> {
    let n: i64 = c.query_row(
        "SELECT COUNT(*) FROM tg_outbox WHERE status IN ('pending', 'sending')",
        [],
        |r| r.get(0),
    )?;
    Ok(n.max(0) as u64)
}

pub fn chunks(c: &mut Connection) -> Result<Vec<(TgChunk, String)>> {
    let mut stmt = c.prepare(&format!(
        "SELECT {CHUNK_COLS}, o.status FROM tg_outbox o LEFT JOIN conversations c
           ON c.issue_id = o.issue_id ORDER BY o.id"
    ))?;
    Ok(stmt
        .query_map([], |r| Ok((chunk_row(r)?, r.get(10)?)))?
        .collect::<rusqlite::Result<_>>()?)
}

const PC_COLS: &str = "id, chat_id, message_id, update_id, issue_id, client_request_id, body,
    status, attempts, last_error, comment_id, notified";

fn pc_row(r: &Row<'_>) -> rusqlite::Result<PcEntry> {
    let status: String = r.get(7)?;
    Ok(PcEntry {
        id: r.get(0)?,
        chat_id: r.get(1)?,
        message_id: r.get(2)?,
        update_id: r.get(3)?,
        issue_id: r.get(4)?,
        client_request_id: r.get(5)?,
        body: r.get(6)?,
        status: PcStatus::parse(&status),
        attempts: r.get::<_, i64>(8)?.max(0) as u32,
        last_error: r.get(9)?,
        comment_id: r.get(10)?,
        notified: r.get::<_, i64>(11)? != 0,
    })
}

pub fn due_messages(c: &mut Connection, now: &str) -> Result<Vec<PcEntry>> {
    let mut stmt = c.prepare(&format!(
        "SELECT {PC_COLS} FROM pc_outbox WHERE status = 'pending' AND next_attempt_at <= ?1
         ORDER BY next_attempt_at, id"
    ))?;
    Ok(stmt
        .query_map([now], pc_row)?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn next_message_due(c: &mut Connection) -> Result<Option<String>> {
    Ok(c.query_row(
        "SELECT MIN(next_attempt_at) FROM pc_outbox WHERE status = 'pending'",
        [],
        |r| r.get(0),
    )?)
}

pub fn message_in_flight(c: &mut Connection, id: i64) -> Result<()> {
    c.execute(
        "UPDATE pc_outbox SET status = 'in_flight', updated_at = ?2 WHERE id = ?1",
        params![id, now()],
    )?;
    Ok(())
}

pub fn message_finish(
    c: &mut Connection,
    id: i64,
    status: PcStatus,
    comment_id: Option<&str>,
    error: Option<&str>,
) -> Result<()> {
    // The body is the user's text: wiped once the entry is finished.
    c.execute(
        "UPDATE pc_outbox SET status = ?2, body = NULL, comment_id = COALESCE(?3, comment_id),
            last_error = COALESCE(?4, last_error), updated_at = ?5 WHERE id = ?1",
        params![id, status.as_str(), comment_id, error, now()],
    )?;
    Ok(())
}

pub fn message_retry(
    c: &mut Connection,
    id: i64,
    attempts: u32,
    next_at: &str,
    last_error: &str,
    notified: bool,
) -> Result<()> {
    c.execute(
        "UPDATE pc_outbox SET status = 'pending', attempts = ?2, next_attempt_at = ?3,
            last_error = ?4, notified = ?5, updated_at = ?6 WHERE id = ?1",
        params![id, attempts, next_at, last_error, notified as i64, now()],
    )?;
    Ok(())
}

pub fn message(c: &mut Connection, id: i64) -> Result<Option<PcEntry>> {
    Ok(c.query_row(
        &format!("SELECT {PC_COLS} FROM pc_outbox WHERE id = ?1"),
        [id],
        pc_row,
    )
    .optional()?)
}

pub fn messages(c: &mut Connection) -> Result<Vec<PcEntry>> {
    let mut stmt = c.prepare(&format!("SELECT {PC_COLS} FROM pc_outbox ORDER BY id"))?;
    Ok(stmt
        .query_map([], pc_row)?
        .collect::<rusqlite::Result<_>>()?)
}
