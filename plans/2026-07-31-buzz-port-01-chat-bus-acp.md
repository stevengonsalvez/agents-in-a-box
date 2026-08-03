# Plan: Daemon Chat Bus + ACP Provider Adapter (buzz-port part 1)

**Research:** research/2026-07-31_14-56-19_buzz-acp-port.md (verdicts, §6 migration sketch, §7 chat ranking)
**Spike:** research/2026-07-31_acp-resume-steering-spike.md (resume + steering matrix; RE-CHECK before Phases 5 and 6, adapter versions drift on npm)
**Companion:** plans/2026-07-31-buzz-port-02-fleet-chat-copilot.md (part 2 reconciles its draft contract against THIS file at its Phase 0 gate)
**Design provenance:** Design B (winner of A/B/C bake-off) with 8 grafts applied and B's 6 named defects amputated; see "Design decisions" below.
**Date:** 2026-07-31
**Code roots:** `ainb-tui/crates/` (hangar-proto, hangar-store, hangar-daemon, fleet-core, core) · `apps/ainb-fleet-macos/` · buzz reference at session scratchpad `scratchpad/buzz/crates/buzz-acp/src/{pool,queue,acp}.rs` (patterns only, adapt not copy)

## Overview

Add a persisted chat bus to the hangar daemon (message model + `fleet/message_*` RPC family riding the existing negotiate/subscribe/replay spine) and an ACP provider adapter (`ainb-acp` crate on the upstream `agent-client-protocol` crate, daemon-owned process-per-scope AgentPool). The chat bus goes live against existing tmux sessions FIRST (Phase 3), then ACP recipients join it (Phase 5). tmux sessions are untouched as an interactive transport; ACP powers chat-grade headless sessions only.

## Requirements (from Stevie, all mandatory)

- **R1** Persisted chat message model in hangar store (channels or channel-like scopes; per-session and broadcast conversations; receipts preserved)
- **R2** New `fleet/message_*` RPC family on the daemon socket, riding existing negotiate/subscribe/revision-replay/capability-gating spine
- **R3** ACP provider integration: new `ainb-acp` crate on upstream `agent-client-protocol` crate + daemon-owned AgentPool (adapt buzz pool/queue/circuit-breaker patterns; session per chat scope with affinity); tmux sessions untouched
- **R4** Execution logs: persist the FULL `session/update` stream (message/thought/tool_call/plan/permission/usage) as a transcript, queryable + streamed live to subscribers; chat timeline gets final message only (buzz observer pattern)
- **R5** Resume: daemon-level resume guarantee: reuse `session/load` where the adapter supports it, otherwise rebuild context from persisted history (re-prime via first prompt). Design must not DEPEND on `session/load`.
- **R6** One fleet protocol version bump covering Provider enum growth + message family
- **R7** Broadcast semantics: message to N recipients, replies threaded per recipient, receipts per delivery
- **R8** Permission requests surfaced as actionable items (answer by JSON-RPC id) at least via RPC, UI can follow later

## Design decisions (Design B + grafts, defects fixed)

Retained from Design B unchanged: PR order (chat bus live on tmux before any ACP code), boot-time recovery scan (turn_interrupted backfill, PENDING deliveries to UNKNOWN, dead pending-permission attention cleared), permission answering via existing `fleet/action` Approve/Deny/StructuredAnswer + fingerprint staleness machinery, daemon-minted stable `session_key` with mutable `acp_session_id`, invariant-to-test mapping (now I1-I12).

Amputated from B (do NOT implement):
1. The per-connection `effective_version` degrade rule. `handle_fleet_negotiate` is a stateless echo (`rpc/mod.rs:964-989`); no connection-scoped version state exists and negotiate is optional. Replacement: bump-and-refuse (Phase 2) + tolerant Swift provider decode so the NEXT provider addition is capability-only, no bump.
2. `fleet_message_scope` + `fleet_message_scope_member` tables. Scopes are minted strings; targets are supplied per send; membership has zero readers in part 1. Six tables become three.
3. `deliver: bool` flag and `fleet/message_scopes` method. No consumer named; `message_list` with no scope filter covers the digest view.
4. Unspecified "compact digest" re-prime. Replaced by fixed-N + byte-cap prelude (graft 7, Phase 6).
5. `fleet_acp_session.can_load` persisted column. Dead weight; loadSession is re-probed on every spawn.
6. The rg-based tripwire for "no ACP path reaches tmux send". Grep-as-test; the integration assertion (I8) stays.

Grafts applied:
1. **Transcript = `fleet_provider_event`** (from C). No new transcript table. `provider = <adapter provider token>`, `source = 'acp'`, `event_type = 'acp.<kind>'` including daemon-minted `acp.turn_started/turn_completed/turn_failed/turn_interrupted/context_rebuilt`; cursor = `ingest_order`; idempotent `event_id` insert for free. Conditions honoured: `raw_blake3` computed on every insert (schema: `NOT NULL CHECK(length=64)`), and the "projection_revision IS NULL = pending recovery work" contract (`repo/fleet_provider_event.rs:22`) is scoped by source in migration 0075 so ACP rows never bloat the pending partial index.
2. **Tolerant Swift provider decode** (from C): unknown provider token decodes to `.unknown` (`FleetWire.swift:124` is currently non-tolerant). Ships inside the bump PR.
3. **No resync notifications for the two new streams** (from C): both are pure append logs; per-connection forwarders page-to-head from their cursor after every wakeup, so broadcast lag is harmless. No `message_resync_required` / `transcript_resync_required`.
4. **`request_id TEXT UNIQUE` + ON CONFLICT replay** on `fleet_message` (from C) for the insert half of idempotency; B's receipt-claim/fingerprint dance kept for the delivery legs only.
5. **Chunk coalescing** (from C): contiguous same-kind text chunks merged per 4 KiB or kind boundary, bounding transcript row count.
6. **Process-per-scope pool, cap N + LRU idle eviction** (from A), replacing B's fixed-slot claim/affinity machinery; B's SlotCircuit kept verbatim; B's stricter at-most-once retry rule kept (requeue only if the prompt provably never reached the adapter).
7. **Resume re-prime = last N=20 rows of the delivery-join corpus (see Scope + threading rules), 32 KiB byte cap, fixed header string** (from A); B's `context_rebuilt {mode}` marker rows and receipt-detail fingerprint of which path ran are kept.
8. **Explicit failing `Backend::from_provider` arm for the ACP token + unit test** (from A), plus A's `-p ainb-acp` CI verify-by-forced-failure step. Today unknown provider strings silently fall back to `Backend::Claude` (`materialise.rs:97`, proven by `dispatch_routing.rs:559-560`), which would route ACP sessions into tmux send machinery.

