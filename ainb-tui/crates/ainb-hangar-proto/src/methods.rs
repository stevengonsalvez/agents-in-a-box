//! Daemon JSON-RPC method-name registry.
//!
//! These are the methods the Hangar **daemon** speaks over its
//! `~/.agents-in-a-box/hangar.sock` socket. They sit on the same JSON-RPC 2.0
//! envelope ([`crate::RpcRequest`] / [`crate::RpcResponse`]) the host
//! plugin caps mediate. P3.7's plugin connection state machine sends
//! [`WORKSPACE_SUBSCRIBE`] right after dialling and renders
//! `"Hangar: Connected"` once the daemon acknowledges.
//!
//! Method names are namespaced (`<area>/<verb>`) except [`PING`], which
//! is the canonical bare liveness probe. The [`ALL_METHODS`] slice is the
//! single source of truth used by the uniqueness / namespacing tests.

/// `workspace/subscribe` — open a workspace event subscription.
///
/// Params: `{ workspace_id: String }`. Result: the current workspace
/// snapshot (empty on a fresh store). After the ack the daemon pushes
/// workspace events on the same stream.
pub const WORKSPACE_SUBSCRIBE: &str = "workspace/subscribe";

/// `workspace/list` — list the workspaces visible to the caller.
///
/// Params: `{}`. Result: `{ workspaces: [...] }`.
pub const WORKSPACE_LIST: &str = "workspace/list";

/// `hangar/issues_list` — snapshot the issues of a workspace.
///
/// Params: `{ workspace_id: String }`. Result: `{ issues: [IssueRow, ...] }`
/// (every lifecycle state; the plugin buckets them into Todo / In Progress /
/// Done client-side). Drives the issue-list landing screen (P4.3).
pub const HANGAR_ISSUES_LIST: &str = "hangar/issues_list";

/// `hangar/issues_search` — ranked full-text-ish issue search (e38.12).
///
/// Params: [`crate::snapshots::IssueSearchParams`] (`{ workspace_id, query }`).
/// Result: [`crate::snapshots::IssuesListResult`] — the matching [`IssueRow`]s in
/// ranked order. A row matches when the case-insensitive `query` substring appears
/// in the issue title, description, OR any of its comment bodies; rows are ranked
/// title > description > comment (strongest surface per issue wins) and ordered
/// strongest-first. Reaches beyond the loaded page and into description / comment
/// bodies, which the plugin's client-side `/` title-only filter cannot. A blank
/// query matches nothing.
///
/// Workspace-scoped like [`HANGAR_ISSUES_LIST`]: a sibling tenant's matching issue
/// is never returned, and an unknown workspace yields an empty result (a read, so
/// no `INVALID_PARAMS` rejection — mirrors the list snapshot).
///
/// [`IssueRow`]: crate::events::IssueRow
pub const HANGAR_ISSUES_SEARCH: &str = "hangar/issues_search";

/// `hangar/search` — ranked cross-entity command-palette search (e38.13).
///
/// Params: [`crate::snapshots::SearchParams`] (`{ workspace_id, query }`). Result:
/// [`crate::snapshots::SearchResult`] — ranked [`crate::snapshots::SearchEntry`]s
/// across the workspace's issues, agents, skills, AND autopilots. An entry matches
/// when the case-insensitive `query` substring appears in the entity's
/// human-readable field (issue title / agent name / skill name / autopilot name);
/// entries are ranked exact-match first, then prefix, then substring, ties broken
/// by a stable kind order (issues, agents, skills, autopilots) and label. Each
/// entry carries `{ kind, id, label, screen }` so the palette can JUMP to the
/// selected entity's screen. A blank query matches nothing.
///
/// This is the cross-entity superset of [`HANGAR_ISSUES_SEARCH`] (which only
/// reaches issues): the command palette (`Ctrl+P`) needs to jump across *all* four
/// entity kinds, which a per-screen `/` filter and the issue-only search cannot.
/// Workspace-scoped like every read: a sibling tenant's matching entity is never
/// returned, and an unknown workspace yields an empty result (a read, so no
/// `INVALID_PARAMS` rejection — mirrors the list snapshot).
pub const HANGAR_SEARCH: &str = "hangar/search";

/// `hangar/agents_list` — snapshot the assignable actors of a workspace.
///
/// Params: `{ workspace_id: String }`. Result: `{ actors: [ActorRow, ...] }`
/// (members + agents in one polymorphic list). Drives the agent-picker modal
/// (P4.5).
pub const HANGAR_AGENTS_LIST: &str = "hangar/agents_list";

/// `hangar/skills_list` — snapshot the skills of a workspace.
///
/// Params: `{ workspace_id: String }`. Result: `{ skills: [SkillRow, ...] }`.
/// Drives the skill-manager list (P4.6).
pub const HANGAR_SKILLS_LIST: &str = "hangar/skills_list";

/// `hangar/skill_get` — fetch one skill's full detail (body + files) by id.
///
/// Params: `{ workspace_id: String, skill_id: String }`. Result: a
/// [`crate::snapshots::SkillDetail`] (the SKILL.md body + ordered file list), or
/// `null` when the id resolves to no skill in the subscribed workspace. Drives
/// the skill-manager detail pane (P6.5). The lookup is workspace-scoped: a skill
/// id from another tenant resolves to `null`, never another workspace's row.
pub const HANGAR_SKILL_GET: &str = "hangar/skill_get";

/// `hangar/skills_sync` — import the curated toolkit skills into a workspace.
///
/// Params: `{ workspace_id: String, source_path: Option<String> }`. Result: a
/// [`crate::snapshots::SkillsSyncResult`] (the imported skill names + count).
/// The `s` key on the skill-manager screen invokes this (P6.5). Idempotent on
/// `(workspace_id, name)` — re-running updates existing rows in place.
pub const HANGAR_SKILLS_SYNC: &str = "hangar/skills_sync";

/// `hangar/skill_attach` — attach a skill to an agent within a workspace.
///
/// Params: `{ workspace_id: String, agent_id: String, skill_id: String }`.
/// Result: `{}`. The `i` key attaches the selected skill to the selected agent
/// (P6.5). Workspace-scoped: both ids must belong to the subscribed workspace or
/// the daemon rejects with an error (the tenant-isolation guard).
pub const HANGAR_SKILL_ATTACH: &str = "hangar/skill_attach";

/// `hangar/skill_detach` — detach a skill from an agent within a workspace.
///
/// Params: `{ workspace_id: String, agent_id: String, skill_id: String }`.
/// Result: `{}`. The `d` key detaches (P6.5). Idempotent (detaching an absent
/// link is a no-op) and workspace-scoped like [`HANGAR_SKILL_ATTACH`].
pub const HANGAR_SKILL_DETACH: &str = "hangar/skill_detach";

/// `hangar/autopilots_list` — snapshot the autopilots of a workspace.
///
/// Params: `{ workspace_id: String }`. Result: a
/// [`crate::snapshots::AutopilotsListResult`] (every autopilot row in the
/// workspace, ordered by name). Drives the autopilot-manager table (P7.5).
pub const HANGAR_AUTOPILOTS_LIST: &str = "hangar/autopilots_list";

/// `hangar/autopilot_runs` — snapshot one autopilot's recent runs.
///
/// Params: `{ workspace_id: String, autopilot_id: String, limit: u32 }`. Result:
/// a [`crate::snapshots::AutopilotRunsResult`] (latest-first run history, capped
/// at `limit`). Drives the run-history pane below the selected autopilot (P7.5).
/// Workspace-scoped: a foreign autopilot id yields an empty set.
pub const HANGAR_AUTOPILOT_RUNS: &str = "hangar/autopilot_runs";

/// `hangar/autopilot_fire_now` — manually fire one autopilot's tick immediately.
///
/// Params: `{ workspace_id: String, autopilot_id: String }`. Result: `{}`.
/// Bypasses the schedule and runs the P7.4 enqueue path now (`r`/"run now" on the
/// manager screen, P7.5). Workspace-scoped: a foreign id fires nothing.
pub const HANGAR_AUTOPILOT_FIRE_NOW: &str = "hangar/autopilot_fire_now";

/// `hangar/autopilot_set_enabled` — enable or disable one autopilot.
///
/// Params: `{ workspace_id: String, autopilot_id: String, enabled: bool }`.
/// Result: `{}`. `false` disables (the scheduler stops considering it); `true`
/// re-enables and recomputes `next_tick_at` from now (no missed-tick replay). The
/// `d` key toggles the selected autopilot (P7.5). Workspace-scoped.
pub const HANGAR_AUTOPILOT_SET_ENABLED: &str = "hangar/autopilot_set_enabled";

