#!/usr/bin/env bash
# Read a message from a disposable inbox and optionally extract a code or magic link.
# Usage: read.sh <slug> [--index 0] [--extract verification|magic-link|raw]

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=_lib.sh
. "$SCRIPT_DIR/_lib.sh"

SLUG=""
INDEX=0
EXTRACT="raw"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --index)   INDEX="$2"; shift 2;;
    --extract) EXTRACT="$2"; shift 2;;
    -h|--help)
      cat <<EOF
Usage: $(basename "$0") <slug> [--index N] [--extract verification|magic-link|raw]

Reads message at INDEX (0 = newest) from the inbox associated with SLUG.

--extract modes:
  raw           Print full JSON message body (default).
  verification  Print first 6-digit code matched in body or subject.
  magic-link    Print first https:// URL matched in body.

Exit: 0 success, 1 no such message / no match, 2 bad args.
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

RESP=$(api_call GET "/inboxes/${ADDRESS}/messages")
STATUS="${RESP%%	*}"
BODY="${RESP#*	}"

if [ "$STATUS" = "429" ]; then
  RETRY=$(echo "$BODY" | jq -r '.error.retry_after_seconds // 60')
  echo "ERROR: rate-limited (20 reads/min). Retry in ${RETRY}s." >&2
  exit 1
fi
if [ "$STATUS" != "200" ]; then
  echo "ERROR: API returned $STATUS" >&2
  echo "Body: $BODY" >&2
  exit 1
fi

MSG_COUNT=$(echo "$BODY" | jq -r '.data | length // 0')
if [ "$MSG_COUNT" = "0" ]; then
  echo "ERROR: inbox is empty" >&2; exit 1
fi
if [ "$INDEX" -ge "$MSG_COUNT" ]; then
  echo "ERROR: index $INDEX out of range (only $MSG_COUNT message(s))" >&2; exit 1
fi

MSG=$(echo "$BODY" | jq ".data[$INDEX]")

case "$EXTRACT" in
  raw)
    echo "$MSG"
    ;;
  verification)
    # Search both subject and body for first 6-digit code.
    SUBJECT=$(echo "$MSG" | jq -r '.subject // ""')
    BODY_TEXT=$(echo "$MSG" | jq -r '.text // .html // .body // ""')
    CODE=$(printf "%s\n%s" "$SUBJECT" "$BODY_TEXT" | grep -oE '\b[0-9]{6}\b' | head -1 || true)
    if [ -z "$CODE" ]; then
      echo "ERROR: no 6-digit verification code found" >&2; exit 1
    fi
    echo "$CODE"
    ;;
  magic-link)
    BODY_TEXT=$(echo "$MSG" | jq -r '.text // .html // .body // ""')
    LINK=$(echo "$BODY_TEXT" | grep -oE 'https://[^[:space:]"<>]+' | head -1 || true)
    if [ -z "$LINK" ]; then
      echo "ERROR: no https:// link found" >&2; exit 1
    fi
    echo "$LINK"
    ;;
  *)
    echo "ERROR: unknown --extract '$EXTRACT' (use raw|verification|magic-link)" >&2
    exit 2
    ;;
esac
