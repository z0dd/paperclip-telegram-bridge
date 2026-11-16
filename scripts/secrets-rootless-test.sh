#!/usr/bin/env bash
set -euo pipefail

# Rootless Docker smoke test: secret file permissions
# Checks that:
# - Strict variant (0400 with subuid ownership) works
# - Permissive variant (0644 in 0700 dir) works
# - Host-user 0400 does NOT work in rootless Docker
# - Compose examples pass config validation

DOCKER_VERSION="29.10.0"
DOCKER_SHA256="2f56b1bbdd18d94b46bb05f96bb35a6b00d1ff8de15a0f2ee96e3f066a06e9b3"
EXTRAS_SHA256="1aeb35dc9eda71be76ae968ed7f69849e4dbfb4fc9e3fe8c27c6b56f27d4fea9"

TMPDIR_ROOTLESS=$(mktemp -d)
trap "rm -rf $TMPDIR_ROOTLESS" EXIT

export XDG_RUNTIME_DIR="/run/user/$(id -u)"
export DOCKER_HOST="unix://$XDG_RUNTIME_DIR/docker.sock"

fail() {
    echo "❌ $1"
    exit 1
}

echo "=== Rootless Docker Setup ==="
echo "Fixing AppArmor..."
sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0 > /dev/null

echo "Installing system packages..."
sudo apt-get update > /dev/null
sudo apt-get install -y uidmap slirp4netns > /dev/null

echo "Downloading Docker rootless $DOCKER_VERSION..."
cd "$TMPDIR_ROOTLESS"
curl -fsSL "https://download.docker.com/linux/static/rootless/x86_64/docker-$DOCKER_VERSION.tgz" -o docker.tgz
echo "$DOCKER_SHA256  docker.tgz" | sha256sum -c - > /dev/null || fail "Docker SHA256 mismatch"

curl -fsSL "https://download.docker.com/linux/static/rootless/x86_64/docker-rootless-extras-$DOCKER_VERSION.tgz" -o extras.tgz
echo "$EXTRAS_SHA256  extras.tgz" | sha256sum -c - > /dev/null || fail "Extras SHA256 mismatch"

tar xzf docker.tgz
tar xzf extras.tgz --strip-components=1 -C docker

echo "Installing rootless Docker setup..."
mkdir -p ~/.local/bin
cp docker/docker* ~/.local/bin/
export PATH="$HOME/.local/bin:$PATH"

echo "Running dockerd-rootless-setuptool.sh..."
docker/dockerd-rootless-setuptool.sh install --skip-iptables > /dev/null

echo "Starting rootless Docker daemon..."
systemctl --user start docker
sleep 2

echo "Verifying Docker is running..."
docker version > /dev/null || fail "Docker not running"
docker ps > /dev/null || fail "Cannot access docker socket"

echo ""
echo "=== Testing Secret File Permissions ==="

# Create test directories
TEST_DIR=$(mktemp -d)
trap "rm -rf $TEST_DIR; systemctl --user stop docker 2>/dev/null || true" EXIT

echo "Getting subuid_start..."
SUBUID_START=$(awk -F: '{print $2}' < /etc/subuid | head -1)
CONTAINER_UID_ON_HOST=$((SUBUID_START + 65531))
echo "Container UID 65532 maps to host UID $CONTAINER_UID_ON_HOST"

# Create docker-compose.test.yml with hardening
cat > "$TEST_DIR/docker-compose.test.yml" << 'COMPOSE_EOF'
version: '3.8'
services:
  test:
    image: busybox:1.37
    read_only: true
    user: "65532:65532"
    cap_drop: [ALL]
    security_opt:
      - no-new-privileges:true
    tmpfs:
      - /tmp
    command: |
      sh -c 'cat /run/secrets/test_secret > /tmp/secret_content.txt 2>/dev/null || exit 1'
    secrets:
      - test_secret
    volumes:
      - test_check:/check

secrets:
  test_secret:
    file: ./secrets/test_secret

volumes:
  test_check:
EOF

# Test 1: Strict variant (0400 with docker run chown)
echo ""
echo "Test 1: Strict variant (0400 owned by container UID)..."
mkdir -p "$TEST_DIR/secrets"
printf "placeholder-not-a-secret" > "$TEST_DIR/secrets/test_secret"
chmod 644 "$TEST_DIR/secrets/test_secret"

docker run --rm --network none -v "$TEST_DIR/secrets:/s" busybox:1.37 \
  sh -c 'chown 65532:65532 /s/test_secret && chmod 0400 /s/test_secret' \
  > /dev/null || fail "chown failed"

FILE_UID=$(stat -c %u "$TEST_DIR/secrets/test_secret")
if [ "$FILE_UID" != "$CONTAINER_UID_ON_HOST" ]; then
    fail "Ownership mismatch: got $FILE_UID, expected $CONTAINER_UID_ON_HOST"
fi

FILE_MODE=$(stat -c %a "$TEST_DIR/secrets/test_secret")
if [ "$FILE_MODE" != "400" ]; then
    fail "Permissions mismatch: got $FILE_MODE, expected 400"
fi

cd "$TEST_DIR"
docker compose -f docker-compose.test.yml run --rm test > /dev/null \
    || fail "Container couldn't read secret in strict mode"
cd - > /dev/null
echo "✓ Container reads 0400 secret; host cannot read"

if cat "$TEST_DIR/secrets/test_secret" 2>/dev/null; then
    fail "Host user should not be able to read 0400 file"
fi
echo "✓ Host user cannot read 0400 file"

