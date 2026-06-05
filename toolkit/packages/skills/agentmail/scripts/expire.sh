#!/usr/bin/env bash
# Remove the state file for a slug (local cleanup). The inbox at AgentMail
# still auto-expires after 24h regardless — this just forgets it locally.
# Usage: expire.sh <slug>

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=_lib.sh
. "$SCRIPT_DIR/_lib.sh"

if [ $# -ne 1 ] || [ "$1" = "-h" ] || [ "$1" = "--help" ]; then
  cat <<EOF
Usage: $(basename "$0") <slug>

Deletes the local state file for <slug>. The disposable inbox at AgentMail
auto-expires after 24h on its own — this script only forgets the slug
locally so you can recreate under the same name.
EOF
  [ $# -ne 1 ] && exit 2 || exit 0
fi

SLUG="$1"
STATE_FILE=$(state_file_for_slug "$SLUG")
if [ ! -f "$STATE_FILE" ]; then
  echo "No state for '$SLUG' (already gone)"
  exit 0
fi

rm -f "$STATE_FILE"
echo "Removed state for '$SLUG'"
