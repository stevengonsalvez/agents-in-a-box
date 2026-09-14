#!/usr/bin/env bash
# Spike 5 key custody on Android: does the device static key survive a relaunch,
# an update (`install -r`) and an uninstall plus reinstall. Reads the app's
# `cold_start` mark (key_created, key_fp) from the peer log.
# Required env on top of android-lib.sh: APK (the release APK to install).
set -euo pipefail
source "$(dirname "$0")/android-lib.sh"
: "${APK:?path to the release APK}"

field() {
  python3 -c '
import json, sys
for line in open(sys.argv[1]):
    e = json.loads(line)
    if e["conn"] == int(sys.argv[2]) and e["event"] == "mark":
        for kv in e["extra"]["label"].split():
            k, _, v = kv.partition("=")
            if k == sys.argv[3]: print(v)
' "$PEER_LOG" "$1" "$2"
}

step() {
  adbs shell am force-stop "$PKG"
  local t conn
  t=$(now_ms)
  adbs shell am start -n "$ACT" >/dev/null
  read -r conn _ < <(wait_hello_after "$t" 60)
  sleep 2
  echo "$1: key_created=$(field "$conn" key_created) key_fp=$(field "$conn" key_fp)"
}

echo "# device: $(adbs shell getprop ro.product.model | tr -d '\r') android $(adbs shell getprop ro.build.version.release | tr -d '\r')"
wake_and_unlock
step "launch"
step "relaunch"
adbs install -r "$APK" >/dev/null
step "after update (install -r)"
adbs uninstall "$PKG" >/dev/null
adbs install "$APK" >/dev/null
adbs shell pm grant "$PKG" android.permission.POST_NOTIFICATIONS
step "after uninstall and reinstall"
adbs shell am force-stop "$PKG"
