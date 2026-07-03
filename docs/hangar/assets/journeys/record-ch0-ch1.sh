#!/bin/bash
# Recording driver for CH0 (Profiles) + CH1 (Boards + confirmed UI-wiring
# defect capture) of the MASTER converged-control-center journey.
set -euo pipefail

W=/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_feat_hangar-parity/ainb-tui
ASSETS="$W/../docs/hangar/assets/journeys"
AINB="$W/target/debug/ainb"
DAEMON="$W/target/debug/ainb-hangar-daemon"
SEEDER="$W/target/debug/examples/seed_master_journey"
PLUGIN_ROOT="$W/dist/plugins"

HOME_DIR=$(mktemp -d /tmp/mj0.XXXXXX)
"$SEEDER" "$HOME_DIR" "$DAEMON"

TAPE="$ASSETS/ch0-ch1-profile-boards.tape"
cat > "$TAPE" <<EOF
# CH0 (profile editor) + CH1 (boards + confirmed defect capture) — MASTER journey.
Output "ch0-ch1-profile-boards.gif"

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

# --- CH0: Profiles screen (P) ---
Type "P"
Sleep 3s
Screenshot "ch0-1-profiles-open.png"

Type "t"
Sleep 2s
Type "t"
Sleep 2s
Screenshot "ch0-2-tier-balanced.png"

# --- CH1: Boards screen (B) ---
Type "B"
Sleep 3s
Screenshot "ch1-1-boards-open.png"

# Defect capture: AddCard ('c') is reducer-only, never lifted to an RPC.
Type "c"
Sleep 2s
Screenshot "ch1-2-addcard-noop.png"

# Defect capture: RunFocusedCard (Enter) is likewise reducer-only.
Enter
Sleep 2s
Screenshot "ch1-3-run-noop.png"
Sleep 1s
EOF

cd "$ASSETS"
vhs ch0-ch1-profile-boards.tape
echo "vhs done"

# --- teardown: kill only the daemon PID we spawned (verified by lsof match on our HOME_DIR) ---
for p in $(pgrep -f 'ainb-hangar-daemon' || true); do
  if lsof -p "$p" 2>/dev/null | grep -q "$HOME_DIR"; then kill "$p" 2>/dev/null || true; echo "killed daemon $p"; fi
done
rm -rf "$HOME_DIR"
echo "teardown complete"
