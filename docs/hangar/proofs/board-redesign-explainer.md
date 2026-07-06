# Hangar — control plane explainer (4 tabs)

**Live explainer:** https://lapis-finch-dy2m.here.now/ (permanent) · deep-link `#status` `#e2e` `#vs`

Four tabs in one self-contained page:

- **Showcase** — the mouse-driven card-board redesign (epic 63l), recorded against the
  real `ainb` TUI in tmux over a seeded local demo (`fake-claude.sh` provider stub;
  real lifecycle FSM + DB transitions).
- **Status & roadmap** — the full feature matrix (what's shipped), verification
  coverage, what's next (merge #250 → v1.0 release ceremony → follow-ups `v70`/`2qo`/
  `2a8`/`0mf`), and the epic timeline (174 build → e38 parity → 63l redesign → v1.0).
- **End-to-end** — the full user loop, validated: create an issue on the board
  (`createissue.gif`, HGR-5 lands), then a cron autopilot fires a real hello-world task
  that the daemon executes (`execute.gif`) — proven against **SQLite** (task `done`,
  `autopilot_run` `completed`, result = the agent output + PR url), not a green count.
- **vs Multica** — a comprehensive feature + user-journey comparison against the
  original `github.com/multica-ai/multica` (verified from a clone + the Hangar source):
  feature matrix by area, gaps with reasons (scope-cut / unbuilt / partial), and journey
  coverage. Full analysis in [`multica-comparison.md`](multica-comparison.md).

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
