#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
app_root="$repo_root/apps/ainb-fleet-macos"
source_root="$app_root/Sources"
allowlist="$app_root/ci/swift-boundary-allowlist.txt"

fail() {
  printf 'Swift boundary violation: %s\n' "$*" >&2
  exit 1
}

[[ -d "$source_root" ]] || fail "missing Sources directory"
[[ -f "$allowlist" ]] || fail "missing allowlist"

for forbidden in \
  'URLSession' \
  'URLRequest' \
  'Process(' \
  'NSTask' \
  'sqlite3' \
  'SQLite' \
  'sessions.json' \
  'transcript' \
  'ainb fleet'; do
  if rg -n -F --glob '*.swift' -- "$forbidden" "$source_root"; then
    fail "forbidden transport or data source: $forbidden"
  fi
done

is_allowlisted() {
  local file="$1"
  local line="$2"
  local relative="${file#"$app_root/"}"
  local entry path fragment

  while IFS= read -r entry || [[ -n "$entry" ]]; do
    [[ -z "$entry" || "$entry" == \#* ]] && continue
    path="${entry%%|*}"
    fragment="${entry#*|}"
    [[ "$path" == "$relative" && "$line" == *"$fragment"* ]] && return 0
  done < "$allowlist"
  return 1
}

while IFS= read -r match; do
  file="${match%%:*}"
  remainder="${match#*:}"
  line="${remainder#*:}"
  if ! is_allowlisted "$file" "$line"; then
    printf '%s\n' "$match" >&2
    fail "opaque JSONValue inspection is not allowlisted"
  fi
done < <(
  rg -n --glob '*.swift' \
    'case[[:space:]]+\.?(object|array)\b|\.(object|array)[[:space:]]*\[|JSONValue[[:space:]]*\[' \
    "$source_root" || true
)