/// `hangar/tasks_list` — snapshot the task queue of a workspace for the Kanban
/// board (P8.4).
///
/// Params: `{ workspace_id: String }`. Result: a
/// [`crate::snapshots::TasksListResult`] (every task row in the workspace, each
/// carrying its raw lifecycle `status`). The plugin buckets the six statuses into
/// the four board columns client-side (queued+dispatched → queued, running →
/// running, done → done, failed+cancelled → failed). Workspace-scoped: a foreign
/// id yields an empty set.
pub const HANGAR_TASKS_LIST: &str = "hangar/tasks_list";

/// `hangar/task_transition` — move one task to a new lifecycle status (P8.4).
///
/// Params: `{ workspace_id: String, task_id: String, to_status: String }`.
/// Result: `{}`. Drives the store FSM column-move when a Kanban card is dragged
/// across columns (`Shift+←` / `Shift+→`). Workspace-scoped: a foreign task id
/// touches no row. The `to_status` must be one of the six
/// [`ainb_hangar_core::task_status::TaskStatus`] wire tokens; an illegal token or
/// transition is an `INVALID_PARAMS` error.
pub const HANGAR_TASK_TRANSITION: &str = "hangar/task_transition";

/// `hangar/issue_update` — edit fields of one existing issue (e38.8).
///
/// Params: [`crate::snapshots::IssueUpdateParams`]
/// (`{ workspace_id, issue_id, state?, assignee?, priority?, due_date? }`).
/// Result: the refreshed [`crate::events::IssueRow`], or an error. Each `Option`
/// field is "leave unchanged when absent"; `assignee` additionally distinguishes
/// "clear the assignee" (an explicit JSON `null`) from "leave it" (the key
/// omitted) via its [`crate::snapshots::FieldUpdate`] wrapper. The four editable
/// fields are the real `issue` columns (`state` / `assignee` / `priority` /
/// `due_date`); there is no `project` column at v1, so project is not editable.
///
/// Mutating + workspace-scoped: the daemon resolves the workspace and rejects a
/// mistyped one with `INVALID_PARAMS` (never a silent no-op, mirroring
/// `hangar/task_transition`), and the update is scoped by `(id, workspace_id)`
/// so a foreign-tenant issue id touches no row. After a committed edit the
/// daemon pushes the matching [`crate::events::HangarEvent::IssueUpdated`].
pub const HANGAR_ISSUE_UPDATE: &str = "hangar/issue_update";

/// `hangar/issue_create` — create one new issue in a workspace (e38.29).
///
/// Params: [`crate::snapshots::IssueCreateParams`]
/// (`{ workspace_id, title, description?, creator }`). Result: the persisted
/// [`crate::events::IssueRow`], or an error. The daemon mints the issue id (a
/// fresh ULID), stamps `created_at`, and inserts the row in the `open` lifecycle
/// state. `creator` is a polymorphic actor-ref (`"agent:<id>"` / `"member:<id>"`);
/// `title` is the issue title (a blank title is rejected with `INVALID_PARAMS`,
/// never an empty row).
///
/// Mutating + workspace-scoped, mirroring [`HANGAR_COMMENT_ADD`]: the daemon
/// resolves the workspace and rejects a mistyped one with `INVALID_PARAMS`
/// (never a silent no-op). After a committed insert the daemon pushes the
/// matching [`crate::events::HangarEvent::IssueCreated`] so a subscribed issue
/// list re-renders the new row without re-pulling the whole snapshot.
pub const HANGAR_ISSUE_CREATE: &str = "hangar/issue_create";

/// `hangar/issue_label_attach` — attach a label to one issue (e38.10).
///
/// Params: [`crate::snapshots::IssueLabelParams`]
/// (`{ workspace_id, issue_id, name, color? }`). Result: the refreshed
/// [`crate::events::IssueRow`], or an error. The `name` is resolve-or-created
/// within the workspace (a fresh label carries the optional `color`; an existing
/// label is reused and its colour left as-is). The attach is idempotent —
/// attaching the same label twice leaves exactly one link.
///
/// Mutating + workspace-scoped, mirroring [`HANGAR_ISSUE_UPDATE`]: the daemon
/// resolves the workspace and rejects a mistyped one with `INVALID_PARAMS`
/// (never a silent no-op), and the mutation is scoped by `(issue_id,
/// workspace_id)` so a foreign-tenant issue id writes no join row (a not-found
/// error, never a cross-tenant attach). After a committed attach the daemon
/// pushes the matching [`crate::events::HangarEvent::IssueUpdated`] so a
/// subscribed issue list re-renders the new chip.
pub const HANGAR_ISSUE_LABEL_ATTACH: &str = "hangar/issue_label_attach";

/// `hangar/issue_label_detach` — detach a label from one issue (e38.10).
///
/// Params: [`crate::snapshots::IssueLabelParams`]
/// (`{ workspace_id, issue_id, name, color? }` — `color` is ignored on detach).
/// Result: the refreshed [`crate::events::IssueRow`], or an error. Detaching an
/// absent link (an unknown label name, or one never attached) is a no-op, so
/// detach is idempotent. The label definition itself is left intact (it can be
/// shared across issues); only the link is removed.
///
/// Mutating + workspace-scoped like [`HANGAR_ISSUE_LABEL_ATTACH`]: a
/// foreign-tenant issue id touches no link and is rejected as a not-found error.
/// A committed detach pushes the matching
/// [`crate::events::HangarEvent::IssueUpdated`].
pub const HANGAR_ISSUE_LABEL_DETACH: &str = "hangar/issue_label_detach";

/// `hangar/comment_add` — append a comment to one issue (e38.5).
///
/// Params: [`crate::snapshots::CommentAddParams`]
/// (`{ workspace_id, issue_id, author, body }`). Result: the persisted
/// [`crate::events::CommentRow`], or an error. The `author` is a polymorphic
/// actor-ref (`"agent:<id>"` / `"member:<id>"`); `body` is the comment text.
///
/// Mutating + workspace-scoped, mirroring [`HANGAR_ISSUE_UPDATE`]: the daemon
/// resolves the workspace and rejects a mistyped one with `INVALID_PARAMS`
/// (never a silent no-op), and the insert is scoped by `(issue_id, workspace_id)`
/// through a join to `issue` so a foreign-tenant issue id writes no row (a
/// not-found error, never a cross-tenant comment). After a committed insert the
/// daemon pushes the matching [`crate::events::HangarEvent::CommentAdded`] so a
/// subscribed task-detail screen re-renders the new comment.
pub const HANGAR_COMMENT_ADD: &str = "hangar/comment_add";

/// `hangar/agent_update` — edit one agent's config knobs (e38.15).
///
/// Params: [`crate::snapshots::AgentUpdateParams`]
/// (`{ workspace_id, agent_id, name?, instructions?, model?, cli_args?,
/// mcp_config?, thinking?, agent_env? }`). Result: the refreshed
/// [`crate::events::ActorRow`] for the edited agent, or an error. Each optional
/// field is "leave unchanged when absent"; the nullable text fields
/// (`instructions` / `model` / `mcp_config` / `thinking`) additionally
/// distinguish "clear to the default" (explicit `null`) from "leave it" (key
/// omitted) via their [`crate::snapshots::FieldUpdate`] wrapper.
///
/// Mutating + workspace-scoped, mirroring [`HANGAR_ISSUE_UPDATE`]: the daemon
/// resolves the workspace and rejects a mistyped one with `INVALID_PARAMS`
/// (never a silent no-op), and the update is scoped by `(agent_id, workspace_id)`
/// so a foreign-tenant agent id touches no row (a not-found error). This bead
/// persists + exposes the config; the provider EXEC consumption of `model`/`args`
/// is a separate bead (e38.16).
pub const HANGAR_AGENT_UPDATE: &str = "hangar/agent_update";

/// `hangar/agent_archive` — archive or un-archive one agent (e38.15).
///
/// Params: [`crate::snapshots::AgentArchiveParams`]
/// (`{ workspace_id, agent_id, archived }`). Result: the refreshed
/// [`crate::events::ActorRow`] for the agent, or an error. `archived: true`
/// hides the agent from the active picker; `false` restores it.
///
/// Mutating + workspace-scoped like [`HANGAR_AGENT_UPDATE`]: a foreign-tenant
/// agent id flips no row and is rejected as a not-found error.
pub const HANGAR_AGENT_ARCHIVE: &str = "hangar/agent_archive";

/// `hangar/members_list` — snapshot the human members of a workspace (e38.11).
///
/// Params: [`crate::snapshots::WorkspaceScopedParams`] (`{ workspace_id }`).
/// Result: [`crate::snapshots::MembersListResult`] — the workspace's members
/// (`user_id` + `email` + `role`), ordered by email. Drives the settings Members
/// pane. Workspace-scoped like every snapshot: a foreign / unknown workspace
/// yields an empty list (a read, so no `INVALID_PARAMS` rejection — mirrors
/// `agents_list`), never another tenant's members.
pub const HANGAR_MEMBERS_LIST: &str = "hangar/members_list";

