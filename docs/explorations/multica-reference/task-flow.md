# Task + Task-Flow: Multica vs Hangar

## 1. Multica Task + Flow

### Schema (`agent_task_queue`)

`server/migrations/001_init.up.sql:123-136` — base table:
```
id, agent_id, issue_id, status, priority, dispatched_at, started_at,
completed_at, result, error, created_at
status IN ('queued','dispatched','running','completed','failed','cancelled')
```
Grown by later migrations (all additive `ALTER TABLE agent_task_queue`):
- `020_task_session.up.sql` — `session_id`, `work_dir` (resume a Claude Code session via `--resume`).
- `022_task_lifecycle_guards.up.sql` — partial unique index `idx_one_pending_task_per_issue` on `(issue_id) WHERE status IN ('queued','dispatched')` — **global** per-issue at this point (superseded later by per-(issue,agent) semantics referenced in `ClaimAgentTask`, see hangar comment citing `pkg/db/queries/agent.sql`).
- `026_task_messages.up.sql` — new table `task_message(task_id, seq, type, tool, content, input, output)` — the transcript/message-stream store, indexed `(task_id, seq)`.
- `028_task_trigger_comment.up.sql` — `trigger_comment_id` FK to `comment` — threads a task back to the comment that fired it.
- `032_task_usage.up.sql` — new table `task_usage(task_id, provider, model, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, created_at)`, `UNIQUE(task_id, provider, model)` — per-model usage row (a task can span multiple provider calls, e.g. tool loop).
- `072-078` — `updated_at` column + rollup infra.
- `101-103` — hourly rollup table `task_usage_hourly` + trigger-driven dirty-queue (`task_usage_hourly_dirty`) replacing daily rollups.
- `207-211` — `client_usage_daily` (desktop/web client heartbeat telemetry, not task-scoped).
- `213_task_usage_authoritative_cost.up.sql` — adds `cost_usd_ticks BIGINT` (1e-10 USD ticks) to `task_usage`, NULL when the provider reported no cost (falls back to client-side rate-table estimate); the hourly rollup carries a parallel `uncosted_*_tokens` split so a bucket can mix authoritative and estimated rows without under- or over-reporting (see inline migration comment, extensively documented — this is the most-engineered piece of the reference schema).

Also relevant tables: `daemon_connection` (agent_id, daemon_id, status, last_heartbeat_at, runtime_info) and `activity_log` (workspace_id, issue_id, actor_type, actor_id, action, details JSONB) — both from `001_init.up.sql:138-163`.

### States & transitions
`queued → dispatched → running → completed|failed|cancelled`, plus a documented `waiting_local_directory` sub-state (daemon parked on a busy local_directory path) and a `deferred` pre-queued state (assignee-fallback escalation, `EnqueueDeferredAssigneeFallback`, promoted by `PromoteDueDeferredTasksForRuntime`).

### Claim protocol (`server/internal/service/task.go`)

- **`ClaimTask(agentID)`** (`task.go:2009`) — single-agent claim, one DB transaction:
  1. `GetAgentForClaimUpdate` (row lock)
  2. `CountRunningTasks` vs `agent.MaxConcurrentTasks` — capacity gate
  3. `ClaimAgentTask` (the actual `queued→dispatched` UPDATE, atomic CAS, `pgx.ErrNoRows` = no candidate)
  4. outside the tx: `ReconcileAgentStatus`, `broadcastTaskDispatch`
  Every step is timed and logged via `maybeLogClaimSlow` (claim latency budget observability).

