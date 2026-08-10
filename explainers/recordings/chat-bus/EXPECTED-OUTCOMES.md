# Chat bus recordings, expected outcomes (validation contract)

Every recording here is a FULL, uncut run of one live smoke journey from
`ainb-tui/scripts/chat-bus-smoke.sh`: the command, the scratch world it builds,
the assertions as they happen, and the final `SMOKE-RESULT` line. Nothing is
staged and nothing is edited. Each run stands up its own hangar home, a real
`ainb-hangar-daemon`, the real `ainb` binary, three real tmux sessions on a
private tmux server, and the ACP fixture adapter (a real adapter is preferred
automatically when one is installed; the banner states which ran).

`j1` through `j5e` drive the DAEMON and the CLI. `j6` and `j7` drive the
OPERATING SURFACE: each additionally cold-launches the real `ainb tui` on that
private tmux server and operates it with the keys a user would press. `j6`
opens the Fleet panel with `f` and reads the roster card; `j7` goes one screen
further, opening the copilot CHAT with `m`, sending a message from the composer
and answering a confirm card from the pane. That distinction is the whole point
of both — part 1 shipped a chat bus the TUI consumes without anything ever
opening the TUI, so a panel-side regression could not be caught by any
recording above them.

A recording is valid only when its frames contain the **expected text** below.
A tape that renders a blank pane, or that ends without a `SMOKE-RESULT` verdict
line, is a failed deliverable regardless of whether the file exists.

Every journey here is expected to PASS. `j6` only passes against a build
containing `193c3e3c`; against anything older it fails on the provider label,
which is the bug it was written to catch (see below).

Validation method (the one used to sign off the table):

1. `ffmpeg -i <tape>.mp4 -vf fps=1/10 /tmp/frames/f-%03d.png`
2. Read the late frames (the journey prints its verdict at the end).
3. Assert the expected text is present verbatim.

`j1`–`j5e` recorded 2026-08-07 against `origin/main` at `f49488c9` (all six
phases merged), adapter mode `fixture`, turn deadline compressed to 45000 ms via
`AINB_ACP_TURN_DEADLINE_MS` so the 30 minute production default is observable.

`j6` recorded 2026-08-08 against `ainb 1.18.0 (bd17c8d1)`, which is the first
build containing `193c3e3c` — the `provider_label` fix j6 was written to catch.
Its row below was read out of extracted frames, same method. Note that
`193c3e3c` is NOT on `f/part2-chat`, so j6 FAILS on that branch, on the exact
`UNKNOWN` provider label it was written to catch. That is the branch being
behind main, not a new regression, and it is not a j7 failure.

`j7` recorded 2026-08-08 against `ainb 1.18.0 (50d2f5c7)` on `f/part2-chat`,
adapter mode `fixture`. Its row below was read out of extracted frames, same
method.

The j6 and j7 videos are SHORT (~7 s for a multi-minute journey) and that is not
a truncation: vhs captures on screen CHANGE, and both spend most of their run
polling silently with a static screen (waiting for the daemon's reconciler, for
the TUI to paint, for the roster, for a turn). Every screen state the journey
produced is present, in order — the final frame carries the whole transcript
through `SMOKE-RESULT overall PASS`. Do not try to "fix" the duration with
`Set Framerate`; see `j6.tape`.