/// `hangar/member_set_role` — change one member's role within a workspace (e38.11).
///
/// Params: [`crate::snapshots::MemberSetRoleParams`]
/// (`{ workspace_id, user_id, role }`). Result: the refreshed
/// [`crate::snapshots::MembersListResult`] for the workspace, or an error. `role`
/// must be one of `owner`/`admin`/`member` (an illegal token is `INVALID_PARAMS`).
///
/// Mutating + workspace-scoped, mirroring [`HANGAR_AGENT_UPDATE`]: the daemon
/// resolves the workspace and **rejects** a mistyped one with `INVALID_PARAMS`
/// (never a silent no-op), and the edit is scoped by `(workspace_id, user_id)` so
/// a foreign-tenant member touches no row (a not-found error). Demoting the
/// workspace's *only* owner is rejected so a workspace always keeps an owner.
pub const HANGAR_MEMBER_SET_ROLE: &str = "hangar/member_set_role";

/// `hangar/member_remove` — remove one member from a workspace (e38.11).
///
/// Params: [`crate::snapshots::MemberRemoveParams`] (`{ workspace_id, user_id }`).
/// Result: the refreshed [`crate::snapshots::MembersListResult`] for the
/// workspace, or an error. The `user` row itself is left intact (a user may
/// belong to other workspaces); only the membership join is dropped.
///
/// Mutating + workspace-scoped like [`HANGAR_MEMBER_SET_ROLE`]: a foreign-tenant
/// member touches no row (a not-found error). Removing the workspace's *only*
/// owner is rejected so a workspace always keeps an owner.
pub const HANGAR_MEMBER_REMOVE: &str = "hangar/member_remove";

/// `hangar/squads_list` — snapshot the squads of a workspace (e38.17).
///
/// Params: [`crate::snapshots::WorkspaceScopedParams`] (`{ workspace_id }`).
/// Result: [`crate::snapshots::SquadsListResult`] — the workspace's squads, each
/// with its leader actor-ref and member actor-refs, ordered by name. Drives the
/// `ainb hangar squad list` status view. Workspace-scoped like every snapshot: a
/// foreign / unknown workspace yields an empty list (a read, so no
/// `INVALID_PARAMS` rejection — mirrors `members_list`), never another tenant's
/// squads.
pub const HANGAR_SQUADS_LIST: &str = "hangar/squads_list";

/// `hangar/squad_create` — create one squad with a leader in a workspace (e38.17).
///
/// Params: [`crate::snapshots::SquadCreateParams`]
/// (`{ workspace_id, name, leader }`). Result: the refreshed
/// [`crate::snapshots::SquadsListResult`] for the workspace, or an error.
/// `leader` is a polymorphic actor-ref (`"agent:<id>"` / `"member:<id>"`) — the
/// actor a squad-assigned task routes to (an `agent` leader's id becomes the
/// task's `agent_id`). The leader is how leader-routing takes effect rather than a
/// new `ActorKind::Squad`.
///
/// Mutating + workspace-scoped, mirroring [`HANGAR_MEMBER_SET_ROLE`]: the daemon
/// resolves the workspace and **rejects** a mistyped one with `INVALID_PARAMS`
/// (never a silent no-op). A squad name already used in the workspace is rejected
/// (the `(workspace_id, name)` resolve-or-reject guard), and a malformed `leader`
/// actor-ref is `INVALID_PARAMS`.
pub const HANGAR_SQUAD_CREATE: &str = "hangar/squad_create";

/// `hangar/squad_member_add` — add one member actor to a squad (e38.17).
///
/// Params: [`crate::snapshots::SquadMemberParams`]
/// (`{ workspace_id, squad_id, member }`). Result: the refreshed
/// [`crate::snapshots::SquadsListResult`] for the workspace, or an error.
/// `member` is a polymorphic actor-ref (`"agent:<id>"` / `"member:<id>"`). The add
/// is idempotent (re-adding the same member is a no-op).
///
/// Mutating + workspace-scoped like [`HANGAR_SQUAD_CREATE`]: a foreign-tenant
/// squad id touches no row and is rejected as a not-found error (never a
/// cross-tenant edit).
pub const HANGAR_SQUAD_MEMBER_ADD: &str = "hangar/squad_member_add";

/// `hangar/squad_member_remove` — remove one member actor from a squad (e38.17).
///
/// Params: [`crate::snapshots::SquadMemberParams`]
/// (`{ workspace_id, squad_id, member }`). Result: the refreshed
/// [`crate::snapshots::SquadsListResult`] for the workspace, or an error. Removing
/// a member that is not in the squad is a no-op (idempotent).
///
/// Mutating + workspace-scoped like [`HANGAR_SQUAD_MEMBER_ADD`]: a foreign-tenant
/// squad id touches no row (a not-found error).
pub const HANGAR_SQUAD_MEMBER_REMOVE: &str = "hangar/squad_member_remove";

/// `hangar/squad_assign` — route a task to a squad's LEADER, making leader
/// routing actually take effect (e38.17).
///
/// Params: [`crate::snapshots::SquadAssignParams`]
/// (`{ workspace_id, squad_id, issue_id?, work_dir?, priority? }`). Result: a
/// [`crate::snapshots::SquadAssignResult`] carrying the enqueued task id and the
/// leader identity it routed to, or an error.
///
/// This is the product seam that converts a squad assignment into a routed task:
/// the daemon resolves the squad's leader agent id, derives that agent's runtime,
/// and enqueues an `agent_task_queue` row keyed to the leader's
/// `(agent_id, runtime_id)`, so the existing claim/dispatch path dispatches the
/// work to the LEADER. Mutating + workspace-scoped like [`HANGAR_SQUAD_CREATE`]:
/// the daemon resolves the workspace and rejects a mistyped one with
/// `INVALID_PARAMS`. A squad with a human-member leader (no agent to dispatch to)
/// or an unknown squad is rejected (`INVALID_PARAMS`).
pub const HANGAR_SQUAD_ASSIGN: &str = "hangar/squad_assign";

/// `hangar/squad_fanout` — fan an issue out across the WHOLE squad: brief the
/// LEADER *and* enqueue one task per distinct `agent` member, all on the same
/// issue (P7).
///
/// Params: [`crate::snapshots::SquadAssignParams`] (the same
/// `{ workspace_id, squad_id, issue_id?, work_dir?, priority? }` as
/// [`HANGAR_SQUAD_ASSIGN`]). Result: a [`crate::snapshots::SquadFanoutResult`]
/// carrying the leader's brief task plus one dispatch per fanned-out member, or an
/// error.
///
/// This is the seam the P7 acceptance turns on — "issue assigned to a squad →
/// leader + ≥2 member tasks claimable in parallel". It works because migration
/// `0012` scoped the pending-task guard to `(issue, agent)`: the leader and every
/// member each hold their own pending task on the one issue. Mutating +
/// workspace-scoped like [`HANGAR_SQUAD_ASSIGN`]: a human-member leader / unknown
/// squad is rejected (`INVALID_PARAMS`); a human `member` and the leader's own
/// agent are never double-dispatched.
pub const HANGAR_SQUAD_FANOUT: &str = "hangar/squad_fanout";

/// `hangar/health` — snapshot the daemon's health for the settings screen.
///
/// Params: `{}`. Result: a [`crate::settings::HealthSnapshot`]. Drives the
/// settings daemon-connection section (P4.7).
pub const HANGAR_HEALTH: &str = "hangar/health";

/// `hangar/daemon_health` — snapshot the daemon-health pane (P8.5).
///
/// Params: `{ workspace_id: String }`. Result: a
/// [`crate::settings::DaemonHealthSnapshot`] — the registered runtimes (from the
/// `agent_runtime` table, workspace-scoped), the bounded claim-slot cache
/// occupancy + the concurrent-task count (`agent_task_queue`
/// `dispatched`/`running`), and the daemon's rolling 60-second task-throughput
/// window (an in-memory ring buffer). Drives the daemon-health screen (`D`). A
/// view-layer snapshot, **not** a persisted aggregate.
pub const HANGAR_DAEMON_HEALTH: &str = "hangar/daemon_health";

