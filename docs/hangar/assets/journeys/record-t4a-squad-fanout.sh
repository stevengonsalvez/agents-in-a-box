#!/bin/bash
# Recording driver for tcp T4a (squad card, ONE owner), one of the remaining tcp
# journeys in the converged-control-center catalogue
# (docs/hangar/verify-converged-goal.md).
#
# Seeds the exact fixture the `tripwire_tcp_squad_card_fanout_e2e` tripwire
# drives (via `seed_t4a_squad_fanout_journey`): a real git repo in the `@` scan
# cache, a `shippers` squad (leader `agent-1` + members `agent-m1`/`agent-m2`), and
# a card ALREADY assigned to that squad on the `Delivery` board. The tape opens
# Boards (`B`), runs the squad card (`Enter`, `Enter` for Headless), and holds the
# run live while a background poller shell-verifies the worktree
# (`git branch --list 'ainb/*'` in the seeded repo + the worktree dir on disk).
# A background STATE-DRIVEN release (polls `agent_task_queue` for the `running`
# row, not a blind sleep, since vhs's own startup latency is not fully
# predictable) then touches `$HOME/interactive-go` once the run is actually
# observed running, so the tape also captures the clean teardown.
#
# CHANGED with migration 0074: this journey used to wait for THREE `running` rows,
# because a squad card BROADCAST one run per member. That was the reported defect,
# not a feature, and dispatch is now a role-gated pull with one owner per card. A
# poller still waiting for 3 would never fire and the tape would hang, so the
# threshold is 1. The committed GIF still shows the old three-worktree behaviour
# and is stale until this script is re-run.
set -euo pipefail

CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/Users/stevengonsalvez/.cache/ccc-shared-target}"
W=/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_feat_hangar-parity/ainb-tui
ASSETS="$(cd "$(dirname "$0")" && pwd)"
AINB="$CARGO_TARGET_DIR/debug/ainb"
DAEMON="$CARGO_TARGET_DIR/debug/ainb-hangar-daemon"
SEEDER="$CARGO_TARGET_DIR/debug/examples/seed_t4a_squad_fanout_journey"
PLUGIN_ROOT="$(dirname "$CARGO_TARGET_DIR")/dist/plugins"

for b in "$AINB" "$DAEMON" "$SEEDER"; do
  [ -x "$b" ] || {
    echo "missing binary: $b — build first: (cd $W && cargo build -p ainb -p ainb-hangar-daemon --example seed_t4a_squad_fanout_journey)" >&2
    exit 2
  }
done

HOME_DIR=$(mktemp -d /tmp/t4af.XXXXXX)
SEED_OUT="$("$SEEDER" "$HOME_DIR" "$DAEMON")"
echo "$SEED_OUT"
DAEMON_PID="$(printf '%s\n' "$SEED_OUT" | sed -n 's/^DAEMON_PID=//p')"

REPO="$HOME_DIR/testrepo"
WT_ROOT="$HOME_DIR/.agents-in-a-box/worktrees"
DB="$HOME_DIR/.agents-in-a-box/hangar.db"
POLL_LOG="$(mktemp /tmp/t4a-worktree-poll.XXXXXX.log)"

# Background poller: every 0.5s for up to 100s, record how many worktree dirs are
# live + which `ainb/<slug>` branches are registered in the seeded repo. This is
# the shell-verification evidence for the "N live worktrees at once" claim — the
# GIF shows the on-screen member chips, this log proves the filesystem/git state
# behind them.
(
  set +e +o pipefail
  for _ in $(seq 1 200); do
    if [ -d "$WT_ROOT" ]; then
      names=$(find "$WT_ROOT" -mindepth 1 -maxdepth 1 -type d ! -name by-name ! -name by-session 2>/dev/null | sed "s#$WT_ROOT/##" | tr '\n' ',')
      count=$(printf '%s' "$names" | tr ',' '\n' | grep -c .)
      branches=$(cd "$REPO" 2>/dev/null && git branch --list 'ainb/*' 2>/dev/null | tr -d ' *+' | tr '\n' ',')
      echo "$(date +%s.%N) live_worktree_dirs=$count names=[$names] branches=[$branches]" >> "$POLL_LOG"
    fi
    sleep 0.5
  done
) &
POLL_BG=$!

# Background STATE-DRIVEN release: wait until the DB actually shows all three
# the task `running` (never a blind sleep, since vhs's own startup latency varies
# run to run), hold 6s more so the tape's "running" screenshot lands, then touch
# the sentinel to release the three blocked fake-claude agents. `set +e` so a
# transient sqlite3/find hiccup can never silently kill this loop early (it did,
# during earlier passes at this script, before this hardening).
(
  set +e +o pipefail
  for _ in $(seq 1 240); do
    n=$(sqlite3 "$DB" "SELECT COUNT(*) FROM agent_task_queue WHERE issue_id='issue-squad-card' AND status='running';" 2>/dev/null)
    if [ "${n:-0}" -ge 1 ] 2>/dev/null; then
      echo "$(date +%s.%N) observed $n/1 running task; holding 6s before release" >> "$POLL_LOG"
      sleep 6
      touch "$HOME_DIR/interactive-go"
      echo "$(date +%s.%N) released interactive-go" >> "$POLL_LOG"
      break
    fi
    sleep 0.5
  done
) &
RELEASE_BG=$!

TAPE="$ASSETS/t4-squad-fanout.tape"
cat > "$TAPE" <<EOF
# T4a — squad-card fan-out (verify-converged-goal.md journey catalogue).
Output "t4-squad-fanout.gif"

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

# --- Boards (B): the squad card, unblocked, not yet run ---
Type "B"
Sleep 3s
Screenshot "t4a-1-board-squad-card.png"

# --- Run (Enter opens Run ▾, Enter again commits Headless) ---
Enter
Sleep 1s
Enter
Sleep 4s
Screenshot "t4a-2-owner-running.png"

# --- hold: three fanned tasks blocked on the release sentinel, each on its own
#     live ainb/<slug> worktree (shell-verified in t4a-worktree-poll.log); the
#     state-driven release fires ~6s after it observes the run ---
Sleep 14s
Screenshot "t4a-3-fanout-done.png"
Sleep 1s
EOF

cd "$ASSETS"
vhs t4-squad-fanout.tape
echo "vhs done"

wait "$RELEASE_BG" 2>/dev/null || true
kill "$POLL_BG" 2>/dev/null || true
wait "$POLL_BG" 2>/dev/null || true

echo "--- worktree poll log ---"
cat "$POLL_LOG"
rm -f "$POLL_LOG"

# --- teardown: kill only the daemon PID the seeder printed ---
if [ -n "${DAEMON_PID:-}" ] && kill -0 "$DAEMON_PID" 2>/dev/null; then
  kill -9 "$DAEMON_PID" 2>/dev/null || true
  echo "killed daemon $DAEMON_PID"
fi
rm -rf "$HOME_DIR"
echo "teardown complete"
