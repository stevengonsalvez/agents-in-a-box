//! Daemon JSON-RPC method-name registry.
//!
//! These are the methods the Hangar **daemon** speaks over its
//! `~/.ainb/hangar.sock` socket. They sit on the same JSON-RPC 2.0
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
    AUTH_HELLO,
    PING,
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
            AUTH_HELLO,
            PING,
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
