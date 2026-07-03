# CH0 (profile editor) + CH1 (boards: live card creation) — MASTER journey.
Output "ch0-ch1-profile-boards.gif"

Set Shell "bash"
Set FontSize 13
Set Width 2200
Set Height 1000
Set Padding 12

Hide
Type "HOME='/tmp/mj0.QR0f6c' AINB_PLUGIN_ROOT='/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_feat_hangar-parity/ainb-tui/dist/plugins' exec '/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_feat_hangar-parity/ainb-tui/target/debug/ainb' tui"
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

# --- CH1: Boards screen (B) — live card creation ---
Type "B"
Sleep 3s
Screenshot "ch1-1-boards-open.png"

# 'c' opens the card-title input.
Type "c"
Sleep 2s
Screenshot "ch1-2-title-input-open.png"

# Type the real journey task title (the same one CH2/CH3 drive interactively).
Type "Ask which env to ship to using the AskUserQuestion tool, wait for the answer, then say DONE"
Sleep 2s

# Enter advances to the assignee-profile picker (offers journey-runner).
Enter
Sleep 3s
Screenshot "ch1-3-assignee-picker.png"

# Enter commits the create — the card lands on the board with its assignee.
Enter
Sleep 4s
Screenshot "ch1-4-card-created.png"
Sleep 1s
