#!/usr/bin/env bash
# Spike 5 cold start on the iOS simulator: terminate, launch, and time host
# `simctl launch` issue to peerd receiving `auth/hello` from the new process.
# Also prints the in-app split from the app's `cold_start` mark. Five runs.
set -euo pipefail
source "$(dirname "$0")/ios-sim-lib.sh"
RUNS=${RUNS:-5}

echo "# simulator: $(xcrun simctl getenv "$SIM" SIMULATOR_MODEL_IDENTIFIER) iOS $(xcrun simctl getenv "$SIM" SIMULATOR_RUNTIME_VERSION)"
overhead=()
for _ in 1 2 3 4 5; do a=$(now_ms); xcrun simctl spawn "$SIM" /usr/bin/true 2>/dev/null || true; overhead+=($(( $(now_ms) - a ))); done
echo "# simctl spawn round trip ms: ${overhead[*]}"
for run in $(seq 1 "$RUNS"); do
  xcrun simctl terminate "$SIM" "$BUNDLE" 2>/dev/null || true
  sleep 3
  t0=$(now_ms)
  xcrun simctl launch "$SIM" "$BUNDLE" >/dev/null
  read -r conn t_hello < <(wait_hello_after "$t0" 60)
  sleep 2
  js=$(mark_field "$conn" js_start); key=$(mark_field "$conn" key_ready); con=$(mark_field "$conn" connected)
  echo "run $run: launch_to_hello_ms=$(( t_hello - t0 )) launch_to_js_ms=$(( js - t0 )) js_to_key_ms=$(( key - js )) key_to_connected_ms=$(( con - key )) ws_ms=$(mark_field "$conn" ws_ms | head -1) noise_ms=$(mark_field "$conn" noise_ms | head -1) connected_to_hello_ms=$(( t_hello - con )) key_created=$(mark_field "$conn" key_created) warm=$(mark_field "$conn" total_ms)/$(mark_field "$conn" noise_ms | tail -1) conn=$conn"
  sleep 2
done
