#!/usr/bin/env bash
# Spike 5 cold start: force-stop, launch, and time host `am start` issue to the
# peer receiving the first AEAD-framed request (`auth/hello`) from the new
# process. Five runs by default.
set -euo pipefail
source "$(dirname "$0")/android-lib.sh"
RUNS=${RUNS:-5}

echo "# device: $(adbs shell getprop ro.product.model | tr -d '\r') android $(adbs shell getprop ro.build.version.release | tr -d '\r') (sdk $(adbs shell getprop ro.build.version.sdk | tr -d '\r')) emulator=$(adbs shell getprop ro.kernel.qemu | tr -d '\r')"
echo "# battery saver: $(adbs shell settings get global low_power | tr -d '\r')"
overhead=()
for _ in 1 2 3 4 5; do a=$(now_ms); adbs shell true; overhead+=($(( $(now_ms) - a ))); done
echo "# adb shell round trip ms: ${overhead[*]}"
wake_and_unlock
for run in $(seq 1 "$RUNS"); do
  adbs shell am force-stop "$PKG"
  sleep 3
  t0=$(now_ms)
  am=$(adbs shell am start -W -n "$ACT" | tr -d '\r')
  read -r conn t_hello < <(wait_hello_after "$t0" 60)
  total=$(grep -E '^TotalTime' <<<"$am" | awk '{print $2}')
  echo "run $run: am_start_TotalTime_ms=$total launch_to_hello_ms=$(( t_hello - t0 )) conn=$conn"
  sleep 4
done
adbs shell am force-stop "$PKG"
