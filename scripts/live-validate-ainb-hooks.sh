#!/usr/bin/env bash
# Live-host validation for ainb-hooks. Confirms the install + wiring
# proof referenced in the goal's success criterion #1:
#
#   "real claude / codex session produces a row in
#    ~/.agents-in-a-box/notifications.db with correct agent + raw_event"
#
# Run from a clean shell on your dev host. Walks through:
#
#   1. Install for both agents
#   2. Start the daemon
#   3. Fire a synthetic Claude-shape hook
#   4. Fire a synthetic Codex-shape hook
#   5. Print rows from notifications.db
#   6. Stop the daemon
#
# Pass `--real-session` to instead pause at step 3 so you can launch a
# real Claude / Codex CLI session manually and observe the row land.

set -euo pipefail

REAL_SESSION=0
for arg in "$@"; do
  case "$arg" in
    --real-session) REAL_SESSION=1 ;;
    -h|--help)
      sed -n '2,/^set -euo/p' "$0" | sed 's/^# \?//'
      exit 0
      ;;
  esac
done

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
NOTIFYD="${AINB_NOTIFYD_BIN:-$REPO_ROOT/ainb-tui/target/debug/ainb-notifyd}"
HOOK="${HOME}/.agents-in-a-box/hooks/notify.sh"
DB="${HOME}/.agents-in-a-box/notifications.db"

if ! [ -x "$NOTIFYD" ]; then
  echo "building ainb-notifyd..."
  (cd "$REPO_ROOT/ainb-tui" && cargo build -p ainb-plugin-notifyd --bin ainb-notifyd)
fi

echo "==> 1. install --all"
"$NOTIFYD" install --all

echo ""
echo "==> 2. start daemon in background"
"$NOTIFYD" run > "${HOME}/.agents-in-a-box/notify.log" 2>&1 &
DAEMON_PID=$!
# Give it up to 2s to bind the socket.
for _ in $(seq 1 200); do
  [ -S "${HOME}/.agents-in-a-box/notify.sock" ] && break
  sleep 0.01
done
if ! [ -S "${HOME}/.agents-in-a-box/notify.sock" ]; then
  echo "FAIL: socket did not appear within 2s" >&2
  kill -TERM "$DAEMON_PID" 2>/dev/null || true
  exit 1
fi
echo "    daemon pid=$DAEMON_PID, socket up"

if [ "$REAL_SESSION" = "1" ]; then
  echo ""
  echo "==> [pause] launch a real Claude or Codex CLI session NOW."
  echo "    Complete a turn, ask the agent for permission, etc."
  echo "    Press <Enter> when you've fired some events to inspect the DB."
  read -r _
else
  echo ""
  echo "==> 3. fire synthetic Claude-shape hook (Notification:idle_prompt)"
  printf '%s\n' '{"hook_event_name":"Notification","matcher":"idle_prompt","session_id":"live-validate-claude","cwd":"'"$PWD"'","payload":{"message":"Input needed"}}' \
    | AINB_AGENT=claude bash "$HOOK"

  echo ""
  echo "==> 4. fire synthetic Codex-shape hook (agent-turn-complete)"
  AINB_AGENT=codex bash "$HOOK" '{"type":"agent-turn-complete","session_id":"live-validate-codex","cwd":"'"$PWD"'"}'

  # Give the daemon a tick to flush.
  sleep 0.3
fi

echo ""
echo "==> 5. notifications.db contents"
if command -v sqlite3 >/dev/null 2>&1; then
  sqlite3 -header -column "$DB" \
    "SELECT substr(id, 1, 8) || '...' AS id, agent, raw_event, session_id, project FROM notifications ORDER BY ts;"
else
  echo "    (install sqlite3 to inspect; falling back to status)"
  "$NOTIFYD" status
fi

echo ""
echo "==> 6. ainb-notifyd status"
"$NOTIFYD" status

echo ""
echo "==> stopping daemon (pid $DAEMON_PID)"
"$NOTIFYD" stop || kill -TERM "$DAEMON_PID" 2>/dev/null || true
wait "$DAEMON_PID" 2>/dev/null || true

echo ""
echo "==> done. logs: ~/.agents-in-a-box/notify.log"
echo "    uninstall with: $NOTIFYD uninstall --all"
