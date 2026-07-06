# P4 / D8 Boards journey — tmux-verify frame-truth recording.
# Launches the real ainb TUI against a running hangar daemon seeded with a
# user-defined five-column "Delivery" board whose card (HG-42 "Ship the tables")
# already RAN to done and AUTO-MOVED into the done-mapped Done column (green ✓).
# Opens the Boards screen (tab B) and reads the rendered outcome. Paths are baked
# in (vhs 0.11 has no `Set Env`); the daemon is stood up by record-boards-journey.sh.
Output "boards-journey.gif"

Set Shell "bash"
Set FontSize 13
Set Width 1900
Set Height 950
Set Padding 12

# Launch the TUI (hidden while it cold-loads).
Hide
Type "HOME='/tmp/bj.VnKWBP' AINB_PLUGIN_ROOT='/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_feat_hangar-parity/ainb-tui/dist/plugins' exec '/Users/stevengonsalvez/.cache/ccc-shared-target/debug/ainb' tui"
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
