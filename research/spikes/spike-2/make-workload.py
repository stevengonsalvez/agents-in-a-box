#!/usr/bin/env python3
"""Generate the agent-transcript workload and the 50 MB flood file.

Deterministic (seeded), and attributed: 256-colour and truecolor SGR runs, bold
and reverse, CJK and emoji, so the RSS and CPU numbers are taken on the kind of
content an agent TUI actually emits rather than on plain text.
"""
import os
import random
import sys

out = os.path.join(os.path.dirname(os.path.abspath(__file__)), "out")
os.makedirs(out, exist_ok=True)
random.seed(2)
words = "the quick brown fox jumps over lazy dog agent tool call result token stream diff patch commit branch".split()
lines = []
for i in range(1800):
    col = 16 + (i % 216)
    txt = " ".join(random.choice(words) for _ in range(random.randint(6, 14)))
    if i % 17 == 0:
        lines.append(f"\x1b[1m\x1b[38;5;{col}m>>> {txt}\x1b[0m\r\n")
    elif i % 23 == 0:
        lines.append(f"\x1b[48;5;236m\x1b[38;2;200;200;80m {txt} \x1b[0m 中文 \U0001F680\r\n")
    else:
        lines.append(f"\x1b[38;5;{col}m{i:04d}\x1b[0m {txt}\r\n")
data = "".join(lines).encode()

with open(os.path.join(out, "workload.bin"), "wb") as f:
    f.write(data)
print(f"workload.bin {len(data)} bytes, {data.count(b'\n')} lines")

if "--flood" in sys.argv:
    target = 50 * 1024 * 1024
    reps = target // len(data) + 1
    with open(os.path.join(out, "flood50.bin"), "wb") as f:
        f.write((data * reps)[:target])
    print(f"flood50.bin {target} bytes")