## Current state analysis (key discoveries, file:line)

- Method registration is 3 append-only places: proto consts + `ALL_METHODS` tail (`ainb-hangar-proto/src/methods.rs:1576-1578`, fleet block at `:1650-1659`), the mirrored `declared` list in `all_methods_covers_every_const` (`methods.rs:1818-1951`), and the daemon dispatch match (`ainb-hangar-daemon/src/rpc/mod.rs:724-954`, fleet arms `:920-931`, unknown method -32601 at `:949-953`). Names must be `<area>/<verb>` namespaced and unique (`methods.rs:1685-1702`).
- Fleet event delivery is durable-log + wakeup: `fleet_tx` broadcasts ONLY committed revision numbers; per-connection forwarders page durable rows from a cursor (`events.rs:87,171-173`, `rpc/mod.rs:567-610`). The chat bus rides this exact pattern with its own cursors.
- `fleet_provider_event` (migration 0071) already has `event_id UNIQUE`, `(session_key, ingest_order)` index, `raw_blake3 NOT NULL CHECK(length=64)`, and a partial index `WHERE projection_revision IS NULL` whose documented meaning is "pending recovery work" (`ainb-hangar-store/src/repo/fleet_provider_event.rs:22`).
- `FLEET_PROTOCOL_VERSION = 1` (`ainb-hangar-proto/src/fleet.rs:9`); `FleetProvider` is `Claude | Codex | Unknown` and on the wire (`fleet.rs:93-98`); Swift mirror `FleetWire.swift:124` decodes non-tolerantly.
- Capability catalogue pattern at `fleet.rs:12-37` (`FLEET_PROTOCOL_CAPABILITY_IDS`).
- Hardened tmux send path: `ainb-fleet-core/src/fleet/send/tmux.rs` (`tmux_send` at `:72`, `-l --` literal + settle + verify + Enter retries). `ainb-core` already depends on `ainb-fleet-core` (`ainb-core/Cargo.toml:122`).
- Unhardened duplicate send path: `ainb-core/src/cli/run.rs:417-430` (`send_prompt_to_tmux`: bare `send-keys` + `C-m`, no `-l`, no `--`, no verify), sole caller at `run.rs:123`. Violates the "ONE verified send path" invariant (research §9).
- Spike PROVED kill-respawn-load resume for `claude-agent-acp` 0.64.0 and `codex-acp` 1.1.7; `session/update` replay arrives BEFORE the `session/load` reply (handler must be live first); codex config does not survive load (re-apply model/mode/reasoning); steering needs `idleBehavior: promptRequired` to avoid ghost detached turns. Gemini resume unverified.

## What we're NOT doing (part 1)

