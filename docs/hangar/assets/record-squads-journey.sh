#!/bin/bash
# Recording driver for the P7 / D17 Squads vhs journey (tmux-verify G2/G3).
#
# Stands up an isolated short $HOME + the real hangar daemon seeded by
# `seed_squads_journey` (a `shippers` squad led by an AGENT, with two AGENT
# members + one HUMAN member, each agent on its own online runtime), generates
# the .tape with baked paths, launches the real `ainb tui`, opens the Squads
# screen (tab `S`), then presses `x` to fan the current issue across the squad —
# leader brief + parallel member dispatch through the REAL daemon path — and
# records the GIF + stills. The squad rows + the "briefed <leader> + N members"
# note render live from the daemon (DB → squads_list/agents_list RPC →
# render_squads), never a mock.
set -euo pipefail

W=/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_feat_hangar-parity/ainb-tui
ASSETS="$W/../docs/hangar/assets"
TARGET_DIR="${CARGO_TARGET_DIR:-$W/target}"
AINB="$TARGET_DIR/debug/ainb"
DAEMON="$TARGET_DIR/debug/ainb-hangar-daemon"
SEEDER="$TARGET_DIR/debug/examples/seed_squads_journey"
PLUGIN_ROOT="$W/dist/plugins"

for b in "$AINB" "$DAEMON" "$SEEDER" "$PLUGIN_ROOT/hangar-tui/hangar-tui"; do
  [ -e "$b" ] || { echo "MISSING: $b (build first: cargo build -p ainb --bin ainb; cargo build -p ainb-hangar-daemon --example seed_squads_journey --features test-support; ./scripts/build-plugins.sh)"; exit 1; }
done
command -v vhs >/dev/null || { echo "vhs not installed"; exit 1; }

# --- fresh short HOME (unix socket path <104 chars) ---
HOME_DIR=$(mktemp -d /tmp/sq.XXXXXX)

# Seed the squad + spawn the daemon detached (left alive for the tape's `ainb tui`).
"$SEEDER" "$HOME_DIR" "$DAEMON"

echo "$HOME_DIR" > /tmp/sq-home-raw.txt

# --- generate the tape (baked paths; vhs 0.11: Output must be relative, no Set Env) ---
TAPE="$ASSETS/squads-journey.tape"
cat > "$TAPE" <<EOF
# P7 / D17 Squads journey — tmux-verify frame-truth recording.
# Launches the real ainb TUI against a running hangar daemon seeded with a
# workspace-scoped squad "shippers" led by the agent lead-bot, with agent members
# worker-bot + deploy-bot and one human member. Opens the Squads screen (tab S),
# reads the rendered team, then presses \`x\` to fan the current issue across the
# squad — the leader brief + parallel member dispatch — surfacing the green
# "briefed agent-2 + 2 members" note. Paths are baked in (vhs 0.11 has no
# \`Set Env\`); the daemon is stood up by record-squads-journey.sh.
Output "squads-journey.gif"

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

# Open the Squads screen (tab S). The seeded squad renders live from the daemon:
# "shippers", leader: <dot> lead-bot (3 members), then the indented member rows.
Type "S"
Sleep 5s
Screenshot "squads-1-open.png"

# PAUSE on the rendered team (squad name, leader tag, member rows w/ agent/human).
Sleep 3s
Screenshot "squads-2-squad.png"

# Fan the current issue across the squad (x → hangar/squad_fanout). The daemon
# briefs the leader agent + enqueues one task per AGENT member (the human is
# skipped), and the screen raises the green "briefed agent-2 + 2 members" note.
Type "x"
Sleep 6s
Screenshot "squads-3-briefed.png"
Sleep 2s
EOF

echo "tape written: $TAPE"

# --- record ---
cd "$ASSETS"
vhs squads-journey.tape
echo "vhs done; gif + screenshots in $ASSETS"
ls -la "$ASSETS"/squads-journey.gif "$ASSETS"/squads-*.png

# --- teardown (kill only the daemon we spawned, by pid bound to this HOME) ---
for p in $(pgrep -f 'ainb-hangar-daemon' || true); do
  if lsof -p "$p" 2>/dev/null | grep -q "$HOME_DIR"; then kill "$p" 2>/dev/null || true; echo "killed daemon $p"; fi
done
rm -rf "$HOME_DIR"
echo "teardown complete"
