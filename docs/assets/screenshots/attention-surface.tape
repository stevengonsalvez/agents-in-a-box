# vhs tape: the attention surface — one screen answers "who needs me".
#
# Drives scripts/attention-surface-demo.sh, which seeds a throwaway $HOME with
# two waiting agents and launches a real `ainb tui`. No hangar daemon runs:
# the chips come from the LOCAL notifyd producer, which is the half the epic is
# judged on, and the answer travels the real tmux send path.
#
# The fixture matches tripwire_sessions_answer_outcome_per_question, so what
# this shows is a path CI drives.
#
# Run from the ainb-tui/ workspace root (so ../scripts resolves):
#   cargo build --release
#   vhs ../docs/assets/screenshots/attention-surface.tape
#
# Output: ../docs/assets/screenshots/attention-surface.gif

Output ../docs/assets/screenshots/attention-surface.gif

Set Theme "Catppuccin Mocha"
Set FontSize 14
Set Width 1600
Set Height 900
Set Padding 20

Type "../scripts/attention-surface-demo.sh"
Enter

# Cold start: the TUI has to paint the home screen before anything is real.
Sleep 12s

# The sessions screen. Its title carries the count, so the answer to "who needs
# me" is legible before any row is read.
Type "s"
Sleep 6s

# The chip strip: two workspaces, three waiting rows, with the ages the local
# producer stamped.
Sleep 4s

# Into the ask pane for the selected question. The strip opens on `preview`,
# so `ask` is ONE Tab away — two lands on `thread`, whose composer prompts the
# session instead of answering the question.
Tab
Sleep 4s

# An answer, typed where the question is, not on another screen.
Type "data-dir"
Sleep 3s
Enter

# Two live sessions share this worktree, so the send REFUSES rather than typing
# into whichever pane the name happens to hit. The pane says so, against the
# question it belongs to.
Sleep 6s

# Away to the other waiting question. The walk goes via `log`, which is the
# only neighbouring tab that does NOT own a composer: `thread` and `copilot`
# both swallow the arrow keys into their own prompt, so a Down pressed there
# never reaches the session list. Strip order is preview│ask│thread│copilot│log,
# so `log` is three Tabs from `ask` and `ask` is two Tabs from `log`.
Tab
Tab
Tab
Sleep 2s
# TWICE. One row down is the OTHER session on the same shared worktree, which
# is the same question and so, correctly, the same outcome. The point here is a
# DIFFERENT question, which is the next workspace down.
Down
Down
Sleep 3s
Tab
Tab
Sleep 6s

# Its pane reports nothing, because nobody answered it. The failure above
# belongs to a different question and stayed there.
Tab
Tab
Tab
Sleep 2s
Up
Up
Sleep 3s
Tab
Tab
Sleep 7s
