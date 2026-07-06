#!/usr/bin/env python3
"""Record one Hangar journey end-to-end into a tight cast, then trim + agg to gif.

Everything runs in ONE process so the cast only spans the journey (no idle
render-ticks bleeding in across tool round-trips).

Usage: rec_journey.py <journey> <ainb_bin>
journeys: hero | drag | rightclick | clickopen
"""
import subprocess as sp, sys, time, os

journey = sys.argv[1]
AINB = sys.argv[2]
HOME = "/tmp/hangar-demo/home"
OUT = "/tmp/hangar-rec"
SESS = "hrec"
cast = f"{OUT}/{journey}.cast"

def tmux(*a): return sp.run(["tmux", *a], capture_output=True, text=True)
def cap(): return tmux("capture-pane", "-t", SESS, "-p").stdout
def key(k): tmux("send-keys", "-t", SESS, k);
def lit(s): tmux("send-keys", "-t", SESS, "-l", s)
def sgr(btn, col, row, press): lit(f"\x1b[<{btn};{col};{row}{'M' if press else 'm'}")
def cell(c, n):
    for y, l in enumerate(c.splitlines()):
        i = l.find(n)
        if i >= 0: return (i+1, y+1)
    return None

# fresh rec session; pane runs asciinema whose child is ainb tui
tmux("kill-session", "-t", SESS)
tmux("new-session", "-d", "-s", SESS, "-x", "180", "-y", "45")
tmux("send-keys", "-t", SESS,
     f"asciinema rec --overwrite -c 'env HOME={HOME} {AINB} tui' {cast}", "Enter")
time.sleep(9)                      # ainb tui launch + daemon connect
key("Escape"); time.sleep(1)       # dismiss notify popup if present
key("g"); time.sleep(6)            # open Hangar -> Issues board (snapshot)

def find_todo_card(c):
    for t in ["Refactor API client retries", "Add dark-mode toggle",
              "Fix flaky upload test", "Document the plugin API"]:
        p = cell(c, t)
        if p: return p, t
    return None, None

if journey == "drag":
    time.sleep(1.5)
    c = cap(); src, title = find_todo_card(c); ip = cell(c, "In Progress")
    pc, pr = src[0]+6, src[1]; dc, dr = ip[0]+6, src[1]
    sgr(0, pc, pr, True); time.sleep(0.6)           # press: card lifts
    n = 9
    for k in range(1, n+1):
        sgr(32, pc + (dc-pc)*k//n, pr, True); time.sleep(0.16)
    sgr(0, dc, dr, False); time.sleep(2.0)          # release + settle

elif journey == "clickopen":
    time.sleep(1.5)
    c = cap(); src, title = find_todo_card(c)
    cc, cr = src[0]+6, src[1]
    sgr(0, cc, cr, True); time.sleep(0.15); sgr(0, cc, cr, False)  # click
    time.sleep(2.5)                                  # task detail opens
    key("Escape"); time.sleep(1.5)                   # back to board

elif journey == "rightclick":
    time.sleep(1.5)
    c = cap(); src, title = find_todo_card(c)
    cc, cr = src[0]+6, src[1]
    sgr(2, cc, cr, True); time.sleep(0.15); sgr(2, cc, cr, False)  # right-click
    time.sleep(2.5)                                  # context menu shows
    key("Down"); time.sleep(0.7); key("Down"); time.sleep(0.9)     # navigate menu
    key("Escape"); time.sleep(1.2)                   # dismiss

elif journey == "hero":
    # grand tour of the new card-board across screens
    for k, hold in [("2",2.0),("3",2.0),("4",2.0),("K",2.2),("U",2.0),
                    ("L",1.8),("I",1.8),(",",2.0),("1",2.2)]:
        key(k); time.sleep(hold)

# quit ainb tui so asciinema finalizes the cast
key("Escape"); time.sleep(0.4); key("Escape"); time.sleep(0.4); key("q"); time.sleep(2.5)
tmux("kill-session", "-t", SESS)

# Encode: gap-cap the WHOLE cast (no mid-stream cut -> every agg frame is a
# complete raster), then ffmpeg-trim the leading startup off the RASTER gif
# (safe on rasters; -ss 4 lands just past the board-load on this startup timing).
full = f"{OUT}/{journey}_full.gif"
sp.run(["python3", f"{OUT}/trim_cast.py", cast, f"{OUT}/{journey}_trim.cast", "", "0.3", "1.0"])
sp.run(["agg", "--theme", "monokai", "--font-size", "14",
        f"{OUT}/{journey}_trim.cast", full], stdout=sp.DEVNULL, stderr=sp.DEVNULL)
# palette-based trim+re-encode for crisp colours
pal = f"{OUT}/{journey}_pal.png"
sp.run(["ffmpeg","-y","-ss","4","-i",full,"-vf","palettegen=max_colors=128",pal],
       stdout=sp.DEVNULL, stderr=sp.DEVNULL)
sp.run(["ffmpeg","-y","-ss","4","-i",full,"-i",pal,"-lavfi","paletteuse",
        f"{OUT}/{journey}.gif"], stdout=sp.DEVNULL, stderr=sp.DEVNULL)
dur = sp.run(["ffprobe","-v","error","-show_entries","format=duration","-of","default=nk=1:nw=1",
              f"{OUT}/{journey}.gif"], capture_output=True, text=True).stdout.strip()
sz = os.path.getsize(f"{OUT}/{journey}.gif")
print(f"{journey}.gif: {dur}s, {sz//1024}KB")
