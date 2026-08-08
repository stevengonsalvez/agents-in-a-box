# ABOUTME: A minimal stand-in for Claude Code's composer, used by
# tests/tmux_send_integrity.rs to drive the real send path against a real tmux
# pane without a real (token-spending) claude session.
#
# It models exactly the four properties the send path depends on, and nothing
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
#   4. A CR into an EMPTY composer does nothing. Claude Code does not submit an
#      empty turn, so an extra Enter from a retry is a no-op rather than a
#      second message, and a test can still assert exactly-once delivery.
#
# A third mode, "mute", models a composer that EXISTS but has not started
# rendering what it was sent, which is the pane state the ingest gate must
# refuse to press Enter into. It draws an empty composer forever and logs a
# literal "CR" for every carriage return it receives, so a test can prove no
# Enter was pressed.
#
# A fourth mode, "lazy", ACCEPTS the turn on the first CR but keeps the payload
# painted in the composer for LAZY_REDRAW_S afterwards, which is what a busy
# pane looks like between accepting a turn and repainting. It is the shape that
# proves the verify loop reports a DELIVERED payload as delivered: the composer
# is still showing the payload at the first post-Enter check and is empty by the
# time the retry looks again.
#
# Every accepted message is appended to the log file, NUL-separated, so a test
# can assert exactly-once delivery byte for byte.

import codecs
import os
import select
import sys
import time

MODE = sys.argv[1]
LOG = sys.argv[2]
COLS = 76
VIEWPORT_ROWS = 6
RULE = "─" * 78
PROMPT = "❯ "
LAZY_REDRAW_S = 1.5

decoder = codecs.getincrementaldecoder("utf-8")()
buffer = ""
accepted = 0
repaint_at = None


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


def submit(text):
    """Take one turn, the way a real composer does: an EMPTY one is ignored."""
    global accepted
    if not text:
        return
    accept(text)
    accepted += 1


draw()
while True:
    timeout = None if repaint_at is None else max(0.0, repaint_at - time.time())
    readable, _, _ = select.select([0], [], [], timeout)
    if repaint_at is not None and time.time() >= repaint_at:
        # The deferred repaint the "lazy" target owes: the turn was accepted a
        # moment ago, and only now does the composer clear.
        buffer = ""
        repaint_at = None
        draw()
    if not readable:
        continue
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
    elif MODE == "lazy":
        while "\r" in text:
            head, _, text = text.partition("\r")
            buffer += head
            # A CR that arrives while the repaint is still owed lands in a
            # composer that is logically already empty, so it is a no-op.
            if repaint_at is None and buffer:
                submit(buffer)
                # The turn is TAKEN, but the composer keeps showing it.
                repaint_at = time.time() + LAZY_REDRAW_S
        buffer += text
    else:
        while "\r" in text:
            head, _, text = text.partition("\r")
            buffer += head
            submit(buffer)
            buffer = ""
        buffer += text
    draw()
