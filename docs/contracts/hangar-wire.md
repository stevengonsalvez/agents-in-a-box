---
title: "Hangar wire contract: protocol version, capabilities, mutations"
---

# Hangar wire contract

What every client of the Hangar daemon can rely on, and what it has to send.
Implements decisions D17 (versioning) and D18 (mutations) of
`docs/plans/2026-09-11-multi-surface-decisions-spec.md`, with critique
amendments 15-21.

## One integer, one catalogue

```
┌────────┐  auth/hello { token, surface?, protocol{min,max}, capabilities[], device? }  ┌────────┐
│ client │ ─────────────────────────────────────────────────────────────────────────▶ │ daemon │
│        │ ◀───────── { protocol{min,max}, selected, capabilities[], daemon_version } ─│        │
└────────┘                       no overlap ──▶ error -32007                            └────────┘
```

| Thing | Answers | Lives in |
| --- | --- | --- |
| `PROTOCOL_VERSION` (integer, currently **1**) | "can these two builds talk?" | `ainb-hangar-proto/src/protocol.rs` |
| Capability strings | "does that build serve the method I am about to call?" | `ainb-hangar-proto/capabilities.catalogue` |
| `FLEET_PROTOCOL_VERSION` (**frozen at 2, forever**) | nothing new; `fleet/negotiate` stays a compatibility echo | `ainb-hangar-proto/src/fleet.rs` |

**Bump rule.** The integer moves only when a peer that does not know about the
change would misread the wire: a method or field is removed, a field's meaning
changes, or framing / auth / crypto changes. A new method, a new optional field
and a new event kind are capability strings, not bumps. That is what lets an
app-store phone, which cannot be force-upgraded, keep working.

**The catalogue is a committed file.** `capabilities.catalogue` is append-only;
`ainb-hangar-proto/tests/capability_catalogue.rs` fails if a string is removed,
reordered, or advertised without being committed. Adding one is two lines (the
const, then the file) in one change. Removing one is a `PROTOCOL_VERSION` bump.

**Backwards compatibility, both directions.** Every hello member except `token`
defaults, and every reply member defaults. A pre-W0-wire client sending
`{ token }` negotiates version 1; a pre-W0-wire daemon answering `{}` reads as
"protocol 1, declares nothing". Both legs are asserted by the skew harness.

### Sockets

The daemon binds `hangar.sock` and symlinks `hangar-v<N>.sock` for every
version it serves. Both names are ONE inode, one listener, one flock singleton.
A client dials the versioned path when it knows one, else the plain path; that
is why a desktop sidecar daemon and a `brew`-installed one cannot end up with
two sockets.

### Skew harness

`ainb-tui/crates/ainb-hangar-daemon/tests/skew_harness.rs` runs
{daemon N, daemon N-1} x {hangar-client, ainb-web, Swift fixture} in both
directions, plus the local leg. "Daemon N-1" is a set of committed frames in
`tests/fixtures/skew_frames.json`, not an old binary: the frames are the
contract, so a change shows up as a diff rather than as a green test against a
moved goalpost. The Swift app's half of the same frames runs on the macOS lane
(`CanonicalFixtureTests`), and a drift gate compares the two copies as text.

Runs in CI in the **Contracts** job, which has a no-rerun-to-green policy.

## Mutations: op ids, fences, receipts

Every mutating method embeds a flattened `MutationEnvelope`:

```json
{ "...method fields...": "...", "op_id": "9f2c…", "fence": { "kind": "attention_version", "version": 7 } }
```

Both members are optional. A client that sends neither is served exactly as it
was before W0-wire.

| Field | Meaning |
| --- | --- |
| `op_id` | A client-minted **opaque** 128-bit value, conventionally 32 lowercase hex. The daemon never parses one, so no freshness rule exists and a phone with a wrong clock is never rejected for skew. |
| `fence` | The state the client believed it was acting on. |

The fleet family renames nothing: `FleetActionParams.request_id`,
`FleetMessageSendParams.request_id`, `FleetStartParams.request_id` and
`FleetBroadcastParams.idempotency_key` ARE the op id for those methods, and
`op_id` is accepted as an alias.

### Fences

| Mutation | Fence | Enforced | Stale means |
| --- | --- | --- | --- |
| `attention/answer` | attention row `version`, and `state = open` | **yes** | already answered: `rejected{already_answered_by}` |
| `fleet/message_send`, prompt send | `fleet_session.lifecycle_updated_at` | not yet | the agent moved on: `rejected{turn_advanced}` |
| `fleet/action{cancel, kill}` | `session_incarnation` | not yet | a different process owns the name: `rejected{incarnation_mismatch}` |
| `device_revoke` (R1) | registry `version` | not yet (R1) | concurrent admin edit: `rejected{conflict}` |

Only the enforced row is declared in `MUTATING_METHODS`; the other two carry
`FenceKind::None` until the handler that reads them lands, because a client that
believes it holds a guard it does not have is worse off than one that knows it
has none. `only_enforced_fences_are_declared` in `ainb-hangar-proto` pins that.

