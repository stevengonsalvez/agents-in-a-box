# Spike 2: control-mode emulator fidelity

Ran 2026-09-12. Question: does a Rust VT emulator fed by tmux control mode
reproduce a live agent screen byte-equal to the same program driven through a
direct PTY, and which crate should R2 use?

The filename carries 2026-09-11 to sit with the rest of the multi-surface spike
set, which was commissioned that day. Every measurement here was taken on
2026-09-12.

All work on a private tmux server, socket `ainb-spike2`, every command carries
`-L ainb-spike2 -f /dev/null`. Sessions killed by exact name. No repo code was
touched; the harness lives in the session scratchpad.

## Verdict

**The control-mode feed is lossless. Fidelity is not the deciding factor; the
crates differ, tmux does not.** [fact]

Across 8 fixtures at 120x40 and 40x20, the byte stream reassembled from
`%extended-output` was **byte-identical** to the bytes read off a direct
`portable-pty`, and every emulator's canonical snapshot was byte-equal between
the two feeds. Zero differences, so there is no difference list to classify.
[fact]

Against two real agent TUIs driven live, an emulator seeded from
`capture-pane -e` and tailed from control mode matched tmux's own grid on
**40 of 40 rows at every capture**, including after keystroke-driven repaints.
[fact]

**Crate: `wezterm-term`, configured `unicode_version: 14`.** It is the only
candidate whose cell advance agrees with tmux on every glyph tested, it is the
only one that costs 413 KB per emulator instead of 3.1-4.0 MB, and it carries
OSC 8, title and alt-screen state natively. It is the slowest parser of the
three, and it gets one margin case wrong. Both are acceptable; see the rationale.

```
┌──────────────┐  %extended-output  ┌───────────┐  byte-identical  ┌───────────┐
│ tmux 3.4 pane│ ─────────────────▶ │  unvis    │ ───────────────▶ │ VT crate  │
└──────────────┘                    └───────────┘                  └───────────┘
        │                                                                │
        │ capture-pane -N                                        canon snapshot
        ▼                                                                ▼
   tmux own grid  ◀────────── 0 rows differ (seeded) ────────────────────┘
```

## Environment [fact]

| item | value |
|------|-------|
| tmux | 3.4 |
| socket | `ainb-spike2`, private, `-f /dev/null` on every command |
| host | Linux 6.8.0-137, AMD EPYC-Rome, 8 cores, 15 GB RAM |
| rustc | 1.96.0 |
| `vt100` | 0.16.2 (crates.io) |
| `alacritty_terminal` | 0.26.0 (crates.io), `vte` 0.15.0 |
| `wezterm-term` | git `wezterm/wezterm` rev `2afb836403838c3ed7e09e5d570190adb054b607`; not published on crates.io |
| `portable-pty` | 0.9.0 |
| feed | `tmux -C attach-session` on a pipe, `refresh-client -C <c>x<r>`, `-f pause-after=2`, `-A "%<pane>:on"` |
| control reference | `capture-pane -p -N` and `-p -N -e`, issued **through the control stream** so the reply is ordered against `%output` |
| live window | 1,000 rows of scrollback in every crate, matching the R2 spec default |
| agents | `claude` 2.1.269, `codex-cli` 0.148.0, real TUIs, 120x40 |

Both feeds run the same fixture with the same environment (`TERM=xterm-256color`,
`LANG=C.UTF-8`, `LC_ALL=C.UTF-8`) and the same geometry, so the program under
test cannot tell them apart.

## 1. Feed fidelity: control mode vs a direct PTY

