# Backup and Restore Procedures

State is stored in SQLite (`/state/bridge.db` and WAL files). This guide covers backing up and restoring bridge state.

## Backup Procedures

### Strategy 1: Cold Backup (Safest)

**When to use**: Scheduled maintenance, quarterly backup verification, or when bridge is not critical.

**Steps**:

1. Stop the bridge:
   ```bash
   docker compose down
   ```

2. Create tar archive (preserves permissions for uid 65532):
   ```bash
   tar -czf backup-$(date +%Y-%m-%d).tar.gz -C /path/to bridge-state/
   ```

3. Copy to secure location (encrypted offsite storage):
   ```bash
   scp backup-*.tar.gz backup_server:/backups/bridge/
   # Or upload to S3
   aws s3 cp backup-*.tar.gz s3://my-backups/bridge/
   ```

4. Restart:
   ```bash
   docker compose up -d
   ```

**Pros**: No messages lost; consistent snapshot.
**Cons**: Brief downtime (acceptable if scheduled).

### Strategy 2: Online Backup (Minimal Downtime)

**When to use**: Daily automated backups; bridge is in production.

**Steps**:

1. While bridge is running, use SQLite's online backup:
   ```bash
   docker compose exec bridge sqlite3 /state/bridge.db \
     "PRAGMA integrity_check; VACUUM INTO '/tmp/backup.db';"
   ```

2. Copy the backup file:
   ```bash
   docker compose exec bridge tar -czf /tmp/backup.tar.gz -C /tmp backup.db
   docker cp $(docker compose ps -q bridge):/tmp/backup.tar.gz backup-online-$(date +%Y-%m-%d).tar.gz
   ```

3. Verify integrity:
   ```bash
   tar -tzf backup-online-*.tar.gz | head -5  # Check archive is valid
   ```

4. Upload to safe storage

**Pros**: No downtime; can be automated.
**Cons**: Requires Paperclip and Telegram to be available; if bridge crashes during VACUUM, may need manual recovery.

### Strategy 3: WAL Archive (Point-in-Time Recovery)

**When to use**: Forensic debugging; understanding when a message was sent/failed.

1. Copy WAL files:
   ```bash
   tar -czf wal-archive-$(date +%s).tar.gz /path/to/bridge-state/bridge.db-wal /path/to/bridge-state/bridge.db-shm
   ```

2. Upload alongside main backup

**Pros**: Can replay transactions up to any point in time.
**Cons**: Complex recovery; rarely needed in v1.

## Restore Procedures

### Complete State Restore (Full Recovery)

**When to use**: After data loss, corruption, or disaster recovery.

**Scenario**: Bridge lost `/state` volume; need to recover from backup.

**Steps**:

1. Stop the bridge:
   ```bash
   docker compose down
   ```

2. Back up current corrupted state (for investigation):
   ```bash
   mv /path/to/bridge-state /path/to/bridge-state.corrupted
   mkdir -p /path/to/bridge-state
   ```

3. Extract backup:
   ```bash
   tar -xzf backup-DATE.tar.gz -C /path/to/
   ```

4. Fix permissions (important for non-root uid 65532):
   ```bash
   chown -R 65532:65532 /path/to/bridge-state
   chmod 700 /path/to/bridge-state
   ```

5. Restart bridge:
   ```bash
   docker compose up -d
   ```

6. Verify:
   ```bash
   docker compose logs bridge | head -20  # Check no errors
   docker compose exec bridge /bridge check-config  # Validate config
   docker compose exec bridge sqlite3 /state/bridge.db "SELECT COUNT(*) FROM outbox;"  # Check DB
   ```

### Partial Recovery (Single Message)

**When to use**: One message is corrupted; need to re-send.

**Steps**:

1. Find the message in outbox:
   ```bash
   docker compose exec bridge sqlite3 /state/bridge.db \
     "SELECT id, issue_ref, status FROM outbox WHERE issue_ref = 'PET-123';"
   ```

2. Reset to pending (to trigger retry):
   ```bash
   docker compose exec bridge sqlite3 /state/bridge.db \
     "UPDATE outbox SET status = 'pending', retry_count = 0 WHERE id = 42;"
   ```

