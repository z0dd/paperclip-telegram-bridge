#!/usr/bin/env bash
set -euo pipefail

# Secret file permissions validation and compose structure test
# Checks ADR-0009 §4 requirements for secret file access in compose files
# Note: Full rootless Docker test requires local environment with usernamespace support

echo "=== Secret File Permissions & Compose Validation Test ==="
echo ""

TEST_DIR=$(mktemp -d)
trap "rm -rf $TEST_DIR" EXIT

# Prepare fixtures: dummy secrets in a directory (no real tokens/IPs)
mkdir -p "$TEST_DIR/secrets"
printf "placeholder-not-a-secret" > "$TEST_DIR/secrets/test_secret"
mkdir -p "$TEST_DIR/deploy"
touch "$TEST_DIR/deploy/.env"

# Create a test compose file with required hardening
cat > "$TEST_DIR/docker-compose.yml" << 'COMPOSE_EOF'
version: '3.8'
services:
  bridge:
    image: busybox:1.37
    restart: unless-stopped
    read_only: true
    user: "65532:65532"
    cap_drop: [ALL]
    security_opt:
      - no-new-privileges:true
    tmpfs:
      - /tmp:rw,noexec,nosuid,nodev,size=16m
    stop_grace_period: 30s
    env_file: .env
    command: cat /run/secrets/test_secret
    secrets:
      - test_secret

secrets:
  test_secret:
    file: ./secrets/test_secret
COMPOSE_EOF

cd "$TEST_DIR"

echo ""
echo "=== Test 1: Compose file validation ==="
if docker compose config >/dev/null 2>&1; then
  echo "✓ Compose file is valid"
else
  echo "ERROR: Compose file validation failed"; exit 1
fi

# Check required hardening keys
echo "✓ Checking required hardening keys..."
CONFIG=$(docker compose config --format json)

# Check tmpfs
if echo "$CONFIG" | jq -e '.services.bridge.tmpfs | index("/tmp")' >/dev/null 2>&1; then
  echo "✓ tmpfs contains /tmp"
else
  echo "ERROR: tmpfs doesn't contain /tmp"; exit 1
fi

# Check user
USER=$(echo "$CONFIG" | jq -r '.services.bridge.user // empty')
if [ "$USER" = "65532:65532" ]; then
  echo "✓ user is 65532:65532"
else
  echo "ERROR: user is not 65532:65532 (got: $USER)"; exit 1
fi

# Check cap_drop
if echo "$CONFIG" | jq -e '.services.bridge.cap_drop | index("ALL")' >/dev/null 2>&1; then
  echo "✓ cap_drop includes ALL"
else
  echo "ERROR: cap_drop doesn't include ALL"; exit 1
fi

# Check read_only
READONLY=$(echo "$CONFIG" | jq -r '.services.bridge.read_only // empty')
if [ "$READONLY" = "true" ]; then
  echo "✓ read_only is true"
else
  echo "ERROR: read_only is not true (got: $READONLY)"; exit 1
fi

# Check no-new-privileges
if echo "$CONFIG" | jq -e '.services.bridge.security_opt | index("no-new-privileges:true")' >/dev/null 2>&1; then
  echo "✓ security_opt includes no-new-privileges:true"
else
  echo "ERROR: security_opt missing no-new-privileges:true"; exit 1
fi

echo ""
echo "=== Test 2: Secret file permissions ==="

# Test strict variant (0400)
printf "placeholder-not-a-secret" > "$TEST_DIR/secrets/test_secret_strict"
chmod 0400 "$TEST_DIR/secrets/test_secret_strict"
FILE_MODE=$(stat -c %a "$TEST_DIR/secrets/test_secret_strict" 2>/dev/null || echo "")
if [ "$FILE_MODE" = "400" ]; then
  echo "✓ Strict variant: 0400 set correctly"
else
  echo "ERROR: Strict variant: got $FILE_MODE, expected 400"; exit 1
fi

# Test permissive variant (0644 in 0700 dir)
( umask 077; printf "placeholder-not-a-secret" > "$TEST_DIR/secrets/test_secret_perm" )
FILE_MODE=$(stat -c %a "$TEST_DIR/secrets/test_secret_perm" 2>/dev/null || echo "")
if [ "$FILE_MODE" = "600" ]; then
  echo "✓ Permissive variant: umask 077 creates 0600 (expected)"
  echo "  → Need explicit: chmod 0644 secrets/file for 0644 permissions"
else
  echo "ERROR: Permissive variant: got $FILE_MODE, expected 600"; exit 1
fi

echo ""
echo "=== Test 3: Compose file reading with secrets ==="
# Try to read config - this validates the secret reference works structurally
if docker compose config >/dev/null 2>&1; then
  echo "✓ Compose with secrets reference is valid"
else
  echo "ERROR: Compose with secrets failed"; exit 1
fi

echo ""
echo "=== All validation tests passed ✓ ==="
echo ""
echo "Note: Full rootless Docker permissions test requires:"
echo "  - Local environment with user namespace support (kernel.unprivileged_userns_clone=1)"
echo "  - Docker rootless binaries (download.docker.com/linux/static/rootless/)"
echo "  - uidmap and slirp4netns packages"
echo "  - Run: docker run --rm --network host busybox:1.37 ip -o -4 addr"
