# vhs tape: phase 3 — the tab strip that replaced five screens.
#
# preview · ask · thread · copilot · log, all scoped to the selected session.
# The log tab is what a session's notification history became when the host
# Inbox was deleted.
#
#   vhs ../docs/assets/screenshots/attention-tabs.tape

Output ../docs/assets/screenshots/attention-tabs.gif

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
Sleep 7s

# preview -> ask: the question, and somewhere to answer it.
Tab
Sleep 5s

# ask -> thread: the session's own chat, and where a broadcast is composed.
Tab
Sleep 5s

# thread -> copilot: the ACP copilot, with its engine picker and guardrail.
Tab
Sleep 5s

# copilot -> log: this session's notification history, which is where the
# deleted host Inbox went.
Tab
Sleep 6s
