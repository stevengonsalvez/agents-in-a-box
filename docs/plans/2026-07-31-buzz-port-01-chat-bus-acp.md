---
title: "Buzz port part 1: daemon chat bus + ACP adapter"
---

# Plan: Daemon Chat Bus + ACP Provider Adapter (buzz-port part 1)

**Research:** [research, discussion #570](https://github.com/stevengonsalvez/agents-in-a-box/discussions/570) (verdicts, §6 migration sketch, §7 chat ranking)
**Explainer:** https://explainers.stevengonsalvez.com/buzz-acp-port/ (committed copy: explainers/buzz-acp-port-research.html)
**Spike:** [spike report, discussion #570 comment](https://github.com/stevengonsalvez/agents-in-a-box/discussions/570#discussioncomment-17880848) (resume + steering matrix; RE-CHECK before Phases 5 and 6, adapter versions drift on npm)
**Companion:** docs/plans/2026-07-31-buzz-port-02-fleet-chat-copilot.md (part 2 reconciles its draft contract against THIS file at its Phase 0 gate)
**Design provenance:** Design B (winner of A/B/C bake-off) with 8 grafts applied and B's 6 named defects amputated; see "Design decisions" below.
**Date:** 2026-07-31
**Amended:** 2026-08-04 (distinguished-engineer review; every amendment is marked `(DE review 2026-08-04)` where it changes or contradicts the original text)
**Code roots:** `ainb-tui/crates/` (hangar-proto, hangar-store, hangar-daemon, fleet-core, core) · `apps/ainb-fleet-macos/` · buzz reference at session scratchpad `scratchpad/buzz/crates/buzz-acp/src/{pool,queue,acp}.rs` (patterns only, adapt not copy)

## Overview

Add a persisted chat bus to the hangar daemon (message model + `fleet/message_*` RPC family riding the existing negotiate/subscribe/replay spine) and an ACP provider adapter (`ainb-acp` crate on the upstream `agent-client-protocol` crate, daemon-owned AgentPool multiplexing sessions per provider process, graft 6). The chat bus goes live against existing tmux sessions FIRST (Phase 3), then ACP recipients join it (Phase 5). tmux sessions are untouched as an interactive transport; ACP powers chat-grade headless sessions only.

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

Retained from Design B unchanged: PR order (chat bus live on tmux before any ACP code), boot-time recovery scan (turn_interrupted backfill, PENDING deliveries to UNKNOWN, dead pending-permission attention cleared), permission answering via existing `fleet/action` Approve/Deny/StructuredAnswer + fingerprint staleness machinery, daemon-minted stable `session_key` with mutable `acp_session_id`, invariant-to-test mapping (now I1-I16).

Amputated from B (do NOT implement):
1. The per-connection `effective_version` degrade rule. `handle_fleet_negotiate` is a stateless echo (`rpc/mod.rs:964-989`); no connection-scoped version state exists and negotiate is optional. Replacement: bump-and-refuse (Phase 2) + tolerant Swift provider decode so the NEXT provider addition is capability-only, no bump.
2. `fleet_message_scope` + `fleet_message_scope_member` tables. Scopes are minted strings; targets are supplied per send; membership has zero readers in part 1. Six tables become three.
3. `deliver: bool` flag and `fleet/message_scopes` method. No consumer named; `message_list` with no scope filter covers the digest view.
4. Unspecified "compact digest" re-prime. Replaced by fixed-N + byte-cap prelude (graft 7, Phase 6).
5. `fleet_acp_session.can_load` persisted column. Dead weight; loadSession is re-probed on every spawn.
6. The rg-based tripwire for "no ACP path reaches tmux send". Grep-as-test; the integration assertion (I8) stays.

Grafts applied:
1. **Transcript = `fleet_provider_event`** (from C). No new transcript table. `provider = <adapter provider token>`, `source = 'acp'`, `event_type = 'acp.<kind>'` including daemon-minted `acp.turn_started/turn_completed/turn_failed/turn_interrupted/context_rebuilt`; cursor = `ingest_order`; idempotent `event_id` insert for free. Conditions honoured: `raw_blake3` computed on every insert (schema: `NOT NULL CHECK(length=64)`), and the "projection_revision IS NULL = pending recovery work" contract (`repo/fleet_provider_event.rs:22`) is preserved by INDEX KEY, not by predicate (DE review 2026-08-04: the original "scope the partial index by `source <> 'acp'`" plan makes the index unusable for the one query that reads it, see Retention and growth).
2. **Tolerant Swift provider decode** (from C): unknown provider token decodes to `.unknown` (`FleetWire.swift:124` is currently non-tolerant). Ships inside the bump PR.
3. **No resync notifications for the two new streams** (from C): both are pure append logs; per-connection forwarders page-to-head from their cursor after every wakeup, so broadcast lag is harmless. No `message_resync_required` / `transcript_resync_required`. (DE review 2026-08-04: verified sound. `spawn_fleet_forwarder` at `rpc/mod.rs:567-609` already drains to head before it ever waits on the channel; its Lagged branch emits resync and RETURNS, killing the forwarder. For an append-only log, continuing from the cursor is strictly better. This graft is correct and load-bearing. It is ONLY correct if the cursor is commit-ordered, which is why graft 9 exists.)
4. **`request_id TEXT UNIQUE` + replay** on `fleet_message` (from C) for the insert half of idempotency; B's receipt-claim/fingerprint dance kept for the delivery legs only. (DE review 2026-08-04: amended from bare `ON CONFLICT DO NOTHING`. The repo's established contract for a reused `request_id` is to REJECT a mismatched replay, not to silently succeed: `rpc/mod.rs:1676-1687` returns `invalid_params("request_id was reused for a different Fleet start")` when the stored fingerprint differs. `fleet_message` therefore carries `request_fingerprint` and `message_send` mirrors that rejection.)
5. **Chunk coalescing** (from C): contiguous same-kind text chunks merged per 4 KiB or kind boundary, bounding transcript row count. (DE review 2026-08-04: coalescing bounds ROW count, not COMMIT count. A commit cadence is added in Phase 4, see Retention and growth.)
6. **Multiplexed pool** (DECIDED by Stevie 2026-08-04, resolving the DE open question AGAINST the DE recommendation of process-per-scope): one adapter process PER PROVIDER hosts many ACP sessions, buzz shape (`scratchpad/buzz/crates/buzz-acp/src/pool.rs:87-107`), pool map = `scope_key -> (provider process, acp_session_id)`. Protocol-legal: both adapters address every method by `sessionId` and accept per-session `cwd`. Per-scope FIFO with ONE prompt in flight per scope is unchanged (that is session-level concurrency control, orthogonal to process topology). Idle eviction is session-level (`session/close`), the process stays warm. B's SlotCircuit kept, now per PROVIDER PROCESS; B's stricter at-most-once retry rule kept (requeue only if the prompt provably never reached the adapter). Accepted trade-off: a provider-process crash interrupts EVERY in-flight session on it; recoverable because spike proved full context recovery via `session/load`, and I16 convergence must fan out to all affected sessions.
7. **Resume re-prime = last N=20 rows of the delivery-join corpus (see Scope + threading rules), 32 KiB byte cap, fixed header string** (from A); B's `context_rebuilt {mode}` marker rows and receipt-detail fingerprint of which path ran are kept. (DE review 2026-08-04: message bodies are untrusted text and are concatenated into a prompt. The corpus must be fenced and escaped, see I15.)
8. **Explicit trap so no ACP session reaches the tmux send machinery** (from A), plus A's `-p ainb-acp` CI verify-by-forced-failure step. (DE review 2026-08-04: the trap has MOVED. The original graft aimed at `Backend::from_provider`, citing `materialise.rs:97`. That citation is wrong and the target is wrong: `materialise.rs:97` is `ProviderSkillLayout::from_provider`, a different function returning `GeminiOrDefault`; the real `Backend::from_provider` is `runner.rs:611` and its ONLY production caller is `resolve_dispatch` at `run_loop.rs:2448`, which resolves an AGENT's provider for issue dispatch and never sees a chat session. Worse, `dispatch_routing.rs:557-560` deliberately pins "a genuinely not-wired / misconfigured provider must still dispatch (to the safe default) rather than strand the task", so making it fallible reverses a live invariant for unrelated callers. The real exposure is `handle_fleet_action`, `rpc/mod.rs:1571-1579`, where any non-codex provider's `SendPrompt` falls through to `verified_tmux_send`. The trap now lives there. `Backend::from_provider` is left alone.)
9. **Commit-ordered cursor for `fleet_message`** (DE review 2026-08-04, NEW). The message stream's cursor is a SQLite-assigned `INTEGER PRIMARY KEY AUTOINCREMENT`, exactly as `fleet_provider_event.ingest_order` already does (`migrations/0071_fleet_provider_event.sql:6`). A daemon-minted ULID is NOT a safe cursor: it is minted in application code before the write transaction, so two concurrent `message_send` handlers can commit out of id order, and a forwarder that has already paged past the higher id will never re-read the lower one. The existing fleet stream is gapless precisely because `revision` is `last_insert_rowid()` assigned INSIDE the write transaction (`repo/fleet.rs:396-413`) and SQLite serialises writers. See I14.

Boundary decision (2026-08-05): `ainb-acp` runs IN-PROCESS as a daemon library, not as a wire client. Rationale: turn-end atomicity (final message + delivery resolve + transcript marker commit together), the transcript hot path stays off the socket, and no ingest-only methods widen the frozen v2 surface. Every UI client rides the same fleet/* wire contract; exactly ONE process touches SQLite. Extraction path stays open by construction: `ainb-acp` has no EventBroker access and `store_writer` returns high-water marks, so promoting it to a standalone wire-speaking process (the buzz harness shape, if isolation is ever needed) is a bounded refactor, not a redesign.

## Current state analysis (key discoveries, file:line)

- Method registration is 3 append-only places: proto consts + `ALL_METHODS` tail (`ainb-hangar-proto/src/methods.rs:1576-1578`, fleet block at `:1650-1660`), the mirrored `declared` list in `all_methods_covers_every_const` (`methods.rs:1818-1951`), and the daemon dispatch match (`ainb-hangar-daemon/src/rpc/mod.rs:724-954`, fleet arms `:920-931`, unknown method -32601 at `:949-953`). Names must be `<area>/<verb>` namespaced and unique (`methods.rs:1685-1702`).
- Fleet event delivery is durable-log + wakeup: `fleet_tx` broadcasts ONLY committed revision numbers; per-connection forwarders page durable rows from a cursor (`events.rs:87,171-173`, `rpc/mod.rs:567-610`). The chat bus rides this exact pattern with its own cursors.
- Fleet revisions are commit-ordered because SQLite assigns them: `last_insert_rowid()` inside the write transaction (`repo/fleet.rs:396-413`). Any new cursor must inherit this property, not approximate it (DE review 2026-08-04).
- `fleet_provider_event` (migration 0071) already has `ingest_order INTEGER PRIMARY KEY AUTOINCREMENT`, `event_id UNIQUE`, `(session_key, ingest_order)` index, `raw_blake3 NOT NULL CHECK(length=64)`, and a partial index `WHERE projection_revision IS NULL` whose documented meaning is "pending recovery work" (`ainb-hangar-store/src/repo/fleet_provider_event.rs:22`).
- That partial index has exactly ONE consumer query: `repo/fleet_provider_event.rs:192-194`, `WHERE provider = ? AND source = ? AND projection_revision IS NULL` (DE review 2026-08-04, this constrains how migration 0079 may touch it).
- `fleet_provider_event`'s header doc (`repo/fleet_provider_event.rs:3-25`) declares "RETENTION: none. This ledger is never trimmed", justified by a MEASURED rate of roughly 21 rows per 2 days at a ~344 byte mean payload (~1.3 MB per YEAR), with an explicit "revisit trigger: this table exceeding ~1M rows or ~100 MB". Putting ACP transcripts in this table invalidates that measurement by two to three orders of magnitude (DE review 2026-08-04, see Retention and growth).
- `FLEET_PROTOCOL_VERSION = 1` (`ainb-hangar-proto/src/fleet.rs:9`); `FleetProvider` is `Claude | Codex | Unknown` and on the wire (`fleet.rs:93-98`); Swift mirror `FleetWire.swift:124` decodes non-tolerantly.
- The Swift client genuinely enforces refusal: `FleetConnection.swift:118-120` throws `protocolReadIncompatible` when `read_compatible` is false, and every write goes through `requireWriteCapability` (`FleetConnection.swift:422`) which gates on `write_compatible`. BUT `FleetStore.swift:386` calls `negotiate(readVersions:)` and never passes `writeVersions`, so it keeps the `(min:1,max:1)` default at `FleetConnection.swift:104` (DE review 2026-08-04, see Phase 2).
- Capability catalogue pattern at `fleet.rs:12-37` (`FLEET_PROTOCOL_CAPABILITY_IDS`).
- The fleet snapshot projects from `fleet_session` only (`daemon/src/fleet.rs:246-251`), and the provider token maps to the wire enum at `daemon/src/fleet.rs:905-907` with everything unrecognised falling to `Unknown` (DE review 2026-08-04, see Session identity).
- `handle_fleet_action` routes any non-codex provider's `SendPrompt` to `verified_tmux_send` (`rpc/mod.rs:1571-1579`). `verified_tmux_send` (`rpc/mod.rs:2589-2605`) fails SAFE when `tmux_target` is NULL, returning `Unknown` with detail "exact tmux process identity is unavailable". `Approve`/`Deny`/`StructuredAnswer` are guarded `if session.provider == "claude"` and otherwise fall to `Unknown` with "authoritative provider request transport is not active" (`rpc/mod.rs:1614-1619`). Every action is pre-gated by `action_capability` against the session's `capabilities` JSON (`rpc/mod.rs:2866-2884`) (DE review 2026-08-04).
- Store is WAL with `busy_timeout(10s)` and a default-sized pool (`ainb-hangar-store/src/store.rs:77-90`); the code comment at `:84` already warns that a contended writer "can exhaust its `busy_timeout` and get swallowed".
- Migrations are forward-only with no down files (`ainb-hangar-store/src/lib.rs:9-13`); `apply_migrations` is `sqlx::migrate!().run()` (`:118-120`), which refuses to start against a database carrying an applied migration the binary does not embed. `schema_drift` (`:139`) exists to surface the softer stale-binary case in the daemon-health pane.
- Hardened tmux send path: `ainb-fleet-core/src/fleet/send/tmux.rs` (`tmux_send` at `:72`, `-l --` literal + settle + verify + Enter retries). `ainb-core` already depends on `ainb-fleet-core` (`ainb-core/Cargo.toml:122`).
- Unhardened duplicate send path: `ainb-core/src/cli/run.rs:418-435` (`send_prompt_to_tmux`: bare `send-keys` + `C-m`, no `-l`, no `--`, no verify), sole caller at `run.rs:123`. Violates the "ONE verified send path" invariant (research §9).
- Daemon observability is tracing-based: JSON daily-rotated `daemon.<date>` under `<hangar_home>/hangar/logs` plus an OTLP seam (`ainb-hangar-daemon/src/observability.rs`), with a live pane fed by `hangar/daemon_health` and the in-memory ring in `health_stats.rs`. This is the surface the new subsystems must join (DE review 2026-08-04, see Observability).
- Spike PROVED kill-respawn-load resume for `claude-agent-acp` 0.64.0 and `codex-acp` 1.1.7; `session/update` replay arrives BEFORE the `session/load` reply (handler must be live first); codex config does not survive load (re-apply model/mode/reasoning); steering needs `idleBehavior: promptRequired` to avoid ghost detached turns. Gemini resume unverified.
- Spike SECURITY finding: `claude-agent-acp`'s `session/new` returned `currentModeId: bypassPermissions` inherited from ambient state, and as a direct consequence ZERO agent-to-client requests fired across every probe run. Trusting the default silently disables the whole permission UX that R8 exists to build.

## What we're NOT doing (part 1)

- No channels UI, threads, confirm cards, copilot service, or MCP tool server (all part 2).
- No `fleet_message_scope` membership tables, no `fleet/message_scopes`, no `deliver` flag (amputated, see above).
- No steering (`_session/steering`) in the delivery path; queue-behind-in-flight-turn only. Steering is part 2 copilot territory.
- No runtime protocol version degrade; v1 clients are refused after the bump (all clients are in-repo and ship in one train, but see Rollback and rollout for the honest version of that claim).
- No client-side transcript replay for `session/load` resume (the adapter's own store replays history; spike Q1).
- No resync notifications for message/transcript streams (graft 3).
- No Gemini/Copilot ACP adapters (matrix unverified; capability-only addition later, no bump needed thanks to graft 2).
- No per-session model/mode/reasoning settings: adapter config is static daemon config (also what Phase 6 re-applies after codex `session/load`); `fleet_acp_session` deliberately carries no config columns. Part 2 adds them if the copilot needs per-session settings.
- No automatic retention sweep. Part 1 ships the growth contract and the operator-invoked export-then-delete command; it does not ship a timer (DE review 2026-08-04, see Retention and growth).

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
│  │ SendPrompt    │               │ 1 proc/provider, │   │
│  │ action path   │               │ N sessions muxed │   │
│  └──────┬────────┘               │ SlotCircuit/proc │   │
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

## Data model (migration 0079, hangar store)

Three new tables plus one index amendment. All in `ainb-tui/crates/ainb-hangar-store/migrations/0079_chat_bus.sql`.

```sql
-- Chat messages. Scope is a minted string: "session:<key>", "broadcast:<ulid>",
-- part 2 mints "channel:<id>" without schema change.
--
-- `seq` is the ONE cursor for the message stream (DE review 2026-08-04, graft 9).
-- SQLite assigns it inside the write transaction and serialises writers, so seq
-- order IS commit order and a page-to-head forwarder cannot skip a row. `id` is
-- the stable external identity used by the wire, by threading, and by clients;
-- it is NEVER a cursor. AUTOINCREMENT (not bare rowid) so a deleted tail cannot
-- hand the same seq to a later row, matching fleet_provider_event.ingest_order.
CREATE TABLE fleet_message (
    seq         INTEGER PRIMARY KEY AUTOINCREMENT,  -- commit-ordered cursor
    id          TEXT NOT NULL UNIQUE,               -- daemon-minted ULID, stable external identity
    request_id  TEXT UNIQUE,                        -- client idempotency token (NULL for daemon-authored rows)
    request_fingerprint TEXT,                       -- stable hash of (scope_key, targets, body); replay with a different fingerprint is REJECTED (graft 4)
    scope_key   TEXT NOT NULL CHECK (length(scope_key) > 0),
    origin_message_id TEXT REFERENCES fleet_message(id),  -- replies only: the message this row answers (R7 thread join)
    sender      TEXT NOT NULL,                 -- "operator" | session_key
    kind        TEXT NOT NULL CHECK (kind IN ('user','agent','marker')),
    body        TEXT NOT NULL,
    created_at  INTEGER NOT NULL
);
CREATE INDEX idx_fleet_message_scope ON fleet_message(scope_key, seq);
CREATE INDEX idx_fleet_message_origin ON fleet_message(origin_message_id, seq)
    WHERE origin_message_id IS NOT NULL;
-- Outbound half of the Phase 6 re-prime delivery join (DE review 2026-08-04).
CREATE INDEX idx_fleet_message_sender ON fleet_message(sender, seq);

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
-- Inbound half of the Phase 6 re-prime delivery join, and every per-session
-- receipt query. Without this, a session_key lookup scans the whole PK index
-- because the PK leads with message_id (DE review 2026-08-04).
CREATE INDEX idx_fleet_message_delivery_session
    ON fleet_message_delivery(session_key, message_id);
-- Boot and runtime convergence scan for stuck legs (I7, I16).
CREATE INDEX idx_fleet_message_delivery_pending
    ON fleet_message_delivery(session_key) WHERE state = 'PENDING';

-- ACP session identity. session_key is daemon-minted and STABLE
-- ('acp:' || ulid); acp_session_id is the adapter's MUTABLE id, swapped on rebuild.
CREATE TABLE fleet_acp_session (
    session_key    TEXT PRIMARY KEY,
    scope_key      TEXT NOT NULL,
    provider       TEXT NOT NULL CHECK (length(provider) > 0),  -- adapter token; validated against the adapter registry at the RPC layer, NOT the schema (0071 `source` style), so the next adapter needs no migration
    provider_version TEXT,                     -- agentInfo version observed at the last successful initialize; NULL until first spawn (DE review 2026-08-04: the plan's top risk is npm drift, and nothing recorded which version a session was built against)
    acp_session_id TEXT,                       -- NULL until session/new succeeds
    cwd            TEXT NOT NULL,
    permission_mode TEXT NOT NULL,             -- the mode PINNED at session/new and re-asserted after load; never inherited (spike security finding, I13)
    state          TEXT NOT NULL CHECK (state IN ('ACTIVE','IDLE','EVICTED','DEAD')),
    open_turn_id   TEXT,                       -- non-NULL while a turn is in flight (boot + runtime convergence input)
    open_turn_started_at INTEGER,              -- turn deadline input (I16)
    created_at     INTEGER NOT NULL,
    last_active_at INTEGER NOT NULL
);
CREATE UNIQUE INDEX idx_fleet_acp_session_scope_active
    ON fleet_acp_session(scope_key) WHERE state IN ('ACTIVE','IDLE');
CREATE INDEX idx_fleet_acp_session_open_turn
    ON fleet_acp_session(open_turn_id) WHERE open_turn_id IS NOT NULL;

-- Keep the pending-recovery contract's PREDICATE identical so the existing
-- consumer query can still use the index, and push ACP rows out of its way with
-- the KEY instead. (DE review 2026-08-04, replaces the original
-- `WHERE ... AND source <> 'acp'` recreation.) The only reader is
-- repo/fleet_provider_event.rs:192-194, `WHERE provider = ? AND source = ? AND
-- projection_revision IS NULL`. SQLite may only use a partial index when the
-- query's WHERE provably implies the index's WHERE; `source = ?` is a bound
-- parameter, so `source <> 'acp'` is NOT provable at plan time and the scoped
-- index would simply never be chosen, degrading the recovery scan to a full
-- table scan over the table this plan is about to inflate.
DROP INDEX idx_fleet_provider_event_projection;
CREATE INDEX idx_fleet_provider_event_projection
    ON fleet_provider_event(source, provider, projection_revision)
    WHERE projection_revision IS NULL;
```

Transcript rows reuse `fleet_provider_event` as-is (graft 1): no schema change beyond the index above; every insert computes `raw_blake3`; `event_id` is a daemon-minted ULID (adapter-supplied id where one exists); cursor is `ingest_order` filtered by `session_key`.

### Session identity: `fleet_acp_session` vs `fleet_session` (normative, DE review 2026-08-04)

The original draft created only `fleet_acp_session`, yet Phase 5 promised "Fleet snapshot surfaces these sessions with `provider: Acp`". Those cannot both be true: `snapshot_wire` (`daemon/src/fleet.rs:246-251`) projects from `fleet_session`, and `daemon/src/fleet.rs:905-907` maps any unrecognised provider token to `FleetProvider::Unknown`. Two session tables with no stated relationship is also the boundary defect that makes "which table is the fleet's session?" unanswerable for every later reader.

Normative rule for part 1:

- Every ACP session gets BOTH rows, keyed by the SAME `session_key`: a `fleet_session` row (the fleet's one session identity, what the snapshot, attention, receipts and `fleet/action` all key on) and a `fleet_acp_session` row (the ACP-specific adjunct: adapter id, cwd, permission mode, open turn).
- The `fleet_session` row for an ACP session carries `provider = 'acp'`, `tmux_target = NULL`, `process_start_fingerprint = NULL`, `management_state = 'MANAGED'`, and a `capabilities` JSON that enables exactly the actions Phase 5 wires (`send_prompt`, `approvals`, `structured_answer`, `interrupt`, `stop`, `kill`) and nothing else. `action_capability` (`rpc/mod.rs:2866-2884`) gates on this JSON, so an unset flag means the action is Rejected before it ever reaches a handler.
- Because that row exists, the existing `fleet/action` paths become REACHABLE for ACP sessions. That is why the I8 trap moved into `handle_fleet_action` (graft 8) and why Phase 5 must add the ACP arms rather than assume the existing ones cover it (I3, I8).

### Scope + threading rules (R7, normative)

- **Scope grammar**: `session:<session_key>` (a session's own scope; minted by `fleet/acp_session_create` for ACP sessions, or derived from the existing tmux session_key for tmux recipients), `broadcast:<ulid>` (minted by `message_send` when it targets more than one session). Part 2 adds `channel:<id>` without schema change.
- **Direct send** (one target): the user message row carries the recipient's own scope; the reply row lands in the SAME scope with `origin_message_id` = the prompting message id.
- **Broadcast send** (N targets): the user message row carries the minted `broadcast:<ulid>` scope; each recipient's reply row lands in that RECIPIENT'S OWN scope with `origin_message_id` = the broadcast message id. Thread view = the broadcast row + `message_list {origin_id: <broadcast id>}` (I11). No parent column ambiguity: `origin_message_id` is the one and only threading join.
- **Execution routing**: an ACP recipient's prompt ALWAYS runs in the recipient's own ACP SESSION, addressed by `sessionId` on that provider's shared process (pool map `scope_key -> (provider process, acp_session_id)`, graft 6); a broadcast scope never owns a session.
- **Re-prime corpus** (Phase 6) is the DELIVERY JOIN, not a raw scope filter: messages having a `fleet_message_delivery` row for the session_key (inbound, broadcasts included) plus messages with `sender = session_key` (outbound), ordered by `seq`. A broadcast-delivered prompt is therefore never lost from a rebuilt context. The join is BOUNDED by `seq < <the in-flight prompt's seq>` (review 2026-08-07): a delivery row exists from the moment a message is QUEUED, not from the moment it is prompted, so an unbounded join renders the prompt about to be asked (and every message still queued behind it) under a header claiming all of it is earlier history, and a burst deeper than N=20 pushes the in-flight prompt out of the window entirely. The bound is the whole exclusion rule; there is no "N+1 rows then drop by id" dance.
- **Cursors are `seq`, identities are `id`** (DE review 2026-08-04). Every `after_id` parameter on the wire is resolved to its `seq` server-side and paged by `seq`; ordering is never by `id`.

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

- `fleet/acp_session_create` is R3's entry point (without it no ACP recipient can ever exist) and part of the SAME frozen v2 surface, so no post-freeze wire change: validates `provider` against the adapter registry, inserts the `fleet_session` + `fleet_acp_session` row pair per Session identity (state IDLE, `acp_session_id` NULL, `scope_key` = supplied or minted `session:<session_key>`). NO process spawn at create; the pool spawns lazily on first prompt (the NULL adapter id then routes through the Phase 6 rebuild path's `session/new` leg naturally). Idempotent per live scope: an existing ACTIVE/IDLE session for `scope_key` is returned as-is (backed by the partial unique index). `message_send` keeps its "targets must exist" rule; it never auto-provisions.
- **Limits are explicit constants.** `message_list` and `transcript_list` clamp `limit` to a named max, matching `FLEET_TIMELINE_MAX`/`FLEET_RECEIPT_LIST_MAX` (`rpc/mod.rs:1052-1053`). An unbounded page is a self-inflicted memory incident (DE review 2026-08-04).
- Capability consts are DEFINED in Phase 2 (part of the frozen surface), but each id is appended to `FLEET_PROTOCOL_CAPABILITY_IDS` only in the phase its dispatch arms land (`fleet.message.*` / `fleet.transcript.read` → Phase 3, `fleet.acp.spawn` → Phase 5), so no daemon build ever advertises a capability whose methods answer -32601.
- `FLEET_PROTOCOL_VERSION` 1 → 2, bump-and-refuse: daemon advertises only v2; clients whose declared range excludes 2 get `read_compatible/write_compatible = false` and must upgrade. All clients are in-repo and ship in the same train. `handle_fleet_negotiate` stays a stateless echo.
- `FleetProvider` grows `Acp` (the wire token for ACP-backed sessions; the concrete adapter lives in `fleet_acp_session.provider`), AND `daemon/src/fleet.rs:905-907` gains the matching `"acp" => FleetProvider::Acp` arm, without which every ACP session decodes as `Unknown` on the wire (DE review 2026-08-04).
- Swift `FleetProvider` gains `case acp` AND a tolerant `init(from:)` (unknown token → `.unknown`), so the provider after this one is capability-only.
- Note for part 2 reconciliation: part 2's draft assumed `fleet/thread_list`; part 1 ships no thread method (threading = `origin_message_id` linkage, replies land in the recipient's own scope per Scope + threading rules, R7; thread reads ride `message_list {origin_id}`). Part 2's Phase 0 gate amends its contract table against this section.

## Invariants (the test contract)

| # | Invariant | Proven by | Phase |
|---|---|---|---|
| I1 | `message_send` is idempotent by `request_id`: replay with the SAME `request_fingerprint` returns the same `message_id` and existing delivery rows, no double delivery; replay with a DIFFERENT fingerprint is rejected with `invalid_params`, mirroring `rpc/mod.rs:1676-1687` | daemon RPC test: send twice, assert one row + one tmux submit; third send reusing the id with different text is rejected | 3 |
| I2 | Subscribers recover from broadcast lag by paging-to-head: no gaps, no duplicates, no resync notification needed | forwarder test: force lag, assert contiguous `seq` sequence | 3 |
| I3 | Every delivery leg resolves to exactly one terminal state via receipt-claim/fingerprint; receipts queryable per (message, recipient); no path leaves a leg PENDING once its session is no longer running | delivery-join test incl. concurrent resolvers, plus the I16 crash/timeout cases | 3, 5, 6 |
| I4 | Chat timeline receives ONLY final agent messages; full stream lands in transcript | e2e: prompt ACP scope, assert timeline row count vs chunk count | 5 |
| I5 | `session_key` is stable across `acp_session_id` churn: rebuild swaps the adapter id, key (and receipts/scope references) unchanged | resume test: force rebuild, assert same key | 6 |
| I6 | At-most-once prompt delivery: requeue ONLY when the prompt provably never reached the adapter; otherwise terminal UNKNOWN | pool fault-injection test (kill between claim and write; kill after write) | 5 |
| I7 | Boot scan converges: open turns backfilled `acp.turn_interrupted`, stuck PENDING deliveries → UNKNOWN, dead pending-permission attention cleared | restart test against seeded dirty store | 6 |
| I8 | No ACP session ever reaches tmux send machinery: `handle_fleet_action` has an explicit ACP arm ahead of the `verified_tmux_send` fallthrough (`rpc/mod.rs:1571-1579`), so an ACP `SendPrompt` never reaches the tmux path and never returns the misleading "exact tmux process identity is unavailable" detail | unit test on the action router + integration assertion in delivery e2e (fake tmux binary on PATH records zero calls) | 5 |
| I9 | v1 client is refused post-bump (bump-and-refuse), on BOTH the read and the write leg; Swift decodes an unknown provider token to `.unknown` | Rust negotiate contract test + Swift `FleetDaemonContractTests` tolerant-decode case + a Swift test that the shipped `FleetStore` negotiation declares v2 on read AND write ranges | 2 |
| I10 | ACP transcript rows never degrade the pending-recovery contract: the partial index predicate is unchanged and the REAL consumer query (`repo/fleet_provider_event.rs:192-194`) still uses it with ACP rows present; every row has valid `raw_blake3`; `event_id` re-insert is a no-op | store repo tests + `EXPLAIN QUERY PLAN` assertion naming `idx_fleet_provider_event_projection` for the actual consumer query, run against a table seeded with ACP rows | 1, 4 |
| I11 | Broadcast replies thread (R7): each reply lands in the recipient's own scope with `origin_message_id` = the broadcast message id; `message_list {origin_id}` returns exactly the reply set | e2e: broadcast to 2 fake ACP scopes → 2 reply rows in the recipients' own scopes, origin join returns both and nothing else | 5 |
| I12 | Transcript is streamed LIVE (R4): a subscriber attached before the prompt receives chunk events during the turn, not only after turn end | e2e: fake adapter with paced chunks; assert first `transcript_event` arrives before `acp.turn_completed` | 5 |
| I13 | Permission mode is PINNED, never inherited (DE review 2026-08-04; spike security finding). Every `session/new` sends an explicit mode, the reply's `currentModeId` is asserted to equal it, a mismatch fails the spawn, and the mode is re-asserted after every `session/load`. The adapter child process gets an allowlisted environment, not the daemon's | `ainb-acp` unit test against a fake adapter that echoes a DIFFERENT mode (spawn must fail); env-allowlist unit test; `#[ignore]` real-adapter assertion that `currentModeId` is the requested mode and not `bypassPermissions` | 4, 6 |
| I14 | The message cursor is commit-ordered: under concurrent `message_send` calls a page-to-head subscriber observes every committed row exactly once (DE review 2026-08-04, graft 9) | daemon test: K concurrent sends against one pool, forwarder attached, assert the delivered `seq` set equals the committed set with no gap and no repeat; a variant that mints ULIDs out of order must still pass | 3 |
| I15 | Re-prime is injection-safe: message bodies are fenced and escaped into the prelude so no body can forge the header, terminate the fence, or impersonate a different sender (DE review 2026-08-04) | `ainb-acp` unit test with hostile bodies (fence terminator, forged header, "ignore previous instructions", null bytes, 1 MiB body) asserting the rendered prelude keeps every body inside its envelope and honours the 32 KiB cap | 6 |
| I16 | Convergence is not boot-only: an adapter process that dies mid-turn, or a turn that exceeds its deadline, converges at RUNTIME exactly as the boot scan would (turn_interrupted row, delivery resolved terminal, `open_turn_id` cleared, queued prompts given a defined outcome, scope reusable without a daemon restart). Under the multiplexed pool, process death converges EVERY session it hosted, and a per-session deadline cancel never disturbs the process's other sessions (decided 2026-08-04) | pool test: SIGKILL the shared provider process while TWO scopes have open turns, assert BOTH converge and BOTH accept a new prompt with no restart; deadline test with a fake adapter that never ends one session's turn while a second session's turn completes normally | 5, 6 |

## Phase 0: Quick win, kill the second unhardened send path

<!-- wave: 1 | depends_on: [] | files: [ainb-tui/crates/ainb-core/src/cli/run.rs, ainb-tui/crates/ainb-core/tests/tripwire_cli_run_prompt.rs] -->

Independent of everything below; restores the fleet-core "ONE verified send path" invariant (research §9).

### Changes

**File**: `ainb-tui/crates/ainb-core/src/cli/run.rs`
- [ ] Delete local `send_prompt_to_tmux` (`run.rs:418-435`: bare `send-keys ... C-m`, no `-l`, no `--` terminator, no submit verification; a prompt beginning with `-` is parsed as a tmux flag and silently corrupted)
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

## Phase 1: Store, migration 0079 + repos

<!-- wave: 1 | depends_on: [] | files: [ainb-tui/crates/ainb-hangar-store/migrations/0079_chat_bus.sql, ainb-tui/crates/ainb-hangar-store/src/repo/fleet_message.rs, ainb-tui/crates/ainb-hangar-store/src/repo/fleet_acp_session.rs, ainb-tui/crates/ainb-hangar-store/src/repo/fleet_provider_event.rs, ainb-tui/crates/ainb-hangar-store/src/repo/mod.rs, ainb-tui/crates/ainb-hangar-store/tests/migration_0079_upgrade.rs] -->

### Changes

- [ ] `migrations/0079_chat_bus.sql` exactly per the Data model section above (3 tables, their indexes, and the projection-index recreation that keeps the PREDICATE unchanged)
- [ ] New `repo/fleet_message.rs`: `insert_message` (takes `origin_message_id` and `request_fingerprint`; on `request_id` conflict, fetch the existing row and REJECT when the stored fingerprint differs, mirroring `rpc/mod.rs:1676-1687`, per graft 4), `list_by_scope(after_seq, limit)`, `list_all(after_seq, limit)`, `list_by_origin(origin_id, after_seq, limit)` (I11 thread join), `list_for_session(session_key, limit)` (the delivery-join re-prime corpus per Scope + threading rules: inbound via `fleet_message_delivery` + outbound via `sender`; Phase 6 consumer), `seq_for_id(id)` (wire `after_id` to cursor resolution), `insert_delivery`, `claim_delivery(fingerprint)` / `resolve_delivery(state, detail)` (B's receipt-claim pattern, delivery legs only), `deliveries_for_message`, `pending_deliveries_for_session` (I7/I16 convergence input)
- [ ] Every list reader pages by `seq`, never by `id` (DE review 2026-08-04, graft 9). No public repo function accepts an id as a cursor
- [ ] New `repo/fleet_acp_session.rs`: mint (`'acp:' || ulid`), insert idempotent per live scope (partial unique index; on conflict return the existing ACTIVE/IDLE row, backing `fleet/acp_session_create`), `set_acp_session_id`, `set_provider_version`, `set_state`, `set_open_turn` / `clear_open_turn` (both stamp `open_turn_started_at`), `list_dirty` (open turn or PENDING deliveries; the ONE query shared by the boot scan and the runtime convergence path per I16), `list_open_turns_older_than(deadline_ms)` (I16 deadline sweep); provider validated against the adapter registry at the RPC layer (schema only length-checks, see Data model)
- [ ] `repo/fleet_provider_event.rs`: **amend the header doc's retention contract** (`:3-25`). The measured "21 rows per 2 days, ~1.3 MB per YEAR, revisit at ~1M rows or ~100 MB" figures were taken BEFORE this table carried ACP transcripts and are no longer the operating regime; state the new regime, the new revisit trigger, and that `source='acp'` rows are the only rows eligible for the operator export-then-delete (they carry no `projection_revision` contract). See Retention and growth (DE review 2026-08-04)
- [ ] `repo/fleet_provider_event.rs`: add `list_by_session_after(session_key, after_order, limit)` reader and `delete_acp_before(session_key, ingest_order)` (the operator export-then-delete leg; never touches a row with `source <> 'acp'`, never touches a row with `projection_revision IS NULL`)
- [ ] Register modules in `repo/mod.rs`

### Success criteria

Automated:
- [ ] `cargo test -p ainb-hangar-store`
- [ ] Repo tests cover: request_id replay with a matching fingerprint returns the original row and with a differing fingerprint is rejected (I1 insert half); delivery claim is single-winner under concurrent claimers (I3); `event_id` duplicate insert is a no-op (I10); one-live-session-per-scope constraint fires and conflicting insert returns the existing live row; `list_by_origin` returns exactly the rows inserted with that origin (I11 store half); `list_for_session` includes a broadcast-delivered row and the session's own replies, excludes unrelated scopes
- [ ] I10 index test: `EXPLAIN QUERY PLAN` for the ACTUAL consumer query (`WHERE provider = ? AND source = ? AND projection_revision IS NULL`) names `idx_fleet_provider_event_projection`, asserted against a table seeded with both ACP and non-ACP rows. Asserting the index EXISTS is not enough; the original design would have passed that and still full-scanned (DE review 2026-08-04)
- [ ] `delete_acp_before` refuses a non-ACP row and refuses a row with `projection_revision IS NULL`
- [ ] New `tests/migration_0079_upgrade.rs` per the repo's populated-database convention (`tests/migration_0050_upgrade.rs` is the model): migration applies cleanly to a snapshot holding pre-existing `fleet_provider_event` rows, the dropped index is recreated, and the consumer query still plans onto it

Manual:
- [ ] None (pure store phase)

---

## Phase 2: Fleet protocol version bump (v1 → v2), the ONE bump

<!-- wave: 2 | depends_on: [1] | files: [ainb-tui/crates/ainb-hangar-proto/src/fleet.rs, ainb-tui/crates/ainb-hangar-proto/src/methods.rs, ainb-tui/crates/ainb-hangar-daemon/src/fleet.rs, ainb-tui/crates/ainb-hangar-daemon/tests/dispatch_routing.rs, apps/ainb-fleet-macos/Sources/FleetRPC/FleetWire.swift, apps/ainb-fleet-macos/Sources/FleetRPC/FleetConnection.swift, apps/ainb-fleet-macos/Sources/App/FleetStore.swift, apps/ainb-fleet-macos/Tests/FleetRPCTests] -->

R6: one bump carries Provider growth AND the message family. Nothing else ever rides it.

### Changes

**Proto** (`ainb-hangar-proto/src/fleet.rs`):
- [ ] `FLEET_PROTOCOL_VERSION` 1 → 2 (`fleet.rs:9`)
- [ ] `FleetProvider` grows `Acp` (`fleet.rs:93-98`)
- [ ] New capability consts DEFINED (`fleet.rs:12-37` pattern): `fleet.message.send`, `fleet.message.read`, `fleet.transcript.read`, `fleet.acp.spawn`. NOT appended to `FLEET_PROTOCOL_CAPABILITY_IDS` yet: each id is advertised only in the phase its dispatch arms land (message/transcript → Phase 3, acp.spawn → Phase 5), so a daemon built between phases never advertises a capability whose methods answer -32601 (the consts are still part of the frozen v2 surface)
- [ ] Named page-size maxima for the two new list methods, matching `FLEET_TIMELINE_MAX` (DE review 2026-08-04)
- [ ] Typed params/results for the 6 methods in the Wire contract section + `fleet/message_event` / `fleet/transcript_event` notification payloads; serde round-trip tests per existing fleet contract-test pattern

**Daemon provider mapping** (DE review 2026-08-04):
- [ ] `daemon/src/fleet.rs:905-907`: add `"acp" => wire::FleetProvider::Acp`. Without this arm the snapshot renders every ACP session as `Unknown` no matter what the store holds, and the Phase 5 promise "Fleet snapshot surfaces these sessions with `provider: Acp`" is silently false

**Methods** (`ainb-hangar-proto/src/methods.rs`, append-only in all 3 places):
- [ ] Consts `FLEET_ACP_SESSION_CREATE`, `FLEET_MESSAGE_SEND`, `FLEET_MESSAGE_LIST`, `FLEET_MESSAGE_SUBSCRIBE`, `FLEET_TRANSCRIPT_LIST`, `FLEET_TRANSCRIPT_SUBSCRIBE` with doc comments, appended at the `ALL_METHODS` tail (the fleet block ends at `methods.rs:1660`)
- [ ] Mirror entries in the `declared` list of `all_methods_covers_every_const` (`methods.rs:1818-1951`)
- [ ] (Dispatch arms land in Phases 3 and 5; until then the daemon answers -32601, which is correct pre-handler AND unadvertised per the capability rule above)

**Backend trap** (graft 8, RETARGETED by DE review 2026-08-04):
- [ ] Do NOT change `Backend::from_provider` (`runner.rs:611`). The original task cited `materialise.rs:97`, which is a different function (`ProviderSkillLayout::from_provider`, falls back to `GeminiOrDefault`); the real `Backend::from_provider` has one production caller, `resolve_dispatch` at `run_loop.rs:2448`, which resolves an AGENT's provider for issue dispatch and never sees a chat session. Making it fallible would also reverse the invariant deliberately pinned at `dispatch_routing.rs:557-560` ("a genuinely not-wired / misconfigured provider must still dispatch to the safe default rather than strand the task") for callers that have nothing to do with ACP
- [ ] The equivalent trap is an explicit ACP arm in `handle_fleet_action` and lands with the ACP session rows in Phase 5 (I8). Phase 2 keeps only the unit assertion that the ACP token is NOT a tmux-routable provider

**Swift** (bump PR, graft 2):
- [ ] `FleetWire.swift:124`: `FleetProvider` gains `case acp` and a tolerant `init(from:)` decoding unknown raw values to `.unknown`
- [ ] Client protocol range advertises v2 on BOTH legs. Concretely: `FleetConnection.swift:103-104` defaults move to `(min: 1, max: 2)`, AND `FleetStore.swift:101` (the stored `readVersions`) plus `FleetStore.swift:386` (the `negotiate(readVersions:)` call, which today never passes `writeVersions` at all) are updated to declare the write range too. Missing the write leg is silent: the app connects, reads fine, and every action/broadcast/start throws at `requireWriteCapability` (`FleetConnection.swift:422`) (DE review 2026-08-04)
- [ ] `FleetDaemonContractTests`: tolerant-decode case (unknown token → `.unknown`) + negotiate-v2 fixture + an assertion on the ranges the SHIPPED `FleetStore` actually declares, not on the `FleetConnection` defaults alone (I9)

### Success criteria

Automated:
- [ ] `cargo test -p ainb-hangar-proto -p ainb-hangar-daemon`
- [ ] Registry-drift guard green (`all_methods_covers_every_const`), namespacing test green
- [ ] `swift test --package-path apps/ainb-fleet-macos` (contract suite incl. new cases); `swift-contract-paths` CI workflow triggers on this diff (it watches fleet proto paths per commit 79cc417b)
- [ ] Negotiate contract test: client declaring read/write range max 1 gets `read_compatible=false, write_compatible=false` (I9)
- [ ] Swift test: the app's own negotiation declares max 2 on read AND write, so no write path is left silently incompatible (I9, DE review 2026-08-04)
- [ ] insta snapshot churn reviewed: the v2 bump + ALL_METHODS growth dents inline snapshots across hangar-daemon/store/core (events.rs, fleet.rs, runner.rs, etc.); run `cargo insta review`, accept only the expected dents, and call out snapshot-only diffs in the PR so reviewers can tell intentional updates from drift

Manual:
- [ ] Existing macOS Fleet app build from the same train connects, negotiates v2, AND successfully performs one write action (send prompt to a tmux session). Connecting is not sufficient evidence, see the write-leg trap above

### Checkpoint
- **`[CHECKPOINT:human-verify]`**: Wire contract freeze. What was built: v2 proto surface (methods, capabilities, provider token, notifications). How to verify: read the fleet.rs/methods.rs diff + Swift contract test fixtures; confirm part 2's Phase 0 gate can reconcile against it; confirm the Rollback and rollout section's back-out procedure has been walked once on a scratch database. Resume: "approved" or name the change. After this checkpoint the v2 surface is append-only.

---

## Phase 3: Daemon chat bus, live on tmux sessions

<!-- wave: 3 | depends_on: [1, 2] | files: [ainb-tui/crates/ainb-hangar-daemon/src/rpc/mod.rs, ainb-tui/crates/ainb-hangar-daemon/src/events.rs, ainb-tui/crates/ainb-hangar-daemon/src/lib.rs, ainb-tui/crates/ainb-hangar-proto/src/fleet.rs] -->

The bus ships useful WITHOUT any ACP code: `message_send` to N tmux sessions is broadcast-with-history (research §7: "broadcast becomes message to N sessions"). This is the phase that makes part 1 incrementally shippable.

### Changes

**Events** (`events.rs`):
- [ ] Two new wakeup channels on `EventBroker`, mirroring `fleet_tx`: `message_tx: broadcast<i64>` (committed `seq`, NOT the ULID, per graft 9) and `transcript_tx: broadcast<(String, i64)>` (session_key, ingest_order). Durable rows are the source of truth; channels only wake forwarders (pattern at `events.rs:87,171-173`)

**Handlers** (`rpc/mod.rs`, free-fn pattern per `handle_fleet_timeline` at `:1056-1105`; dispatch arms appended in the fleet block `:920-931`):
- [ ] `handle_fleet_message_send`: parse + validate targets exist and are tmux-backed (ACP targets arrive in Phase 5; unknown target → per-delivery REJECTED, not request failure); idempotent insert by `request_id` with `request_fingerprint` mismatch rejected (I1); insert PENDING delivery rows; per recipient, run the EXISTING SendPrompt fleet action path (same receipts/fingerprint machinery, `fleet-core` verified tmux send); resolve delivery DELIVERED on verified submit, FAILED on error; emit `message_tx` wakeup AFTER commit, carrying the committed `seq`
- [ ] `handle_fleet_message_list`: cursor page by `(scope_key?, origin_id?, after_id, limit)` where `after_id` is resolved to its `seq` and paging is by `seq` (`origin_id` = the I11 thread join); `limit` clamped to the named max
- [ ] `handle_fleet_message_subscribe`: register receiver BEFORE reading head (the `pending_fleet_rx` ordering trick at `rpc/mod.rs:364-366`), ack `{head_id}`, then `spawn_message_forwarder` modeled on `spawn_fleet_forwarder` (`rpc/mod.rs:567-610`) with ONE difference (graft 3): on broadcast lag, page-to-head from the cursor and continue; no resync notification, no exit
- [ ] `handle_fleet_transcript_list` / `handle_fleet_transcript_subscribe`: same shapes over `fleet_provider_event` filtered `session_key`, cursor `ingest_order` (readers exist from Phase 1; rows appear in Phase 5; empty until then is fine and testable). The transcript forwarder filters on its own `session_key` BEFORE issuing any query, so an unrelated session's chunk costs a wakeup and nothing more (DE review 2026-08-04: `transcript_tx` is a single unfiltered broadcast, so every transcript subscriber wakes on every session's chunk)
- [ ] Capability gating on all five message/transcript handlers, per existing write-surface pattern
- [ ] Append `fleet.message.send`, `fleet.message.read`, `fleet.transcript.read` to `FLEET_PROTOCOL_CAPABILITY_IDS` (`fleet.rs`), in the SAME change as the dispatch arms (consts were defined in Phase 2; advertisement deliberately deferred to here so no daemon build advertises -32601 methods)
- [ ] Tracing spans + counters per the Observability section (DE review 2026-08-04)
- [ ] CLI verbs `ainb fleet msg send|list|follow` per the CLI surface section (same PR as the dispatch arms; contract tests + `docs/tui/cli.md`)
- [ ] Unknown `after_id` semantics pinned (review 2026-08-05): an `after_id` that does not resolve via `seq_for_id` is `invalid_params`, never treated as start-of-log; contract test for both list and subscribe
- [ ] Pre-migration auto-backup (review 2026-08-05, turns the Rollback section's documented file-restore from procedure into guarantee): before `apply_migrations` runs any PENDING migration, the daemon copies `hangar.db` (WAL checkpointed first) to `hangar.db.pre-<version>.bak`, keeping the newest N=2 backups; test: seeded store + pending migration produces the backup, re-boot with nothing pending does not

**Wiring** (`lib.rs`):
- [ ] Broker construction + forwarder spawn parity with fleet stream (boot wiring at `lib.rs:422-432` pattern)

### Success criteria

Automated:
- [ ] `cargo test -p ainb-hangar-daemon`
- [ ] I1 test: double `message_send` with same `request_id` and same content → one message row, one tmux submit, identical response; same `request_id` with different content → `invalid_params`
- [ ] I2 test: subscriber under forced broadcast lag receives a contiguous, duplicate-free `seq` sequence via page-to-head
- [ ] I14 test: K concurrent `message_send` calls with a subscriber attached deliver every committed row exactly once. Include a variant that forces ULID minting out of commit order, which must still pass (DE review 2026-08-04; this is the test that fails against the original ULID-as-cursor design)
- [ ] I3 test: N-target send yields N delivery rows, each resolving exactly once; receipts queryable per (message, recipient)
- [ ] Capability-gate test: connection without `fleet.message.send` gets the standard capability error
- [ ] `limit` clamp test: an oversized `limit` is clamped, not honoured
- [ ] insta snapshot churn reviewed: capability-list growth dents inline snapshots; `cargo insta review`, snapshot-only diffs called out in the PR (same rule as Phase 2)

Manual:
- [ ] From a v2 client: send one message to 3 running tmux sessions; all 3 receive it (verified submit), `message_list` shows the row, deliveries show 3 terminal states

---

## Phase 4: `ainb-acp` crate (client + transcript reducer)

<!-- wave: 3 | depends_on: [1] | files: [ainb-tui/crates/ainb-acp/**, ainb-tui/Cargo.toml, .github/workflows/**] -->

Pure library phase, shippable with zero daemon wiring. Parallel with Phase 3 (no file overlap).

### Changes

**New crate** `ainb-tui/crates/ainb-acp/` on upstream `agent-client-protocol` v1.x (pinned; do NOT hand-roll the protocol, buzz predates the crate):
- [ ] `client.rs`: spawn adapter by name (`claude-agent-acp`, `codex-acp`), `initialize` (pin protocolVersion 1; record `agentInfo` name + version and persist it to `fleet_acp_session.provider_version`), `session/new`, `session/prompt`, `session/cancel`, `session/load`; notification handler registered and routing BEFORE any `session/load` is issued (spike: replay arrives before the load reply; "the single most likely implementation bug in the port")
- [ ] `client.rs` permission-mode pinning (I13, DE review 2026-08-04: promoted from a prose bullet to a tested invariant because the spike showed the default silently disables the entire R8 permission surface). `session/new` ALWAYS carries an explicit mode from daemon config, the reply's `currentModeId` is compared to the requested value, and a mismatch is a hard spawn failure with a typed error, not a warning. The same assertion runs after `session/load`. Name the default mode value in daemon config; do not leave it implicit
- [ ] `client.rs` child-process environment hygiene (I13, DE review 2026-08-04): the adapter is spawned with an ALLOWLISTED environment (PATH, HOME, the adapter's own credential path, explicit daemon-config passthroughs), never `Command::env_clear`-less inheritance of the daemon's environment. The spike's `bypassPermissions` leak was ambient-state inheritance; nothing in the original plan stopped the daemon's whole environment reaching every adapter child
- [ ] `client.rs` never fires and forgets: every request's reply is inspected, and a `-32602` is surfaced as a typed error rather than swallowed (spike: `session/set_config_option` takes `configId`, not `optionId`, and the wrong name silently continues on the default model)
- [ ] `reducer.rs`: `session/update` stream → normalized `TranscriptChunk { kind: message|thought|tool_call|plan|permission|usage, ... }`; final-message extraction for the timeline (R4); chunk coalescing per graft 5 (contiguous same-kind text merged, flush at 4 KiB or kind boundary), unit-tested against recorded update streams
- [ ] `store_writer.rs`: chunks → `fleet_provider_event` rows (`source='acp'`, `event_type='acp.<kind>'`, `event_id` = adapter id else minted ULID, `raw_blake3` computed, `session_key` from `fleet_acp_session`); daemon-minted lifecycle rows `acp.turn_started/turn_completed/turn_failed/turn_interrupted/context_rebuilt` (B's markers). Pure library, no EventBroker access: each commit RETURNS the committed `(session_key, ingest_order)` high-water mark; the Phase 5 pool transcript pump owns the `transcript_tx` emit
- [ ] `store_writer.rs` commit cadence (DE review 2026-08-04): flush on whichever comes first, a 4 KiB coalesced batch or a configurable interval (default 250 ms), and never one transaction per chunk. Coalescing bounds ROW count; only a cadence bounds COMMIT count, and every commit takes the single SQLite write lock shared with the fleet event log, the claim loop and the outbox drain (`store.rs:77-90`, whose own comment warns a contended writer can exhaust its 10 s `busy_timeout`)
- [ ] `reprime.rs` (moved out of the pool so it is unit-testable without a process, I15): renders the fixed header + fenced, escaped corpus rows with the 32 KiB cap. Bodies are untrusted text, including agent-authored replies, and part 2 hands the copilot destructive tools, so a body must not be able to forge the header, close the fence, or impersonate another sender
- [ ] `circuit.rs`: SlotCircuit verbatim from B (per-process crash breaker, jittered exponential backoff; adapt buzz `lib.rs:1027-1136` pattern)
- [ ] Real-adapter integration tests behind `#[ignore]` + env gate (spike probes promoted; disclosure comment real-adapter vs fixture per house rule)

**CI** (graft 8, A's step; coupling enforcement added 2026-08-05):
- [ ] Add `-p ainb-acp` to the workspace test lane
- [ ] Verify-by-forced-failure: one throwaway commit with a failing `ainb-acp` test proving the lane actually executes it; revert before merge, link the red run in the PR description
- [ ] Coupled-lane path filters: any change under `ainb-tui/crates/ainb-hangar-store/**` (migrations especially) triggers the daemon lane, the `ainb-acp` lane, AND the Swift contract lane (`swift-contract-paths` precedent, commit 79cc417b); any change under `ainb-tui/crates/ainb-acp/**` triggers the daemon lane. Verified by the same forced-failure trick, once per new filter rule
- [ ] Store-fence test (structural, not grep): a workspace test parses `cargo metadata` and asserts `sqlx` appears in the dependency set of `ainb-hangar-store` ONLY. Repo functions are the single door to the store; a crate reaching for raw SQL fails CI with a message pointing at this rule

### Success criteria

Automated:
- [ ] `cargo test -p ainb-acp` (reducer + coalescing + writer against a fake adapter binary speaking scripted ndjson)
- [ ] I10 writer half: every row has 64-char `raw_blake3`; duplicate `event_id` no-op
- [ ] I13: fake adapter echoing a different `currentModeId` fails the spawn; env allowlist drops a planted ambient variable
- [ ] I15: hostile-body corpus renders inside its envelope and honours the byte cap
- [ ] Commit-cadence test: a 500-chunk scripted turn produces a bounded number of transactions, not one per chunk
- [ ] `cargo test -p ainb-acp -- --ignored` green locally against real `claude-agent-acp` 0.64.0 and `codex-acp` 1.1.7 (documented in PR, not CI-required), including the assertion that the negotiated mode is the requested one and not `bypassPermissions`
- [ ] Red CI run linked, proving the `-p ainb-acp` lane fires
- [ ] Red runs linked for each new coupled-lane path filter (store change fires daemon + acp + Swift lanes)
- [ ] Store-fence test green; a scratch commit adding `sqlx` to `ainb-acp`'s Cargo.toml turns it red (reverted before merge)

Manual:
- [ ] None (library phase)

---

## Phase 5: AgentPool + ACP delivery leg + actionable permissions

<!-- wave: 4 | depends_on: [3, 4] | files: [ainb-tui/crates/ainb-hangar-daemon/src/acp_pool.rs, ainb-tui/crates/ainb-hangar-daemon/src/rpc/mod.rs, ainb-tui/crates/ainb-hangar-daemon/src/lib.rs, ainb-tui/crates/ainb-hangar-proto/src/fleet.rs] -->

### Changes

**Session create** (R3's entry point, per the Wire contract and Session identity sections):
- [ ] `handle_fleet_acp_session_create`: validate `provider` against the adapter registry (`claude-agent-acp`, `codex-acp`); insert the `fleet_session` + `fleet_acp_session` PAIR under one `session_key` in one transaction per Session identity (state IDLE, `acp_session_id` NULL, `permission_mode` from daemon config, `scope_key` supplied or minted `session:<session_key>`, `capabilities` JSON enabling exactly the wired actions); idempotent per live scope (existing ACTIVE/IDLE row returned); NO process spawn here, the pool spawns lazily on first prompt
- [ ] Dispatch arm + append `fleet.acp.spawn` to `FLEET_PROTOCOL_CAPABILITY_IDS` (advertisement lands with its handler, per the Phase 2 capability rule)

**Fleet action arms for ACP sessions** (DE review 2026-08-04, I3/I8; this work was implied by R8's "no new method, reuse `fleet/action`" but was not a task, and the existing code does NOT accept an ACP session today):
- [ ] Explicit ACP arm in `handle_fleet_action` ahead of the `verified_tmux_send` fallthrough (`rpc/mod.rs:1571-1579`). Today a non-codex provider's `SendPrompt` falls into the tmux path; it fails safe only because `tmux_target` is NULL (`rpc/mod.rs:2589-2605`) and it reports the misleading detail "exact tmux process identity is unavailable"
- [ ] `Approve` / `Deny` / `StructuredAnswer` arms for the ACP provider. Today these are guarded `if session.provider == "claude"` and everything else falls to `(Unknown, "authoritative provider request transport is not active")` at `rpc/mod.rs:1614-1619`, so R8's answer path would return Unknown for every ACP permission without this work
- [ ] `Interrupt` / `Stop` / `Kill` arms for the ACP provider, routed to `session/cancel` plus pool teardown. Without them the ONLY way to unstick a wedged ACP turn is a daemon restart (I16)
- [ ] Confirm the `capabilities` JSON written at create enables exactly these actions, since `action_capability` (`rpc/mod.rs:2866-2884`) rejects any action whose flag is unset before the handler ever runs

**Pool** (new `ainb-hangar-daemon/src/acp_pool.rs`, graft 6 as DECIDED 2026-08-04: multiplexed):
- [ ] One adapter process PER PROVIDER (spawned lazily on first prompt for that provider); sessions multiplexed on it via ACP `sessionId`, pool map `scope_key -> (provider process, acp_session_id)`; broadcast scopes NEVER own a session (Scope + threading rules)
- [ ] Session isolation on the shared process: `session/update` notifications are demultiplexed by their `sessionId` to the owning scope's reducer; a chunk carrying an unknown `sessionId` is logged and dropped, never attributed to another scope
- [ ] Transcript pump (owns R4's live-stream leg, I12): per-SESSION pump task consumes that session's reducer chunks → `store_writer` commit → emits `transcript_tx (session_key, ingest_order)` after EVERY committed batch, using the high-water mark `store_writer` returns
- [ ] Cap N concurrent SESSIONS per provider process (config, default 16) + session-level LRU idle eviction: evict = `session/close` + `fleet_acp_session.state = EVICTED`; the provider process stays warm; `session_key` survives for Phase 6 resume. A provider process with zero live sessions is stopped after an idle window (config, default 10 min)
- [ ] Per-scope FIFO queue, ONE prompt in flight per scope; mid-turn arrivals queue (no steering in part 1). The queue is BOUNDED (config, default 32) and a send to a full queue is rejected per-delivery as REJECTED with a detail, never silently dropped and never unbounded (DE review 2026-08-04). Concurrent turns across DIFFERENT scopes on one process are allowed; a per-process in-flight ceiling (config, default 4) bounds interleaving
- [ ] At-most-once retry (B's rule, I6): requeue only if the prompt provably never reached the adapter (stdin write failed before flush); after write, outcome is turn-end or UNKNOWN, never a blind resend
- [ ] SlotCircuit wraps each PROVIDER PROCESS; breaker-open provider fails deliveries fast with FAILED + detail for every scope routed to it
- [ ] **Runtime convergence on process exit** (I16, DE review 2026-08-04; blast radius widened by the multiplex decision): a provider process that exits runs the SAME convergence routine the boot scan runs (Phase 6 owns the shared function) for EVERY session it hosted. Concretely, per affected session: write `acp.turn_interrupted` if a turn was open, resolve the in-flight delivery to a terminal state per the I6 rule, clear `open_turn_id`, give every queued prompt a defined outcome (fail-fast FAILED with detail while the breaker is open; requeue only under the I6 rule). Sessions recover on next prompt via the Phase 6 resume routine (`session/load` proven by spike). Without this, one adapter crash wedges every scope on that provider until someone restarts the daemon
- [ ] **Turn deadline** (I16): a configurable per-turn wall-clock deadline (default 30 min) sweeps `open_turn_started_at`; on expiry issue `session/cancel` for THAT session only, resolve the delivery UNKNOWN with detail `turn_deadline`, and converge the scope; the shared process and its other sessions are untouched. An adapter that never ends its turn must not be able to block a scope forever
- [ ] Pool observability surface per the Observability section: provider process count + state, sessions live/idle/evicted per provider, per-scope queue depth, per-process in-flight count, oldest in-flight turn age, breaker state per provider

**Delivery leg** (`rpc/mod.rs` `handle_fleet_message_send` extension):
- [ ] ACP-backed recipients accepted (`targets` must exist in `fleet_acp_session`, minted via `fleet/acp_session_create`; no auto-provision); `session/prompt` dispatched via the pool to the RECIPIENT'S OWN scope process (Scope + threading rules; a broadcast scope never spawns a process); delivery stays PENDING at write-ack and resolves at TURN END (`acp.turn_completed` → DELIVERED, `acp.turn_failed` → FAILED; C-defect 5 fix), through the Phase 1 claim/resolve receipt path
- [ ] On turn end, reducer's final message inserted as `fleet_message {sender: session_key, kind: 'agent', scope_key: recipient's own scope, origin_message_id: the prompting message id}` (R7/I11: direct replies share the scope, broadcast replies thread via `origin_message_id`) + `message_tx` wakeup; full stream already flowing to `fleet_provider_event` + `transcript_tx` via the pool pump (I4, I12)
- [ ] Integration assertion (I8): delivery e2e asserts zero tmux invocations for ACP recipients (fake tmux binary on PATH recording calls)

**Permissions** (R8, B retained):
- [ ] `session/request_permission` → attention row (APPROVAL/ASK) + fleet event carrying option ids and the pending JSON-RPC request id
- [ ] Answering rides the EXISTING `fleet/action` Approve/Deny/StructuredAnswer with fingerprint staleness (no new method), THROUGH the new ACP arms above; the daemon routes the answer back to the adapter's pending JSON-RPC id
- [ ] A pending permission whose adapter process dies is resolved by the convergence routine, not left as a ghost attention row (I7/I16)
- [ ] Fleet snapshot surfaces these sessions with `provider: Acp` (this works only because of the `fleet_session` row from Session identity and the `"acp"` mapping arm added in Phase 2)
- [ ] CLI verbs `ainb fleet acp create` + `ainb fleet transcript [--follow]` per the CLI surface section (same PR as the dispatch arms)

### Success criteria

Automated:
- [ ] `cargo test -p ainb-hangar-daemon`
- [ ] I4 e2e (fake adapter): one prompt → timeline gets exactly the final message; transcript gets all chunks in order
- [ ] I6 fault injection: kill process between claim and stdin write → requeued once; kill after write → UNKNOWN, no resend
- [ ] I16 crash convergence (multiplexed): SIGKILL the shared provider process while TWO scopes have open turns; assert `acp.turn_interrupted` written for both, both deliveries terminal, `open_turn_id` cleared on both, queued prompts have defined outcomes, and BOTH scopes accept a fresh prompt with no daemon restart
- [ ] I16 deadline (per-session): fake adapter where session A never ends its turn while session B completes normally on the same process → A resolves UNKNOWN with detail `turn_deadline` and `session/cancel` carried A's sessionId; B's turn is untouched
- [ ] Session demux test: interleaved `session/update` streams for two sessions on one scripted process land in the correct transcripts with zero cross-attribution; an unknown sessionId chunk is dropped and logged
- [ ] I8 integration: ACP delivery run records zero tmux calls; unit test that an ACP `SendPrompt` action never reaches `verified_tmux_send`
- [ ] Permission answer on an ACP session returns a real outcome, not the `"authoritative provider request transport is not active"` Unknown the current code produces for a non-claude provider
- [ ] I11 e2e: broadcast to 2 fake ACP scopes → 2 reply rows, each in its recipient's own scope with `origin_message_id` = the broadcast message id; `message_list {origin_id}` returns exactly those two
- [ ] I12 e2e: subscriber attached before the prompt receives `transcript_event` chunks DURING the fake-adapter turn (first chunk before `acp.turn_completed`), proving the pump's live leg
- [ ] `acp_session_create` tests: unknown provider rejected; double create for the same scope_key returns the same `session_key`; both rows written under one key; capability JSON matches the wired action set; capability-gated
- [ ] Bounded-queue test: filling the per-scope queue yields REJECTED deliveries with a detail, not unbounded growth
- [ ] LRU eviction test: N+1 sessions on one provider → oldest idle session closed (`session/close`), state EVICTED, key intact, process still warm; zero-session process stops after the idle window

Manual:
- [ ] From a v2 client: `fleet/acp_session_create {provider: claude, cwd}` → `message_send` to the returned `session_key`: reply appears in `message_list`, `transcript_list` shows thought/tool chunks, a permission request is answerable via `fleet/action`, the session appears in the fleet snapshot as `acp`, and `fleet/action Interrupt` visibly stops a long turn

---

## Phase 6: Resume + convergence (boot and runtime)

<!-- wave: 5 | depends_on: [5] | files: [ainb-tui/crates/ainb-hangar-daemon/src/acp_pool.rs, ainb-tui/crates/ainb-hangar-daemon/src/lib.rs, ainb-tui/crates/ainb-acp/src/client.rs, ainb-tui/crates/ainb-acp/src/reprime.rs] -->

GATE before starting: re-read [spike report, discussion #570 comment](https://github.com/stevengonsalvez/agents-in-a-box/discussions/570#discussioncomment-17880848) and re-run its probe scripts against the adapter versions actually pinned at implementation time (npm drift). The resume routine below must hold for the re-checked matrix. Record the observed `agentInfo` version into `fleet_acp_session.provider_version` so a later resume can tell whether the adapter changed underneath it.

### Changes

**Resume routine** (pool spawn-for-existing-`session_key`; R5, must not DEPEND on `session/load`):
- [ ] Probe per spawn (no persisted `can_load`, B-defect 5): adapter advertises `loadSession` AND `acp_session_id` is non-NULL → attempt `session/load` with the notification handler live FIRST (spike); on success, BOTH adapters (gate re-run 2026-08-06: claude-agent-acp 0.65.0 ALSO reverts to ambient mode on load, the original codex-only scoping was an artifact of the untested claude case): re-apply mode/model/reasoning FROM STATIC DAEMON ADAPTER CONFIG, which is the source of truth in part 1 (spike: config does not survive load; the same config was applied at `session/new`, so re-applying it restores the pre-load state exactly; no per-session config columns exist by design, see "What we're NOT doing"), mark path `loaded`
- [ ] Re-assert the pinned permission mode after every successful load and fail the spawn on mismatch (I13). Codex config demonstrably does not survive load, and the permission mode is the one config whose silent loss disables R8 entirely
- [ ] The load is probed on the FIRST attach attempt only; the one legal I6 requeue skips it and rebuilds (review 2026-08-07). A load failure that is not provably unknown-session deliberately leaves `acp_session_id` in place (rebuilding would throw away the adapter-side history the load exists to recover) and NOTHING else ever clears it: not convergence, not teardown. Retrying the same load on attempt two therefore made an adapter whose replay is slower than the spawn timeout fail every prompt forever, with no operator path back. Losing adapter-side history to a re-primed context is recoverable; a scope that can never take another prompt is not
- [ ] Any load failure, missing capability, or NULL adapter id → rebuild: `session/new` → store new `acp_session_id` (SAME `session_key`, I5) → re-prime prompt rendered by `reprime.rs` = fixed header + last N=20 rows from `list_for_session(session_key, before_seq)` where `before_seq` is the IN-FLIGHT prompt's `seq` (the delivery-join corpus per Scope + threading rules: inbound deliveries INCLUDING broadcast-delivered prompts + the session's own replies, ordered by `seq`; never a raw scope filter; never unbounded, see the seq bound there), each body fenced and escaped (I15), 32 KiB byte cap, oldest dropped first (graft 7, deterministic and testable)
- [ ] A session with no history BELOW that bound is `RESUME_FRESH`, not `reprimed`: a first-ever turn that happens to arrive in a burst has nothing to rebuild, and a `context_rebuilt{reprimed}` marker for it is a false claim in the transcript (review 2026-08-07)
- [ ] Either path: `context_rebuilt {mode: loaded|reprimed}` marker row into the transcript; next delivery's receipt `detail` carries the path fingerprint (B retained)

**Convergence routine** (B §7 retained, generalised by DE review 2026-08-04):
- [ ] ONE shared `converge_dirty_session(session_key)` function, called from BOTH the daemon boot sequence (`lib.rs`) and the pool's process-exit and deadline paths (Phase 5). The original plan defined this work only at boot, which meant a mid-life adapter crash left the scope wedged until a restart (I16)
- [ ] Sessions with `open_turn_id` set → insert `acp.turn_interrupted` transcript row, clear open turn
- [ ] Deliveries stuck PENDING whose responder no longer exists → UNKNOWN with detail (`daemon_restart` at boot, `adapter_exit` or `turn_deadline` at runtime)
- [ ] Pending-permission attention rows whose ACP responder no longer exists → resolved/cleared + fleet event (the poison A leaves forever)
- [ ] All idempotent: running it twice, or at boot after a runtime run, changes nothing

### Success criteria

Automated:
- [ ] `cargo test -p ainb-hangar-daemon -p ainb-acp`
- [ ] I5: forced rebuild swaps `acp_session_id`, `session_key` and existing delivery rows untouched
- [ ] Re-prime determinism: fixed corpus (including one broadcast-delivered row, proving the delivery join) → byte-identical prelude; 21st message dropped; 32 KiB cap enforced
- [ ] I15: hostile bodies in the corpus cannot forge the header or escape the fence
- [ ] I7: seeded dirty store (open turn + PENDING deliveries + orphan permission attention) → boot converges; second boot changes nothing
- [ ] I16: the same seeded dirty state converges identically when driven by the runtime path, proving one shared routine and not two drifting copies
- [ ] I13: a load that returns a different `currentModeId` fails the spawn
- [ ] `#[ignore]` real-adapter test: secret-word resume, SIGKILL daemon + adapter mid-conversation, restart, `session/load` path recalls the word (claude + codex)
- [ ] `#[ignore]` real-adapter test: force load-failure (fabricated adapter id) → re-prime path still yields a contextful answer; `context_rebuilt {mode: reprimed}` present

Manual:
- [ ] Kill the daemon mid-turn on a live chat, restart, continue the conversation from a v2 client; attention list carries no ghost permission items
- [ ] Kill only the ADAPTER mid-turn (daemon stays up); the scope recovers and accepts the next message with no daemon restart (I16)

### Checkpoint
- **`[CHECKPOINT:human-verify]`**: Part 1 exit review. What was built: chat bus (tmux + ACP legs), transcripts, permissions, resume, convergence. How to verify: run the Live E2E smoke below, then the Phase 5 and 6 manual steps; walk the Operational runbook's three questions against a live daemon; confirm part 2's Phase 0 gate reconciles cleanly against the landed contract. Resume: "approved" unlocks part 2 implementation phases.

## Live E2E smoke (tmux-status style, added 2026-08-06 per Stevie)

One scripted, repeatable, whole-system validation run against REAL processes, the tmux-verify discipline applied to the chat bus. Lands with Phase 6 and runs at the exit checkpoint (then stays as the release smoke).

- [x] Script `ainb-tui/scripts/chat-bus-smoke.sh`: scratch hangar home + private tmux server (`TMUX_TMPDIR`, `TMUX` removed, exact-name cleanup only), real `ainb-hangar-daemon`, 3 real tmux sessions running the fake-agent harness from `tripwire_cli_run_prompt.rs`. Registration is the REAL path: the panes run an agent process named `claude`, and the daemon's own tmux reconciler discovers them, so nothing is seeded into the store
- [x] Journey `j1`, bus on tmux: `ainb fleet msg send --target <3 sessions>` lands verbatim in all 3 panes (capture-pane assertion), deliveries all DELIVERED, `msg list` shows the row, `msg follow` in a side process observed the event
- [x] Journey `j2`, ACP: `ainb fleet acp create` + `msg send` to it; a chunk is readable from `transcript --follow` while the leg is still PENDING and before `acp.turn_completed`; timeline gets exactly the final message. Real adapter when installed AND credentialled, `fake_acp_adapter` otherwise, mode disclosed in the banner
- [x] Journey `j3`, resume: SIGKILL the daemon mid-turn, restart, same conversation continues (`acp.context_rebuilt {loaded|reprimed}` marker in the transcript); no ghost attention rows. SKIPS WITH A REASON on a daemon without the Phase 6 resume routine (probe: the re-prime prelude marker is dead-stripped from a pre-Phase-6 binary), and flips to a real run the day it lands, with no edit to the script
- [x] Journey `j4`, convergence: SIGKILL only the adapter process; scope accepts the next message with no daemon restart; delivery terminal `UNKNOWN` with the enumerated detail `adapter_exit`
- [x] Fault matrix, one journey each: `j5a` queue overflow (32 accepted, then REJECTED `queue_full`), `j5b` turn deadline (UNKNOWN `turn_deadline` plus the adapter's own `session/cancel` for that session id, deadline compressed for the run via `AINB_ACP_TURN_DEADLINE_MS`), `j5c` idempotency (replay delivers once, conflicting reuse exits 5), `j5d` permission round trip (attention row → `fleet/action` approve → DELIVERED, row closed), `j5e` unknown target (per-delivery REJECTED `target_unknown`, request still exit 0 and persisted)
- [x] Each journey asserts the EXACT user-visible outcome (frame truth, not "screen shows something"); failures dump pane captures + daemon log tail; each prints `SMOKE-RESULT <journey> <PASS|FAIL|SKIP> <reason>` and any FAIL exits the script non-zero
- [x] Runnable one at a time for recording (`./scripts/chat-bus-smoke.sh j2`) and all together for CI
- [x] Wired as a CI-optional lane (`chat-bus-smoke` in `.github/workflows/ci.yml`, opt-in by PR label / repo variable / manual dispatch, the same posture as the `#[ignore]`d adapter tests) and documented in the Operational runbook below as the "is the chat bus actually alive" command
- [ ] Journey `j5d` has no CLI verb to exercise: part 1 ships no `ainb` command that answers an ACP permission (`ainb fleet approve` is the notifyd broker path for Claude hooks), so the smoke speaks `fleet/action` on `hangar.sock` directly, exactly as the TUI and macOS app do. Worth a CLI verb in part 2
- [ ] Receipts have no wire reader: `fleet/message_list` returns messages, not the delivery join, so every `detail` assertion in the smoke reads `fleet_message_delivery` from SQLite READ-ONLY. The runbook's question 1 ("why did this message not deliver") is therefore not answerable from the CLI alone today

---

## Observability (DE review 2026-08-04, new section)

The daemon already has a real observability surface and the original plan joined none of it: tracing to JSON `daemon.<date>` under `<hangar_home>/hangar/logs` with an OTLP seam (`ainb-hangar-daemon/src/observability.rs`), plus the `hangar/daemon_health` pane fed by the in-memory ring in `health_stats.rs`. Two new subsystems that are invisible to both are not operable.

Minimum surface, landing with the phase that creates each subsystem:

- [ ] Phase 3, spans: `fleet.message.send` span carrying `request_id`, `scope_key`, target count, and per-leg outcome; `fleet.message.forwarder` span per subscriber carrying its cursor and lag events. A dropped delivery must be answerable from the log alone
- [ ] Phase 3, delivery detail taxonomy: every non-DELIVERED delivery writes a SHORT, ENUMERATED reason into `fleet_message_delivery.detail` (for example `target_unknown`, `target_not_running`, `tmux_identity_changed`, `queue_full`, `breaker_open`, `adapter_exit`, `turn_deadline`, `daemon_restart`). Free text alone is not greppable and cannot be counted
- [ ] Phase 5, pool health fields on `hangar/daemon_health`: live process count, cap, per-scope queue depth, oldest in-flight turn age, breaker state per scope, evicted count. "Why is the copilot stuck" must be answerable from one pane, not from a debugger
- [ ] Phase 5, spans: `acp.turn` span per turn (session_key, provider, provider_version, chunk count, bytes, outcome), `acp.spawn` span (load-vs-reprime path, mode assertion result)
- [ ] Phase 4, counters: transcript rows written, transcript bytes written, commits issued (the write-amplification early warning per Retention and growth)
- [ ] Every one of these must be exercised at least once by an existing test asserting the field is populated, so the surface cannot silently rot

## Retention and growth (DE review 2026-08-04, new section)

The original plan's Risks table listed "Transcript volume bloats the store" and pointed at the existing `fleet_provider_event` growth contract as the mitigation. That contract is the thing this plan invalidates, so it cannot also be the mitigation.

What the contract actually says (`repo/fleet_provider_event.rs:3-25`): retention is NONE, deliberately, justified by a MEASURED rate of roughly 21 rows per 2 days at a ~344 byte mean payload, about 1.3 MB per YEAR against a ~4 MB database, with an explicit "revisit trigger: this table exceeding ~1M rows or ~100 MB" and an escape hatch that is "an explicit operator-invoked export-then-delete, NOT an automatic sweep".

What ACP transcripts do to it: one turn emits tens to low hundreds of coalesced rows. Five sessions at twenty turns a day at fifty rows a turn is 5,000 rows and roughly 1.8 MB per DAY, which is around 650 MB per year, roughly 500x the documented rate, and it crosses the documented revisit trigger inside the first year rather than never. A busy day at cap 8 crosses it faster.

Required work:

- [ ] Phase 1: amend the `fleet_provider_event` header doc with the new regime, the new revisit trigger, and the rule that ONLY `source='acp'` rows are eligible for deletion (they carry no `projection_revision` recovery contract, which is precisely why the pending-index predicate stays untouched)
- [ ] Phase 1: `delete_acp_before(session_key, ingest_order)` repo function, refusing any non-ACP row and any row with `projection_revision IS NULL`
- [ ] Phase 5 or later: an operator-invoked export-then-delete command for ACP transcript rows older than a chosen watermark. No timer, no automatic sweep, per the existing contract's explicit instruction. The DELETE runs against the watermark the export actually reached (`last exported ingest_order + 1`), never against the one the operator asked for (review 2026-08-07): the two diverge the moment a live turn commits a row after the export read its last page, and re-evaluating the operator's predicate would destroy exactly the rows that never made it into the only copy. `deleted` can therefore never exceed `exported`. The export is PAGED and each line is the FULL durable row (`provider`, `source`, `provider_session_id`, `received_at`, `raw_blake3`, `projection_revision` included, `raw_payload` as the exact stored string), because the row cap is a ROW cap and after the delete the file is the only thing a row can be reconstructed from
- [ ] `fleet_message` growth: state the shape. A broadcast to N recipients is 1 + N message rows and N delivery rows, permanently. Bodies are small, so this is a slow-burn table, but it has NO retention story at all today and should at minimum carry the same header-doc discipline the provider-event ledger has, including a revisit trigger
- [ ] Write amplification is the operational risk before disk is: one SQLite writer serves the whole control plane, `busy_timeout` is 10 s, and the store's own comment (`store.rs:84`) warns a contended writer can exhaust it. The Phase 4 commit cadence is the control; the Phase 4 commits-issued counter is the early warning

## Rollback and rollout (DE review 2026-08-04, new section)

The original plan had no back-out path for a one-way door. Both halves of this change are one-way:

- Migrations are forward-only with no down files (`ainb-hangar-store/src/lib.rs:9-13`) and `apply_migrations` is `sqlx::migrate!().run()` (`:118-120`), which refuses to start against a database holding an applied migration the binary does not embed. A daemon downgraded below 0079 therefore does not start at all. `schema_drift` (`:139`) already exists to surface the softer stale-binary case in the daemon-health pane
- Bump-and-refuse is symmetric: a v1 client is refused by a v2 daemon (by design, I9), and a v2 client is equally useless against a v1 daemon

Required work:

- [ ] Document the back-out procedure in the PR that lands 0079: restore the pre-upgrade database file (the only real rollback) and pin the client train to match. Say plainly that there is no in-place downgrade
- [ ] Walk the procedure once against a scratch database before the Phase 2 checkpoint is approved, and record the result in the checkpoint
- [ ] Client train claim CONFIRMED (2026-08-04): `release.yml` builds Rust targets only; the macOS app is a local Xcode build from the same checkout, coupling accepted. The bump PR's description must state: rebuild the app from the same train when updating the daemon; there is no distributed app artifact

## Operational runbook (DE review 2026-08-04, new section)

Three questions an operator will ask, each of which must have an answer built by the phases above:

1. **"Why did this message not deliver?"** `fleet/message_list` gives the row; the delivery join gives per-recipient state plus an enumerated `detail` reason (Observability); the `fleet.message.send` span in `daemon.<date>` gives the request-scoped trace. If `detail` is free text only, this question is unanswerable at scale
2. **"Why is the copilot stuck?"** `hangar/daemon_health` pool fields give queue depth, oldest in-flight turn age, and breaker state. The remedy is `fleet/action Interrupt` on the session (Phase 5 arms), and failing that the turn deadline converges it automatically (I16). Before this review the honest answer was "restart the daemon"
3. **"What is the pool doing?"** Live/cap/evicted counts and per-scope state on the same pane, with the `acp.spawn` span recording whether each session resumed by load or by re-prime
4. **"Is the chat bus actually alive?"** `ainb-tui/scripts/chat-bus-smoke.sh` — one command, a throwaway hangar home and tmux server, and a `SMOKE-RESULT` line per journey (exit non-zero on any FAIL). Run it after a daemon upgrade, before a release, and at the part 1 exit checkpoint:

   ```bash
   cd ainb-tui && ./scripts/chat-bus-smoke.sh          # every journey
   cd ainb-tui && ./scripts/chat-bus-smoke.sh j2       # one journey, for a recording
   cd ainb-tui && ./scripts/chat-bus-smoke.sh --keep j4  # keep the scratch world to poke at
   ```

   It touches nothing an operator owns: scratch `$AINB_HANGAR_HOME`, scratch `$HOME`, a private tmux server, and cleanup that kills tmux sessions by exact name only. Journeys that a build cannot support SKIP with a reason instead of failing (`j3` against a daemon with no Phase 6 resume; `j5d` when a real adapter occupies the fixture's registry slot).

## CLI surface (parity rule, added 2026-08-05)

Everything in ainb is CLI compatible. Rule: every chat-bus wire method ships its CLI verb IN THE SAME PHASE as its dispatch arm, never later, so the CLI is a first-class client of the same frozen contract the TUI and macOS app use (and the surface `ainb-web` proxies stays complete).

Transport: the CLI speaks `hangar.sock` directly (auth token from `hangar/daemon.token`, Content-Length JSON-RPC, negotiate v2). Task at Phase 3 start: extract or reuse an existing daemon client (candidates: `ainb-web`'s `DaemonClient`, the daemon test `Client`) rather than writing a third one.

Output contract (buzz-cli ergonomics, research doc recommendation 5): JSON on stdout by default under `--format json`; errors as JSON on stderr with a `retryable` boolean; semantic exit codes `0` ok, `1` bad input, `2` daemon/network, `3` auth, `4` other, `5` idempotency conflict (`request_fingerprint` mismatch maps here); stdin `-` accepted for any free-text argument; errors name the follow-up command where one exists.

| Phase | CLI verbs (land with that phase's dispatch arms) |
|---|---|
| 3 | `ainb fleet msg send --target <session_key>... [--text <t> \| -] [--request-id <id>]` · `ainb fleet msg list [--scope <k>] [--origin <id>] [--after <id>] [--limit N]` · `ainb fleet msg follow [--after <id>]` (subscribe rendered as NDJSON stream; the `--follow` mode buzz-cli lacks) |
| 5 | `ainb fleet acp create --provider <p> --cwd <dir> [--scope <k>]` · `ainb fleet transcript <session_key> [--after N] [--limit N] [--follow]` |
| 6 | `ainb fleet transcript prune --session <key> --before <order> --export <path>` (the Retention section's operator export-then-delete lands as this CLI verb; refuses without `--export` unless `--no-export` is explicit) |

Tests + docs per phase: CLI contract tests against the fixture daemon (exit codes, JSON error shape, `--follow` streams before turn end riding I12); `docs/tui/cli.md` updated in the same PR (the `CLI reference freshness` CI lane enforces drift).

## Testing strategy

| Layer | Tool | Notes |
|---|---|---|
| Store | `cargo test -p ainb-hangar-store` unit tests | idempotency, claim races, index contracts (I1 insert half, I3, I10), `EXPLAIN QUERY PLAN` on the real consumer query |
| Migration | `tests/migration_0079_upgrade.rs` per the repo convention (`migration_0050_upgrade.rs` is the model) | populated-database upgrade incl. the index drop/recreate |
| Proto contract | round-trip tests in `ainb-hangar-proto` + registry-drift guards | append-only registries, I9 negotiate |
| Swift contract | `Tests/FleetRPCTests` (`FleetDaemonContractTests` + fixtures), CI `swift-contract-paths` | tolerant decode, v2 fixtures, read AND write range assertions on the shipped store; ONE fixture set shared with part 2 once its gate runs |
| Daemon | RPC integration tests against fake adapter + fake tmux recorder | I1, I2, I3, I4, I6, I7, I8, I11, I12, I14, I16 |
| Library | `ainb-acp` unit tests against a scripted fake adapter | I13 (mode pinning, env allowlist), I15 (re-prime escaping), commit cadence |
| Real adapters | spike probes promoted to `ainb-acp/tests/` behind `#[ignore]` + env gate | resume secret-word, load-failure fallback, mode assertion; every test comments real-adapter vs fixture |
| CI | `-p ainb-acp` lane, verify-by-forced-failure once | graft 8 |
| Coupling | coupled-lane path filters (store change fires daemon + acp + Swift lanes) + cargo-metadata store-fence test (`sqlx` only in `ainb-hangar-store`) | added 2026-08-05; compiler covers Rust-level coupling, this covers lane routing and the raw-SQL bypass |

## Risks

| Risk | Mitigation |
|---|---|
| Adapter drift on npm invalidates spike facts | Phase 6 gate re-runs spike probes against pinned versions; version floors asserted from `agentInfo` at initialize AND persisted to `fleet_acp_session.provider_version` so a resume can detect the drift after the fact |
| `session/load` replay dropped (handler not live) | Named as the port's most likely bug; client.rs enforces handler-before-load by construction, real-adapter test proves history arrives |
| ACP session silently routed to tmux send | I8, retargeted: explicit ACP arm in `handle_fleet_action` ahead of the `verified_tmux_send` fallthrough (`rpc/mod.rs:1571-1579`), plus the integration assertion. NOT a `Backend::from_provider` change, which would have trapped an unrelated code path and reversed a live invariant (graft 8) |
| Transcript volume outgrows the store | Chunk coalescing (graft 5) plus a commit cadence (Phase 4). The `fleet_provider_event` growth contract is NOT a mitigation here: this plan invalidates its measured premise by roughly 500x and must amend it, see Retention and growth |
| Message stream silently loses a row under concurrency | Commit-ordered `seq` cursor (graft 9) and I14's concurrent-send test. A daemon-minted ULID cursor would have lost rows with no error and no gap visible to the subscriber |
| Adapter crash or hung turn wedges a scope until restart | Runtime convergence and turn deadline (I16), sharing one routine with the boot scan; `fleet/action Interrupt` as the operator remedy |
| Shared provider process crash interrupts every session on it (multiplex decision 2026-08-04) | I16 fan-out convergence over all hosted sessions; recovery per session via spike-proven `session/load`; SlotCircuit per provider prevents crash-loop churn; per-process in-flight ceiling bounds simultaneous casualties |
| Permission UX silently disabled by ambient state | I13: mode pinned at `session/new`, asserted from the reply, re-asserted after load, spawn fails on mismatch; adapter child gets an allowlisted environment |
| Broadcast-lag correctness without resync frames | I2 page-to-head test under forced lag; append-only logs make it safe by construction, given a commit-ordered cursor |
| Part 2 lands assumptions this file broke (e.g. `fleet/thread_list`) | Wire-contract checkpoint in Phase 2; part 2's Phase 0 gate reconciles; divergence already flagged in the Wire contract section |
| v2 bump strands an out-of-train client | Bump-and-refuse is explicit and tested (I9), tolerant decode makes the next provider bump-free. The write-leg trap (`FleetStore.swift:386` never passing `writeVersions`) is a named Phase 2 task; the macOS distribution train is an Open question |
| No way back after 0079 | Rollback and rollout section: documented file-restore procedure, walked once before the Phase 2 checkpoint |

## Open questions (pre-implementation gates, none block Phases 0-3)

- [ ] `session/load` with a well-formed but NONEXISTENT UUID on claude-agent-acp: spike inferred `-32603` but did not measure; one probe during the Phase 6 gate (also feeds part 2's identical open question)
- [ ] Session-cap default (16/provider), per-process in-flight ceiling (4) and idle windows: tune from real memory + interleaving behaviour during Phase 5; config knobs either way
- [ ] Scope-key grammar for part 2 channels (`channel:<id>`): confirm at part 2's Phase 0 gate that minted strings suffice (they did for C's proof); no schema change expected

Resolved (2026-08-04):

- **Pool shape: MULTIPLEX NOW** (Stevie decision, against the DE recommendation of process-per-scope-first). One adapter process per provider hosting many sessions, buzz shape. Rationale accepted: cheaper at scale from day 1; crash blast radius is acceptable because spike-proven `session/load` recovery makes a shared-process crash recoverable, and I16 convergence fans out to every hosted session. Graft 6, Phase 5, and I16 amended accordingly.
- **macOS Fleet app release train: LOCAL BUILD, coupling accepted.** Verified: `release.yml`'s matrix ships Rust targets only; no workflow packages the app (`fleet-macos-contract.yml` is contract tests). The app reaches the operator as a local Xcode build from the same source train. Consequence recorded in Rollback and rollout: updating the daemon past the v2 bump REQUIRES rebuilding the app from the same checkout; there is no distributed artifact to coordinate. If the app is ever distributed, the bump needs a coordinated release note.
