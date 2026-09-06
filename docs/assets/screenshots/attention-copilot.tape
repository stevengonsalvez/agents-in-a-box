# vhs tape: phase 7 — the copilot tab, its engine picker and guardrail dial.
#
# Drives scripts/attention-surface-demo.sh --daemon: this half of the surface
# is daemon-owned, so it needs one running to show anything true. The daemon
# comes up under its own isolated home and is stopped on the way out.
#
# The key prefix here (`s`, Down, then Tab per tab) is copied from
# attention-answer.tape because that sequence is PROVEN to land on `ask`.
# `Tab` is overloaded on this screen — it cycles the tab strip and also
# switches focus between the list and the pane — so a tape that just presses
# it three times from a cold screen lands somewhere unpredictable, which is
# how two earlier takes of this ended on `preview`.
#
#   vhs ../docs/assets/screenshots/attention-copilot.tape

Output ../docs/assets/screenshots/attention-copilot.gif

Set Theme "Catppuccin Mocha"
Set FontSize 14
Set Width 1600
Set Height 900
Set Padding 20

Type "../scripts/attention-surface-demo.sh --daemon"
Enter

# Cold start plus the daemon's own bring-up before the TUI dials it.
Sleep 42s

Type "s"
Sleep 14s

Down
Sleep 5s

# preview -> ask
Tab
Sleep 7s
# ask -> thread
Tab
Sleep 7s
# thread -> copilot
Tab
Sleep 14s
