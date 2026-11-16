//! SQL for the state store. Migrations are tracked in `PRAGMA user_version`.

use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};

use super::*;
use crate::domain::{Awaiting, DraftAnswers};
use crate::timeutil;

pub const SCHEMA_VERSION: i64 = 1;

const MIGRATIONS: &[&str] = &[
    // 1: initial schema (architecture §4.1, plus deliveries.follow_until).
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
    let outbox_reset = tx.execute(
        "UPDATE outbox SET status = 'pending', updated_at = ?1 WHERE status = 'in_flight'",
        [now()],
    )?;
    let sending = {
        let mut stmt =
            tx.prepare("SELECT id FROM deliveries WHERE status = 'sending' ORDER BY id")?;
        stmt.query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<i64>>>()?
    };
    if get_meta(&tx, meta::STARTED_AT)?.is_none() {
        set_meta(&tx, meta::STARTED_AT, &now())?;
    }
    tx.commit()?;
    Ok(Recovery {
        outbox_reset,
        sending,
    })
}

pub fn commit_update(
    c: &mut Connection,
    update_id: Option<i64>,
    ops: Vec<StoreOp>,
) -> Result<CommitOutcome> {
    let tx = c.transaction()?;
    if let Some(uid) = update_id {
        let offset: i64 = get_meta(&tx, meta::TELEGRAM_OFFSET)?
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        if uid < offset {
            return Ok(CommitOutcome::Duplicate);
        }
    }
    for op in ops {
        apply_op(&tx, op)?;
    }
    if let Some(uid) = update_id {
        set_meta(&tx, meta::TELEGRAM_OFFSET, &(uid + 1).to_string())?;
    }
    tx.commit()?;
    Ok(CommitOutcome::Applied)
}

fn apply_op(tx: &Transaction<'_>, op: StoreOp) -> Result<()> {
    let ts = now();
    match op {
        StoreOp::PutDraft(d) => put_draft(tx, &d)?,
        StoreOp::DeleteDraft(id) => {
            tx.execute("DELETE FROM drafts WHERE delivery_id = ?1", [id])?;
        }
        StoreOp::ClearOtherTextAwaiting { keep } => {
            tx.execute(
                "UPDATE drafts SET awaiting = NULL, updated_at = ?2
                 WHERE delivery_id != ?1 AND awaiting IS NOT NULL AND awaiting != 'governed_confirm'",
                params![keep, ts],
            )?;
        }
        StoreOp::Enqueue {
            delivery_id,
            resolution,
        } => {
            let body = serde_json::to_string(&resolution).map_err(|_| StoreError::Decode)?;
            let inserted = tx.execute(
                "INSERT OR IGNORE INTO outbox(delivery_id, action, body_json, status, attempts,
                                              next_attempt_at, created_at, updated_at)
                 VALUES (?1, ?2, ?3, 'pending', 0, ?4, ?4, ?4)",
                params![delivery_id, resolution.action_name(), body, ts],
            )?;
            if inserted > 0 {
                tx.execute(
                    "UPDATE deliveries SET status = 'answering', updated_at = ?2 WHERE id = ?1",
                    params![delivery_id, ts],
                )?;
            }
        }
        StoreOp::SetStatus {
            delivery_id,
            status,
            closed_reason,
        } => {
            tx.execute(
                "UPDATE deliveries SET status = ?2, closed_reason = COALESCE(?3, closed_reason),
                        updated_at = ?4 WHERE id = ?1",
                params![delivery_id, status.as_str(), closed_reason, ts],
            )?;
        }
        StoreOp::BumpGeneration(id) => {
            tx.execute(
                "UPDATE deliveries SET gen = gen + 1, updated_at = ?2 WHERE id = ?1",
                params![id, ts],
            )?;
        }
    }
    Ok(())
}

