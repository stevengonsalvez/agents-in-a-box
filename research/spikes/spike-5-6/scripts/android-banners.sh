#!/usr/bin/env bash
# Spike 6 local banners: schedule needs-input notifications 60, 300 and 1800 s
# ahead through the app's deep link, background the app and turn the screen
# off, then poll the notification service until each is posted.
set -euo pipefail
source "$(dirname "$0")/android-lib.sh"
LOCK=${LOCK:-1}

echo "# device: $(adbs shell getprop ro.product.model | tr -d '\r') android $(adbs shell getprop ro.build.version.release | tr -d '\r') battery_saver=$(adbs shell settings get global low_power | tr -d '\r') lock=$LOCK"
echo "# exact alarm permission: $(adbs shell appops get $PKG SCHEDULE_EXACT_ALARM | tr -d '\r')"
wake_and_unlock
adbs shell am force-stop "$PKG"
adbs shell am start -n "$ACT" >/dev/null
sleep 8
declare -A scheduled=()
for s in 60 300 1800; do
  scheduled[$s]=$(now_ms)
  adbs shell am start -a android.intent.action.VIEW -d "ainbspike://schedule/$s" "$PKG" >/dev/null
  sleep 2
done
adbs shell dumpsys alarm | grep -A2 "$PKG" | grep -E "Alarm\{|window=" | sed 's/^/# alarm: /'
adbs shell input keyevent KEYCODE_HOME
[ "$LOCK" = 1 ] && adbs shell input keyevent KEYCODE_SLEEP
declare -A fired=()
deadline=$(( $(date +%s) + 1800 + 300 ))
while [ "$(date +%s)" -lt "$deadline" ] && [ ${#fired[@]} -lt 3 ]; do
  dump=$(adbs shell dumpsys notification --noredact | tr -d '\r')
  for s in 60 300 1800; do
    if [ -z "${fired[$s]:-}" ] && grep -q "scheduled ${s}s ahead" <<<"$dump"; then
      fired[$s]=$(now_ms)
      echo "banner ${s}s: posted $(( (fired[$s] - scheduled[$s]) / 1000 ))s after scheduling (lateness $(( (fired[$s] - scheduled[$s]) / 1000 - s ))s, poll resolution 2s)"
    fi
  done
  sleep 2
done
for s in 60 300 1800; do [ -z "${fired[$s]:-}" ] && echo "banner ${s}s: NOT POSTED within window"; done
adbs exec-out screencap -p > "${SHOT:-/dev/null}" || true
wake_and_unlock
adbs shell am force-stop "$PKG"
