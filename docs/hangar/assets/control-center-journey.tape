# Control-center journey — tmux-verify frame-truth recording.
# Launches the real ainb TUI against a running hangar daemon seeded with one
# 3-option ask_user_question, opens the control center (C), and answers option 2.
# Paths are baked in (vhs 0.11 has no `Set Env`); the daemon + delivery-target
# tmux session are stood up by scratchpad/record_cc.sh before this tape runs.
Output control-center-journey.gif

Set Shell "bash"
Set FontSize 13
Set Width 1600
Set Height 900
Set Padding 12

# Launch the TUI (hidden while it cold-loads).
Hide
Type "HOME='/tmp/cc.Maokrv' AINB_PLUGIN_ROOT='/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_feat_hangar-parity/ainb-tui/dist/plugins' exec '/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_feat_hangar-parity/ainb-tui/target/debug/ainb' tui"
Enter
Sleep 12s
Show

# Home screen is up. Open the Hangar plugin screen.
Type "g"
Sleep 4s

# Open the control center (tab `C`). The seeded ASK auto-selects at the top.
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