3. Bridge will re-send on next outbox flush cycle (within seconds)

### Recovery After Crash (Automatic)

If bridge crashes mid-delivery:

1. On restart, bridge runs `recover()` in `src/store/sqlite.rs`
2. All `in_flight` entries reset to `pending`
3. Messages are re-sent to Telegram (which returns 400 for duplicates, handled gracefully)
4. No data loss; idempotency ensures no duplication

**Manual check**:
```bash
docker compose exec bridge sqlite3 /state/bridge.db \
  "SELECT COUNT(*) FROM outbox WHERE status = 'in_flight';"
# Should be 0 after startup
```

## Backup Retention Policy (Recommended)

| Frequency | Retention | Storage |
|-----------|-----------|---------|
| Daily (cold) | 7 days | Local NAS or S3 |
| Weekly (cold) | 4 weeks | Offsite (S3 Glacier, etc.) |
| Monthly (full) | 12 months | Offsite encrypted |
| Ad-hoc (before key rotation) | 30 days | Local + offsite |

## Testing Restore (Quarterly)

To ensure backups are usable, test restoration quarterly:

1. **Staging environment**: Spin up a test bridge container
2. **Restore from backup**: Follow "Complete State Restore" above
3. **Verify data**:
   ```bash
   docker compose exec bridge sqlite3 /state/bridge.db \
     "SELECT COUNT(*) FROM outbox; SELECT COUNT(*) FROM deliveries;"
   ```
4. **Test operations**: Send a test message via `/status` command
5. **Document**: Record restore time and any issues

## Disaster Recovery Timeline

| Scenario | Recovery Time | Data Loss |
|----------|---------------|-----------|
| Backup available, crash | < 5 min | None (idempotent) |
| Backup available, corruption | < 10 min | None |
| No backup, DB corruption | 30+ min | Potential (depends on corruption) |
| Host failure, no backup | > 1 hour | Complete (all enqueued messages) |

**Recommendation**: Keep at least one backup at all times. Test quarterly.

## Advanced: Point-in-Time Recovery

If you need to understand when a message was sent/failed:

1. Restore backup to staging
2. Compare SQLite transactions using `sqlite3` CLI:
   ```bash
   sqlite3 /path/to/backup.db "SELECT * FROM outbox WHERE issue_ref = 'PET-123' ORDER BY created_at DESC LIMIT 5;"
   ```
3. Compare with WAL files (if available):
   ```bash
   hexdump -C bridge.db-wal | head -50  # Shows transaction log
   ```

This is rarely needed; escalate to development team if forensics are required.

## Encryption and Security

**Current state** (v1):
- SQLite DB is not encrypted at rest
- Backups should be stored with encryption (e.g., S3 server-side encryption, encrypted USB drives)
- Secrets (tokens) are in `/state/health.json` (not encrypted)

**Recommendations**:
- Use encrypted storage for backup media (dm-crypt, LUKS, or cloud provider encryption)
- Restrict backup access (SSH keys, IAM roles, not public URLs)
- Consider encrypting `/state` volume with dm-crypt for added security:
  ```bash
  # One-time setup
  dd if=/dev/zero of=/path/to/encrypted-state.img bs=1M count=1000
  sudo cryptsetup luksFormat /path/to/encrypted-state.img
  sudo cryptsetup luksOpen /path/to/encrypted-state.img bridge-state
  sudo mkfs.ext4 /dev/mapper/bridge-state
  sudo mount /dev/mapper/bridge-state /path/to/bridge-state
  ```

## Troubleshooting Restore

| Issue | Cause | Fix |
|-------|-------|-----|
| Permission denied during restore | Wrong uid/gid | Run `chown 65532:65532` on restored files |
| SQLite database is locked | WAL lock file present | Delete `.db-wal` and `.db-shm` files, retry |
| Corrupt database after restore | Archive corrupted | Verify checksum; try different backup |
| Bridge won't start after restore | Config mismatch | Verify `/state/health.json` exists; check logs |
| Old messages re-sent after restore | Expected behavior | Idempotency handles duplicates; this is OK |

See [RUNBOOK.md](RUNBOOK.md) for other troubleshooting guidance.
