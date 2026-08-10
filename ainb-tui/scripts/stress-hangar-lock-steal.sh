#!/usr/bin/env bash
# Stress proof for the OTHER half of single-instance: a hard-killed daemon must
# not lock its home out forever, and the contenders that reclaim it must not all
# reclaim it at once.
#
# Rust destructors never run after SIGKILL, so the lock file survives naming a
# dead pid. Each round SIGKILLs the owner and then starts a fleet of daemons
# simultaneously: exactly one must steal the stale lock and live. Zero survivors
# means a home wedged by its own debris (the failure mode a naive
# liveness-only guard has after a reboot recycles the pid); more than one means
# the steal is not winner-take-all.
#
# Safety (matches ~/.claude global rules and scripts/reap-stress-codex-orphans.sh):
# every daemon is started BY this script and signalled by its EXACT recorded pid.
# Never pkill/killall/wildcards. Isolated mktemp hangar home; AINB_CODEX_MANAGED=0
# so the codex manager and the desktop Codex/ChatGPT app are never involved.
#
# Usage: scripts/stress-hangar-lock-steal.sh [rounds] [daemons-per-round]
#        (defaults: 20 rounds, 8 daemons)
set -uo pipefail

ROUNDS="${1:-20}"
FLEET="${2:-8}"
REPO="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$REPO/target/debug/ainb-hangar-daemon"

STRESS_HOME="$(mktemp -d)/stress-hangar"
mkdir -p "$STRESS_HOME"
export AINB_HANGAR_HOME="$STRESS_HOME"
export AINB_CODEX_MANAGED=0
LOCK="$STRESS_HOME/hangar/daemon.lock"

STARTED=()
cleanup() {
  for pid in "${STARTED[@]:-}"; do
    [[ -n "$pid" ]] && kill -KILL "$pid" 2>/dev/null
  done
  rm -rf "$(dirname "$STRESS_HOME")" 2>/dev/null
}
trap cleanup EXIT

start_daemon() { "$BIN" >>"$STRESS_HOME/daemon.log" 2>&1 & echo $!; }

wait_for_lock() {
  for _ in $(seq 1 100); do
    [[ -s "$LOCK" ]] && { cat "$LOCK"; return 0; }
    sleep 0.2
  done
  return 1
}

count_alive() {
  local n=0 pid
  for pid in "$@"; do
    kill -0 "$pid" 2>/dev/null && n=$((n + 1))
  done
  echo "$n"
}

if [[ ! -x "$BIN" ]]; then
  echo "Building daemon (debug)..."
  ( cd "$REPO" && cargo build -p ainb-hangar-daemon --bin ainb-hangar-daemon ) || {
    echo "FAIL: build failed"; exit 1; }
fi

# Run a PRIVATE COPY, never the shared target/ path. A `cargo build` or `cargo
# test` running concurrently replaces that file, and daemons launched from it
# then die mid-run with no shutdown line — which reads exactly like the
# double-daemon defect this script exists to detect. Copying removes the
# confound, so a FAIL here always means the lock, never the toolchain.
RUN_BIN="$STRESS_HOME/ainb-hangar-daemon"
cp "$BIN" "$RUN_BIN" || { echo "FAIL: could not stage the daemon binary"; exit 1; }
chmod +x "$RUN_BIN"
BIN="$RUN_BIN"

echo "== stress-hangar-lock-steal: $ROUNDS rounds x $FLEET contenders =="
echo "AINB_HANGAR_HOME=$AINB_HANGAR_HOME"

fail=0
for ((round = 1; round <= ROUNDS; round++)); do
  owner="$(start_daemon)"
  STARTED+=("$owner")
  if ! held="$(wait_for_lock)"; then
    echo "round $round: FAIL — no daemon ever claimed the home"
    fail=1
    kill -KILL "$owner" 2>/dev/null
    continue
  fi

  # SIGKILL by EXACT pid: no destructor runs, so the lock file stays behind
  # naming a pid that is now dead. That debris is the case under test.
  kill -KILL "$owner" 2>/dev/null
  wait "$owner" 2>/dev/null
  if [[ ! -s "$LOCK" ]]; then
    echo "round $round: FAIL — expected a stale lock after SIGKILL"
    fail=1
    continue
  fi

  round_pids=()
  for ((i = 0; i < FLEET; i++)); do
    pid="$(start_daemon)"
    round_pids+=("$pid")
    STARTED+=("$pid")
  done
  sleep 3

  alive="$(count_alive "${round_pids[@]}")"
  if [[ "$alive" -gt 1 ]]; then
    # Distinguish a genuine double-hold from a loser that had not finished
    # exiting yet: give it a grace window and re-count. A real double-hold is
    # stable — both processes are in their run loops and never leave.
    echo "round $round: $alive alive at first count, re-checking after grace..."
    for p in "${round_pids[@]}"; do
      kill -0 "$p" 2>/dev/null && ps -p "$p" -o pid=,stat=,etime=,args= 2>/dev/null | head -1
    done
    sleep 5
    alive="$(count_alive "${round_pids[@]}")"
  fi
  if [[ "$alive" -ne 1 ]]; then
    if [[ "$alive" -eq 0 ]]; then
      echo "round $round: FAIL — the home stayed wedged by the stale lock of $held"
    else
      echo "round $round: FAIL — $alive daemons stole the same stale lock"
    fi
    fail=1
  else
    echo "round $round: ok (stale lock of $held reclaimed by exactly 1 of $FLEET)"
  fi

  for pid in "${round_pids[@]}"; do
    if kill -0 "$pid" 2>/dev/null; then
      kill -TERM "$pid" 2>/dev/null
      for _ in $(seq 1 50); do
        kill -0 "$pid" 2>/dev/null || break
        sleep 0.2
      done
    fi
  done
done

if [[ "$fail" -eq 0 ]]; then
  echo "PASS: every stale lock was reclaimed by exactly one successor"
  exit 0
fi
echo "FAIL: stale-lock reclaim is not winner-take-all"
exit 1
