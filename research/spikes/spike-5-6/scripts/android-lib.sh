#!/usr/bin/env bash
# Shared helpers for the Android measurement scripts. All times are the HOST
# wall clock in epoch milliseconds, the same clock peerd stamps its log with.
#
# Required env: ADB (adb binary), SERIAL (device serial), PEER_LOG (peerd log).
PKG=dev.ainb.spike.wire
ACT=$PKG/.MainActivity

now_ms() { python3 -c 'import time; print(int(time.time()*1000))'; }
adbs() { "$ADB" -s "$SERIAL" "$@"; }

# Newest conn id that sent auth/hello at or after $1 (ms); waits up to $2 s.
wait_hello_after() {
  local since=$1 deadline=$(( $(date +%s) + ${2:-60} ))
  while [ "$(date +%s)" -lt "$deadline" ]; do
    local hit
    hit=$(python3 - "$PEER_LOG" "$since" <<'EOF'
import json, sys
since = int(sys.argv[2]); best = None
for line in open(sys.argv[1]):
    try: e = json.loads(line)
    except ValueError: continue
    if e["event"] == "hello" and e["t_ms"] >= since: best = (e["conn"], e["t_ms"])
print(f"{best[0]} {best[1]}" if best else "")
EOF
)
    if [ -n "$hit" ]; then echo "$hit"; return 0; fi
    sleep 0.05
  done
  return 1
}

# Pings for one connection as epoch ms, one per line.
pings_for() {
  python3 - "$PEER_LOG" "$1" <<'EOF'
import json, sys
conn = int(sys.argv[2])
for line in open(sys.argv[1]):
    try: e = json.loads(line)
    except ValueError: continue
    if e["conn"] == conn and e["event"] == "ping": print(e["t_ms"])
EOF
}

wake_and_unlock() {
  adbs shell input keyevent KEYCODE_WAKEUP
  sleep 1
  adbs shell wm dismiss-keyguard
  sleep 1
}
