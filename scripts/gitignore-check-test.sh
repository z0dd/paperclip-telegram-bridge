#!/usr/bin/env bash
set -euo pipefail

# Negative test for the secrets gitignore check (ADR-0009 §4): the paths must be
# ignored by the repository .gitignore, and the check must fail when the rules
# are removed. Works on a temporary git repository; nothing in the working tree
# is touched. Only placeholder file names are used.

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/gitignore-check.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

fail() { echo "❌ $1"; exit 1; }

PATHS="secrets/telegram_bot_token secrets/paperclip_token"

git init -q "$WORK"
cd "$WORK"

# 1. Real .gitignore: every path is ignored.
cp "$REPO_ROOT/.gitignore" .gitignore
for p in $PATHS; do
    git check-ignore -q "$p" || fail "$p is not ignored by the repository .gitignore"
done
echo "✓ repository .gitignore ignores: $PATHS"

# 2. Without the secrets rules the same check must fail.
grep -vE '^/?secrets/?$|^paperclip_token$|^telegram_bot_token$' "$REPO_ROOT/.gitignore" > .gitignore
for p in $PATHS; do
    if git check-ignore -q "$p"; then
        fail "$p is still ignored after the secrets rules were removed: the check cannot detect removal"
    fi
done
echo "✓ check fails when the secrets rules are removed"

echo "=== gitignore negative test passed ✓ ==="
