# vhs tape: phase 8 — broadcast, with the checkboxes AS the recipient list.
#
# The deleted Fleet panel had its own recipient picker. Here the rows an
# operator has already checked are the recipients, so there is no second list
# to disagree with the first.
#
#   vhs ../docs/assets/screenshots/attention-broadcast.tape

Output ../docs/assets/screenshots/attention-broadcast.gif

Set Theme "Catppuccin Mocha"
Set FontSize 14
Set Width 1600
Set Height 900
Set Padding 20

Type "../scripts/attention-surface-demo.sh"
Enter

# Cold start: the TUI has to paint the home screen before anything is real.
Sleep 14s

Type "s"
Sleep 8s

# Check a row in each workspace. The checkboxes ARE the recipient list.
Space
Sleep 3s
Down
Down
Sleep 2s
Space
Sleep 4s

# The same proven prefix as the answer tape: one Down to settle the list, then
# one Tab per tab. `Tab` is overloaded here — it cycles the strip and also
# switches focus — so pressing it blind from a cold screen is how earlier takes
# ended on the wrong pane.
Down
Sleep 2s
# preview -> ask
Tab
Sleep 3s
# ask -> the broadcast tab, which only exists while rows are checked and
# renames itself with the count.
Tab
Sleep 6s

Type "status check please"
Sleep 3s
Enter

# NO daemon runs here, and broadcast is daemon-owned: the pane refuses and
# says exactly why, keeping the text. That is the honest half of this phase —
# the recipients came from the checkboxes, and a failure is reported as one
# rather than swallowed.
Sleep 9s
