# Operational Runbook

This runbook covers common operational tasks for running the bridge in production.

## Table of Contents

- [Health Monitoring](#health-monitoring)
- [Key Rotation](#key-rotation)
- [Emergency Procedures](#emergency-procedures)
- [Troubleshooting](#troubleshooting)

## Health Monitoring

### Health Check

The bridge writes a health probe file to `/state/health.json`:

```json
{"ts": "2025-11-16T08:39:00Z", "status": "ok"}
```

**Docker healthcheck** (in compose example):

```yaml
healthcheck:
  test: ["CMD", "test", "-f", "/state/health.json", "-a", "-n", "$(find /state/health.json -mmin -5)"]
  interval: 30s
  timeout: 5s
  retries: 3
```

File is updated every 30 seconds. If not updated for more than 5 minutes (`HEALTH_MAX_STALE`), the health file is considered stale.

**Alert on**:
- Health file not present
- Health file older than `HEALTH_MAX_STALE`
- Container restarts
- Error logs (ERROR level or higher)

### Log Monitoring

Bridge logs to stdout in JSON format:

```json
{"timestamp":"2025-11-16T08:39:00Z","level":"INFO","message":"Attention poll: 5 items","component":"polling"}
```

Use structured logging collection (e.g., Fluent Bit, Datadog, Papertrail) to aggregate logs.

**Alerts**:
- `level: ERROR` messages (auth failure, database errors, network timeout after retries)
- High retry count (attempt > 3 for same message)
- Attention feed unavailable (consecutive errors)
- Rate limiting (HTTP 429)

### Metrics (v1 Baseline)

No built-in metrics export in v1. Track from logs:
- Message delivery latency (time between poll and sent)
- Retry count distribution
- Cache hit rate (agent/issue names)
- Attention feed response time

## Key Rotation

Rotating keys is a planned operational task to limit blast radius if a key is compromised.

### Paperclip Board Key Rotation

The board key grants the bridge read access to Paperclip API. Rotating it requires:

1. **Create new key in Paperclip UI**:
   - Sign in to Paperclip (admin account)
   - Navigate to Admin → Board Keys
   - Click "Create New Key"
   - Copy the new token (format: `pcp_...`)

2. **Stop the bridge** (optional, but safer):
   ```bash
   docker compose down
   ```

3. **Update the secret**:
   ```bash
   # Option A: Update secret file directly (if using docker secrets)
   echo "pcp_NEW_TOKEN_HERE" > /run/secrets/paperclip_token
   
   # Option B: Recreate Docker secret (if using named secrets)
   docker secret rm paperclip_token  # Old secret
   echo "pcp_NEW_TOKEN_HERE" | docker secret create paperclip_token -
   ```

4. **Revoke old key** (in Paperclip UI):
   - Navigate to Admin → Board Keys
   - Find the old key (by creation date or usage pattern)
   - Click "Revoke"

5. **Restart the bridge**:
   ```bash
   docker compose up -d
   ```

6. **Verify**:
   ```bash
   docker compose exec bridge /bridge check-config
   docker compose logs bridge | grep "Paperclip" | head -5
   ```

   Look for successful API calls; if you see 401 errors, the token was not updated correctly.

**Automation**: Schedule quarterly key rotation via cron or CI/CD webhook.

### Telegram Bot Token Rotation

If the bot token is compromised or rotated:

1. **In BotFather** (Telegram):
   - Send `/mybots` → select your bot
   - Select "Edit Bot" → "Edit Token"
   - BotFather will generate a new token; copy it
   - The old token becomes invalid immediately

2. **Update bridge secret**:
   ```bash
   echo "123456:ABCDEFGhIjklmnoPQRstUVWXYZabcDEf" > /run/secrets/telegram_bot_token
   ```

3. **Restart**:
   ```bash
   docker compose restart bridge
   ```

4. **Test**:
   ```bash
   docker compose exec bridge /bridge check-config
   ```

### Revoking and Recreating Keys

If a key is suspected leaked, revoke immediately:

1. **Revoke in origin system** (Paperclip or BotFather)
   - Paperclip: Admin → Board Keys → Revoke (bridge can no longer poll)
   - BotFather: `/mybots` → Edit Token (bot stops accepting updates)

2. **Create new key**

3. **Update bridge secret**

4. **Restart bridge**

5. **Audit logs** (check if the old key was used to make unexpected changes in Paperclip; if so, escalate to security team)

## Emergency Procedures

### Bridge Crash Loop

**Symptom**: Bridge container starts and stops repeatedly, or health check fails.

**Diagnosis**:
```bash
docker compose logs bridge | tail -50
```

Look for:
- Configuration error (missing env var)
- Database corruption (SQLite error)
- Network error (cannot reach Paperclip/Telegram proxy)
- Permissions error (cannot write to `/state`)

**Fix**:
- **Missing env var**: Add to `.env` file, restart.
- **Database corruption**: Back up, delete, let bridge rebuild state (see BACKUP_RESTORE.md).
- **Network error**: Check connectivity to Paperclip/Xray; verify firewall rules; check proxy configuration.
- **Permissions**: Ensure `/state` volume is writable by uid 65532: `chmod 700 /path/to/bridge-state; chown 65532:65532 /path/to/bridge-state`.

### Messages Not Delivering to Telegram

**Symptom**: Bridge enqueues messages but they never reach Telegram; `/status` shows "queue: N > 0".

**Diagnosis**:
```bash
docker compose exec bridge sqlite3 /state/bridge.db "SELECT id, issue_ref, status FROM outbox LIMIT 10;"
```

Check status field:
- `pending`: Waiting to send
- `sent`: Delivered to Telegram
- `failed`: Max retries exceeded; user notified

**If stuck as `pending`**:
```bash
docker compose logs bridge | grep -i "telegram\|retry\|429" | tail -20
```

Look for:
- 401/403: Bot token invalid (rotate token)
- 429: Rate limit (backoff; check for other outbox messages being hammered)
- Network timeout: Proxy unreachable (check connectivity to Xray)

**Recovery**:
- Check Xray proxy status
- Verify bot token is valid (restart with new token if needed)
- If stuck, manually update state: `UPDATE outbox SET status = 'pending' WHERE status = 'failed';` (re-enqueue)

### Paperclip Unreachable

**Symptom**: Health file stale; `/status` shows "Paperclip unavailable"; attention feed polling returns 0 items.

**Diagnosis**:
```bash
docker compose exec bridge ping -c 3 paperclip.internal
docker compose logs bridge | grep -i "paperclip\|attention" | tail -10
```

**Fix**:
- Check Paperclip service status (ask infrastructure team)
- Verify network connectivity (VPN, firewall rules, DNS resolution)
- Check bridge → Paperclip proxy route (should be `.no_proxy()`, no Xray)
- If transient: Wait for recovery; bridge will resume polling automatically

**Note**: Bridge continues to deliver enqueued Telegram messages even if Paperclip is down. The outbox will drain independently.

### Telegram Proxy (Xray) Unreachable

**Symptom**: Messages in outbox are stuck in `pending` or `failed`; logs show "connection refused" or "proxy unreachable".

**Diagnosis**:
```bash
docker compose logs bridge | grep -i "proxy\|socks\|telegram" | tail -20
```

**Fix**:
- Check Xray service status
- Verify bridge can reach proxy: `docker compose exec bridge curl -x socks5://xray:1080 https://api.telegram.org/getMe`
- Check firewall rules (bridge host → Xray host)
- Verify `TELEGRAM_PROXY_URL` env var is correct (should be `socks5h://xray:1080` or similar)

**Recovery**: When proxy is restored, bridge automatically resumes delivery (retry loop continues).

### Manual Message Retry

If a message is stuck and you want to force a retry:

```bash
# Connect to SQLite
docker compose exec bridge sqlite3 /state/bridge.db

# Manually reset stuck message to pending (triggers retry on next outbox flush)
UPDATE outbox SET status = 'pending', retry_count = 0 WHERE id = 123;

# Verify
SELECT id, issue_ref, status, retry_count FROM outbox WHERE id = 123;
```

The bridge will pick it up on the next outbox flush cycle.

## Troubleshooting

### Issue: Health File Not Updating

**Cause**: Bridge process is hung or stuck in infinite loop.

**Check**:
```bash
docker compose exec bridge ps aux | grep bridge
```

Look for multiple processes or zombie processes.

**Fix**:
```bash
docker compose restart bridge
docker compose logs bridge
```

### Issue: High Memory Usage

**Cause**: Cache growing unbounded (issue/agent name cache); large SQLite DB.

**Check**:
```bash
docker compose exec bridge sqlite3 /state/bridge.db ".tables"
docker compose exec bridge sqlite3 /state/bridge.db "SELECT COUNT(*) FROM deliveries;"
```

If `deliveries` table has millions of rows, consider archiving old records (v2 feature).

**Mitigation**: Restart bridge periodically (e.g., weekly), which clears in-memory cache.

### Issue: Duplicate Messages in Telegram

**Cause**: Outbox idempotency not working; message sent twice with different message IDs.

**Check**: 
```bash
docker logs <container> | grep -i "message_id\|duplicate\|409" | tail -20
```

Look for 409 (conflict) responses from Telegram, which indicate proper deduplication.

**If still seeing duplicates**:
1. Check SQLite recover logic (src/store/sqlite.rs:recover)
2. Verify message IDs are being set correctly
3. Check if health file is being corrupted (sign of crash)
4. Escalate to development team

### Issue: Bridge Not Processing Telegram Updates

**Cause**: Long polling stuck or updates not being processed.

**Check**:
```bash
docker compose logs bridge | grep -i "getUpdates\|update" | tail -10
```

**Fix**:
- Verify bot token is valid (try `/status` command manually in Telegram)
- Check network connectivity to Telegram API (through proxy)
- Restart bridge: `docker compose restart bridge`

### Issue: Governance Decision Not Reflected in Paperclip

**Cause**: Bridge sent decision to Paperclip, but Paperclip rejected or didn't apply it.

**Check**:
```bash
docker compose logs bridge | grep "issues.*interactions\|approve\|reject" | tail -10
```

Look for:
- 404: Issue or interaction ID not found
- 409: Already resolved elsewhere (someone else approved first)
- 422: Invalid decision (e.g., wrong approval state)

**Fix**:
- If 409: This is OK; Paperclip already resolved it. Check UI.
- If 404: Sync issue; check if Paperclip cache is in sync.
- If 422: Check Paperclip logs for validation error.
- If timeout: Check Paperclip availability; may be slow.

## Maintenance Checklist (Monthly)

- [ ] Review bridge logs for errors and warnings
- [ ] Check health file update frequency (should be every 30s)
- [ ] Test backup and recovery procedure (restore to staging)
- [ ] Verify token rotation capability (rehearse, don't actually rotate)
- [ ] Check disk space on `/state` volume
- [ ] Review Paperclip audit logs for bridge activity (anomalies)

## Maintenance Checklist (Quarterly)

- [ ] Rotate Paperclip board key (see Key Rotation section)
- [ ] Audit bot token usage (check for leaked credentials in logs)
- [ ] Test disaster recovery (full bridge rebuild from backup)
- [ ] Review and update this runbook
