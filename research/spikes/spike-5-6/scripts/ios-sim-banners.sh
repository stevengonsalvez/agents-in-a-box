#!/usr/bin/env bash
# Spike 6 local banners on the iOS simulator: schedule needs-input notifications
# 60, 300 and 1800 s ahead through the app's deep link, accept the permission
# prompt with the actuator, lock the device (LOCK=1) or press Home (LOCK=0), then
# have the actuator wait on SpringBoard for each notification to appear.
set -euo pipefail
source "$(dirname "$0")/ios-sim-lib.sh"
LOCK=${LOCK:-1}

echo "# simulator: $(xcrun simctl getenv "$SIM" SIMULATOR_MODEL_IDENTIFIER) iOS $(xcrun simctl getenv "$SIM" SIMULATOR_RUNTIME_VERSION) lock=$LOCK (the simulator has no Low Power Mode)"
act testUnlockAndForeground
xcrun simctl terminate "$SIM" "$BUNDLE" 2>/dev/null || true
xcrun simctl launch "$SIM" "$BUNDLE" >/dev/null
sleep 8
declare -A scheduled=()
for s in 60 300 1800; do
  scheduled[$s]=$(now_ms)
  xcrun simctl openurl "$SIM" "ainbspike://schedule/$s"
  # The first schedule waits on the notification permission alert.
  [ "$s" = 60 ] && { act testAllowNotifications; line=$(tail -1 "$EVENTS"); echo "# permission: ${line#* }"; }
  sleep 2
done
# The clock starts when the app actually scheduled: its `banner_scheduled` mark.
for s in 60 300 1800; do
  scheduled[$s]=$(python3 - "$PEER_LOG" "$s" <<'EOF'
import json, sys
hits = [json.loads(l) for l in open(sys.argv[1])]
hits = [e["t_ms"] for e in hits if e["event"] == "mark" and e["extra"]["label"].startswith(f"banner_scheduled seconds={sys.argv[2]} ")]
print(hits[-1])
EOF
)
done
if [ "$LOCK" = 1 ]; then act testLock; else act testHome; fi
echo "# transition at $(tail -1 "$EVENTS")"
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
