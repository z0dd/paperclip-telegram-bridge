#!/usr/bin/env bash
set -euo pipefail

# Docker smoke test для paperclip-telegram-bridge
# Проверяет:
# - non-root user (uid ≠ 0)
# - read-only filesystem (Config.ReadonlyRootfs)
# - no exposed ports
# - healthcheck (healthy right after start, unhealthy once upstreams stay silent)
# - uid of the running process on the host
# - SIGTERM graceful shutdown (exit code 0)
# - no credentials in logs

IMAGE_NAME="paperclip-telegram-bridge:test"
CONTAINER_ID=""
SECRETS_DIR=""
STATE_VOLUME=""

fail() {
    echo "❌ $1"
    exit 1
}

cleanup() {
    if [ -n "$CONTAINER_ID" ]; then
        docker rm -f "$CONTAINER_ID" 2>/dev/null || true
    fi
    if [ -n "$STATE_VOLUME" ]; then
        docker volume rm "$STATE_VOLUME" 2>/dev/null || true
    fi
    if [ -n "$SECRETS_DIR" ]; then
        rm -rf "$SECRETS_DIR"
    fi
}
trap cleanup EXIT

echo "=== Docker Smoke Test ==="

# Prepare fixtures: dummy secrets in a directory (no real tokens/IPs)
SECRETS_DIR=$(mktemp -d)
echo "000000:TEST-DUMMY-TOKEN" > "$SECRETS_DIR/tg"
echo "pcp_dummy" > "$SECRETS_DIR/pc"
chmod 755 "$SECRETS_DIR"
chmod 644 "$SECRETS_DIR"/*
echo "Created dummy secrets in $SECRETS_DIR"

# Create a named volume for /state
STATE_VOLUME="smoke-test-state-$(date +%s)"
docker volume create "$STATE_VOLUME" > /dev/null
echo "Created named volume: $STATE_VOLUME"

# Test 1: Check non-root user in image config
echo ""
echo "Test 1: Checking non-root user in image config..."
USER_ID=$(docker inspect "$IMAGE_NAME" | jq -r '.[0].Config.User // empty')
if [ -z "$USER_ID" ]; then
    fail "User not set in Dockerfile"
fi
if [ "$USER_ID" = "0" ] || [ "$USER_ID" = "root" ]; then
    fail "Container runs as root (uid=$USER_ID)"
fi
echo "✓ Config.User = $USER_ID (non-root)"

# Test 2: Check no exposed ports
echo ""
echo "Test 2: Checking no exposed ports..."
EXPOSED=$(docker inspect "$IMAGE_NAME" | jq -r '.[0].Config.ExposedPorts // empty')
if [ "$EXPOSED" != "null" ] && [ -n "$EXPOSED" ]; then
    fail "Exposed ports found in Config.ExposedPorts: $EXPOSED"
fi
echo "✓ Config.ExposedPorts = null (no exposed ports)"

COMMON_ENV=(
    -e PAPERCLIP_URL="http://127.0.0.1:9"
    -e PAPERCLIP_COMPANY_IDS="test-company"
    -e PAPERCLIP_TOKEN_FILE="/secrets/pc"
    -e TELEGRAM_BOT_TOKEN_FILE="/secrets/tg"
    -e TELEGRAM_CHAT_ID="111"
    -e TELEGRAM_USER_ID="111"
    -e TELEGRAM_PROXY_URL="http://127.0.0.1:9"
    -e STATE_PATH="/state/bridge.db"
    -e HEALTH_FILE="/state/health.json"
    -e SHUTDOWN_GRACE="10s"
    -e LOG_FORMAT="text"
)

# Test 3: check-config with the same environment
echo ""
echo "Test 3: bridge check-config..."
CC_OUT=$(docker run --rm --read-only --cap-drop ALL --security-opt no-new-privileges \
    -v "$SECRETS_DIR:/secrets:ro" -v "$STATE_VOLUME:/state" \
    "${COMMON_ENV[@]}" "$IMAGE_NAME" check-config 2>&1) \
    || fail "check-config failed: $CC_OUT"
if echo "$CC_OUT" | grep -qE "TEST-DUMMY-TOKEN|pcp_dummy"; then
    fail "check-config output leaks a secret"
fi
echo "✓ check-config exit 0, no secrets in output"

# Test 4: start the service with the default entrypoint
echo ""
echo "Test 4: starting container (read-only rootfs, closed-port upstreams)..."
CONTAINER_ID=$(docker run -d \
    --read-only \
    --tmpfs /tmp \
    -v "$STATE_VOLUME:/state" \
    --cap-drop ALL \
    --security-opt no-new-privileges \
    -v "$SECRETS_DIR:/secrets:ro" \
    "${COMMON_ENV[@]}" \
    "$IMAGE_NAME" 2>&1) || fail "docker run failed: $CONTAINER_ID"
echo "Container started: $CONTAINER_ID"
START_EPOCH=$(date +%s)

# Test 5: healthy right after start (heartbeat counts from process start)
echo ""
echo "Test 5: healthcheck right after start must be healthy..."
HC_OUT=""
HEALTHY=0
for _ in 1 2 3 4 5 6 7 8 9 10; do
    if HC_OUT=$(docker exec "$CONTAINER_ID" /usr/local/bin/bridge healthcheck 2>&1); then
        HEALTHY=1
        break
    fi
    sleep 1
done
if [ "$HEALTHY" != "1" ]; then
    fail "healthcheck not healthy after start: $HC_OUT; logs: $(docker logs "$CONTAINER_ID" 2>&1)"
fi
echo "✓ healthcheck exit 0: $HC_OUT"

# Let the service run ~5s before checking state
ELAPSED=$(( $(date +%s) - START_EPOCH ))
if [ "$ELAPSED" -lt 5 ]; then
    sleep $(( 5 - ELAPSED ))
fi
RUNNING=$(docker inspect "$CONTAINER_ID" | jq -r '.[0].State.Running')
if [ "$RUNNING" != "true" ]; then
    fail "Container not running (State.Running=$RUNNING). Logs: $(docker logs "$CONTAINER_ID" 2>&1)"
fi
echo "✓ Container is running (State.Running = true)"
RESTART_COUNT=$(docker inspect "$CONTAINER_ID" | jq -r '.[0].RestartCount')
if [ "$RESTART_COUNT" != "0" ]; then
    fail "Container restarted ($RESTART_COUNT times)"
fi
echo "✓ RestartCount = 0"

# Test 6: read-only rootfs
echo ""
echo "Test 6: read-only root filesystem..."
READONLY=$(docker inspect "$CONTAINER_ID" | jq -r '.[0].HostConfig.ReadonlyRootfs')
if [ "$READONLY" != "true" ]; then
    fail "HostConfig.ReadonlyRootfs = $READONLY (expected true)"
fi
echo "✓ HostConfig.ReadonlyRootfs = true (service runs, SQLite/health file live in /state)"

# Test 7: no published ports
echo ""
echo "Test 7: no published ports..."
PORTS=$(docker port "$CONTAINER_ID")
if [ -n "$PORTS" ]; then
    fail "Published ports found: $PORTS"
fi
echo "✓ docker port is empty"

# Test 8: uid of the running process, seen from the host
echo ""
echo "Test 8: uid of the running process..."
PID=$(docker inspect -f '{{.State.Pid}}' "$CONTAINER_ID")
if [ -z "$PID" ] || [ "$PID" = "0" ]; then
    fail "cannot get container PID"
fi
PROC_UID=$(ps -o uid= -p "$PID" | tr -d ' ')
if [ -z "$PROC_UID" ]; then
    fail "ps returned no uid for pid $PID"
fi
if [ "$PROC_UID" = "0" ]; then
    fail "process runs as uid 0"
fi
echo "✓ pid $PID runs as uid $PROC_UID (non-root)"

# Test 9: degraded -> unhealthy (upstreams on a closed port never succeed)
echo ""
echo "Test 9: healthcheck must be unhealthy when upstreams stay silent..."
ELAPSED=$(( $(date +%s) - START_EPOCH ))
if [ "$ELAPSED" -lt 3 ]; then
    sleep $(( 3 - ELAPSED ))
fi
set +e
DEG_OUT=$(docker exec -e HEALTH_MAX_STALE=1s "$CONTAINER_ID" /usr/local/bin/bridge healthcheck 2>&1)
DEG_CODE=$?
set -e
if [ "$DEG_CODE" = "0" ]; then
    fail "healthcheck returned 0 with HEALTH_MAX_STALE=1s: $DEG_OUT"
fi
if ! echo "$DEG_OUT" | grep -q "no successful"; then
    fail "unexpected unhealthy reason (exit $DEG_CODE): $DEG_OUT"
fi
echo "✓ healthcheck exit $DEG_CODE: $DEG_OUT"

# Test 10: credentials not in logs
echo ""
echo "Test 10: no credentials in logs..."
LOGS=$(docker logs "$CONTAINER_ID" 2>&1)
if echo "$LOGS" | grep -qF "TEST-DUMMY-TOKEN"; then
    fail "Telegram token found in logs"
fi
if echo "$LOGS" | grep -qF "pcp_dummy"; then
    fail "Paperclip token found in logs"
fi
echo "✓ no credentials in logs"

# Test 11: SIGTERM -> exit 0 within SHUTDOWN_GRACE
echo ""
echo "Test 11: SIGTERM graceful shutdown..."
STOP_START=$(date +%s.%N)
docker stop -t 15 "$CONTAINER_ID" > /dev/null || fail "docker stop failed"
STOP_END=$(date +%s.%N)
STOP_SECS=$(awk -v a="$STOP_START" -v b="$STOP_END" 'BEGIN { printf "%.2f", b - a }')
EXIT_CODE=$(docker inspect "$CONTAINER_ID" | jq -r '.[0].State.ExitCode')
if [ "$EXIT_CODE" != "0" ]; then
    fail "Container exited with code $EXIT_CODE (expected 0)"
fi
if ! awk -v s="$STOP_SECS" 'BEGIN { exit !(s < 12) }'; then
    fail "shutdown took ${STOP_SECS}s (limit 12s)"
fi
echo "✓ SIGTERM: exit code 0 in ${STOP_SECS}s"

echo ""
echo "=== All smoke tests passed ✓ ==="