/// `hangar/usage_rollup` — snapshot the workspace's token/cost usage dashboard
/// (e38.35).
///
/// Params: [`crate::snapshots::WorkspaceScopedParams`] (`{ workspace_id }`).
/// Result: [`crate::snapshots::UsageRollupResult`] — the grand totals (summed
/// input/output tokens + cost + run count across every recorded run) plus the
/// per-agent breakdown (the same totals grouped by agent, heaviest cost first).
/// Drives the usage-dashboard screen (`U`). Reads the durable `task_usage`
/// aggregate the daemon's run loop records at each task's finalize seam (store
/// migration 0022), so usage that accrued while no plugin was attached is still
/// counted. Workspace-scoped like every snapshot: a foreign / unknown workspace
/// yields all-zero totals + an empty rollup (a read, so no `INVALID_PARAMS`
/// rejection — mirrors `inbox_list`).
pub const HANGAR_USAGE_ROLLUP: &str = "hangar/usage_rollup";

/// `hangar/pr_status_refresh` — fetch the CI + merge status of an issue's bound
/// PR and auto-move the issue to Done on merge (e38.34).
///
/// Params: [`crate::snapshots::PrStatusRefreshParams`] (`{ workspace_id,
/// issue_id }`). Result: [`crate::snapshots::PrStatusRefreshResult`] — the fetched
/// [`crate::pr_status::PrStatus`] (CI rollup + mergeable + merge state) plus
/// `transitioned_to_done`. The daemon resolves the issue's latest task
/// `result.pr_url`, shells `gh pr view --json statusCheckRollup,mergeable,state`
/// behind an injectable seam (degrading to an all-unknown status when `gh` is
/// absent / unauthenticated — never a panic), and — only when the PR is `merged`
/// and the issue is not already `done` — moves the issue to `done` via
/// `IssueRepo::update_state` and pushes an `IssueUpdated` event. Mutating +
/// workspace-scoped: a mistyped workspace is rejected with `INVALID_PARAMS`; an
/// issue with no bound PR answers an all-unknown status + no transition (never an
/// error).
pub const HANGAR_PR_STATUS_REFRESH: &str = "hangar/pr_status_refresh";

/// `hangar/inbox_list` — snapshot the aggregated notification inbox of a
/// workspace (e38.14).
///
/// Params: [`crate::snapshots::WorkspaceScopedParams`] (`{ workspace_id }`).
/// Result: [`crate::snapshots::InboxListResult`] — the workspace's inbox entries
/// (newest-first) plus the unread count. Drives the Inbox screen's list + unread
/// badge. The entries are the durable aggregate the daemon's inbox writer folds
/// live issue / comment / task events into (store migration 0021), so an event
/// that fired while no plugin was attached is still here. Workspace-scoped like
/// every snapshot: a foreign / unknown workspace yields an empty list + zero
/// unread (a read, so no `INVALID_PARAMS` rejection — mirrors `issues_list`).
pub const HANGAR_INBOX_LIST: &str = "hangar/inbox_list";

/// `hangar/inbox_mark_read` — mark a workspace's inbox entries read (e38.14).
///
/// Params: [`crate::snapshots::WorkspaceScopedParams`] (`{ workspace_id }`).
/// Result: [`crate::snapshots::InboxMarkReadResult`] — how many entries the sweep
/// flipped + the unread count after (which is `0` for a whole-workspace sweep).
/// This is the mark-read sweep: it stamps `read_at` on every currently-unread
/// entry so the unread count drops to zero. Idempotent (a re-sweep flips nothing
/// and leaves already-read entries on their original timestamp).
///
/// Mutating + workspace-scoped: the daemon resolves the workspace and rejects a
/// mistyped one with `INVALID_PARAMS` (never a silent no-op, mirroring
/// `hangar/task_transition`); a sibling tenant's inbox is never touched.
pub const HANGAR_INBOX_MARK_READ: &str = "hangar/inbox_mark_read";

/// `hangar/boards_list` — snapshot the user-defined kanban boards of a workspace
/// (P4 / D8).
///
/// Params: [`crate::snapshots::WorkspaceScopedParams`] (`{ workspace_id }`).
/// Result: [`crate::snapshots::BoardsListResult`] — the workspace's boards, each
/// with its ordered columns and its cards (an issue placed in a column, with the
/// issue title + latest task status folded in for the render). Drives the Boards
/// screen. Workspace-scoped like every snapshot: a foreign / unknown workspace
/// yields an empty list (a read, so no `INVALID_PARAMS` rejection). The
/// `board_*` mutations all re-read and answer with this same envelope so a caller
/// re-renders from the response without a separate round-trip.
pub const HANGAR_BOARDS_LIST: &str = "hangar/boards_list";

/// `hangar/board_create` — create one empty board in a workspace (P4 / D8).
///
/// Params: [`crate::snapshots::BoardCreateParams`] (`{ workspace_id, name }`).
/// Result: the refreshed [`crate::snapshots::BoardsListResult`]. The board starts
/// with no columns (added via `board_column_add`) and its auto-move master toggle
/// on. A board name already used in the workspace is rejected (resolve-or-reject),
/// never a silent no-op.
pub const HANGAR_BOARD_CREATE: &str = "hangar/board_create";

/// `hangar/board_update` — rename a board and/or flip its auto-move toggle (P4).
///
/// Params: [`crate::snapshots::BoardUpdateParams`]
/// (`{ workspace_id, board_id, name?, auto_move? }`). Result: the refreshed
/// [`crate::snapshots::BoardsListResult`]. Mutating + workspace-scoped: a
/// foreign-tenant board id touches no row (a not-found error). A rename that
/// collides with another board's name is rejected.
pub const HANGAR_BOARD_UPDATE: &str = "hangar/board_update";

/// `hangar/board_delete` — delete a board with its columns + cards (P4).
///
/// Params: [`crate::snapshots::BoardIdParams`] (`{ workspace_id, board_id }`).
/// Result: the refreshed [`crate::snapshots::BoardsListResult`]. Mutating +
/// workspace-scoped: a foreign-tenant board id touches no row (not-found).
pub const HANGAR_BOARD_DELETE: &str = "hangar/board_delete";

/// `hangar/board_column_add` — append a column to a board (P4 / D8).
///
/// Params: [`crate::snapshots::BoardColumnAddParams`]
/// (`{ workspace_id, board_id, name, fsm_state?, auto_move? }`). Result: the
/// refreshed [`crate::snapshots::BoardsListResult`]. `fsm_state` (a task-status
/// token) + `auto_move` set the column's auto-move mapping; omit `fsm_state` for a
/// purely manual column. Mutating + workspace-scoped via the board.
pub const HANGAR_BOARD_COLUMN_ADD: &str = "hangar/board_column_add";

/// `hangar/board_column_update` — rename / re-map / retune a column (P4 / D8).
///
/// Params: [`crate::snapshots::BoardColumnUpdateParams`]
/// (`{ workspace_id, board_id, column_id, name?, fsm_state?, auto_move? }`).
/// Result: the refreshed [`crate::snapshots::BoardsListResult`]. An OMITTED
/// `fsm_state` leaves the mapping unchanged; an EMPTY-STRING `fsm_state` clears it
/// to a manual column. Mutating + workspace-scoped via the board.
pub const HANGAR_BOARD_COLUMN_UPDATE: &str = "hangar/board_column_update";

/// `hangar/board_column_delete` — delete a column, parking its cards (P4 / D8).
///
/// Params: [`crate::snapshots::BoardColumnDeleteParams`]
/// (`{ workspace_id, board_id, column_id }`). Result: the refreshed
/// [`crate::snapshots::BoardsListResult`]. The deleted column's cards are parked
/// UNMAPPED (no data loss, the edge-case contract) and the remaining columns'
/// order renumbers contiguous. Mutating + workspace-scoped via the board.
pub const HANGAR_BOARD_COLUMN_DELETE: &str = "hangar/board_column_delete";

/// `hangar/board_column_reorder` — set a board's column order (P4 / D8).
///
/// Params: [`crate::snapshots::BoardColumnReorderParams`]
/// (`{ workspace_id, board_id, column_ids }`). Result: the refreshed
/// [`crate::snapshots::BoardsListResult`]. `column_ids` must be exactly the
/// board's current columns (a permutation); any other set is rejected. Because
/// cards reference the stable column id, a reorder never moves a card. Mutating +
/// workspace-scoped via the board.
pub const HANGAR_BOARD_COLUMN_REORDER: &str = "hangar/board_column_reorder";

/// `hangar/board_card_add` — place an issue on a board in a column (P4 / D8).
///
/// Params: [`crate::snapshots::BoardCardParams`]
/// (`{ workspace_id, board_id, issue_id, column_id? }`). Result: the refreshed
/// [`crate::snapshots::BoardsListResult`]. Idempotent: re-adding the same issue
/// re-targets its column. Omit `column_id` to place the card unmapped. Mutating +
/// workspace-scoped via the board.
pub const HANGAR_BOARD_CARD_ADD: &str = "hangar/board_card_add";

