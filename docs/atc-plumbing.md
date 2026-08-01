---
title: "ATC plumbing — event-driven orchestration"
---

# ATC plumbing — event-driven orchestration

The ATC plumbing upgrades **Air Traffic Control** from *poll-mode* (act on the
next OS-timer heartbeat) to *event-driven* (act the instant a child session
finishes). It is shared session-lifecycle infrastructure in `ainb-core`
(`fleet::plumbing`) + the `ainb-hooks` shell shim — **not** inside `/ainb-fleet`;
ATC merely consumes it. The phone bridge and ainb itself can consume it too.

The mechanism mirrors agent-deck's chain:

```
lifecycle hooks ──▶ atomic status files
                └──▶ durable per-parent inbox (last-wins · fsync · exactly-once)
                        └──▶ synchronous Stop-hook drain ──▶ {"decision":"block"}
```

When a child's turn ends, its `Stop` hook commits a completion to its **parent's**
durable inbox. The parent's own `Stop` hook drains that inbox and returns
`{"decision":"block","reason":<completions>}`, which Claude Code feeds back as the
parent's next turn — so the parent (ATC) reacts immediately instead of waiting for
its heartbeat. Empty inbox ⇒ no block, no writes (every leaf session pays nothing).

---

## On-disk layout (under `$AINB_HOME`, default `~/.agents-in-a-box`)

```
status/<session_id>.json     per-session lifecycle status (atomic, one per event)
inbox/<parent_id>.jsonl      durable per-parent completion inbox
inbox/<parent_id>.consumed   exactly-once consumed-fingerprint marker (capped 1000)
inbox/<parent_id>.budget     consecutive Stop-drain block budget
inbox/<parent_id>.lock       fs2 advisory lock guarding commit/drain
inbox/dead-letter.jsonl      completions whose parent is unresolvable (audit trail)
parents.json                 durable child→parent map ({child_id: parent_id})
```

All writes go through a crash-safe atomic helper: temp-in-same-dir → `fsync` →
`rename` → `fsync(dir)`. A reader never sees a torn file, and once a write returns
the bytes survive a power loss.

### Status file format

```json
{
  "status": "done",
  "session_id": "abc-123",
  "event": "Stop",
  "ts": 1700000000000,
  "done_summary": "merged the PR"
}
```

| field | meaning |
|---|---|
| `status` | derived lifecycle state: `waiting` · `running` · `done` · `dead` |
| `session_id` | the session this record describes |
| `event` | the raw hook event that produced it (provenance) |
| `ts` | epoch milliseconds |
| `done_summary` | one-line completion summary, present only on a `Stop` with output |

Event → status mapping (`SessionStatus::for_event`):

