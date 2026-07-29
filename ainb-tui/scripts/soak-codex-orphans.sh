#!/usr/bin/env bash
# Soak proof for success criterion 1: repeatedly hard-killing (SIGKILL) the
# hangar daemon does NOT let orphaned `codex app-server` processes accumulate.
# The pre-fix incident reached 900+ here; with bound_by_child (no duplicate
# spawn) plus boot-time adoption of the shared server, the count stays bounded
# at ~1 no matter how many times the daemon is killed.
#
# Note: when NO daemon is running, one shared-socket server lingers at ppid==1.
# That is by design under the daemon-owns-cleanup model: the next daemon boot
# ADOPTS it (reuse), so it never accumulates; a running daemon also reaps every
# OTHER orphan on its boot + periodic sweep. This soak proves "no accumulation",
# not "exactly zero while idle".
#
# Safety (matches ~/.claude global rules): the daemon is killed by its EXACT pid
# only ($! of the process we start). Never pkill/killall/wildcard. This script
# only COUNTS orphans machine-wide (the success metric); it kills only the
# daemons it starts and the one server bound to its own isolated test home.
#
# Usage: scripts/soak-codex-orphans.sh [iterations]   (default 20)
set -uo pipefail

ITERS="${1:-20}"
REPO="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$REPO/target/debug/ainb-hangar-daemon"

# Isolate the hangar home so we never touch the real ~/.agents-in-a-box socket/db.
SOAK_HOME="$(mktemp -d)/soak-agents-in-a-box"
mkdir -p "$SOAK_HOME"
export AINB_HANGAR_HOME="$SOAK_HOME"
cleanup() {
  # kill any codex server still bound to this test home (the lingering shared one)
  for pid in $(ps -Ao pid,args | grep "$SOAK_HOME" | grep -i codex | grep -v grep | awk '{print $1}'); do
    kill -TERM "$pid" 2>/dev/null
  done
  rm -rf "$(dirname "$SOAK_HOME")" 2>/dev/null
}
trap cleanup EXIT

# Global count of orphaned codex app-server processes (the exact success metric).
orphan_count() {
  ps -Ao pid,ppid,args | grep '[b]in/codex' | grep -v grep | awk '$2==1' \
    | grep -c 'app-server'
}
desktop_app_alive() {
  ps -Ao pid,ppid,args | grep -i 'ChatGPT.app/Contents/Resources/codex' \
    | grep -v grep | grep -qi 'app-server' && echo yes || echo no
}
start_daemon() { "$BIN" >>"$SOAK_HOME/daemon.log" 2>&1 & echo $!; }

if [[ ! -x "$BIN" ]]; then
  echo "Building daemon (debug)..."
  ( cd "$REPO" && cargo build -p ainb-hangar-daemon --bin ainb-hangar-daemon ) || {
    echo "FAIL: build failed"; exit 1; }
fi

echo "== soak-codex-orphans: $ITERS iterations =="
echo "AINB_HANGAR_HOME=$AINB_HANGAR_HOME"
echo "desktop app server alive at start: $(desktop_app_alive)"
echo "baseline orphan count: $(orphan_count)"

# A codex app-server reaches ppid==1 only when its parent launcher dies. In this
# isolated, auth-less run initialize never completes, so the daemon's spawn
# attempt fails and its cleanup kills the launcher, orphaning the native server to
# ppid==1 a few seconds after boot (the same state a SIGKILLed daemon leaves).
# Poll for that orphan so the SIGKILL always lands after it exists.
server_spawned() {
  ps -Ao pid,ppid,args | grep 'app-server --listen' | grep -v grep \
    | grep -q "$SOAK_HOME/codex-app-server.sock"
}
wait_for_server() {
  for _ in $(seq 1 80); do server_spawned && return 0; sleep 0.25; done
  return 1
}

max=0
for i in $(seq 1 "$ITERS"); do
  pid="$(start_daemon)"
  if wait_for_server; then armed="armed"; else armed="NO-SERVER"; fi
  kill -9 "$pid" 2>/dev/null   # SIGKILL: Drop never runs; the app-server is left at ppid==1.
  wait "$pid" 2>/dev/null
  n="$(orphan_count)"
  [[ "$n" -gt "$max" ]] && max="$n"
  printf '  iter %2d/%s: killed daemon pid %s (%s), orphans now %s\n' \
    "$i" "$ITERS" "$pid" "$armed" "$n"
done

echo "-- peak orphan count across $ITERS SIGKILLs: $max"
echo "   (bound_by_child + boot-time adoption bound this to ~1; the pre-fix"
echo "    incident reached 900+ here). The single shared server that lingers when"
echo "    no daemon runs is reused via adoption on the next boot, not accumulated."
echo "desktop app server alive at end: $(desktop_app_alive)"

# A running daemon reaps every OTHER orphan; the point of THIS test is that 20
# hard kills never make the count grow. Allow a tiny straggler bound; anything
# larger would be accumulation.
if [[ "$max" -ge 1 && "$max" -le 3 ]]; then
  echo "PASS: orphan count stayed bounded (peak $max) across $ITERS SIGKILLs; no accumulation."
  exit 0
elif [[ "$max" -eq 0 ]]; then
  echo "INCONCLUSIVE: the leak never reproduced (peak 0): window too short or codex unavailable."
  exit 2
else
  echo "FAIL: orphan count accumulated to $max across $ITERS SIGKILLs."
  exit 1
fi
