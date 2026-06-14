# Hangar board redesign — visual proof (epic agents-in-a-box-63l)

**Live explainer:** https://lapis-finch-dy2m.here.now/ (permanent)

The mouse-driven card-board redesign, recorded against the real `ainb` TUI driven
in tmux over a seeded local demo (mocked `fake-claude.sh` provider; real lifecycle
FSM + DB transitions).

## Recorded journeys

| Journey | What it shows | Proof |
|---|---|---|
| Hero tour | `g` opens Hangar, then every screen tab-by-tab | `hero.gif` |
| The board | 5-column card-board (Backlog/Todo/In Progress/In Review/Done), HGR-N cards, priority chips, clay focus border | `board_still.png` |
| Drag to move | real mouse drag moves a card across columns → `Todo (4)→(3)`, `In Progress (0)→(1)`, durable over the daemon socket | `drag.gif` |
| Right-click | context menu (Open / Move to / Priority / Assign / Copy id / Delete), each leaf → real RPC | `rightclick.gif` |
| Click to open | left-click a card → its task detail (status / assignee / project) | `clickopen.gif` |

## How the mouse was captured

vhs cannot drive a mouse, so the mouse journeys use **real SGR mouse escape
sequences** injected into the live TUI over `tmux send-keys -l`, captured with
`asciinema` + `agg`:

- press:   `ESC[<0;col;rowM`
- drag:    `ESC[<32;col;rowM`
- release: `ESC[<0;col;rowm`
- right-click press/release: `ESC[<2;col;rowM` / `ESC[<2;col;rowm`

Coordinates are resolved from `tmux capture-pane` (locate the card title + the
destination column header), exactly as the `tripwire_mouse_drag_moves_card`
real-tmux tripwire does. Casts are gap-capped (compress idle) then the leading
startup is trimmed off the raster gif with ffmpeg.

Recording harness: `rec_journey.py` (one-shot per journey), `trim_cast.py`
(gap-cap), `build2.py` (self-contained Claude-themed HTML). Kept under the
session's `/tmp/hangar-rec` + `/tmp/hangar-explainer`.
