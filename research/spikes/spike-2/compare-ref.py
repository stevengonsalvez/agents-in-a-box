#!/usr/bin/env python3
"""Compare a canonical snapshot's text grid against tmux's own capture-pane grid.

tmux 3.4 is the emulator R2 sits behind, so its grid is the closest available
ground truth for how the bytes should land. Only the text layer is compared;
capture-pane cannot express per-cell attributes the same way the snapshot does.
"""
import re
import sys

snap, ref, rows = sys.argv[1], sys.argv[2], int(sys.argv[3])

snap_rows = []
for line in open(snap, encoding="utf-8", errors="surrogateescape"):
    m = re.match(r"^r(\d{3}) t \|(.*)\|\n?$", line)
    if m:
        snap_rows.append(m.group(2).rstrip())

ref_rows = [l.rstrip("\n").rstrip() for l in open(ref, encoding="utf-8", errors="surrogateescape")]
while len(ref_rows) < rows:
    ref_rows.append("")
ref_rows = ref_rows[:rows]
snap_rows = (snap_rows + [""] * rows)[:rows]

bad = []
for i, (a, b) in enumerate(zip(snap_rows, ref_rows)):
    if a != b:
        bad.append((i, a, b))

print(f"rows={rows} mismatched={len(bad)}")
for i, a, b in bad:
    print(f"  r{i:03d} emu |{a}|")
    print(f"  r{i:03d} tmx |{b}|")
sys.exit(0)
