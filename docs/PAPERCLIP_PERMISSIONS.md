# Required Paperclip API Permissions

This document lists the Paperclip API endpoints that the bridge requires access to, and the corresponding permissions needed for the board key.

## Endpoints Summary

| Method | Endpoint | Purpose | Permission |
|--------|----------|---------|-----------|
| GET | `/api/health` | Health check | None (public) |
| GET | `/api/companies/{cid}/attention` | Poll incoming questions/governance items | `read:companies` |
| GET | `/api/issues/{id}/interactions` | Fetch interactions for issue | `read:issues` |
| POST | `/api/issues/{id}/interactions/{iid}/accept` | Approve governance decision | `write:issues` |
| POST | `/api/issues/{id}/interactions/{iid}/reject` | Reject governance decision | `write:issues` |
| GET | `/api/issues/{id}/approvals` | Fetch approvals (future use) | `read:issues` |
| POST | `/api/approvals/{id}/approve` | Approve in approvals workflow | `write:issues` |
| POST | `/api/approvals/{id}/reject` | Reject in approvals workflow | `write:issues` |
| GET | `/api/companies/{cid}/issues` | Fetch issues by status | `read:issues` |
| GET | `/api/companies/{cid}/heartbeat-runs` | Poll recent run failures | `read:runs` |
| GET | `/api/companies/{cid}/live-runs` | Fetch live run IDs | `read:runs` |
| GET | `/api/companies/{cid}/agents` | Fetch agent metadata (for cache) | `read:companies` |
| POST | `/api/cli-auth/revoke-current` | Revoke the current board key | None (token-scoped) |

## Recommended Board Key Permissions

Create a board key with the following scopes:

```json
{
  "scopes": [
    "read:companies",
    "read:issues",
    "read:runs",
    "write:issues"
  ]
}
```

This grants:
- **read:companies**: Access to attention feed and agent metadata
- **read:issues**: View interactions and approvals
- **read:runs**: Monitor run statuses and failures
- **write:issues**: Send approvals and rejections

**Excluded** (for safety):
- `write:companies`: Cannot create/delete companies
- `delete:*`: Cannot delete anything
- `admin:*`: No administrative access

## API Response Codes

The bridge handles the following Paperclip API responses:

| Code | Meaning | Bridge Behavior |
|------|---------|-----------------|
| 200 | Success | Process response, mark delivery as sent |
| 400 | Bad request (e.g., invalid issue ID) | Log error, skip message, don't retry |
| 401 | Unauthorized (invalid token) | Log auth error, retry with backoff |
| 403 | Forbidden (token lacks permission) | Log permission error, retry with backoff |
| 404 | Not found (issue/interaction doesn't exist) | Log error, skip message, don't retry |
| 409 | Conflict (interaction already resolved by another client) | Mark as sent (idempotent), don't retry |
| 429 | Rate limited | Retry with exponential backoff + Retry-After header |
| 5xx | Server error | Retry with exponential backoff |

## Rate Limiting

Paperclip may enforce rate limits per token or globally. The bridge respects 429 responses:

- **Rate-Limit-Limit**: Total requests allowed per window
- **Rate-Limit-Remaining**: Requests remaining
- **Retry-After**: Seconds to wait before retrying (if present, used instead of backoff)

Configuration:
- `RETRY_BASE_DELAY=1s`: Initial delay
- `RETRY_MAX_DELAY=60s`: Cap on backoff
- `RETRY_MAX_ATTEMPTS=6`: Total retry attempts before failing

## Error Handling Examples

### Example 1: Valid Approval

```bash
curl -X POST \
  -H "Authorization: Bearer pcp_..." \
  -H "Content-Type: application/json" \
  -d '{}' \
  https://paperclip.internal/api/issues/iss_123/interactions/int_456/accept
```

**Response 200**:
```json
{"id": "int_456", "status": "accepted", "resolved_at": "2025-11-16T08:39:00Z"}
```

Bridge updates outbox: `status = sent`.

### Example 2: Already Resolved Elsewhere

```bash
# Same call, but someone else approved first
```

**Response 409**:
```json
{"error": "interaction_already_resolved", "code": "interaction_already_resolved"}
```

Bridge treats 409 as idempotent success: `status = sent` (no retry).

### Example 3: Permission Denied

```bash
# Token lacks write:issues permission
```

**Response 403**:
```json
{"error": "insufficient_permissions", "code": "insufficient_scopes"}
```

Bridge logs error and retries with backoff. **Fix**: Add `write:issues` to token scopes.

### Example 4: Rate Limited

```bash
# Too many requests in short time
```

**Response 429**:
```
Retry-After: 5
```

Bridge backs off 5 seconds, then retries. If Retry-After is missing, uses exponential backoff.

## Health Check

The `/api/health` endpoint is public (no auth required) and used for liveness probes:

```bash
curl https://paperclip.internal/api/health
```

**Response 200**:
```json
{"status": "ok", "timestamp": "2025-11-16T08:39:00Z"}
```

Bridge uses this to determine if Paperclip is reachable before attempting other operations.

## Audit and Logging

All bridge requests are logged in Paperclip's audit trail:

- Timestamp
- Board key ID
- Action (read/write)
- Resource (issue, company, etc.)
- Result (success/failure)

Review quarterly:
```bash
# In Paperclip admin UI:
Admin → Audit Log → Filter by board_key_id
```

Look for:
- Unexpected interactions (should only be approvals/rejections for Telegram questions)
- Failures (might indicate permission issues)
- High frequency (might indicate retry storm)

## Future Enhancements (v2+)

- **Granular approval scope**: Token can only approve specific issue types or patterns
- **Time-limited tokens**: Tokens that expire after N days (encourages rotation)
- **Webhook delivery**: Instead of polling, receive push notifications from Paperclip (reduce latency)
- **Request signing**: HMAC-SHA256 signatures for webhook authenticity

## Support

If you encounter unexpected responses:

1. Check Paperclip logs: `Admin → Logs → Filter by IP (bridge host)`
2. Verify token scopes: `Admin → Board Keys → Select Token → View Scopes`
3. Test manually: `curl -H "Authorization: Bearer pcp_..." https://paperclip.internal/api/health`
4. Check bridge logs: `docker compose logs bridge | grep -i "401\|403\|403"`
5. Contact Paperclip admin or support

## Security Notes

- **Never log the full token**: Bridge logs only the first 10 characters (`pcp_XXXXXXX[REDACTED]`)
- **Rotate tokens quarterly**: See [RUNBOOK.md](RUNBOOK.md) for rotation procedure
- **Use separate token per environment**: Don't reuse prod token in staging
- **Monitor for unexpected requests**: Check audit log for unusual access patterns
