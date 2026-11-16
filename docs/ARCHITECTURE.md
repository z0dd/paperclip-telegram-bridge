# System Architecture

## Overview

The Paperclip-Telegram Bridge is a long-polling middleware service that connects Paperclip (infrastructure for agents and workflows) to Telegram for real-time notifications and approvals. It runs in a stateless container with SQLite state storage, ensuring idempotent message delivery and graceful recovery from failures.

```
┌──────────────────┐
│   Paperclip      │
│   (private)      │
└────────┬─────────┘
         │ (egress only)
         │ /api/companies/{id}/attention
         │ /api/issues/{id}/interactions
         │ /api/issues/{id}/approvals
         │ etc.
         │
    ┌────▼─────────────────────┐
    │ Bridge (container)        │
    │ ┌──────────────────────┐  │
    │ │ Polling threads:     │  │
    │ │ - attention feed     │  │
    │ │ - governance results │  │
    │ │ - heartbeat runs     │  │
    │ └──────────────────────┘  │
    │ ┌──────────────────────┐  │
    │ │ SQLite state store   │  │
    │ │ - outbox (pending)   │  │
    │ │ - delivery tracking  │  │
    │ │ - cache              │  │
    │ └──────────────────────┘  │
    └────┬────────────┬─────────┘
         │            │
         │ (egress)   │ (outbound only)
         │            └─────────► Xray proxy (socks5)
         │                            │
         └────────────────────────────┼─────────────────────┐
                                      │                     │
                                      ▼                     │
                          ┌───────────────────┐             │
                          │ Telegram Bot API  │             │
                          │ (api.telegram.org)│             │
                          └───────────────────┘             │
                                      │                     │
                          (getUpdates, sendMessage)   (no direct
                                      │            connection from
                                      ▼            Telegram)
                          ┌───────────────────┐
                          │ Telegram Bot      │
                          │ (chat with user)  │
                          └───────────────────┘
```

## Components

### Polling

Three independent polling loops run concurrently:

1. **Attention feed** (`PAPERCLIP_POLL_INTERVAL`, default 15s)
   - Polls `/api/companies/{id}/attention` for new questions and governance items
   - Handles pagination via cursor
   - Recovers from 400 (cursor stale) by restarting from page 1

2. **Governance results** (within attention poll)
   - Follows governance decisions (approvals/rejections) for `GOVERNED_RESULT_FOLLOW` duration (default 30m)
   - Updates Telegram card reaction when decision is confirmed

3. **Runs** (two independent loops)
   - **Heartbeat runs** (`PAPERCLIP_RUNS_POLL_INTERVAL`, default 60s): Polls recent failures for optional notifications
   - **Live runs**: Polls active run IDs; populates agent cache for `/status` command

### State Store (SQLite)

State is stored in `/state/bridge.db` with write-ahead logging (WAL):

- **outbox**: Messages awaiting delivery to Telegram (status: pending, sent, failed)
- **deliveries**: Tracking table (issue_ref, message_id, status, timestamps)
- **run_cache**: Live run IDs for quick lookup
- **agent_cache**: Agent name cache (issue_id → agent_name)

Recovery on startup:
- Entries with status `in_flight` are reset to `pending` (assume Telegram request timed out mid-transmission)
- Duplicates are idempotent: retrying a Telegram message with same `message_id` returns 400 (recovered from outbox, same effect)
- Already-sent messages (by Telegram offset) are skipped

### Retry and Backoff

- **Retry config**: base delay 1s, max delay 60s, 6 attempts (exponential backoff)
- **Notifications**: After attempt 5 (`RETRY_NOTIFY_AFTER`), bridge sends a notification to Telegram about the stalled message
- **Transient errors** (timeout, connect, 429 rate-limit): retry with backoff
- **Fatal errors** (401 auth, 404 not found, 422 invalid): log and skip (no retry)

## Deployment

### Container Security

- **User**: Non-root (uid 65532, distroless base `gcr.io/distroless/cc-debian12:nonroot`)
- **Capabilities**: ALL dropped (`cap_drop: [ALL]`); no special privileges
- **Filesystem**: Read-only root filesystem; only `/state` writable (for SQLite)
- **Network**: No exposed ports; all connections outbound only

