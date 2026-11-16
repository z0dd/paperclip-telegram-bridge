# Threat Model and Security Analysis

## Scope

This v1 release runs the bridge in a controlled environment (trusted network, private Paperclip, Xray proxy, secrets in Docker secrets). The threat model assumes:

- **Trusted container host**: Docker daemon and volumes are not compromised.
- **Trusted networks**: Paperclip is on a private network (not internet-facing).
- **Trusted proxy**: Xray proxy is properly configured and not compromised.
- **Secrets handling**: Tokens are provided via Docker secrets (read-only files), not environment variables.
- **Telegram account**: User controls the bot token; 2FA is recommended.

## STRIDE Analysis

### Spoofing

**Threat**: Attacker impersonates the bridge, Paperclip, or Telegram API.

- **Bridge ↔ Paperclip**: Token in `PAPERCLIP_TOKEN_FILE` is opaque (user controls secret file). Bearer auth over HTTPS (to private Paperclip). **Mitigation**: Store token in Docker secret; don't expose in env.
- **Bridge ↔ Telegram**: Bot token in `TELEGRAM_BOT_TOKEN_FILE`; Telegram API enforces token ownership. **Mitigation**: Protect token file; use 2FA to prevent account takeover.
- **User ↔ Telegram**: Telegram client-server connection uses TLS. **Mitigation**: Recommended 2FA in Telegram settings.

**Residual risk**: If host is compromised, attacker can read secrets; if Xray config is broken, Telegram token may leak.

### Tampering

**Threat**: Attacker modifies messages in transit or state on disk.

- **Bridge → Telegram**: TLS to proxy, then to Telegram API (authenticated). **Mitigation**: Proxy must enforce TLS; bridge validates Telegram responses for errors.
- **State (SQLite)**: Stored in `/state` volume (not encrypted at rest). **Mitigation**: Use encrypted storage (e.g., dm-crypt) if required by policy; bridge runs in immutable container (no direct shell access).
- **Approval workflow**: Bridge sends decision to Paperclip; Paperclip validates business rules. Bridge does not make final decisions (user does in Telegram UI).

**Residual risk**: Attacker with host access can modify `/state/bridge.db` offline; Paperclip must validate all decisions server-side.

### Repudiation

**Threat**: Attacker denies approving or rejecting a decision.

- **Audit trail**: Bridge logs all interactions (timestamp, user, decision, Paperclip response). Paperclip maintains canonical audit log.
- **Message traceability**: Telegram message IDs and Paperclip issue IDs are linked in bridge logs.

**Mitigation**: All decisions logged to stdout (captured by docker logs); Paperclip enforces server-side validation.

### Information Disclosure

**Threat**: Sensitive data (tokens, approval decisions, arguments) is exposed.

- **Secrets in env**: Never; always in files.
- **Secrets in logs**: Redaction built-in (tokens never logged). **Mitigation**: Review logs for sensitive content; use `GOVERNED_SHOW_ARGUMENTS=false` to hide function arguments in Telegram previews.
- **Secrets in Docker image**: Build-time only (Dockerfile has no embedded secrets); final image is distroless (minimal surface).
- **Secrets in state**: SQLite contains message content (encrypted at Paperclip level if required); bridge DB is not encrypted but can be.
- **Secrets in Telegram**: Bot token is private (user controls); approval decisions are encrypted by Telegram client-server TLS.

**Residual risk**: Host compromise leaks all secrets; unencrypted SQLite DB can be read if volume is extracted.

### Denial of Service

**Threat**: Attacker prevents bridge from functioning.

- **Resource exhaustion**: Container is limited by Docker resource constraints (memory, CPU). Polling loops do not spawn unbounded tasks.
- **Infinite loops**: Attention feed has max page limit (ATTENTION_MAX_PAGES=10); pagination cursor handles loops.
- **Telegram rate limit**: Bridge respects 429 (rate-limit) response with backoff; after max attempts, notifies user and skips.
- **Paperclip rate limit**: Retries with exponential backoff; no retry storm.
- **Crash loop**: Graceful shutdown + health file; Docker restart policy prevents crash loops.

