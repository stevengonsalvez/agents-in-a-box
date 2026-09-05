# vhs tape: phases 1-2 — a waiting agent is a chip on its own row.
#
# What replaced "check five surfaces, then guess": the sessions screen names
# the count in its own title, and each waiting row carries its own chip with
# the age the producer stamped. No hangar daemon runs here.
#
#   cargo build --release   # debug is enough
#   vhs ../docs/assets/screenshots/attention-chips.tape

Output ../docs/assets/screenshots/attention-chips.gif

Set Theme "Catppuccin Mocha"
Set FontSize 14
Set Width 1600
Set Height 900
Set Padding 20

Type "../scripts/attention-surface-demo.sh"
Enter

# Cold start: the TUI has to paint the home screen before anything is real.
Sleep 12s

# The home screen names the sessions screen's key.
Sleep 3s

# The sessions screen. Its title carries the answer to "who needs me" before
# any row is read: "Workspaces (2) · 3 need you".
Type "s"
Sleep 8s

# Two workspaces, three waiting rows, each chip carrying its own age. The
# grouping is by worktree, so the two sessions sharing one are together.
Sleep 8s
