#!/bin/bash
# Recording driver for S3 (autopilot fire-now) — one of the two remaining side
# journeys in the converged-control-center catalogue
# (docs/hangar/verify-converged-goal.md).
#
# Seeds ONE cron-scheduled autopilot ("nightly-report", `0 9 * * *`, next tick
# hours away on the REAL wall clock — see `seed_autopilot_fire.rs` for why the
# seed clock matters) against a minimal tenancy (no P4 fixture baggage, so the
# fired run is the only activity on screen), with the daemon's claim loop ARMED
# against a fake-`claude` provider that completes in well under a second. The
# tape opens the Autopilots manager (`4`), fires the seeded row (`r`), and
# switches to Usage (`U`) to show the completed run landed in history.
set -euo pipefail

CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/Users/stevengonsalvez/.cache/ccc-shared-target}"
W=/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_feat_hangar-parity/ainb-tui
ASSETS="$(dirname "$0")"
AINB="$CARGO_TARGET_DIR/debug/ainb"
DAEMON="$CARGO_TARGET_DIR/debug/ainb-hangar-daemon"
SEEDER="$CARGO_TARGET_DIR/debug/examples/seed_autopilot_fire"
PLUGIN_ROOT="$(dirname "$CARGO_TARGET_DIR")/dist/plugins"

for b in "$AINB" "$DAEMON" "$SEEDER"; do
  [ -x "$b" ] || {
    echo "missing binary: $b — build first: (cd $W && cargo build -p ainb -p ainb-hangar-daemon --example seed_autopilot_fire)" >&2
    exit 2
  }
done

HOME_DIR=$(mktemp -d /tmp/s3af.XXXXXX)
"$SEEDER" "$HOME_DIR" "$DAEMON"

TAPE="$ASSETS/s3-autopilot-firenow.tape"
cat > "$TAPE" <<EOF
# S3 — autopilot fire-now (verify-converged-goal.md journey catalogue).
Output "s3-autopilot-firenow.gif"

Set Shell "bash"
Set FontSize 13
Set Width 2200
Set Height 1000
Set Padding 12

Hide
Type "HOME='$HOME_DIR' AINB_PLUGIN_ROOT='$PLUGIN_ROOT' exec '$AINB' tui"
Enter
Sleep 12s
Show

Type "g"
Sleep 4s

# --- Autopilots manager (4): the seeded row before any fire ---
Type "4"
Sleep 3s
Screenshot "s3-1-autopilots-open.png"

# --- fire-now (r): dispatch → claim → fake-provider completion, all within ~1s ---
Type "r"
Sleep 3s
Screenshot "s3-2-fired-completed.png"

# --- Usage (U): the completed run landed in recent-runs history ---
Type "U"
Sleep 3s
Screenshot "s3-3-usage-recent-runs.png"
Sleep 1s
EOF

cd "$ASSETS"
vhs s3-autopilot-firenow.tape
echo "vhs done"

# --- teardown: kill only the daemon PID we spawned (verified by lsof match on our HOME_DIR) ---
for p in $(pgrep -f 'ainb-hangar-daemon' || true); do
  if lsof -p "$p" 2>/dev/null | grep -q "$HOME_DIR"; then kill -9 "$p" 2>/dev/null || true; echo "killed daemon $p"; fi
done
rm -rf "$HOME_DIR"
echo "teardown complete"