# Test 2: Permissive variant (0644 in 0700 dir)
echo ""
echo "Test 2: Permissive variant (0644 in 0700 dir)..."
rm "$TEST_DIR/secrets/test_secret"
mkdir -p "$TEST_DIR/secrets"
( umask 077; printf "placeholder-not-a-secret" > "$TEST_DIR/secrets/test_secret" )

FILE_MODE=$(stat -c %a "$TEST_DIR/secrets/test_secret")
if [ "$FILE_MODE" != "644" ]; then
    fail "Permissions not set correctly by umask: got $FILE_MODE, expected 644"
fi

cd "$TEST_DIR"
docker compose -f docker-compose.test.yml run --rm test > /dev/null \
    || fail "Container couldn't read secret in permissive mode"
cd - > /dev/null
echo "✓ Container reads 0644 secret"

if ! cat "$TEST_DIR/secrets/test_secret" 2>/dev/null | grep -q "placeholder"; then
    fail "Host user should be able to read 0644 file"
fi
echo "✓ Host user can read 0644 file"

# Test 3: Host-user 0400 should NOT work
echo ""
echo "Test 3: Host-user 0400 does NOT work..."
rm "$TEST_DIR/secrets/test_secret"
printf "placeholder-not-a-secret" > "$TEST_DIR/secrets/test_secret"
chmod 400 "$TEST_DIR/secrets/test_secret"

cd "$TEST_DIR"
if docker compose -f docker-compose.test.yml run --rm test > /dev/null 2>&1; then
    fail "Container should NOT be able to read host-user 0400 file (but it did)"
fi
cd - > /dev/null
echo "✓ Container cannot read host-user 0400 file (expected)"

echo ""
echo "=== Testing Compose Config Validation ==="

# Prepare placeholders for the actual compose files
REPO_ROOT="${PAPERCLIP_WORKSPACE_CWD:-$(pwd)}"
mkdir -p "$REPO_ROOT/.env" 2>/dev/null || true
mkdir -p "$REPO_ROOT/secrets" 2>/dev/null || true

echo "placeholder-not-a-secret" > "$REPO_ROOT/secrets/paperclip_token"
echo "placeholder-not-a-secret" > "$REPO_ROOT/secrets/telegram_bot_token"
[ -f "$REPO_ROOT/.env" ] || printf "" > "$REPO_ROOT/.env"

echo ""
echo "Validating compose.example.yml..."
cd "$REPO_ROOT"
CONFIG_ISOLATED=$(docker compose -f deploy/compose.example.yml config --format json)
echo "✓ compose.example.yml validates"

echo "Validating compose.host-network.example.yml..."
CONFIG_HOST=$(docker compose -f deploy/compose.host-network.example.yml config --format json)
echo "✓ compose.host-network.example.yml validates"

echo ""
echo "Comparing hardening between both compose files..."

# Extract hardening keys for 'bridge' service
ISOLATED_HARDENING=$(echo "$CONFIG_ISOLATED" | jq '.services.bridge | {restart, read_only, user, cap_drop, security_opt, tmpfs, stop_grace_period}')
HOST_HARDENING=$(echo "$CONFIG_HOST" | jq '.services.bridge | {restart, read_only, user, cap_drop, security_opt, tmpfs, stop_grace_period}')

if [ "$ISOLATED_HARDENING" != "$HOST_HARDENING" ]; then
    echo "Isolated hardening: $ISOLATED_HARDENING"
    echo "Host hardening:     $HOST_HARDENING"
    fail "Hardening mismatch between compose files"
fi
echo "✓ Hardening identical in both files"

# Check tmpfs in both
if ! echo "$CONFIG_ISOLATED" | jq '.services.bridge.tmpfs[]' | grep -q "/tmp"; then
    fail "tmpfs /tmp not found in compose.example.yml"
fi
echo "✓ tmpfs /tmp in compose.example.yml"

if ! echo "$CONFIG_HOST" | jq '.services.bridge.tmpfs[]' | grep -q "/tmp"; then
    fail "tmpfs /tmp not found in compose.host-network.example.yml"
fi
echo "✓ tmpfs /tmp in compose.host-network.example.yml"

# Check host network config
if [ "$(echo "$CONFIG_HOST" | jq -r '.services.bridge.network_mode')" != "host" ]; then
    fail "network_mode is not 'host' in compose.host-network.example.yml"
fi
echo "✓ network_mode = host in compose.host-network.example.yml"

if echo "$CONFIG_HOST" | jq '.services.bridge.networks' | grep -q .; then
    fail "networks should not be defined in compose.host-network.example.yml"
fi
echo "✓ No networks defined in compose.host-network.example.yml"

if echo "$CONFIG_HOST" | jq '.services.bridge.ports' | grep -q .; then
    fail "ports should not be defined in compose.host-network.example.yml"
fi
echo "✓ No ports defined in compose.host-network.example.yml"

# Check loopback addresses
if [ "$(echo "$CONFIG_HOST" | jq -r '.services.bridge.environment.PAPERCLIP_URL')" != "http://127.0.0.1:3100" ]; then
    fail "PAPERCLIP_URL should be http://127.0.0.1:3100"
fi
echo "✓ PAPERCLIP_URL = http://127.0.0.1:3100"

if [ "$(echo "$CONFIG_HOST" | jq -r '.services.bridge.environment.TELEGRAM_PROXY_URL')" != "http://127.0.0.1:10808" ]; then
    fail "TELEGRAM_PROXY_URL should be http://127.0.0.1:10808"
fi
echo "✓ TELEGRAM_PROXY_URL = http://127.0.0.1:10808"

echo ""
echo "=== All rootless tests passed ✓ ==="
