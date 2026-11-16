#!/usr/bin/env bash
set -euo pipefail

# Secret file permissions in rootless Docker (ADR-0009 §4).
# Requires a running rootless Docker daemon (DOCKER_HOST points at it) and the
# compose plugin. CI installs it with scripts/rootless-docker-install.sh.
# Every case runs a hardened compose service (read_only, user 65532:65532,
# cap_drop ALL, no-new-privileges, `secrets:`) and compares the observed result
# with the expected one; any mismatch exits 1. Secret contents are never printed.

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/secrets-rootless.XXXXXX")"
PROJECT="secrets-rootless-$$"
BUSYBOX="busybox:1.37"

fail() { echo "❌ $1"; exit 1; }

cleanup() {
    (cd "$WORK" 2>/dev/null && docker compose -p "$PROJECT" -f compose.yml -f override.yml down -v >/dev/null 2>&1) || true
    chmod -R u+rwX "$WORK" 2>/dev/null || true
    rm -rf "$WORK" 2>/dev/null || true
}
trap cleanup EXIT

docker info --format '{{.SecurityOptions}}' | grep -q rootless \
    || fail "docker daemon is not rootless (SecurityOptions has no 'rootless')"

# --- 1. UID mapping: container 65532 == host subuid_start + 65531 ----------
SUBUID_START=$(awk -F: -v u="$(id -un)" '$1 == u { print $2; exit }' /etc/subuid)
[ -n "$SUBUID_START" ] || fail "no /etc/subuid entry for $(id -un)"
EXPECTED_UID=$((SUBUID_START + 65531))

mkdir -p "$WORK/probe"
chmod 0777 "$WORK/probe"
docker run --rm --network none --user 65532:65532 -v "$WORK/probe:/p" "$BUSYBOX" touch /p/owned-by-container
ACTUAL_UID=$(stat -c %u "$WORK/probe/owned-by-container")
[ "$ACTUAL_UID" = "$EXPECTED_UID" ] \
    || fail "UID mapping: file owner on host is $ACTUAL_UID, expected $EXPECTED_UID (subuid_start $SUBUID_START + 65531)"
echo "✓ UID mapping: container 65532 -> host $ACTUAL_UID (subuid_start $SUBUID_START + 65531)"

# --- 2. Hardened compose service reading /run/secrets/telegram_bot_token ----
cp "$REPO_ROOT/deploy/compose.host-network.example.yml" "$WORK/compose.yml"
: > "$WORK/.env"
# Quoted heredoc: `$$` is the compose escape for a literal `$` in the script.
cat > "$WORK/override.yml" <<'YAML'
services:
  bridge:
    image: busybox:1.37
    entrypoint:
      - sh
      - -c
      - |
        if head -c1 /run/secrets/telegram_bot_token >/dev/null 2>&1; then r=readable; else r=denied; fi
        echo "RESULT=$$r uid=$$(id -u)"
      - sh
YAML
mkdir -p "$WORK/secrets"
chmod 0700 "$WORK/secrets"
cd "$WORK"
docker pull -q "$BUSYBOX" >/dev/null

# Prepares both secret files with the placeholder and the given recipe.
prepare() {
    rm -f secrets/paperclip_token secrets/telegram_bot_token
    ( umask 077
      printf '%s' placeholder-not-a-secret > secrets/telegram_bot_token
      printf '%s' placeholder-not-a-secret > secrets/paperclip_token )
    case "$1" in
        strict)  # recommended recipe from README
            docker run --rm --network none -v "$WORK/secrets:/s" "$BUSYBOX" \
                sh -c 'chown 65532:65532 /s/telegram_bot_token /s/paperclip_token && chmod 0400 /s/telegram_bot_token /s/paperclip_token' ;;
        0400|0600|0640|0644)
            chmod "$1" secrets/telegram_bot_token secrets/paperclip_token ;;
        *) fail "unknown recipe $1" ;;
    esac
}

host_can_read() { head -c1 secrets/telegram_bot_token >/dev/null 2>&1 && echo yes || echo no; }

run_case() { # recipe expected-container-result expected-host-read
    local recipe=$1 want=$2 want_host=$3 out got host
    prepare "$recipe"
    out=$(docker compose -p "$PROJECT" -f compose.yml -f override.yml run --rm --no-deps -T bridge 2>&1) \
        || fail "[$recipe] docker compose run failed: $out"
    got=$(printf '%s\n' "$out" | grep -E '^RESULT=' | tail -n1) || fail "[$recipe] no RESULT line in output: $out"
    host=$(host_can_read)
    echo "  [$recipe] container: $got; host reads: $host; owner uid: $(stat -c %u secrets/telegram_bot_token); mode: $(stat -c %a secrets/telegram_bot_token)"
    [ "$got" = "RESULT=$want uid=65532" ] || fail "[$recipe] container result '$got', expected 'RESULT=$want uid=65532'"
    [ "$host" = "$want_host" ] || fail "[$recipe] host read '$host', expected '$want_host'"
    echo "✓ [$recipe] matches expectation (container $want, host reads $want_host)"
}

echo "=== matrix (recipe, container result, host can read) ==="
run_case strict readable no     # 0400, owner subuid+65531 (recommended)
run_case 0400   denied   yes    # 0400, host user owner
run_case 0600   denied   yes
run_case 0640   denied   yes
run_case 0644   readable yes    # dir 0700 + file 0644 (acceptable)

# --- 3. host network sanity for QA (informational + sanity assertion) ------
echo "=== docker run --network host $BUSYBOX ip -o -4 addr ==="
IP_OUT=$(docker run --rm --network host "$BUSYBOX" ip -o -4 addr)
echo "$IP_OUT"
printf '%s\n' "$IP_OUT" | awk '{print $2}' | grep -qvE '^(lo|tap0)$' \
    || fail "network_mode host in rootless sees only lo/tap0: host interfaces are not visible"
echo "✓ host interfaces visible with --network host"

echo "=== all secret permission checks passed ✓ ==="
