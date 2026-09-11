# Spike 7: tmux control-mode per-pane flow control

Ran 2026-09-11. All work on a private tmux server, socket `ainb-spike7`. Session killed by exact
name at the end; `tmux -L ainb-spike7 ls` reports no server running.

## Verdict

Per-pane pause IS usable as the per-viewer flow-control primitive, with one hard condition: the
daemon MUST re-snapshot the pane on `%continue`, because tmux discards everything produced during
the pause. Measured 17.18 MB silently dropped across one 5.75 s pause. [fact]

The idle pane is completely unaffected while the flooding pane is paused. [fact]

## Environment

| item | value |
|------|-------|
| tmux | 3.4 |
| socket | `ainb-spike7` (private, `-L` on every command) |
| config | `-f /dev/null` (the box's `~/.tmux.conf` sets `history-limit 1000000`, which would have dominated server RSS) |
| history-limit | 2000 lines (tmux default) |
| session | `s7`, one window, 200x50, two panes |
| reader | python3 3.14.6, `subprocess` on `tmux -C attach-session` |
| host | Linux 6.8.0, 15.6 GB RAM |

Exact commands:

```
tmux -L ainb-spike7 -f /dev/null new-session -d -s s7 -x 200 -y 50 -n w0 'sh -i'
tmux -L ainb-spike7 split-window -t s7:w0 "sh <scratch>/hb.sh"
tmux -L ainb-spike7 -C attach-session -t s7
```

Pane A (`%0`) flood, capped at 20M tokens (200 MB) and 70 s:

```sh
exec timeout 70 awk 'BEGIN{for(i=1;i<=20000000;i++) printf "%09d%s", i, (i%20==0?"\n":" ")}'
```

A monotonic 9-digit counter, not `yes`, so loss and contiguity are measurable byte by byte.
Pane B (`%1`) prints `hb <n> <epoch_ms>` once per second.

Commands the reader writes into the control stream, in order:

```
refresh-client -C 200x50
refresh-client -f pause-after=2
refresh-client -A '%0:on' -A '%1:on'
send-keys -t %0 'sh <scratch>/flood.sh' Enter
refresh-client -A '%0:continue'        # 5 s after %pause
```

### Syntax trap, tmux 3.4

`refresh-client -A %0:continue` fails with `parse error: syntax error`. [fact] The tmux command
lexer treats a token whose first character is `%` as a conditional directive (`%if`, `%endif`), so a
bare pane id as its own argument never parses. [inference, from the observed failure plus tmux's
documented `%if` directives]

Working forms, all verified: `-A%0:continue`, `-A"%0:continue"`, `-A '%0:continue'`,
`-A"%0:continue"`. [fact] The first spike run lost its resume because of this: `%error` came back on
both the `:on` and the `:continue` command, pane A never resumed, and the only visible symptom was
that output stopped. A daemon that does not parse `%error` blocks would see a silently dead pane.

Man page, `refresh-client`: "-A allows a control mode client to trigger actions on a pane. The
argument is a pane ID (with leading `%'), a colon, then one of `on', `off', `continue' or `pause'."
Client flag: "pause-after=seconds -- output is paused once the pane is seconds behind in control
mode." Notifications: `%pause pane-id`, `%continue pane-id`, and `%extended-output pane-id age ... :
value` where "age is the time in milliseconds for which tmux had buffered the output before it was
sent."

## Run A: slow reader, pause-after=2

Reader loop: `read(65536)` then `sleep(0.2)`, so roughly 320 KiB/s against a producer running at
2.8 MB/s. 45 s.

| t (s) | event | pane | bytes since last |
|-------|-------|------|------------------|
| 0.010 | flags set, flood started | - | - |
| 0.21 | first `%extended-output`, age 0 ms | %0 | - |
| 1.226 | hb 3 | %1 | 30 B |
| 2.438 | `%pause %0`, last age seen 1880 ms | %0 | 558.4 KiB delivered pre-pause |
| 2.438 | hb 4 | %1 | - |
| 2.980 | hb 5, age 0 ms | %1 | 0 B from %0 |
| 3.981 | hb 6 | %1 | 0 B from %0 |
| 4.984 | hb 7 | %1 | 0 B from %0 |
| 5.986 | hb 8 | %1 | 0 B from %0 |
| 6.989 | hb 9 | %1 | 0 B from %0 |
| 7.991 | hb 10; `refresh-client -A '%0:continue'` sent | %1 | 0 B from %0 |
| 8.192 | `%continue %0` inside the command's `%begin`/`%end` block | %0 | 0.2 KiB from %1 during whole pause |
| 8.192 | output resumes at token 1773247 | %0 | - |
| 10.420 | `%pause %0` again (reader still slow) | %0 | - |
| 10.420 | hb 11 and hb 12 arrive together, 2.429 s gap | %1 | - |
| 45.08 | run ends, hb 47 | %1 | 550.3 KiB from %0 post-continue |

Notification form under `pause-after`: every line is `%extended-output`, zero plain `%output`
lines. [fact] Turning the flag on changes the wire format for the whole client, not just the
paused pane.

Age field, the direct measure of per-pane independence:

| pane | age min | age max |
|------|---------|---------|
| %0 flood | 0 ms | 1880 ms |
| %1 heartbeat | 0 ms | 56 ms |

Pane A climbed to 1880 ms and tripped the 2000 ms threshold. Pane B never exceeded 56 ms in the
same stream. [fact]

## Heartbeat continuity during pause

Yes, uninterrupted. [fact] Heartbeats 5 through 10 arrived at 2.980, 3.981, 4.984, 5.986, 6.989 and
7.991 s, entirely inside the 2.438 to 8.192 s pause window, at a 1.002 s cadence.

| window | max hb gap |
|--------|-----------|
| whole 45 s run | 2.429 s |
| inside pane A's pause | 1.003 s |

Every gap above 1.01 s in the run happened while pane A was actively flooding, never while it was
paused. The worst gap, 2.429 s, sits exactly at t=10.420, the moment of the second `%pause %0`: two
heartbeats had queued behind pane A's backlog and both landed as pane A was cut off. [fact] So
pause-after does not merely preserve the idle pane, it repairs it: the idle pane's latency drops to
its natural 1.0 s cadence the instant the noisy pane is paused.

## Resume semantics: skipped, not contiguous

| measurement | value |
|-------------|-------|
| last pane A token before `%pause` | 55233 |
| first pane A token after `%continue` | 1773247 |
| tokens skipped | 1718014 |
| bytes skipped | 17.18 MB |
| other discontinuities > 50 tokens in the 45 s run | 0 |

Exactly one break in the sequence, and it sits precisely at the pause/continue boundary. Raw bytes:

```
... 000055231 000055232 000055233
%pause %0
%extended-output %1 0 : hb 4 1789146069078\015\012
   (six heartbeats, no pane A output)
%begin 1789146075 288 1
%continue %0
%end 1789146075 288 1
%extended-output %0 0 : 001773247 001773248 001773249 ...
```

The man page says only "tmux will return to sending output to the pane if it was paused". It does
not mention the drop. [fact] The observed behaviour is that on continue tmux advances the client's
pane offset to the pane's current write position and discards the interval. [inference, from the
byte evidence plus the flat producer rate below]

The producer was never throttled by the pause. 1718014 tokens in the 5.753 s pause is 2.99 MB/s,
matching the unthrottled 2.8 MB/s measured in the fast run. [fact] tmux kept draining pane A's pty
at full speed throughout the pause and threw the data away for this client. Pane content and
scrollback therefore stay current; only this viewer's stream has the hole.

## Run B: fast reader, pause-after=2 (control)

Same setup, no sleep in the read loop, 45 s.

| measurement | value |
|-------------|-------|
| `%pause` notifications | 0 |
| pane A payload delivered | 127.3 MB (1307919 output lines) |
| pane A tokens parsed | 12297276, first 1, last 12300932 |
| discontinuities > 50 tokens | 0 |
| pane A age min/max | 0 ms / 14 ms |
| max hb gap | 1.014 s |

No pause, no loss, and age never exceeded 14 ms at 2.8 MB/s sustained. [fact] Confirms the pause is
purely a consequence of the reader falling behind, not of the flood rate.

## Run C: slow reader, NO pause-after (counterfactual)

This is the run that changes the design conclusion, so it is worth stating plainly.

| measurement | Run A (pause-after=2) | Run C (no pause-after) |
|-----------------------|------------------|------------------|
| notification form | `%extended-output` | `%output`, no age field |
| `%pause` seen | yes, 2 | none |
| pane A payload delivered | 1.13 MB | 11.6 MB |
| pane A sequence loss | 17.18 MB at resume | 0 bytes, 1109513 tokens, no gaps |
| implied producer rate | 2.99 MB/s during pause | 0.25 MB/s |
| max hb gap | 2.429 s | 2.656 s |
| hb gap pattern | clean 1.002 s once paused | 2.2 to 2.66 s throughout, arrivals coalesced |
| tmux server RSS max | 17.7 MB | 18.4 MB |

Without `pause-after`, tmux does not buffer without limit and does not drop. It applies backpressure
all the way through to the producing program: the producer's rate collapsed from 2.8 MB/s to
0.25 MB/s, exactly the reader's consumption rate. [fact] awk was blocked on its pty write because
tmux stopped reading the pane.

So the trade is not memory against loss. It is:

```
┌──────────────────────┐   ┌────────────────────────────────┐
│ no pause-after       │──▶│ lossless, but slow viewer      │
│                      │   │ throttles the REAL program     │
│                      │   │ and delays every other pane    │
└──────────────────────┘   └────────────────────────────────┘
┌──────────────────────┐   ┌────────────────────────────────┐
│ pause-after=N        │──▶│ program runs full speed, idle  │
│                      │   │ panes stay at 1.0 s, noisy     │
│                      │   │ pane loses the paused interval │
└──────────────────────┘   └────────────────────────────────┘
```

## Memory bounds

Both processes bounded in all three runs. Server baseline before attach was 4.5 MB.

| run | tmux server RSS min | max | reader RSS min | max |
|-----|--------------------|-----|----------------|-----|
| A, slow, pause-after=2 | 4.55 MB | 17.72 MB | 1.78 MB | 5.04 MB |
| B, fast, pause-after=2 | 4.52 MB | 16.11 MB | 1.72 MB | 5.00 MB |
| C, slow, no pause-after | 4.56 MB | 18.43 MB | 0.74 MB | 5.21 MB |

Server RSS reached its ceiling within the first 5 s and then stayed flat for the remaining 40 s in
every run, including Run C where the client was 11x behind the producer. [fact] Sampled once per
second from `/proc/<pid>/status` VmRSS.

Caveat worth carrying into R2: this measurement deliberately used `history-limit 2000`. The box's
own `~/.tmux.conf` sets `history-limit 1000000`, which at 200 bytes per line is up to 200 MB of
scrollback per pane inside the tmux server, independent of anything control mode does. [fact]
Scrollback, not control-mode buffering, is the dominant server memory term in a real deployment.

## Decision input for R2

**1. Is per-pane pause usable as the per-viewer flow-control primitive?** Yes. Pause is per-pane and
genuinely isolated: the paused pane stops dead while every other pane keeps delivering at full
cadence with sub-60 ms buffering age. `pause-after` is a per-client flag, so each viewer's control
client gets its own threshold and its own pause decisions, which is exactly the per-viewer shape R2
needs.

**2. What must the daemon do on `%pause`?** Re-snapshot, unconditionally. The resumed stream is not
contiguous, and tmux gives no indication of how much it dropped. The sequence is:

- on `%pause %<id>`, mark that pane's incremental stream as broken for that viewer
- when ready, send `refresh-client -A"%<id>:continue"` (no space after `-A`)
- on `%continue %<id>`, discard any partial line still buffered for that pane, run
  `capture-pane -p -e -t %<id>` (or equivalent) to get authoritative state, push that to the
  viewer as a full repaint, and only then resume applying `%extended-output` deltas

Treating post-continue bytes as a continuation of the pre-pause stream corrupts the viewer's screen
model. Any parser holding a partial escape sequence or partial UTF-8 across the pause must drop it.

**3. Pick `pause` over `off`.** Both stop delivery, but per the man page `off` also makes tmux stop
reading the pane once no client wants it, which is the Run C failure mode: a slow or backgrounded
viewer throttles the user's actual program. `pause` leaves the program running at full speed. Use
`off` only for a pane genuinely nobody is watching.

**4. Set the threshold deliberately.** `pause-after` is in whole seconds, minimum 1. At 2 s the
flood tripped it in 2.4 s. There is no sub-second option, so a viewer can be up to a second behind
before flow control engages. If R2 wants tighter latency on idle panes it must come from a smaller
read granularity in the daemon, not from this knob.

**5. Parse `%error` blocks.** Not optional. The `-A %pane` syntax trap turns a typo into a pane that
looks alive and silently never resumes.

**6. Use `refresh-client -B` for pane metadata.** Subscriptions deliver `%subscription-changed` at
most once a second and are independent of pane output, so title, size and activity can be tracked
without parsing the output stream. Not exercised in this spike. [fact, from the man page]

## Surprises

- Bare `%0` as an argument is a parse error; the pane id must be glued to `-A` or quoted. Cost the
  first run its resume. [fact]
- Resume drops the paused interval with no notification and no mention in the man page. [fact]
- `pause-after` switches the whole client from `%output` to `%extended-output`. A parser that only
  handles `%output` goes blind the moment the flag is set. [fact]
- Without `pause-after`, tmux's backpressure reaches the producing program and slows it to the
  viewer's speed. The real risk of no flow control is a throttled program, not a memory leak. [fact]
- The idle pane's latency improves when the noisy pane is paused, from a 2.43 s worst gap to a clean
  1.002 s cadence. Flow control on one pane is a latency fix for the others. [fact]

## Artifacts

Under `/tmp/claude-1000/-home-claude--ref-agents-in-a-box-desktop-app-part2/7ec9b5c8-5403-4a49-b0ee-f6b71be9354a/scratchpad/spike7/`:
`reader.py`, `reader_nopause.py`, `flood.sh`, `hb.sh`, `analyze.py`, `run-slow3.json` (+ `.raw`,
1.2 MB, the byte evidence for the resume gap), `run-fast.json`, `run-nopause.json`, and the matching
`.log` event streams. The 160 MB and 147 MB raw captures from the fast runs were deleted after
analysis.