fn put_draft(tx: &Connection, d: &Draft) -> Result<()> {
    let answers = serde_json::to_string(&d.answers).map_err(|_| StoreError::Decode)?;
    tx.execute(
        "INSERT INTO drafts(delivery_id, step, answers_json, awaiting, prompt_message_id, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(delivery_id) DO UPDATE SET step = excluded.step,
            answers_json = excluded.answers_json, awaiting = excluded.awaiting,
            prompt_message_id = excluded.prompt_message_id, updated_at = excluded.updated_at",
        params![
            d.delivery_id,
            d.step as i64,
            answers,
            d.awaiting.map(Awaiting::to_db),
            d.prompt_message_id,
            d.updated_at,
        ],
    )?;
    Ok(())
}

pub fn insert_delivery(c: &mut Connection, d: &NewDelivery) -> Result<Option<i64>> {
    let snapshot = serde_json::to_string(&d.snapshot).map_err(|_| StoreError::Decode)?;
    let ts = now();
    let n = c.execute(
        "INSERT OR IGNORE INTO deliveries(interaction_id, company_id, issue_id, issue_ref, kind,
                                          snapshot_json, status, gen, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, ?8, ?8)",
        params![
            d.interaction_id,
            d.company_id,
            d.issue_id,
            d.issue_ref,
            d.snapshot.interaction.kind.as_str(),
            snapshot,
            d.status.as_str(),
            ts
        ],
    )?;
    Ok((n > 0).then(|| c.last_insert_rowid()))
}

const DELIVERY_COLS: &str =
    "id, interaction_id, company_id, issue_id, issue_ref, kind, snapshot_json,
    status, gen, closed_reason, follow_until, created_at, updated_at";

fn delivery_row(r: &Row<'_>) -> rusqlite::Result<(Delivery, String)> {
    let snapshot_json: String = r.get(6)?;
    let status: String = r.get(7)?;
    Ok((
        Delivery {
            id: r.get(0)?,
            interaction_id: r.get(1)?,
            company_id: r.get(2)?,
            issue_id: r.get(3)?,
            issue_ref: r.get(4)?,
            kind: r.get(5)?,
            snapshot: Snapshot {
                interaction: placeholder_interaction(),
                issue_title: None,
                agent_name: None,
            },
            status: DeliveryStatus::parse(&status).unwrap_or(DeliveryStatus::Closed),
            generation: r.get(8)?,
            closed_reason: r.get(9)?,
            follow_until: r.get(10)?,
            created_at: r.get(11)?,
            updated_at: r.get(12)?,
            messages: Vec::new(),
        },
        snapshot_json,
    ))
}

fn placeholder_interaction() -> Interaction {
    use crate::domain::*;
    Interaction {
        id: String::new(),
        company_id: String::new(),
        issue_id: String::new(),
        kind: InteractionKind::Other(String::new()),
        status: InteractionStatus::Pending,
        title: None,
        created_by_agent_id: None,
        created_at: String::new(),
        resolved_by_user_id: None,
        resolved_by_agent_id: None,
        addressee_agent_id: None,
        addressee_user_id: None,
        body: InteractionBody::Unsupported {
            reason: UnsupportedReason::Malformed,
        },
        result: None,
    }
}

