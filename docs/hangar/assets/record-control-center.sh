#!/bin/bash
# Recording driver for the control-center vhs journey (tmux-verify G2/G3).
# Seeds a fresh isolated HOME + a real deliverable target tmux session + the
# hangar daemon, generates the .tape with baked paths, and records the GIF.
set -euo pipefail

W=/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_feat_hangar-parity/ainb-tui
ASSETS="$W/../docs/hangar/assets"
AINB="$W/target/debug/ainb"
DAEMON="$W/target/debug/ainb-hangar-daemon"
SEEDER="$W/target/debug/examples/seed_control_center"
PLUGIN_ROOT="$W/dist/plugins"

# --- fresh short HOME (unix socket path <104 chars) ---
HOME_DIR=$(mktemp -d /tmp/cc.XXXXXX)
TARGET="cc-target-$$"
FAKE_AINB="$HOME_DIR/fake-ainb.sh"

cat > "$FAKE_AINB" <<EOF
#!/bin/sh
if [ "\$1" = "list" ]; then
  printf '%s' '[{"session_id":"s-deploy","tmux_session_name":"$TARGET","workspace_name":"deploy","worktree_path":"/work/deploy","created_at":"2026-07-01T00:00:00+00:00","is_running":true,"claude_active":true}]'
  exit 0
fi
printf '%s' '[]'
EOF
chmod +x "$FAKE_AINB"

# real delivery-target tmux session (a plain shell pane accepts the send-keys)
tmux new-session -d -s "$TARGET" -x 80 -y 24

# seed DB (P4 fixture + WAIT + 3-option ASK) and spawn the daemon detached
"$SEEDER" "$HOME_DIR" "$DAEMON" "$FAKE_AINB"

echo "$HOME_DIR" > /tmp/cc-home-raw.txt
echo "$TARGET" > /tmp/cc-target.txt

# --- generate the tape (baked paths; vhs 0.11: Output must be relative, no Set Env) ---
TAPE="$ASSETS/control-center-journey.tape"
cat > "$TAPE" <<EOF
# Control-center journey — tmux-verify frame-truth recording.
# Launches the real ainb TUI against a running hangar daemon seeded with one
# 3-option ask_user_question, opens the control center (C), and answers option 2.
# Paths are baked in (vhs 0.11 has no \`Set Env\`); the daemon + delivery-target
# tmux session are stood up by scratchpad/record_cc.sh before this tape runs.
Output control-center-journey.gif

Set Shell "bash"
Set FontSize 13
Set Width 1600
Set Height 900
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

# Open the control center (tab \`C\`). The seeded ASK auto-selects at the top.
Type "C"
Sleep 5s
Screenshot "cc-1-open.png"

# PAUSE on the visible ASK card (question + ①②③ options, ▶ASK above WAIT).
Sleep 3s
Screenshot "cc-2-card.png"

# Answer option 2 (prod). The daemon delivers it and flips the row to answered,
# dropping the ASK off the open board (0 need you).
Type "2"
Sleep 6s
Screenshot "cc-3-answered.png"
Sleep 2s
EOF

echo "tape written: $TAPE"

# --- record ---
cd "$ASSETS"
vhs control-center-journey.tape
echo "vhs done; gif + screenshots in $ASSETS"
ls -la "$ASSETS"

# --- teardown (kill only what we created, by exact name / pid) ---
tmux kill-session -t "$TARGET" 2>/dev/null || true
# kill the daemon we spawned (bound to this HOME)
for p in $(pgrep -f 'ainb-hangar-daemon' || true); do
  if lsof -p "$p" 2>/dev/null | grep -q "$HOME_DIR"; then kill "$p" 2>/dev/null || true; echo "killed daemon $p"; fi
done
rm -rf "$HOME_DIR"
echo "teardown complete"
