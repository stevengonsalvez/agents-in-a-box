# j1.tape, full run of the live chat bus smoke journey "j1".
# chat bus fan out into three real tmux panes
#
# Nothing is staged: this drives ainb-tui/scripts/chat-bus-smoke.sh, which
# stands up a scratch hangar home, a real daemon, real tmux sessions and the
# ACP fixture adapter, then asserts the user visible outcome and prints
# SMOKE-RESULT. The whole journey is recorded end to end, no cuts.

Output explainers/recordings/chat-bus/j1.gif
Output explainers/recordings/chat-bus/j1.mp4

Set Theme "Dracula"
Set FontSize 15
Set Width 1600
Set Height 900
Set Padding 16
Set Shell bash
Set TypingSpeed 25ms

Type "./ainb-tui/scripts/chat-bus-smoke.sh j1"
Enter
# Terminates on the VERDICT, never a guessed Sleep: a tape whose Sleep ends
# before the journey does yields a banner with no verdict, which looks like
# evidence and is not. The pattern is NOT in the typed command above, because
# `Wait+Screen` matches the whole screen and the shell echoes what it types.
Wait+Screen@900s /SMOKE-RESULT j1 /
Sleep 3s
