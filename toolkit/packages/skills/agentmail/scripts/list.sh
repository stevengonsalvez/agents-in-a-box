#!/usr/bin/env bash
# List all known disposable inboxes from the state directory.
# Usage: list.sh [--json]

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=_lib.sh
. "$SCRIPT_DIR/_lib.sh"

JSON_OUT=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --json) JSON_OUT=1; shift;;
    -h|--help)
      cat <<EOF
Usage: $(basename "$0") [--json]

Lists all inboxes recorded under \$AGENTMAIL_STATE_DIR/inboxes/ (or default).
Default output: table (slug | address | status | expires).
--json: emit array of state file contents.
EOF
      exit 0;;
    *) echo "Unknown arg: $1" >&2; exit 2;;
  esac
done

STATE_DIR=$(resolve_state_dir)
INBOX_DIR="$STATE_DIR/inboxes"

if [ ! -d "$INBOX_DIR" ] || [ -z "$(ls -A "$INBOX_DIR" 2>/dev/null)" ]; then
  if [ "$JSON_OUT" = "1" ]; then
    echo "[]"
  else
    echo "No inboxes in $INBOX_DIR"
  fi
  exit 0
fi

NOW_EPOCH=$(date -u +%s)

if [ "$JSON_OUT" = "1" ]; then
  jq -s '.' "$INBOX_DIR"/*.json
  exit 0
fi

printf "%-20s  %-45s  %-10s  %s\n" "SLUG" "ADDRESS" "STATUS" "EXPIRES"
printf "%-20s  %-45s  %-10s  %s\n" "----" "-------" "------" "-------"
for f in "$INBOX_DIR"/*.json; do
  SLUG=$(jq -r '.slug' "$f")
  ADDR=$(jq -r '.address' "$f")
  EXP=$(jq -r '.expires_at' "$f")
  EXP_EPOCH=$(iso_to_epoch "$EXP")
  if [ "$EXP_EPOCH" -le "$NOW_EPOCH" ]; then
    STATUS="expired"
  else
    HOURS_LEFT=$(( (EXP_EPOCH - NOW_EPOCH) / 3600 ))
    STATUS="live(${HOURS_LEFT}h)"
  fi
  printf "%-20s  %-45s  %-10s  %s\n" "$SLUG" "$ADDR" "$STATUS" "$EXP"
done