/// `hangar/board_card_move` — move an existing card to another column (P4 / D8).
///
/// Params: [`crate::snapshots::BoardCardParams`]
/// (`{ workspace_id, board_id, issue_id, column_id? }`). Result: the refreshed
/// [`crate::snapshots::BoardsListResult`]. The card must already be on the board
/// (else not-found); omit `column_id` to park it unmapped. Mutating +
/// workspace-scoped via the board.
pub const HANGAR_BOARD_CARD_MOVE: &str = "hangar/board_card_move";

/// `hangar/board_card_create` — create an issue from a card and place it on a
/// board in one round-trip (ccc / D8, D16).
///
/// Params: [`crate::snapshots::BoardCardCreateParams`]
/// (`{ workspace_id, board_id, column_id?, title, assignee_profile? }`). Result:
/// the refreshed [`crate::snapshots::BoardsListResult`]. Creates a fresh issue
/// with `title`, assigns it to the agent named for `assignee_profile` (the D16
/// board-assignee slug = profile slug) when one resolves in the workspace, then
/// places the card in `column_id` (omit for unmapped). Atomic + workspace-scoped:
/// the interactive `c` card-create the reducer raises lifts to exactly this call,
/// so the TUI never chains issue-create + assign + card-add over three trips.
pub const HANGAR_BOARD_CARD_CREATE: &str = "hangar/board_card_create";

/// `hangar/board_card_run` — launch a card's issue on its assignee profile now
/// (ccc / D6, D16).
///
/// Params: [`crate::snapshots::BoardCardRunParams`]
/// (`{ workspace_id, board_id, issue_id, mode }`). Result:
/// [`crate::snapshots::BoardCardRunResult`] — the enqueued task id + the agent /
/// runtime it routed to + the echoed mode. Enqueues one `agent_task_queue` row
/// for the card's issue keyed to the assignee agent's `(agent_id, runtime_id)` —
/// the same claim/dispatch path a squad assignment rides — so the daemon's claim
/// loop runs it and the D8 auto-move hook slides the card on each FSM transition.
/// `mode` is `headless` or `interactive` (D6 `Run ▾`); both dispatch through the
/// one provider-runner path the daemon exposes today (the mode is carried for the
/// D6 launch surface and echoed back). The assignee resolves from the issue's
/// assignee agent, falling back to the workspace's agent so a card always runs.
/// Mutating + workspace-scoped.
pub const HANGAR_BOARD_CARD_RUN: &str = "hangar/board_card_run";

/// `hangar/board_card_cancel` — cancel a card's in-flight run (tcp T3 / F6).
///
/// Params: [`crate::snapshots::BoardCardCancelParams`]
/// (`{ workspace_id, board_id, issue_id }`). Result:
/// [`crate::snapshots::BoardCardCancelResult`] — the cancelled task id, or
/// `cancelled = false` when the card has no active (queued / dispatched /
/// running) task. Resolves the card's issue to its single active task, flips it
/// to `cancelled` (the idempotent `CancelTaskService` FSM edge), then signals
/// the daemon's run loop to KILL the in-flight run — the headless provider's
/// process group or the interactive tmux session by its exact name. The run's
/// provisioned worktree is torn down (keep-if-dirty) on the finalize seam. A
/// finished / failed / already-cancelled card cannot be retroactively cancelled
/// (`cancelled = false`). Mutating + workspace-scoped via the board.
pub const HANGAR_BOARD_CARD_CANCEL: &str = "hangar/board_card_cancel";

/// `hangar/board_card_reorder` — set the order of a column's cards (tcp T3 / F6).
///
/// Params: [`crate::snapshots::BoardCardReorderParams`]
/// (`{ workspace_id, board_id, column_id?, issue_ids }`). Result: the refreshed
/// [`crate::snapshots::BoardsListResult`]. `issue_ids` must be exactly the cards
/// currently in `column_id` (omit `column_id` for the unmapped pool) — a
/// permutation of them; any other set is rejected. A pure `ord` rewrite within the
/// one column (a card's slot is `board_card.ord`, migration 0034), so no card ever
/// changes column. Mutating + workspace-scoped via the board.
pub const HANGAR_BOARD_CARD_REORDER: &str = "hangar/board_card_reorder";

/// `hangar/board_card_remove` — take an issue card off a board (tcp T3 / F6).
///
/// Params: [`crate::snapshots::BoardCardParams`]
/// (`{ workspace_id, board_id, issue_id }`, `column_id` ignored). Result: the
/// refreshed [`crate::snapshots::BoardsListResult`]. Removes ONLY the board
/// placement — the underlying issue is left intact (a card can be re-added). A
/// card with an ACTIVE run is refused (`INVALID_PARAMS`): cancel the run first, so
/// removing a card never orphans a live task. Idempotent otherwise: removing a
/// card that is not on the board is a no-op. Mutating + workspace-scoped via the
/// board.
pub const HANGAR_BOARD_CARD_REMOVE: &str = "hangar/board_card_remove";

/// `hangar/board_card_timeline` — the card's latest run transcript, for the
/// prettied JSONL timeline overlay (tcp T3 / F6, P10 §4.9).
///
/// Params: [`crate::snapshots::BoardCardParams`]
/// (`{ workspace_id, board_id, issue_id }`, `column_id` ignored). Result:
/// [`crate::snapshots::BoardCardTimelineResult`] — the RAW provider stream-json
/// (`claude.jsonl` / `codex.jsonl`) the card's newest task teed to disk, bounded
/// to a tail so a huge run never floods the socket. The plugin parses it into the
/// transcript taxonomy ([`ainb-plugin-hangar`'s `jsonl_timeline`]). A card that
/// never ran (or whose log is gone) yields an empty transcript, never an error. A
/// read, workspace-scoped via the board.
pub const HANGAR_BOARD_CARD_TIMELINE: &str = "hangar/board_card_timeline";

/// `hangar/repo_list` — the card-create `@` autocomplete repo roster (spec F3).
///
/// Params: `{}` (host-scoped — the roster is the host's favorites + scan cache,
/// not workspace-partitioned). Result: [`crate::snapshots::RepoListResult`] —
/// favorites first (★, most-recent-first via `stats.last_used`), then the
/// scanned repos in cache order, deduped. The daemon reads New Session's
/// `favorites.yaml` + `cache/repositories.json` AS-IS via the fleet-core roster
/// reader (it NEVER triggers a scan — a card-create must not block on a cold
/// filesystem walk). Fuzzy filtering on the `@`-query happens plugin-side; the
/// plugin also prepends the `📁 scratch` first entry (F2), which is not a roster
/// row. A read: a cold / first-run install yields an empty roster, never an error.
pub const HANGAR_REPO_LIST: &str = "hangar/repo_list";

/// `hangar/run_history` — snapshot the workspace's per-run observability timeline
/// (P10 / D19).
///
/// Params: [`crate::snapshots::RunHistoryParams`] (`{ workspace_id, limit? }`).
/// Result: [`crate::snapshots::RunHistoryResult`] — the newest-first run rows
/// (each carrying provider / session / profile / outcome / duration / token-cost).
/// Reads the durable `run_history` rows the daemon's run loop appends at each
/// run's finalize seam (store migration 0029), so runs that accrued while no
/// plugin was attached are still on the timeline. Workspace-scoped like every
/// snapshot: a foreign / unknown workspace yields an empty timeline (a read, so no
/// `INVALID_PARAMS` rejection — mirrors `usage_rollup`).
pub const HANGAR_RUN_HISTORY: &str = "hangar/run_history";

/// `hangar/board_card_assign_squad` — assign (or clear) a SQUAD as a card's
/// assignee (tcp T4 / F7).
///
/// Params: [`crate::snapshots::BoardCardAssignSquadParams`]
/// (`{ workspace_id, board_id, issue_id, squad_id? }`, omit / null `squad_id` to
/// clear). Result: the refreshed [`crate::snapshots::BoardsListResult`]. Persists
/// the squad onto the card's issue (`issue.squad_id`, migration 0035) so a later
/// `board_card_run` fans the card out across the whole squad (leader brief + one
/// task per distinct `agent` member, each in its own worktree) and the board
/// renders one member chip per fanned-out task. A `squad_id` that names no squad
/// in the workspace is rejected (`INVALID_PARAMS`); clearing reverts the card to a
/// single-agent run. Mutating + workspace-scoped via the board.
pub const HANGAR_BOARD_CARD_ASSIGN_SQUAD: &str = "hangar/board_card_assign_squad";

