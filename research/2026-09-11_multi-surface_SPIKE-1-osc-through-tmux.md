# Spike 1: does a custom OSC status frame survive tmux?

Question: can a process inside a tmux pane emit an in-band status frame
(`OSC 9999`) that a desktop app reads back, and through which tmux feed?

## Environment [fact]

| item | value |
|------|-------|
| tmux | 3.4 |
| server | private socket, `tmux -L ainb-spike1` (all commands) |
| pane/window | 120x40, `new-session -d -s s1 -x 120 -y 40` |
| rendered reader A | `script -q -f -c "tmux -L ainb-spike1 attach-session -t s1 -f read-only,ignore-size"` (80x24 pty) |
| rendered reader B | Rust, `portable-pty` 0.8, pty 120x40, same attach command |
| control reader | `sleep 16 \| tmux -L ainb-spike1 -C attach-session -t s1` |
| capture | `tmux -L ainb-spike1 capture-pane -p -e -t s1 -S -200` |
| emitter | 20 iterations, each: heartbeat line, bare frame, DCS-wrapped frame, 500 ms apart |

`-f read-only,ignore-size` is valid syntax on 3.4. No fallback to `-r` needed. [fact]

`allow-passthrough` default on a fresh 3.4 server is `off`. It is a pane option;
`set-option -g` and `set-option -p` both read back consistently. [fact]

### Frame bytes as emitted [fact]

```
bare:  1b 5d 39 39 39 39 3b 7b ... 7d 07
       ESC ] 9 9 9 9 ;  {json}  BEL

dcs:   1b 50 74 6d 75 78 3b 1b 1b 5d 39 39 39 39 3b 7b ... 7d 07 1b 5c
       ESC P t m u x ;  ESC ESC ] 9 9 9 9 ; {json} BEL ESC \
```

## Results matrix

Cells are frames found / 20 emitted.

| allow-passthrough | form | rendered attach | control mode `%output` | `capture-pane -e` |
|-------------------|------|-----------------|------------------------|-------------------|
| `off` (default)   | bare | 0 / 20          | 20 / 20 intact         | 0 / 20            |
| `off` (default)   | dcs  | 0 / 20          | 20 / 20 intact         | 0 / 20            |
| `on`              | bare | 0 / 20          | 20 / 20 intact         | 0 / 20            |
| `on`              | dcs  | **20 / 20 intact** | 20 / 20 intact      | 0 / 20            |
| `all`             | bare | 0 / 20          | 20 / 20 intact         | 0 / 20            |
| `all`             | dcs  | **20 / 20 intact** | 20 / 20 intact      | 0 / 20            |

All cells [fact]. Rendered `on`/`dcs` reproduced identically by both readers:
`script` pty (8367 bytes, 20 hits) and `portable-pty` (8038 bytes, 20 hits).

Nothing was mangled anywhere. Every hit carried the full JSON payload and its
`form` marker byte for byte. Misses were total absences, not corruptions: the
string `9999` appears zero times in the whole artefact. [fact]

`on` and `all` behaved identically here because the emitting pane was the
current pane of the only window. `all` is expected to differ only for panes in
non-visible windows. [inference]

### One hit: rendered attach, passthrough `on`, dcs form [fact]

From `spike1/out-pty/rendered.bin`. tmux has consumed the DCS envelope and
forwarded only the inner OSC:

```
b'ARTBEAT 01\r\n\x1b]9999;{"v":1,"state":"working","form":"dcs","n":1}\x07\x1b(B\x1b[m\x1b[?12l\x1b[?25h'
```

### One hit: control mode, passthrough `off` [fact]

