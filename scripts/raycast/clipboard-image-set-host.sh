#!/bin/bash
# Required parameters:
# @raycast.schemaVersion 1
# @raycast.title Set Clipboard Host
# @raycast.mode silent
# @raycast.packageName Stevie Utils
# @raycast.icon 🎯
# @raycast.description Choose where "Clipboard Image -> Host Path" copies to. Persists the choice.
#
# Optional parameters:
# @raycast.argument1 { "type": "dropdown", "placeholder": "Host", "data": [{ "title": "This Mac (no ssh)", "value": "this" }, { "title": "Mac (mac-lan)", "value": "mac" }, { "title": "Hetzner (claude@claude-hetzner)", "value": "hetzner" }] }

set -euo pipefail

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)/_cc-paste-lib.sh"

KEY="${1:-}"
HOST_SPEC="$(cc_resolve "$KEY")"
[ -n "$HOST_SPEC" ] || { echo "unknown host: $KEY" >&2; exit 1; }

mkdir -p "$(dirname "$CC_STATE_FILE")"
printf '%s' "$KEY" > "$CC_STATE_FILE"

printf "SSH host set to %s (%s)\n" "$KEY" "${HOST_SPEC%%|*}"