/// `hangar/board_card_dep_add` — add a beads-style `depends-on` edge between two
/// cards (tcp T4 / F7).
///
/// Params: [`crate::snapshots::BoardCardDepParams`] (`{ workspace_id, board_id,
/// dependent_issue_id, blocker_issue_id }`). Result: the refreshed
/// [`crate::snapshots::BoardsListResult`]. The DEPENDENT card is blocked until the
/// BLOCKER card finishes: a blocked card refuses to `board_card_run` (a clear
/// message) and is never auto-dispatched. A self-edge, an edge that would create a
/// CYCLE (checked by a DFS over the existing edges before the write), or an
/// endpoint not on this board is rejected (`INVALID_PARAMS`). Re-adding an existing
/// edge is idempotent. Mutating + workspace-scoped via the board.
pub const HANGAR_BOARD_CARD_DEP_ADD: &str = "hangar/board_card_dep_add";

/// `hangar/board_card_dep_remove` — remove a `depends-on` edge between two cards
/// (tcp T4 / F7).
///
/// Params: [`crate::snapshots::BoardCardDepParams`] (`{ workspace_id, board_id,
/// dependent_issue_id, blocker_issue_id }`). Result: the refreshed
/// [`crate::snapshots::BoardsListResult`]. Removing an absent edge is an idempotent
/// no-op. Mutating + workspace-scoped via the board.
pub const HANGAR_BOARD_CARD_DEP_REMOVE: &str = "hangar/board_card_dep_remove";

/// `hangar/board_card_set_auto_run` — flip a card's auto-run flag (tcp T4 / F7).
///
/// Params: [`crate::snapshots::BoardCardAutoRunParams`] (`{ workspace_id, board_id,
/// issue_id, auto_run }`). Result: the refreshed
/// [`crate::snapshots::BoardsListResult`]. When `auto_run` is on, the card
/// auto-launches the instant its LAST blocker completes (respecting the claim-loop
/// concurrency caps); default OFF keeps EXPLICIT run the default. A card with no
/// blockers ignores the flag. Mutating + workspace-scoped via the board.
pub const HANGAR_BOARD_CARD_SET_AUTO_RUN: &str = "hangar/board_card_set_auto_run";

/// `attention/list` — snapshot the OPEN control-plane inbox for a scope (spec P2).
///
/// Params: [`crate::snapshots::AttentionListParams`]
/// (`{ workspace_id?, fleet }`). Result: [`crate::snapshots::AttentionListResult`]
/// — the open [`crate::events::AttentionRow`]s, oldest-first. Three scopes:
/// `fleet = true` is the host-wide feed (every workspace + the no-workspace host
/// sessions), `fleet = false` with `workspace_id = Some(ws)` is one workspace,
/// and `workspace_id = None` is the no-workspace host rows. A read, so an unknown
/// workspace yields an empty list (no `INVALID_PARAMS`, mirroring `inbox_list`).
pub const ATTENTION_LIST: &str = "attention/list";

/// `attention/subscribe` — open the FLEET-WIDE attention event stream (spec P2).
///
/// Params: [`crate::snapshots::AttentionSubscribeParams`] (`{ workspace_id? }`).
/// Result: [`crate::snapshots::AttentionSubscribeResult`] — the current open
/// snapshot, after which the daemon pushes `AttentionRaised` / `AttentionAnswered`
/// deltas live. Deliberately SEPARATE from [`WORKSPACE_SUBSCRIBE`]: attention is
/// not workspace-partitioned (the control centre answers for the whole host), so
/// this stream is unfiltered by default and carries the no-workspace host
/// sessions the workspace forwarder would drop.
pub const ATTENTION_SUBSCRIBE: &str = "attention/subscribe";

/// `attention/answer` — answer one open attention row from any surface (spec P2).
///
/// Params: [`crate::snapshots::AnswerParams`]
/// (`{ attention_id, answer, answered_by, is_answer }`). Result:
/// [`crate::snapshots::AnswerResult`] — a tagged outcome. The daemon runs the
/// first-answer-wins guard (a conditional `open → answered` flip: a second
/// answer to the same row loses and gets `already_answered`) and then, on the
/// win, the C1 cwd-ambiguity guard (`ambiguous` refusal rather than a mis-route)
/// before delivering `answer` into the raising session via the one verified send
/// path. Mutating: exactly one answer is ever delivered per row.
pub const ATTENTION_ANSWER: &str = "attention/answer";

/// `atc/register` — register (or re-register) an ATC instance on the daemon
/// (spec P9, D12). Params: [`crate::snapshots::AtcRegisterParams`]. Result:
/// [`crate::snapshots::AtcRegisterResult`] (the persisted name + next heartbeat
/// tick). This is the daemon-native replacement for `ainb fleet atc setup`'s old
/// launchd/systemd timer install: the instance lands in `atc_instance` and the
/// heartbeat becomes a daemon cron. Mutating + idempotent by name.
pub const ATC_REGISTER: &str = "atc/register";

/// `atc/list` — list the registered ATC instances (spec P9, D12). Params: `{}`.
/// Result: [`crate::snapshots::AtcListResult`]. A read (host-wide, since ATC is
/// not workspace-partitioned).
pub const ATC_LIST: &str = "atc/list";

/// `atc/escalate` — raise an ATC escalation as an `escalation` attention row
/// (spec P9, D12). Params: [`crate::snapshots::AtcEscalateParams`]. Result:
/// [`crate::snapshots::AtcEscalateResult`] (the raised attention id). The
/// escalation flows through the same attention pipeline as every other input
/// request, so it reaches the phone/web push instead of dead-ending in
/// `task-log.md`. Mutating.
pub const ATC_ESCALATE: &str = "atc/escalate";

/// `atc/unregister` — disable a registered ATC instance's heartbeat cron (spec
/// P9, D12). Params: [`crate::snapshots::AtcUnregisterParams`] (`{ name }`).
/// Result: [`crate::snapshots::AtcUnregisterResult`]. The daemon-native
/// counterpart to `ainb fleet atc teardown`'s launchd/systemd timer removal: it
/// flips `enabled = 0` and clears `next_tick_at` so the heartbeat cron stops
/// scheduling the instance, without deleting its audit/ledger rows. Mutating +
/// idempotent (unregistering an unknown or already-disabled instance is a no-op).
pub const ATC_UNREGISTER: &str = "atc/unregister";

/// `profile/list` — list the indexed agent profiles (spec P5, D14-D16).
///
/// Params: `{}`. Result: [`crate::snapshots::ProfileListResult`] — every indexed
/// profile (`slug`, `tier`, `mtime`), slug-ordered. A read over the daemon's
/// fs-watch-maintained index of the on-disk masters
/// (`~/.agents-in-a-box/profiles/<slug>.md`). Host-scoped, not workspace-partitioned
/// (a profile drives runs in any workspace).
pub const PROFILE_LIST: &str = "profile/list";

/// `profile/get` — fetch one profile master + its two compile previews (spec P5).
///
/// Params: [`crate::snapshots::ProfileGetParams`] (`{ slug }`). Result:
/// [`crate::snapshots::ProfileGetResult`] — the parsed master fields plus the
/// lossless Claude `.md` preview and the lossy Codex fragment/prompt preview with
/// its dropped-field warnings (D14). An unknown slug yields
/// [`crate::snapshots::ProfileGetResult::not_found`], not an error.
pub const PROFILE_GET: &str = "profile/get";

/// `profile/upsert` — create or replace a profile master on disk (spec P5).
///
/// Params: [`crate::snapshots::ProfileUpsertParams`] (`{ slug, description, tier,
/// tools, color, body }`). Result: [`crate::snapshots::ProfileUpsertResult`]. The
/// daemon writes the canonical master to `~/.agents-in-a-box/profiles/<slug>.md` and
/// refreshes the DB index row; the fs-watch reconciler would also catch the write,
/// so the RPC and the watch converge on the same index. Mutating.
pub const PROFILE_UPSERT: &str = "profile/upsert";

/// `hangar/notify_rules_list` — the per-attention-kind notification routing grid
/// for a scope (tcp T5).
///
/// Params: [`crate::snapshots::NotifyRulesListParams`] (`{ workspace_id? }`).
/// Result: [`crate::snapshots::NotifyRulesListResult`] — one
/// [`crate::snapshots::NotifyRuleWireRow`] per attention kind (in declaration
/// order), each carrying the EFFECTIVE channel set for the scope and whether it
/// is a per-workspace override. `workspace_id = None` returns the global rows. A
/// read (an unknown workspace still resolves the globals), so no
/// `INVALID_PARAMS` — it mirrors the other list snapshots.
pub const HANGAR_NOTIFY_RULES_LIST: &str = "hangar/notify_rules_list";

