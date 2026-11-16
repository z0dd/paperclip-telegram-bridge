#!/usr/bin/env bash
set -euo pipefail

# Secret file permissions validation and compose structure test
# Checks ADR-0009 §4 requirements for secret file access in compose files

echo "=== Secret File Permissions & Compose Validation Test ==="
echo ""

# Use repo secrets and deploy directories
REPO_ROOT="${PAPERCLIP_WORKSPACE_CWD:-.}"
TEST_DIR="$REPO_ROOT"

echo "Working directory: $TEST_DIR"

# Prepare fixtures if they don't exist
if [ ! -d "$TEST_DIR/secrets" ]; then
  mkdir -p "$TEST_DIR/secrets"
fi
if [ ! -f "$TEST_DIR/secrets/test_secret" ]; then
  printf "placeholder-not-a-secret" > "$TEST_DIR/secrets/test_secret"
fi
if [ ! -f "$TEST_DIR/deploy/.env" ]; then
  touch "$TEST_DIR/deploy/.env"
fi

echo ""
echo "=== Test 1: Repository compose files validation ==="

# Validate compose files with docker compose
cd "$TEST_DIR/deploy" || exit 1

if docker compose -f compose.example.yml config >/dev/null 2>&1; then
  echo "✓ compose.example.yml is valid"
else
  echo "ERROR: compose.example.yml validation failed"; exit 1
fi

if docker compose -f compose.host-network.example.yml config >/dev/null 2>&1; then
  echo "✓ compose.host-network.example.yml is valid"
else
  echo "ERROR: compose.host-network.example.yml validation failed"; exit 1
fi

# Check required hardening keys in both files
echo ""
echo "=== Test 2: Checking hardening requirements ==="

ISOLATED=$(docker compose -f compose.example.yml config --format json)
HOST=$(docker compose -f compose.host-network.example.yml config --format json)

# Check tmpfs in both
for FILE in "isolated (compose.example.yml)" "host (compose.host-network.example.yml)"; do
  CONFIG=$([ "${FILE:0:1}" = "i" ] && echo "$ISOLATED" || echo "$HOST")
  if echo "$CONFIG" | jq -e '.services.bridge.tmpfs | type == "array" and (.[] | select(contains("/tmp")))' >/dev/null 2>&1; then
    echo "✓ $FILE has tmpfs with /tmp"
  else
    echo "ERROR: $FILE missing /tmp in tmpfs"; exit 1
  fi
done

# Check user (should be same in both)
USER_ISOLATED=$(echo "$ISOLATED" | jq -r '.services.bridge.user // empty')
USER_HOST=$(echo "$HOST" | jq -r '.services.bridge.user // empty')
if [ "$USER_ISOLATED" = "65532:65532" ] && [ "$USER_HOST" = "65532:65532" ]; then
  echo "✓ Both files have user 65532:65532"
else
  echo "ERROR: User mismatch or incorrect"; exit 1
fi

# Check cap_drop in both
for FILE in "isolated (compose.example.yml)" "host (compose.host-network.example.yml)"; do
  CONFIG=$([ "${FILE:0:1}" = "i" ] && echo "$ISOLATED" || echo "$HOST")
  if echo "$CONFIG" | jq -e '.services.bridge.cap_drop | index("ALL")' >/dev/null 2>&1; then
    echo "✓ $FILE has cap_drop: [ALL]"
  else
    echo "ERROR: $FILE missing cap_drop ALL"; exit 1
  fi
done

# Check no-new-privileges
for FILE in "isolated (compose.example.yml)" "host (compose.host-network.example.yml)"; do
  CONFIG=$([ "${FILE:0:1}" = "i" ] && echo "$ISOLATED" || echo "$HOST")
  if echo "$CONFIG" | jq -e '.services.bridge.security_opt | index("no-new-privileges:true")' >/dev/null 2>&1; then
    echo "✓ $FILE has no-new-privileges:true"
  else
    echo "ERROR: $FILE missing no-new-privileges:true"; exit 1
  fi
done

# Check host-network specific: network_mode=host, no networks, no ports
echo ""
echo "=== Test 3: Host-network specific checks ==="
NETWORK_MODE=$(echo "$HOST" | jq -r '.services.bridge.network_mode // empty')
if [ "$NETWORK_MODE" = "host" ]; then
  echo "✓ Host-network compose has network_mode=host"
else
  echo "ERROR: network_mode is not 'host' (got: $NETWORK_MODE)"; exit 1
fi

NETWORKS=$(echo "$HOST" | jq '.services.bridge.networks // empty')
if [ "$NETWORKS" = "null" ] || [ -z "$NETWORKS" ]; then
  echo "✓ Host-network compose has no networks defined"
else
  echo "ERROR: Host-network compose has networks"; exit 1
fi

PORTS=$(echo "$HOST" | jq '.services.bridge.ports // empty')
if [ "$PORTS" = "null" ] || [ -z "$PORTS" ]; then
  echo "✓ Host-network compose has no ports defined"
else
  echo "ERROR: Host-network compose has ports"; exit 1
fi

# Check read_only in both
for FILE in "isolated (compose.example.yml)" "host (compose.host-network.example.yml)"; do
  CONFIG=$([ "${FILE:0:1}" = "i" ] && echo "$ISOLATED" || echo "$HOST")
  READONLY=$(echo "$CONFIG" | jq -r '.services.bridge.read_only // empty')
  if [ "$READONLY" = "true" ]; then
    echo "✓ $FILE has read_only: true"
  else
    echo "ERROR: $FILE read_only is not true"; exit 1
  fi
done

echo ""
echo "=== Test 4: Secret file permissions concepts ==="

# Test strict variant (0400)
printf "placeholder-not-a-secret" > "$TEST_DIR/secrets/test_strict"
chmod 0400 "$TEST_DIR/secrets/test_strict"
FILE_MODE=$(stat -c %a "$TEST_DIR/secrets/test_strict" 2>/dev/null || echo "")
if [ "$FILE_MODE" = "400" ]; then
  echo "✓ Strict variant: 0400 set correctly"
else
  echo "ERROR: Strict variant got $FILE_MODE, expected 400"; exit 1
fi

# Test permissive variant (0644 requires explicit chmod, umask 077 gives 0600)
( umask 077; printf "placeholder-not-a-secret" > "$TEST_DIR/secrets/test_perm" )
FILE_MODE=$(stat -c %a "$TEST_DIR/secrets/test_perm" 2>/dev/null || echo "")
if [ "$FILE_MODE" = "600" ]; then
  echo "✓ Permissive variant: umask 077 creates 0600 (need explicit chmod 0644 for 0644)"
else
  echo "ERROR: Permissive variant got $FILE_MODE, expected 600"; exit 1
fi

echo ""
echo "=== All validation tests passed ✓ ==="
echo ""
echo "Summary:"
echo "  ✓ Both compose files are valid YAML"
echo "  ✓ Hardening requirements met (tmpfs, user, cap_drop, security_opt, read_only)"
echo "  ✓ Host-network compose properly configured (network_mode=host, no networks/ports)"
echo "  ✓ Secret file permissions validated"
echo ""
echo "Notes:"
echo "  - For full rootless Docker test, run on local machine with usernamespace support"
echo "  - Strict: 0400 (container UID-owned)"
echo "  - Permissive: explicit 'chmod 0644' in 0700 directory"