| Tape | Journey | Expected text, verified in-frame |
|---|---|---|
| `j1.gif` / `.mp4` | chat bus on tmux | `RECEIVED:[j1 ...] hello fleet, deliver me verbatim` and `3/3 DELIVERED · 3/3 panes verbatim · follower saw it` then `✓ j1 PASS` |
| `j2.gif` / `.mp4` | ACP leg | `7 transcript chunks · 1 timeline reply · first chunk line 3 < turn end line 8` then `✓ j2 PASS` |
| `j3.gif` / `.mp4` | resume across a daemon SIGKILL | `resumed via [loaded] on the same session_key · 2 agent replies · 0 ghost attention rows` then `✓ j3 PASS` |
| `j4.gif` / `.mp4` | convergence, adapter only | `converged UNKNOWN/adapter_exit; ... · same daemon pid ... · next message DELIVERED` then `✓ j4 PASS` |
| `j5a.gif` / `.mp4` | fault: queue overflow | `queue accepted 32 prompts, then REJECTED/queue_full` then `✓ j5a PASS` |
| `j5b.gif` / `.mp4` | fault: turn deadline | `converged UNKNOWN/turn_deadline after the deadline · scope reusable` then `✓ j5b PASS` |
| `j5c.gif` / `.mp4` | fault: idempotency replay | `one delivery for two identical sends · exit 5 on the conflicting third` then `✓ j5c PASS` |
| `j5d.gif` / `.mp4` | fault: permission round trip | `attention row → fleet/action approve → DELIVERED, row closed` then `✓ j5d PASS` |
| `j5e.gif` / `.mp4` | fault: unknown target | `DELIVERED + REJECTED/target_unknown in one request, message persisted` then `✓ j5e PASS` |
| `j6.gif` / `.mp4` | the operating surface: an ACP session in the real Fleet panel | `pressing \`f\` ONCE, the way a user opens Fleet`, then `card provider row, as the operator sees it:│ branch unknown · ACP · REMOTE`, then `Fleet opened on the first \`f\` ·🛫 Fleet · 4 sessions · 0 need attention · Hangar· card [j6-project] labelled [ACP]` and `✓ j6 PASS` |
| `j7.gif` / `.mp4` | the operating surface: the copilot chat, operated | `pressing \`m\` ONCE, the way a user opens the copilot chat`, then `chat header, as the operator sees it: Fleet chat · #copilot · channel:01KZH51MFJKSX7CN5HNDANHBQA`, then `operator row, as the operator sees it: YOU │ what is blocked right now` and `copilot row, as the operator sees it: COPILOT │ session one is waiting on an approval`, then `confirm card, as the operator sees it: [OPEN] kill {"session":"legacy:claude:ainb-smoke-18… y approve n deny e answer`, then `chat opened on one \`f\` + one \`m\` · scope [channel:01KZH51MFJKSX7CN5HNDANHBQA] on screen · YOU and COPILOT on distinct rows · card [j7-card-1831130-1786208373] answered [approved]` and `✓ j7 PASS` |

## What j7 exists to catch

j7 asserts four things the operator can only see by opening the chat, each
anchored on the row it means rather than on a substring anywhere in the pane:

1. **The channel scope is the DAEMON's.** The header is compared against the
   scope read back from `fleet/channel_list`, anchored at the start of its own
   row and bounded by a space. The first version of this screen hardcoded
   `channel:copilot` and read an empty timeline forever while every one of its
   unit tests stayed green, so a reader that took its expected value off the
   pane would have agreed with the bug.
2. **The operator's own message reaches the conversation**, typed in the
   composer and submitted with one `Enter`.
3. **Two authors are DISTINGUISHABLE**, checked BOTH ways: neither body may
   appear under the other's label. The wire carries the author precisely so a
   copilot write cannot wear a human's name, and that guarantee dies at the last
   inch if the panel paints both rows the same. The copilot line is minted by
   the daemon from `fleet/message_send`'s `actor`, the same field
   `copilot::post_channel_message` sets.
4. **An open confirm card is answerable from the pane**: `Tab` then `y`, with
   the store's own `approved` row as the receipt and `CONFIRM CARDS · none open`
   as the operator-visible one.

Every key is pressed ONCE, after waiting for the screen that receives it. A
retry loop is how a modal swallowing the first press goes unnoticed.

The one thing j7 stages is the confirm card itself: a card is minted by
`ainb_hangar_daemon::copilot::gate`, called from the copilot's tool bridge, and
that bridge is not built yet — nothing on the wire, in the CLI or in the TUI can
open one. The row is a precondition; everything asserted after it is real
product behaviour on the real wire (`fleet/confirm_list` decoding it,
`fleet/confirm_answer` resolving it). See `db_write` in the smoke script.

## What j6 exists to catch

j6 asserts, anchored on the ACP session's own card in the real Fleet panel, that
the panel names the session's provider. The passing frame reads:

```
│ ▶ j6-project
│ branch unknown  ·  ACP  ·  REMOTE
```

It was written against a build where that cell read `UNKNOWN`, and it failed
there — the bug it caught. `ainb-tui/crates/ainb-plugin-hangar/src/screen/fleet.rs`
maps a provider TWICE: `FleetSessionRow::from` turns `FleetProvider::Acp` into
the wire token `acp`, and `provider_label` turns that token into the label the
operator reads. `provider_label` knew only `claude` and `codex`, so `acp` and
`copilot` both fell through to `UNKNOWN` while every daemon-level and CLI-level
test stayed green. Fixed in `193c3e3c` with an exhaustive test that fails to
compile when a new `FleetProvider` variant skips the display mapping.

