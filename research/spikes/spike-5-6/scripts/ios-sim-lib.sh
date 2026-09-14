#!/usr/bin/env bash
# Shared helpers for the iOS simulator measurement scripts. The simulator runs
# on the host kernel, so the app, the XCUITest actuator and peerd all stamp the
# same wall clock (epoch milliseconds).
#
# Required env: SIM (simulator UDID), XCTESTRUN (from `xcodebuild
# build-for-testing -scheme AinbSpikeUITests`), PEER_LOG (peerd log),
# EVENTS (actuator stamp file).
source "$(dirname "$0")/android-lib.sh"   # now_ms, wait_hello_after, pings_for
BUNDLE=dev.ainb.spike.wire

# One actuator action: testLock, testHome, testUnlockAndForeground,
# testAllowNotifications, testWaitBanner. Extra env goes through TEST_RUNNER_*.
act() {
  local test=$1
  env TEST_RUNNER_SPIKE_EVENTS="$EVENTS" xcodebuild test-without-building -xctestrun "$XCTESTRUN" \
    -destination "id=$SIM" -only-testing:"AinbSpikeUITests/Actuator/$test" >>"${ACT_LOG:-/dev/null}" 2>&1
}

# CoreSimulatorService relaunches under host memory pressure and shuts every
# booted device down; boot again (no-op when already booted) before each run.
ensure_booted() { xcrun simctl bootstatus "$SIM" -b >/dev/null; }

# Epoch ms of CoreSimulatorService shutting this device down after $1, or empty.
service_shutdown_after() {
  python3 - "$SIM" "$1" <<'EOF'
import os, re, sys, datetime
since = int(sys.argv[2]) / 1000
for line in open(os.path.expanduser("~/Library/Logs/CoreSimulator/CoreSimulator.log"), errors="replace"):
    if sys.argv[1] in line and "Shutting down" in line:
        stamp = datetime.datetime.strptime(f"{datetime.date.today().year} {line[:15]}", "%Y %b %d %H:%M:%S").timestamp()
        if stamp >= since: print(int(stamp * 1000)); break
EOF
}

# Newest actuator stamp for an event name, epoch ms.
last_event() { grep " $1" "$EVENTS" | tail -1 | cut -d' ' -f1; }

# Field from the app's `cold_start` mark for a connection, e.g. `mark_field 12 key_ready`.
mark_field() {
  python3 - "$PEER_LOG" "$1" "$2" <<'EOF'
import json, sys
for line in open(sys.argv[1]):
    e = json.loads(line)
    if e["conn"] == int(sys.argv[2]) and e["event"] == "mark":
        for kv in e["extra"]["label"].split():
            k, _, v = kv.partition("=")
            if k == sys.argv[3]: print(v)
EOF
}
