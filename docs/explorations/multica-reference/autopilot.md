# Autopilot / Workflow-automation: Multica vs Hangar

## 1. Multica Autopilot

Schema (`server/migrations/042_autopilot.up.sql`, extended by 11 later migrations).

**`autopilot`** (042_autopilot.up.sql:3-25; 058 dropped `priority`/`project_id`, 097 re-added `project_id`, 096 added `assignee_type`):
- `workspace_id`, `project_id` (nullable), `title`, `description`
- `assignee_id` + `assignee_type` (`agent`|`squad`, migration 096 — squad autopilots dispatch to `squad.leader_id`, app-layer resolved, no FK)
- `status`: `active`|`paused`|`archived`
- `execution_mode`: `create_issue`|`run_only` (042_autopilot.up.sql:14-15)
- `issue_title_template` (validated via `service.ValidateIssueTitleTemplate`)
- `concurrency_policy`: `skip`|`queue`|`replace` (042_autopilot.up.sql:16-17) — **dead column**: `grep -rn "ConcurrencyPolicy" server/**/*.go` returns zero hits outside the raw migration SQL; not even in the sqlc-generated `Autopilot` struct. Schema declared it, application code never reads it.
- `created_by_type`/`created_by_id`, `last_run_at`

**`autopilot_trigger`** — three kinds (042_autopilot.up.sql:29-40):
- `kind`: `schedule`|`webhook`|`api`
- schedule: `cron_expression` + `timezone` (default UTC) + cached `next_run_at` (display-only)
- webhook: `webhook_token` (unique per 091_autopilot_webhook_triggers.up.sql, partial-unique on `kind='webhook'`), `event_filters` JSONB added in 110 (`[{"event":"workflow_run","actions":["completed"]}]`, NULL = accept all)
- 189_autopilot_trigger_publisher.up.sql: `published_by_type`/`published_by_id` — per-trigger "responsible publisher" for human-attribution audit, re-stamped on any substantive edit to that trigger's cron/filter/enabled state

**`autopilot_run`** — lifecycle (042_autopilot.up.sql:47-65):
- `status`: `pending → issue_created|running → completed|failed` (+`skipped` added 079_autopilot_run_skipped_status.up.sql)
- `source`: `schedule`|`manual`|`webhook`|`api`
- links `issue_id`, `task_id` (`agent_task_queue.id`), `trigger_id`
- `trigger_payload`/`result` JSONB, `failure_reason`
- 124_autopilot_run_planned_at: `planned_at` + partial-unique `(trigger_id, planned_at)` — the idempotency guard so a stale-lease-steal retry cannot double-create a run for the same cron occurrence
- 096: `squad_id` attribution hook (not yet consumed)

