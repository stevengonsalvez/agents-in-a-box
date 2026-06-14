#!/usr/bin/env python3
"""Trim + tighten an asciinema v2 cast for a crisp explainer gif.

Usage: trim_cast.py IN.cast OUT.cast [START_MARKER] [GAP_CAP] [TAIL_PAD]

- Drops everything before the first event whose output contains START_MARKER
  (default: first real frame), rebasing timestamps to 0.
- Caps every inter-event idle gap to GAP_CAP seconds (default 0.5) so dead
  pauses (startup waits, settle sleeps) compress without losing motion frames.
- Keeps TAIL_PAD seconds of hold on the final frame (default 1.2).
"""
import json, sys

inp, outp = sys.argv[1], sys.argv[2]
marker = sys.argv[3] if len(sys.argv) > 3 else None
gap_cap = float(sys.argv[4]) if len(sys.argv) > 4 else 0.5
tail_pad = float(sys.argv[5]) if len(sys.argv) > 5 else 1.2

lines = open(inp).read().splitlines()
header = json.loads(lines[0])
events = [json.loads(l) for l in lines[1:] if l.strip()]

# find start index: first event whose data contains the marker (if given)
start = 0
if marker:
    for i, e in enumerate(events):
        if len(e) >= 3 and marker in e[2]:
            start = i  # start AT the board-paint event, not the preceding clear
            break
events = events[start:]

# rebase + cap gaps
out_events = []
prev_in = events[0][0] if events else 0.0
clock = 0.0
for e in events:
    t, code, data = e[0], e[1], e[2]
    gap = t - prev_in
    prev_in = t
    if gap < 0:
        gap = 0.0
    clock += min(gap, gap_cap)
    out_events.append([round(clock, 3), code, data])

# pad the tail so the final state holds a beat
if out_events:
    last = out_events[-1]
    out_events.append([round(last[0] + tail_pad, 3), "o", ""])

with open(outp, "w") as f:
    f.write(json.dumps(header) + "\n")
    for e in out_events:
        f.write(json.dumps(e) + "\n")

dur = out_events[-1][0] if out_events else 0
print(f"trimmed {len(lines)-1} -> {len(out_events)} events, {dur:.1f}s (from start marker {marker!r}, gap_cap {gap_cap}s)")