Why the assertion is anchored rather than a plain search: `unknown` appears in
every card's `branch unknown` fallback, and `acp` appears in the session key,
the scratch paths, and any cwd named after ACP. A journey that grepped the whole
pane for `acp` would pass on an incidental substring while the operator-visible
label was still wrong, which is exactly the failure mode j6 exists to close. j6
reads the provider cell out of the row under its own card and compares it
exactly, checking `unknown` FIRST so a regression names its own cause.

## Why there is no whole-suite tape

There deliberately is not one. Three attempts to capture the ~15 minute
end-to-end run in this environment died mid-capture (vhs drives a headless
browser; the longest successful capture here truncated at 453 seconds), and a
truncated tape that stops after four journeys is worse than none: it looks like
evidence and is not. The per-journey tapes ARE the uncut end-to-end evidence,
one full run each, and the suite is reproducible in one command:

```bash
./ainb-tui/scripts/chat-bus-smoke.sh    # eleven journeys, one SMOKE-RESULT line each
```

If a whole-suite tape is wanted later, record it somewhere a 15 minute capture
is reliable and terminate on `Wait+Screen@2400s /SMOKE-RESULT overall/`, never a
fixed `Sleep`.

## Fail signatures

Any of these in the final frames means the recording is invalid, whatever the
file size says:

- an empty pane, or a shell prompt with no journey banner
- `SMOKE-RESULT ... FAIL`, or a `SKIP` on any journey other than a deliberate
  capability probe (j3 legitimately SKIPs against a daemon built before the
  Phase 6 resume routine; against current main it must PASS)
- `SMOKE-RESULT setup FAIL world did not come up`, usually preceded by
  `missing build artifact: .../ainb`. This is not a journey failure at all: the
  binaries the tape was pointed at are gone, and nothing under test ever ran
- a journey banner with no verdict line, which means the tape's `Sleep` ended
  before the journey did

## Reproducing

```bash
cd <repo>
./ainb-tui/scripts/chat-bus-smoke.sh          # every journey
./ainb-tui/scripts/chat-bus-smoke.sh j5b      # one journey
./ainb-tui/scripts/chat-bus-smoke.sh j6       # the real TUI, the operating surface
./ainb-tui/scripts/chat-bus-smoke.sh j7       # the real TUI's copilot chat screen

# Recording. Run from the REPO ROOT: the tapes' Output paths are repo relative.
# OPENSSL_NO_VENDOR=1 is required or openssl-sys fails to find its headers.
cd ainb-tui && OPENSSL_NO_VENDOR=1 cargo build -j2 -p ainb -p ainb-hangar-daemon -p ainb-acp --bins && cd ..
VHS_NO_SANDBOX=true AINB_SMOKE_SKIP_BUILD=1 \
  AINB_SMOKE_BIN_SOURCE="$PWD/ainb-tui/target/debug" \
  vhs explainers/recordings/chat-bus/j7.tape
```

Recording gotchas, all of them learned by losing a tape to each:

- Do NOT set `Set Framerate` low to "slow down" a recording. It does the
  opposite: `Set Framerate 10` captured 196 frames of a three minute j6 run and
  produced a 7 second blur. vhs captures on screen change; leave it alone.
- Terminate on `Wait+Screen@<n>s /SMOKE-RESULT .../`, never a fixed `Sleep`.
  A `Sleep` that ends before the journey does yields a banner with no verdict,
  which looks like evidence and is not. Every tape here now does this.
- Keep the pattern out of the TYPED command. `Wait+Screen` matches the whole
  screen, and the shell echoes the command as it is typed, so a tape that types
  its own sentinel matches instantly and records three seconds of nothing.
- Check `ls -l` on every artifact afterwards. The gif encoder has been seen to
  never finish, leaving a ZERO-BYTE gif next to a perfectly good mp4. The write
  ORDER is not fixed (j6 got the mp4 first, j7 the gif), so check both rather
  than assuming which one is at risk. Regenerate an empty gif from the mp4:

  ```bash
  ffmpeg -i j6.mp4 -vf "fps=10,scale=800:-1:flags=lanczos,palettegen" -y /tmp/pal.png
  ffmpeg -i j6.mp4 -i /tmp/pal.png -lavfi "fps=10,scale=800:-1:flags=lanczos[x];[x][1:v]paletteuse" -y j6.gif
  ```
- `/explainers/` is in `.gitignore`. Everything here is tracked only because it
  was force-added; a new artifact needs `git add -f` or it is silently skipped.

`VHS_NO_SANDBOX=true` is required wherever Chromium cannot use its SUID sandbox
(containers, most CI images); without it vhs exits with `could not launch
browser` and leaves any previous artifact in place, which is how a stale tape
gets mistaken for a fresh one.