- No channels UI, threads, confirm cards, copilot service, or MCP tool server (all part 2).
- No `fleet_message_scope` membership tables, no `fleet/message_scopes`, no `deliver` flag (amputated, see above).
- No steering (`_session/steering`) in the delivery path; queue-behind-in-flight-turn only. Steering is part 2 copilot territory.
- No runtime protocol version degrade; v1 clients are refused after the bump (all clients are in-repo and ship in one train).
- No client-side transcript replay for `session/load` resume (the adapter's own store replays history; spike Q1).
- No resync notifications for message/transcript streams (graft 3).
- No Gemini/Copilot ACP adapters (matrix unverified; capability-only addition later, no bump needed thanks to graft 2).
- No per-session model/mode/reasoning settings: adapter config is static daemon config (also what Phase 6 re-applies after codex `session/load`); `fleet_acp_session` deliberately carries no config columns. Part 2 adds them if the copilot needs per-session settings.

## Architecture

```
┌──────────────┐        ┌──────────────┐
│ macOS Fleet  │        │ hangar-tui   │
│ app          │        │ plugin / web │
└──────┬───────┘        └──────┬───────┘
       │  fleet/* over hangar.sock (v2)
       ▼                       ▼
┌─────────────────────────────────────────────────────────┐
│ hangar daemon                                           │
│  fleet/message_send ─▶ fleet_message (+request_id)      │
│                        fleet_message_delivery (receipts)│
│       ┌────────────────────┴───────────────┐            │
│       ▼ per recipient                      ▼            │
│  ┌───────────────┐               ┌──────────────────┐   │
│  │ tmux leg      │               │ ACP leg          │   │
│  │ existing      │               │ AgentPool        │   │
│  │ SendPrompt    │               │ process-per-scope│   │
│  │ action path   │               │ cap N + LRU      │   │
│  └──────┬────────┘               │ SlotCircuit      │   │
│         ▼                        └───┬──────────┬───┘   │
│   tmux sessions                      │ stdio    │       │
│   (untouched)              ┌─────────▼──┐  ┌────▼─────┐ │
│                            │claude-agent│  │codex-acp │ │
│                            │-acp        │  │          │ │
│                            └─────┬──────┘  └────┬─────┘ │
│              session/update      ▼              ▼       │
│  transcript: fleet_provider_event (source='acp')        │
│  timeline:   final message only ─▶ fleet_message        │
│  permission: attention row ─▶ answer via fleet/action   │
└─────────────────────────────────────────────────────────┘
```

## Data model (migration 0075, hangar store)

Three new tables plus one index amendment. All in `ainb-tui/crates/ainb-hangar-store/migrations/0075_chat_bus.sql`.

```sql
-- Chat messages. Scope is a minted string: "session:<key>", "broadcast:<ulid>",
-- part 2 mints "channel:<id>" without schema change.
CREATE TABLE fleet_message (
    id          TEXT PRIMARY KEY,              -- daemon-minted ULID, cursor + sort order
    request_id  TEXT UNIQUE,                   -- client idempotency token (NULL for daemon-authored rows)
    scope_key   TEXT NOT NULL CHECK (length(scope_key) > 0),
    origin_message_id TEXT REFERENCES fleet_message(id),  -- replies only: the message this row answers (R7 thread join)
    sender      TEXT NOT NULL,                 -- "operator" | session_key
    kind        TEXT NOT NULL CHECK (kind IN ('user','agent','marker')),
    body        TEXT NOT NULL,
    created_at  INTEGER NOT NULL
);
CREATE INDEX idx_fleet_message_scope ON fleet_message(scope_key, id);
CREATE INDEX idx_fleet_message_origin ON fleet_message(origin_message_id)
    WHERE origin_message_id IS NOT NULL;

-- One row per (message, recipient): the delivery join R7 requires.
-- States mirror the existing broadcast receipt vocabulary (fleet.rs).
CREATE TABLE fleet_message_delivery (
    message_id  TEXT NOT NULL REFERENCES fleet_message(id),
    session_key TEXT NOT NULL,
    state       TEXT NOT NULL CHECK (state IN ('PENDING','DELIVERED','FAILED','UNKNOWN','REJECTED')),
    fingerprint TEXT,                          -- receipt-claim fingerprint (B machinery)
    detail      TEXT,                          -- incl. resume-path fingerprint (loaded|reprimed)
    resolved_at INTEGER,
    PRIMARY KEY (message_id, session_key)
);

-- ACP session identity. session_key is daemon-minted and STABLE
-- ('acp:' || ulid); acp_session_id is the adapter's MUTABLE id, swapped on rebuild.
CREATE TABLE fleet_acp_session (
    session_key    TEXT PRIMARY KEY,
    scope_key      TEXT NOT NULL,
    provider       TEXT NOT NULL CHECK (length(provider) > 0),  -- adapter token; validated against the adapter registry at the RPC layer, NOT the schema (0071 `source` style), so the next adapter needs no migration
    acp_session_id TEXT,                       -- NULL until session/new succeeds
    cwd            TEXT NOT NULL,
    state          TEXT NOT NULL CHECK (state IN ('ACTIVE','IDLE','EVICTED','DEAD')),
    open_turn_id   TEXT,                       -- non-NULL while a turn is in flight (boot scan input)
    created_at     INTEGER NOT NULL,
    last_active_at INTEGER NOT NULL
);
CREATE UNIQUE INDEX idx_fleet_acp_session_scope_active
    ON fleet_acp_session(scope_key) WHERE state IN ('ACTIVE','IDLE');

-- Scope the pending-recovery contract by source so ACP transcript rows
-- (which never get a projection_revision) do not bloat the partial index.
DROP INDEX idx_fleet_provider_event_projection;
CREATE INDEX idx_fleet_provider_event_projection
    ON fleet_provider_event(projection_revision)
    WHERE projection_revision IS NULL AND source <> 'acp';
```

Transcript rows reuse `fleet_provider_event` as-is (graft 1): no schema change beyond the index above; every insert computes `raw_blake3`; `event_id` is a daemon-minted ULID (adapter-supplied id where one exists); cursor is `ingest_order` filtered by `session_key`.

### Scope + threading rules (R7, normative)

- **Scope grammar**: `session:<session_key>` (a session's own scope; minted by `fleet/acp_session_create` for ACP sessions, or derived from the existing tmux session_key for tmux recipients), `broadcast:<ulid>` (minted by `message_send` when it targets more than one session). Part 2 adds `channel:<id>` without schema change.
- **Direct send** (one target): the user message row carries the recipient's own scope; the reply row lands in the SAME scope with `origin_message_id` = the prompting message id.
- **Broadcast send** (N targets): the user message row carries the minted `broadcast:<ulid>` scope; each recipient's reply row lands in that RECIPIENT'S OWN scope with `origin_message_id` = the broadcast message id. Thread view = the broadcast row + `message_list {origin_id: <broadcast id>}` (I11). No parent column ambiguity: `origin_message_id` is the one and only threading join.
- **Execution routing**: an ACP recipient's prompt ALWAYS runs in the process for the recipient's own scope (pool key = `fleet_acp_session.scope_key`); a broadcast scope never owns a process.
- **Re-prime corpus** (Phase 6) is the DELIVERY JOIN, not a raw scope filter: messages having a `fleet_message_delivery` row for the session_key (inbound, broadcasts included) plus messages with `sender = session_key` (outbound), ordered by id. A broadcast-delivered prompt is therefore never lost from a rebuilt context.

## Wire contract (proto v2)

New methods (registered in all 3 places per recon):

| Method | Params (sketch) | Result | Capability |
|---|---|---|---|
| `fleet/acp_session_create` | `{provider, cwd, scope_key?}` | `{session_key, scope_key}` | `fleet.acp.spawn` |
| `fleet/message_send` | `{scope_key, targets: [session_key], text, request_id}` | `{message_id, deliveries: [{session_key, state}]}` | `fleet.message.send` |
| `fleet/message_list` | `{scope_key?, origin_id?, after_id?, limit}` | `{messages: [...], next_after_id?}` | `fleet.message.read` |
| `fleet/message_subscribe` | `{after_id?}` | `{head_id}` then `fleet/message_event` notifications | `fleet.message.read` |
| `fleet/transcript_list` | `{session_key, after_order?, limit}` | `{chunks: [...], next_after_order?}` | `fleet.transcript.read` |
| `fleet/transcript_subscribe` | `{session_key, after_order?}` | `{head_order}` then `fleet/transcript_event` notifications | `fleet.transcript.read` |

- `fleet/acp_session_create` is R3's entry point (without it no ACP recipient can ever exist) and part of the SAME frozen v2 surface, so no post-freeze wire change: validates `provider` against the adapter registry, inserts the `fleet_acp_session` row (state IDLE, `acp_session_id` NULL, `scope_key` = supplied or minted `session:<session_key>`). NO process spawn at create; the pool spawns lazily on first prompt (the NULL adapter id then routes through the Phase 6 rebuild path's `session/new` leg naturally). Idempotent per live scope: an existing ACTIVE/IDLE session for `scope_key` is returned as-is (backed by the partial unique index). `message_send` keeps its "targets must exist" rule; it never auto-provisions.
- Capability consts are DEFINED in Phase 2 (part of the frozen surface), but each id is appended to `FLEET_PROTOCOL_CAPABILITY_IDS` only in the phase its dispatch arms land (`fleet.message.*` / `fleet.transcript.read` → Phase 3, `fleet.acp.spawn` → Phase 5), so no daemon build ever advertises a capability whose methods answer -32601.
- `FLEET_PROTOCOL_VERSION` 1 → 2, bump-and-refuse: daemon advertises only v2; clients whose declared range excludes 2 get `read_compatible/write_compatible = false` and must upgrade. All clients are in-repo and ship in the same train. `handle_fleet_negotiate` stays a stateless echo.
- `FleetProvider` grows `Acp` (the wire token for ACP-backed sessions; the concrete adapter lives in `fleet_acp_session.provider`).
- Swift `FleetProvider` gains `case acp` AND a tolerant `init(from:)` (unknown token → `.unknown`), so the provider after this one is capability-only.
- Note for part 2 reconciliation: part 2's draft assumed `fleet/thread_list`; part 1 ships no thread method (threading = `origin_message_id` linkage, replies land in the recipient's own scope per Scope + threading rules, R7; thread reads ride `message_list {origin_id}`). Part 2's Phase 0 gate amends its contract table against this section.

