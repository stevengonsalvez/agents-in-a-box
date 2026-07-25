# Multica ↔ Hangar Parity Reference

The ground-truth reference for how far Hangar (ainb's control plane) is from
Multica's behavioral model, entity by entity. Drive the roadmap from this
README; dive into the per-entity files for field-level detail, schema
citations, and `file:line` references.

## Purpose & how to use

- **What this is:** a durable, verify-against-it map of what Multica does that
  Hangar does not (and the handful of things Hangar already does *better*).
  Each entity file was produced by a deep-dive over both codebases and carries
  full field tables, migration references, and source citations.
- **Source of the reference:** Multica — `github.com/multica-ai/multica`,
  fair-source Apache-2.0. This is a **behavioral reference, not a verbatim
  port**: read it for the *shape* of a feature (what fields exist, what the
  dispatch predicate decides, how a trigger fans out), then re-implement in
  Hangar's Rust/SQLite idiom. Do not copy source line-by-line.
- **The north star:** Multica's organizing principle is **"agents are team
  members, not tools."** Concretely that means a *polymorphic actor* model —
  every "who did this" column is `actor_type ('member'|'agent') + actor_id`, so
  agents own issues, post comments, receive inbox items, and get @mentioned
  exactly like humans. Most of Hangar's biggest gaps trace back to it being
  **agent-centric** (agents are the thing that does work) rather than
  **actor-polymorphic** (agents and humans are symmetric collaborators). When
  ranking work, "does this move Hangar toward actor symmetry?" is the tie-breaker.

## Entity files

| File | Entity | Rough parity | Headline gap |
|---|---|---|---|
| [issue.md](issue.md) | Issue | ~35% | No comment @mention dispatch; no subtasks |
| [squad.md](squad.md) | Squad | ~40% | Leader gets no briefing — "leader" is nominal |
| [workspace.md](workspace.md) | Workspace / membership | ~40% | Single singleton workspace; no human members |
| [agent.md](agent.md) | Agent | ~55% | No 2-dim presence; no agent builder |
| [autopilot.md](autopilot.md) | Autopilot | ~75% | No rule-versioning/attribution; no `api` trigger |
| [task-flow.md](task-flow.md) | Task + dispatch flow | ~80% | No dispatch reason codes; no activity log |

Parity % is a rough eyeball, not a measured metric — it exists to signal where
the structural holes are (Issue/Squad/Workspace), versus where Hangar is already
close (Task-flow, Autopilot, where it is *ahead* on some axes).

## Master gap matrix

One ranked table across all entities. Overlapping gaps (the polymorphic-actor
gap surfaces in Workspace + Issue + Squad; comment-dispatch underpins Squad
coordination too) are **stated once** at their foundational home and
cross-referenced. Ranked by "changes what the product *is*" first, then
user-visible impact, then effort. Effort: **S** ≈ days, **M** ≈ 1–2 weeks,
**L** ≈ multi-week / touches many surfaces.

| # | Entity | Gap | Multica has | Hangar has | Impact (what it unlocks) | Effort |
|---|---|---|---|---|---|---|
| **1** | Cross-cutting | **Polymorphic actors + human members** | `actor_type (member\|agent)` on assignee/creator/comment/inbox/activity; humans are first-class collaborators | Agent-centric; no human assignee/commenter/inbox concept; `user`+`member` tables exist but nothing mints a 2nd human | The whole "agents are team members" identity. Every collaboration surface (assign, comment, mention, inbox) becomes symmetric. Foundation the mention-dispatch and squad-human-members gaps sit on | L |
| **2** | Issue / Squad | **Comment @mention auto-dispatch** | `[@x](mention://type/id)` parsed → routes to explicit-mention / reply-parent / thread-owner / assignee-fallback; per-target `queued\|coalesced\|deferred\|blocked` outcomes; preview; self-loop + private-agent gates; merge-into-pending dedup | Comments are inert text — zero side effects on write | Hand off work by *conversation*. This is the single biggest behavioral feature Hangar lacks — it is also the mechanism squad leader→member delegation and re-trigger loops are built on | L |
| **3** | Issue | **Subtasks: parent/child + stage barriers + child-done cascade** | `parent_issue_id` self-FK; `stage` barrier groups; child terminal→parent wake comment; batched multi-stage aggregation | No parent/child of any kind; only an untyped `card_dependency` blocks-edge | Decompose an issue into tracked sub-issues with roll-up progress and automatic parent wake when a stage closes | L |
| **4** | Workspace | **Multi-workspace (create / switch / delete)** | `CreateWorkspace` API + `/{slug}/…` nav, reserved-slug validation, per-instance creation lockdown flag | Exactly one bootstrapped singleton (`default`); no create path at any layer | More than one project/tenant at all. Everything below (invites, roles, per-workspace config) is moot with one workspace | L |
| **5** | Squad | **Task-level `squad_id` + claim-time leader briefing hook** | `agent_task_queue.squad_id` (mig 127) so the daemon injects briefing at claim | Task rows carry no `squad_id`; issue has `squad_id` but the task doesn't — no claim-time hook to key briefing off | Structural prerequisite for a real squad leader (gap #7). Cheap column, but unlocks the whole leader-coordination model | S |
| **6** | Agent | **Two-dimensional derived presence** | `Availability` (online/unstable/offline/archived, 5-min "unstable" grace) × `Workload` (working/queued/idle from live task counts) — pure derivation | `agent.archived` bool + `agent_runtime.status` binary online/offline; no grace window, no workload signal | Every list/card dot. Users can tell "runtime blipped" from "dead", and "queued/stuck" from "idle" — the signal Multica's whole list UI is built on | M |
| **7** | Squad | **Leader briefing (Operating Protocol + Roster + Instructions)** | System-prompt injection at claim: coordinator role + 6 responsibilities, ready-to-paste mention roster with skill names, user-authored routing instructions | Nothing — a fanned-out leader runs like a solo agent, no idea it's a leader | Makes "squad leader" a real role instead of nominal. Depends on #5 (task `squad_id`) + #2 (mention dispatch for delegation) | L |
| **8** | Agent | **Invocation permissions (private / public_to + allow-list)** | `permission_mode` + `agent_invocation_target` (workspace/member/team, OR-matched); admin does NOT bypass private | `visibility` (workspace/private) only; no allow-list, no `canInvokeAgent` gate | Share one agent with a *subset* of people; an auditable invoke gate. Depends on #1 for member targets to exist | M |
| **9** | Agent | **Conversational Agent Builder** | Chat with a hidden `kind='system'` agent that proposes name/description/instructions/model/skills/permission as a structured draft the user confirms | Bare name-input field (TUI) or flag-driven CLI; no assisted authoring | Non-technical users create good agents without hand-writing instructions or guessing model ids. Needs `kind`/`system_key` first | L |
| **10** | Issue | **Structured / faceted filtering** | status/priority/assignee/creator/label/property/date-range facets, facet-value-with-counts, 2-level grouping, cursor pagination | 4 fixed chips (All/Members/Agents/Mine) + free-text substring | Actually find issues at scale. No filter by status/priority/label/date exists today | L |
| **11** | Issue | **Acceptance criteria + context refs** | `acceptance_criteria` JSONB structured list + `context_refs` JSONB | Single free-text `external_ref` string | Give an agent a structured definition-of-done and multiple linked context items instead of one URL | M |
| **12** | Task | **Dispatch reason codes** | `dispatch.ReasonCode` enum (`invocation_not_allowed`/`target_unavailable`/`runtime_offline`/`attribution_blocked`/`already_active`/`self_trigger_suppressed`/…), wire-shared | Only `FailureReason` (post-hoc run failure); nothing for admission-time skips | "Why didn't this run?" observability for an issue that never produced a task at all | M |
| **13** | Task | **Generic activity log / audit trail** | `activity_log(actor_type, actor_id, action, details)` fed by bus listeners → per-issue timeline | `run_history` (task-execution-scoped only) | A per-issue narrative of every assign/label/comment/status event, not just run outcomes | M |
| **14** | Autopilot | **Rule versioning + human attribution** | `autopilot_rule_version` append-only per substantive publish; `originator` vs `accountable_user` split | None | Who is accountable for an unattended run — matters once autopilots are shared. Related to #1/#13 | M |
| **15** | Autopilot | **`api` trigger kind + `skipped` run status** | 3rd trigger kind (bare programmatic fire); `skipped` a first-class run row | schedule + webhook only; policy-skip is a log event, not a queryable row | CI/other-tool fire without minting a webhook secret; reporting can tell "skipped" from "never happened" | S |
| **16** | Squad | **Selective leader routing vs spray fan-out** | Leader reads issue, picks the *fit* member(s) by skill/role, delegates via @mention; others get no task | `assign_fanout` sprays to *every* agent member concurrently, no selection | Product decision, not a pure gap: mediated routing vs parallel-worker fan-out. Depends on #2 + #7. Decide before "fixing" | L |
| **17** | Issue | **Custom properties + metadata scratch** | `issue_property` typed catalog (select/multi_select, archivable) + flat `metadata` KV bag for agent pipeline state | Neither | User-defined fields (Linear/Notion-style) + a place for agents to stash pipeline state (PR#, status) | M–L |
| **18** | Workspace | **Membership lifecycle (invite → accept/expire)** | `workspace_invitation` (7-day expiry, one-pending-per-email, stale sweep), auto-stub user, role-at-invite | `MemberRepo` list/set-role/remove + last-owner guard, but nothing *creates* a 2nd member | Add a second human. Repo primitives already exist; needs invite table + accept flow. Depends on #4 | M |
| 19 | Issue | **`blocked` + `cancelled` states** | 7-state lifecycle with DB CHECK | 5 states, no `blocked`/`cancelled`, no CHECK (free text) | Represent blocked/cancelled as first-class states | S |
| 20 | Issue | **Typed dependency graph** | `issue_dependency` `blocks`/`blocked_by`/`related` | `card_dependency` single untyped blocks-edge (cycle-checked, auto-run) — core mechanic already parity | "related"/reverse edges + a browsable graph | S–M |
| 21 | Issue | **Origin provenance** | `origin_type`/`origin_id` (autopilot/quick_create/lark/slack/agent_create) | None | Trace who/what caused an agent-created issue | S–M |
| 22 | Issue | **Subscribers + reactions** | `issue_subscriber` (reason-tagged) + `issue_reaction` | Neither | Notification subscriptions + emoji reactions | M |
| 23 | Agent | **Metadata columns** | `description` (255-cap), `avatar_url`, `kind`(user/system), `service_tier`, `UNIQUE(workspace,name)` | None of these | Blurb/avatar in lists; no silent duplicate names; Codex service-tier control. `kind`/`system_key` also unblocks #9 | S–M |
| 24 | Agent | **Per-agent skill enable/disable** | `agent_skill.enabled` + `disabled_runtime_skills` | Attach/detach only, no toggle | Temporarily disable a skill for one agent without detaching | S |
| 25 | Squad | **Per-member `role` + `instructions` + archive** | Free-text `role` (leader routes by fit), `squad.instructions`, `archived_at`/`archived_by` w/ transfer-on-archive | None of these columns | Route by stated specialty; per-squad routing guidance; safe squad retirement | S–M |
| 26 | Agent / Squad | **Archive audit trail** | `archived_at` + `archived_by` (who/when) | `archived` boolean only (agent); no archive at all (squad) | Accountability for who retired an agent/squad and when | S |
| 27 | Autopilot | **Subscriber / collaborator model** | `autopilot_subscriber` (auto-subscribe to spawned issues) + `autopilot_collaborator` write-grants | Single-owner only | Team-shared autopilots. Depends on #1. Fine to defer while solo | S–M |
| 28 | Issue | **Surface existing priority/due/labels in create wizard** | `CreateIssueRequest` accepts priority/status/labels/dates directly | Schema has priority/due/labels since mig 0014 — **wizard never surfaces them** | Cheapest real win: three more wizard rows, columns already exist | S |
| 29 | Squad | **#450: Boards `q:squad` hotkey unreachable** | (n/a) | Global router steals bare `q` as quit before Boards' `q → AssignSquad`; card fan-out unreachable by keyboard | Unblocks the *existing* fan-out feature. Add a Boards-no-overlay guard, same pattern as existing screen guards | S |
| 30 | Agent | **`custom_env` redaction contract** | Never serialized; `has_custom_env`/key-count only; audited GET/PUT endpoint | Stored/returned plain JSON | Secrets hygiene if Hangar ever grows multi-user/remote | S |

## Deliberately NOT chasing

The task-flow deep-dive flagged several "gaps" that are **moot given Hangar's
architecture** (single-process daemon colocated with SQLite, one TUI client, no
browser/remote client). Listing them so nobody re-opens them:

| Multica feature | Why it's not a gap for Hangar |
|---|---|
| `EmptyClaim` Redis short-circuit cache | SQLite claim is one local statement — no network round-trip to amortize |
| `reconcileBroadcaster` one-slot replay | Daemon + DB are colocated; no daemon↔server WS reconnect to recover from |
| `FinalizeTaskClaim` split-commit + requeue-on-payload-fail | Claim + dispatch are same-process; no network boundary between "claimed" and "daemon has payload" |
| Two-hub fan-out (browser realtime.Hub + daemon wakeup) | No browser client to fan out to; the TUI plugin is the only subscriber and gets full payloads directly |
| Authoritative-vs-estimated cost split (`cost_usd_ticks`) | `run_history` + `cost_rollup` VIEW is adequate at Hangar's scale; cosmetic |
| Generic DB-backed lease/scope scheduler | Purpose-built single-daemon scheduler is fine; no multi-instance lease contention |

And two axes where **Hangar is ahead of Multica**:

- **`concurrency_policy` (skip/queue/replace)** — Hangar's is fully wired and
  tested (`scheduler.rs` + `scheduler_concurrency_policies.rs`); Multica's is a
  **dead schema column** read nowhere.
- **Squad fan-out** — Hangar dispatches leader + every agent member concurrently
  (each its own worktree); Multica only dispatches the leader. (Whether spray or
  mediated routing is *wanted* is gap #16, a product call.)

Parity axes already reached: autopilot `execution_mode`, autopilot webhook
delivery-audit + event-filters, the per-(issue,agent) claim guard + concurrency
cap, the dispatched/reclaim/timeout sweeper ladder.

## Prioritized roadmap

Grouped by tier. Within a tier, items are roughly dependency-ordered.

### P0 — Foundational (structural; other work depends on these)

| # | Item | Effort | Unblocks |
|---|---|---|---|
| 1 | Polymorphic actors + human members (member\|agent everywhere) | L | #2, #8, #18, #25-human, autopilot attribution |
| 5 | Task-level `squad_id` + claim-time briefing hook | S | #7 (leader briefing) |
| 3 | Subtasks: `parent_issue_id` + stage barriers + child-done cascade | L | roll-up progress, staged completion |
| 4 | Multi-workspace (create/switch) | L | #18 invites, per-workspace config, slug validation |

Cheapest P0 to land first: **#5** (one column + a claim hook) makes the squad
leader briefing (#7) possible and is nearly free.

### P1 — High-impact (the features that make agents feel like team members)

| # | Item | Effort |
|---|---|---|
| 2 | Comment @mention auto-dispatch (route + outcomes + preview + gates) | L |
| 7 | Squad leader briefing (protocol + roster + instructions) | L |
| 6 | Two-dimensional derived agent presence (availability × workload) | M |
| 8 | Invocation permissions (private/public_to + allow-list) | M |
| 9 | Conversational Agent Builder | L |
| 10 | Structured / faceted issue filtering | L |
| 11 | Acceptance criteria + context refs | M |
| 12 | Dispatch reason codes | M |
| 13 | Generic activity log / audit trail | M |
| 14 | Autopilot rule versioning + human attribution | M |
| 16 | Squad selective routing (product decision first) | L |

### P2 — Polish / cheap wins (several are UI-only; schema already has the column)

| # | Item | Effort | Note |
|---|---|---|---|
| 28 | Surface priority/due/labels in issue create wizard | S | **schema already has these (mig 0014)** — UI-only |
| 29 | Fix #450 Boards `q:squad` hotkey | S | unblocks existing fan-out feature |
| 19 | `blocked` + `cancelled` issue states | S | |
| 23 | Agent metadata columns (description/avatar/kind/service_tier/unique-name) | S–M | `kind`/`system_key` also unblocks #9 |
| 24 | Per-agent skill enable/disable toggle | S | |
| 26 | Archive audit trail (agent + squad `archived_at`/`archived_by`) | S | |
| 25 | Squad per-member `role` + `instructions` + archive | S–M | `instructions` feeds #7 briefing |
| 20 | Typed issue dependency graph (`blocked_by`/`related`) | S–M | core auto-run mechanic already parity |
| 21 | Issue origin provenance (`origin_type`/`origin_id`) | S–M | |
| 15 | Autopilot `api` trigger + `skipped` status | S | |
| 30 | `custom_env` redaction contract | S | |
| 17 | Custom properties + metadata scratch | M–L | larger; promote to P1 if agent-pipeline-state is needed |
| 22 | Issue subscribers + reactions | M | |
| 27 | Autopilot subscriber/collaborator | S–M | depends on #1; defer while solo |
| 18 | Membership invite lifecycle | M | depends on #4; defer while single-workspace |

**Cheap-wins where the schema already has it and only the UI is missing:**
issue **priority / due_date / labels** (columns since mig 0014, wizard omits
them — #28), and the `card_dependency` auto-run mechanic (#20 core already
built, only the typed-edge variants + graph browse are missing).
