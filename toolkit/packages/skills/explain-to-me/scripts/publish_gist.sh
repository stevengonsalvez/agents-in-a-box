#!/usr/bin/env bash
# Publish an explain-to-me HTML to a GitHub Gist and print the htmlpreview
# render URL — the ALTERNATE to here.now (`--gist`).
#
# Why: here.now anonymous publishes expire in 24h. A Gist is permanent (as long
# as it exists), and htmlpreview.github.io renders its raw HTML as a live page —
# so the shareable link never goes stale. The explainer HTML is self-contained
# (inline SVG + CSS), so a single-file gist renders perfectly.
#
# Usage:
#   publish_gist.sh <file.html> [--desc "..."] [--public] [--update <gist-id>]
#
# Output (key=value lines, easy to parse):
#   gist_url=...      the gist page
#   gist_id=...       the gist id
#   raw_url=...       the always-latest raw HTML (no commit SHA)
#   preview_url=...   the htmlpreview.github.io rendered page  <- share THIS
set -euo pipefail

FILE=""
DESC="explain-to-me explainer"
PUBLIC=0
UPDATE_ID=""
while [ $# -gt 0 ]; do
  case "$1" in
    --desc)    DESC="$2"; shift 2 ;;
    --public)  PUBLIC=1; shift ;;
    --update)  UPDATE_ID="$2"; shift 2 ;;
    -h|--help) sed -n '2,17p' "$0"; exit 0 ;;
    *)         FILE="$1"; shift ;;
  esac
done

[ -n "$FILE" ] && [ -f "$FILE" ] || { echo "error: HTML file not found: ${FILE:-<none>}" >&2; exit 1; }
command -v gh >/dev/null 2>&1 || { echo "error: gh CLI required (and authenticated)" >&2; exit 1; }
gh auth status >/dev/null 2>&1 || { echo "error: gh not authenticated — run 'gh auth login'" >&2; exit 1; }

BASENAME="$(basename "$FILE")"

if [ -n "$UPDATE_ID" ]; then
  # Update an existing gist in place (keeps the same URL — re-publish flow).
  gh gist edit "$UPDATE_ID" -a "$FILE" >/dev/null 2>&1 \
    || { echo "error: gh gist edit failed for $UPDATE_ID" >&2; exit 1; }
  GID="$UPDATE_ID"
else
  VIS=(); [ "$PUBLIC" = "1" ] && VIS=(--public)   # default: secret/unlisted (raw still public)
  URL="$(gh gist create "$FILE" --desc "$DESC" "${VIS[@]}" 2>/dev/null)" \
    || { echo "error: gh gist create failed" >&2; exit 1; }
  GID="$(basename "$URL")"
fi

OWNER="$(gh api user --jq .login 2>/dev/null)"
GIST_URL="https://gist.github.com/${OWNER}/${GID}"
# Non-SHA raw URL = always the latest gist revision.
RAW="https://gist.githubusercontent.com/${OWNER}/${GID}/raw/${BASENAME}"
PREVIEW="https://htmlpreview.github.io/?${RAW}"

echo "gist_url=${GIST_URL}"
echo "gist_id=${GID}"
echo "raw_url=${RAW}"
echo "preview_url=${PREVIEW}"