## Invariants (the test contract)

| # | Invariant | Proven by | Phase |
|---|---|---|---|
| I1 | `message_send` is idempotent by `request_id`: replay returns the same `message_id` and existing delivery rows, no double delivery | daemon RPC test: send twice, assert one row + one tmux submit | 3 |
| I2 | Subscribers recover from broadcast lag by paging-to-head: no gaps, no duplicates, no resync notification needed | forwarder test: force lag, assert contiguous ids | 3 |
| I3 | Every delivery leg resolves to exactly one terminal state via receipt-claim/fingerprint; receipts queryable per (message, recipient) | delivery-join test incl. concurrent resolvers | 3, 5 |
| I4 | Chat timeline receives ONLY final agent messages; full stream lands in transcript | e2e: prompt ACP scope, assert timeline row count vs chunk count | 5 |
| I5 | `session_key` is stable across `acp_session_id` churn: rebuild swaps the adapter id, key (and receipts/scope references) unchanged | resume test: force rebuild, assert same key | 6 |
| I6 | At-most-once prompt delivery: requeue ONLY when the prompt provably never reached the adapter; otherwise terminal UNKNOWN | pool fault-injection test (kill between claim and write; kill after write) | 5 |
| I7 | Boot scan converges: open turns backfilled `acp.turn_interrupted`, stuck PENDING deliveries → UNKNOWN, dead pending-permission attention cleared | restart test against seeded dirty store | 6 |
| I8 | No ACP session ever reaches tmux send machinery: `Backend::from_provider` rejects the ACP token explicitly (no silent Claude fallback) | unit test in `dispatch_routing.rs` + integration assertion in delivery e2e | 2, 5 |
| I9 | v1 client is refused post-bump (bump-and-refuse); Swift decodes an unknown provider token to `.unknown` | Rust negotiate contract test + Swift `FleetDaemonContractTests` tolerant-decode case | 2 |
| I10 | ACP transcript rows never pollute the pending-recovery contract: partial index excludes `source='acp'`; every row has valid `raw_blake3`; `event_id` re-insert is a no-op | store repo tests + index EXPLAIN assertion | 1, 4 |
| I11 | Broadcast replies thread (R7): each reply lands in the recipient's own scope with `origin_message_id` = the broadcast message id; `message_list {origin_id}` returns exactly the reply set | e2e: broadcast to 2 fake ACP scopes → 2 reply rows in the recipients' own scopes, origin join returns both and nothing else | 5 |
| I12 | Transcript is streamed LIVE (R4): a subscriber attached before the prompt receives chunk events during the turn, not only after turn end | e2e: fake adapter with paced chunks; assert first `transcript_event` arrives before `acp.turn_completed` | 5 |

## Phase 0: Quick win, kill the second unhardened send path

<!-- wave: 1 | depends_on: [] | files: [ainb-tui/crates/ainb-core/src/cli/run.rs, ainb-tui/crates/ainb-core/tests/tripwire_cli_run_prompt.rs] -->

Independent of everything below; restores the fleet-core "ONE verified send path" invariant (research §9).

### Changes

**File**: `ainb-tui/crates/ainb-core/src/cli/run.rs`
- [ ] Delete local `send_prompt_to_tmux` (`run.rs:417-430`: bare `send-keys ... C-m`, no `-l`, no `--` terminator, no submit verification; a prompt beginning with `-` is parsed as a tmux flag and silently corrupted)
- [ ] Replace the call at `run.rs:123` with `ainb_fleet_core::fleet::send::tmux::tmux_send` (hardened: `-l --` literal, paste settle, Enter verify + retries); dependency already present (`ainb-core/Cargo.toml:122`)
- [ ] Keep the surrounding `wait_for_prompt_ready` polling untouched

**File**: `ainb-tui/crates/ainb-core/tests/tripwire_cli_run_prompt.rs` (new, repo `tripwire_cli_*` convention)
- [ ] Committed tmux tripwire: `ainb run` with an initial prompt starting with `-y` reaches the pane verbatim and submits (capture-pane assertion). Pins the behavior change of this phase so it cannot regress silently; the rg check below stays a review-time spot check only.

### Success criteria

Automated:
- [ ] `cargo test -p ainb-core -p ainb-fleet-core` (includes the new `tripwire_cli_run_prompt` test)
- [ ] `cargo clippy -p ainb-core -- -D warnings`
- [ ] `rg -n "send-keys" ainb-tui/crates/ainb-core/src/cli/run.rs` returns nothing (spot check during review, not a committed test)

Manual:
- [ ] `ainb run` with an initial prompt starting with `-y` reaches the session verbatim and submits (same case as the tripwire, eyeballed once on a real agent)

---

## Phase 1: Store, migration 0075 + repos

<!-- wave: 1 | depends_on: [] | files: [ainb-tui/crates/ainb-hangar-store/migrations/0075_chat_bus.sql, ainb-tui/crates/ainb-hangar-store/src/repo/fleet_message.rs, ainb-tui/crates/ainb-hangar-store/src/repo/fleet_acp_session.rs, ainb-tui/crates/ainb-hangar-store/src/repo/fleet_provider_event.rs, ainb-tui/crates/ainb-hangar-store/src/repo/mod.rs] -->

### Changes

