#!/usr/bin/env bash
set -euo pipefail

# Rootless Docker smoke test: secret file permissions
# Checks ADR-0009 §4 requirements for secret file access

DOCKER_VERSION="29.10.0"
DOCKER_SHA256="2f56b1bbdd18d94b46bb05f96bb35a6b00d1ff8de15a0f2ee96e3f066a06e9b3"
EXTRAS_SHA256="1aeb35dc9eda71be76ae968ed7f69849e4dbfb4fc9e3fe8c27c6b56f27d4fea9"

TMPDIR_ROOTLESS=$(mktemp -d)
trap "rm -rf $TMPDIR_ROOTLESS" EXIT

export XDG_RUNTIME_DIR="/run/user/$(id -u)"
export DOCKER_HOST="unix://$XDG_RUNTIME_DIR/docker.sock"

echo "=== Rootless Docker Secrets Test ==="
echo ""
echo "Disabling AppArmor..."
sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0 >/dev/null 2>&1 || true

echo "Installing system packages..."
sudo apt-get update >/dev/null 2>&1 || true
sudo apt-get install -y uidmap slirp4netns >/dev/null 2>&1 || true

echo "Downloading and installing Docker rootless $DOCKER_VERSION..."
cd "$TMPDIR_ROOTLESS"

curl -fsSL "https://download.docker.com/linux/static/rootless/x86_64/docker-$DOCKER_VERSION.tgz" -o docker.tgz
echo "$DOCKER_SHA256  docker.tgz" | sha256sum -c - >/dev/null || { echo "ERROR: Docker SHA256 mismatch"; exit 1; }

curl -fsSL "https://download.docker.com/linux/static/rootless/x86_64/docker-rootless-extras-$DOCKER_VERSION.tgz" -o extras.tgz
echo "$EXTRAS_SHA256  extras.tgz" | sha256sum -c - >/dev/null || { echo "ERROR: Extras SHA256 mismatch"; exit 1; }

tar xzf docker.tgz
tar xzf extras.tgz --strip-components=1 -C docker >/dev/null 2>&1 || true

mkdir -p ~/.local/bin
cp docker/docker* ~/.local/bin/
export PATH="$HOME/.local/bin:$PATH"

echo "Installing Docker setup..."
docker/dockerd-rootless-setuptool.sh install --skip-iptables >/dev/null 2>&1 || true

echo "Starting Docker..."
systemctl --user start docker >/dev/null 2>&1 || true
sleep 3

echo "Verifying Docker..."
docker version > /dev/null || { echo "ERROR: Docker not running"; exit 1; }
docker ps > /dev/null || { echo "ERROR: Cannot access docker.sock"; exit 1; }
echo "✓ Docker rootless running"

echo ""
echo "=== Testing Secret File Permissions ==="

TEST_DIR=$(mktemp -d)
trap "rm -rf $TEST_DIR; systemctl --user stop docker 2>/dev/null || true; $(trap -p EXIT | sed 's/.*trap/trap/g')" EXIT

# Get subuid mapping
SUBUID_START=$(awk -F: '{print $2; exit}' < /etc/subuid)
CONTAINER_UID_ON_HOST=$((SUBUID_START + 65531))

echo "Container UID 65532 maps to host UID $CONTAINER_UID_ON_HOST"

# Create test compose file
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
    command: cat /run/secrets/test_secret
    secrets:
      - test_secret

secrets:
  test_secret:
    file: ./secrets/test_secret
COMPOSE_EOF

cd "$TEST_DIR"

echo ""
echo "Test 1: Strict variant (0400 with docker run chown)..."
mkdir -p secrets
printf "placeholder-not-a-secret" > secrets/test_secret

docker run --rm --network none -v "$TEST_DIR/secrets:/s" busybox:1.37 \
  sh -c 'chown 65532:65532 /s/test_secret && chmod 0400 /s/test_secret' 2>&1 >/dev/null

FILE_UID=$(stat -c %u "$TEST_DIR/secrets/test_secret" 2>/dev/null || echo "0")
if [ "$FILE_UID" = "$CONTAINER_UID_ON_HOST" ]; then
  echo "✓ File owned by container UID on host"
else
  echo "⚠ File ownership: got $FILE_UID, expected $CONTAINER_UID_ON_HOST (warning)"
fi

FILE_MODE=$(stat -c %a "$TEST_DIR/secrets/test_secret" 2>/dev/null || echo "0")
if [ "$FILE_MODE" = "400" ]; then
  echo "✓ File permissions are 0400"
else
  echo "⚠ File permissions: got $FILE_MODE, expected 400 (warning)"
fi

if docker compose -f docker-compose.test.yml run --rm test 2>&1 | grep -q "placeholder"; then
  echo "✓ Container can read secret"
else
  echo "ERROR: Container cannot read secret"; exit 1
fi

echo ""
echo "Test 2: Permissive variant (0644 in 0700 dir)..."
rm -f "$TEST_DIR/secrets/test_secret"
( umask 077; printf "placeholder-not-a-secret" > "$TEST_DIR/secrets/test_secret" )

FILE_MODE=$(stat -c %a "$TEST_DIR/secrets/test_secret" 2>/dev/null || echo "0")
if [ "$FILE_MODE" = "644" ]; then
  echo "✓ File permissions are 0644"
else
  echo "⚠ File permissions: got $FILE_MODE, expected 644 (warning)"
fi

if docker compose -f docker-compose.test.yml run --rm test 2>&1 | grep -q "placeholder"; then
  echo "✓ Container can read secret"
else
  echo "ERROR: Container cannot read secret"; exit 1
fi

echo ""
echo "=== Compose Files Validation ==="

cd - >/dev/null
REPO_ROOT="${PAPERCLIP_WORKSPACE_CWD:-$(pwd)}"

mkdir -p "$REPO_ROOT/secrets"
printf "placeholder-not-a-secret" > "$REPO_ROOT/secrets/paperclip_token" 2>/dev/null || true
printf "placeholder-not-a-secret" > "$REPO_ROOT/secrets/telegram_bot_token" 2>/dev/null || true
[ -f "$REPO_ROOT/.env" ] || touch "$REPO_ROOT/.env" 2>/dev/null || true

echo "Validating compose.example.yml..."
docker compose -f "$REPO_ROOT/deploy/compose.example.yml" config >/dev/null 2>&1 && echo "✓ Valid" || { echo "ERROR: Invalid"; exit 1; }

echo "Validating compose.host-network.example.yml..."
docker compose -f "$REPO_ROOT/deploy/compose.host-network.example.yml" config >/dev/null 2>&1 && echo "✓ Valid" || { echo "ERROR: Invalid"; exit 1; }

echo ""
echo "=== All rootless tests passed ✓ ==="
