#!/usr/bin/env bash
set -euo pipefail

# Smoke test of both compose examples with the real image
# (paperclip-telegram-bridge:local, built by the docker CI job).
# Placeholders only; fixtures live in a temp dir. Any failed check exits 1.
#
#  host-network example:
#    (a) real image starts, stays running, healthcheck ok, reaches a test HTTP
#        server on the host's 127.0.0.1:3100 (proves host loopback is visible);
#    (b) same service with busybox: uid 65532, read-only root, writable /tmp,
#        readable secret, host loopback reachable.
#  isolated example (external networks created here, upstreams unreachable):
#    container starts, keeps running (retries) with RestartCount 0, healthcheck
#    ok, no published ports, stops with exit 0 on SIGTERM.

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/compose-smoke.XXXXXX")"
HOST_P="smoke-host-$$"
ISO_P="smoke-iso-$$"
SERVER_PID=""

fail() {
    echo "❌ $1"
    for p in "$HOST_P" "$ISO_P"; do
        for c in $(docker ps -aq --filter "label=com.docker.compose.project=$p"); do
            docker logs --tail 30 "$c" 2>&1 | sed 's/^/    log: /' || true
        done
    done
    exit 1
}

cleanup() {
    [ -z "$SERVER_PID" ] || kill "$SERVER_PID" 2>/dev/null || true
    (cd "$WORK" && docker compose -p "$HOST_P" -f compose.host-network.example.yml down -v >/dev/null 2>&1) || true
    (cd "$WORK" && docker compose -p "$ISO_P" -f compose.example.yml down -v >/dev/null 2>&1) || true
    docker network rm paperclip_internal xray_egress >/dev/null 2>&1 || true
    rm -rf "$WORK"
}
trap cleanup EXIT

docker image inspect paperclip-telegram-bridge:local >/dev/null 2>&1 \
    || fail "image paperclip-telegram-bridge:local not found (retag the built image)"

