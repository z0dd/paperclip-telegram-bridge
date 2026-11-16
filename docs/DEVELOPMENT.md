# Development Guide

## Prerequisites

- Rust 1.82+ (install via [rustup](https://rustup.rs/))
- Docker (for local testing and image builds)
- `docker-compose` (for integration tests)
- `git`

## Project Structure

```
src/
├── main.rs                 # Entry points: run, check-config, revoke-paperclip-key
├── app.rs                  # Main application loop (polling, graceful shutdown)
├── config.rs               # Configuration parsing (environment variables)
├── domain.rs               # Core types (Attention, Interaction, Approval, etc.)
├── core/
│   ├── mod.rs              # Core application logic
│   ├── dialog.rs           # Event handling (callbacks, messages, commands)
│   ├── commands.rs         # Bot commands (/status, /backlog, /help, /cancel)
│   ├── sender.rs           # Outbox and delivery orchestration
│   └── allowlist.rs        # User ID allowlist enforcement
├── paperclip/
│   ├── mod.rs              # Paperclip API trait
│   └── http.rs             # HTTP client for Paperclip (reqwest-based)
├── telegram/
│   ├── mod.rs              # Telegram API trait
│   ├── http.rs             # HTTP client for Telegram API
│   └── types.rs            # Telegram types (Update, Message, Callback, etc.)
├── store/
│   ├── mod.rs              # Storage trait
│   └── sqlite.rs           # SQLite implementation (WAL mode)
├── render.rs               # HTML/Telegram markup rendering
├── timeutil.rs             # Timestamp parsing and formatting
└── logging.rs              # Structured logging setup (JSON or text)

tests/
├── core_flow.rs            # Integration tests (Q&A flow, idempotency, allowlist)
├── store.rs                # SQLite recovery and idempotency tests
└── telegram_proxy.rs       # Proxy enforcement tests

deploy/
├── compose.example.yml     # Example Docker Compose configuration
└── .env.example            # Environment variable template
```

## Local Development

### 1. Clone and Setup

```bash
git clone https://github.com/z0dd/paperclip-telegram-bridge.git
cd paperclip-telegram-bridge

# Copy env template
cp deploy/.env.example .env.local

# Edit .env.local with your Paperclip/Telegram details
# (or use mock servers for testing; see TESTING.md)
```

### 2. Check Configuration

```bash
# Validate config without starting the service
cargo run --bin bridge -- check-config
```

This loads `.env.local` and reports any missing or invalid variables without starting the server.

### 3. Run Locally (Development Mode)

```bash
# Requires real Paperclip and Telegram (see TESTING.md for mocks)
cargo run --bin bridge -- run

# With debug logging
RUST_LOG=debug cargo run --bin bridge -- run
```

### 4. Build Docker Image (Local)

```bash
# Build and tag locally
docker build -t paperclip-telegram-bridge:local .

# Run with Docker Compose (for integration tests)
docker compose -f deploy/compose.example.yml up
```

## Code Style and Formatting

### Automatic Formatting

```bash
cargo fmt           # Format all code
cargo fmt -- --check  # Check without modifying
```

### Linting and Auditing

```bash
cargo clippy        # Lint for common mistakes
cargo clippy -- -D warnings  # Fail on warnings

cargo deny check     # Audit dependencies for security/license issues
```

### Pre-commit Hook (Optional)

To prevent committing badly formatted code:

```bash
# Add to .git/hooks/pre-commit
#!/bin/sh
set -e
cargo fmt -- --check
cargo clippy -- -D warnings
echo "Formatting and linting passed ✓"
```

## Testing

See [TESTING.md](TESTING.md) for comprehensive test strategy and how to run:

```bash
cargo test           # All unit + integration tests
cargo test --test core_flow  # Specific test file
```

## Debugging

### Log Levels

```bash
# Development
RUST_LOG=debug cargo run --bin bridge -- run

# Specific module
RUST_LOG=bridge::paperclip=debug,bridge::telegram=warn cargo run --bin bridge -- run
```

### Inspecting State

```bash
# Connect to SQLite during development
sqlite3 /tmp/bridge-state/bridge.db

# Query outbox
sqlite> SELECT id, issue_ref, status, created_at FROM outbox LIMIT 5;
```

### Docker Debugging

```bash
# Shell into running container
docker compose exec -it bridge sh

# View logs
docker compose logs -f bridge

# Inspect image layers
docker history paperclip-telegram-bridge:local

# Check for secrets in image
docker save paperclip-telegram-bridge:local | tar -x && grep -r "token\|secret" .
```

## Key Components to Understand

### 1. Application Loop (src/app.rs)

Entry point for the `run` subcommand. Sets up:
- Configuration loading
- Database initialization
- Tokio runtime (async)
- Three polling tasks (attention, runs, health)
- SIGTERM handler (graceful shutdown)
- Health file updater

### 2. Core Dialog Handler (src/core/dialog.rs)

Processes all incoming events:
- **Attention items** (questions, governance decisions): Render, enqueue to outbox
- **User responses** (Telegram messages, callbacks): Validate, format, send to Paperclip
- **Telegram updates**: Fetch with long polling (`getUpdates`), process

### 3. Outbox and Delivery (src/core/sender.rs)

Manages the delivery queue:
- Dequeue pending messages
- Retry with exponential backoff
- Handle Telegram errors (rate limit, auth, etc.)
- Update state on success

### 4. SQLite Store (src/store/sqlite.rs)

Stateful operations:
- **insert_delivery**: Enqueue message
- **update_delivery**: Mark as sent/failed
- **recover**: On startup, reset in-flight → pending
- **Idempotency**: Message ID prevents duplicates

### 5. Paperclip Client (src/paperclip/http.rs)

HTTP calls with error classification:
- 401/403: Auth error (transient if retryable)
- 409: Already resolved elsewhere (idempotent retry)
- 5xx: Server error (retry with backoff)
- 429: Rate limit (back off, then retry)

## Common Tasks

### Adding a New Paperclip API Endpoint

1. Add the HTTP call to `src/paperclip/http.rs` (e.g., `async fn my_endpoint(...) -> Result<...>`)
2. Add a trait method to `src/paperclip/mod.rs`
3. Call it from the appropriate handler (dialog.rs, commands.rs)
4. Add test in `tests/core_flow.rs`

Example:
```rust
// src/paperclip/http.rs
async fn fetch_run_logs(&self, run_id: &str) -> Result<String> {
    let v = self.get(&["runs", run_id, "logs"], &[]).await?;
    dto::run_logs(&v).ok_or(PaperclipError::Decode)
}

// src/paperclip/mod.rs
#[async_trait]
pub trait PaperclipApi: Send + Sync {
    async fn fetch_run_logs(&self, run_id: &str) -> Result<String>;
}
```

### Adding a New Bot Command

1. Add command to `bot_commands()` in `src/core/commands.rs`
2. Add handler logic (async function)
3. Add match arm in `handle()`
4. Test in `tests/core_flow.rs`

Example:
```rust
pub fn bot_commands() -> Vec<BotCommand> {
    [
        ("status", "Status…"),
        ("logs", "Show run logs"),  // New
    ]…
}

pub async fn handle<P, T>(ctx: &Ctx<P, T>, name: &str) -> Result<Effects> {
    Ok(match name {
        "start" | "help" => …,
        "logs" => Effects::message(show_logs(ctx).await?),  // New
        …
    })
}
```

### Changing Configuration

Edit `src/config.rs`:
1. Add field to relevant struct (PaperclipConfig, TelegramConfig, etc.)
2. Add parsing logic in `from_env()` (with default value)
3. Update `.env.example` and README.md
4. Add test in `tests/config_*.rs` (if applicable)

## Performance Considerations

- **Attention polling**: Limited to `ATTENTION_MAX_PAGES=10` to prevent runaway pagination
- **Long polling timeout**: `TELEGRAM_LONG_POLL_TIMEOUT=25s` (Telegram max is 50s)
- **Retry backoff**: Exponential, capped at 60s to prevent thundering herd
- **Database**: WAL mode allows concurrent reads during writes; journal mode would stall

## Debugging Idempotency Issues

If a message is getting delivered twice:

1. Check SQLite: `SELECT message_id, status FROM outbox WHERE issue_ref = '...';`
2. Look at logs for Telegram 400 (conflict) vs 200 (success)
3. Verify that `recover()` in `src/store/sqlite.rs` is running on startup
4. Check if Telegram offset tracking is working (compare message IDs in logs)

## Common Issues

| Issue | Cause | Fix |
|-------|-------|-----|
| Config fails with "TELEGRAM_PROXY_URL is required" | Env var missing | Add to .env.local |
| `cargo build` fails with "cannot find crate" | Dependency issue | `cargo update`, `cargo clean` |
| Tests hang | Mocks not responding | Check `deploy/compose.yml` mock setup |
| Logs don't show JSON | LOG_FORMAT env var | Set to `json` or `text` |
| Health file never updates | `/state` permission issue | Check volume mount, uid 65532 |
| Messages get duplicated | Outbox recovery issue | Check SQLite `recover()` logic; inspect WAL |

## Next Steps

- Read [TESTING.md](TESTING.md) to understand test coverage and run tests
- Read [ARCHITECTURE.md](ARCHITECTURE.md) for system design and data flow
- See [RUNBOOK.md](RUNBOOK.md) for operational procedures