**`autopilot_rule_version`** (186/187) — append-only, one row per *substantive* publish (create/enable/resume/target/instructions change; cosmetic edits like rename don't count), records `published_by_id` + `config_summary` JSONB. Dispatch reads the newest row per `(workspace_id, autopilot_id)` (index in 187) as the run's accountable human — decoupled from `originator_user_id` (which stays NULL for unattended fires). Human-attribution project MUL-4302.

**`autopilot_subscriber`** (120) / **`autopilot_collaborator`** (128) — auto-subscribe workspace members to spawned issues; explicit write-grants beyond creator/owner/admin. Both no-FK, app-layer integrity, mirror `issue_subscriber`.

### Scheduling engine (the real trigger-firing mechanism)

`server/internal/scheduler/jobs_autopilot.go` + `spec.go` — a generic DB-backed lease/lock scheduler (`sys_cron_executions` unique key on `(job_name, scope_kind, scope_id, plan_time)`), autopilot plugs into it as one job:

- Each **enabled schedule trigger is its own scope** (`scope_kind="autopilot_trigger"`, `scope_id=trigger.id`) — `jobs_autopilot.go:169-218`.
- `PlansForScope` hook (`jobs_autopilot.go:235-317`) computes cron occurrences in `(lastPlan, dbNow]` via `service.NextOccurrencesUTC`, collapses missed fires to the **latest only** (`CatchUpLatestOnly`), rejects anything > 5 min late (`maxAutopilotScheduleLateness`, line 38).
- Anchor for "since when do we enumerate" is 3-tiered: prior `sys_cron_executions` row → `trigger.last_fired_at` → `trigger.created_at`, capped by a 24h replay window — this ordering exists specifically to avoid replaying an occurrence already fired by a legacy pre-migration goroutine (comment cites the actual incident, `spec.go` equivalent doc at jobs_autopilot.go:261-301).
- Handler (`jobs_autopilot.go:328-412`) re-loads trigger+autopilot fresh (so a mid-tick disable/pause takes effect immediately), calls `AutopilotService.DispatchAutopilotForPlan`, then advances the **display-only** `next_run_at` floored at `max(now, planTime)` so a lagging clock can't recompute the same slot (MUL-3749 regression guard).
- Retry: `AllowStaleReentry=true`, `MaxAttempts=3`, backoff `[1m,5m,15m]`; a crashed claim promotes to FAILED via stale-lease sweep and a sibling can steal it — idempotent via the `(trigger_id, planned_at)` unique index.
- Generic `JobSpec` (`spec.go`) is reused across other scheduled jobs (hourly rollups etc.) — Autopilot is just the arbitrary-cron-per-scope instance of it, via `PlansForScope` overriding the uniform-cadence default.

### Dispatch core (`server/internal/service/autopilot.go`)

- `DispatchAutopilotForPlan` (scheduled path, :356-414): idempotency lookup by `(trigger_id, planned_at)` first; a complete run short-circuits, a partial run (e.g. `issue_created` with NULL `issue_id`) is recovered/failed and re-dispatched fresh — `isAutopilotRunComplete` (:438-449) explicitly treats `pending`/valueless in-flight states as **not** safe to reuse (cites a prior incident, #4443).
- `shouldSkipDispatch` (:1186-1259) — admission gate, run **every** dispatch regardless of source: no assignee → skip; agent/squad resolution failure (archived squad / hard-deleted agent) → skip with typed reason; `AgentReadiness` check (offline runtime tolerated only for `create_issue` mode, since the issue can sit queued); an **invocation/access gate** (`autopilotAdmitInvoke`) — manual "run now" is judged against the clicking human's own access to the assignee agent, automation falls back to the autopilot's creator. **This is where `concurrency_policy` should live and doesn't** — there is no concurrency/in-flight check anywhere in this function or its callers.
- `dispatchAutopilotRun` (:498-549) switches on `execution_mode`: `create_issue` creates an issue (assignee inherited, squad-leader-resolved) then the existing issue-listener chain enqueues the task; `run_only` enqueues a task directly, no issue. Unknown mode fails closed.
- Manual trigger (`TriggerAutopilot`, handler:1995-2036) resolves the human actor and attributes the run `direct_human` (vs. `rule_owner` for unattended fires) — the attribution model forks on how dispatch was invoked, not just who created the rule.
- Webhook ingress (`autopilot_webhook.go: HandleAutopilotWebhook`, :345+): 2-tier rate limiting (absolute IP ceiling + bad-credential-IP limiter), token lookup distinguishes no-row (404, generic) from DB error (500, so providers retry), workspace-consistency cross-check before persisting the delivery row, event-filter matching (`webhookEventAllowedByTriggerScope`) against the trigger's declared `event_filters`.

### What creating an autopilot collects (`CreateAutopilot`, handler:598-727)
`title` (required), `assignee_id` (required) + `assignee_type` (agent|squad, validated against workspace), `execution_mode` (required, `create_issue`|`run_only`), `issue_title_template` (optional, template-validated), `description`, `project_id` (optional), `subscribers` (list of member ids, validated before insert so a bad payload can't half-create the row). Creation itself is a transaction that **also** writes rule-version v1 (creator = accountable human) — every autopilot has an accountable human from birth (MUL-4302 §3.4).

## 2. Hangar (ainb) Autopilot — current state

This is considerably further along than a first grep suggests — the pure/IO-free layer (`ainb-hangar-core/src/autopilot/{mod,service,cron}.rs`) is the *older, simpler* v1 sketch (cron+agent+instructions+`max_concurrent_runs` only, no execution_mode/concurrency_policy). The **live** implementation is in `ainb-hangar-store/src/repo/autopilot.rs` + `ainb-hangar-daemon/src/scheduler.rs`, which has since grown real parity on several axes:

- **`Autopilot` row** (`repo/autopilot.rs:133-160`): `workspace_id`, `agent_id`, `name` (unique per workspace), `instructions`, `cron_expr`, `max_concurrent_runs`, `execution_mode`, `concurrency_policy`, `next_tick_at`, `enabled`, `created_at`.
- **`ExecutionMode`** (`repo/autopilot.rs:50-59`): `RunOnly` (default) | `CreateIssue` — direct match to multica's execution_mode, same two variants, same semantics (issue-first vs task-only).
- **`ConcurrencyPolicy`** (`repo/autopilot.rs:91-104`): `Skip` (default) | `Queue` | `Replace` — **same three-value enum as multica's schema**, but unlike multica this one is fully wired: `scheduler.rs:381-430` (`fire_or_skip`) checks in-flight count against `max_concurrent_runs` and branches on the policy live — `skip` drops the tick + emits `tick_skipped`; `queue` fires anyway (relies on the shared claim/dispatch queue to serialize); `replace` cancels the open run(s) + their task, then fires fresh. Proven under contention by `tests/scheduler_concurrency_policies.rs` (real sqlite + advanceable clock, asserts actual row counts/status, not just the emitted event). **Hangar's concurrency_policy is a real, tested behavior; multica's is a dead column.**
- **Trigger kinds**: schedule (cron, via `AutopilotScheduler` loop, `scheduler.rs`) + **webhook** (`ainb-hangar-daemon/src/webhook_ingress.rs`) — a hand-rolled localhost-only (`127.0.0.1`, never `0.0.0.0`) HTTP/1.1 listener at `POST /hangar/webhook/<autopilot_id>`, HMAC-SHA256 signature verification (`X-Hangar-Signature`) against a per-autopilot secret (0600 file store, DB holds only the digest), event-filter matching (`event_passes_filter`), and a full delivery audit log (`autopilot_webhook_delivery` table via `AutopilotWebhookRepo`, inspectable via `ainb hangar autopilot deliveries <id>`). **No `api` trigger kind** (multica's third kind, a bare programmatic-fire path) — not present in hangar.
- **Run lifecycle**: `running → completed|failed|cancelled` (`repo/autopilot_run.rs`) — no `pending`/`issue_created`/`skipped` intermediate states; the replace-policy path directly writes `cancelled` on supersede.
- **No rule versioning** — no equivalent of `autopilot_rule_version` / human-attribution audit trail. No `originator`/`accountable_user` split; runs aren't attributed to a publishing human vs. the triggering source.
- **No subscriber/collaborator tables** — no auto-subscribe-members-to-spawned-issues, no explicit collaborator write-grants beyond implicit ownership.
- **No squad/team assignee** — `agent_id` only, single agent, no squad-leader resolution path.
- **Create flow** (`ainb-core/src/cli/hangar/mod.rs:288+`, `AutopilotCreateArgs`): CLI-only (`hangar autopilot create`) — name, agent, cron, instructions, `max_concurrent_runs`, execution-mode arg, concurrency-policy arg, workspace slug. No TUI creation wizard yet in `screen/autopilots.rs` (that screen is list/inspect-oriented — cron, state, last-run — not a create form); no title-template, no project scoping, no subscriber list.
- Separate from the D13 "auto-standup" watcher (`ainb-hangar-daemon/src/standup.rs`) — that's a fixed always-on idle-session nudge, not a user-defined autopilot rule (documented previously in memory as off-by-default).

## 3. GAPS

| Multica has | Hangar has | Gap | Effort |
|---|---|---|---|
| `api` trigger kind (bare programmatic fire, alongside schedule/webhook) | schedule + webhook only | Add a 3rd trigger kind: authenticated local RPC/CLI verb that fires a run without going through cron or the webhook HMAC path (useful for CI/other-tool integration without minting a webhook secret) | S |
| Rule versioning + human attribution (`autopilot_rule_version`, `originator` vs `accountable_user`) | none | No audit trail of "who is accountable for this unattended run" — matters once autopilots are multi-user/shared | M |
| `autopilot_subscriber` / `autopilot_collaborator` (auto-subscribe + write-grants) | none | Single-owner model only; fine for solo `ainb` but blocks any future team-shared workspace autopilots | S–M |
| Squad/team assignee (leader-resolves dispatch) | single agent only | No equivalent concept of squads in ainb yet — likely out of scope until ainb has a team/squad primitive | L (blocked on a missing primitive) |
| Run lifecycle: `pending→issue_created|running→completed|failed|skipped` (5-6 states, `skipped` distinct from `failed`) | `running→completed|failed|cancelled` (4 states) | Hangar conflates "policy-skipped tick" into a log event (`tick_skipped`), not a queryable run row — history/reporting can't distinguish "we skipped this" from "it never happened" without re-parsing logs | S |
| `execution_mode` (`create_issue`/`run_only`) | same two variants, fully wired | **Parity** — no gap | — |
| `concurrency_policy` (`skip`/`queue`/`replace`) | same, but hangar's is tested/live, multica's is a dead schema column | **Hangar ahead here** | — |
| TUI/UI create form with title-template, project scoping, subscriber picker | CLI-only create, no template/project/subscriber fields | No TUI creation wizard for autopilots at all; today it's `hangar autopilot create` CLI only | M |
| Webhook delivery audit + event filters | same shape, already built (HMAC, filters, delivery log, CLI verb) | **Parity** — no gap | — |
| Scheduler: generic DB-backed lease/scope engine reused across multiple job types, stale-lease steal + idempotent replan | Purpose-built `AutopilotScheduler` loop, single job type, cron-per-autopilot with `max_concurrent_runs` limit check at fire time | Hangar's scheduler is simpler/narrower (fine at current scale — SQLite single-daemon, no multi-instance lease contention to solve) — not a gap worth closing unless ainb goes multi-daemon | — (defer) |

**Top gaps ranked**: (1) no `api` trigger kind, (2) no rule-versioning/human-attribution audit trail, (3) `skipped` not a first-class run status (only a log event), (4) no subscriber/collaborator model, (5) no TUI create wizard (CLI-only today).
