#!/usr/bin/env bash
# Spike 6 socket lifetime. MODE=background presses HOME with the screen kept on;
# MODE=lock turns the screen off with the app in the foreground. The app's
# in-crate heartbeat fires every 15 s; the first missed heartbeat is the first
# 15 s slot, after the transition, with no ping at the peer within 5 s of it.
set -euo pipefail
source "$(dirname "$0")/android-lib.sh"
MODE=${MODE:?background|lock}
RUNS=${RUNS:-5}
WINDOW_S=${WINDOW_S:-600}

echo "# mode=$MODE window=${WINDOW_S}s device: $(adbs shell getprop ro.product.model | tr -d '\r') android $(adbs shell getprop ro.build.version.release | tr -d '\r') battery_saver=$(adbs shell settings get global low_power | tr -d '\r') stay_on=$(adbs shell settings get global stay_on_while_plugged_in | tr -d '\r')"
for run in $(seq 1 "$RUNS"); do
  wake_and_unlock
  adbs shell am force-stop "$PKG"
  sleep 2
  t_launch=$(now_ms)
  adbs shell am start -n "$ACT" >/dev/null
  read -r conn _ < <(wait_hello_after "$t_launch" 60)
  sleep 20   # at least one foreground heartbeat
  t_bg=$(now_ms)
  if [ "$MODE" = background ]; then adbs shell input keyevent KEYCODE_HOME; else adbs shell input keyevent KEYCODE_SLEEP; fi
  sleep "$WINDOW_S"
  pings=$(pings_for "$conn" | tr '\n' ' ')
  disconnect=$(python3 - "$PEER_LOG" "$conn" <<'EOF'
import json, sys
for line in open(sys.argv[1]):
    try: e = json.loads(line)
    except ValueError: continue
    if e["conn"] == int(sys.argv[2]) and e["event"] == "disconnect": print(e["t_ms"], e["extra"]["reason"]); break
EOF
)
  python3 - "$t_bg" "$WINDOW_S" "$pings" "$disconnect" "$run" "$MODE" <<'EOF'
import sys
t_bg, window = int(sys.argv[1]), int(sys.argv[2]) * 1000
pings = [int(p) for p in sys.argv[3].split()]
before = [p for p in pings if p < t_bg]
after = [p for p in pings if p >= t_bg]
slot = (before[-1] if before else t_bg) + 15000
missed = None
while slot < t_bg + window:
    if not any(abs(p - slot) <= 5000 for p in after):
        missed = slot; break
    slot = min((p for p in after if abs(p - slot) <= 5000)) + 15000
alive = [p for p in after if missed is None or p < missed]
resumed = [p for p in after if missed is not None and p > missed + 5000]
disc = sys.argv[4]
print(f"run {sys.argv[5]} {sys.argv[6]}: first_missed_after_s={'>%d' % (window // 1000) if missed is None else round((missed - t_bg) / 1000, 1)}"
      f" pings_after_transition={len(after)} alive_pings={len(alive)} late_pings_after_miss={len(resumed)}"
      f" disconnect={'none' if not disc else str(round((int(disc.split()[0]) - t_bg) / 1000, 1)) + 's ' + ' '.join(disc.split()[1:])}")
EOF
done
wake_and_unlock
adbs shell am force-stop "$PKG"
