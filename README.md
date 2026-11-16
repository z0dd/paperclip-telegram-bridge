# paperclip-telegram-bridge

A bridge service connecting Paperclip infrastructure to Telegram, enabling secure and reliable message exchange through Xray proxy with long polling. The bridge is designed for reliable, auditable communication while ensuring Paperclip remains unexposed to the internet.

## Overview

This service acts as a middleware between Paperclip and Telegram, supporting both incoming and outgoing message flows with idempotent delivery and authentication. All traffic is routed through the Xray proxy — no direct connections to Telegram are possible.

**Key properties:**
- **Idempotent**: duplicate requests produce one effect; answers are never lost or duplicated after restart
- **Audited**: all interactions logged with decision context; all governance rules enforced
- **Resilient**: graceful degradation when upstreams are unavailable; automatic retry with backoff
- **Secure**: non-root container, read-only filesystem, no hardcoded secrets, all traffic via proxy

## Installation and Configuration

### Environment Variables

Configuration is set via environment variables. Secrets (tokens, keys) are read from files via `*_FILE` variables:

- `PAPERCLIP_URL` (required): Base URL of Paperclip API
- `PAPERCLIP_TOKEN_FILE` (required): Path to file containing Paperclip API token
- `PAPERCLIP_COMPANY_IDS` (required): Comma-separated list of company UUIDs
- `TELEGRAM_BOT_TOKEN_FILE` (required): Path to file containing Telegram bot token (from BotFather)
- `TELEGRAM_CHAT_ID` (required): Telegram chat ID (private chat with the bot)
- `TELEGRAM_USER_ID` (required): Telegram user ID (owner of the bot)
- `TELEGRAM_PROXY_URL` (required): Proxy for Telegram API (typically Xray: `socks5h://xray:1080`)

Optional configuration (defaults shown):
```
PAPERCLIP_UI_URL=                          # UI base URL for notification links
TELEGRAM_API_BASE_URL=https://api.telegram.org
STATE_PATH=/state/bridge.db                # SQLite database with WAL
HEALTH_FILE=/state/health.json
HEALTH_MAX_STALE=5m
PAPERCLIP_POLL_INTERVAL=15s                # Frequency of attention feed polling
PAPERCLIP_RUNS_POLL_INTERVAL=60s           # Frequency of run failures polling
TELEGRAM_LONG_POLL_TIMEOUT=25s             # Long poll timeout for new messages
PAPERCLIP_REQUEST_TIMEOUT=15s
PAPERCLIP_GOVERNED_TIMEOUT=120s            # Timeout for governance decisions
TELEGRAM_REQUEST_TIMEOUT=15s
CONNECT_TIMEOUT=10s
RETRY_BASE_DELAY=1s
RETRY_MAX_DELAY=60s
RETRY_MAX_ATTEMPTS=6
RETRY_NOTIFY_AFTER=5                       # After N retries, send a notification
NOTIFY_RUN_FAILURES=true                   # Send notifications for run failures
NOTIFY_TASK_COMPLETED=false                # Send when tasks complete
NOTIFY_RESOLVED_ELSEWHERE=true             # Send when cards are resolved outside Telegram
GOVERNED_APPROVALS_ENABLED=true
GOVERNED_DESTRUCTIVE_MODE=confirm          # "confirm" (2-step) or "ui_only" (no Telegram buttons)
GOVERNED_SHOW_ARGUMENTS=false               # Include sensitive arguments in Telegram preview
GOVERNED_RESULT_FOLLOW=30m                 # How long to follow governance decision results
MAX_MESSAGE_CHUNKS=3                       # Max messages per card (long texts are capped)
SHUTDOWN_GRACE=10s                         # Grace period for graceful shutdown (see below)
LOG_LEVEL=info
LOG_FORMAT=json
```

See [`.env.example`](.env.example) for a complete template.

### Docker Deployment

The bridge runs in a distroless container as non-root user (uid 65532) with a read-only filesystem. Only `/state` is writable (for SQLite database and heartbeat file).

Example Docker Compose (from `deploy/compose.example.yml`):

```yaml
services:
  bridge:
    image: paperclip-telegram-bridge:local
    restart: unless-stopped
    read_only: true
    user: "65532:65532"
    cap_drop: [ALL]
    security_opt:
      - no-new-privileges:true
    stop_grace_period: 30s                  # See graceful shutdown section
    env_file: .env
    environment:
      PAPERCLIP_TOKEN_FILE: /run/secrets/paperclip_token
      TELEGRAM_BOT_TOKEN_FILE: /run/secrets/telegram_bot_token
    secrets:
      - paperclip_token
      - telegram_bot_token
    volumes:
      - bridge-state:/state
    networks:
      - paperclip_internal
      - xray_egress
```

