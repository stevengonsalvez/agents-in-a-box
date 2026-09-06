# vhs tape: phases 5-6 — what is NOT on the screen any more.
#
# The host Fleet panel (`f`) and the host Inbox (`b`) are gone, and their
# claims moved rather than lapsed. A deletion is only real when the key that
# opened it stops being advertised, so the help overlay is the evidence.
#
#   vhs ../docs/assets/screenshots/attention-deletions.tape

Output ../docs/assets/screenshots/attention-deletions.gif

Set Theme "Catppuccin Mocha"
Set FontSize 14
Set Width 1600
Set Height 900
Set Padding 20

Type "../scripts/attention-surface-demo.sh"
Enter

# Cold start: the TUI has to paint the home screen before anything is real.
Sleep 12s

Type "s"
Sleep 6s

# The help overlay lists every key this screen answers. Neither `b Inbox` nor
# `f Fleet control panel` is among them; the panel legend at the foot of the
# screen does not offer them either.
Type "?"
Sleep 10s