### Network Isolation

- **Paperclip**: Reachable via `PAPERCLIP_URL` (must be private; reqwest `.no_proxy()` ensures no proxy used)
- **Telegram**: Reachable only through `TELEGRAM_PROXY_URL` (mandatory Xray proxy; reqwest configured to use proxy for `api.telegram.org`)
- **Paperclip remains private**: Bridge never publishes Paperclip data to internet; Telegram API calls go through proxy

### Graceful Shutdown

SIGTERM triggers two-phase shutdown with timeout `SHUTDOWN_GRACE` (default 10s) per phase:

1. **Phase 1**: Stop polling, cancel acceptance of new work (poller shutdown)
2. **Phase 2**: Wait for in-flight Telegram messages to complete (outbox flush)

Total worst-case shutdown time: 2 × `SHUTDOWN_GRACE`. Docker `stop_grace_period` must be > 2 × `SHUTDOWN_GRACE` to avoid SIGKILL.

## Failure Modes and Recovery

### Crash mid-Telegram delivery

**Scenario**: Bridge sends message to Telegram, receives 200 OK, then crashes before updating state.
- **Result**: Telegram received message; bridge recovers with in_flight → pending, retries delivery, Telegram returns 400 (duplicate), both sides accept (idempotent).
- **Safety**: Message is not duplicated on recovery.

### Crash mid-DB write

**Scenario**: Bridge writes outbox entry, then crashes before flushing to disk.
- **Result**: On recovery, in-flight entries reset to pending; message is re-sent; Telegram deduplicates by message_id.

### Paperclip unavailable

**Scenario**: Polling loop cannot reach Paperclip for 30+ minutes.
- **Result**: Attention feed returns 0 items; commands like `/status` show "Paperclip unavailable"; outbox continues to drain (in-flight messages to Telegram).
- **Recovery**: When Paperclip is reachable, polling resumes; cursor recovery handles stale cursor (400 → restart from page 1).

### Telegram proxy unavailable

**Scenario**: Xray or proxy becomes unreachable.
- **Result**: Outbox messages fail with transient error; retries with backoff; after 5 attempts, notification to Telegram fails too (bridge not in panic; just logs).
- **Recovery**: When proxy is reachable, retries resume.

## Data Flow

### Incoming (Attention → Telegram)

1. Poller: `GET /api/companies/{cid}/attention` → new question
2. Question appears in local cache and in outbox (status: pending)
3. Formatter: Render question, inline buttons, etc.
4. Retry loop: `POST /api/3/sendMessage` with `message_id` (idempotency key)
5. Telegram: Returns message; bridge updates outbox (status: sent)
6. User reads message in Telegram

### Outgoing (Telegram → Paperclip)

1. User: Tap button (callback query) or send text
2. Poller: `getUpdates` returns update with callback or message
3. Dialog/handlers: Process response, validate, format decision
4. Bridge: `POST /api/issues/{id}/interactions/{iid}/accept` or `reject`
5. Paperclip: Returns 200 or 409 (already resolved elsewhere)
6. Bridge: Updates card in Telegram (edit inline buttons, show ✅ reaction)

## Thread Safety

- Polling loops: independent tokio tasks; no shared state (poll results go into store via Arc<Mutex>)
- Store: single writer (main task consuming from outbox queue); readers (command handlers) via Arc<Mutex>
- Database: SQLite with journal mode = WAL; PRAGMA busy_timeout = 5s handles contention

## Observability

### Health File

- **Location**: `/state/health.json`
- **Format**: `{"ts": "2025-11-16T08:39:00Z", "status": "ok"}`
- **Update freq**: Every 30 seconds or on critical event
- **Use**: Docker healthcheck probe

### Structured Logging

- **Format**: JSON by default (LOG_FORMAT=json)
- **Fields**: timestamp, level, message, component, error, retry_count, etc.
- **Sampling**: High-volume events (polling) at debug level only

### Metrics (future)

- Not included in v1; architecture supports adding:
  - Message delivery latency
  - Retry counts and backoff distribution
  - Cache hit rate
  - Poll response times
