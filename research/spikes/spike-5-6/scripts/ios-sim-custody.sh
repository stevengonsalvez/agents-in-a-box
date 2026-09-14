#!/usr/bin/env bash
# Spike 5 key custody on the iOS simulator: does the device static key survive
# an app update and an uninstall plus reinstall, and what does a
# `requireAuthentication` item do without and with an enrolled Face ID.
# Required env on top of ios-sim-lib.sh: APP (the built ainbwirespike.app).
set -euo pipefail
source "$(dirname "$0")/ios-sim-lib.sh"
: "${APP:?path to ainbwirespike.app}"

launch_and_report() {
  local label=$1 t0 conn
  xcrun simctl terminate "$SIM" "$BUNDLE" 2>/dev/null || true
  t0=$(now_ms)
  xcrun simctl launch "$SIM" "$BUNDLE" >/dev/null
  read -r conn _ < <(wait_hello_after "$t0" 60)
  sleep 2
  echo "$label: key_created=$(mark_field "$conn" key_created) key_fp=$(mark_field "$conn" key_fp)"
}

probe() {
  local label=$1 answer=${2:-} since
  since=$(now_ms)
  act testTapBiometricProbe &
  local actor=$!
  if [ -n "$answer" ]; then
    # Wait for the tap, give the Face ID sheet a moment, then answer it.
    until [ "$(last_event biometric_probe_tapped)" -ge "$since" ] 2>/dev/null; do sleep 0.5; done
    sleep 3
    xcrun simctl spawn "$SIM" notifyutil -p "com.apple.BiometricKit_Sim.pearl.$answer"
  fi
  wait "$actor"
  python3 - "$PEER_LOG" "$since" "$label" <<'EOF'
import json, sys
hits = [json.loads(l) for l in open(sys.argv[1])]
hits = [e["extra"]["label"] for e in hits if e["event"] == "mark" and e["t_ms"] >= int(sys.argv[2]) and e["extra"]["label"].startswith("biometric_probe")]
print(f"{sys.argv[3]}: {hits[-1] if hits else 'no probe result'}")
EOF
}

echo "# simulator: $(xcrun simctl getenv "$SIM" SIMULATOR_MODEL_IDENTIFIER) iOS $(xcrun simctl getenv "$SIM" SIMULATOR_RUNTIME_VERSION)"
act testUnlockAndForeground
launch_and_report "baseline"
xcrun simctl install "$SIM" "$APP"
launch_and_report "after update (install over)"
xcrun simctl uninstall "$SIM" "$BUNDLE"
xcrun simctl install "$SIM" "$APP"
launch_and_report "after uninstall and reinstall"

xcrun simctl spawn "$SIM" notifyutil -s com.apple.BiometricKit.enrollmentChanged 0
xcrun simctl spawn "$SIM" notifyutil -p com.apple.BiometricKit.enrollmentChanged
probe "gated item, Face ID not enrolled"
xcrun simctl spawn "$SIM" notifyutil -s com.apple.BiometricKit.enrollmentChanged 1
xcrun simctl spawn "$SIM" notifyutil -p com.apple.BiometricKit.enrollmentChanged
# Match first: a non-match leaves the sheet up and the read pending.
probe "gated item, Face ID enrolled, matching face" match
probe "gated item, Face ID enrolled, non-matching face" nomatch
xcrun simctl spawn "$SIM" notifyutil -s com.apple.BiometricKit.enrollmentChanged 0
xcrun simctl spawn "$SIM" notifyutil -p com.apple.BiometricKit.enrollmentChanged