Each fixture is run twice: once as the command of a tmux session read through a
control client, once under `portable-pty` at the same size. Each side is then
replayed into each crate **using its own real arrival boundaries** (the pty's
`read()` sizes, the control feed's per-notification payload sizes), so a parser
that mishandles a split escape sequence or a split grapheme would show up.

`raw` compares the reassembled byte streams. `feed_vs_pty` compares the
canonical snapshots (grid text, per-cell fg/bg/bold/italic/underline/inverse,
OSC 8 target, double-width flags, cursor row/col/visibility, alt-screen flag,
title, mouse mode) after ANSI normalisation, which here means dropping trailing
default blank cells per row.

| fixture | size | bytes pty/tmux | raw | pty reads / ctl notifications | feed vs pty |
|---|---|---|---|---|---|
| f1 alt-screen TUI | 120x40 | 2308/2308 | **SAME** | 1 / 2 | vt100 EQ · alacritty EQ · wezterm EQ · wezterm14 EQ |
| f1 alt-screen TUI | 40x20 | 2308/2308 | **SAME** | 21 / 1 | all EQ |
| f2 mouse tracking | 120x40 | 246/246 | **SAME** | 1 / 1 | all EQ |
| f2 mouse tracking | 40x20 | 246/246 | **SAME** | 6 / 1 | all EQ |
| f3 wide chars | 120x40 | 281/281 | **SAME** | 5 / 2 | all EQ |
| f3 wide chars | 40x20 | 281/281 | **SAME** | 5 / 2 | all EQ |
| f4 OSC 8 links | 120x40 | 338/338 | **SAME** | 4 / 1 | all EQ |
| f4 OSC 8 links | 40x20 | 338/338 | **SAME** | 4 / 1 | all EQ |
| f5 OSC 9999 status | 120x40 | 837/837 | **SAME** | 2 / 2 | all EQ |
| f5 OSC 9999 status | 40x20 | 837/837 | **SAME** | 4 / 1 | all EQ |
| f6 scroll | 120x40 | 4993/4993 | **SAME** | 2 / 2 | all EQ |
| f6 scroll | 40x20 | 4993/4993 | **SAME** | 46 / 3 | all EQ |
| f7 DECSTBM probe | 120x40 | 31/31 | **SAME** | 1 / 1 | all EQ |
| f7 DECSTBM probe | 40x20 | 31/31 | **SAME** | 1 / 1 | all EQ |
| f8 width probe | 120x40 | 175/175 | **SAME** | 6 / 1 | all EQ |
| f8 width probe | 40x20 | 175/175 | **SAME** | 3 / 2 | all EQ |

All cells [fact]. 16 fixture-size pairs, 4 emulator configurations, 64
snapshot comparisons, zero differences. Criterion 1's fallback branch ("or every
difference is listed with the exact bytes and a verdict") is empty because the
primary branch holds.

The stronger statement the `raw` column licenses: for these workloads tmux
control mode is a **transparent byte pipe**. The emulator does not need to be
tolerant of a tmux transform, because there is no transform. What it does need
is tolerance of chunking, which the next section covers.

### What tmux does to the bytes on the wire [fact]

`%extended-output %<pane> <age> : <data>`, one line per notification.
`<data>` is `vis(3)`-escaped: every byte tmux considers unprintable becomes a
backslash plus **exactly three octal digits**, including the backslash itself.
Measured directly:

```
input  : A \ TAB h é 中 文 🚀 ESC ] 8 ; ; ... BEL
on wire: A\134\011h M-C M-) M-d M-8 M-- ... \033]8;;...\007
```

`\134` is `\`, `\011` is TAB, `\033` is ESC, `\007` is BEL. UTF-8 is **not**
escaped, it passes through as raw bytes. There is no `\\` form, so the decoder
is a single rule. [fact]

### The chunking hazard, with bytes [fact]

tmux will end a notification **in the middle of a multi-byte grapheme**. In one
27.7 MB control capture, 98 of 14,349 lines were not valid UTF-8. First
occurrence, exact bytes:

```
b'n patch diff agent token \\033[0m \xe4\xb8\xad\xe6\x96\x87 \xf0\x9f\n'
                                                              ^^^^^^^^
                                    first two bytes of U+1F680, rest in the
                                    next %extended-output line
```

Consequence, and it is not theoretical: the first version of this harness read
the control stream with `BufRead::read_line`, which requires UTF-8. It returned
`Err` on that line, the reader loop exited, and `Child::wait()` then deadlocked
against a tmux client still writing into a pipe nobody was draining. The symptom
was a silent hang under load only, never on small fixtures. **A control-mode
parser must be byte-oriented end to end.** [fact, reproduced and fixed]

### What a control client does *not* get [fact]

A control client receives nothing that was produced before it attached. Attaching
to a pane that had already painted a static frame and then tailing gave a grid
that differed from tmux's on exactly the two rows painted before attach:

```
  r000 emu ||                                            <- never seen
  r000 tmx |┌─ live agent ──────────────────────────┐|
  r003 emu ||
  r003 tmx |└───────────────────────────────────────┘|
```

Seeding the emulator from `capture-pane -p -e` first and then tailing gave
0 of 40 rows different, at two successive capture points, on all four
configurations. This is snapshot-then-tail, and it is mandatory, not an
optimisation.

## 2. Live agent TUIs

`claude` and `codex` were each started in a 120x40 pane, left to paint, then
attached by a control client that seeded from `capture-pane -e`, tailed
`%extended-output`, and pressed `Down` then `Up` through `send-keys` on the
control stream. At each of three points the harness asked tmux for
`capture-pane -p -N` **inside the same ordered stream** and diffed it against
the emulator's grid at the moment the reply arrived.

| agent | backend | live bytes tailed | s0 | s1 (after Down) | s2 (after Up) |
|---|---|---|---|---|---|
| claude | vt100 | 222 | 0/40 | 0/40 | 0/40 |
| claude | alacritty | 222 | 0/40 | 0/40 | 0/40 |
| claude | wezterm | 222 | 0/40 | 0/40 | 0/40 |
| claude | wezterm14 | 222 | 0/40 | 0/40 | 0/40 |
| codex | vt100 | 2563 | 0/40 | 0/40 | 0/40 |
| codex | alacritty | 2563 | 0/40 | 0/40 | 0/40 |
| codex | wezterm | 2563 | 0/40 | 0/40 | 0/40 |
| codex | wezterm14 | 2563 | 0/40 | 0/40 | 0/40 |

All cells "mismatched rows / total rows" [fact].

The keystrokes did move the screen, so this is a live test and not a static one:

```
claude, s0 -> s1
-  ❯ No, exit                          +   No, exit
-    Yes, I trust this folder          + ❯ Yes, I trust this folder

codex, s0 -> s1
- › 1. Update now (...)                +   1. Update now (...)
-   2. Skip                            + › 2. Skip
```

## 3. Crate comparison

### 3a. Fidelity against tmux's own grid

tmux 3.4 is the emulator R2 sits behind, so its grid is the reference: where the
daemon's emulator and tmux disagree, a `tmux attach` user and a daemon-mediated
viewer see different screens. Cells are **mismatched rows out of the viewport**,
control-mode-fed snapshot vs `capture-pane -p -N`.

| fixture | size | vt100 | alacritty | wezterm | wezterm14 |
|---|---|---|---|---|---|
| f1 alt-screen TUI | 120x40 | **5** | 0 | 0 | 0 |
| f1 alt-screen TUI | 40x20 | **5** | 0 | 0 | 0 |
| f2 mouse tracking | both | 0 | 0 | 0 | 0 |
| f3 wide chars | 120x40 | 1 | 2 | 2 | 2 |
| f3 wide chars | 40x20 | 1 | 2 | **4** | **4** |
| f4 OSC 8 links | both | 0 | 0 | 0 | 0 |
| f5 OSC 9999 status | both | 0 | 0 | 0 | 0 |
| f6 scroll | both | 0 | 0 | 0 | 0 |
| f7 DECSTBM probe | both | **2** | 0 | 0 | 0 |
| f8 width probe | both | 1 | 2 | 2 | 2 |

Every non-zero cell is explained below. Two of the three explanations are
artefacts of comparing text grids; one is a real defect in each of vt100 and
wezterm.

### 3b. vt100 does not home the cursor on DECSTBM [fact, blocker for vt100]

Minimal probe f7: `CUP 6;1`, then `DECSTBM 6;18`, then print `MARK`.

```
tmux 3.4      MARK on row 1     (DECSTBM homes the cursor)
alacritty     MARK on row 1
wezterm       MARK on row 1
vt100         MARK on row 6     ✗
```

This is why vt100 loses 5 rows on the alt-screen fixture: a real TUI that sets a
scroll region and then paints starts painting in the wrong place, and every row
of the frame it should have overwritten stays stale. Any full-screen agent UI
that uses a scroll region is affected. **Verdict: blocker for vt100.**

### 3c. Cell advance per glyph [fact]

The authoritative width comparison, because it does not depend on how anything
prints a double-width cell. Each glyph is written at column 1 and the resulting
cursor column read back: from tmux with `#{cursor_x}` after writing straight to
`#{pane_tty}`, from each crate with the same bytes through the harness.

| glyph | tmux | vt100 | alacritty | wezterm | wezterm14 |
|---|---|---|---|---|---|
| `A` | 1 | 1 | 1 | 1 | 1 |
| U+4E2D 中 | 2 | 2 | 2 | 2 | 2 |
| U+1F600 😀 | 2 | 2 | 2 | 2 | 2 |
| U+2764 ❤ bare | 1 | 1 | 1 | 1 | 1 |
| U+2764 U+FE0F ❤️ | 2 | **1** | **1** | **1** | 2 |
| U+2764 U+FE0E | 1 | 1 | 1 | 1 | 1 |
| `e` U+0301 | 1 | 1 | 1 | 1 | 1 |
| U+1F469 ZWJ U+1F4BB 👩‍💻 | 2 | **4** | **4** | 2 | 2 |
| U+1F3F4 + tag seq 🏴󠁧󠁢󠁥󠁮󠁧󠁿 | 2 | 2 | 2 | 2 | 2 |
| U+FF71 ｱ | 1 | 1 | 1 | 1 | 1 |
| U+2588 █ | 1 | 1 | 1 | 1 | 1 |
| U+2500 ─ | 1 | 1 | 1 | 1 | 1 |
| U+00E9 é | 1 | 1 | 1 | 1 | 1 |

**wezterm-term with `unicode_version: 14` is the only configuration that agrees
with tmux on all 13.** vt100 and alacritty each disagree on two: they give an
emoji-presentation sequence width 1 where tmux gives 2, and they give a ZWJ
sequence width 4 where tmux gives 2. Both errors shift every cell to the right
of the glyph on that row, in a TUI that draws box borders by column. wezterm at
its default Unicode 9 gets the ZWJ case right and the VS16 case wrong; the knob
fixes it, and there is no equivalent knob in the other two.

The residual text-grid rows in 3a break down differently per crate, and lumping
them together would hide the one that matters. Row by row, measured:

| crate | fixture, size | rows | which rows | cause | verdict |
|---|---|---|---|---|---|
| alacritty | f3, both sizes | 2 | EMO, TAG | VS16 width 1 against tmux's 2; tag-sequence truncation | cosmetic |
| wezterm14 | f3, 120x40 | 2 | EMO, TAG | pad-space artefact; tag-sequence truncation | cosmetic |
| wezterm14 | f3, **40x20** | **4** | EMO, TAG, **STRD and the row below it** | pad-space, truncation, **and the section 3d margin defect** | **two rows are real** |
| all | f8, both sizes | 1 to 2 | the VS16 and tag probe rows | as above | cosmetic |

Two clarifications this table forces. First, wezterm14 has **no** width
disagreement with tmux (3c: 13 of 13), so its EMO row is purely a printing
artefact of this harness: `capture-pane` emits a pad space for the second column
of a width-2 grapheme and the canonical dump does not, so an identical grid
reads as a differing line. Alacritty's EMO row looks the same but is not the
same thing: it is a genuine width disagreement.

Second, wezterm14's jump from 2 rows to 4 at 40x20 is **not** an artefact. It is
the section 3d last-column defect, and the exact rows are:

```
  r005 emu |STRD:abcdefghijklmnopqrstuvwxyz01234567中|
  r005 tmx |STRD:abcdefghijklmnopqrstuvwxyz01234567|
  r006 emu | after|
  r006 tmx |中 after|
```

alacritty stays at 2 rows at the same size, which is what isolates the cause to
wezterm rather than to the width model. [fact]

The tag-sequence row is a third case where the crates are arguably ahead of
tmux: tmux truncates U+1F3F4 plus six tag characters to five codepoints in its
cell, alacritty and wezterm keep all seven. Both advance 2 columns, so nothing
downstream moves. **Verdict: cosmetic.**

### 3d. wezterm places a double-width glyph in the last column [fact]

60-column terminal, N narrow columns already filled, then `中ZZ`:

| filled | tmux | vt100 | alacritty | wezterm / wezterm14 |
|---|---|---|---|---|
| 57 | `...xxx中Z` / `Z` | same | same | same |
| 58 | `...xxxx中` / `ZZ` | same | same | same |
| 59 | `...xxxxxx ` / `中ZZ` | same | same | **`...xxxx中` / `ZZ`** ✗ |

With exactly one column free, tmux, vt100 and alacritty leave it blank and wrap
the glyph. wezterm squeezes the glyph into columns 59-60, **destroying the
narrow character already in column 59**. One cell of content is lost and the row
is one column out of step with tmux until it is rewritten.

**Verdict: real defect, cosmetic in effect, and narrower terminals hit it more
often.** It costs one cell, not the line, and the next full repaint clears it.
It is not a blocker. But "rare" needs qualifying: across the fixture set it fired
in 1 of 8 fixtures and **only at 40x20, never at 120x40** (section 3a), because
the fewer the columns the likelier a double-width glyph lands on the last one.
40x20 is the phone frame R2 explicitly targets, so this is most likely exactly
where R2 meets it. Watch for a CJK-heavy agent UI reading one column off on the
phone surface; if it becomes visible the fix belongs upstream in `wezterm-term`,
not in a workaround. [fact for the fixture counts, inference for the frequency
argument]

### 3e. Feature surface [fact]

| capability | vt100 0.16 | alacritty 0.26 | wezterm-term |
|---|---|---|---|
| OSC 8 hyperlink per cell | **absent** | yes | yes |
| window title | callback only, no stored state | private field, event listener only | `get_title()` |
| alt-screen flag | yes | yes | yes |
| mouse reporting mode | `mouse_protocol_mode()` + encoding | `TermMode` bits | not exposed on the model |
| scrollback bound | `Parser::new(rows, cols, n)` | `Config::scrolling_history` | `TerminalConfiguration::scrollback_size` |
| Unicode width version | fixed | fixed | **configurable** |
| snapshot-then-tail fit | feed bytes, read grid | feed bytes, read grid | feed bytes, read grid |
| re-snapshot on `%continue` | works, no reset API needed | works | works |

OSC 8 is load-bearing for R2 (agent TUIs emit file and issue links) and vt100
has no model for it at all. Verified: on the OSC 8 fixture, alacritty and
wezterm both recovered 5 link runs including two adjacent links on one row and
one link that survives a wrap; vt100 recovered 0.

Mouse mode is the one place vt100 and alacritty are ahead: both expose the
current reporting mode as state, wezterm-term keeps it inside its input encoder.
R2 needs the mode to decide whether a phone viewer should send mouse events at
all. Cheap workaround: the daemon already parses the byte stream, so it can
track `DECSET 1000/1002/1003/1006` itself, or ask tmux for
`#{?pane_in_mode,...}` and the pane's own mode flags. **Verdict: fixable, small.**

None of the three needs a reset API for the `%continue` path: replaying
`ESC[H ESC[2J` plus one absolute-positioned row per `capture-pane -e` line
reproduces the grid exactly, measured at 0/40 rows difference.

### 3f. Cost

RSS, 100 emulators at 120x40 with a 1,000-row live window, each fed the same
140,530-byte agent-transcript workload (1,800 lines, SGR, 256-colour, truecolor,
CJK and emoji). `delta_kb` is process RSS after minus before.

| crate | 1 emulator | 100 emulators | per emulator | 100 sessions |
|---|---|---|---|---|
| vt100 | 3,968 KB | 395,472 KB | 3,955 KB | **386 MB** |
| alacritty | 3,292 KB | 309,652 KB | 3,097 KB | **302 MB** |
| wezterm | 2,012 KB | 41,288 KB | **413 KB** | **40 MB** |
| wezterm14 | 2,004 KB | 41,280 KB | **413 KB** | **40 MB** |

All [fact]. wezterm-term is 7.5x cheaper than alacritty and 9.6x cheaper than
vt100 at the same live-window depth, because it stores each line as a string
plus cluster runs rather than a fixed array of fat cells per scrollback row.
[inference for the mechanism, fact for the numbers]

CPU, `cat` of a 50 MB file in a pane, delivered end to end through control mode
into a live emulator, 120x40, 1,000-row window. The file is the RSS workload
repeated to 50 MB, so it is **attributed** content: 256-colour and truecolor SGR
runs, bold and reverse, CJK and emoji, not plain text. (The plain monotonic
counter, `f10-flood.sh`, appears only in section 5, where loss has to be
countable token by token.)

| crate | delivered | notifications | pauses | max age | emulator CPU | process CPU | process RSS | tmux server CPU | grid vs tmux |
|---|---|---|---|---|---|---|---|---|---|
| vt100 | 53.1 MB | 33,463 | 0 | 4 ms | 1.58 s | 2.35 s | 8.7 MB | 7.64 s | **0/40** |
| alacritty | 53.1 MB | 33,086 | 0 | 4 ms | 1.51 s | 2.19 s | 7.8 MB | 7.55 s | **0/40** |
| wezterm | 53.1 MB | 33,621 | 0 | 9 ms | 3.80 s | 4.40 s | 6.5 MB | 8.00 s | **0/40** |
| wezterm14 | 53.1 MB | 33,616 | 0 | 5 ms | 3.53 s | 4.17 s | 6.1 MB | 7.71 s | **0/40** |

All [fact]. The delivered figure is exact, not rounded: the file is 52,428,800
bytes with 671,544 newlines, the pty turns each `LF` into `CRLF`, and the feed
delivered 53,100,344 bytes, which is 52,428,800 + 671,544 to the byte. Nothing
was lost or duplicated across 33,000 notifications. [fact]

Read three things off this table. First, **the flood never tripped
`pause-after=2` for any crate**: buffering age peaked at 9 ms against a 2,000 ms
threshold, so a daemon-speed reader is nowhere near the flow-control boundary.
Second, **the tmux server costs about twice the most expensive emulator** (7.5
to 8.0 s against 1.5 to 3.8 s), so the emulator is not the bottleneck in the
hybrid; tmux is, and it is a cost R2 pays either way. Third, the grid after
53 MB of flood still matched tmux exactly.

Pure parse throughput, same 50 MB with no tmux in the path:

| crate | wall | CPU | MB/s |
|---|---|---|---|
| alacritty | 0.558 s | 0.550 s | **94.0** |
| vt100 | 0.763 s | 0.760 s | 68.7 |
| wezterm | 2.220 s | 2.150 s | 23.6 |
| wezterm14 | 2.201 s | 2.190 s | 23.8 |

wezterm-term is 4x slower per byte than alacritty. The number that matters is
the previous table, not this one: delivering the same 50 MB cost the tmux server
7.71 s of CPU and cost wezterm14 3.53 s, so the chosen emulator adds about 46%
on top of a cost the hybrid already pays, where alacritty would add about 20%.
[fact for the seconds, arithmetic for the percentages] At 100 sessions this only
bites if many flood at once, and per-pane `pause-after` exists to cap that.

### 3g. The call

**`wezterm-term` at `unicode_version: 14`.**

It is the only crate whose cell advance matches tmux on every glyph tested, and
that matters more than raw speed here: the whole premise of D10's hybrid is that
the daemon's grid and the grid a `tmux attach` user sees are the same grid, and
a width disagreement breaks that silently on exactly the content agent TUIs are
full of, box-drawn panels next to emoji and CJK. It is also the only crate that
fits 100 sessions in 40 MB rather than 300 to 390 MB, which decides whether the
live window stays at 1,000 rows or has to be cut. It has OSC 8 and title state
built in. The two costs are real and both are survivable: it parses at 24 MB/s
instead of 94, which on the 50 MB run in 3f is 3.53 s of CPU against the 7.71 s
the tmux server spent delivering the same bytes, and it mishandles a double-width glyph landing on the very last
column, which costs one cell until the next repaint. vt100 is out on DECSTBM
alone, before OSC 8 and before memory. alacritty is the fallback if wezterm's
git-only distribution is unacceptable: it is fast, correct on margins, and its
two width errors are the price.

One packaging caveat: `wezterm-term` is not on crates.io. This spike pinned
`git = "https://github.com/wezterm/wezterm", rev = "2afb836..."`, which pulls a
large repository and its submodules at build time. Vendoring the `term`,
`wezterm-cell`, `wezterm-surface` and `wezterm-escape-parser` crates, or using a
published fork, is an R2 packaging decision, not a fidelity one.

## 4. Window geometry: what the daemon's control client does to a second client

The claim under test is D10's "control mode, excluded from window sizing".
Method: a session pinned at 120x40, a second **ordinary rendered client**
attached at 100x30 through `portable-pty`, then a daemon-style control client
attached and detached while `#{window_width}x#{window_height}` is read back
**as that second client sees it** (`display -p -c <client>`).

| `window-size` | control client sets `-C 120x40` | before | during | during +2 s | after detach |
|---|---|---|---|---|---|
| `manual` + `resize-window` | yes | 120x40 | **120x40** | **120x40** | **120x40** |
| `manual` + `resize-window` | no | 120x40 | **120x40** | **120x40** | **120x40** |
| `latest` (tmux default) | yes | 100x29 | **120x40** ✗ | 120x40 | 100x29 |
| `latest` (tmux default) | no | 100x29 | **100x29** | **100x29** | **100x29** |

All [fact], two clients attached in every row.

Criterion met: with `window-size manual` plus `resize-window`, the second
client's `#{window_width}` **never changes**, whatever the control client does.
[fact]

D10's wording needs one correction. A control client is excluded from window
sizing only while it has no size of its own. The moment it calls
`refresh-client -C`, it participates like any other client, and under tmux's
default `window-size latest` it yanked the window from 100x29 to 120x40 under
the human's feet, restoring it only on detach. Two safe configurations, and R2
should adopt both rather than pick:

1. `set-option window-size manual` plus `resize-window` on daemon-owned sessions
   (already required by spike 1 for a different reason), and
2. the daemon's control client never sends `refresh-client -C`; it reads the
   geometry with `refresh-client -B` on `#{window_width}x#{window_height}` and
   sizes its emulator to whatever tmux reports.

Rule 2 alone is sufficient and is the one that also protects sessions a user
started by hand, where the daemon has no business calling `resize-window`.

## 5. Pause and resume, reproduced

Method: 120x40 pane flooding a monotonic 9-digit counter at full pty speed, a
control client with `pause-after=2` deliberately slowed to about 300 KB/s,
`refresh-client -A "%0:continue"` after 8 s, then two grid comparisons against
tmux, the first with no re-snapshot and the second after re-seeding from
`capture-pane -e`.

| measurement | value |
|---|---|
| `%pause` notifications | 1 |
| `%continue` notifications | 1 |
| max buffering age before the pause | 1,970 ms (threshold 2,000 ms) |
| last token the emulator held after `%continue`, no re-snapshot | 000044400 |
| token tmux's grid held at the same instant | 001212781 |
| tokens never delivered | 1,168,381 |
| bytes never delivered | approx 11.7 MB |
| rows differing from tmux, no re-snapshot | **40 of 40** |
| rows differing from tmux, after `capture-pane -e` re-seed | **0 of 40** |

All [fact]. This confirms spike 7's byte-level finding at the level R2 cares
about: the resumed stream is not a continuation, and the rendered grid does not
self-heal. Every row was wrong, and stayed wrong, until the daemon re-snapshotted.

Two parser details that spike 7 did not surface, both measured here:

- **`%continue` arrives inside the `%begin`/`%end` block of the command that
  caused it**, not as a top-level notification. A parser that only inspects
  lines outside command blocks counts zero continues and never re-snapshots.
  This harness scored `continues=0` until it was fixed to scan block bodies.
  [fact, reproduced]
- `%pause` likewise. Both must be recognised wherever they appear in the stream.

### What a `capture-pane` re-seed does and does not carry [fact]

The 0 of 40 results above are text-row comparisons, so on their own they do not
prove that a re-seed restores everything an emulator holds. Measured separately,
with a fixture that enters the alternate screen, sets a window title, hides the
cursor, turns on mouse reporting and paints an OSC 8 hyperlink, then dumping the
emulator's whole canonical state rather than its text:

| state | tmux | after a grid-only re-seed | after the complete re-seed |
|---|---|---|---|
| OSC 8 hyperlink | present | **present** | present |
| SGR attributes | present | present | present |
| alternate screen | on | **off** | on |
| window title | `spike2 altlink` | **`wezterm`** (crate default) | `spike2 altlink` |
| cursor visibility | hidden | **shown** | hidden |
| cursor position | 3, 9 | 3, 9 | 3, 9 |
| mouse reporting | on | **off** | on |

So `capture-pane -p -e` carries more than SGR: it emits OSC 8 verbatim, which is
worth knowing given section 3e treats hyperlinks as load-bearing.

```
capture-pane -p -e -t %0 | sed -n 2p
^[]8;;https://example.invalid/seed^[\SEEDLINK^[]8;;^[\ plain
```

What it does not carry is everything tmux keeps outside the character grid. All
of it is available as a format string on the same control connection, so the fix
costs one extra `display-message`:

```
#{alternate_on} #{cursor_flag} #{cursor_y} #{cursor_x}
#{scroll_region_upper} #{scroll_region_lower}
#{keypad_cursor_flag} #{keypad_flag} #{wrap_flag} #{origin_flag} #{insert_flag}
#{mouse_standard_flag} #{mouse_button_flag} #{mouse_any_flag}
#{mouse_sgr_flag} #{mouse_utf8_flag} #{pane_title}
```

Order matters, and three orderings are load-bearing:

1. `ESC[?1049h` or `l` **before** the grid, or the rows paint into the wrong
   buffer and the primary screen is corrupt when the app exits alt mode.
2. `ESC[r` and `ESC[?6l` before the grid, so the repaint is not clipped by a
   scroll region or reinterpreted by origin mode; the real region and origin mode
   go back **after**.
3. the cursor position after DECSTBM (which homes the cursor) and before
   re-enabling origin mode (which changes what `CUP` means).

With that sequence the right-hand column above is exact on all four crate
configurations. One honest limit: tmux's mouse flags are coarser than the modes
they describe. The fixture set `1000`, `1002` and `1006`, and tmux reported
`mouse_any_flag=1`, so the reconstruction turns on `1003` as well and the
emulator reports `AnyMotion` where the app asked for button tracking. The daemon
already parses the byte stream, so it should track `DECSET` itself and use the
tmux flags only for the cold-start seed. [fact for the flag values, inference for
the remedy]

## 6. Decision input

### Does R2 proceed on the tmux hybrid as specified? Yes. [fact-backed]

Fidelity is not a risk. The feed is byte-transparent, the reassembled stream is
byte-identical to a direct pty on every fixture at both sizes, and a seeded
emulator tracks two real agent TUIs and a 53 MB flood with zero rows of drift
against tmux's own grid. The "poor fidelity reopens daemon-owned PTY" branch in
the spike-2 row does not fire, and the do-not-build row for a daemon-owned PTY
stays closed. Its remaining trigger is native Windows, unchanged.

### R2's live-window and re-snapshot rules

1. **Snapshot-then-tail is mandatory, not an optimisation.** A control client
   receives nothing produced before it attached. Seed every pane from
   `capture-pane -p -e` and only then apply `%extended-output`. Measured: 2 rows
   wrong without it, 0 with it.
2. **Re-snapshot on `%continue`, unconditionally**, and emit
   `data_gap{reason: paused}` first. Measured cost of not doing it: 40 of 40
   rows wrong and 11.7 MB silently missing. Same rule on feed loss, with
   `data_gap{reason: feed_lost}`.
3. **Recognise `%pause` and `%continue` inside command reply blocks.** They do
   not arrive as top-level notifications.
4. **Parse the control stream as bytes.** tmux splits multi-byte graphemes
   across notifications under load, 98 lines in 14,349 in one capture. A UTF-8
   line reader fails only under load, which is the worst way to find out.
5. **Re-seed by replay, and replay more than the grid.** `capture-pane -e`
   carries SGR and OSC 8 but nothing outside the character grid, so a grid-only
   replay silently drops the alternate-screen flag, the title, cursor visibility
   and mouse reporting. Restore them from tmux formats in the same round trip,
   in this order: `ESC[?1049h/l`, title, `ESC[r` and `ESC[?6l`, the grid rows,
   then the real scroll region, the modes, the cursor position, origin mode, and
   cursor visibility last. Exact on all four crate configurations, no
   crate-specific reset API needed. Track `DECSET` from the byte stream rather
   than trusting `#{mouse_any_flag}`, which over-reports.
6. **Order the snapshot against the tail by issuing `capture-pane` on the control
   stream itself.** The reply lands in the same ordered stream as `%output`, so
   the daemon knows precisely which bytes the snapshot already contains. This is
   how every 0/40 result above was obtained, and it removes the need for any
   wall-clock fence between snapshot and tail.
7. **Live window: keep 1,000 rows.** With wezterm-term that is 413 KB per
   session, 40 MB at 100 sessions. Raising the window to 5,000 rows measured
   726 KB per session on the same workload, but that workload is only 1,800
   lines long, so the deeper window was never filled: the window is a cap, and
   real cost tracks how much a session actually emits. [fact, and the caveat is
   why it is not 5x] With vt100 or alacritty, 1,000 rows already costs 300 to
   390 MB at 100 sessions and the range would have to be cut.
8. **Never send `refresh-client -C` from the daemon's control client**, and set
   `window-size manual` on daemon-created sessions. Either alone holds the
   geometry; the combination also protects hand-started sessions.
9. **`pause-after=2` has ample headroom at daemon speed.** 53 MB through the feed
   peaked at 9 ms of buffering age against a 2,000 ms threshold and never
   paused. The pause path is for slow *viewers*, not for fast panes; keep it
   armed, expect it to be quiet on the daemon's own client.
10. **Budget tmux, not the emulator.** The tmux server burned 7.5 to 8.0 s of CPU
    delivering the 50 MB that cost the chosen emulator 3.5 s. Per-pane
    `pause-after` protects the viewer; nothing here protects the server, and its
    cost is the floor of the hybrid.

### If fidelity had been a blocker

It was not, on any crate, so the concrete case for reopening the daemon-owned
PTY row is not made here. For the record, the evidence that would have made it:
a transform in the `raw` column of section 1, or a live agent capture that could
not be brought to 0 rows by re-snapshotting. Neither occurred in 64 snapshot
comparisons, 24 live agent captures and a 53 MB flood.

## 7. Surprises

- The control stream is **byte-identical** to the pty, not merely equivalent
  after normalisation. The normalisation step in the success criterion turned
  out to be unnecessary. [fact]
- tmux ends notifications mid-grapheme, so the obvious `read_line` parser hangs,
  and only under load. [fact]
- `%continue` is delivered inside a command's reply block. [fact]
- A control client is excluded from window sizing only until it calls
  `refresh-client -C`; then it resizes the window for everyone. This contradicts
  the plain reading of D10. [fact]
- vt100 does not home the cursor on DECSTBM, which quietly corrupts any TUI that
  uses a scroll region. [fact]
- wezterm-term uses 7.5x less memory than alacritty and 9.6x less than vt100 at
  the same live-window depth, and is four times slower per byte. Both were larger
  effects than expected, in opposite directions. [fact]
- `remain-on-exit on` adds a "Pane is dead" banner that scrolls the grid by one
  row, which silently corrupts any `capture-pane` reference taken that way. The
  reference runs here hold the pane open with a trailing `sleep` instead. [fact]

## Artefacts

Parked in the repo at `research/spikes/spike-2/` (force-added; `research/` is
gitignored). Every script locates itself, so the tree runs from wherever it is
checked out. Build products and captures land in `target/` and `out/`, both
ignored by the directory's own `.gitignore`.

```
Cargo.toml Cargo.lock          pinned crate set, incl. the wezterm git rev
src/canon.rs                   crate-independent snapshot and its renderer
src/backends.rs                vt100, alacritty, wezterm, wezterm+unicode14
src/capture.rs                 byte-oriented control-mode reader, pty reader, unvis
src/live.rs                    live attach, seed from capture-pane -e, ordered compare
src/main.rs                    capture-pty capture-tmux snapshot live bench-rss
                               bench-cpu cursor-col attach-client unvis
fixtures/f1-altscreen.sh       alt screen, scroll region, SGR, truecolor
fixtures/f2-mouse.sh           DECSET 1000 1002 1005 1006 1004 2004 DECCKM
fixtures/f3-widechars.sh       CJK, emoji, VS15/VS16, combining, ZWJ, tag flag, margins
fixtures/f4-osc8.sh            OSC 8, BEL and ST forms, id=, adjacent, wrapped
fixtures/f5-oscstatus.sh       OSC 9999 bare and tmux-DCS-wrapped, per spike 1
fixtures/f6-scroll.sh          60 lines of plain scroll
fixtures/f7-decstbm.sh         DECSTBM cursor-home probe
fixtures/f8-widthprobe.sh      per-glyph column advance
fixtures/f9-live.sh            long-lived repainting panel
fixtures/f10-flood.sh          bounded monotonic counter flood, section 5 only
fixtures/f11-altlink.sh        alt screen + title + mouse + OSC 8, for the re-seed test
make-workload.py               the attributed workload and the 50 MB flood file
run-matrix2.sh                 sections 1 and 3a
capture-ref.sh compare-ref.py  tmux capture-pane reference and differ
width-probe.sh                 section 3c
margin-probe.sh                section 3d
geom-proof.sh                  section 4
flood2.sh flood-all.sh         section 3f, 50 MB end to end through the feed
bench-all.sh                   section 3f, RSS at 1 and 100, and parse throughput
pause-run.sh                   section 5
seedstate.sh fmtprobe.sh       section 5, what a re-seed carries
live-agent.sh live-run.sh      section 2
steps-*.txt                    the driving scripts each live run replays
out/                           every .bin .ctl .chunks .snap .refdiff .diff
```

Reproduce, from `research/spikes/spike-2/`:

```sh
cargo build --release
bash run-matrix2.sh                  # sections 1 and 3a, both sizes
bash width-probe.sh                  # section 3c, cell advance against tmux
bash margin-probe.sh                 # section 3d, wide glyph at the margin
bash geom-proof.sh                   # section 4, four geometry cells
bash bench-all.sh                    # section 3f, RSS and parse throughput
bash flood-all.sh                    # section 3f, 50 MB through the feed
bash pause-run.sh wezterm14          # section 5, pause and resume
bash seedstate.sh                    # section 5, what a re-seed carries
bash live-agent.sh claude claude     # section 2, live agent TUI
```

`bench-all.sh` and `flood-all.sh` regenerate `out/workload.bin` and the 50 MB
`out/flood50.bin` when absent, so nothing large is committed and the numbers stay
reproducible. Every script drives the private `ainb-spike2` socket and kills its
sessions by exact name.