Secrets must be mounted or passed via `secrets` in Docker Compose. Files are mounted as readable by the container (permissions 644 or better).

The container exposes no ports: all network connections are outgoing (to Paperclip and via proxy to Telegram).

### Telegram Bot Setup

1. Create a Telegram bot via [@BotFather](https://t.me/BotFather) with `/newbot`
2. Extract the token (format: `123456789:ABCDEFGhIjklmnoPQRstUVWXYZabcDEf`)
3. **Strongly recommended**: Enable 2-factor authentication in Telegram settings to protect the chat
4. Add commands via `/setcommands`:
   - `/status` — show current bridge and Paperclip state
   - `/backlog` — list pending approvals and notifications

To revoke the board key in case of leak or suspicion:
```bash
# Revoke the current key programmatically (requires PAPERCLIP_TOKEN_FILE and PAPERCLIP_URL in environment)
bridge revoke-paperclip-key

# Or revoke it manually in the Paperclip UI:
# 1. Go to Board API Keys in settings
# 2. Find the active key and revoke it
# 3. Create a new key
# 4. Update PAPERCLIP_TOKEN_FILE with the new token and restart the bridge
```

## Graceful Shutdown and stop_grace_period

The bridge handles SIGTERM with graceful shutdown in two phases:

1. **Poller/Sync/Runs/Health shutdown** (up to `SHUTDOWN_GRACE`): cancel polling tasks and stop accepting new work
2. **Outbox delivery** (up to `SHUTDOWN_GRACE`): finish in-flight Telegram message delivery

In the worst case, shutdown takes **up to 2×`SHUTDOWN_GRACE`** to complete. The `stop_grace_period` in Docker/Kubernetes **must be strictly greater than 2×`SHUTDOWN_GRACE`** to avoid SIGKILL:

```
stop_grace_period > 2 × SHUTDOWN_GRACE (+ safety margin recommended)
```

**Recommended values** (with defaults `SHUTDOWN_GRACE=10s`):
- Docker Compose: `stop_grace_period: 30s` (3× the grace)
- Kubernetes: `terminationGracePeriodSeconds: 30`

If you increase `SHUTDOWN_GRACE`, adjust `stop_grace_period` accordingly. Example with `SHUTDOWN_GRACE=15s`:
```yaml
environment:
  SHUTDOWN_GRACE: 15s
stop_grace_period: 35s  # > 2×15s
```

**Why two timeouts?** The first timeout ensures polling loops exit quickly; the second ensures in-flight Telegram messages are sent even if the poller is stuck. Both are bounded to prevent hanging indefinitely on network issues.

## Backup and Restore

State is stored in SQLite (`/state/bridge.db` and WAL files) in the mounted volume. To back up:

```bash
# Stop the bridge
docker compose down

# Create archive of the state volume
tar -czf backup-$(date +%Y%m%d).tar.gz -C /path/to/volumes bridge-state/

# Restart
docker compose up
```

To restore from a backup:

```bash
# Stop the bridge
docker compose down

# Remove current state
sudo rm -rf /path/to/volumes/bridge-state

# Extract from backup
sudo tar -xzf backup-DATE.tar.gz -C /path/to/volumes/

# Verify permissions (should be readable by uid 65532)
sudo chown -R 65532:65532 /path/to/volumes/bridge-state

# Restart
docker compose up
```

On restart, the bridge automatically:
- Recovers in-flight Telegram messages (entries return to pending)
- Reprocesses incomplete governance decisions
- Skips already-sent messages (by Telegram offset)

## Architecture and Design

See [`docs/`](docs/) for detailed documentation and design decisions:

- `TESTING.md` — test coverage matrix and how to run tests locally
- `paperclip-api.md` — Paperclip API contract, endpoints, and error handling
- `adr/` — architectural decision records (ADR-0001 through ADR-0008)

## Known Limitations (v1)

- **Deployment to user server not included**: This v1 release provides the bridge service and documentation. User deployment (setup, secrets, networks) is outside scope.
- **Single Telegram account**: The bridge serves one chat in one Telegram account. Multiple accounts or group chats require separate bridge instances.
- **Governance decisions are Telegram-only**: Decisions (approvals, confirmations) can only be made via Telegram buttons; the UI shows decisions as read-only.
- **No message history sync**: Only new messages after startup are bridged; history is not synced from Telegram or Paperclip.
- **Heartbeat file required**: Health check depends on `/state/health.json`. If `/state` is lost, health goes stale (but data recovers from DB).
- **Forms with >20 questions**: Question forms with more than 20 answerable questions cannot be completed via Telegram (API limit); users are directed to the Paperclip UI instead.

## Development

See [TESTING.md](docs/TESTING.md) for how to run tests locally and understand test coverage.

## License

This project is licensed under the Apache License 2.0 — see the [LICENSE](LICENSE) file for details.