The fence VALUE reaches a client on the wire `AttentionRow.version`, gated by
the `hangar.attention.fence` capability. A daemon that does not advertise it
reports `0`, and a client must then send no fence rather than one it invented.

### Two tiers

1. **Dedupe**, every mutation, at dispatch. The ledger row stores the
   serialized reply and a replay returns it verbatim.
2. **Receipt**, the PTY-effecting mutations only: `attention/answer`,
   `fleet/action`, `fleet/message_send`. (`terminal/input` joins in R2, when the
   method exists.) Adding a handler to this tier is a checklist item, not a
   default, and `ainb-hangar-proto/src/mutation.rs` has a test that fails when
   the set changes.

### What the daemon answers

The reply carries a `mutation` ack, beside the result on success, inside
`error.data` on a refusal. It is additive: an N-1 client deserializes its own
result type and never sees it.

```json
{ "outcome": "created" | "replayed",          // absent when nothing of yours ran
  "status":  "accepted" | "rejected" | "unknown",
  "reason":  "op_id_foreign" | "op_expired" | "already_answered_by" | "…",
  "receipt": "claimed" | "writing" | "delivered" | "failed" | "unknown" }
```

| Code | Meaning |
| --- | --- |
| `-32007` `PROTOCOL_INCOMPATIBLE` | Version ranges do not overlap. Change binary, not token. |
| `-32008` `MUTATION_REJECTED` | Foreign op id, stale fence, or a body that disagrees with a committed one. Re-read state; do not resend. |
| `-32009` `MUTATION_UNKNOWN` | The effect cannot be established. Show "could not confirm, check the session". |

### Receipt lifecycle

```
claimed ──▶ writing ──▶ delivered | failed
   │            │
   └────────────┴──▶ (daemon dies) ──▶ unknown{effects_ambiguous}
```

`claimed` is written in the SAME SQLite transaction as the state flip, never
through the event outbox, which has a documented crash loss window. `writing`
is committed immediately before the first byte reaches the PTY.

At boot (`receipt_sweep::run`, before the socket accepts anything):

| Found | Done |
| --- | --- |
| receipt `writing` | `unknown{effects_ambiguous}`, plus an attention row of kind `delivery_unconfirmed` naming the answer. Not reopened (a retry could double-type), not closed silently, only an operator closes it. |
| receipt `claimed` | `writing` never committed, so no byte can have left: the attention row is reopened, exactly as a failed send already compensates. |
| any other `in_flight` claim | `unknown{effects_ambiguous}`. Never re-executed. |

### Ledger and retention

Table `mutation_ledger` (migration 0097), keyed `(host_id, principal, op_id)`.
`principal` is `local` on the unix leg, `pal:<scope>` for Pal's tool server, and
`device:<id>` off-box in R1. `host_id` defaults to `local` until R1 mints a
per-daemon ULID.

Retention is two-stage, because D18 wants both a storage bound and an
answerable `unknown{op_expired}`:

| Stage | Trigger | Effect |
| --- | --- | --- |
| expire | older than 7 days, or beyond the newest 100k live rows | drop the reply, keep the key (`expired = 1`) |
| delete | an expired row older than 14 days, or beyond 100k tombstones | remove it |

A hard delete alone cannot do both: once the key is gone the daemon cannot tell
a stale retry from a new operation, and would execute it a second time.

## One-way store steps

Two binaries can share one `hangar.db` during an upgrade, so a schema addition
is also a downgrade question.

| Step | Effect on an N-1 binary |
| --- | --- |
| `attention.kind = 'delivery_unconfirmed'` (migration 0097) | A reader that does not know the kind **skips that row**. Older builds shipped before this tolerance fail their whole attention list once an N daemon writes one, which cannot be fixed retroactively, only for the next new kind. |
| `attention.version`, `mutation_ledger` (migration 0097) | Additive columns and a new table; an N-1 binary ignores both. |

## Test seams

Both are compiled out of a shipped daemon (`cfg(any(test, feature =
"test-support"))`) and exist because the cases they cover cannot be reproduced
any other way.

| Seam | Why |
| --- | --- |
| `answer::set_stall_at_write_boundary_for_test` | Parks the answer at its `writing` boundary so a test can abort the task there. A returned error would be RECORDED as that op id's answer; a killed daemon records nothing, and that difference is the whole point of the receipt. |
| `answer::set_forced_delivery_for_test` | Reports a successful tmux delivery without a tmux, so the race test asserts one `Delivered` and one `AlreadyAnswered` rather than testing `send-keys`. |

## Benchmark

`cargo test -p ainb-hangar-store --test answer_commit_bench -- --ignored --nocapture`

Commit-only p50 for the answer write path. Gate: at or below **0.40 ms**, which
is spike 8's measured p50 for the three-transactions-per-event shape this
replaces. `#[ignore]` by default, a latency assertion on a shared runner is a
flake generator, and a muted gate is worse than a deliberate one.