mkdir -p "$WORK/secrets" "$WORK/www"
cp "$REPO_ROOT"/deploy/compose*.example.yml "$WORK/"
printf 'placeholder-not-a-secret' > "$WORK/secrets/paperclip_token"
printf 'placeholder-not-a-secret' > "$WORK/secrets/telegram_bot_token"
chmod 0755 "$WORK" "$WORK/secrets"
chmod 0644 "$WORK"/secrets/*
cat > "$WORK/.env" <<ENV
PAPERCLIP_URL=http://127.0.0.1:9
TELEGRAM_PROXY_URL=http://127.0.0.1:9
PAPERCLIP_COMPANY_IDS=test-company
TELEGRAM_CHAT_ID=111
TELEGRAM_USER_ID=111
LOG_FORMAT=text
ENV
cd "$WORK"

wait_running() { # project service-name, 8s stability window
    local cid
    sleep 8
    cid=$(docker compose -p "$1" ps -q bridge)
    [ -n "$cid" ] || fail "$1: no container"
    [ "$(docker inspect -f '{{.State.Running}}' "$cid")" = "true" ] || fail "$1: container is not running"
    [ "$(docker inspect -f '{{.RestartCount}}' "$cid")" = "0" ] || fail "$1: container restarted"
    docker exec "$cid" /usr/local/bin/bridge healthcheck >/dev/null || fail "$1: healthcheck failed"
    echo "$cid"
}

assert_stop_ok() { # project cid
    local t0 t1 code
    t0=$(date +%s)
    docker compose -p "$1" stop -t 30 >/dev/null || fail "$1: compose stop failed"
    t1=$(date +%s)
    code=$(docker inspect -f '{{.State.ExitCode}}' "$2")
    [ "$code" = "0" ] || fail "$1: exit code $code after SIGTERM, expected 0"
    [ $((t1 - t0)) -lt 15 ] || fail "$1: stop took $((t1 - t0))s (limit 15s)"
    echo "✓ $1: SIGTERM -> exit 0 in $((t1 - t0))s"
}

echo "=== host-network example, real image ==="
# Test HTTP server on the host loopback where Paperclip would listen.
python3 -m http.server 3100 --bind 127.0.0.1 --directory "$WORK/www" >"$WORK/server.log" 2>&1 &
SERVER_PID=$!
for _ in $(seq 20); do curl -fsS -o /dev/null http://127.0.0.1:3100/ 2>/dev/null && break; sleep 0.5; done
: > "$WORK/server.log"   # drop our own probe request

docker compose -p "$HOST_P" -f compose.host-network.example.yml up -d >/dev/null
CID=$(wait_running "$HOST_P")
[ "$(docker inspect -f '{{.HostConfig.NetworkMode}}' "$CID")" = "host" ] || fail "host: NetworkMode is not host"
[ "$(docker inspect -f '{{.HostConfig.ReadonlyRootfs}}' "$CID")" = "true" ] || fail "host: root fs is not read-only"
[ "$(docker inspect -f '{{.Config.User}}' "$CID")" = "65532:65532" ] || fail "host: user is not 65532:65532"
[ "$(docker inspect -f '{{json .HostConfig.CapDrop}}' "$CID")" = '["ALL"]' ] || fail "host: cap_drop is not [ALL]"
[ -z "$(docker port "$CID")" ] || fail "host: published ports found"
echo "✓ host: running, healthy, NetworkMode=host, read-only root, 65532:65532, cap_drop ALL"
for _ in $(seq 40); do grep -q 'GET ' "$WORK/server.log" && break; sleep 1; done
grep -q 'GET ' "$WORK/server.log" || fail "host: bridge did not reach the test server on host 127.0.0.1:3100"
echo "✓ host: bridge reached the test HTTP server on host loopback 127.0.0.1:3100"
assert_stop_ok "$HOST_P" "$CID"
docker compose -p "$HOST_P" -f compose.host-network.example.yml down -v >/dev/null

echo "=== host-network example, busybox with the same hardening ==="
cat > "$WORK/override-busybox.yml" <<'YAML'
services:
  bridge:
    image: busybox:1.37
    entrypoint:
      - sh
      - -c
      - |
        set -e
        [ "$(id -u)" = 65532 ] || { echo "uid is $(id -u)"; exit 1; }
        if touch /rootfs-probe 2>/dev/null; then echo "root fs is writable"; exit 1; fi
        touch /tmp/probe || { echo "/tmp is not writable"; exit 1; }
        head -c1 /run/secrets/telegram_bot_token >/dev/null || { echo "secret unreadable"; exit 1; }
        wget -q -T 5 -O /dev/null http://127.0.0.1:3100/ || { echo "host loopback unreachable"; exit 1; }
        echo FS-CHECKS-OK
      - sh
YAML
OUT=$(docker compose -p "$HOST_P" -f compose.host-network.example.yml -f override-busybox.yml run --rm --no-deps -T bridge 2>&1) \
    || fail "host busybox checks failed: $OUT"
printf '%s\n' "$OUT" | grep -q '^FS-CHECKS-OK$' || fail "host busybox: no FS-CHECKS-OK marker: $OUT"
echo "✓ host: uid 65532, read-only root, writable /tmp, secret readable, host loopback reachable"
docker compose -p "$HOST_P" -f compose.host-network.example.yml down -v >/dev/null
kill "$SERVER_PID" 2>/dev/null || true; SERVER_PID=""

echo "=== isolated example, real image, placeholders, no real services ==="
docker network create --internal paperclip_internal >/dev/null
docker network create --internal xray_egress >/dev/null
docker compose -p "$ISO_P" -f compose.example.yml up -d >/dev/null
CID=$(wait_running "$ISO_P")
[ "$(docker inspect -f '{{.HostConfig.ReadonlyRootfs}}' "$CID")" = "true" ] || fail "isolated: root fs is not read-only"
[ -z "$(docker port "$CID")" ] || fail "isolated: published ports found"
echo "✓ isolated: started, stays running without upstreams, healthy, read-only root, no ports"
assert_stop_ok "$ISO_P" "$CID"

echo "=== compose smoke passed ✓ ==="
