#!/usr/bin/env bash
set -euo pipefail

# Validates both compose examples (ADR-0009 §5):
#   docker compose config --format json + jq comparison of hardening keys.
# Any mismatch exits 1. Fixtures (empty .env, placeholder secrets) live in a
# temp directory, never in the repository working tree.

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/compose-config.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

fail() { echo "❌ $1"; exit 1; }

mkdir -p "$WORK/deploy" "$WORK/secrets"
cp "$REPO_ROOT"/deploy/compose*.example.yml "$WORK/deploy/"
printf 'placeholder-not-a-secret' > "$WORK/secrets/paperclip_token"
printf 'placeholder-not-a-secret' > "$WORK/secrets/telegram_bot_token"
: > "$WORK/deploy/.env"
cd "$WORK/deploy"

docker compose -f compose.example.yml config --format json > "$WORK/isolated.json" \
    || fail "compose.example.yml: docker compose config failed"
docker compose -f compose.host-network.example.yml config --format json > "$WORK/host.json" \
    || fail "compose.host-network.example.yml: docker compose config failed"
echo "✓ both compose files render with docker compose config"

KEYS="restart read_only user cap_drop security_opt tmpfs stop_grace_period"
for key in $KEYS; do
    iso=$(jq -cS --arg k "$key" '.services.bridge[$k] // "MISSING"' "$WORK/isolated.json")
    host=$(jq -cS --arg k "$key" '.services.bridge[$k] // "MISSING"' "$WORK/host.json")
    [ "$iso" != '"MISSING"' ] || fail "isolated: bridge.$key is not set"
    [ "$host" != '"MISSING"' ] || fail "host-network: bridge.$key is not set"
    [ "$iso" = "$host" ] || fail "bridge.$key differs: isolated=$iso host-network=$host"
    echo "✓ bridge.$key equal in both files: $iso"
done

# Expected hardening values (guards against both files drifting together).
jq -e '.services.bridge.read_only == true and .services.bridge.user == "65532:65532"
       and (.services.bridge.cap_drop | index("ALL")) != null
       and (.services.bridge.security_opt | index("no-new-privileges:true")) != null' \
    "$WORK/host.json" > /dev/null || fail "hardening values (read_only/user/cap_drop/no-new-privileges) are wrong"
echo "✓ hardening values are as expected"

for f in isolated host; do
    jq -e '.services.bridge.tmpfs | map(select(startswith("/tmp"))) | length > 0' "$WORK/$f.json" > /dev/null \
        || fail "$f: tmpfs does not contain /tmp"
    echo "✓ $f: tmpfs contains /tmp"
done

jq -e '.services.bridge.network_mode == "host"' "$WORK/host.json" > /dev/null \
    || fail "host-network: network_mode is not host"
jq -e '.services.bridge | has("networks") | not' "$WORK/host.json" > /dev/null \
    || fail "host-network: bridge has networks"
jq -e '.services.bridge | has("ports") | not' "$WORK/host.json" > /dev/null \
    || fail "host-network: bridge has ports"
echo "✓ host-network: network_mode=host, no networks, no ports"

jq -e '.services.bridge | has("ports") | not' "$WORK/isolated.json" > /dev/null \
    || fail "isolated: bridge has published ports"
jq -e '.services.bridge | has("network_mode") | not' "$WORK/isolated.json" > /dev/null \
    || fail "isolated: bridge has network_mode (isolated example must use networks)"
echo "✓ isolated: no ports, no network_mode"

echo "=== compose config checks passed ✓ ==="
