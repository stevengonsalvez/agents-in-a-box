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
# Output: ../docs/assets/screenshots/attention-answer.gif

Output ../docs/assets/screenshots/attention-answer.gif

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

# Down to the payments-api rows. TWO live sessions share that worktree, which
# is what makes an answer there refuse rather than guess a pane. billing-worker
# sorts first and has only one, so it would simply succeed.
Down
Sleep 3s

# Into the ask pane for the selected question. The strip opens on `preview`,
# so `ask` is ONE Tab away — two lands on `thread`, whose composer prompts the
# session instead of answering the question.
Tab
Sleep 5s

# An answer, typed where the question is, not on another screen.
Type "data-dir"
Sleep 3s
Enter

# Two live sessions share this worktree, so the send REFUSES rather than typing
# into whichever pane the name happens to hit. The pane says so, against the
# question it belongs to, and keeps what was typed.
Sleep 7s

# Away to the other waiting question. The walk goes via `log`, the only
# neighbouring tab that does NOT own a composer: `thread` and `copilot` both
# swallow the arrows into their own prompt. Strip order is
# preview|ask|thread|copilot|log, so `log` is three Tabs from `ask` and `ask`
# is two Tabs from `log`.
Tab
Tab
Tab
Sleep 2s
Up
Sleep 3s
Tab
Tab
Sleep 7s

# Its pane reports NOTHING, because nobody answered it. The failure above
# belongs to a different question and stayed there.
Tab
Tab
Tab
Sleep 2s
Down
Sleep 3s
Tab
Tab
Sleep 8s
