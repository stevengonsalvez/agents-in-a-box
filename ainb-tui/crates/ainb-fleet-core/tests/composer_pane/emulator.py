# ABOUTME: A minimal stand-in for Claude Code's composer, used by
# tests/tmux_send_integrity.rs to drive the real send path against a real tmux
# pane without a real (token-spending) claude session.
#
# It models exactly the three properties the send path depends on, and nothing
# else:
#
#   1. The composer is the block between two full-width horizontal rules, and
#      only its FIRST row carries the prompt glyph (followed by U+00A0, as the
#      real one does).
#   2. The viewport is TAIL-ANCHORED and short, so a long payload's head
#      scrolls out of view and is unrecoverable from the capture, while the
#      glyph stays on the first VISIBLE row. That is the measured 80x24
#      behaviour (see pane_fixtures/x80-parked-heartbeat.txt) and it is what
#      made a head-derived needle useless.
#   3. Submission is a property of the TARGET, not of the sender: in "deaf"
#      mode a CR is swallowed exactly as it is when it arrives fused onto the
#      tail of a payload read on a busy pane, and the payload stays parked.
#
# A third mode, "mute", models a composer that EXISTS but has not started
# rendering what it was sent, which is the pane state the ingest gate must
# refuse to press Enter into. It draws an empty composer forever and logs a
# literal "CR" for every carriage return it receives, so a test can prove no
# Enter was pressed.
#
# Every accepted message is appended to the log file, NUL-separated, so a test
# can assert exactly-once delivery byte for byte.

import codecs
import os
import sys

MODE = sys.argv[1]
LOG = sys.argv[2]
COLS = 76
VIEWPORT_ROWS = 6
RULE = "─" * 78
PROMPT = "❯ "

decoder = codecs.getincrementaldecoder("utf-8")()
buffer = ""
accepted = 0


def rows():
    wrapped = [buffer[i : i + COLS] for i in range(0, len(buffer), COLS)] or [""]
    visible = wrapped[-VIEWPORT_ROWS:]
    return [PROMPT + visible[0]] + ["  " + row for row in visible[1:]]


def draw():
    screen = ["\x1b[2J\x1b[H", "transcript: %d accepted" % accepted, RULE]
    screen.extend(rows())
    screen.append(RULE)
    screen.append("status line")
    sys.stdout.write("\r\n".join(screen) + "\r\n")
    sys.stdout.flush()


def accept(text):
    with open(LOG, "ab") as handle:
        handle.write(text.encode("utf-8") + b"\x00")


draw()
while True:
    chunk = os.read(0, 4096)
    if not chunk:
        break
    text = decoder.decode(chunk)
    if MODE == "deaf":
        # The CR never reaches a submit handler: it is consumed inside the
        # paste buffer, which is the measured CR-fusion failure.
        buffer += text.replace("\r", "")
    elif MODE == "mute":
        # The composer stays empty: nothing of the payload is ever rendered, so
        # the ingest gate can never observe it. Every CR is logged instead of
        # being acted on, so a test can assert Enter was never pressed.
        for _ in range(text.count("\r")):
            accept("CR")
    else:
        while "\r" in text:
            head, _, text = text.partition("\r")
            buffer += head
            accept(buffer)
            accepted += 1
            buffer = ""
        buffer += text
    draw()
