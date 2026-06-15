---
name: ainb-fleet:atc
description: |
  ATC (Air Traffic Control) — the persistent orchestrating brain of the fleet.
  A plain `ainb` Claude session running a generated CLAUDE.md policy, woken on
  an OS-timer heartbeat (default 15 min) built from the LLM-free `fleet needs`
  read. It auto-clears the safe/blocked sessions via the fleet verbs (confident
  ASK → broadcast; ERR → continue within a retry cap) and escalates the
  uncertain ones to the phone bridge. Poll-mode: no hook/inbox plumbing needed.
  Supersedes the `daemon` skill for managed fleets. Use to provision / inspect /
  tear down an ATC instance.
version: "0.1.0"
user-invocable: true
triggers:
  - ainb-fleet:atc
  - fleet atc
  - air traffic control
  - fleet brain
  - conductor
  - watch the fleet
  - auto-clear and escalate
allowed-tools:
  - Bash
---

# ainb fleet:atc — Air Traffic Control

ATC is the orchestrating brain that ties the fleet verbs together on a schedule
with a policy. It is a real `ainb run` Claude session reading a generated
`CLAUDE.md`, woken every N minutes by an OS timer. On each heartbeat it pulls
the **LLM-free** `ainb fleet needs --format json` read, auto-clears the safe
cases itself, and pages you (via the phone bridge) only for the calls that
genuinely need a human.

```
OS timer ──[HEARTBEAT]──▶ ATC session ──reads──▶ ainb fleet needs --format json
                              │                         (LLM-free scan)
                              ├─ auto-clear ─▶ ainb fleet broadcast (answer / continue)
                              └─ escalate ───▶ phone bridge ─▶ your phone
```

## Provision

```bash
# Default: 15-min heartbeat, 60-min idle-pause, spawns the session + installs
# the OS timer (launchd on macOS, systemd --user timer on Linux).
ainb fleet atc setup tower

# Tune the cadence.
ainb fleet atc setup tower --interval 10 --idle-pause 30

# Provision files only (no OS timer, no session) — useful for inspection / CI.
ainb fleet atc setup tower --no-spawn --no-heartbeat
```

`setup` is **idempotent**: re-running re-renders `CLAUDE.md` + `meta.json` and
re-installs the timer, but never clobbers the accumulated `state.json` /
`task-log.md`. It writes to `~/.agents-in-a-box/atc/<name>/`:

| file | role |
|---|---|
| `CLAUDE.md` | the rendered policy ATC reads (verbs, playbooks, safety, memory) |
| `meta.json` | `{name, heartbeat_enabled, heartbeat_interval_min, idle_pause_min}` |
| `state.json` | ATC's durable machine state (retry counts, escalations) — survives compaction |
| `task-log.md` | ATC's human-readable running log |

## Inspect

```bash
ainb fleet atc status tower            # one instance: meta + timer + session liveness
ainb fleet atc list                    # all provisioned instances
ainb --format json fleet atc status tower   # JSON for tooling
```

## Tear down

```bash
ainb fleet atc teardown tower          # remove the timer + kill the session (keeps state/log)
ainb fleet atc teardown tower --purge  # also delete the instance dir
```

Teardown is idempotent and safe when nothing is installed — it removes the OS
timer cleanly, so there is no stale launchd/systemd unit left behind.

## The heartbeat (internal)

```bash
# This is what the OS timer runs every N minutes — you do not call it by hand.
ainb fleet atc heartbeat tower
```

It builds the `[HEARTBEAT ...]` nudge from `ainb fleet needs --no-enrich
--format json` (cheap, 0-token) and tmux-sends it into the ATC session, so ATC
spends tokens only **deciding**, not scanning. When the fleet is quiet it sends
a one-line idle ping; when the session is gone it no-ops harmlessly until
teardown.

## The policy (what ATC decides)

The generated `CLAUDE.md` encodes a conservative, **escalate-on-uncertainty**
policy. Per signal:

| signal | ATC action |
|---|---|
| **ASK** (blocked on a question) | answer via `broadcast` **only if confident**; ambiguous / opinionated / irreversible → **ESCALATE** |
| **ERR** (API error) | send `continue` via `broadcast`, capped at **3 auto-continues per session**; cap reached + still erroring → **ESCALATE** |
| **IDLE** (turn finished, silent) | leave it (work likely done); only nudge if mid-orchestration |
| **WAIT** (`WAITING:` marker) | unblock via `broadcast` if it is waiting on another session; waiting on you → **ESCALATE** |

ATC **never** auto-runs destructive actions (delete / force-push / merge /
deploy / spend / touch secrets) — those always escalate.

### Escalation

ATC's voice to you is the phone bridge (`plugins/ainb-fleet/bridge/`). It raises
a call by emitting a self-contained line the bridge surfaces:

```
NEED: <session> — <decision required> — <options>
```

## Supersedes `ainb fleet daemon`

For any fleet ATC manages, it **replaces** the standalone `daemon` skill: the
daemon's blind 5-second regex auto-`continue` becomes ATC's ERR playbook, now
**with the retry cap the daemon never had**. Do not run `ainb fleet daemon`
against sessions ATC manages — they would race. The `daemon` skill remains in
place for unmanaged one-off recovery, marked superseded.

## Safety model + limitations

- **Safety is prompt-enforced.** ATC's conservatism lives in the `CLAUDE.md`
  policy, not in a hard sandbox. The default is deliberately to escalate on any
  uncertainty; a wrong autonomous action is costlier than an extra page.
- **Poll-mode latency.** ATC reacts at its next heartbeat, not the instant a
  session blocks. The separate plumbing track (hooks → status files → durable
  inboxes → Stop-hook drain) upgrades ATC to event-driven; it is a drop-in
  enhancement, not a prerequisite — ATC works standalone today.
- **tmux fire-and-forget.** Like the rest of the fleet, sends have no ACK; ATC
  confirms effect on the next heartbeat read.

## Caveats

- The heartbeat timer and the ATC session are independent: tearing down only the
  session leaves the timer firing into a dead pane (it no-ops). Use `teardown`
  to remove both.
- `--no-heartbeat` provisions without a timer — you can drive heartbeats
  manually (`ainb fleet atc heartbeat <name>`) for testing.
- One ATC instance manages the whole host fleet; run multiple named instances
  only if you deliberately want to partition (they will each see all sessions).
