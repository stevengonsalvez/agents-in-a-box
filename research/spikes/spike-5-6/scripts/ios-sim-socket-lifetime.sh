#!/usr/bin/env bash
# Spike 6 socket lifetime on the iOS simulator. MODE=background presses Home;
# MODE=lock presses the lock button; both through the XCUITest actuator, whose
# stamp is the transition time. Same heartbeat analysis as the Android script:
# the first missed heartbeat is the first 15 s slot after the transition with
# no ping at the peer within 5 s of it.
set -euo pipefail
source "$(dirname "$0")/ios-sim-lib.sh"
MODE=${MODE:?background|lock}
RUNS=${RUNS:-5}
WINDOW_S=${WINDOW_S:-600}

echo "# mode=$MODE window=${WINDOW_S}s simulator: $(xcrun simctl getenv "$SIM" SIMULATOR_MODEL_IDENTIFIER) iOS $(xcrun simctl getenv "$SIM" SIMULATOR_RUNTIME_VERSION)"
# Runs a simulator-service restart interrupts are reported as aborted and
# repeated, up to three attempts per wanted run.
run=0; attempt=0
while [ "$run" -lt "$RUNS" ] && [ "$attempt" -lt $(( RUNS * 3 )) ]; do
  attempt=$(( attempt + 1 ))
  ensure_booted
  act testUnlockAndForeground || true
  xcrun simctl terminate "$SIM" "$BUNDLE" 2>/dev/null || true
  sleep 2
  t_launch=$(now_ms)
  xcrun simctl launch "$SIM" "$BUNDLE" >/dev/null || { echo "attempt $attempt $MODE: aborted, launch failed"; continue; }
  read -r conn _ < <(wait_hello_after "$t_launch" 60) || { echo "attempt $attempt $MODE: aborted, no auth/hello"; continue; }
  sleep 20   # at least one foreground heartbeat
  if [ "$MODE" = background ]; then act testHome; t_bg=$(last_event home_pressed); else act testLock; t_bg=$(last_event lock_pressed); fi
  [ "$t_bg" -ge "$t_launch" ] || { echo "attempt $attempt $MODE: aborted, actuator did not stamp"; continue; }
  sleep $(( WINDOW_S - ( $(now_ms) - t_bg ) / 1000 ))
  down=$(service_shutdown_after "$t_launch")
  [ -z "$down" ] || { echo "attempt $attempt $MODE: aborted, simulator service shut the device down at +$(( (down - t_bg) / 1000 ))s"; continue; }
  run=$(( run + 1 ))
  pings=$(pings_for "$conn" | tr '\n' ' ')
  disconnect=$(python3 - "$PEER_LOG" "$conn" <<'EOF'
import json, sys
for line in open(sys.argv[1]):
    e = json.loads(line)
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
    slot = min(p for p in after if abs(p - slot) <= 5000) + 15000
resumed = [p for p in after if missed is not None and p > missed + 5000]
disc = sys.argv[4]
print(f"run {sys.argv[5]} {sys.argv[6]}: first_missed_after_s={'>%d' % (window // 1000) if missed is None else round((missed - t_bg) / 1000, 1)}"
      f" last_ping_before_s={round((before[-1] - t_bg) / 1000, 1) if before else 'none'}"
      f" pings_after_transition={len(after)} late_pings_after_miss={len(resumed)}"
      f" disconnect={'none' if not disc else str(round((int(disc.split()[0]) - t_bg) / 1000, 1)) + 's ' + ' '.join(disc.split()[1:])}")
EOF
done
act testUnlockAndForeground
