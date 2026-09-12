#!/usr/bin/env python3
"""Marker column per glyph: the exact cell advance each emulator gave it."""
import re, sys
labels = ["ASCII A","CJK 中","emoji 😀","U+2764 ❤ bare","U+2764+VS16","U+2764+VS15",
          "e + combining","ZWJ 👩‍💻","tag flag 🏴...","halfwidth ｱ","block █","box ─"]
def cols_from_snap(path):
    out={}
    for line in open(path, encoding="utf-8", errors="surrogateescape"):
        m=re.match(r"^r(\d{3}) t \|(.*)\|$", line.rstrip("\n"))
        if m:
            r=int(m.group(1)); t=m.group(2)
            if "|" in t: out[r]=t.index("|")
    return out
def cols_from_ref(path):
    out={}
    for i,line in enumerate(open(path, encoding="utf-8", errors="surrogateescape")):
        t=line.rstrip("\n")
        if "|" in t: out[i]=t.index("|")
    return out
ref=cols_from_ref(sys.argv[1])
snaps={n:cols_from_snap(p) for n,p in (a.split("=",1) for a in sys.argv[2:])}
names=list(snaps)
print(f"{'glyph':<18}{'tmux':>6}" + "".join(f"{n:>12}" for n in names))
for i,lab in enumerate(labels):
    row=i
    r=ref.get(row,"-")
    cells="".join(f"{snaps[n].get(row,'-'):>12}" for n in names)
    print(f"{lab:<18}{r:>6}{cells}")