/// `hangar/notify_rule_set` — set (or clear) one routing rule (tcp T5).
///
/// Params: [`crate::snapshots::NotifyRuleSetParams`]
/// (`{ workspace_id?, kind, channels }`). Result:
/// [`crate::snapshots::NotifyRuleSetResult`]. Upserts the rule for the scope +
/// kind (global when `workspace_id` is absent, a per-workspace override
/// otherwise); the settings grid maps a toggled cell to this call. Mutating +
/// idempotent (re-setting the same channels is a no-op replace). An unknown
/// `kind` is rejected with `INVALID_PARAMS`.
pub const HANGAR_NOTIFY_RULE_SET: &str = "hangar/notify_rule_set";

/// `hangar/daemon_config_get` — read one `daemon_config` value by key (D13).
///
/// Params: [`crate::snapshots::DaemonConfigGetParams`] (`{ key }`). Result:
/// [`crate::snapshots::DaemonConfigGetResult`] (`{ key, value }`), where `value`
/// is `None` when the key has no stored row (the caller applies the coded
/// default). A read — an unknown key is `value = None`, never an error. The
/// Settings Daemon-section auto-standup toggle reads `autostandup.enabled` through
/// this.
pub const HANGAR_DAEMON_CONFIG_GET: &str = "hangar/daemon_config_get";

/// `hangar/daemon_config_set` — write one `daemon_config` value by key (D13).
///
/// Params: [`crate::snapshots::DaemonConfigSetParams`] (`{ key, value }`). Result:
/// [`crate::snapshots::DaemonConfigSetResult`] (`{ key, value }`, the stored value
/// echoed). Mutating + idempotent (re-writing the same value is a no-op replace).
/// The Settings auto-standup toggle persists `autostandup.enabled` through this.
pub const HANGAR_DAEMON_CONFIG_SET: &str = "hangar/daemon_config_set";

/// `hangar/daemon_config_list` — read EVERY user-config knob in one round trip.
///
/// Params: none (`{}`). Result: [`crate::snapshots::DaemonConfigListResult`]
/// (`{ entries: [{ key, value }] }`), one entry per
/// [`ainb_hangar_core::daemon_config::DAEMON_CONFIG_REGISTRY`] descriptor, whose
/// `value` is `None` when the key has no stored row. The Settings Daemon-section
/// editor reads the whole configurable set through this rather than a get per
/// key, so a new registry knob surfaces without new wiring.
pub const HANGAR_DAEMON_CONFIG_LIST: &str = "hangar/daemon_config_list";

/// `auth/hello` — authenticate a freshly-opened socket connection.
///
/// Params: [`crate::auth::HelloParams`] (`{ token: String }` — the plaintext
/// daemon token read from `{hangar_home}/hangar/daemon.token`). Result: `{}`.
/// MUST be the **first frame** of every connection; the daemon answers any
/// other first frame (or a token that fails the constant-time digest check)
/// with an [`crate::auth::UNAUTHORIZED`] error and closes the connection.
pub const AUTH_HELLO: &str = "auth/hello";

/// `ping` — bare liveness probe. Params: `{}`. Result: `{}`.
pub const PING: &str = "ping";