fn finish_delivery(c: &Connection, (mut d, snapshot_json): (Delivery, String)) -> Result<Delivery> {
    d.snapshot = serde_json::from_str(&snapshot_json).map_err(|_| StoreError::Decode)?;
    let mut stmt = c.prepare_cached(
        "SELECT chunk, message_id FROM delivery_messages WHERE delivery_id = ?1 ORDER BY chunk",
    )?;
    d.messages = stmt
        .query_map([d.id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(d)
}

fn query_deliveries(
    c: &Connection,
    where_sql: &str,
    p: impl rusqlite::Params,
) -> Result<Vec<Delivery>> {
    let sql = format!("SELECT {DELIVERY_COLS} FROM deliveries WHERE {where_sql}");
    let rows = {
        let mut stmt = c.prepare(&sql)?;
        stmt.query_map(p, delivery_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    rows.into_iter().map(|r| finish_delivery(c, r)).collect()
}

pub fn delivery(c: &mut Connection, id: i64) -> Result<Option<Delivery>> {
    Ok(query_deliveries(c, "id = ?1", [id])?.pop())
}

pub fn delivery_by_interaction(c: &mut Connection, iid: &str) -> Result<Option<Delivery>> {
    Ok(query_deliveries(c, "interaction_id = ?1", [iid])?.pop())
}

pub fn delivery_by_message(c: &mut Connection, message_id: i64) -> Result<Option<Delivery>> {
    Ok(query_deliveries(
        c,
        "id = (SELECT delivery_id FROM delivery_messages WHERE message_id = ?1 LIMIT 1)",
        [message_id],
    )?
    .pop())
}

pub fn deliveries_with_status(
    c: &mut Connection,
    statuses: &[DeliveryStatus],
) -> Result<Vec<Delivery>> {
    if statuses.is_empty() {
        return Ok(Vec::new());
    }
    let list = statuses
        .iter()
        .map(|s| format!("'{}'", s.as_str()))
        .collect::<Vec<_>>()
        .join(",");
    query_deliveries(c, &format!("status IN ({list}) ORDER BY id"), [])
}

pub fn known_interactions(c: &mut Connection, ids: &[String]) -> Result<Vec<String>> {
    let mut stmt = c.prepare_cached("SELECT 1 FROM deliveries WHERE interaction_id = ?1")?;
    let mut out = Vec::new();
    for id in ids {
        if stmt.exists([id])? {
            out.push(id.clone());
        }
    }
    Ok(out)
}

pub fn mark_sent(
    c: &mut Connection,
    id: i64,
    status: DeliveryStatus,
    messages: &[i64],
) -> Result<()> {
    let tx = c.transaction()?;
    tx.execute("DELETE FROM delivery_messages WHERE delivery_id = ?1", [id])?;
    for (chunk, m) in messages.iter().enumerate() {
        tx.execute(
            "INSERT INTO delivery_messages(delivery_id, chunk, message_id) VALUES (?1, ?2, ?3)",
            params![id, chunk as i64, m],
        )?;
    }
    tx.execute(
        "UPDATE deliveries SET status = ?2, updated_at = ?3 WHERE id = ?1 AND status IN ('new', 'sending')",
        params![id, status.as_str(), now()],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn replace_last_message(c: &mut Connection, id: i64, message_id: i64) -> Result<()> {
    let tx = c.transaction()?;
    let chunk: Option<i64> = tx
        .query_row(
            "SELECT MAX(chunk) FROM delivery_messages WHERE delivery_id = ?1",
            [id],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    tx.execute(
        "INSERT INTO delivery_messages(delivery_id, chunk, message_id) VALUES (?1, ?2, ?3)
         ON CONFLICT(delivery_id, chunk) DO UPDATE SET message_id = excluded.message_id",
        params![id, chunk.unwrap_or(0), message_id],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn close_delivery(
    c: &mut Connection,
    id: i64,
    status: DeliveryStatus,
    reason: &str,
) -> Result<()> {
    let tx = c.transaction()?;
    tx.execute(
        "UPDATE deliveries SET status = ?2, closed_reason = ?3, updated_at = ?4 WHERE id = ?1",
        params![id, status.as_str(), reason, now()],
    )?;
    tx.execute("DELETE FROM drafts WHERE delivery_id = ?1", [id])?;
    tx.commit()?;
    Ok(())
}

pub fn update_snapshot(c: &mut Connection, id: i64, s: &Snapshot) -> Result<()> {
    let json = serde_json::to_string(s).map_err(|_| StoreError::Decode)?;
    c.execute(
        "UPDATE deliveries SET snapshot_json = ?2, updated_at = ?3 WHERE id = ?1",
        params![id, json, now()],
    )?;
    Ok(())
}

pub fn set_following(c: &mut Connection, id: i64, until: &str) -> Result<()> {
    c.execute(
        "UPDATE deliveries SET status = 'following', follow_until = ?2, updated_at = ?3 WHERE id = ?1",
        params![id, until, now()],
    )?;
    Ok(())
}

fn draft_row(r: &Row<'_>) -> rusqlite::Result<(Draft, String)> {
    let awaiting: Option<String> = r.get(3)?;
    Ok((
        Draft {
            delivery_id: r.get(0)?,
            step: r.get::<_, i64>(1)?.max(0) as usize,
            answers: DraftAnswers::default(),
            awaiting: awaiting.as_deref().and_then(Awaiting::from_db),
            prompt_message_id: r.get(4)?,
            updated_at: r.get(5)?,
        },
        r.get(2)?,
    ))
}

fn query_drafts(c: &Connection, where_sql: &str, p: impl rusqlite::Params) -> Result<Vec<Draft>> {
    let sql = format!(
        "SELECT delivery_id, step, answers_json, awaiting, prompt_message_id, updated_at
         FROM drafts WHERE {where_sql}"
    );
    let mut stmt = c.prepare(&sql)?;
    let rows = stmt
        .query_map(p, draft_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter()
        .map(|(mut d, json)| {
            d.answers = serde_json::from_str(&json).map_err(|_| StoreError::Decode)?;
            Ok(d)
        })
        .collect()
}

pub fn draft(c: &mut Connection, id: i64) -> Result<Option<Draft>> {
    Ok(query_drafts(c, "delivery_id = ?1", [id])?.pop())
}

pub fn drafts(c: &mut Connection) -> Result<Vec<Draft>> {
    query_drafts(c, "1 = 1 ORDER BY delivery_id", [])
}

pub fn text_awaiting_draft(c: &mut Connection) -> Result<Option<Draft>> {
    Ok(query_drafts(
        c,
        "awaiting IS NOT NULL AND awaiting != 'governed_confirm' ORDER BY updated_at DESC LIMIT 1",
        [],
    )?
    .pop())
}

pub fn draft_by_prompt_message(c: &mut Connection, message_id: i64) -> Result<Option<Draft>> {
    Ok(query_drafts(c, "prompt_message_id = ?1", [message_id])?.pop())
}

pub fn set_prompt_message(c: &mut Connection, id: i64, message_id: i64) -> Result<()> {
    c.execute(
        "UPDATE drafts SET prompt_message_id = ?2 WHERE delivery_id = ?1",
        params![id, message_id],
    )?;
    Ok(())
}

const OUTBOX_COLS: &str =
    "id, delivery_id, body_json, status, attempts, next_attempt_at, last_error, created_at";

fn outbox_row(r: &Row<'_>) -> rusqlite::Result<OutboxEntry> {
    let body: Option<String> = r.get(2)?;
    let status: String = r.get(3)?;
    Ok(OutboxEntry {
        id: r.get(0)?,
        delivery_id: r.get(1)?,
        resolution: body.and_then(|b| serde_json::from_str(&b).ok()),
        status: OutboxStatus::parse(&status).unwrap_or(OutboxStatus::Abandoned),
        attempts: r.get::<_, i64>(4)?.max(0) as u32,
        next_attempt_at: r.get(5)?,
        last_error: r.get(6)?,
        created_at: r.get(7)?,
    })
}

pub fn outbox_for(c: &mut Connection, delivery_id: i64) -> Result<Option<OutboxEntry>> {
    Ok(c.query_row(
        &format!("SELECT {OUTBOX_COLS} FROM outbox WHERE delivery_id = ?1"),
        [delivery_id],
        outbox_row,
    )
    .optional()?)
}

pub fn due_outbox(c: &mut Connection, now: &str) -> Result<Vec<OutboxEntry>> {
    let mut stmt = c.prepare(&format!(
        "SELECT {OUTBOX_COLS} FROM outbox WHERE status = 'pending' AND next_attempt_at <= ?1
         ORDER BY next_attempt_at, id"
    ))?;
    Ok(stmt
        .query_map([now], outbox_row)?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn next_outbox_due(c: &mut Connection) -> Result<Option<String>> {
    Ok(c.query_row(
        "SELECT MIN(next_attempt_at) FROM outbox WHERE status = 'pending'",
        [],
        |r| r.get(0),
    )?)
}

pub fn pending_outbox_count(c: &mut Connection) -> Result<u64> {
    let n: i64 = c.query_row(
        "SELECT COUNT(*) FROM outbox WHERE status IN ('pending', 'in_flight')",
        [],
        |r| r.get(0),
    )?;
    Ok(n.max(0) as u64)
}

pub fn outbox_set_status(c: &mut Connection, id: i64, status: OutboxStatus) -> Result<()> {
    c.execute(
        "UPDATE outbox SET status = ?2, updated_at = ?3 WHERE id = ?1",
        params![id, status.as_str(), now()],
    )?;
    Ok(())
}

pub fn outbox_finish(
    c: &mut Connection,
    id: i64,
    delivery_id: i64,
    status: OutboxStatus,
    delivery_status: DeliveryStatus,
    reason: Option<&str>,
) -> Result<()> {
    let tx = c.transaction()?;
    let ts = now();
    // The body holds the user's answers: wiped once the entry is finished.
    tx.execute(
        "UPDATE outbox SET status = ?2, body_json = NULL, updated_at = ?3 WHERE id = ?1",
        params![id, status.as_str(), ts],
    )?;
    tx.execute(
        "UPDATE deliveries SET status = ?2, closed_reason = COALESCE(?3, closed_reason), updated_at = ?4
         WHERE id = ?1",
        params![delivery_id, delivery_status.as_str(), reason, ts],
    )?;
    if matches!(
        delivery_status,
        DeliveryStatus::Answered | DeliveryStatus::Closed | DeliveryStatus::Following
    ) {
        tx.execute("DELETE FROM drafts WHERE delivery_id = ?1", [delivery_id])?;
    }
    tx.commit()?;
    Ok(())
}

pub fn outbox_remove(
    c: &mut Connection,
    id: i64,
    delivery_id: i64,
    status: DeliveryStatus,
) -> Result<()> {
    let tx = c.transaction()?;
    tx.execute("DELETE FROM outbox WHERE id = ?1", [id])?;
    tx.execute(
        "UPDATE deliveries SET status = ?2, updated_at = ?3 WHERE id = ?1",
        params![delivery_id, status.as_str(), now()],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn outbox_retry(
    c: &mut Connection,
    id: i64,
    attempts: u32,
    next_at: &str,
    last_error: &str,
) -> Result<()> {
    c.execute(
        "UPDATE outbox SET status = 'pending', attempts = ?2, next_attempt_at = ?3, last_error = ?4,
                updated_at = ?5 WHERE id = ?1",
        params![id, attempts as i64, next_at, last_error, now()],
    )?;
    Ok(())
}

pub fn is_seen(c: &mut Connection, table: &str, col: &str, id: &str) -> Result<bool> {
    let mut stmt = c.prepare_cached(&format!("SELECT 1 FROM {table} WHERE {col} = ?1"))?;
    Ok(stmt.exists([id])?)
}

pub fn mark_seen(c: &mut Connection, table: &str, col: &str, ids: &[String]) -> Result<()> {
    let tx = c.transaction()?;
    {
        let mut stmt = tx.prepare_cached(&format!(
            "INSERT OR IGNORE INTO {table}({col}, seen_at) VALUES (?1, ?2)"
        ))?;
        let ts = now();
        for id in ids {
            stmt.execute(params![id, ts])?;
        }
    }
    tx.commit()?;
    Ok(())
}

pub fn purge_seen(c: &mut Connection, before: &str) -> Result<usize> {
    let tx = c.transaction()?;
    // seen_done_issues is kept: an old done issue may still be in the feed.
    let n = tx.execute("DELETE FROM seen_runs WHERE seen_at < ?1", [before])?;
    tx.commit()?;
    Ok(n)
}
