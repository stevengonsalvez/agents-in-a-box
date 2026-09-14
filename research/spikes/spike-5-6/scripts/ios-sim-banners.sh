#!/usr/bin/env bash
# Spike 6 local banners on the iOS simulator: tap the app's 30, 5 and 1 min
# needs-input buttons, answer the permission alert and lock the device (LOCK=1)
# or press Home (LOCK=0) in one actuator run, then have the actuator wait on
# SpringBoard for each notification to appear. The simulator asks before it
# opens a deep link, so taps replace the Android script's deep link.
set -euo pipefail
source "$(dirname "$0")/ios-sim-lib.sh"
LOCK=${LOCK:-1}

echo "# simulator: $(xcrun simctl getenv "$SIM" SIMULATOR_MODEL_IDENTIFIER) iOS $(xcrun simctl getenv "$SIM" SIMULATOR_RUNTIME_VERSION) lock=$LOCK (the simulator has no Low Power Mode)"
ensure_booted
act testUnlockAndForeground || true
xcrun simctl terminate "$SIM" "$BUNDLE" 2>/dev/null || true
t_launch=$(now_ms)
xcrun simctl launch "$SIM" "$BUNDLE" >/dev/null
read -r conn _ < <(wait_hello_after "$t_launch" 60)
TEST_RUNNER_SPIKE_LOCK="$LOCK" act testScheduleBannersAndLeave
if tail -6 "$EVENTS" | grep -q notifications_allowed; then echo "# permission: allowed through the alert"; else echo "# permission: no alert, already decided"; fi
echo "# transition at $(tail -1 "$EVENTS")"

# The clock starts when the app scheduled: its `banner_scheduled` mark on this connection.
declare -A scheduled=()
for s in 60 300 1800; do
  scheduled[$s]=$(python3 -c '
import json, sys
log, secs, conn = sys.argv[1], sys.argv[2], int(sys.argv[3])
hits = [json.loads(l) for l in open(log)]
print([e["t_ms"] for e in hits if e["conn"] == conn and e["event"] == "mark" and e["extra"]["label"].startswith(f"banner_scheduled seconds={secs} ")][-1])
' "$PEER_LOG" "$s" "$conn")
done

for s in 60 300 1800; do
  remaining=$(( s + 600 - ($(now_ms) - scheduled[$s]) / 1000 ))
  TEST_RUNNER_SPIKE_BANNER="scheduled ${s}s ahead" TEST_RUNNER_SPIKE_WAIT_S="$remaining" act testWaitBanner
  line=$(tail -1 "$EVENTS")
  t=${line%% *}
  case "$line" in
    *banner_seen*) echo "banner ${s}s: seen $(( (t - scheduled[$s]) / 1000 ))s after scheduling (lateness $(( (t - scheduled[$s]) / 1000 - s ))s)";;
    *) echo "banner ${s}s: NOT SEEN within $(( s + 600 ))s";;
  esac
done
act testUnlockAndForeground
