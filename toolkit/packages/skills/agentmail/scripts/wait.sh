#!/usr/bin/env bash
# Poll a disposable inbox until a message arrives or timeout fires.
# Usage: wait.sh <slug> [--timeout 120] [--interval 5] [--from <substr>] [--subject <substr>]
# Exit codes: 0 message arrived, 124 timeout, 1 hard error, 2 bad args.

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=_lib.sh
. "$SCRIPT_DIR/_lib.sh"

SLUG=""
TIMEOUT=120
INTERVAL=5
FROM_FILTER=""
SUBJECT_FILTER=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --timeout)  TIMEOUT="$2"; shift 2;;
    --interval) INTERVAL="$2"; shift 2;;
    --from)     FROM_FILTER="$2"; shift 2;;
    --subject)  SUBJECT_FILTER="$2"; shift 2;;
    -h|--help)
      cat <<EOF
Usage: $(basename "$0") <slug> [--timeout 120] [--interval 5] [--from "text"] [--subject "text"]

Polls /inboxes/<address>/messages until first matching message arrives.
Filters --from and --subject are substring matches (case-insensitive).
Backs off to 30s on 429 (rate limit).

Exit: 0 success, 124 timeout, 1 error, 2 bad args.
EOF
      exit 0;;
    --) shift; break;;
    -*) echo "Unknown flag: $1" >&2; exit 2;;
    *)
      if [ -z "$SLUG" ]; then SLUG="$1"; else echo "Extra arg: $1" >&2; exit 2; fi
      shift;;
  esac
done

if [ -z "$SLUG" ]; then
  echo "ERROR: slug required" >&2; exit 2
fi

STATE_FILE=$(state_file_for_slug "$SLUG")
if [ ! -f "$STATE_FILE" ]; then
  echo "ERROR: no inbox '$SLUG' (run create.sh first)" >&2; exit 1
fi

ADDRESS=$(jq -r '.address' "$STATE_FILE")
EXPIRES_AT=$(jq -r '.expires_at' "$STATE_FILE")

NOW_EPOCH=$(date -u +%s)
EXP_EPOCH=$(iso_to_epoch "$EXPIRES_AT")
if [ "$EXP_EPOCH" -le "$NOW_EPOCH" ]; then
  echo "ERROR: inbox '$SLUG' expired at $EXPIRES_AT" >&2; exit 1
fi

DEADLINE=$((NOW_EPOCH + TIMEOUT))
CUR_INTERVAL="$INTERVAL"
MAX_INTERVAL=30

while :; do
  NOW_EPOCH=$(date -u +%s)
  if [ "$NOW_EPOCH" -ge "$DEADLINE" ]; then
    echo "TIMEOUT: no message after ${TIMEOUT}s" >&2
    exit 124
  fi

  RESP=$(api_call GET "/inboxes/${ADDRESS}/messages")
  STATUS="${RESP%%	*}"
  BODY="${RESP#*	}"

  if [ "$STATUS" = "429" ]; then
    CUR_INTERVAL=$(( CUR_INTERVAL * 2 ))
    [ "$CUR_INTERVAL" -gt "$MAX_INTERVAL" ] && CUR_INTERVAL="$MAX_INTERVAL"
    sleep "$CUR_INTERVAL"
    continue
  fi

  if [ "$STATUS" != "200" ]; then
    echo "ERROR: API returned $STATUS" >&2
    echo "Body: $BODY" >&2
    sleep "$CUR_INTERVAL"
    continue
  fi

  # Filter messages
  JQ_FILTER='.data // []'
  if [ -n "$FROM_FILTER" ]; then
    JQ_FILTER="$JQ_FILTER | map(select((.from // \"\") | ascii_downcase | contains(\"$(echo "$FROM_FILTER" | tr '[:upper:]' '[:lower:]')\")))"
  fi
  if [ -n "$SUBJECT_FILTER" ]; then
    JQ_FILTER="$JQ_FILTER | map(select((.subject // \"\") | ascii_downcase | contains(\"$(echo "$SUBJECT_FILTER" | tr '[:upper:]' '[:lower:]')\")))"
  fi

  COUNT=$(echo "$BODY" | jq -r "[$JQ_FILTER] | .[0] | length // 0" 2>/dev/null || echo 0)

  if [ "$COUNT" != "0" ] && [ "$COUNT" != "" ] && [ "$COUNT" != "null" ]; then
    # Update last_polled_at and message_count in state
    POLLED_AT=$(iso_now)
    TOTAL=$(echo "$BODY" | jq -r '.data | length')
    TMP=$(mktemp)
    jq --arg p "$POLLED_AT" --argjson c "$TOTAL" \
      '.last_polled_at = $p | .message_count = $c' "$STATE_FILE" > "$TMP" && mv "$TMP" "$STATE_FILE"
    echo "$BODY" | jq -c "$JQ_FILTER | .[0]"
    exit 0
  fi

  sleep "$CUR_INTERVAL"
  # Reset interval after a successful (non-429) poll
  CUR_INTERVAL="$INTERVAL"
done
