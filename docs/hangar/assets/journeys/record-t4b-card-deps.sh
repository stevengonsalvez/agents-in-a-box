#!/bin/bash
# Recording driver for tcp T4b (card-dependency chain) — one of the remaining
# tcp journeys in the converged-control-center catalogue
# (docs/hangar/verify-converged-goal.md).
#
# Seeds the exact fixture the `tripwire_tcp_card_dependency_chain_e2e` tripwire
# drives (via `seed_t4b_card_deps_journey`): two cards on the `Delivery` board's
# Todo column — A (`DepBlockerA`) and B (`DepDependentB`, depends-on A, auto-run
# ON). The tape opens Boards (`B`), focuses B (blocked, renders 🔒[issue-dep-a]),
# attempts Run and captures the REFUSAL, then focuses A and runs IT — a
# background STATE-DRIVEN release (polls `agent_task_queue` for A's status, not a
# blind sleep) touches `$HOME/interactive-go` once A is observed running, so A
# finalizes to done, the finalize seam AUTO-LAUNCHES B (its last blocker is now
# done), and the tape's final screenshot captures both cards done.
set -euo pipefail

CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/Users/stevengonsalvez/.cache/ccc-shared-target}"
W=/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_feat_hangar-parity/ainb-tui
ASSETS="$(cd "$(dirname "$0")" && pwd)"
AINB="$CARGO_TARGET_DIR/debug/ainb"
DAEMON="$CARGO_TARGET_DIR/debug/ainb-hangar-daemon"
SEEDER="$CARGO_TARGET_DIR/debug/examples/seed_t4b_card_deps_journey"
PLUGIN_ROOT="$(dirname "$CARGO_TARGET_DIR")/dist/plugins"

for b in "$AINB" "$DAEMON" "$SEEDER"; do
  [ -x "$b" ] || {
    echo "missing binary: $b — build first: (cd $W && cargo build -p ainb -p ainb-hangar-daemon --example seed_t4b_card_deps_journey)" >&2
    exit 2
  }
done

HOME_DIR=$(mktemp -d /tmp/t4bd.XXXXXX)
SEED_OUT="$("$SEEDER" "$HOME_DIR" "$DAEMON")"
echo "$SEED_OUT"
DAEMON_PID="$(printf '%s\n' "$SEED_OUT" | sed -n 's/^DAEMON_PID=//p')"

DB="$HOME_DIR/.agents-in-a-box/hangar.db"
POLL_LOG="$(mktemp /tmp/t4b-dep-poll.XXXXXX.log)"

# Background state poller: every 0.5s, record A's + B's task status/count — the
# shell-verification evidence for "B gains no task until A completes, then
# auto-runs".
(
  set +e +o pipefail
  for _ in $(seq 1 200); do
    a_status=$(sqlite3 "$DB" "SELECT status FROM agent_task_queue WHERE issue_id='issue-dep-a' ORDER BY created_at DESC, id DESC LIMIT 1;" 2>/dev/null)
    b_count=$(sqlite3 "$DB" "SELECT COUNT(*) FROM agent_task_queue WHERE issue_id='issue-dep-b';" 2>/dev/null)
    b_status=$(sqlite3 "$DB" "SELECT status FROM agent_task_queue WHERE issue_id='issue-dep-b' ORDER BY created_at DESC, id DESC LIMIT 1;" 2>/dev/null)
    echo "$(date +%s.%N) a_status=[$a_status] b_task_count=$b_count b_status=[$b_status]" >> "$POLL_LOG"
    sleep 0.5
  done
) &
POLL_BG=$!

# Background STATE-DRIVEN release: wait until A is observed `running` (never a
# blind sleep), hold 4s so the tape's screenshot lands, then release the
# blocking fake-claude — A finalizes to done, the finalize seam auto-runs B.
(
  set +e +o pipefail
  for _ in $(seq 1 240); do
    st=$(sqlite3 "$DB" "SELECT status FROM agent_task_queue WHERE issue_id='issue-dep-a' ORDER BY created_at DESC, id DESC LIMIT 1;" 2>/dev/null)
    if [ "$st" = "running" ]; then
      echo "$(date +%s.%N) observed A running; holding 4s before release" >> "$POLL_LOG"
      sleep 4
      touch "$HOME_DIR/interactive-go"
      echo "$(date +%s.%N) released interactive-go" >> "$POLL_LOG"
      break
    fi
    sleep 0.5
  done
) &
RELEASE_BG=$!

TAPE="$ASSETS/t4-card-deps.tape"
cat > "$TAPE" <<EOF
# T4b — card-dependency chain (verify-converged-goal.md journey catalogue).
Output "t4-card-deps.gif"

Set Shell "bash"
Set FontSize 13
Set Width 2200
Set Height 1000
Set Padding 12

Hide
Type "HOME='$HOME_DIR' AINB_PLUGIN_ROOT='$PLUGIN_ROOT' exec '$AINB' tui"
Enter
Sleep 12s
Show

Type "g"
Sleep 4s

# --- Boards (B): A unblocked, B depends-on A (renders 🔒[issue-dep-a]) ---
Type "B"
Sleep 3s
Screenshot "t4b-1-board-both-cards.png"

# --- focus B (j), attempt Run -> REFUSED (daemon note: "run failed: blocked ...") ---
Type "j"
Sleep 1s
Enter
Sleep 1s
Enter
Sleep 3s
Screenshot "t4b-2-blocked-refused.png"

# --- focus back to A (k), Run it (Headless) ---
Type "k"
Sleep 1s
Enter
Sleep 1s
Enter
Sleep 3s

# --- hold: A blocked on the release sentinel; state-driven release fires ~4s
#     after A is observed running (shell-verified in the poll log) ---
Sleep 18s
Screenshot "t4b-3-both-done-autoran.png"
Sleep 1s
EOF

cd "$ASSETS"
vhs t4-card-deps.tape
echo "vhs done"

wait "$RELEASE_BG" 2>/dev/null || true
kill "$POLL_BG" 2>/dev/null || true
wait "$POLL_BG" 2>/dev/null || true

echo "--- dependency-chain poll log ---"
cat "$POLL_LOG"
rm -f "$POLL_LOG"

# --- teardown: kill only the daemon PID the seeder printed ---
if [ -n "${DAEMON_PID:-}" ] && kill -0 "$DAEMON_PID" 2>/dev/null; then
  kill -9 "$DAEMON_PID" 2>/dev/null || true
  echo "killed daemon $DAEMON_PID"
fi
rm -rf "$HOME_DIR"
echo "teardown complete"
