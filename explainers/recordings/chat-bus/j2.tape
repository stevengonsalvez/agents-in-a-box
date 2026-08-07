# j2.tape, full run of the live chat bus smoke journey "j2".
# ACP transcript streaming while the delivery is still pending
#
# Nothing is staged: this drives ainb-tui/scripts/chat-bus-smoke.sh, which
# stands up a scratch hangar home, a real daemon, real tmux sessions and the
# ACP fixture adapter, then asserts the user visible outcome and prints
# SMOKE-RESULT. The whole journey is recorded end to end, no cuts.

Output explainers/recordings/chat-bus/j2.gif
Output explainers/recordings/chat-bus/j2.mp4

Set Theme "Dracula"
Set FontSize 15
Set Width 1600
Set Height 900
Set Padding 16
Set Shell bash
Set TypingSpeed 25ms

Type "cd /home/claude/.agents-in-a-box/worktrees/by-name/agents-in-a-box--f-chat-bus-e2e"
Enter
Sleep 1s
Type "./ainb-tui/scripts/chat-bus-smoke.sh j2"
Enter
Sleep 220s