- **`ClaimTaskForRuntime(runtimeID)`** (`task.go:2100`) — the per-runtime entry point a daemon actually calls:
  1. `PromoteDueDeferredTasksForRuntime` — promote due `deferred` rows first
  2. `ReclaimStaleDispatchedTaskForRuntime` — reclaim a stuck `dispatched` row **before** the empty-cache check (a lost claim response moves the task out of `queued`, so the empty-cache can't represent it)
  3. **`EmptyClaim` cache** (Redis-backed) — if a recent check found the runtime's queue empty, short-circuit without touching Postgres. Race-safe via a **version sample before the SELECT**: `preSelectVersion := EmptyClaim.CurrentVersion(...)` taken before `ListQueuedClaimCandidatesByRuntime`, so a concurrent enqueue's `Bump` in between is visible to the *next* `IsEmpty` check even though this call's `MarkEmpty` races it.
  4. `ListQueuedClaimCandidatesByRuntime` → loop distinct `agent_id`s, delegate each to `ClaimTask` until one lands on this runtime.

- **`ClaimTasksForRuntimes(runtimeIDs, maxTasks)`** (`task.go:2296`) — MUL-4257 batch counterpart: one promote-UPDATE + one reclaim-UPDATE + one candidate-SELECT across the *whole set* of a daemon's runtimes, then per-distinct-agent `ClaimTask` calls (preserves per-agent concurrency cap + per-issue guard) until `maxTasks`. Lets one daemon poll for N runtimes with O(1) DB round-trips instead of O(N).

- **`FinalizeTaskClaim`** (`task.go:2217`) — a *second* atomic step after claim: persists the task-scoped auth token + (for comment-triggered tasks) the exact `delivered_comment_ids` snapshot, in one transaction. Must be called only after the full HTTP response payload is built; a failure here calls **`RequeueTaskAfterClaimFailure`** (CAS on `dispatched_at` so a late handler can't roll back a *newer* reclaim) which puts the row back to `queued` and re-broadcasts + re-notifies — i.e. claim and "hand the payload to the daemon" are split into two commit points so a crash between them can't strand a claimed-but-undelivered task.

### Dispatch reason codes (`server/internal/dispatch/reason.go`)
A dedicated leaf package (MUL-4525) — the *admission* decision (not failure classification):
```
queued | coalesced | deferred                         (success path)
invocation_not_allowed | target_unavailable | runtime_offline
| attribution_blocked | already_active | self_trigger_suppressed
internal_error
```
Deliberately generic where enumeration-safety matters (`invocation_not_allowed` doesn't distinguish "private" from "doesn't exist"). Shared by the service layer (decides) and handler layer (serializes to wire) so they can't drift — one canonical vocabulary for "why did/didn't this trigger dispatch."

### Full create → assign → dispatch → run → review → done chain
```
comment/@mention/assignment
        │
        ▼
enqueueIssueTask / enqueueMentionTask family (task.go:973-1291)
  → resolves attribution (who is responsible), builds MCP overlay,
    stamps trigger_comment_id / coalesced_comment_ids / review SHA
        │  captureTaskQueued (analytics) + broadcastTaskEvent(EventTaskQueued)
        │  notifyTaskAvailable → EmptyClaim.Bump + Wakeup.NotifyTaskAvailable(runtime, task)
        ▼
ClaimTaskForRuntime / ClaimTasksForRuntimes  (daemon polls, or wakeup-driven)
        │  captureTaskDispatched, ReconcileAgentStatus, broadcastTaskDispatch (EventTaskDispatch)
        ▼
FinalizeTaskClaim (token + delivered_comment_ids persisted atomically with the response)
        │
        ▼
daemon spawns provider CLI → StartTask (EventTaskRunning) → streams task_message rows (EventTaskMessage/Progress)
        │
        ▼
CompleteTask / FailTask / CancelTask  (task.go:2603 / 2929 / 1737)
  - idempotent: a status-CAS `UPDATE ... WHERE status='running'` that returns
    no rows is NOT an error — re-fetch and treat as already-finalized (race-safe
    against a duplicate daemon callback)
  - CompleteTask: writes chat outcome row in the SAME tx as the status flip,
    synthesizes a fallback issue comment if the agent posted nothing,
    reconciles agent status, broadcasts EventTaskCompleted
  - FailTask: classifies failure reason → retry ladder (retryEligible,
    retryAttemptCeiling, retryDelayForAttempt) → may auto-enqueue a retry child
  - CancelTask: distinguishes not-started (synchronous draft restore) vs
    started-with-transcript (synchronous stop) vs started-empty-transcript
    (DEFERRED judgment — FinalizeDeferredCancelledChat resolves later once the
    daemon flushes or the sweeper grace period expires)
        │
        ▼
CaptureTaskUsage (task_usage row per provider/model) → hourly rollup → dashboard
activity_log entries (via bus listeners) → issue timeline / audit trail
```

### Wire protocol / two-hub event fan-out
- **In-process pub/sub**: `events.Bus` (`server/internal/events/bus.go`) — synchronous `Subscribe(type, handler)` / `SubscribeAll(handler)` / `Publish(Event{Type, WorkspaceID, ActorType, ActorID, Payload})`, panic-isolated per-handler. Every side-effect (Slack/Lark outbound, autopilot triggers, activity-log writes, notifications, subscriber fanout) is a bus subscriber wired in `server/cmd/server/*_listeners.go` — task.go itself never imports those concerns, it only publishes.
- **Hub 1 — browser realtime** (`server/internal/realtime`, wired in `server/cmd/server/listeners.go:152`): `bus.SubscribeAll` → JSON-marshal → `realtime.Hub` fans out over the workspace-scoped WebSocket to web/desktop clients; a parallel per-user personal-event branch (`SendToUser`) handles `inbox:*`/mentions.
- **Hub 2 — daemon WS** (`server/internal/daemon/wsrpc.go`, `daemon.go`): NOT bus-driven. The claim path calls `TaskWakeupNotifier.NotifyTaskAvailable(runtimeID, taskID)` (`task.go:3961`) directly — a push down the daemon's own WS control connection (`EventDaemonTaskAvailable`) so a daemon claims immediately instead of waiting its next poll tick. This is deliberately a *separate* channel from the browser Hub: task events for daemons are wakeup pokes, not full event payloads (the daemon still calls HTTP/WS-RPC claim to get the actual row).
- `EventDaemonRPCRequest`/`EventDaemonRPCResponse` — generic daemon→server correlated RPC over the same WS control connection (MUL-4257), the transport for WS-first claim with HTTP fallback.

### Reconciliation on daemon reconnect (`server/internal/daemon/reconcile.go`)
`reconcileBroadcaster` — close-and-replace channel broadcaster with **one-slot replay**: coarse daemon-side tickers (task-cancellation poll @5s, workspace sync @30s) would otherwise miss a server-side change that happened during a WS disconnect until their next tick. On WS reconnect, `broadcast()` wakes every current subscriber and arms a replay flag for late subscribers, debounced within `minBroadcastInterval` (1s) so a flapping connection can't stampede `GetTaskStatus`/`ListWorkspaces`. A companion `workspaceChangeSignal` (buffered chan, size 1) coalesces workspace-set-changed notifications the same way.

### Usage/cost tracking
`CaptureTaskUsage(task, provider, model, inputTokens, outputTokens, cacheReadTokens, cacheWriteTokens, costUSDTicks)` (`task.go:673`) writes one `task_usage` row per (task, provider, model). `213_task_usage_authoritative_cost` layers a provider-reported-cost column (`cost_usd_ticks`, nullable) on top of the pre-existing static-rate-table estimate, with the hourly rollup carrying a parallel `uncosted_*` split so mixed buckets (some rows authoritative, some estimated) never silently under/over-report — this is the single most heavily-engineered piece of the reference schema (see migration comment, `~90 lines` of rationale).

### Activity log / audit trail
`activity_log(workspace_id, issue_id, actor_type, actor_id, action, details JSONB, created_at)` (`001_init.up.sql:150-159`, handler in `server/internal/handler/activity.go`, 291 lines) — populated by bus listeners (`server/cmd/server/activity_listeners.go:23,51,244,249` — subscribes to `EventIssueCreated/Updated/TaskCompleted/TaskFailed`) — a free-text `action` + structured `details` JSONB audit trail scoped to an issue, rendered as the issue's activity timeline in the UI. Task lifecycle events (completed/failed) ARE captured here, giving a per-issue narrative independent of the raw `agent_task_queue` row.

---

## 2. Hangar (ainb) Task + Flow

### Schema (`crates/ainb-hangar-store`, SQLite)
`agent_task_queue` (repo type in `crates/ainb-hangar-store/src/repo/task.rs:69-148`): `id (ULID), workspace_id, runtime_id, agent_id, issue_id?, status, result?, session_id?, work_dir?, attempt, max_attempts, parent_task_id?, failure_reason?, priority (0..3, higher=more urgent), created_at, dispatched_at?, started_at?, finished_at?, autopilot_run_id?, mode (headless|interactive), session_name? (tmux), repo_ref? (checkout path | "scratch"), agent_kind (claude|codex|copilot), branch? (ainb/<slug>, stamped only if commits landed), generation (run/fan-out grouping), source_branch?`.

Two dedicated cost/history tables (parity migrations, both cite `task.go`/reference line numbers in their doc comments):
- `0022_task_usage.sql` — `task_usage(task_id PK, workspace_id, agent_id, input_tokens, output_tokens, cost_usd, created_at)` — one row per task, **`INSERT OR REPLACE`** on re-run (so a retried task's usage is overwritten, not accumulated).
- `0029_run_history.sql` — `run_history(run_id PK, task_id?, workspace_id, session_id?, provider, profile?, started_at?, finished_at NOT NULL, outcome (success|failed), input_tokens, output_tokens, cost_usd, diff_add, diff_del)` + a live `cost_rollup` VIEW (workspace, provider, UTC-day bucket, summed tokens/cost/run-count) — this is the one that actually preserves history across retries (distinct `run_id` per attempt, unlike `task_usage`'s overwrite-on-retry).

### Claim protocol (single claim loop, no cache layer)
`ClaimTaskService::claim_for_runtime` (`crates/ainb-hangar-store/src/service/claim.rs:96-178`) — **one SQL statement**, no separate transaction/cache dance:
```sql
UPDATE agent_task_queue SET status='dispatched', dispatched_at=?1
WHERE id = (
  SELECT q.id FROM agent_task_queue q JOIN agent a ON a.id=q.agent_id
  WHERE q.status='queued' AND q.runtime_id=?2
    AND (SELECT COUNT(*) FROM agent_task_queue r
         WHERE r.agent_id=q.agent_id AND r.status IN ('dispatched','running')) < a.max_concurrent_tasks
    AND NOT EXISTS (SELECT 1 FROM agent_task_queue s
         WHERE s.issue_id=q.issue_id AND s.agent_id=q.agent_id AND s.id<>q.id
           AND s.status IN ('queued','dispatched','running'))
  ORDER BY q.priority DESC, q.created_at, q.id LIMIT 1
) RETURNING *
```
SQLite's single-writer serialization makes this atomic by construction — no explicit transaction wrapper needed, no analog to multica's Redis `EmptyClaim` short-circuit cache (SQLite reads are cheap/local; there's no network round-trip to amortize). Comment block explicitly documents the concurrency-cap race this closes (counting `dispatched`, not just `running`, e38.27) and cites the reference's `task.go:761` / `ClaimAgentTask` as the model for the per-(issue,agent) `NOT EXISTS` guard and the per-agent concurrency cap.

### Dispatch loop, single-process (`crates/ainb-hangar-daemon/src/run_loop.rs`, 3735 lines)
- On daemon start: `reclaim_orphaned_on_startup(pool, runtime_id)` (`run_loop.rs:465`) — one-time crash-recovery reclaim of anything left `dispatched`/`running` from a prior process; a reclaim fault is non-fatal because the time-based sweepers still backstop it.
- `execute_claimed` (`run_loop.rs:840`) — the whole spawn/stream/finalize pipeline for one claimed task, in-process, no second daemon/server split: `resolve_dispatch` → `prepare_spawn_inputs` → spawn provider CLI (headless `claude -p` style, or `run_interactive` into a real tmux session for `mode=interactive`) → stream transcript → `finalize_success` / `finalize_failure` / `finalize_setup_failure` / `finalize_cancelled`.
- `FailureReason` enum (`crates/ainb-hangar-store/src/service/fail.rs:39-127`, values: `AgentError, Timeout, SpawnError, ProvisionError, ...`, `as_db_str()`) — this is **failure classification**, not an admission/dispatch reason code. There is no equivalent of multica's `dispatch.ReasonCode` (`invocation_not_allowed`/`target_unavailable`/`runtime_offline`/`attribution_blocked`/`already_active`/`self_trigger_suppressed`) anywhere in hangar — a task either gets claimed or it doesn't; nothing records *why* an issue's task wasn't dispatched (no assignee, agent offline, already active, etc.) as a stable machine-readable code.
- `maybe_spawn_retry` (`run_loop.rs:1913`) re-reads the failed row and decides on a retry child, paralleling multica's `MaybeRetryFailedTask`.

### Sweepers (`crates/ainb-hangar-daemon/src/sweeper.rs`, 422 lines) — direct parity with the reference, cites reference line numbers in doc comments
```
dispatched_at        +90s              +5min
     │                 │                  │
─────┼─────────────────┼──────────────────┼────────▶ age
     │ recovery window │ reclaim → queued │ fail → Timeout
     │ (in-flight, skip)│ (lost response)  │ (runtime crashed)
```
`QUEUED_TTL` = 2h, running TTL = 2.5h (cites `runtime_sweeper.go:52,40`), dispatched reclaim window = 90s (cites `task.go:85`). Every sweep statement constrains source `status` in its WHERE clause so terminal rows are never re-touched (idempotent).

### JSON-RPC surface / wire protocol (`crates/ainb-hangar-proto`)
Single channel: the daemon exposes a JSON-RPC 2.0 method surface (`crates/ainb-hangar-proto/src/methods.rs`) — `hangar/task_transition`, `hangar/task_retry`, `hangar/issue_run`, `hangar/tasks_list`, `hangar/usage_rollup`, etc. — over a **single UDS socket** (not two WS hubs). Events push as JSON-RPC *notifications* (no `id`) on method `hangar/event`, payload an internally-tagged `HangarEvent` enum (`crates/ainb-hangar-proto/src/events.rs`): `TaskQueued, TaskStarted, TaskProgress, TaskMessage, TaskFinished, CommentAdded, AgentPresence, SkillUpdated, ...`. There is one subscription channel per plugin connection carrying every event type — no split between "browser realtime" and "daemon wakeup" because there is no browser client and no separate wakeup-vs-payload split: the TUI plugin *is* the only subscriber, and it gets full event payloads directly (no poke-then-fetch two-step).

### Reconciliation on daemon restart
Limited to the one-shot `reclaim_orphaned_on_startup` call above — no reconcile-broadcaster / one-slot-replay mechanism, because there is no daemon↔server WS reconnect scenario to handle (hangar's daemon and its SQLite store are colocated; a plugin reconnecting to the daemon just re-subscribes and re-fetches via `hangar/tasks_list`, it doesn't need a "what changed while I was gone" broadcast since the daemon itself never lost the DB).

### Activity/audit log
No `activity_log` equivalent. `run_history` (0029) captures per-run outcome/cost/session but is task-execution-scoped only — no generic `(actor_type, actor_id, action, details)` audit trail for issue/comment/label/assignment events the way multica's `activity_log` does. The Hangar issue timeline (if any) would have to be reconstructed from `comment` rows + task terminal states; there's no single append-only audit stream.

---

## 3. GAPS

| Multica has | Hangar has | Gap | Effort |
|---|---|---|---|
| `dispatch.ReasonCode` enum (`queued/coalesced/deferred/invocation_not_allowed/target_unavailable/runtime_offline/attribution_blocked/already_active/self_trigger_suppressed/internal_error`) — a stable, wire-shared vocabulary for *why* a task did/didn't dispatch | Only `FailureReason` (post-hoc run failure classification: `agent_error/timeout/spawn_error/provision_error`) — nothing for admission-time skips (no assignee, agent offline, already active) | No dispatch-explain surface: a user cannot see "why didn't this run" for an issue that never got a task at all, only why a task that WAS created later failed | **M** — needs a new leaf enum + a call site at every non-dispatch decision point (`hangar/issue_run`, autopilot fire, squad assign) |
| `activity_log` table + bus-listener writers (issue/comment/task events) + handler — generic per-issue audit timeline | `run_history` (task-execution-scoped only) | No general audit trail for issue/label/assignment/comment lifecycle — only run outcomes are durable | **M** — new table + write call sites; could piggyback on existing `HangarEvent` notification points |
| `task_usage` (per model/provider row, cumulative across retries via `UNIQUE(task_id,provider,model)`) + `task_usage_hourly` rollup + `client_usage_daily` + authoritative-cost split (`cost_usd_ticks`, provider-reported vs rate-table-estimated) | `task_usage` (overwritten on retry, `INSERT OR REPLACE`) + `run_history` (append-only, has the retry history) + `cost_rollup` VIEW (daily, workspace+provider) | Functionally close — hangar's `run_history`+VIEW covers what multica needed two migrations (hourly rollup, dirty-queue) to get; hangar has NO authoritative-vs-estimated cost split (all `cost_usd` is provider `total_cost_usd`, no per-request tick-level accounting) | **S** — cosmetic gap only, current design is arguably simpler/adequate at hangar's scale |
| Two-hub fan-out: `events.Bus` → **browser realtime.Hub** (web/desktop WS) + **separate daemon-wakeup push** (`TaskWakeupNotifier` over daemon's own WS, decoupled from the bus) | Single JSON-RPC notification channel (`hangar/event`) over one UDS socket, TUI plugin is the only subscriber | Structural, not a gap: hangar has no browser client to fan out to. If a future hangar web/remote client is added, today's single-channel design would need the same wakeup-vs-full-payload split multica has (to avoid pushing full transcripts down a control channel meant for pokes) | **L** (only if a second client class is ever added — not needed today) |
| `EmptyClaim` Redis cache (version-stamped, race-closed against concurrent enqueue) short-circuits empty-queue polls without hitting Postgres | None — every claim call is one local SQLite statement | Non-gap: SQLite has no network round-trip to amortize; the cache exists in multica specifically because Postgres-over-network claim polling at scale needed it | **N/A** |
| `reconcileBroadcaster` — one-slot-replay wakeup for daemon-side tickers on WS reconnect (so a missed change isn't invisible until the next 5s/30s tick) | One-shot `reclaim_orphaned_on_startup`, no reconnect-broadcast mechanism | Non-gap at hangar's current architecture (daemon+DB colocated, no daemon↔server WS to drop); would become relevant if hangar ever splits daemon from a remote store | **N/A** today |
| `FinalizeTaskClaim` — split commit: claim (queued→dispatched) then a SECOND atomic step to persist the delivery token/comment-ids, with `RequeueTaskAfterClaimFailure` (dispatched_at-CAS) to roll back a claim whose payload build failed before the HTTP response was written | Single claim statement; `execute_claimed` runs the whole pipeline in-process after claim, with `finalize_setup_failure` requeuing on a provision error | Hangar's claim+dispatch is same-process so there's no network-boundary failure window between "claimed" and "daemon has the payload" the way multica's claim-then-HTTP-response split has — this asymmetry may not need closing | **N/A** — different architecture removes the failure window multica's split exists to cover |

**Top-ranked, worth scoping**: (1) dispatch reason codes — genuine observability gap, medium effort; (2) generic activity/audit log — genuine gap, medium effort. The rest are either already-adequate (usage/cost) or architecturally moot given hangar's single-process, single-client design (no second hub, no cache layer, no reconnect-broadcast needed).