**Mitigation**: Use resource limits in Docker Compose; monitor health file; set restart policy to `unless-stopped`.

**Residual risk**: If Xray proxy is unavailable, bridge cannot reach Telegram (accepted per design; bridge remains available for polling Paperclip).

### Elevation of Privilege

**Threat**: Attacker gains unauthorized access to Paperclip or Telegram via bridge.

- **Paperclip API**: Bridge operates with a single board token (admin-like). **Mitigation**: Least-privilege token (ideally: read attention, write interactions, read approvals, etc., no delete); store in Docker secret.
- **Telegram bot**: Bridge controls the bot. User cannot escalate via Telegram (user can only approve/reject within Paperclip's permissions).
- **Container process**: Runs as uid 65532 (non-root); no setuid binaries.

**Mitigation**: Use a dedicated, scoped board token; enable 2FA to prevent Telegram account takeover; do not run as root.

**Residual risk**: If board token is overly permissive or compromised, attacker can impersonate bridge in Paperclip.

## Mitigation Strategies

1. **Secrets Management**:
   - Use Docker secrets (mounted as read-only files, uid 65532)
   - Never pass secrets via environment variables
   - Rotate board token regularly (see RUNBOOK.md)
   - Enable 2FA in Telegram account

2. **Network Isolation**:
   - Keep Paperclip on private network (no internet routing)
   - Force all Telegram traffic through authenticated proxy (Xray)
   - Use VPN or firewall rules to restrict bridge host access

3. **Audit and Monitoring**:
   - Enable Docker log collection (forward to centralized logging)
   - Monitor health file for staleness
   - Alert on high retry counts or Telegram errors

4. **Data Protection**:
   - Use encrypted storage for `/state` volume (optional, policy-dependent)
   - Run SQLite with encryption (future enhancement)
   - Backups: store encrypted offsite; restrict access

5. **Incident Response**:
   - If bot token leaked: Revoke in BotFather (`/revoke`), issue new token
   - If board token leaked: Revoke in Paperclip, issue new token, rotate bridge secret
   - If Xray proxy compromised: Pause bridge, rotate proxy credentials, validate no messages leaked

## Residual Risks (Accepted in v1)

| Risk | Severity | Mitigation | Acceptance |
|------|----------|-----------|-----------|
| Host compromise leaks all secrets | High | Secure host, detect intrusions, isolate network | Operator responsibility |
| SQLite DB not encrypted at rest | Medium | Use dm-crypt or EBS encryption if required by policy | Optional future enhancement |
| Overly permissive board token | Medium | Use least-privilege token; rotate regularly | Board token scope defined by Paperclip admin |
| Telegram account takeover (2FA off) | Medium | Enable 2FA in Telegram settings | User responsibility |
| Xray proxy misconfiguration | Medium | Validate proxy rules; test with tcpdump | Operator responsibility |
| User shares Telegram chat (side-channel) | Low | Not in scope; trust model includes trusted user | Accepted |

## Security Best Practices for Operators

1. **Initial Setup**:
   - Create a dedicated Paperclip board key (don't reuse admin token)
   - Create bot via BotFather; enable 2FA in Telegram
   - Run bridge on isolated host; use firewall rules to restrict access

2. **Ongoing Operations**:
   - Rotate board token quarterly (see RUNBOOK.md)
   - Monitor bridge logs for auth errors or unusual retry patterns
   - Keep Docker and base OS patched
   - Use encrypted backups; test restore procedures quarterly

3. **Incident Response**:
   - Document procedure for rotating both tokens (board key and bot token)
   - Have backup communication channel if primary is compromised
   - Test recovery from SQLite backup before need arises
