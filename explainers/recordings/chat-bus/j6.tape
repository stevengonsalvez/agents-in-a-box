# j6.tape, full run of the live chat bus smoke journey "j6".
# the OPERATING SURFACE: a chat bus ACP session in the real Fleet panel
#
# Nothing is staged: this drives ainb-tui/scripts/chat-bus-smoke.sh, which
# stands up a scratch hangar home, a real daemon, real tmux sessions, the ACP
# fixture adapter AND a cold `ainb tui` on a private tmux server, then asserts
# the user visible outcome and prints SMOKE-RESULT. The whole journey is
# recorded end to end, no cuts.
#
# Run FROM THE REPO ROOT; the Output paths below are repo relative on purpose,
# so this tape records the same way on any checkout:
#
#   VHS_NO_SANDBOX=true \
#   AINB_SMOKE_SKIP_BUILD=1 AINB_SMOKE_BIN_SOURCE=<dir with the three binaries> \
#     vhs explainers/recordings/chat-bus/j6.tape
#
# VHS_NO_SANDBOX=true is required wherever Chromium cannot use its SUID sandbox
# (containers, most CI images). Without it vhs exits `could not launch browser`
# and leaves any PREVIOUS artifact in place, which is how a stale recording
# passes for a fresh one. The two AINB_SMOKE_* knobs are inherited from the
# environment and keep cargo out of the frame; omit them and the harness builds
# first, which is correct but puts minutes of build log before the journey.

# mp4 FIRST. vhs writes the outputs in order, and this box's gif encoder has
# been seen to never finish; with the mp4 already on disk that costs a gif, not
# the recording. Regenerate an empty gif from the mp4 with an ffmpeg palette
# pass — the command is in EXPECTED-OUTCOMES.md.
Output "explainers/recordings/chat-bus/j6.mp4"
Output "explainers/recordings/chat-bus/j6.gif"

Set Theme "Dracula"
Set FontSize 15
Set Width 1400
Set Height 800
Set Padding 20
Set Shell bash
Set TypingSpeed 25ms
# No `Set Framerate` on purpose, same as j1-j5e. vhs captures as fast as the
# headless browser manages and writes at 25 fps, so a slow box yields a mildly
# time-compressed video, which is fine. Do NOT "fix" that by lowering the
# framerate: `Set Framerate 10` on this box captured 196 frames of a three
# minute journey and produced a 7 second blur that still contained every line.
# It looks like a recording and is useless as one.

Sleep 1s
Type "./ainb-tui/scripts/chat-bus-smoke.sh j6"
Enter
# Terminates on the VERDICT itself, never a guessed Sleep: a tape whose Sleep
# ends before the journey does renders a banner with no result line, which looks
# like evidence and is not. The trailing Sleep only lets the summary settle.
#
# `j6` and not `overall` on purpose. When the harness cannot find its binaries it
# prints `SMOKE-RESULT setup FAIL` and never runs a journey at all; matching the
# journey's own name means that case burns the timeout and produces NOTHING,
# rather than a plausible looking tape of a world that never came up.
Wait+Screen@600s /SMOKE-RESULT j6/
Sleep 4s
