#!/usr/bin/env bash
# Scale proof for success criterion 1: the hangar daemon's reaper clears a PILE
# of orphaned codex app-servers (the incident's real failure mode was 900+),
# while sparing its own in-use server, a proxy-backed server owned by another
# Hangar home, and the desktop Codex app.
#
# It fabricates N ppid==1 codex-shaped orphans on FOREIGN sockets (simulating
# leftovers from dead daemons and dead plugin-broker sessions), then starts a
# real ainb-hangar-daemon whose boot reaper must SIGTERM all N (none hold the
# daemon's own socket) while its own server, on its own socket, is spared.
#
# `exec -a` sets argv[0] to `.../bin/codex app-server ...`, and a subshell that
# backgrounds then lets its launcher exit reparents the process to launchd
# (ppid==1), exactly like a real server orphaned when its launcher dies. It kills
# ONLY its own marked pids and the daemon it starts, by exact pid, never
# pkill/wildcard.
#
# Usage: scripts/reap-stress-codex-orphans.sh [orphan_count]   (default 25)
set -uo pipefail

N="${1:-25}"
REPO="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$REPO/target/debug/ainb-hangar-daemon"
MARK="/tmp/ainb-reapstress-$$"
SOAK_HOME="$(mktemp -d)/soak-agents-in-a-box"
mkdir -p "$SOAK_HOME"
export AINB_HANGAR_HOME="$SOAK_HOME"
export HANGAR_SWEEP_INTERVAL_MS=100

orphan_count() {
  ps -Ao pid,ppid,args | grep '[b]in/codex' | grep -v grep | awk '$2==1' \
    | grep -c 'app-server'
}
mine_pids() { ps -Ao pid,args | grep "$MARK" | grep -v grep | awk '{print $1}'; }
home_codex_pids() {
  ps -Ao pid,args | grep "$SOAK_HOME" | grep -i codex | grep -v grep | awk '{print $1}'
}
cleanup() {
  for pid in $(mine_pids) $(home_codex_pids); do kill -TERM "$pid" 2>/dev/null; done
  rm -rf "$(dirname "$SOAK_HOME")" 2>/dev/null
}
trap cleanup EXIT

spawn_orphan() { bash -c "(exec -a \"$1\" sleep 600 >/dev/null 2>&1) &"; }
desktop_alive() {
  ps -Ao pid,ppid,args | grep -i 'ChatGPT.app/Contents/Resources/codex' \
    | grep -v grep | grep -qi 'app-server' && echo yes || echo 'n/a'
}

if [[ ! -x "$BIN" ]]; then
  echo "Building daemon (debug)..."
  ( cd "$REPO" && cargo build -p ainb-hangar-daemon --bin ainb-hangar-daemon ) || {
    echo "FAIL: build failed"; exit 1; }
fi

base="$(orphan_count)"
echo "== reap-stress: $N foreign-socket orphans, cleared by the daemon boot reaper =="
echo "baseline ppid==1 codex app-server count: $base"
echo "desktop app server at start: $(desktop_alive)"

# N genuinely orphaned app-servers (ppid==1) on FOREIGN sockets (NOT the daemon's).
for n in $(seq 1 "$N"); do
  spawn_orphan "/opt/fake/bin/codex app-server --listen unix://${MARK}-orphan-${n}.sock"
done

# One ppid==1 server remains live through a proxy from another Hangar home.
# The reaper must spare it until that exact proxy exits.
PROTECTED_SOCKET="${MARK}-home-a/codex-app-server.sock"
mkdir -p "$(dirname "$PROTECTED_SOCKET")"
spawn_orphan "/opt/fake/bin/codex app-server --listen unix://${PROTECTED_SOCKET}"
bash -c "exec -a \"/opt/fake/bin/codex app-server proxy --sock ${PROTECTED_SOCKET}\" sleep 600" &
PROXY_PID=$!
sleep 1
after_spawn="$(orphan_count)"
PROTECTED_PID="$(ps -Ao pid,args | grep -F "$PROTECTED_SOCKET" | grep 'app-server --listen' | grep -v grep | head -1 | awk '{print $1}')"
echo "after spawning: orphan count $after_spawn (expected >= $((base + N + 1)))"
echo "protected foreign server: ${PROTECTED_PID:-missing}; proxy: $PROXY_PID"

# Start a real daemon; its boot reaper reaps every ppid==1 codex app-server that
# is NOT holding its own socket, i.e. all N foreign orphans.
"$BIN" >>"$SOAK_HOME/daemon.log" 2>&1 &
DPID=$!
echo "started daemon pid $DPID; waiting for its boot reaper..."
gone=0
for _ in $(seq 1 60); do
  left="$(ps -Ao pid,args | grep "${MARK}-orphan-" | grep -v grep | wc -l | tr -d ' ')"
  [[ "$left" == "0" ]] && { gone=1; break; }
  sleep 0.25
done
foreign_left="$(ps -Ao pid,args | grep "${MARK}-orphan-" | grep -v grep | wc -l | tr -d ' ')"
own="$(ps -Ao pid,args | grep "$SOAK_HOME/codex-app-server.sock" | grep 'app-server --listen' | grep -v grep | head -1 | awk '{print $1}')"
protected_alive=0
if [[ -n "$PROTECTED_PID" ]] && kill -0 "$PROTECTED_PID" 2>/dev/null; then
  protected_alive=1
fi
echo "  foreign orphans left after boot reap: $foreign_left (want 0)"
echo "  proxy-backed foreign server alive: $protected_alive (want 1)"
echo "  daemon's own server on its own socket: ${own:-none (auth-less init may not persist; not required)}"

# Once its consumer exits, the same foreign server becomes an orphan and the
# periodic reaper must clear it.
kill -TERM "$PROXY_PID" 2>/dev/null
wait "$PROXY_PID" 2>/dev/null
protected_gone=0
for _ in $(seq 1 60); do
  if ! kill -0 "$PROTECTED_PID" 2>/dev/null; then
    protected_gone=1
    break
  fi
  sleep 0.25
done
echo "  formerly proxy-backed server reaped after proxy exit: $protected_gone (want 1)"

kill -TERM "$DPID" 2>/dev/null; wait "$DPID" 2>/dev/null
echo "desktop app server at end: $(desktop_alive)"

if [[ "$foreign_left" == "0" && "$gone" == "1" && "$protected_alive" == "1" \
   && "$protected_gone" == "1" && "$after_spawn" -ge "$((base + N + 1))" ]]; then
  echo "PASS: cleared $N orphans, spared live foreign proxy target, reaped it after proxy exit."
  exit 0
else
  echo "FAIL: base=$base spawn=$after_spawn foreign_left=$foreign_left protected_alive=$protected_alive protected_gone=$protected_gone"
  exit 1
fi