- [ ] `migrations/0075_chat_bus.sql` exactly per the Data model section above (3 tables + partial-index recreation)
- [ ] New `repo/fleet_message.rs`: `insert_message` (takes `origin_message_id`; ON CONFLICT(request_id) DO NOTHING + fetch-existing replay per graft 4), `list_by_scope(after_id, limit)`, `list_all(after_id, limit)`, `list_by_origin(origin_id, after_id, limit)` (I11 thread join), `list_for_session(session_key, limit)` (the delivery-join re-prime corpus per Scope + threading rules: inbound via `fleet_message_delivery` + outbound via `sender`; Phase 6 consumer), `insert_delivery`, `claim_delivery(fingerprint)` / `resolve_delivery(state, detail)` (B's receipt-claim pattern, delivery legs only), `deliveries_for_message`
- [ ] New `repo/fleet_acp_session.rs`: mint (`'acp:' || ulid`), insert idempotent per live scope (partial unique index; on conflict return the existing ACTIVE/IDLE row, backing `fleet/acp_session_create`), `set_acp_session_id`, `set_state`, `set_open_turn` / `clear_open_turn`, `list_dirty_at_boot` (open turn or PENDING deliveries); provider validated against the adapter registry at the RPC layer (schema only length-checks, see Data model)
- [ ] `repo/fleet_provider_event.rs`: amend the header doc, the "projection_revision IS NULL = pending recovery work" contract is now scoped to `source <> 'acp'`; add `list_by_session_after(session_key, after_order, limit)` reader
- [ ] Register modules in `repo/mod.rs`

### Success criteria

Automated:
- [ ] `cargo test -p ainb-hangar-store`
- [ ] Repo tests cover: request_id replay returns the original row (I1 insert half); delivery claim is single-winner under concurrent claimers (I3); `event_id` duplicate insert is a no-op and pending partial index excludes `source='acp'` (I10); one-live-session-per-scope constraint fires and conflicting insert returns the existing live row; `list_by_origin` returns exactly the rows inserted with that origin (I11 store half); `list_for_session` includes a broadcast-delivered row and the session's own replies, excludes unrelated scopes
- [ ] Migration applies cleanly on a store snapshot containing pre-existing `fleet_provider_event` rows

Manual:
- [ ] None (pure store phase)

---

## Phase 2: Fleet protocol version bump (v1 → v2), the ONE bump

<!-- wave: 2 | depends_on: [1] | files: [ainb-tui/crates/ainb-hangar-proto/src/fleet.rs, ainb-tui/crates/ainb-hangar-proto/src/methods.rs, ainb-tui/crates/ainb-hangar-daemon/src/materialise.rs, ainb-tui/crates/ainb-hangar-daemon/src/runner.rs, ainb-tui/crates/ainb-hangar-daemon/tests/dispatch_routing.rs, apps/ainb-fleet-macos/Sources/FleetRPC/FleetWire.swift, apps/ainb-fleet-macos/Tests/FleetRPCTests] -->

R6: one bump carries Provider growth AND the message family. Nothing else ever rides it.

### Changes

**Proto** (`ainb-hangar-proto/src/fleet.rs`):
- [ ] `FLEET_PROTOCOL_VERSION` 1 → 2 (`fleet.rs:9`)
- [ ] `FleetProvider` grows `Acp` (`fleet.rs:93-98`)
- [ ] New capability consts DEFINED (`fleet.rs:12-37` pattern): `fleet.message.send`, `fleet.message.read`, `fleet.transcript.read`, `fleet.acp.spawn`. NOT appended to `FLEET_PROTOCOL_CAPABILITY_IDS` yet: each id is advertised only in the phase its dispatch arms land (message/transcript → Phase 3, acp.spawn → Phase 5), so a daemon built between phases never advertises a capability whose methods answer -32601 (the consts are still part of the frozen v2 surface)
- [ ] Typed params/results for the 6 methods in the Wire contract section + `fleet/message_event` / `fleet/transcript_event` notification payloads; serde round-trip tests per existing fleet contract-test pattern

**Methods** (`ainb-hangar-proto/src/methods.rs`, append-only in all 3 places):
- [ ] Consts `FLEET_ACP_SESSION_CREATE`, `FLEET_MESSAGE_SEND`, `FLEET_MESSAGE_LIST`, `FLEET_MESSAGE_SUBSCRIBE`, `FLEET_TRANSCRIPT_LIST`, `FLEET_TRANSCRIPT_SUBSCRIBE` with doc comments, appended at the `ALL_METHODS` tail (`methods.rs:1650-1659` block)
- [ ] Mirror entries in the `declared` list of `all_methods_covers_every_const` (`methods.rs:1818-1951`)
- [ ] (Dispatch arms land in Phases 3 and 5; until then the daemon answers -32601, which is correct pre-handler AND unadvertised per the capability rule above)

**Backend trap** (graft 8):
- [ ] `Backend::from_provider` (`ainb-hangar-daemon/src/materialise.rs:97`) and its mirror (`runner.rs:611`): explicit rejecting arm for the ACP provider token, typed error surfaced to the dispatch caller, NEVER the silent `Backend::Claude` fallback the tests currently pin (`dispatch_routing.rs:559-560`)
- [ ] Unit test in `dispatch_routing.rs`: ACP token is rejected, not routed to Claude (I8 unit half)

**Swift** (bump PR, graft 2):
- [ ] `FleetWire.swift:124`: `FleetProvider` gains `case acp` and a tolerant `init(from:)` decoding unknown raw values to `.unknown`
- [ ] Client protocol range advertises v2
- [ ] `FleetDaemonContractTests`: tolerant-decode case (unknown token → `.unknown`) + negotiate-v2 fixture (I9)

### Success criteria

Automated:
- [ ] `cargo test -p ainb-hangar-proto -p ainb-hangar-daemon`
- [ ] Registry-drift guard green (`all_methods_covers_every_const`), namespacing test green
- [ ] `swift test --package-path apps/ainb-fleet-macos` (contract suite incl. new cases); `swift-contract-paths` CI workflow triggers on this diff (it watches fleet proto paths per commit 79cc417b)
- [ ] Negotiate contract test: client declaring read/write range max 1 gets `read_compatible=false, write_compatible=false` (I9)
- [ ] insta snapshot churn reviewed: the v2 bump + ALL_METHODS growth dents inline snapshots across hangar-daemon/store/core (events.rs, fleet.rs, runner.rs, etc.); run `cargo insta review`, accept only the expected dents, and call out snapshot-only diffs in the PR so reviewers can tell intentional updates from drift

Manual:
- [ ] Existing macOS Fleet app build from the same train connects and negotiates v2

### Checkpoint
- **`[CHECKPOINT:human-verify]`**: Wire contract freeze. What was built: v2 proto surface (methods, capabilities, provider token, notifications). How to verify: read the fleet.rs/methods.rs diff + Swift contract test fixtures; confirm part 2's Phase 0 gate can reconcile against it. Resume: "approved" or name the change. After this checkpoint the v2 surface is append-only.

---

## Phase 3: Daemon chat bus, live on tmux sessions

<!-- wave: 3 | depends_on: [1, 2] | files: [ainb-tui/crates/ainb-hangar-daemon/src/rpc/mod.rs, ainb-tui/crates/ainb-hangar-daemon/src/events.rs, ainb-tui/crates/ainb-hangar-daemon/src/lib.rs, ainb-tui/crates/ainb-hangar-proto/src/fleet.rs] -->

The bus ships useful WITHOUT any ACP code: `message_send` to N tmux sessions is broadcast-with-history (research §7: "broadcast becomes message to N sessions"). This is the phase that makes part 1 incrementally shippable.

### Changes

**Events** (`events.rs`):
- [ ] Two new wakeup channels on `EventBroker`, mirroring `fleet_tx`: `message_tx: broadcast<String>` (message id) and `transcript_tx: broadcast<(String, i64)>` (session_key, ingest_order). Durable rows are the source of truth; channels only wake forwarders (pattern at `events.rs:87,171-173`)

**Handlers** (`rpc/mod.rs`, free-fn pattern per `handle_fleet_timeline` at `:1056-1105`; dispatch arms appended in the fleet block `:920-931`):
- [ ] `handle_fleet_message_send`: parse + validate targets exist and are tmux-backed (ACP targets arrive in Phase 5; unknown target → per-delivery REJECTED, not request failure); idempotent insert by `request_id` (replay returns original message + deliveries, I1); insert PENDING delivery rows; per recipient, run the EXISTING SendPrompt fleet action path (same receipts/fingerprint machinery, `fleet-core` verified tmux send); resolve delivery DELIVERED on verified submit, FAILED on error; emit `message_tx` wakeup after commit
- [ ] `handle_fleet_message_list`: cursor page by `(scope_key?, origin_id?, after_id, limit)` (`origin_id` = the I11 thread join)
- [ ] `handle_fleet_message_subscribe`: register receiver BEFORE reading head (the `pending_fleet_rx` ordering trick at `rpc/mod.rs:364-366`), ack `{head_id}`, then `spawn_message_forwarder` modeled on `spawn_fleet_forwarder` (`rpc/mod.rs:567-610`) with ONE difference (graft 3): on broadcast lag, page-to-head from the cursor and continue; no resync notification, no exit
- [ ] `handle_fleet_transcript_list` / `handle_fleet_transcript_subscribe`: same shapes over `fleet_provider_event` filtered `session_key`, cursor `ingest_order` (readers exist from Phase 1; rows appear in Phase 5; empty until then is fine and testable)
- [ ] Capability gating on all five message/transcript handlers, per existing write-surface pattern
- [ ] Append `fleet.message.send`, `fleet.message.read`, `fleet.transcript.read` to `FLEET_PROTOCOL_CAPABILITY_IDS` (`fleet.rs`), in the SAME change as the dispatch arms (consts were defined in Phase 2; advertisement deliberately deferred to here so no daemon build advertises -32601 methods)

**Wiring** (`lib.rs`):
- [ ] Broker construction + forwarder spawn parity with fleet stream (boot wiring at `lib.rs:422-432` pattern)

### Success criteria

Automated:
- [ ] `cargo test -p ainb-hangar-daemon`
- [ ] I1 test: double `message_send` with same `request_id` → one message row, one tmux submit, identical response
- [ ] I2 test: subscriber under forced broadcast lag receives a contiguous, duplicate-free id sequence via page-to-head
- [ ] I3 test: N-target send yields N delivery rows, each resolving exactly once; receipts queryable per (message, recipient)
- [ ] Capability-gate test: connection without `fleet.message.send` gets the standard capability error
- [ ] insta snapshot churn reviewed: capability-list growth dents inline snapshots; `cargo insta review`, snapshot-only diffs called out in the PR (same rule as Phase 2)

Manual:
- [ ] From a v2 client: send one message to 3 running tmux sessions; all 3 receive it (verified submit), `message_list` shows the row, deliveries show 3 terminal states

---

## Phase 4: `ainb-acp` crate (client + transcript reducer)

<!-- wave: 3 | depends_on: [1] | files: [ainb-tui/crates/ainb-acp/**, ainb-tui/Cargo.toml, .github/workflows/**] -->

Pure library phase, shippable with zero daemon wiring. Parallel with Phase 3 (no file overlap).

### Changes

**New crate** `ainb-tui/crates/ainb-acp/` on upstream `agent-client-protocol` v1.x (pinned; do NOT hand-roll the protocol, buzz predates the crate):
- [ ] `client.rs`: spawn adapter by name (`claude-agent-acp`, `codex-acp`), `initialize` (pin protocolVersion 1), `session/new` (cwd, mcpServers, permission mode PINNED explicitly at session/new, never inherited from env, spike security flag), `session/prompt`, `session/cancel`, `session/load`; notification handler registered and routing BEFORE any `session/load` is issued (spike: replay arrives before the load reply; "the single most likely implementation bug in the port")
- [ ] `reducer.rs`: `session/update` stream → normalized `TranscriptChunk { kind: message|thought|tool_call|plan|permission|usage, ... }`; final-message extraction for the timeline (R4); chunk coalescing per graft 5 (contiguous same-kind text merged, flush at 4 KiB or kind boundary), unit-tested against recorded update streams
- [ ] `store_writer.rs`: chunks → `fleet_provider_event` rows (`source='acp'`, `event_type='acp.<kind>'`, `event_id` = adapter id else minted ULID, `raw_blake3` computed, `session_key` from `fleet_acp_session`); daemon-minted lifecycle rows `acp.turn_started/turn_completed/turn_failed/turn_interrupted/context_rebuilt` (B's markers). Pure library, no EventBroker access: each commit RETURNS the committed `(session_key, ingest_order)` high-water mark; the Phase 5 pool transcript pump owns the `transcript_tx` emit
- [ ] `circuit.rs`: SlotCircuit verbatim from B (per-process crash breaker, jittered exponential backoff; adapt buzz `lib.rs:1027-1136` pattern)
- [ ] Real-adapter integration tests behind `#[ignore]` + env gate (spike probes promoted; disclosure comment real-adapter vs fixture per house rule)

**CI** (graft 8, A's step):
- [ ] Add `-p ainb-acp` to the workspace test lane
- [ ] Verify-by-forced-failure: one throwaway commit with a failing `ainb-acp` test proving the lane actually executes it; revert before merge, link the red run in the PR description

### Success criteria

Automated:
- [ ] `cargo test -p ainb-acp` (reducer + coalescing + writer against a fake adapter binary speaking scripted ndjson)
- [ ] I10 writer half: every row has 64-char `raw_blake3`; duplicate `event_id` no-op
- [ ] `cargo test -p ainb-acp -- --ignored` green locally against real `claude-agent-acp` 0.64.0 and `codex-acp` 1.1.7 (documented in PR, not CI-required)
- [ ] Red CI run linked, proving the `-p ainb-acp` lane fires

Manual:
- [ ] None (library phase)

---

## Phase 5: AgentPool + ACP delivery leg + actionable permissions

<!-- wave: 4 | depends_on: [3, 4] | files: [ainb-tui/crates/ainb-hangar-daemon/src/acp_pool.rs, ainb-tui/crates/ainb-hangar-daemon/src/rpc/mod.rs, ainb-tui/crates/ainb-hangar-daemon/src/lib.rs, ainb-tui/crates/ainb-hangar-proto/src/fleet.rs] -->

### Changes

**Session create** (R3's entry point, per the Wire contract section):
- [ ] `handle_fleet_acp_session_create`: validate `provider` against the adapter registry (`claude-agent-acp`, `codex-acp`); insert `fleet_acp_session` row (state IDLE, `acp_session_id` NULL, `scope_key` supplied or minted `session:<session_key>`); idempotent per live scope (existing ACTIVE/IDLE row returned); NO process spawn here, the pool spawns lazily on first prompt
- [ ] Dispatch arm + append `fleet.acp.spawn` to `FLEET_PROTOCOL_CAPABILITY_IDS` (advertisement lands with its handler, per the Phase 2 capability rule)

**Pool** (new `ainb-hangar-daemon/src/acp_pool.rs`, graft 6):
- [ ] Process-per-scope: one adapter process per session's own scope (`fleet_acp_session.scope_key` → live `AcpSession`); affinity is structural, no slot claim machinery; broadcast scopes NEVER own a process (Scope + threading rules)
- [ ] Transcript pump (owns R4's live-stream leg, I12): per-process task consumes reducer chunks → `store_writer` commit → emits `transcript_tx (session_key, ingest_order)` after EVERY committed batch, using the high-water mark `store_writer` returns
- [ ] Cap N concurrent processes (config, default 8) + LRU idle eviction: evict = `session/close` + process stop + `fleet_acp_session.state = EVICTED`; `session_key` survives for Phase 6 resume
- [ ] Per-scope FIFO queue, ONE prompt in flight per scope; mid-turn arrivals queue (no steering in part 1)
- [ ] At-most-once retry (B's rule, I6): requeue only if the prompt provably never reached the adapter (stdin write failed before flush); after write, outcome is turn-end or UNKNOWN, never a blind resend
- [ ] SlotCircuit wraps each process; breaker-open scope fails deliveries fast with FAILED + detail

**Delivery leg** (`rpc/mod.rs` `handle_fleet_message_send` extension):
- [ ] ACP-backed recipients accepted (`targets` must exist in `fleet_acp_session`, minted via `fleet/acp_session_create`; no auto-provision); `session/prompt` dispatched via the pool to the RECIPIENT'S OWN scope process (Scope + threading rules; a broadcast scope never spawns a process); delivery stays PENDING at write-ack and resolves at TURN END (`acp.turn_completed` → DELIVERED, `acp.turn_failed` → FAILED; C-defect 5 fix), through the Phase 1 claim/resolve receipt path
- [ ] On turn end, reducer's final message inserted as `fleet_message {sender: session_key, kind: 'agent', scope_key: recipient's own scope, origin_message_id: the prompting message id}` (R7/I11: direct replies share the scope, broadcast replies thread via `origin_message_id`) + `message_tx` wakeup; full stream already flowing to `fleet_provider_event` + `transcript_tx` via the pool pump (I4, I12)
- [ ] Integration assertion (I8): delivery e2e asserts zero tmux invocations for ACP recipients (fake tmux binary on PATH recording calls)

**Permissions** (R8, B retained):
- [ ] `session/request_permission` → attention row (APPROVAL/ASK) + fleet event carrying option ids and the pending JSON-RPC request id
- [ ] Answering rides the EXISTING `fleet/action` Approve/Deny/StructuredAnswer with fingerprint staleness (no new method); the daemon routes the answer back to the adapter's pending JSON-RPC id
- [ ] Fleet snapshot surfaces these sessions with `provider: Acp`

### Success criteria

Automated:
- [ ] `cargo test -p ainb-hangar-daemon`
- [ ] I4 e2e (fake adapter): one prompt → timeline gets exactly the final message; transcript gets all chunks in order
- [ ] I6 fault injection: kill process between claim and stdin write → requeued once; kill after write → UNKNOWN, no resend
- [ ] I8 integration: ACP delivery run records zero tmux calls
- [ ] I11 e2e: broadcast to 2 fake ACP scopes → 2 reply rows, each in its recipient's own scope with `origin_message_id` = the broadcast message id; `message_list {origin_id}` returns exactly those two
- [ ] I12 e2e: subscriber attached before the prompt receives `transcript_event` chunks DURING the fake-adapter turn (first chunk before `acp.turn_completed`), proving the pump's live leg
- [ ] `acp_session_create` tests: unknown provider rejected; double create for the same scope_key returns the same `session_key`; capability-gated
- [ ] Permission round-trip: request → attention row → `fleet/action` Approve → adapter receives the answer on the original JSON-RPC id; stale fingerprint rejected
- [ ] LRU eviction test: N+1 scopes → oldest idle evicted, state EVICTED, key intact

Manual:
- [ ] From a v2 client: `fleet/acp_session_create {provider: claude, cwd}` → `message_send` to the returned `session_key`: reply appears in `message_list`, `transcript_list` shows thought/tool chunks, a permission request is answerable via `fleet/action`

---

## Phase 6: Resume + boot recovery

<!-- wave: 5 | depends_on: [5] | files: [ainb-tui/crates/ainb-hangar-daemon/src/acp_pool.rs, ainb-tui/crates/ainb-hangar-daemon/src/lib.rs, ainb-tui/crates/ainb-acp/src/client.rs] -->

GATE before starting: re-read research/2026-07-31_acp-resume-steering-spike.md and re-run its probe scripts against the adapter versions actually pinned at implementation time (npm drift). The resume routine below must hold for the re-checked matrix.

### Changes

**Resume routine** (pool spawn-for-existing-`session_key`; R5, must not DEPEND on `session/load`):
- [ ] Probe per spawn (no persisted `can_load`, B-defect 5): adapter advertises `loadSession` AND `acp_session_id` is non-NULL → attempt `session/load` with the notification handler live FIRST (spike); on success, codex only: re-apply model/mode/reasoning FROM STATIC DAEMON ADAPTER CONFIG, which is the source of truth in part 1 (spike: config does not survive load; the same config was applied at `session/new`, so re-applying it restores the pre-load state exactly; no per-session config columns exist by design, see "What we're NOT doing"), mark path `loaded`
- [ ] Any load failure, missing capability, or NULL adapter id → rebuild: `session/new` → store new `acp_session_id` (SAME `session_key`, I5) → re-prime prompt = fixed header string + last N=20 rows from `list_for_session(session_key)` (the delivery-join corpus per Scope + threading rules: inbound deliveries INCLUDING broadcast-delivered prompts + the session's own replies; never a raw scope filter), 32 KiB byte cap, oldest dropped first (graft 7, deterministic and testable)
- [ ] Either path: `context_rebuilt {mode: loaded|reprimed}` marker row into the transcript; next delivery's receipt `detail` carries the path fingerprint (B retained)

**Boot scan** (daemon start, B §7 retained; `lib.rs` boot sequence):
- [ ] Sessions with `open_turn_id` set → insert `acp.turn_interrupted` transcript row, clear open turn
- [ ] Deliveries stuck PENDING whose in-memory responder died with the daemon → UNKNOWN with detail `daemon_restart`
- [ ] Pending-permission attention rows whose ACP responder no longer exists → resolved/cleared + fleet event (the poison A leaves forever)
- [ ] All idempotent: double boot scan is a no-op

### Success criteria

Automated:
- [ ] `cargo test -p ainb-hangar-daemon -p ainb-acp`
- [ ] I5: forced rebuild swaps `acp_session_id`, `session_key` and existing delivery rows untouched
- [ ] Re-prime determinism: fixed corpus (including one broadcast-delivered row, proving the delivery join) → byte-identical prelude; 21st message dropped; 32 KiB cap enforced
- [ ] I7: seeded dirty store (open turn + PENDING deliveries + orphan permission attention) → boot converges; second boot changes nothing
- [ ] `#[ignore]` real-adapter test: secret-word resume, SIGKILL daemon + adapter mid-conversation, restart, `session/load` path recalls the word (claude + codex)
- [ ] `#[ignore]` real-adapter test: force load-failure (fabricated adapter id) → re-prime path still yields a contextful answer; `context_rebuilt {mode: reprimed}` present

Manual:
- [ ] Kill the daemon mid-turn on a live chat, restart, continue the conversation from a v2 client; attention list carries no ghost permission items

### Checkpoint
- **`[CHECKPOINT:human-verify]`**: Part 1 exit review. What was built: chat bus (tmux + ACP legs), transcripts, permissions, resume. How to verify: run the Phase 5 and 6 manual steps; confirm part 2's Phase 0 gate reconciles cleanly against the landed contract. Resume: "approved" unlocks part 2 implementation phases.

---

## Testing strategy

| Layer | Tool | Notes |
|---|---|---|
| Store | `cargo test -p ainb-hangar-store` unit tests | idempotency, claim races, index contracts (I1 insert half, I3, I10) |
| Proto contract | round-trip tests in `ainb-hangar-proto` + registry-drift guards | append-only registries, I9 negotiate |
| Swift contract | `Tests/FleetRPCTests` (`FleetDaemonContractTests` + fixtures), CI `swift-contract-paths` | tolerant decode, v2 fixtures; ONE fixture set shared with part 2 once its gate runs |
| Daemon | RPC integration tests against fake adapter + fake tmux recorder | I1, I2, I4, I6, I7, I8, I11, I12 |
| Real adapters | spike probes promoted to `ainb-acp/tests/` behind `#[ignore]` + env gate | resume secret-word, load-failure fallback; every test comments real-adapter vs fixture |
| CI | `-p ainb-acp` lane, verify-by-forced-failure once | graft 8 |

## Risks

| Risk | Mitigation |
|---|---|
| Adapter drift on npm invalidates spike facts | Phase 6 gate re-runs spike probes against pinned versions; version floors asserted from `agentInfo` at initialize |
| `session/load` replay dropped (handler not live) | Named as the port's most likely bug; client.rs enforces handler-before-load by construction, real-adapter test proves history arrives |
| ACP session silently routed to tmux backend | I8: explicit `from_provider` rejection (Phase 2) + integration assertion (Phase 5) |
| Transcript volume bloats the store | Chunk coalescing (graft 5); `fleet_provider_event` growth contract already documents the operator-export escape hatch; pending index scoped so recovery scans stay cheap |
| Broadcast-lag correctness without resync frames | I2 page-to-head test under forced lag; append-only logs make it safe by construction |
| Part 2 lands assumptions this file broke (e.g. `fleet/thread_list`) | Wire-contract checkpoint in Phase 2; part 2's Phase 0 gate reconciles; divergence already flagged in the Wire contract section |
| v2 bump strands an out-of-train client | All clients in-repo, one train; bump-and-refuse is explicit and tested (I9), tolerant decode makes the next provider bump-free |

## Open questions (pre-implementation gates, none block Phases 0-3)

- [ ] `session/load` with a well-formed but NONEXISTENT UUID on claude-agent-acp: spike inferred `-32603` but did not measure; one probe during the Phase 6 gate (also feeds part 2's identical open question)
- [ ] Pool cap default (8) and idle-eviction window: pick from real memory footprint of one adapter process during Phase 5; config knob either way
- [ ] Scope-key grammar for part 2 channels (`channel:<id>`): confirm at part 2's Phase 0 gate that minted strings suffice (they did for C's proof); no schema change expected