From `spike1/out-off/control.log`, raw (tmux octal-escapes `%output`). Both
forms arrive in one `%output`, and the DCS envelope is delivered verbatim,
un-consumed, including `Ptmux;` and the trailing `\033\134` (ESC `\`):

```
%output %2 HEARTBEAT 01\015\012\033]9999;{"v":1,"state":"working","form":"bare","n":1}\007\033Ptmux;\033\033]9999;{"v":1,"state":"working","form":"dcs","n":1}\007\033\134
```

### One miss: rendered attach, passthrough `off` [fact]

From `spike1/out-off/rendered.bin`, 4600 bytes, zero occurrences of `9999`.
tmux emitted only its own redraw traffic; the heartbeat text survives, the OSC
does not:

```
b'\x1b[?1049h\x1b[22;0;0t\x1b[?1h\x1b=\x1b[H\x1b[2J\x1b[?12l\x1b[?25h\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1005l\x1b[?2004h\x1b(B\x1b[m'
```

### Duplication behaviour [fact]

In the rendered stream the heartbeat text appears 26 to 32 times for 20
emissions, because tmux repaints screen regions. The passthrough OSC appears
exactly 20 times, never duplicated. Passthrough is forwarded once at emission
time and is not part of the repainted screen state, so a rendered-attach
consumer does not need to de-duplicate frames. It does need to de-duplicate
text scraped from the same stream. [inference from the counts above]

## ignore-size result

Fresh 120x40 session per case, one 80x24 pty client attached, measured via
`display -p -t s1 '#{window_width}x#{window_height}'`. [fact]

| client flags | before | during | after detach |
|--------------|--------|--------|--------------|
| plain attach | 120x40 | 80x23  | 80x23 |
| `-f ignore-size` | 120x40 | 80x23 | 80x23 |
| `-f read-only,ignore-size` | 120x40 | 80x23 | 80x23 |
| `-f read-only` | 120x40 | 80x23 | 80x23 |

`-f ignore-size` does NOT prevent the window resize on tmux 3.4. All four cases
shrank the window to the client size minus one row for the status line, and the
window did not recover after the client detached. [fact]

What does hold the size: `set-option -g window-size manual` plus
`resize-window -t s1 -x 120 -y 40`. With that set, a plain 80x24 attach left the
window at 120x40 during attach and after detach. [fact]

```
┌──────────────┐  plain / -f ignore-size   ┌──────────┐
│ window 120x40│ ─────────────────────────▶│  80x23   │  ✗ lost, permanent
└──────────────┘                           └──────────┘

┌──────────────┐  window-size manual       ┌──────────┐
│ window 120x40│ + resize-window ─────────▶│ 120x40   │  ✓ pinned
└──────────────┘                           └──────────┘
```

One measurement was inconclusive: with a control-mode client already attached, a
subsequent plain writable attach caused the test session to disappear before the
size could be read. Cause not established, likely an artefact of driving
`script` with a non-tty stdin. Not pursued, because `window-size manual` answers
the size question without needing it. [fact that it happened, cause unestablished]

## Decision input

1. **Tier-2 feed that works: control mode `%output`.** It delivers pre-parse
   bytes. Both frame forms arrive at 20/20 with `allow-passthrough off`, the
   server default. The daemon sets nothing. [fact]

2. **The spec's claim held.** "Control mode delivers pre-parse bytes so
   passthrough is not needed" is confirmed: the DCS envelope itself reaches the
   control client un-consumed, which is only possible if tmux forwards the pane's
   output before its own escape parsing. [fact]

3. **Rendered attach also works, but only under two conditions**: the daemon must
   set `allow-passthrough on` (or `all` for background windows), and the emitter
   must wrap every frame in the tmux DCS envelope with each inner ESC doubled. A
   bare OSC through a rendered attach is 0/20 at every setting and is not a viable
   path. [fact]

4. **`capture-pane -e` is dead for this.** 0/20 in all six cells. It replays the
   screen grid with SGR attributes; an OSC sets no cell attribute so nothing is
   retained. Do not design a polling fallback on it. [fact for the counts,
   inference for the mechanism]

5. **Emit both forms unconditionally.** Bare plus DCS-wrapped costs about 60
   extra bytes per frame and makes the emitter correct under control mode,
   rendered attach, and a direct non-tmux pty, with no runtime detection of
   whether tmux is present. [inference]

6. **Window sizing is a separate daemon obligation.** `-f ignore-size` is not
   protection on 3.4. If the daemon attaches any rendered client, it must first
   set `window-size manual` and `resize-window` to the intended geometry, or the
   pane is permanently resized to whatever the app's pty happened to be. [fact]

### Recommendation

Control mode for the status feed. Zero tmux configuration, both forms arrive,
no redraw duplication to filter, and no dependence on a pane option a user's
own `tmux.conf` could turn off.

## Artefacts

```
spike1/emit.sh            emitter
spike1/run_cell.sh        one matrix cell (control + rendered + capture)
spike1/pty_cell.sh        portable-pty verification cell
spike1/bonus2.sh          ignore-size, fresh session per case
spike1/bonus3.sh          window-size manual
spike1/analyze.py         octal-decodes %output, counts frames
spike1/attach-reader/     Rust portable-pty reader
spike1/out-off/           passthrough off: rendered.bin control.log capture.txt
spike1/out-on/            passthrough on
spike1/out-all/           passthrough all
spike1/out-pty/           portable-pty rendered.bin
```
