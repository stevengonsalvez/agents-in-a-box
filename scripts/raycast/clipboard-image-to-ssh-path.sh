#!/bin/bash
# Required parameters:
# @raycast.schemaVersion 1
# @raycast.title Clipboard Image -> SSH Path
# @raycast.mode silent
# @raycast.packageName Stevie Utils
# @raycast.icon 📋
# @raycast.description Copy clipboard image to the current host's /tmp over Tailscale SSH; put bare /tmp/... path on clipboard. Change host with "Set SSH Host".

set -euo pipefail

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)/_cc-paste-lib.sh"

HOST_KEY="$(cc_current_key)"
HOST_SPEC="$(cc_resolve "$HOST_KEY")"
REMOTE_HOST="${HOST_SPEC%%|*}"
PEER_NAME="${HOST_SPEC##*|}"

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

if ! tailscale status --json | PEER_NAME="$PEER_NAME" /usr/bin/python3 -c '
import json, os, sys
want = os.environ["PEER_NAME"].lower()
j = json.load(sys.stdin)
peer = next((p for p in j.get("Peer", {}).values()
             if p.get("HostName", "").lower() == want
             or p.get("DNSName", "").lower().startswith(want + ".")), None)
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
printf "Copied to %s: %s\n" "$HOST_KEY" "$remote_path"
