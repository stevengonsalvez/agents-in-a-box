#!/usr/bin/env bash
# Create a new disposable inbox and save state.
# Usage: create.sh <slug> [--purpose "freeform text"] [--force]

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=_lib.sh
. "$SCRIPT_DIR/_lib.sh"

SLUG=""
PURPOSE=""
FORCE=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --purpose) PURPOSE="$2"; shift 2;;
    --force)   FORCE=1; shift;;
    -h|--help)
      cat <<EOF
Usage: $(basename "$0") <slug> [--purpose "text"] [--force]

Creates a disposable inbox at myagentinbox.com and saves state to:
  \$AGENTMAIL_STATE_DIR/inboxes/<slug>.json
  (default: <git-root>/.agents/agentmail/inboxes/<slug>.json)

Outputs the inbox address to stdout (suitable for capture via \$()).

Errors out if a non-expired inbox already exists under this slug,
unless --force is passed.
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
  echo "ERROR: slug required. Usage: $(basename "$0") <slug>" >&2
  exit 2
fi

STATE_FILE=$(state_file_for_slug "$SLUG")

# Honour existing non-expired state
if [ -f "$STATE_FILE" ] && [ "$FORCE" -eq 0 ]; then
  EXISTING_EXPIRES=$(jq -r '.expires_at // empty' "$STATE_FILE")
  if [ -n "$EXISTING_EXPIRES" ]; then
    NOW_EPOCH=$(date -u +%s)
    EXP_EPOCH=$(iso_to_epoch "$EXISTING_EXPIRES")
    if [ "$EXP_EPOCH" -gt "$NOW_EPOCH" ]; then
      echo "ERROR: inbox '$SLUG' already exists and not expired." >&2
      echo "       Address: $(jq -r '.address' "$STATE_FILE")" >&2
      echo "       Expires at: $EXISTING_EXPIRES" >&2
      echo "       Pass --force to override." >&2
      exit 1
    fi
  fi
fi

# Create via API
RESP=$(api_call POST "/inboxes")
STATUS="${RESP%%	*}"
BODY="${RESP#*	}"

if [ "$STATUS" = "429" ]; then
  RETRY_AFTER=$(echo "$BODY" | jq -r '.error.retry_after_seconds // 60')
  echo "ERROR: rate-limited (3 inboxes/min). Retry in ${RETRY_AFTER}s." >&2
  exit 1
fi
if [ "$STATUS" != "200" ] && [ "$STATUS" != "201" ]; then
  echo "ERROR: API returned status $STATUS" >&2
  echo "Body: $BODY" >&2
  exit 1
fi

ADDRESS=$(echo "$BODY" | jq -r '.data.address // empty')
CREATED_AT=$(echo "$BODY" | jq -r '.data.created_at // empty')

if [ -z "$ADDRESS" ]; then
  echo "ERROR: malformed API response (no .data.address)" >&2
  echo "Body: $BODY" >&2
  exit 1
fi

EXPIRES_AT=$(iso_plus_24h)
[ -z "$CREATED_AT" ] && CREATED_AT=$(iso_now)

jq -n \
  --arg slug "$SLUG" \
  --arg address "$ADDRESS" \
  --arg created_at "$CREATED_AT" \
  --arg expires_at "$EXPIRES_AT" \
  --arg purpose "$PURPOSE" \
  '{slug: $slug, address: $address, created_at: $created_at, expires_at: $expires_at, purpose: $purpose, last_polled_at: null, message_count: 0}' \
  > "$STATE_FILE"

echo "$ADDRESS"
