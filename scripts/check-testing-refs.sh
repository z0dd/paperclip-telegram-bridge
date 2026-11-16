#!/usr/bin/env bash
# Проверяет, что каждая ссылка `путь.rs::тест` из docs/TESTING.md указывает на существующую функцию.
set -euo pipefail
cd "$(dirname "$0")/.."
missing=0
total=0
while read -r ref; do
    file="${ref%%::*}"
    name="${ref##*::}"
    total=$((total + 1))
    if [ ! -f "$file" ] || ! grep -qE "fn ${name}\b" "$file"; then
        echo "MISSING: $ref"
        missing=$((missing + 1))
    fi
done < <(grep -oE '[A-Za-z0-9_/]+\.rs::[a-z_0-9]+' docs/TESTING.md | sort -u)
echo "checked $total references, missing $missing"
[ "$missing" -eq 0 ]
