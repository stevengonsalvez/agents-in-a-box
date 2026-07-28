#!/usr/bin/env bash
# Direct scale proof for success criterion 1: the SessionStart reaper clears a
# PILE of orphaned codex app-servers back to 0 (the incident's real failure mode
# was 900+), while SPARING a server that is still in use (a live `app-server
# proxy` consumer on its socket) and never touching the desktop Codex app.
#
# It uses fabricated processes with codex-shaped argv: `exec -a` sets argv[0] to
# `.../bin/codex app-server ...`, and a subshell that backgrounds then lets its
# launcher exit reparents the process to launchd (ppid==1), exactly like a real
# server orphaned when its launcher dies. This is the right tool for testing the reaper's
# identify + SIGTERM logic without a working codex login. It kills ONLY its own
# marked pids, by exact pid, never pkill/wildcard.
#
# Usage: scripts/reap-stress-codex-orphans.sh [orphan_count]   (default 25)
set -uo pipefail

N="${1:-25}"
REPO="$(cd "$(dirname "$0")/.." && pwd)"
REAPER="$REPO/scripts/reap-codex-orphans.mjs"
MARK="/tmp/ainb-reapstress-$$"

# The exact success metric: ppid==1 codex app-servers (incl. any our fakes add).
orphan_count() {
  ps -Ao pid,ppid,args | grep '[b]in/codex' | grep -v grep | awk '$2==1' \
    | grep -c 'app-server'
}
mine_pids() { ps -Ao pid,args | grep "$MARK" | grep -v grep | awk '{print $1}'; }
kill_mine() { for pid in $(mine_pids); do kill -TERM "$pid" 2>/dev/null; done; }
trap kill_mine EXIT

# Orphaned (ppid==1) fake: launcher exits, backgrounded exec reparents to init.
# Redirect the child's fds so it never holds a caller's stdout pipe open.
spawn_orphan() { bash -c "(exec -a \"$1\" sleep 600 >/dev/null 2>&1) &"; }

desktop_alive() {
  ps -Ao pid,ppid,args | grep -i 'ChatGPT.app/Contents/Resources/codex' \
    | grep -v grep | grep -qi 'app-server' && echo yes || echo 'n/a'
}

base="$(orphan_count)"
echo "== reap-stress: $N orphans + 1 in-use (proxied) server =="
echo "baseline ppid==1 codex app-server count: $base"
echo "desktop app server at start: $(desktop_alive)"

# N genuinely orphaned app-servers (ppid==1, unique socket, NO proxy).
for n in $(seq 1 "$N"); do
  spawn_orphan "/opt/fake/bin/codex app-server --listen unix://${MARK}-orphan-${n}.sock"
done
# One "in-use" server: a ppid==1 orphan that still has a live proxy consumer on
# its socket (a transient state), so the reaper's defensive proxy-check spares it.
INUSE_SOCK="${MARK}-inuse.sock"
spawn_orphan "/opt/fake/bin/codex app-server --listen unix://${INUSE_SOCK}"
# The proxy stays a live child of THIS script (ppid != 1), so it is a genuine
# live consumer, not an orphan, and the ppid==1 metric never counts it.
bash -c "exec -a \"/opt/fake/bin/codex app-server proxy --sock ${INUSE_SOCK}\" sleep 600" >/dev/null 2>&1 &
PROXY_PID=$!
sleep 1

after_spawn="$(orphan_count)"
inuse_pid="$(ps -Ao pid,args | grep "$INUSE_SOCK" | grep 'app-server --listen' | grep -v grep | awk '{print $1}')"
echo "after spawning: orphan count $after_spawn (expected >= $((base + N + 1)))"
echo "in-use server pid $inuse_pid, live proxy pid $PROXY_PID on $INUSE_SOCK"

echo "-- reaper pass 1 (proxy still live): node scripts/reap-codex-orphans.mjs"
node "$REAPER"
sleep 1
after_reap1="$(orphan_count)"
orphans_left="$(ps -Ao pid,args | grep "${MARK}-orphan-" | grep -v grep | wc -l | tr -d ' ')"
inuse_alive1="$(ps -p "$inuse_pid" -o pid= 2>/dev/null | tr -d ' ')"
echo "   orphan count $after_reap1; leftover fake orphans $orphans_left; in-use server alive: ${inuse_alive1:-NO}"

echo "-- ending the session: kill the proxy (pid $PROXY_PID), then reaper pass 2"
kill -TERM "$PROXY_PID" 2>/dev/null
sleep 1
node "$REAPER"
sleep 1
after_reap2="$(orphan_count)"
inuse_alive2="$(ps -p "$inuse_pid" -o pid= 2>/dev/null | tr -d ' ')"
echo "   orphan count $after_reap2; in-use server alive: ${inuse_alive2:-NO}"
echo "desktop app server at end: $(desktop_alive)"

ok=1
[[ "$after_spawn" -ge "$((base + N + 1))" ]] || { echo "  FAIL: pile did not build"; ok=0; }
[[ "$orphans_left" == "0" ]]              || { echo "  FAIL: orphans survived pass 1"; ok=0; }
[[ -n "$inuse_alive1" ]]                  || { echo "  FAIL: in-use server was wrongly reaped in pass 1"; ok=0; }
[[ "$after_reap1" == "$((base + 1))" ]]   || { echo "  FAIL: pass 1 count != base+1 (in-use spared)"; ok=0; }
[[ -z "$inuse_alive2" ]]                  || { echo "  FAIL: in-use server survived after its proxy ended"; ok=0; }
[[ "$after_reap2" == "$base" ]]           || { echo "  FAIL: pass 2 count != baseline"; ok=0; }

if [[ "$ok" == "1" ]]; then
  echo "PASS: reaped $N-orphan pile to baseline; spared the in-use server while proxied; reaped it once the session ended; desktop app untouched."
  exit 0
else
  echo "FAIL (see above): base=$base spawn=$after_spawn reap1=$after_reap1 reap2=$after_reap2"
  exit 1
fi
