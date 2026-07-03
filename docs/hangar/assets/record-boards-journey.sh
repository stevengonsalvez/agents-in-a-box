#!/bin/bash
# Recording driver for the P4 / D8 Boards vhs journey (tmux-verify G2/G3).
#
# Stands up an isolated short $HOME + the real hangar daemon seeded by
# `seed_boards_journey` (a user-defined 5-column board whose card ran to `done`
# and AUTO-MOVED into the done-mapped Done column — green ✓), generates the
# .tape with baked paths, launches the real `ainb tui`, opens the Boards screen
# (tab B), and records the GIF + stills. The card is green through the REAL
# render path (daemon DB → boards_list RPC → render_boards), never a mock.
set -euo pipefail

W=/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_feat_hangar-parity/ainb-tui
ASSETS="$W/../docs/hangar/assets"
TARGET_DIR="${CARGO_TARGET_DIR:-$W/target}"
AINB="$TARGET_DIR/debug/ainb"
DAEMON="$TARGET_DIR/debug/ainb-hangar-daemon"
SEEDER="$TARGET_DIR/debug/examples/seed_boards_journey"
PLUGIN_ROOT="$W/dist/plugins"

for b in "$AINB" "$DAEMON" "$SEEDER" "$PLUGIN_ROOT/hangar-tui/hangar-tui"; do
  [ -e "$b" ] || { echo "MISSING: $b (build first: cargo build -p ainb --bin ainb; cargo build -p ainb-hangar-daemon --example seed_boards_journey; refresh dist/plugins/hangar-tui)"; exit 1; }
done
command -v vhs >/dev/null || { echo "vhs not installed"; exit 1; }

# --- fresh short HOME (unix socket path <104 chars) ---
HOME_DIR=$(mktemp -d /tmp/bj.XXXXXX)

# Seed the board + run the card to `done` + wait for the auto-move to land, then
# spawn the daemon detached (left alive for the tape's `ainb tui`).
"$SEEDER" "$HOME_DIR" "$DAEMON"

echo "$HOME_DIR" > /tmp/bj-home-raw.txt

# --- generate the tape (baked paths; vhs 0.11: Output must be relative, no Set Env) ---
TAPE="$ASSETS/boards-journey.tape"
cat > "$TAPE" <<EOF
# P4 / D8 Boards journey — tmux-verify frame-truth recording.
# Launches the real ainb TUI against a running hangar daemon seeded with a
# user-defined five-column "Delivery" board whose card (HG-42 "Ship the tables")
# already RAN to done and AUTO-MOVED into the done-mapped Done column (green ✓).
# Opens the Boards screen (tab B) and reads the rendered outcome. Paths are baked
# in (vhs 0.11 has no \`Set Env\`); the daemon is stood up by record-boards-journey.sh.
Output "boards-journey.gif"

Set Shell "bash"
Set FontSize 13
Set Width 1900
Set Height 950
Set Padding 12

# Launch the TUI (hidden while it cold-loads).
Hide
Type "HOME='$HOME_DIR' AINB_PLUGIN_ROOT='$PLUGIN_ROOT' exec '$AINB' tui"
Enter
Sleep 12s
Show

# Home screen is up. Open the Hangar plugin screen.
Type "g"
Sleep 4s
Screenshot "boards-1-hangar.png"

# Open the Boards screen (tab B). The seeded board renders live from the daemon.
Type "B"
Sleep 5s
Screenshot "boards-2-open.png"

# PAUSE on the rendered board: 5 columns (Backlog + Queued/Running/Failed/Done),
# the green ✓ HG-42 card sitting in the done-mapped Done column, auto-move ON.
Sleep 3s
Screenshot "boards-3-done.png"
Sleep 2s
EOF

echo "tape written: $TAPE"

# --- record ---
cd "$ASSETS"
vhs boards-journey.tape
echo "vhs done; gif + screenshots in $ASSETS"
ls -la "$ASSETS"/boards-journey.gif "$ASSETS"/boards-*.png

# --- teardown (kill only the daemon we spawned, by pid bound to this HOME) ---
for p in $(pgrep -f 'ainb-hangar-daemon' || true); do
  if lsof -p "$p" 2>/dev/null | grep -q "$HOME_DIR"; then kill "$p" 2>/dev/null || true; echo "killed daemon $p"; fi
done
rm -rf "$HOME_DIR"
echo "teardown complete"
