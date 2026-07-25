# Issue entity: Multica vs Hangar

## Multica Issue

### Schema (`server/migrations/001_init.up.sql:52-94` + later migrations)

Base `issue` table (`001_init.up.sql:52-72`):

| Field | Type | Notes |
|---|---|---|
| `id` | UUID PK | |
| `workspace_id` | UUID FK | cascade delete |
| `title` | TEXT NOT NULL | |
| `description` | TEXT | nullable |
| `status` | TEXT CHECK | `backlog\|todo\|in_progress\|in_review\|done\|blocked\|cancelled` (7 states) |
| `priority` | TEXT CHECK | `urgent\|high\|medium\|low\|none` |
| `assignee_type` | TEXT CHECK | `member\|agent` at init; **`squad` added later** (no dedicated migration found, but `assignee_type` string is validated `'member', 'agent', or 'squad'` in handler, `issue.go:3055`) |
| `assignee_id` | UUID | nullable, polymorphic |
| `creator_type` | TEXT CHECK | `member\|agent` |
| `creator_id` | UUID NOT NULL | |
| `parent_issue_id` | UUID FK → issue(id) ON DELETE SET NULL | **SUBTASKS** |
| `acceptance_criteria` | JSONB NOT NULL DEFAULT '[]' | structured criteria list |
| `context_refs` | JSONB NOT NULL DEFAULT '[]' | linked context references |
| `position` | FLOAT NOT NULL DEFAULT 0 | manual ordering, see Move below |
| `due_date` | TIMESTAMPTZ → later `DATE` (migration 112) | |
| `created_at`/`updated_at` | TIMESTAMPTZ | |

Sibling tables from the same init migration:
- `issue_label` (workspace-scoped `name`+`color`) and `issue_to_label` (composite PK many-to-many join) — `001_init.up.sql:75-86`.
- `issue_dependency` (`001_init.up.sql:89-94`): `(issue_id, depends_on_issue_id, type)` where `type ∈ {blocks, blocked_by, related}` — a **typed dependency graph**, not just a single-direction blocker edge.
- `comment` table with `type ∈ {comment, status_change, progress_update, system}` (`001_init.up.sql:97-107`) — feeds the mention-trigger pipeline below.

Fields added by later migrations (all additive, non-destructive):
- `number` + workspace `issue_prefix`/`issue_counter` → human-readable `PREFIX-N` ids, unique per workspace (`020_issue_number.up.sql`).
- `origin_type`/`origin_id` (added in `042_autopilot`, extended `060`/`111`/`131`/`149`) — provenance: `autopilot`, `quick_create`, `lark_chat`, `slack_chat`, `agent_create`. Used to attribute agent-created issues back to a human originator (`149_issue_origin_agent_create.up.sql`).
- `start_date` (`091_issue_start_date.up.sql`) pairs with `due_date` for Gantt; both converted `TIMESTAMPTZ → DATE` in `112_issue_dates_to_date.up.sql` (calendar-day semantics, timezone-bug fix).
- `metadata` JSONB (`105_issue_metadata.up.sql`) — free-form small KV bag (≤50 keys, primitives only, 8KB cap, GIN `jsonb_path_ops` index) for agent pipeline state (PR number, pipeline_status, waiting_on…). Single-key atomic writes only (`issue_metadata.go:1-30`) — `UpdateIssue` never touches it, to avoid races with concurrent agent writes.
- `first_executed_at` (`050_issue_first_executed_at.up.sql`) — stamped once, atomically, first time the issue's task reaches `done`; analytics funnel source of truth.
- `stage` INTEGER, nullable, `>=1` (`123_issue_stage.up.sql`) — orders sub-issues sharing a `parent_issue_id` into **barrier groups**; drives the child-done cascade (below).
- `properties` JSONB (`191_issue_properties.up.sql`) + `issue_property` catalog table — custom typed per-workspace properties (text/number/select/multi_select/date/checkbox/url), keyed by definition UUID so renames don't require value migrations. Capped at 20 active defs/workspace, 16KB value bag.
- `issue_subscriber` (`015_issue_subscriber.up.sql`) — notification subscription per `(issue, user)` with `reason ∈ {creator, assignee, commenter, mentioned, manual}`.
- `issue_reaction` (`027_issue_reactions.up.sql`) — emoji reactions, unique per `(issue, actor, emoji)`.
- `issue_pull_request` — link table (not in 001_init, found via later migrations `109`/`127`) carrying `close_intent` (explicit "Closes #N" keyword) and `reference_only` (bare mention, no closing keyword) flags, so the PR↔issue auto-advance gate can distinguish a working PR from a passing reference.

