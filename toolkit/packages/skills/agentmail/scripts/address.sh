#!/usr/bin/env bash
# Print the email address for a known slug.
# Usage: address.sh <slug>

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=_lib.sh
. "$SCRIPT_DIR/_lib.sh"

if [ $# -ne 1 ] || [ "$1" = "-h" ] || [ "$1" = "--help" ]; then
  cat <<EOF
Usage: $(basename "$0") <slug>

Prints the address for the inbox stored under <slug>.
Useful in shell pipelines: ADDR=\$(address.sh mytest)
EOF
  [ $# -ne 1 ] && exit 2 || exit 0
fi

SLUG="$1"
STATE_FILE=$(state_file_for_slug "$SLUG")
if [ ! -f "$STATE_FILE" ]; then
  echo "ERROR: no inbox '$SLUG'" >&2
  exit 1
fi

jq -r '.address' "$STATE_FILE"
