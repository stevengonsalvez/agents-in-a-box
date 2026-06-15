# vhs tape: overlay-panels — open the Stats panel from the SESSION LIST
# and Esc back to the SESSION LIST (panel returns to its origin, NOT home).
#
# Same seeded-fixture setup as overlay-home-stats-home.tape (see that file
# for the $HOME seed steps). The only difference is the `s` hop to the
# session list before opening Stats with `i`.

Output overlay-sessions-stats-sessions.gif

Set FontSize 12
Set Width 1500
Set Height 880
Set Padding 8

Hide
Type "HOME=$HOME_FIXTURE AINB_PLUGIN_ROOT=$PLUGINS AINB_NOW=$NOW exec ./target/debug/ainb tui"
Enter
Sleep 30s
Show

Sleep 2s
Type "s"
Sleep 3.5s
Type "i"
Sleep 11s
Escape
Sleep 4s
