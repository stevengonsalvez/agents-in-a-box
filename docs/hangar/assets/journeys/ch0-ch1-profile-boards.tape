# CH0 (profile editor) + CH1 (boards + confirmed defect capture) — MASTER journey.
Output "ch0-ch1-profile-boards.gif"

Set Shell "bash"
Set FontSize 13
Set Width 2200
Set Height 1000
Set Padding 12

Hide
Type "HOME='/tmp/mj0.fYJjtL' AINB_PLUGIN_ROOT='/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_feat_hangar-parity/ainb-tui/dist/plugins' exec '/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_feat_hangar-parity/ainb-tui/target/debug/ainb' tui"
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