### Status lifecycle
7 states: `backlog, todo, in_progress, in_review, done, blocked, cancelled`. `done`/`cancelled` are terminal (`isTerminalChildStatus`, `issue_child_done.go`). `backlog` is an explicit "parking lot" — assigning/creating into backlog never auto-starts a run (`issue_trigger.go` service, `WillEnqueueRun`).

### Assignee polymorphism
`assignee_type ∈ {member, agent, squad}` + `assignee_id` UUID, no FK (cross-table polymorphism). Validated in `validateAssigneePair` (`issue.go:2996-3063`, error message literally enumerates `"assignee_type must be 'member', 'agent', or 'squad'"`). A **squad** assignee resolves to its `leader_id` agent for actually running (`service/issue_trigger.go:130-146`) — squad assignment fans a run to the squad leader, not a member-per-task at the issue level (task-level squad fanout is a separate service, `SquadAssignService`, referenced only from hangar's analogue below).

### Subtasks (parent/child)
`parent_issue_id` (self-FK, `ON DELETE SET NULL`) is a first-class column since `001_init`. `ListChildIssues` / `ListChildrenByParents` / `ChildIssueProgress` (`issue.go:1902-2065`) serve the child list + roll-up progress. `stage` (migration 123) groups children into ordered barrier stages under one parent — an unstaged set is one implicit stage.

**Child-done → parent cascade** (`issue_child_done.go`, 695 lines): when a child transitions non-terminal → terminal (done OR cancelled):
1. Guard: parent exists, parent not already done/cancelled, parent not `backlog` (a backlog parent is deliberately parked — waking it would let the agent auto-promote siblings, a previously-reported bug MUL-3497/#4320), parent assignee not a human member (no task to trigger, no point notifying a human via bot comment).
2. **Stage barrier**: fires only when the *stage closes* — every sibling in the lowest unfinished stage is now terminal (`stageBarrierClosed`). This collapses "wake on every child" into "wake once when the stage's last child finishes."
3. Posts a system comment on the parent (bypassing the normal `on_comment` trigger path so it can't accidentally re-mention unrelated members) embedding a `mention://{agent,squad}/<id>` link targeting the parent's own assignee, which explicitly fires that assignee's own agent-run trigger (`dispatchParentAssigneeTrigger`).
4. Batch variant (`notifyParentsOfBatchChildDone`) aggregates across a whole batch update so multiple stages closing in one request produce one comment per parent from final state, not one per intermediate stage (order-independence fix, MUL-4155).

### Dependencies
`issue_dependency` table: `(issue_id, depends_on_issue_id, type)`, `type ∈ {blocks, blocked_by, related}` — a real directed, typed dependency graph (`001_init.up.sql:88-94`). (Full CRUD/gating handler not read in this pass; table exists since day one.)

### Labels
`issue_label` (workspace-scoped name+color) + `issue_to_label` many-to-many join (`001_init.up.sql:74-86`). Faceted filtering supports `label_ids` array (see Filtering below).

### Custom properties vs metadata (two distinct JSONB surfaces)
- `metadata` (105): agent-internal KV scratch, flat primitives, no catalog, not user-facing structure — for pipeline bookkeeping.
- `properties` (191) + `issue_property` catalog: user-defined typed custom fields (like Linear/Notion properties), with select/multi_select option catalogs, position-ordered, archivable (never hard-deleted).

### Comment `@mention` auto-dispatch trigger (the "comment_trigger" model)
Lives in `server/internal/handler/comment.go` (not `issue_trigger.go`, which only covers **assign/status** writes — a deliberately separate predicate; see `issue_trigger.go:66-72`).

Mention markup: `[@Label](mention://{member|agent|squad|issue|all}/<uuid|all>)`, parsed both server-side (Go, `util.ParseMentions`) and client-side (`packages/core/issues/comment-trigger-outcomes.ts:8-9` regex — kept in sync).

Flow (`comment.go:1897-1998`, `computeCommentAgentTriggers`):
1. Parse mentions in the comment body.
2. If any explicit `@agent`/`@squad` mention exists → route ONLY to those targets (`resolveMentionedAgentCommentTriggers`), skipping the fallback logic entirely.
3. Else if a `@member` mention exists → member-mention path (notification only, no agent trigger).
4. Else → **assignee-fallback routing**: reply-to-parent-author, thread-root-owner routing, or the issue's own assignee (`routeReplyToParentAuthor`, `routeThreadRootOwners`).
5. Each resolved trigger is enqueued (`enqueueCommentAgentTriggers`) with per-target outcomes: `queued | coalesced | deferred | blocked` (`CommentTriggerOutcome`, MUL-4525 §2) — the wire response includes this array so the client can show "N not triggered" toasts.
6. A **squad** mention routes to the squad's leader (`commentTriggerSourceMentionSquadLeader`); an **agent** mention routes directly (`commentTriggerSourceMentionAgent`).
7. Private-agent visibility gate (`CanAccessAgent`/`canInvokeAgent`) applies identically to preview and real enqueue so preview never leaks a private agent's readiness to someone who can't see it.
8. Self-loop guard: an agent's own comment on its own running issue does not re-trigger itself (`IsSelfLoop`).
9. Merge-into-pending: a new mention against an agent that already holds a pending/active task for the issue coalesces into that task rather than double-enqueuing (`mergeCommentIntoPendingTask`).
10. `PreviewCommentTriggers` / `PreviewIssueTrigger` (dry-run, `comment.go:1130` / `issue_trigger.go` handler) let the UI show "will start N runs" before the user hits send — single source of truth shared with the real write path (MUL-3375).

This is the single biggest structural feature hangar lacks entirely: **comments never dispatch agent work in hangar.**

### Issue creation — all inputs
`CreateIssueRequest` (`issue.go:2372-2399`): `title` (required), `description`, `status`, `priority`, `assignee_type`+`assignee_id`, `parent_issue_id`, `acceptance_criteria`, `context_refs`, `due_date`/`start_date`, `label_ids`, `origin_type`+`origin_id` (provenance, optional pair). No repo/branch fields at all — multica's "issue" is decoupled from any git workspace concept (that lives in its separate task/run layer), unlike hangar where repo/branch selection is baked into issue creation.

Also: `QuickCreateIssue` (`issue.go:2066-2282`) — a daemon-driven fast-path creation (stamps `origin_type=quick_create`) used by autopilot / chat-origin flows (Lark/Slack), with a runtime-online + CLI-version gate before allowing it.

### Filtering / faceted list view
Two query surfaces:
1. **Legacy flat list** `ListIssues` (`issue.go:778-1275`) — query params: `assignee_types` (CSV), `assignee` actor filter, workspace scope, status, etc.
2. **Modern table query** (`issue_table_query.go` + `issue_table_facets.go`, 993 combined lines) — a structured POST body (`issueTableQuerySpec`):
   - `Filters`: `Statuses[]`, `Priorities[]`, `Assignees[]` (+`IncludeNoAssignee`), `Creators[]`, `ProjectIDs[]` (+`IncludeNoProject`), `LabelIDs[]`, `Properties map[propertyID][]values` (custom-property faceted filter), `Date{Field,Start,End}` range filter, `WorkingOnly`/`WorkingIssueIDs`, `IncludeSubIssues`.
   - `Scope`: `kind` (e.g. workspace/project/assignee-group), `AssigneeTypes[]`, `ProjectID`, `Actor` (an involves-filter), `Relation`.
   - `Sort{Field,Direction}`, free-text `Search`.
   - `Group`: group-by kind (status/assignee/property/etc.), with `Secondary`/`SecondaryValues` for a 2-level grouping, cursor-paginated (`Page{Limit,Cursor}`), optional parent-hierarchy expansion (`Hierarchy{Enabled}`, `ParentID`).
   - `Facets` endpoint (`ListIssueTableFacets`) returns, per requested facet (e.g. "distinct assignees present in this filtered set" with counts), the value+count pairs used to render filter-picker option lists that only show options actually present in-scope.
   - Full-text search (`SearchIssues`, `issue.go:625-764`) with snippet extraction, numeric issue-number matching, term highlighting.

### Move / reorder
`MoveIssue` (`issue_move.go`) takes relative neighbor anchors (`before_id`/`after_id`, optional `project_id`) rather than a client-authored `position` float; server derives the new float position (midpoint, or ±1 at a list end) and re-dispatches through the same `UpdateIssue` write path so realtime/triggers/validation stay on one path. Stale/exhausted-float anchors fail closed with 409 rather than silently renumbering.

---

## Hangar Issue

### Schema (`crates/ainb-hangar-store/migrations/0003_issue_comment.sql` + later)

Base `issue` table (`0003_issue_comment.sql:19-29`):

| Field | Type | Notes |
|---|---|---|
| `id` | TEXT PK (ULID) | |
| `workspace_id` | TEXT FK | |
| `title` | TEXT NOT NULL | |
| `description` | TEXT | nullable |
| `state` | TEXT DEFAULT 'open' | no CHECK constraint (free text); remapped `open→todo`, `closed→done` in migration `0023` |
| `assignee_type`/`assignee_id` | TEXT CHECK `member\|agent` | polymorphic, **no `squad` value in the CHECK** at the schema level |
| `creator_type`/`creator_id` | TEXT NOT NULL CHECK `member\|agent` | |
| `created_at` | INTEGER (epoch ms) | |

Sibling: `comment` table (`author_type/author_id` polymorphic, `body`, `created_at`) — plain comment, no `type` discriminant (no status_change/progress_update/system distinction).

Fields added later, all `ALTER TABLE ADD COLUMN`:
- `priority` INTEGER `0..3` (`0014`) — HIGHER = MORE URGENT (inverse of multica's `urgent|high|medium|low|none` text enum), mirrors `agent_task_queue.priority`.
- `due_date` INTEGER epoch ms (`0014`).
- `labels` TEXT JSON-array column (`0014`) — denormalized read-cache; promoted to a real `label` + `issue_label` join table in `0016` (source of truth is the join, `issue.labels` JSON kept in sync as a cache, never re-derived by JOIN on the hot list path).
- `external_ref` TEXT (`0043`) — free-form upstream GitHub/Jira reference (URL or `owner/repo#123`), shown on the board card and appended to the dispatched brief as `Linked issue: <ref>`. Roughly analogous to multica's `context_refs`/`issue_pull_request` but far more minimal (single string, no structured link table, no close-intent tracking).
- `auto_run` INTEGER boolean, default 0 (`0036`) — per-issue opt-in: auto-launch once its last dependency (`card_dependency`) completes.
- `squad_id` TEXT nullable (`0035`) — squad assignment lives as a SEPARATE column from `assignee_type/assignee_id`, not a third polymorphic value; when set, a run fans out leader+members each into their own worktree. **This is structurally different from multica**, where `squad` is just a third `assignee_type` enum value resolved to the leader for a single run.
- `ord` INTEGER on `board_card` (not `issue` itself) (`0034`) — per-column manual ordering, default 0, only meaningful within one board column (not a single global float position like multica's `issue.position`).

`card_dependency` table (`0036_card_dependency.sql`): `(workspace_id, dependent_issue_id, blocker_issue_id, created_at)`, composite PK `(dependent_issue_id, blocker_issue_id)` — a single **untyped, blocks-only** edge (no `blocked_by`/`related` variants, no reverse "this relates to" semantic). Enforced service-side: DFS cycle check on insert, self-edge rejected, dependent refuses to RUN while an unfinished blocker exists, auto-dispatches dependents only if `auto_run=1`.

### Rust repo type (`crates/ainb-hangar-store/src/repo/issue.rs:58-84`)
```rust
pub struct Issue {
    pub id: String,
    pub workspace_id: String,
    pub title: String,
    pub description: Option<String>,
    pub state: String,
    pub assignee: Option<ActorRef>,
    pub creator: ActorRef,
    pub created_at: i64,
    pub priority: i64,
    pub due_date: Option<i64>,
    pub labels: Vec<String>,
    pub external_ref: Option<String>,
}
```
Notably **`squad_id` and `auto_run` are NOT fields on this typed struct** at all (verified via grep — zero hits in `issue.rs`) — those columns exist in the schema but are read/written through other ad-hoc SQL paths (board/squad-assign/dependency services), not the canonical `IssueRepo`/`Issue` type. There is no `parent_issue_id`, no `acceptance_criteria`, no `context_refs`, no `metadata`, no `properties`, no `position` float, no `stage`, no `origin_type`, no `start_date` on this struct or in the schema at all.

### Status lifecycle (`crates/ainb-plugin-hangar/src/screen/issue_list.rs:51-61`, `IssueLifecycle`)
5 canonical states: `backlog, todo, in_progress, in_review, done`. **No `blocked`, no `cancelled`.** Legacy `open`/`closed` map forward once (migration `0023`) but the column has no CHECK constraint — any string is accepted, unknown values fail-visible into the `Todo` column.

### Create wizard (`issue_list.rs:307-499`, `WizardRow`)
7 rows, all present at once (not staged): **Title → Brief → Link → Repo → Source → Target → Agent**.
- `Title` (required, non-blank)
- `Brief` (optional multi-line) → becomes `issue.description` and the dispatched `claude -p` prompt
- `Link` (optional) → `issue.external_ref`
- `Repo` (required, `@` fuzzy picker or ←/→ cycle)
- `Source` branch (prefilled `main`)
- `Target` branch (prefilled `main`, where a future PR would land)
- `Agent` (←/→ cycle `AgentChip::ALL`, or the named-agent roster via `WizardAgent` if any exist)

**No priority, due date, labels, acceptance criteria, or custom properties in the create wizard** — despite the schema supporting priority/due_date/labels since migration 0014, the create UI never surfaces them (schema-ahead-of-UI gap). Only a title-only/repo-less/agent-less create is blocked (Enter jumps focus to the missing required row instead).

### Filtering (`issue_list.rs:150-234`, `FilterChip`)
4 fixed chips only: `All`, `Members`, `Agents`, `Mine` (Mine is a documented P5 placeholder currently behaving as `All`). Cycled with Tab/Shift+Tab. Plus a free-text `/` query filter (`IssueListState::query`) — substring search, not the structured multi-facet query multica has. **No status/priority/label/property/date-range filters, no faceted "show me only values present in this view" endpoint, no involves/creator filters, no server-side paginated table query with grouping.**

### Assignee polymorphism
`assignee_type ∈ {member, agent}` only at the schema CHECK level. Squad assignment is bolted on as an orthogonal `squad_id` column rather than a third assignee-type value — meaning "who is this issue assigned to" is answerable from two different columns depending on whether it's solo or squad, unlike multica's single `(assignee_type, assignee_id)` pair that already covers `squad`.

### Subtasks / parent-child
**Absent entirely.** No `parent_issue_id` column, no child-list query, no stage/barrier concept, no child-done → parent cascade or notification. An issue in hangar has no relationship to any other issue except through the untyped `card_dependency` blocks-edge.

### Dependencies
`card_dependency`: single directed "blocks" edge, DFS-cycled-checked, auto-run opt-in on completion. No `related`/`blocked_by` distinction, no dependency-type enum, no UI to browse the dependency graph beyond gating a run.

### Acceptance criteria / context refs
**Absent.** No JSONB structured-criteria column, no linked-context-references column. The closest analogue is the single free-text `external_ref` string (one upstream link, no structured list, no closing-keyword tracking).

### Comment @mention auto-dispatch
**Absent entirely.** Confirmed via grep across `ainb-plugin-hangar/src` and `ainb-hangar-store/src`: no mention parsing, no comment-triggered task enqueue, no per-target outcome tracking, no preview endpoint. Hangar's `comment` table is pure text storage with zero side effects on write.

### Custom properties / metadata
**Absent.** No `issue_property` catalog, no per-issue JSONB properties bag, no per-issue metadata KV scratch space for agent pipeline state.

### Move / reorder
Board-column-scoped `ord` integer (`board_card.ord`, migration 0034) — reordering is local to one column, contiguous `0..n` rewrite on reorder, no cross-column global float position, no anchor-based (`before_id`/`after_id`) API — reorder mechanics not directly inspected this pass but the storage model is structurally simpler (integer bucket-local index vs. multica's workspace-global float with anchor-derived midpoint math and 409-on-stale-anchor).

---

## GAPS (multica has → hangar has → gap → effort)

| # | Multica has | Hangar has | Gap | Effort |
|---|---|---|---|---|
| 1 | Comment `@mention` auto-dispatch: parses `[@x](mention://type/id)`, routes to explicit mention / reply-parent / thread-owner / assignee-fallback, per-target `queued/coalesced/deferred/blocked` outcomes, preview endpoint, self-loop + private-agent gates, merge-into-pending dedup | Nothing — comments are inert text | **Largest structural gap.** No way to hand off work via conversation; every dispatch must go through explicit assign/status-change | **L** |
| 2 | `parent_issue_id` subtasks + `stage` barrier groups + child-done→parent cascade comment+wake, staged multi-child completion aggregation | No parent/child relationship of any kind | Cannot decompose an issue into tracked sub-issues at all; no roll-up progress | **L** |
| 3 | `issue_dependency`: typed graph (`blocks`/`blocked_by`/`related`), any-direction | `card_dependency`: single untyped "blocks" edge, cycle-checked, `auto_run` opt-in | Narrower semantics (no "related", no reverse blocked_by as distinct type) but the *core* auto-run-on-blocker-done mechanic already exists — this is the closest-to-parity gap | **S–M** |
| 4 | `acceptance_criteria` JSONB structured list + `context_refs` JSONB | Neither; only a single free-text `external_ref` string | No structured acceptance criteria or multi-item context linking | **M** |
| 5 | Structured table-query filtering: status/priority/assignee/creator/project/label/property/date-range facets, faceted-value-with-counts endpoint, 2-level grouping, cursor pagination, involves/scope filters | 4 fixed chips (All/Members/Agents/Mine) + free-text substring search | No real filtering beyond assignee-kind + text query; no way to filter by status/priority/label/date/property, no facet counts | **L** |
| 6 | Custom `issue_property` catalog (typed, select/multi_select options, archivable) + separate `metadata` KV scratch | Neither | No user-defined custom fields, no agent-pipeline-state scratch space on the issue | **M–L** |
| 7 | `assignee_type` single enum incl. `squad`, resolved via `WillEnqueueRun` shared predicate for create/assign/status-promote triggers, self-loop + private-agent gated, preview endpoint | `squad_id` bolted on as a separate column from `assignee_type/id`; no shared "will this start a run" predicate exposed for preview | Squad-assignment model less unified; no dry-run "what will happen" preview for issue writes | **M** |
| 8 | `origin_type`/`origin_id` provenance (autopilot/quick_create/lark_chat/slack_chat/agent_create) for attribution chains | None found | Cannot trace who/what ultimately caused an agent-created issue | **S–M** |
| 9 | 7-state lifecycle incl. `blocked`, `cancelled`, DB-level CHECK constraint | 5-state lifecycle, no `blocked`/`cancelled`, no CHECK (free text) | Cannot represent "blocked" or "cancelled" as first-class terminal/blocking states | **S** |
| 10 | Workspace-global float `position` + anchor-based (`before_id`/`after_id`) move endpoint, 409 on stale/exhausted anchors | Column-local integer `ord`, simpler reorder | Lower fidelity reordering across columns/views but functionally adequate for board-only use | **S** |
| 11 | Create wizard equivalent (`CreateIssueRequest`) accepts priority/status/labels/acceptance_criteria/parent/dates directly | Create wizard (7 rows) omits priority/due/labels/acceptance-criteria entirely even though schema supports priority/due/labels since 0014 | UI hasn't caught up to schema — quick win since columns already exist | **S** |
| 12 | `issue_subscriber` (notification subscriptions, reason-tagged) + `issue_reaction` (emoji reactions) | Neither | No subscription model, no reactions | **M** |

**Top-ranked (do first):** #1 (comment-mention dispatch) and #2 (subtasks/parent-child) are the two gaps that change what the product *is*, not just what it displays — everything downstream (stage barriers, cascade notifications, mention-based handoff) builds on them. #11 is the cheapest real win (schema already has priority/due/labels; wizard just needs three more rows).