| hook event | status |
|---|---|
| `SessionStart` | `waiting` |
| `UserPromptSubmit` | `running` (+ resets the session's block budget) |
| `Stop` | `waiting`, or `done` when it carries a `done_summary` |
| `Notification` | `waiting` |
| `SessionEnd` | `dead` |

### Inbox record format

```json
{
  "child_id": "worker-7",
  "parent_id": "tower",
  "turn_fingerprint": "<blake3 hex>",
  "summary": "merged the PR",
  "event": "Stop",
  "ts": 1700000000000
}
```

- **last-wins-per-child**: a child that finishes several turns before the parent
  drains leaves exactly one record (its latest), keyed by `child_id`.
- **exactly-once**: `turn_fingerprint = blake3(child_id ∥ summary ∥ ts)`. `drain`
  records delivered fingerprints in `.consumed` *before* clearing the JSONL, so a
  crash never double-delivers, and a completion written while the consumer was
  offline replays exactly once on the consumer's next drain.
- **dead-letter**: a completion whose parent cannot be resolved is appended to
  `inbox/dead-letter.jsonl` rather than silently dropped.

This durable inbox **replaces reliance on the claude-peers broker's lossy
fire-and-forget path** (the broker has a known silent-delivery gap). The broker is
not patched.

---

## Hook event set

The full lifecycle set is installed into Claude Code's `~/.claude/settings.json`
by `ainb fleet atc setup` (read-preserve-modify-write — see below). Each managed
entry runs the shared `notify.sh` with `AINB_HOOK_EVENT=<event> AINB_MANAGED=atc`,
which forwards to `ainb fleet atc hook`:

| event | side-effects |
|---|---|
| `SessionStart` | write `waiting` status |
| `UserPromptSubmit` | write `running` status; reset the block budget (genuine user turn) |
| `Stop` | write status; commit completion to the parent inbox (or dead-letter); drain own inbox → maybe `{"decision":"block"}` |
| `Notification` | write `waiting` status |
| `SessionEnd` | write `dead` status |

### Idempotent hook merge

The merge into `settings.json` is **read-preserve-modify-write**: every ATC entry
is tagged `"_ainb_atc_managed": true`, so a re-install replaces exactly the prior
ATC entries and re-running is byte-idempotent. Crucially it **preserves all other
hooks** on the same events — the reflect plugin's `Stop`/`PreCompact`/`SessionStart`/
`UserPromptSubmit`/`PostToolUse` hooks and the ainb-hooks/notifyd `Notification`/
`Stop` hooks all survive untouched. Uninstall (on the last ATC teardown) strips
exactly the ATC block back out.

### The Stop-drain

On a parent's synchronous `Stop` hook:

- **empty inbox** → no block, no writes beyond the status file. Leaf sessions
  (no children ⇒ empty inbox) pay nothing on every Stop.
- **non-empty inbox** → drain exactly-once, then if the consecutive-block budget
  (default 3) allows, print `{"decision":"block","reason":<formatted completions>}`
  so the completions become the session's next turn. The budget caps consecutive
  self-blocks to avoid wedging a session in a loop; a genuine user turn
  (`UserPromptSubmit`) resets it.

---

## Parent linkage

A child records its parent — the inbox routing key — two ways, resolved in this
order by the Stop hook:

1. `AINB_PARENT_SESSION` env var, seeded into the child's tmux session by
   `ainb run --parent <id>` (`tmux new-session -e`). Live, zero-lookup.
2. The durable `parents.json` map, also written by `ainb run --parent`. The
   restart-safe fallback.

```bash
# Spawn a child linked to the ATC instance "tower":
ainb run --repo . --worktree --parent tower -p "fix the failing tests"
```

---

## How ATC consumes it

`ainb fleet atc setup <name>` installs the lifecycle hooks (skip with
`--no-hooks`). On each heartbeat, `ainb fleet atc heartbeat <name>` **drains the
ATC session's own inbox first**: any child completions are prepended to the
`[HEARTBEAT]` nudge so ATC handles freshly-finished children before the polled
roster. A pending completion overrides idle-pause (a finished child wakes ATC even
during a quiet window). The poll-mode `fleet needs` path remains the always-on
fallback, so ATC behaves identically whether or not any child has the hooks
installed — the plumbing is a pure drop-in enhancement.

### Operator / debug verbs

```bash
ainb fleet atc inbox peek   <parent>                     # show undrained (no consume)
ainb fleet atc inbox drain  <parent>                     # drain exactly-once + print decision
ainb fleet atc inbox commit <parent> --child <id> --summary "<text>"   # commit (testing)
ainb fleet atc hook --event <E> --session-id <id>        # internal (called by notify.sh)
```

---

## Source map

| file | responsibility |
|---|---|
| `crates/ainb-core/src/fleet/plumbing/atomic.rs` | crash-safe atomic write + fsync |
| `crates/ainb-core/src/fleet/plumbing/paths.rs` | `status/` + `inbox/` layout (honours `AINB_HOME`) |
| `crates/ainb-core/src/fleet/plumbing/status.rs` | status files + event→status mapping |
| `crates/ainb-core/src/fleet/plumbing/inbox.rs` | durable inbox: last-wins · exactly-once · dead-letter |
| `crates/ainb-core/src/fleet/plumbing/drain.rs` | Stop-drain decision + block budget |
| `crates/ainb-core/src/fleet/plumbing/hooks.rs` | pure settings.json merge (preserve-modify-write) |
| `crates/ainb-core/src/fleet/plumbing/settings.rs` | disk install/uninstall of the hooks |
| `crates/ainb-core/src/fleet/plumbing/parent.rs` | child→parent linkage |
| `crates/ainb-core/src/cli/fleet/atc.rs` | `hook` + `inbox` verbs; heartbeat inbox drain |
| `plugins/ainb-hooks/hooks/notify.sh` | shell shim: notifyd delivery + ATC plumbing forward |
```
