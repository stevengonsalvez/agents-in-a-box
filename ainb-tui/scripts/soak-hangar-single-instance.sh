#!/usr/bin/env bash
# Soak proof: N daemons started at once against ONE hangar home leave exactly
# ONE running. Every other one declines the home and exits 0.
#
# The incident this proves against: 69 `ainb hangar daemon run` processes on one
# machine (65 orphaned at ppid==1, 2.41 GB RSS), which twice exhausted a 32 GB
# Mac. The old guard was a pidfile written at the END of boot and read by a
# different process BEFORE spawning, so every duplicate booted.
#
# Safety (matches ~/.claude global rules and scripts/soak-codex-orphans.sh):
# every daemon here is started BY this script and killed by its EXACT recorded
# pid. Never pkill/killall/wildcards. The hangar home is a fresh mktemp dir, so
# nothing touches ~/.agents-in-a-box, and AINB_CODEX_MANAGED=0 keeps the codex
# manager (and the desktop Codex/ChatGPT app) entirely out of it.
#
# Usage: scripts/soak-hangar-single-instance.sh [rounds] [daemons-per-round]
#        (defaults: 20 rounds, 10 daemons)
set -uo pipefail

ROUNDS="${1:-20}"
FLEET="${2:-10}"
REPO="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$REPO/target/debug/ainb-hangar-daemon"

SOAK_HOME="$(mktemp -d)/soak-hangar"
mkdir -p "$SOAK_HOME"
export AINB_HANGAR_HOME="$SOAK_HOME"
export AINB_CODEX_MANAGED=0

# Every pid this script has ever started, so cleanup can never signal anything
# it did not create.
STARTED=()

cleanup() {
  for pid in "${STARTED[@]:-}"; do
    [[ -n "$pid" ]] && kill -TERM "$pid" 2>/dev/null
  done
  sleep 1
  for pid in "${STARTED[@]:-}"; do
    [[ -n "$pid" ]] && kill -KILL "$pid" 2>/dev/null
  done
  rm -rf "$(dirname "$SOAK_HOME")" 2>/dev/null
}
trap cleanup EXIT

start_daemon() { "$BIN" >>"$SOAK_HOME/daemon.log" 2>&1 & echo $!; }

# How many of the pids passed in are still running (exact pids only).
count_alive() {
  local n=0 pid
  for pid in "$@"; do
    kill -0 "$pid" 2>/dev/null && n=$((n + 1))
  done
  echo "$n"
}

# Poll until exactly one of the given pids is left, or the deadline passes.
#
# The property under test is "exactly one daemon ENDS UP running", not "exactly
# one is running at some fixed instant". A loser is a real process: it has to be
# exec'd, linked and scheduled before it can even look at the lock, and under a
# loaded box with a fleet starting at once that can outlast a fixed sleep. Poll
# for the settled state instead, so a slow decline reads as slow rather than as
# the double-hold this script exists to detect.
SETTLE_TIMEOUT="${SETTLE_TIMEOUT:-30}"
wait_until_settled() {
  local deadline=$((SECONDS + SETTLE_TIMEOUT)) alive
  while true; do
    alive="$(count_alive "$@")"
    [[ "$alive" -eq 1 ]] && { echo "$alive"; return 0; }
    [[ "$SECONDS" -ge "$deadline" ]] && { echo "$alive"; return 1; }
    sleep 0.5
  done
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
RUN_BIN="$SOAK_HOME/ainb-hangar-daemon"
cp "$BIN" "$RUN_BIN" || { echo "FAIL: could not stage the daemon binary"; exit 1; }
chmod +x "$RUN_BIN"
BIN="$RUN_BIN"

echo "== soak-hangar-single-instance: $ROUNDS rounds x $FLEET daemons =="
echo "AINB_HANGAR_HOME=$AINB_HANGAR_HOME"

worst=0
fail=0
for ((round = 1; round <= ROUNDS; round++)); do
  round_pids=()
  for ((i = 0; i < FLEET; i++)); do
    pid="$(start_daemon)"
    round_pids+=("$pid")
    STARTED+=("$pid")
  done

  alive="$(wait_until_settled "${round_pids[@]}")" || {
    # Never settled. Print who is still up so a genuine double-hold is
    # diagnosable after the fact — a real one is STABLE, both processes sitting
    # in their run loops.
    echo "round $round: did not settle to 1 within ${SETTLE_TIMEOUT}s; still up:"
    for p in "${round_pids[@]}"; do
      kill -0 "$p" 2>/dev/null && ps -p "$p" -o pid=,stat=,etime=,args= 2>/dev/null | head -1
    done
  }
  [[ "$alive" -gt "$worst" ]] && worst="$alive"
  if [[ "$alive" -ne 1 ]]; then
    echo "round $round: FAIL — $alive daemons alive, expected exactly 1"
    fail=1
  else
    echo "round $round: ok (1 of $FLEET survived)"
  fi

  # Stop the survivor by its EXACT pid so the next round starts from a free home.
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

echo "worst-case concurrent daemons in one round: $worst"
if [[ "$fail" -eq 0 ]]; then
  echo "PASS: a single hangar home never had more than one daemon"
  exit 0
fi
echo "FAIL: duplicate daemons survived on one home"
exit 1
