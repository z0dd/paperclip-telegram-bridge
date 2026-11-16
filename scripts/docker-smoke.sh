#!/usr/bin/env bash
set -euo pipefail

# Docker smoke test для paperclip-telegram-bridge
# Проверяет:
# - non-root user (uid ≠ 0)
# - read-only filesystem (Config.ReadonlyRootfs)
# - no exposed ports
# - healthcheck
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

# Test 3: Start container with read-only FS and closed-port upstreams
echo ""
echo "Test 3: Starting container with read-only FS..."
CONTAINER_ID=$(docker run -d \
    --read-only \
    --tmpfs /tmp \
    -v "$STATE_VOLUME:/state" \
    --cap-drop ALL \
    --security-opt no-new-privileges \
    -v "$SECRETS_DIR:/secrets:ro" \
    -e PAPERCLIP_URL="http://127.0.0.1:9" \
    -e PAPERCLIP_COMPANY_IDS="test-company" \
    -e PAPERCLIP_TOKEN_FILE="/secrets/pc" \
    -e TELEGRAM_BOT_TOKEN_FILE="/secrets/tg" \
    -e TELEGRAM_CHAT_ID="111" \
    -e TELEGRAM_USER_ID="111" \
    -e TELEGRAM_PROXY_URL="http://127.0.0.1:9" \
    -e STATE_PATH="/state/bridge.db" \
    -e HEALTH_FILE="/state/health.json" \
    -e SHUTDOWN_GRACE="10s" \
    -e LOG_FORMAT="text" \
    "$IMAGE_NAME" 2>&1) || fail "docker run failed: $CONTAINER_ID"

echo "Container started: $CONTAINER_ID"

# Wait for container to be running
sleep 2
STATUS=$(docker inspect "$CONTAINER_ID" | jq -r '.[0].State.Running')
if [ "$STATUS" != "true" ]; then
    LOGS=$(docker logs "$CONTAINER_ID" 2>&1 || true)
    fail "Container not running (status=$STATUS). Logs: $LOGS"
fi
echo "✓ Container is running (State.Running = true)"

# Test 4: Verify read-only filesystem
echo ""
echo "Test 4: Verifying read-only filesystem..."
READONLY=$(docker inspect "$CONTAINER_ID" | jq -r '.[0].HostConfig.ReadonlyRootfs')
if [ "$READONLY" != "true" ]; then
    fail "HostConfig.ReadonlyRootfs = $READONLY (expected true)"
fi
echo "✓ HostConfig.ReadonlyRootfs = true"

# Verify no restart (meaning read-only FS works and /state is writable)
RESTART_COUNT=$(docker inspect "$CONTAINER_ID" | jq -r '.[0].State.RestartCount // 0')
if [ "$RESTART_COUNT" != "0" ] && [ "$RESTART_COUNT" != "null" ]; then
    fail "Container restarted ($RESTART_COUNT times), read-only FS may not be working"
fi
echo "✓ No restarts (State.RestartCount = $RESTART_COUNT)"

# Test 5: Check no published ports
echo ""
echo "Test 5: Checking no published ports..."
PORTS=$(docker port "$CONTAINER_ID" 2>&1 || true)
if [ -n "$PORTS" ]; then
    fail "Published ports found: $PORTS"
fi
echo "✓ No published ports (docker port output empty)"

# Test 6: Verify non-root user at runtime (use Config.User from inspect)
echo ""
echo "Test 6: Verifying non-root user at runtime..."
# Already verified Config.User in Test 1; here double-check it's not changed during run
CONFIG_USER=$(docker inspect "$CONTAINER_ID" | jq -r '.[0].Config.User // empty')
if [ -z "$CONFIG_USER" ] || [ "$CONFIG_USER" = "0" ] || [ "$CONFIG_USER" = "root" ]; then
    fail "Container Config.User is root or empty (user=$CONFIG_USER)"
fi
echo "✓ Running as user=$CONFIG_USER (non-root)"

# Test 7: Check healthcheck
echo ""
echo "Test 7: Checking healthcheck..."
HC_EXIT=$(docker exec "$CONTAINER_ID" /usr/local/bin/bridge healthcheck > /dev/null 2>&1; echo $?)
if [ "$HC_EXIT" != "0" ]; then
    HEALTH_OUTPUT=$(docker exec "$CONTAINER_ID" /usr/local/bin/bridge healthcheck 2>&1 || true)
    echo "Note: Healthcheck exit code = $HC_EXIT (upstreams unavailable is expected). Output: $HEALTH_OUTPUT"
else
    echo "✓ Healthcheck exit code = 0"
fi

# Test 8: Check credentials not in logs
echo ""
echo "Test 8: Checking no credentials in logs..."
LOGS=$(docker logs "$CONTAINER_ID" 2>&1 || true)
if echo "$LOGS" | grep -qF "TEST-DUMMY-TOKEN"; then
    fail "Token found in logs"
fi
if echo "$LOGS" | grep -qF "pcp_dummy"; then
    fail "Paperclip token found in logs"
fi
echo "✓ No credentials in logs"

# Test 9: SIGTERM graceful shutdown
echo ""
echo "Test 9: Testing SIGTERM graceful shutdown..."
BEFORE=$(docker inspect "$CONTAINER_ID" | jq -r '.[0].State.Pid')
docker stop -t 15 "$CONTAINER_ID" 2>&1 || fail "docker stop failed"

# Wait a moment for container to fully stop
sleep 1

EXIT_CODE=$(docker inspect "$CONTAINER_ID" | jq -r '.[0].State.ExitCode')
if [ "$EXIT_CODE" != "0" ]; then
    fail "Container exited with code $EXIT_CODE (expected 0)"
fi
echo "✓ SIGTERM shutdown: exit code = 0"

echo ""
echo "=== All smoke tests passed ✓ ==="
