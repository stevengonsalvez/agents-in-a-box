# Chat bus recordings, expected outcomes (validation contract)

Every recording here is a FULL, uncut run of one live smoke journey from
`ainb-tui/scripts/chat-bus-smoke.sh`: the command, the scratch world it builds,
the assertions as they happen, and the final `SMOKE-RESULT` line. Nothing is
staged and nothing is edited. Each run stands up its own hangar home, a real
`ainb-hangar-daemon`, the real `ainb` binary, three real tmux sessions on a
private tmux server, and the ACP fixture adapter (a real adapter is preferred
automatically when one is installed; the banner states which ran).

A recording passes only when its frames contain the **expected text** below. A
tape that renders a blank pane, or that ends without its `SMOKE-RESULT ... PASS`
line, is a failed deliverable regardless of whether the file exists.

Validation method (the one used to sign off the table):

1. `ffmpeg -i <tape>.mp4 -vf fps=1/10 /tmp/frames/f-%03d.png`
2. Read the late frames (the journey prints its verdict at the end).
3. Assert the expected text is present verbatim.

Recorded 2026-08-07 against `origin/main` at `f49488c9` (all six phases merged),
adapter mode `fixture`, turn deadline compressed to 45000 ms via
`AINB_ACP_TURN_DEADLINE_MS` so the 30 minute production default is observable.

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
| `full-suite.gif` / `.mp4` | every journey, one run | nine `SMOKE-RESULT j... PASS` lines followed by `SMOKE-RESULT overall PASS` |

## Fail signatures

Any of these in the final frames means the recording is invalid, whatever the
file size says:

- an empty pane, or a shell prompt with no journey banner
- `SMOKE-RESULT ... FAIL`, or a `SKIP` on any journey other than a deliberate
  capability probe (j3 legitimately SKIPs against a daemon built before the
  Phase 6 resume routine; against current main it must PASS)
- a journey banner with no verdict line, which means the tape's `Sleep` ended
  before the journey did

## Reproducing

```bash
cd <repo>
./ainb-tui/scripts/chat-bus-smoke.sh          # every journey
./ainb-tui/scripts/chat-bus-smoke.sh j5b      # one journey
VHS_NO_SANDBOX=true vhs explainers/recordings/chat-bus/j5b.tape
```

`VHS_NO_SANDBOX=true` is required wherever Chromium cannot use its SUID sandbox
(containers, most CI images); without it vhs exits with `could not launch
browser` and leaves any previous artifact in place, which is how a stale tape
gets mistaken for a fresh one.
