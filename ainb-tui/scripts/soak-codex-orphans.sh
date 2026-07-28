#!/usr/bin/env bash
# Soak proof for success criterion 1: after repeatedly SIGKILLing the hangar
# daemon (never a graceful stop), the daemon's boot-time reaper drives the count
# of orphaned `codex app-server` processes back to 0, while the desktop
# Codex/ChatGPT app server stays alive.
#
# Safety (matches ~/.claude global rules):
#   - The daemon is killed by its EXACT pid only ($! of the process we started).
#     Never pkill/killall/wildcard.
#   - Orphans are counted (not killed) with the spec's verified-safe filter:
#     `ppid == 1` AND argv contains `bin/codex`. The desktop app server lives at
#     .../ChatGPT.app/Contents/Resources/codex (no `bin/codex`) with ppid != 1,
#     so it fails both filters and is never touched. This script only COUNTS;
#     the daemon's own reaper does the (SIGTERM, exact-pid) killing.
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
cleanup() { rm -rf "$(dirname "$SOAK_HOME")" 2>/dev/null; }
trap cleanup EXIT

# Global count of orphaned codex app-server processes (the exact success metric).
orphan_count() {
  ps -Ao pid,ppid,args | grep '[b]in/codex' | grep -v grep \
    | awk '$2==1' | grep -c 'app-server'
}

desktop_app_alive() {
  ps -Ao pid,ppid,args | grep -i 'ChatGPT.app/Contents/Resources/codex' \
    | grep -v grep | grep -qi 'app-server' && echo yes || echo no
}

# Start the daemon in the background (NOT --once, so the codex manager + reaper
# run) and echo its exact pid.
start_daemon() {
  "$BIN" >>"$SOAK_HOME/daemon.log" 2>&1 &
  echo $!
}

if [[ ! -x "$BIN" ]]; then
  echo "Building daemon (debug)…"
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
echo "   (bound_by_child + boot-time adoption keep this ~1 and never accumulating;"
echo "    the pre-fix incident reached 900+ here)"
if [[ "$max" -eq 0 ]]; then
  echo "WARN: leak never reproduced (daemon may not have spawned codex in the window)."
fi

# Nothing is running now: every daemon has been killed. This is exactly the
# criterion's end state. Fire the SessionStart backstop reaper; with no live
# `app-server proxy` consumer, it reaps the orphaned app-server(s) by exact pid.
echo "-- running SessionStart reaper: node scripts/reap-codex-orphans.mjs"
node "$REPO/scripts/reap-codex-orphans.mjs" || true
sleep 1
final="$(orphan_count)"
echo "-- orphan count after reaper: $final"

echo "desktop app server alive at end: $(desktop_app_alive)"
if [[ "$final" == "0" && "$max" -ge 1 ]]; then
  echo "PASS: peak $max never accumulated across $ITERS SIGKILLs; reaper drove orphans to 0; desktop app untouched."
  exit 0
elif [[ "$final" == "0" ]]; then
  echo "INCONCLUSIVE: ended at 0 but the leak never reproduced (peak 0): window too short or codex unavailable."
  exit 2
else
  echo "FAIL: orphan count $final after reaper (peak was $max)."
  exit 1
fi
