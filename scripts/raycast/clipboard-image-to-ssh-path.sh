#!/bin/bash
# Required parameters:
# @raycast.schemaVersion 1
# @raycast.title Clipboard Image -> SSH Path
# @raycast.mode silent
# @raycast.packageName Stevie Utils
# @raycast.icon 📋
# @raycast.description Copy clipboard image to remote Mac /tmp over Tailscale SSH; put bare /tmp/... path on clipboard for pasting into Claude SSH session

set -euo pipefail

REMOTE_HOST="stevens-macbook-pro-5"
REMOTE_DIR="/tmp"
FILE_PREFIX="cc-paste"

fail() {
  echo "$1" >&2
  exit 1
}

command -v pngpaste >/dev/null 2>&1 || fail "pngpaste missing — brew install pngpaste"
command -v tailscale >/dev/null 2>&1 || fail "tailscale missing"

if ! tailscale status --json >/dev/null 2>&1; then
  fail "tailscale not running"
fi

if ! tailscale status --json | /usr/bin/python3 -c '
import json, sys
j = json.load(sys.stdin)
peer = next((p for p in j.get("Peer", {}).values()
             if p.get("HostName") == "Stevens-MacBook-Pro-5"
             or p.get("DNSName", "").startswith("stevens-macbook-pro-5.")), None)
raise SystemExit(0 if peer and peer.get("Online") else 1)
'; then
  fail "remote host offline: ${REMOTE_HOST}"
fi

local_tmp="$(mktemp "/tmp/${FILE_PREFIX}.XXXXXX.png")"
trap 'rm -f "$local_tmp"' EXIT

if ! pngpaste "$local_tmp" 2>/dev/null; then
  fail "No image in clipboard"
fi

remote_name="${FILE_PREFIX}-$(date +%Y%m%d-%H%M%S)-$RANDOM.png"
remote_path="${REMOTE_DIR}/${remote_name}"

if ! tailscale ssh "$REMOTE_HOST" "cat > $(printf '%q' "$remote_path")" < "$local_tmp"; then
  fail "Copy to remote failed"
fi

if ! tailscale ssh "$REMOTE_HOST" "test -s $(printf '%q' "$remote_path")"; then
  fail "Remote file verify failed"
fi

printf "%s" "$remote_path" | pbcopy
printf "Copied: %s\n" "$remote_path"