/// Every daemon method name, in declaration order.
///
/// Single source of truth for the registry tests in this module. The
/// `all_methods_covers_every_const` test guards against registry drift (a
/// method const declared but never appended here), while `method_names_unique`
/// and `methods_namespaced_or_ping` guard the shape of the wire surface.
pub const ALL_METHODS: &[&str] = &[
    WORKSPACE_SUBSCRIBE,
    WORKSPACE_LIST,
    HANGAR_ISSUES_LIST,
    HANGAR_ISSUES_SEARCH,
    HANGAR_SEARCH,
    HANGAR_AGENTS_LIST,
    HANGAR_SKILLS_LIST,
    HANGAR_SKILL_GET,
    HANGAR_SKILLS_SYNC,
    HANGAR_SKILL_ATTACH,
    HANGAR_SKILL_DETACH,
    HANGAR_AUTOPILOTS_LIST,
    HANGAR_AUTOPILOT_RUNS,
    HANGAR_AUTOPILOT_FIRE_NOW,
    HANGAR_AUTOPILOT_SET_ENABLED,
    HANGAR_TASKS_LIST,
    HANGAR_TASK_TRANSITION,
    HANGAR_ISSUE_UPDATE,
    HANGAR_ISSUE_LABEL_ATTACH,
    HANGAR_ISSUE_LABEL_DETACH,
    HANGAR_COMMENT_ADD,
    HANGAR_AGENT_UPDATE,
    HANGAR_AGENT_ARCHIVE,
    HANGAR_MEMBERS_LIST,
    HANGAR_MEMBER_SET_ROLE,
    HANGAR_MEMBER_REMOVE,
    HANGAR_SQUADS_LIST,
    HANGAR_SQUAD_CREATE,
    HANGAR_SQUAD_MEMBER_ADD,
    HANGAR_SQUAD_MEMBER_REMOVE,
    HANGAR_SQUAD_ASSIGN,
    HANGAR_HEALTH,
    HANGAR_DAEMON_HEALTH,
    HANGAR_USAGE_ROLLUP,
    HANGAR_PR_STATUS_REFRESH,
    HANGAR_INBOX_LIST,
    HANGAR_INBOX_MARK_READ,
    ATTENTION_LIST,
    ATTENTION_SUBSCRIBE,
    ATTENTION_ANSWER,
    ATC_REGISTER,
    ATC_LIST,
    ATC_ESCALATE,
    ATC_UNREGISTER,
    AUTH_HELLO,
    PING,
    // Board methods (P4 / D8) are APPENDED at the catalogue tail — the wire
    // catalogue is append-only, so new methods must follow every pre-existing
    // entry (attention/auth/ping) rather than being spliced ahead of them.
    HANGAR_BOARDS_LIST,
    HANGAR_BOARD_CREATE,
    HANGAR_BOARD_UPDATE,
    HANGAR_BOARD_DELETE,
    HANGAR_BOARD_COLUMN_ADD,
    HANGAR_BOARD_COLUMN_UPDATE,
    HANGAR_BOARD_COLUMN_DELETE,
    HANGAR_BOARD_COLUMN_REORDER,
    HANGAR_BOARD_CARD_ADD,
    HANGAR_BOARD_CARD_MOVE,
    // P7 squad fan-out is appended at the tail (append-only wire catalogue).
    HANGAR_SQUAD_FANOUT,
    // Observability (P10 / D19) is APPENDED at the catalogue tail — the wire
    // catalogue is append-only, so a new method must follow every pre-existing
    // entry.
    HANGAR_RUN_HISTORY,
    // Agent profiles (P5 / D14-D16) are APPENDED at the catalogue tail — the
    // wire catalogue is append-only, so profile methods follow every
    // pre-existing entry (boards / squad fan-out / run history).
    PROFILE_LIST,
    PROFILE_GET,
    PROFILE_UPSERT,
    // Board card interaction (ccc / D6, D8, D16) is APPENDED at the catalogue
    // tail — the wire catalogue is append-only, so the card create/run methods
    // follow every pre-existing entry (boards / squad fan-out / run history /
    // profiles).
    HANGAR_BOARD_CARD_CREATE,
    HANGAR_BOARD_CARD_RUN,
    // Card-create repo roster (spec F3) is APPENDED at the catalogue tail — the
    // wire catalogue is append-only.
    HANGAR_REPO_LIST,
    // Card lifecycle (tcp T3 / F6) is APPENDED at the catalogue tail — the wire
    // catalogue is append-only.
    HANGAR_BOARD_CARD_CANCEL,
    HANGAR_BOARD_CARD_REORDER,
    HANGAR_BOARD_CARD_REMOVE,
    HANGAR_BOARD_CARD_TIMELINE,
    // Squad-from-card + card dependencies (tcp T4 / F7) are APPENDED at the
    // catalogue tail — the wire catalogue is append-only.
    HANGAR_BOARD_CARD_ASSIGN_SQUAD,
    HANGAR_BOARD_CARD_DEP_ADD,
    HANGAR_BOARD_CARD_DEP_REMOVE,
    HANGAR_BOARD_CARD_SET_AUTO_RUN,
    // Notification routing rules (tcp T5) are APPENDED at the catalogue tail —
    // the wire catalogue is append-only.
    HANGAR_NOTIFY_RULES_LIST,
    HANGAR_NOTIFY_RULE_SET,
    // Daemon-config get/set (D13) is APPENDED at the catalogue tail — the wire
    // catalogue is append-only.
    HANGAR_DAEMON_CONFIG_GET,
    HANGAR_DAEMON_CONFIG_SET,
    HANGAR_DAEMON_CONFIG_LIST,
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// No two daemon methods share a name.
    #[test]
    fn method_names_unique() {
        let set: HashSet<&&str> = ALL_METHODS.iter().collect();
        assert_eq!(set.len(), ALL_METHODS.len(), "duplicate method name");
    }

    /// Every method is either namespaced (`<area>/<verb>`) or the bare
    /// `ping` liveness probe. No empty or whitespace names.
    #[test]
    fn methods_namespaced_or_ping() {
        for m in ALL_METHODS {
            assert!(!m.is_empty(), "empty method name");
            assert!(!m.contains(char::is_whitespace), "whitespace in {m:?}");
            assert!(
                *m == PING || m.contains('/'),
                "{m:?} is neither namespaced nor `ping`"
            );
        }
    }

    /// The workspace methods live under the `workspace/` namespace.
    #[test]
    fn workspace_methods_namespaced() {
        assert!(WORKSPACE_SUBSCRIBE.starts_with("workspace/"));
        assert!(WORKSPACE_LIST.starts_with("workspace/"));
    }

    /// The control-plane attention methods live under the `attention/` namespace.
    #[test]
    fn attention_methods_namespaced() {
        for m in [ATTENTION_LIST, ATTENTION_SUBSCRIBE, ATTENTION_ANSWER] {
            assert!(m.starts_with("attention/"), "{m:?} not under attention/");
        }
    }

    /// The P5 agent-profile methods live under the `profile/` namespace.
    #[test]
    fn profile_methods_namespaced() {
        for m in [PROFILE_LIST, PROFILE_GET, PROFILE_UPSERT] {
            assert!(m.starts_with("profile/"), "{m:?} not under profile/");
        }
    }

    /// The P4 snapshot methods live under the `hangar/` namespace.
    #[test]
    fn snapshot_methods_namespaced() {
        for m in [
            HANGAR_ISSUES_LIST,
            HANGAR_ISSUES_SEARCH,
            HANGAR_SEARCH,
            HANGAR_AGENTS_LIST,
            HANGAR_SKILLS_LIST,
            HANGAR_SKILL_GET,
            HANGAR_SKILLS_SYNC,
            HANGAR_SKILL_ATTACH,
            HANGAR_SKILL_DETACH,
            HANGAR_AUTOPILOTS_LIST,
            HANGAR_AUTOPILOT_RUNS,
            HANGAR_AUTOPILOT_FIRE_NOW,
            HANGAR_AUTOPILOT_SET_ENABLED,
            HANGAR_TASKS_LIST,
            HANGAR_TASK_TRANSITION,
            HANGAR_ISSUE_UPDATE,
            HANGAR_ISSUE_LABEL_ATTACH,
            HANGAR_ISSUE_LABEL_DETACH,
            HANGAR_COMMENT_ADD,
            HANGAR_AGENT_UPDATE,
            HANGAR_AGENT_ARCHIVE,
            HANGAR_MEMBERS_LIST,
            HANGAR_MEMBER_SET_ROLE,
            HANGAR_MEMBER_REMOVE,
            HANGAR_SQUADS_LIST,
            HANGAR_SQUAD_CREATE,
            HANGAR_SQUAD_MEMBER_ADD,
            HANGAR_SQUAD_MEMBER_REMOVE,
            HANGAR_SQUAD_ASSIGN,
            HANGAR_HEALTH,
            HANGAR_DAEMON_HEALTH,
            HANGAR_USAGE_ROLLUP,
            HANGAR_PR_STATUS_REFRESH,
            HANGAR_INBOX_LIST,
            HANGAR_INBOX_MARK_READ,
            HANGAR_BOARDS_LIST,
            HANGAR_BOARD_CREATE,
            HANGAR_BOARD_UPDATE,
            HANGAR_BOARD_DELETE,
            HANGAR_BOARD_COLUMN_ADD,
            HANGAR_BOARD_COLUMN_UPDATE,
            HANGAR_BOARD_COLUMN_DELETE,
            HANGAR_BOARD_COLUMN_REORDER,
            HANGAR_BOARD_CARD_ADD,
            HANGAR_BOARD_CARD_MOVE,
            HANGAR_SQUAD_FANOUT,
            HANGAR_RUN_HISTORY,
        ] {
            assert!(m.starts_with("hangar/"), "{m:?} not under hangar/");
        }
    }

    /// Registry-drift guard: every individually-declared method const must be
    /// present in [`ALL_METHODS`]. Rust has no compile-time reflection over
    /// module consts, so the full set is mirrored here explicitly — adding a
    /// new `pub const` method without also appending it to `ALL_METHODS` (and
    /// to this list) fails this test, keeping the wire registry honest.
    #[test]
    fn all_methods_covers_every_const() {
        // Every method const known to this module. Keep in sync with the
        // `pub const` declarations above.
        let declared: &[&str] = &[
            WORKSPACE_SUBSCRIBE,
            WORKSPACE_LIST,
            HANGAR_ISSUES_LIST,
            HANGAR_ISSUES_SEARCH,
            HANGAR_SEARCH,
            HANGAR_AGENTS_LIST,
            HANGAR_SKILLS_LIST,
            HANGAR_SKILL_GET,
            HANGAR_SKILLS_SYNC,
            HANGAR_SKILL_ATTACH,
            HANGAR_SKILL_DETACH,
            HANGAR_AUTOPILOTS_LIST,
            HANGAR_AUTOPILOT_RUNS,
            HANGAR_AUTOPILOT_FIRE_NOW,
            HANGAR_AUTOPILOT_SET_ENABLED,
            HANGAR_TASKS_LIST,
            HANGAR_TASK_TRANSITION,
            HANGAR_ISSUE_UPDATE,
            HANGAR_ISSUE_LABEL_ATTACH,
            HANGAR_ISSUE_LABEL_DETACH,
            HANGAR_COMMENT_ADD,
            HANGAR_AGENT_UPDATE,
            HANGAR_AGENT_ARCHIVE,
            HANGAR_MEMBERS_LIST,
            HANGAR_MEMBER_SET_ROLE,
            HANGAR_MEMBER_REMOVE,
            HANGAR_SQUADS_LIST,
            HANGAR_SQUAD_CREATE,
            HANGAR_SQUAD_MEMBER_ADD,
            HANGAR_SQUAD_MEMBER_REMOVE,
            HANGAR_SQUAD_ASSIGN,
            HANGAR_HEALTH,
            HANGAR_DAEMON_HEALTH,
            HANGAR_USAGE_ROLLUP,
            HANGAR_PR_STATUS_REFRESH,
            HANGAR_INBOX_LIST,
            HANGAR_INBOX_MARK_READ,
            ATTENTION_LIST,
            ATTENTION_SUBSCRIBE,
            ATTENTION_ANSWER,
            ATC_REGISTER,
            ATC_LIST,
            ATC_ESCALATE,
            ATC_UNREGISTER,
            AUTH_HELLO,
            PING,
            HANGAR_BOARDS_LIST,
            HANGAR_BOARD_CREATE,
            HANGAR_BOARD_UPDATE,
            HANGAR_BOARD_DELETE,
            HANGAR_BOARD_COLUMN_ADD,
            HANGAR_BOARD_COLUMN_UPDATE,
            HANGAR_BOARD_COLUMN_DELETE,
            HANGAR_BOARD_COLUMN_REORDER,
            HANGAR_BOARD_CARD_ADD,
            HANGAR_BOARD_CARD_MOVE,
            HANGAR_SQUAD_FANOUT,
            HANGAR_RUN_HISTORY,
            PROFILE_LIST,
            PROFILE_GET,
            PROFILE_UPSERT,
            HANGAR_BOARD_CARD_CREATE,
            HANGAR_BOARD_CARD_RUN,
            HANGAR_REPO_LIST,
            HANGAR_BOARD_CARD_CANCEL,
            HANGAR_BOARD_CARD_REORDER,
            HANGAR_BOARD_CARD_REMOVE,
            HANGAR_BOARD_CARD_TIMELINE,
            HANGAR_BOARD_CARD_ASSIGN_SQUAD,
            HANGAR_BOARD_CARD_DEP_ADD,
            HANGAR_BOARD_CARD_DEP_REMOVE,
            HANGAR_BOARD_CARD_SET_AUTO_RUN,
            HANGAR_NOTIFY_RULES_LIST,
            HANGAR_NOTIFY_RULE_SET,
            HANGAR_DAEMON_CONFIG_GET,
            HANGAR_DAEMON_CONFIG_SET,
            HANGAR_DAEMON_CONFIG_LIST,
        ];
        for m in declared {
            assert!(
                ALL_METHODS.contains(m),
                "method const {m:?} is missing from ALL_METHODS"
            );
        }
        assert_eq!(
            declared.len(),
            ALL_METHODS.len(),
            "ALL_METHODS has {} entries but {} method consts are declared",
            ALL_METHODS.len(),
            declared.len()
        );
    }
}
