//! Request params + result wrappers for the P4 `hangar/*` snapshot RPCs.
//!
//! Each snapshot RPC carries a `{ workspace_id }` request (except
//! [`crate::methods::HANGAR_HEALTH`], which is workspace-agnostic) and answers
//! with a thin envelope wrapping the row vec the corresponding screen renders
//! from. The row types themselves ([`crate::events::IssueRow`],
//! [`crate::events::ActorRow`], [`crate::events::SkillRow`],
//! [`crate::settings::HealthSnapshot`]) live next to the event/settings wire
//! types; this module only adds the request/response envelopes so the daemon
//! handler and the plugin client agree on the exact JSON shape.
//!
//! These are **pure wire types** — `serde` only, no host deps — matching the
//! rest of `ainb-hangar-proto`.

use serde::{Deserialize, Serialize};

use crate::events::{
    ActorRow, AutopilotRow, AutopilotRunRow, InboxEntryRow, IssueRow, SkillFile, SkillRow,
    TaskCardRow,
};

/// The `{ workspace_id }` params shared by every workspace-scoped snapshot RPC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceScopedParams {
    /// The workspace whose rows to snapshot.
    pub workspace_id: String,
}

/// Params for [`crate::methods::WORKSPACE_SUBSCRIBE`] — the workspace to stream,
/// plus an optional resume cursor (T1 event-bus catch-up).
///
/// `since_seq` is the client's last-seen event-log [`seq`]: when present, the
/// daemon replays every event with `seq > since_seq` as `hangar/event`
/// notifications immediately after the subscribe ack (the "deltas" half of
/// "snapshot then deltas") before the live stream continues. Omitted (the
/// default) means a fresh subscription with no backlog — today's behaviour, so a
/// pre-cursor client wire-compatibly decodes/encodes without the field.
///
/// [`seq`]: SubscribeSnapshot::cursor
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSubscribeParams {
    /// The workspace to subscribe to (slug or resolved id).
    pub workspace_id: String,
    /// The client's resume cursor: replay events strictly after this `seq`.
    /// `None` (omitted) subscribes without a backlog.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_seq: Option<i64>,
}

/// The real snapshot [`crate::methods::WORKSPACE_SUBSCRIBE`] acks with — the
/// current head of the workspace's durable event log.
///
/// A client records [`cursor`](Self::cursor) as its resume point: a later
/// reconnect passes it back as [`WorkspaceSubscribeParams::since_seq`] to catch
/// up on exactly the events logged while it was away. `0` means the workspace has
/// no events yet (nothing to resume from).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SubscribeSnapshot {
    /// The workspace's current head event-log sequence (the resume cursor).
    pub cursor: i64,
}

/// Result envelope of [`crate::methods::WORKSPACE_SUBSCRIBE`]: `{ snapshot }`.
///
/// The envelope shape (`{ "snapshot": { … } }`) is preserved from the pre-cursor
/// empty ack so a plugin that only checks for a non-error response still reaches
/// `Connected`; the snapshot now carries a real [`cursor`](SubscribeSnapshot::cursor)
/// instead of an empty object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SubscribeResult {
    /// The real snapshot (the event-log head cursor).
    pub snapshot: SubscribeSnapshot,
}

/// Result of [`crate::methods::HANGAR_ISSUES_LIST`].
///
/// Every issue row in the workspace, in daemon order (`created_at` ascending).
/// The plugin buckets them into the Todo / In Progress / Done columns
/// client-side.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssuesListResult {
    /// The issue rows.
    pub issues: Vec<IssueRow>,
}

/// Params for [`crate::methods::HANGAR_ISSUES_SEARCH`] (e38.12): ranked
/// title + description + comment search within a workspace.
///
/// `workspace_id` is the tenant scope (a sibling tenant's matching issue is never
/// returned). `query` is the case-insensitive substring to match across the issue
/// title, description, and comment bodies; a blank query matches nothing. The
/// result reuses [`IssuesListResult`] — the matching [`IssueRow`]s in ranked order
/// (title hits before description hits before comment-only hits).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct IssueSearchParams {
    /// The workspace to search within (tenant scope).
    pub workspace_id: String,
    /// The case-insensitive substring to match across title / description /
    /// comment bodies. Blank matches nothing.
    pub query: String,
}

/// Params for [`crate::methods::HANGAR_SEARCH`] (e38.13): ranked cross-entity
/// command-palette search within a workspace.
///
/// `workspace_id` is the tenant scope (a sibling tenant's matching entity is never
/// returned). `query` is the case-insensitive substring matched across each
/// entity's human-readable field (issue title / agent name / skill name /
/// autopilot name); a blank query matches nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SearchParams {
    /// The workspace to search within (tenant scope).
    pub workspace_id: String,
    /// The case-insensitive substring to match across every entity's
    /// human-readable field. Blank matches nothing.
    pub query: String,
}

/// The entity kind a [`SearchEntry`] points at — the cross-entity axis the
/// command palette (e38.13) searches over.
///
/// The wire tag is `snake_case` (`"issue"` / `"agent"` / `"skill"` /
/// `"autopilot"`). The variant order is also the tie-break ranking order the
/// daemon applies after match strength (issues before agents before skills before
/// autopilots), so a deterministic palette ordering is part of the wire contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchEntryKind {
    /// An issue (matched on its title); jumping opens the issue-list screen.
    Issue,
    /// An agent (matched on its name); jumping opens the agent-picker / settings.
    Agent,
    /// A skill (matched on its name); jumping opens the skill-manager screen.
    Skill,
    /// An autopilot (matched on its name); jumping opens the autopilots screen.
    Autopilot,
}

impl SearchEntryKind {
    /// The screen the palette jumps to when this entry is chosen.
    ///
    /// A stable wire token (`"issue_list"` / `"skill_manager"` / `"autopilots"`)
    /// the plugin maps to its [`Screen`](crate) routing target. Agents have no
    /// dedicated list screen at v1, so an agent entry lands on the issue list
    /// (where the agent picker is reachable via `a`); this keeps every entry
    /// navigable rather than dead.
    #[must_use]
    pub const fn target_screen(self) -> &'static str {
        match self {
            Self::Issue | Self::Agent => "issue_list",
            Self::Skill => "skill_manager",
            Self::Autopilot => "autopilots",
        }
    }
}

/// One ranked cross-entity search hit (e38.13): an entity that matched the
/// palette query, carrying everything the palette needs to render the row AND
/// jump to it.
///
/// `kind` is the entity axis, `id` its workspace-local row id, `label` the
/// human-readable field that matched (rendered in the result list), and `screen`
/// the routing token the plugin opens on Enter (derived from
/// [`SearchEntryKind::target_screen`], carried on the wire so the plugin needs no
/// kind→screen table of its own).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchEntry {
    /// The entity kind that matched.
    pub kind: SearchEntryKind,
    /// The matched entity's workspace-local id.
    pub id: String,
    /// The human-readable label that matched (issue title / entity name).
    pub label: String,
    /// The screen-routing token the palette opens on Enter (e.g. `"issue_list"`).
    pub screen: String,
}

/// Result of [`crate::methods::HANGAR_SEARCH`] (e38.13): the matching
/// [`SearchEntry`]s in ranked order (exact > prefix > substring, then kind order,
/// then label).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SearchResult {
    /// The ranked cross-entity hits.
    pub entries: Vec<SearchEntry>,
}

/// Result of [`crate::methods::HANGAR_AGENTS_LIST`]: the polymorphic actor list
/// (members + agents) the agent-picker modal renders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentsListResult {
    /// The actor rows (members and agents in one flat list).
    pub actors: Vec<ActorRow>,
}

/// Result of [`crate::methods::HANGAR_SKILLS_LIST`]: the workspace's skills.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillsListResult {
    /// The skill rows.
    pub skills: Vec<SkillRow>,
}

/// Params for [`crate::methods::HANGAR_SKILL_GET`].
///
/// The workspace plus the skill id to fetch in detail. The `workspace_id` scopes
/// the lookup so a skill id from another tenant resolves to `null`, never
/// another workspace's body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillGetParams {
    /// The subscribed workspace the skill must belong to.
    pub workspace_id: String,
    /// The skill id (its slug — the stable id the list rows carry).
    pub skill_id: String,
}

/// Result of [`crate::methods::HANGAR_SKILL_GET`]: one skill's full detail.
///
/// The `SKILL.md` body plus the ordered file list, rendered by the skill-manager
/// detail pane. `body` is the top-level skill content (`None` when the skill is
/// file-only); `files` is the path-ordered child list (the file tree). Distinct
/// from the list-row [`SkillRow`], which carries only name / `used` / `updated_at`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillDetail {
    /// The skill slug (its stable id, matching the list row).
    pub slug: String,
    /// Human-readable skill name.
    pub name: String,
    /// Short description; `None` when unset.
    pub description: Option<String>,
    /// The top-level `SKILL.md` body; `None` when the skill is file-only.
    pub body: Option<String>,
    /// The skill's child files, ordered by `path` (the file tree).
    pub files: Vec<SkillFile>,
}

/// Params for [`crate::methods::HANGAR_SKILLS_SYNC`]: the target workspace plus
/// an optional source directory override.
///
/// When `source_path` is `None` the daemon resolves the source the same way the
/// CLI does (`$AINB_TOOLKIT_SKILLS_DIR`, else a walk to `ainb-toolkit/skills`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillsSyncParams {
    /// The workspace to import into.
    pub workspace_id: String,
    /// Override the source directory (`<name>/SKILL.md` shaped), or `None` to
    /// resolve the default toolkit source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
}

/// Result of [`crate::methods::HANGAR_SKILLS_SYNC`]: the imported skill names.
///
/// `imported` is the kebab-case names of every skill the sync upserted (in
/// import order); `count` is its length, carried explicitly so the plugin can
/// surface "Imported N skills" without re-counting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillsSyncResult {
    /// The imported skill names, in import order.
    pub imported: Vec<String>,
    /// The number of skills imported (`imported.len()`).
    pub count: usize,
}

/// Params for `hangar/skill_attach` / `hangar/skill_detach`.
///
/// The workspace plus the agent + skill to (de)associate. The `workspace_id` is
/// the tenant-isolation guard — the daemon verifies both ids belong to it before
/// mutating the junction. See [`crate::methods::HANGAR_SKILL_ATTACH`] /
/// [`crate::methods::HANGAR_SKILL_DETACH`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillAttachParams {
    /// The subscribed workspace both ids must belong to.
    pub workspace_id: String,
    /// The agent the skill is attached to / detached from.
    pub agent_id: String,
    /// The skill being (de)attached.
    pub skill_id: String,
}

/// Result of [`crate::methods::HANGAR_AUTOPILOTS_LIST`]: the workspace's
/// autopilots (P7.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutopilotsListResult {
    /// The autopilot rows, ordered by name.
    pub autopilots: Vec<AutopilotRow>,
}

/// Params for [`crate::methods::HANGAR_AUTOPILOT_RUNS`].
///
/// The workspace (tenant guard) plus the autopilot id and a row cap. The
/// `workspace_id` scopes the lookup so a foreign autopilot id yields an empty
/// run set, never another tenant's history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutopilotRunsParams {
    /// The subscribed workspace the autopilot must belong to.
    pub workspace_id: String,
    /// The autopilot whose runs to list.
    pub autopilot_id: String,
    /// Maximum number of runs to return (latest-first).
    pub limit: u32,
}

/// Result of [`crate::methods::HANGAR_AUTOPILOT_RUNS`]: one autopilot's recent
/// runs, latest-first (P7.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutopilotRunsResult {
    /// The run rows, latest-first, capped at the requested limit.
    pub runs: Vec<AutopilotRunRow>,
}

/// Params for [`crate::methods::HANGAR_AUTOPILOT_FIRE_NOW`]: the workspace
/// (tenant guard) plus the autopilot to fire immediately (P7.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutopilotFireNowParams {
    /// The subscribed workspace the autopilot must belong to.
    pub workspace_id: String,
    /// The autopilot to fire now (bypassing the schedule).
    pub autopilot_id: String,
}

/// Params for [`crate::methods::HANGAR_AUTOPILOT_SET_ENABLED`]: the workspace
/// (tenant guard), the autopilot, and the target enabled flag (P7.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutopilotSetEnabledParams {
    /// The subscribed workspace the autopilot must belong to.
    pub workspace_id: String,
    /// The autopilot to toggle.
    pub autopilot_id: String,
    /// `true` enables (recompute next-tick from now); `false` disables.
    pub enabled: bool,
}

/// Result of [`crate::methods::HANGAR_TASKS_LIST`]: every task in the workspace
/// for the Kanban board (P8.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TasksListResult {
    /// The task card rows, in daemon order (`created_at` ascending). The plugin
    /// buckets them into the four board columns by their `status`.
    pub tasks: Vec<TaskCardRow>,
}

/// Result of [`crate::methods::HANGAR_INBOX_LIST`] (e38.14): the workspace's
/// aggregated notification entries plus the unread count.
///
/// `entries` are newest-first (the daemon orders `created_at DESC`); `unread` is
/// the count of entries whose `read_at` is NULL — the badge figure the inbox
/// screen renders. Bundling the count avoids a second round-trip: one list call
/// answers both "what's in my inbox" and "how many are unread".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxListResult {
    /// The aggregated inbox rows, newest-first.
    pub entries: Vec<InboxEntryRow>,
    /// The number of unread entries (`read_at IS NULL`) in the workspace.
    pub unread: i64,
}

/// Result of [`crate::methods::HANGAR_INBOX_MARK_READ`] (e38.14): the unread
/// count AFTER the sweep (which is `0` when the whole workspace was marked).
///
/// Returning the post-sweep unread count lets the caller update the badge without
/// re-listing. `marked` is how many rows the sweep flipped (the unread count
/// before the sweep), so the client can show "marked N read" feedback.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxMarkReadResult {
    /// How many entries the sweep flipped from unread to read.
    pub marked: i64,
    /// The unread count after the sweep (`0` for a whole-workspace sweep).
    pub unread: i64,
}

/// One per-agent usage row in the dashboard rollup
/// ([`crate::methods::HANGAR_USAGE_ROLLUP`], e38.35): an agent's summed tokens +
/// cost over the runs it executed in the workspace.
///
/// Carries an `f64` cost, so it is `PartialEq` only (no `Eq`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentUsageRow {
    /// The agent the row aggregates (`agent.id`, the GROUP BY key).
    pub agent_id: String,
    /// Sum of input tokens over this agent's runs.
    pub input_tokens: i64,
    /// Sum of output tokens over this agent's runs.
    pub output_tokens: i64,
    /// Sum of cost (USD) over this agent's runs.
    pub cost_usd: f64,
    /// Number of runs this agent executed.
    pub runs: i64,
}

/// Result of [`crate::methods::HANGAR_USAGE_ROLLUP`] (e38.35): the workspace's
/// token/cost usage dashboard.
///
/// The grand totals (summed across every recorded run) drive the dashboard
/// header; `agents` is the per-agent breakdown, heaviest cost first. A workspace
/// with no recorded usage answers all-zero totals + an empty `agents` vec (the
/// empty-dashboard state). Carries `f64` costs, so it is `PartialEq` only.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct UsageRollupResult {
    /// Sum of input tokens over every recorded run in the workspace.
    pub total_input_tokens: i64,
    /// Sum of output tokens over every recorded run in the workspace.
    pub total_output_tokens: i64,
    /// Sum of cost (USD) over every recorded run in the workspace.
    pub total_cost_usd: f64,
    /// Number of recorded runs the totals aggregate.
    pub total_runs: i64,
    /// The per-agent breakdown rows, heaviest cost first.
    pub agents: Vec<AgentUsageRow>,
}

/// Params for [`crate::methods::HANGAR_PR_STATUS_REFRESH`] (e38.34): refresh the
/// CI + merge status of one issue's bound PR.
///
/// `workspace_id` is the tenant guard (a foreign-tenant issue id touches
/// nothing); `issue_id` is the issue whose latest task `result.pr_url` the daemon
/// resolves before shelling `gh`. An issue with no bound PR answers an all-unknown
/// status + no transition (a read, never an error).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PrStatusRefreshParams {
    /// The subscribed workspace the issue must belong to (tenant guard).
    pub workspace_id: String,
    /// The issue whose bound PR to refresh (`issue.id`).
    pub issue_id: String,
}

/// Result of [`crate::methods::HANGAR_PR_STATUS_REFRESH`] (e38.34): the freshly
/// fetched [`crate::pr_status::PrStatus`] plus whether the refresh moved the
/// backing issue to Done.
///
/// `transitioned_to_done` is `true` only when the fetched PR was `merged` AND the
/// issue was not already `done` — the side effect the auto-move performs. The
/// plugin uses it to optimistically reflect the column move; subscribers also see
/// the pushed `IssueUpdated` event. A degrade fetch (no PR, `gh` absent) answers
/// the all-unknown status + `transitioned_to_done: false`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PrStatusRefreshResult {
    /// The fetched CI + merge status (all-unknown on a degrade fetch).
    pub status: crate::pr_status::PrStatus,
    /// `true` when this refresh moved the issue to Done (a merged PR).
    #[serde(default)]
    pub transitioned_to_done: bool,
}

/// Params for [`crate::methods::HANGAR_TASK_TRANSITION`]: the workspace (tenant
/// guard), the task to move, and the target status (P8.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskTransitionParams {
    /// The subscribed workspace the task must belong to.
    pub workspace_id: String,
    /// The task to move.
    pub task_id: String,
    /// The target lifecycle status — one of the six `TaskStatus` wire tokens.
    pub to_status: String,
}

/// A three-state edit instruction for a nullable issue field (e38.8).
///
/// A bare `Option<T>` collapses "leave this field unchanged" and "clear this
/// field to NULL" into the same `None`, which a partial-update RPC must keep
/// distinct. This wrapper separates the two:
///
/// - the **key omitted** in the request JSON → `Keep` (leave the column as-is);
/// - the key present as **`null`** → `Clear` (set the column to NULL);
/// - the key present with a **value** → `Set(value)` (overwrite the column).
///
/// `#[serde(untagged)]` lets a wire `null` decode as `Clear` and a wire value as
/// `Set`; the *absent* case is supplied by `#[serde(default)]` on the field that
/// holds this wrapper (which yields [`FieldUpdate::Keep`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(untagged)]
pub enum FieldUpdate<T> {
    /// The key was present with an explicit `null`: clear the column to NULL.
    Clear,
    /// The key was present with a value: overwrite the column with it.
    Set(T),
    /// The key was omitted: leave the column unchanged. The `default` variant so
    /// `#[serde(default)]` on the holding field maps an absent key to this.
    #[serde(skip)]
    #[default]
    Keep,
}

impl<T> FieldUpdate<T> {
    /// `true` when this update leaves the column unchanged (the omitted case).
    #[must_use]
    pub const fn is_keep(&self) -> bool {
        matches!(self, Self::Keep)
    }
}

/// Params for [`crate::methods::HANGAR_ISSUE_UPDATE`] (e38.8): edit one issue's
/// fields, scoped to a workspace.
///
/// `workspace_id` + `issue_id` identify the row (the workspace is the
/// tenant-isolation guard — a foreign-tenant issue id touches nothing). The
/// remaining fields are partial-update instructions: a non-nullable field uses
/// `Option<T>` (`None` = leave unchanged) and a nullable field uses
/// [`FieldUpdate`] (omitted = leave, `null` = clear, value = set). The editable
/// fields are the real `issue` columns — `state` / `assignee` / `priority` /
/// `due_date`. There is no `project` column at v1, so project is not editable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct IssueUpdateParams {
    /// The subscribed workspace the issue must belong to (tenant guard).
    pub workspace_id: String,
    /// The issue to edit (`issue.id`).
    pub issue_id: String,
    /// New lifecycle state (e.g. `"in_progress"`); `None` leaves it unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// New assignee actor-ref (`"agent:<id>"` / `"member:<id>"`); omitted leaves
    /// it, an explicit `null` clears the assignee (unassign).
    #[serde(default, skip_serializing_if = "FieldUpdate::is_keep")]
    pub assignee: FieldUpdate<String>,
    /// New urgency `0..3` (P3..P0, HIGHER = MORE URGENT); `None` leaves it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i64>,
    /// New due date (epoch milliseconds); omitted leaves it, explicit `null`
    /// clears the deadline.
    #[serde(default, skip_serializing_if = "FieldUpdate::is_keep")]
    pub due_date: FieldUpdate<i64>,
}

/// Params for [`crate::methods::HANGAR_ISSUE_LABEL_ATTACH`] /
/// [`crate::methods::HANGAR_ISSUE_LABEL_DETACH`] (e38.10): attach or detach a
/// label on one issue, scoped to a workspace.
///
/// `workspace_id` + `issue_id` identify the target row (the workspace is the
/// tenant-isolation guard — a foreign-tenant issue id touches nothing). `name`
/// is the label name, resolved (or, on attach, created) within the workspace.
/// `color` is an optional presentation hint applied only when an attach mints a
/// fresh label; it is ignored on detach and when an existing label is reused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct IssueLabelParams {
    /// The subscribed workspace the issue must belong to (tenant guard).
    pub workspace_id: String,
    /// The issue to (de)label (`issue.id`).
    pub issue_id: String,
    /// The label name (resolved or, on attach, created within the workspace).
    pub name: String,
    /// Optional presentation colour (hex) applied when an attach mints a fresh
    /// label; omitted when unset, ignored on detach / label reuse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

/// Params for [`crate::methods::HANGAR_ISSUE_CREATE`] (e38.29): create one new
/// issue in a workspace.
///
/// `workspace_id` is the tenant-isolation guard — the daemon resolves it and
/// rejects a foreign one. `title` is the issue title (mandatory + non-blank).
/// `description` is optional free-form body text. `creator` is the polymorphic
/// actor-ref (`"agent:<id>"` / `"member:<id>"`) the daemon parses. The daemon
/// mints the id, stamps `created_at`, and inserts the row in the `open` state —
/// the client supplies none of those.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct IssueCreateParams {
    /// The subscribed workspace the new issue belongs to (tenant guard).
    pub workspace_id: String,
    /// The issue title (mandatory, non-blank).
    pub title: String,
    /// Optional free-form description; omitted when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The creating actor in canonical `member:<id>` / `agent:<id>` form.
    pub creator: String,
}

/// Params for [`crate::methods::HANGAR_AGENT_UPDATE`] (e38.15): edit one agent's
/// config knobs, scoped to a workspace.
///
/// `workspace_id` + `agent_id` identify the row (the workspace is the
/// tenant-isolation guard — a foreign-tenant agent id touches nothing). The
/// remaining fields are partial-update instructions: `name` is non-nullable so
/// it uses `Option<String>` (`None` = leave); the four nullable text fields use
/// [`FieldUpdate`] (omitted = leave, `null` = clear to the column default, value
/// = set); and the two JSON collection fields (`cli_args` / `agent_env`) use a
/// plain `Option` (an empty collection is a valid "no args" / "no env" value,
/// distinct from leaving the field). `agent_env` is an ordered key-value list so
/// it serialises deterministically.
///
/// This bead persists + exposes the config; the provider EXEC consumption of
/// `model` / `cli_args` is a separate bead (e38.16).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AgentUpdateParams {
    /// The subscribed workspace the agent must belong to (tenant guard).
    pub workspace_id: String,
    /// The agent to edit (`agent.id`).
    pub agent_id: String,
    /// New agent name; `None` leaves it unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// New instructions; omitted leaves it, `null` clears it, a value sets it.
    #[serde(default, skip_serializing_if = "FieldUpdate::is_keep")]
    pub instructions: FieldUpdate<String>,
    /// New model override; omitted leaves it, `null` clears it (provider
    /// default), a value sets it.
    #[serde(default, skip_serializing_if = "FieldUpdate::is_keep")]
    pub model: FieldUpdate<String>,
    /// New CLI-args list; `None` leaves it (an empty list is a valid "no args").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli_args: Option<Vec<String>>,
    /// New MCP config (raw JSON-object string); omitted leaves it, `null` clears
    /// it, a value sets it.
    #[serde(default, skip_serializing_if = "FieldUpdate::is_keep")]
    pub mcp_config: FieldUpdate<String>,
    /// New thinking level; omitted leaves it, `null` clears it, a value sets it.
    #[serde(default, skip_serializing_if = "FieldUpdate::is_keep")]
    pub thinking: FieldUpdate<String>,
    /// New per-agent env map (ordered key-value pairs); `None` leaves it (an
    /// empty list is a valid "no env").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_env: Option<Vec<(String, String)>>,
}

/// Params for [`crate::methods::HANGAR_AGENT_ARCHIVE`] (e38.15): archive or
/// un-archive one agent, scoped to a workspace.
///
/// `workspace_id` is the tenant-isolation guard — the daemon resolves it and
/// scopes the flip by `(agent_id, workspace_id)` so a foreign-tenant agent id
/// archives nothing. `archived: true` hides the agent from the active picker;
/// `false` restores it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AgentArchiveParams {
    /// The subscribed workspace the agent must belong to (tenant guard).
    pub workspace_id: String,
    /// The agent to (un)archive (`agent.id`).
    pub agent_id: String,
    /// `true` archives (hides from the active picker); `false` restores.
    pub archived: bool,
}

/// Params for [`crate::methods::HANGAR_COMMENT_ADD`] (e38.5): append one comment
/// to an issue, scoped to a workspace.
///
/// `workspace_id` is the tenant-isolation guard — the daemon resolves it and
/// rejects a foreign one, and scopes the insert by `(issue_id, workspace_id)` so
/// a cross-tenant issue id writes no comment. `author` is the polymorphic
/// actor-ref (`"agent:<id>"` / `"member:<id>"`) the daemon parses; `body` is the
/// comment text. Both are mandatory (a comment with no author or empty body is
/// rejected with `INVALID_PARAMS`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CommentAddParams {
    /// The subscribed workspace the issue must belong to (tenant guard).
    pub workspace_id: String,
    /// The issue to comment on (`issue.id`).
    pub issue_id: String,
    /// The comment author in canonical `member:<id>` / `agent:<id>` form.
    pub author: String,
    /// The comment body text.
    pub body: String,
}

/// One workspace member for the settings Members pane
/// ([`crate::methods::HANGAR_MEMBERS_LIST`], e38.11).
///
/// `user_id` is the stable member identity; `email` is the human display label;
/// `role` is the `owner`/`admin`/`member` token. A pure wire row — the pane
/// renders these read-only and the `member_set_role`/`member_remove` RPCs key off
/// `user_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberWireRow {
    /// The member's user id (`user.id`) — what set-role / remove key off.
    pub user_id: String,
    /// The member's email (`user.email`) — the display label.
    pub email: String,
    /// The member's role token (`owner` / `admin` / `member`).
    pub role: String,
}

/// Result of [`crate::methods::HANGAR_MEMBERS_LIST`] and the refreshed view the
/// `member_set_role` / `member_remove` mutations answer with (e38.11).
///
/// The workspace's members, ordered by email. The mutations re-read and return
/// this same envelope so the pane re-renders from the response without a separate
/// `members_list` round-trip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MembersListResult {
    /// The member rows.
    pub members: Vec<MemberWireRow>,
}

/// Params for [`crate::methods::HANGAR_MEMBER_SET_ROLE`] (e38.11): change one
/// member's role, scoped to a workspace.
///
/// `workspace_id` is the tenant-isolation guard — the daemon resolves it and
/// rejects a foreign one, and scopes the edit by `(workspace_id, user_id)` so a
/// cross-tenant member touches no row. `role` must be one of
/// `owner`/`admin`/`member` (an illegal token is `INVALID_PARAMS`). Demoting the
/// workspace's only owner is rejected (a workspace always keeps an owner).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MemberSetRoleParams {
    /// The subscribed workspace the member must belong to (tenant guard).
    pub workspace_id: String,
    /// The member to re-role (`user.id`).
    pub user_id: String,
    /// The new role token (`owner` / `admin` / `member`).
    pub role: String,
}

/// Params for [`crate::methods::HANGAR_MEMBER_REMOVE`] (e38.11): remove one
/// member from a workspace.
///
/// `workspace_id` is the tenant-isolation guard, scoping the removal by
/// `(workspace_id, user_id)`. Removing the workspace's only owner is rejected (a
/// workspace always keeps an owner). The `user` row itself is left intact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MemberRemoveParams {
    /// The subscribed workspace the member must belong to (tenant guard).
    pub workspace_id: String,
    /// The member to remove (`user.id`).
    pub user_id: String,
}

/// One squad for the `ainb hangar squad list` status view
/// ([`crate::methods::HANGAR_SQUADS_LIST`], e38.17).
///
/// `id` + `name` identify the squad; `leader` is the squad's leader as a
/// canonical actor-ref (`member:<id>` / `agent:<id>`) — the actor a squad-assigned
/// task routes to; `members` are the squad's member actor-refs in the same form.
/// A pure wire row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SquadWireRow {
    /// The squad's id (`squad.id`) — what `squad_member_add`/`remove` key off.
    pub id: String,
    /// The squad's name (unique within its workspace) — the display label.
    pub name: String,
    /// The squad's leader as a canonical actor-ref (`member:<id>` / `agent:<id>`).
    /// An `agent` leader is the actor a squad-assigned task is routed to.
    pub leader: String,
    /// The squad's member actor-refs (`member:<id>` / `agent:<id>`), ordered.
    pub members: Vec<String>,
}

/// Result of [`crate::methods::HANGAR_SQUADS_LIST`] and the refreshed view the
/// `squad_create` / `squad_member_add` / `squad_member_remove` mutations answer
/// with (e38.17).
///
/// The workspace's squads, ordered by name. The mutations re-read and return this
/// same envelope so a caller re-renders from the response without a separate
/// `squads_list` round-trip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SquadsListResult {
    /// The squad rows.
    pub squads: Vec<SquadWireRow>,
}

/// Params for [`crate::methods::HANGAR_SQUAD_CREATE`] (e38.17): create one squad
/// with a leader, scoped to a workspace.
///
/// `workspace_id` is the tenant-isolation guard — the daemon resolves it and
/// rejects a foreign one. `name` must be unique within the workspace (the
/// resolve-or-reject guard). `leader` is a canonical actor-ref
/// (`"agent:<id>"` / `"member:<id>"`) the daemon parses — an `agent` leader is the
/// actor a squad-assigned task is routed to (leader-routing rides this ref rather
/// than a new `ActorKind::Squad`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SquadCreateParams {
    /// The subscribed workspace the squad belongs to (tenant guard).
    pub workspace_id: String,
    /// The squad name (unique within the workspace).
    pub name: String,
    /// The squad leader in canonical `member:<id>` / `agent:<id>` form.
    pub leader: String,
}

/// Params for [`crate::methods::HANGAR_SQUAD_MEMBER_ADD`] and
/// [`crate::methods::HANGAR_SQUAD_MEMBER_REMOVE`] (e38.17): add / remove one member
/// actor from a squad, scoped to a workspace.
///
/// `workspace_id` is the tenant-isolation guard, scoping the mutation by
/// `(workspace_id, squad_id)` so a cross-tenant squad touches no row. `member` is a
/// canonical actor-ref (`"agent:<id>"` / `"member:<id>"`). Add is idempotent
/// (re-adding is a no-op); remove of an absent member is a no-op.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SquadMemberParams {
    /// The subscribed workspace the squad belongs to (tenant guard).
    pub workspace_id: String,
    /// The squad to mutate (`squad.id`).
    pub squad_id: String,
    /// The member actor in canonical `member:<id>` / `agent:<id>` form.
    pub member: String,
}

/// Params for [`crate::methods::HANGAR_SQUAD_ASSIGN`] (e38.17): route a task to a
/// squad's LEADER, scoped to a workspace.
///
/// `workspace_id` is the tenant-isolation guard, scoping the routing by
/// `(workspace_id, squad_id)` so a cross-tenant squad routes nothing. `issue_id`
/// is the issue the routed task carries (omit for a chat/ad-hoc squad task);
/// `work_dir` is the run's working directory (or omit); `priority` is the claim
/// urgency (0..3, default `0` = routine). The daemon resolves the squad's leader
/// agent id, derives the leader's runtime, and enqueues the task keyed to the
/// leader — leader routing taking effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SquadAssignParams {
    /// The subscribed workspace the squad belongs to (tenant guard).
    pub workspace_id: String,
    /// The squad whose leader the task routes to (`squad.id`).
    pub squad_id: String,
    /// The issue the routed task carries (`issue.id`), or `None` for an ad-hoc task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_id: Option<String>,
    /// The run's working directory, or `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_dir: Option<String>,
    /// Claim urgency (0..3, higher = more urgent); omitted defaults to `0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i64>,
}

/// Result of [`crate::methods::HANGAR_SQUAD_ASSIGN`] (e38.17): the enqueued task
/// id plus the leader identity it routed to, so a caller can report *who* the
/// squad's work landed on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SquadAssignResult {
    /// The enqueued `agent_task_queue` row id.
    pub task_id: String,
    /// The leader agent the task was routed to (`agent.id`).
    pub leader_agent_id: String,
    /// The runtime the task was keyed to (the leader agent's `runtime_id`).
    pub runtime_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::PresenceState;

    /// The params + result envelopes round-trip through JSON.
    #[test]
    fn envelopes_roundtrip() {
        let p = WorkspaceScopedParams {
            workspace_id: "ws-1".into(),
        };
        let s = serde_json::to_string(&p).unwrap();
        assert_eq!(
            serde_json::from_str::<WorkspaceScopedParams>(&s).unwrap(),
            p
        );

        // A pre-cursor subscribe (no `since_seq`) omits the field entirely, so a
        // legacy `{ workspace_id }` frame decodes and the field defaults to None.
        let bare: WorkspaceSubscribeParams =
            serde_json::from_str(r#"{"workspace_id":"ws-1"}"#).unwrap();
        assert_eq!(bare.since_seq, None);
        assert_eq!(
            serde_json::to_string(&bare).unwrap(),
            r#"{"workspace_id":"ws-1"}"#,
            "an absent cursor is omitted from the wire"
        );

        // A resume subscribe carries the cursor round-trip.
        let resume = WorkspaceSubscribeParams {
            workspace_id: "ws-1".into(),
            since_seq: Some(42),
        };
        let s = serde_json::to_string(&resume).unwrap();
        assert_eq!(
            serde_json::from_str::<WorkspaceSubscribeParams>(&s).unwrap(),
            resume
        );

        // The subscribe ack carries the real head cursor under `snapshot`.
        let ack = SubscribeResult {
            snapshot: SubscribeSnapshot { cursor: 7 },
        };
        let s = serde_json::to_string(&ack).unwrap();
        assert_eq!(s, r#"{"snapshot":{"cursor":7}}"#);
        assert_eq!(serde_json::from_str::<SubscribeResult>(&s).unwrap(), ack);

        let issues = IssuesListResult {
            issues: vec![IssueRow {
                id: ainb_hangar_core::ids::IssueId::from_str("i1").unwrap(),
                display_id: None,
                workspace_id: "ws-1".into(),
                title: "Refactor API".into(),
                description: None,
                state: "open".into(),
                assignee: None,
                creator: "member:alice".into(),
                created_at: 0,
                priority: 0,
                due_date: None,
                labels: Vec::new(),
                pr_url: None,
            }],
        };
        let s = serde_json::to_string(&issues).unwrap();
        assert_eq!(
            serde_json::from_str::<IssuesListResult>(&s).unwrap(),
            issues
        );

        let actors = AgentsListResult {
            actors: vec![ActorRow {
                actor_ref: "agent:a1".into(),
                display_name: "claude-agent".into(),
                subtitle: "agent · claude".into(),
                presence: PresenceState::Online,
                is_agent: true,
                recent_rank: Some(0),
            }],
        };
        let s = serde_json::to_string(&actors).unwrap();
        assert_eq!(
            serde_json::from_str::<AgentsListResult>(&s).unwrap(),
            actors
        );

        let skills = SkillsListResult {
            skills: vec![SkillRow {
                slug: "commit".into(),
                name: "commit".into(),
                used: true,
                updated_at: 0,
            }],
        };
        let s = serde_json::to_string(&skills).unwrap();
        assert_eq!(
            serde_json::from_str::<SkillsListResult>(&s).unwrap(),
            skills
        );
    }

    /// The e38.13 cross-entity search envelopes round-trip through JSON, and the
    /// `kind` tag + `screen` token carry their wire spelling unchanged.
    #[test]
    fn search_envelopes_roundtrip() {
        let p = SearchParams {
            workspace_id: "ws-1".into(),
            query: "refactor".into(),
        };
        let s = serde_json::to_string(&p).unwrap();
        assert_eq!(serde_json::from_str::<SearchParams>(&s).unwrap(), p);

        let result = SearchResult {
            entries: vec![
                SearchEntry {
                    kind: SearchEntryKind::Issue,
                    id: "issue-1".into(),
                    label: "Refactor API".into(),
                    screen: SearchEntryKind::Issue.target_screen().into(),
                },
                SearchEntry {
                    kind: SearchEntryKind::Skill,
                    id: "skill-1".into(),
                    label: "refactor".into(),
                    screen: SearchEntryKind::Skill.target_screen().into(),
                },
            ],
        };
        let s = serde_json::to_string(&result).unwrap();
        assert_eq!(serde_json::from_str::<SearchResult>(&s).unwrap(), result);
        // The wire tag spelling is part of the contract.
        assert!(s.contains("\"kind\":\"issue\""), "issue kind tag: {s}");
        assert!(s.contains("\"kind\":\"skill\""), "skill kind tag: {s}");
        assert!(
            s.contains("\"screen\":\"issue_list\""),
            "issue screen token: {s}"
        );
        assert!(
            s.contains("\"screen\":\"skill_manager\""),
            "skill screen token: {s}"
        );
    }

    /// Each search kind maps to a stable jump-target screen token; agents fall
    /// back to the issue list (no dedicated agent screen at v1).
    #[test]
    fn search_kind_target_screens() {
        assert_eq!(SearchEntryKind::Issue.target_screen(), "issue_list");
        assert_eq!(SearchEntryKind::Agent.target_screen(), "issue_list");
        assert_eq!(SearchEntryKind::Skill.target_screen(), "skill_manager");
        assert_eq!(SearchEntryKind::Autopilot.target_screen(), "autopilots");
    }

    /// The P6.5 skill-management envelopes round-trip through JSON.
    #[test]
    fn p6_skill_envelopes_roundtrip() {
        let get = SkillGetParams {
            workspace_id: "ws-1".into(),
            skill_id: "skill-commit".into(),
        };
        let s = serde_json::to_string(&get).unwrap();
        assert_eq!(serde_json::from_str::<SkillGetParams>(&s).unwrap(), get);

        let detail = SkillDetail {
            slug: "commit".into(),
            name: "commit".into(),
            description: Some("Create well-formatted commits".into()),
            body: Some("# Commit\n".into()),
            files: vec![SkillFile {
                path: "SKILL.md".into(),
            }],
        };
        let s = serde_json::to_string(&detail).unwrap();
        assert_eq!(serde_json::from_str::<SkillDetail>(&s).unwrap(), detail);

        let sync = SkillsSyncParams {
            workspace_id: "ws-1".into(),
            source_path: Some("/tmp/skills".into()),
        };
        let s = serde_json::to_string(&sync).unwrap();
        assert_eq!(serde_json::from_str::<SkillsSyncParams>(&s).unwrap(), sync);
        // `source_path` is omitted when None.
        let no_src = SkillsSyncParams {
            workspace_id: "ws-1".into(),
            source_path: None,
        };
        assert_eq!(
            serde_json::to_string(&no_src).unwrap(),
            "{\"workspace_id\":\"ws-1\"}"
        );

        let report = SkillsSyncResult {
            imported: vec!["commit".into(), "review".into()],
            count: 2,
        };
        let s = serde_json::to_string(&report).unwrap();
        assert_eq!(
            serde_json::from_str::<SkillsSyncResult>(&s).unwrap(),
            report
        );

        let attach = SkillAttachParams {
            workspace_id: "ws-1".into(),
            agent_id: "agent-1".into(),
            skill_id: "skill-commit".into(),
        };
        let s = serde_json::to_string(&attach).unwrap();
        assert_eq!(
            serde_json::from_str::<SkillAttachParams>(&s).unwrap(),
            attach
        );
    }

    /// The P7.5 autopilot envelopes round-trip through JSON.
    #[test]
    fn p7_autopilot_envelopes_roundtrip() {
        let list = AutopilotsListResult {
            autopilots: vec![AutopilotRow {
                id: "ap-1".into(),
                workspace_id: "ws-1".into(),
                agent_id: "agent-1".into(),
                name: "daily-triage".into(),
                cron_expr: "0 9 * * 1-5".into(),
                next_tick_at: Some(1_700_000_000_000),
                enabled: true,
                last_run_status: Some("completed".into()),
                last_run_at: Some(1_699_000_000_000),
            }],
        };
        let s = serde_json::to_string(&list).unwrap();
        assert_eq!(
            serde_json::from_str::<AutopilotsListResult>(&s).unwrap(),
            list
        );

        let runs_params = AutopilotRunsParams {
            workspace_id: "ws-1".into(),
            autopilot_id: "ap-1".into(),
            limit: 10,
        };
        let s = serde_json::to_string(&runs_params).unwrap();
        assert_eq!(
            serde_json::from_str::<AutopilotRunsParams>(&s).unwrap(),
            runs_params
        );

        let runs = AutopilotRunsResult {
            runs: vec![AutopilotRunRow {
                id: "run-1".into(),
                autopilot_id: "ap-1".into(),
                started_at: 1_699_000_000_000,
                completed_at: Some(1_699_000_120_000),
                status: "completed".into(),
            }],
        };
        let s = serde_json::to_string(&runs).unwrap();
        assert_eq!(
            serde_json::from_str::<AutopilotRunsResult>(&s).unwrap(),
            runs
        );

        let fire = AutopilotFireNowParams {
            workspace_id: "ws-1".into(),
            autopilot_id: "ap-1".into(),
        };
        let s = serde_json::to_string(&fire).unwrap();
        assert_eq!(
            serde_json::from_str::<AutopilotFireNowParams>(&s).unwrap(),
            fire
        );

        let toggle = AutopilotSetEnabledParams {
            workspace_id: "ws-1".into(),
            autopilot_id: "ap-1".into(),
            enabled: false,
        };
        let s = serde_json::to_string(&toggle).unwrap();
        assert_eq!(
            serde_json::from_str::<AutopilotSetEnabledParams>(&s).unwrap(),
            toggle
        );
    }

    /// The P8.4 Kanban task envelopes round-trip through JSON.
    #[test]
    fn p8_task_envelopes_roundtrip() {
        let list = TasksListResult {
            tasks: vec![TaskCardRow {
                id: ainb_hangar_core::ids::TaskId::from_str("task-1").unwrap(),
                workspace_id: "ws-1".into(),
                agent_id: "agent-1".into(),
                issue_id: Some("issue-1".into()),
                status: "running".into(),
                priority: 2,
                created_at: 1_700_000_000_000,
            }],
        };
        let s = serde_json::to_string(&list).unwrap();
        assert_eq!(serde_json::from_str::<TasksListResult>(&s).unwrap(), list);

        let transition = TaskTransitionParams {
            workspace_id: "ws-1".into(),
            task_id: "task-1".into(),
            to_status: "done".into(),
        };
        let s = serde_json::to_string(&transition).unwrap();
        assert_eq!(
            serde_json::from_str::<TaskTransitionParams>(&s).unwrap(),
            transition
        );
    }

    /// The usage-rollup result + its per-agent rows round-trip through JSON,
    /// preserving the grand totals and the ordered breakdown (e38.35).
    #[test]
    fn e38_usage_rollup_roundtrips() {
        let rollup = UsageRollupResult {
            total_input_tokens: 3500,
            total_output_tokens: 1100,
            total_cost_usd: 0.055,
            total_runs: 3,
            agents: vec![
                AgentUsageRow {
                    agent_id: "agent-y".into(),
                    input_tokens: 2000,
                    output_tokens: 800,
                    cost_usd: 0.04,
                    runs: 1,
                },
                AgentUsageRow {
                    agent_id: "agent-x".into(),
                    input_tokens: 1500,
                    output_tokens: 300,
                    cost_usd: 0.015,
                    runs: 2,
                },
            ],
        };
        let s = serde_json::to_string(&rollup).unwrap();
        let back: UsageRollupResult = serde_json::from_str(&s).unwrap();
        assert_eq!(back, rollup);
        // The empty-dashboard state is the type default (all-zero, no agents).
        assert_eq!(UsageRollupResult::default().agents.len(), 0);
        assert_eq!(UsageRollupResult::default().total_runs, 0);
    }

    /// The PR-status refresh params + result round-trip through JSON, and the
    /// result's `transitioned_to_done` defaults to `false` when absent (e38.34).
    #[test]
    fn e38_pr_status_refresh_roundtrips() {
        use crate::pr_status::{CiRollup, MergeState, Mergeable, PrStatus};

        let params = PrStatusRefreshParams {
            workspace_id: "ws-1".into(),
            issue_id: "issue-1".into(),
        };
        let s = serde_json::to_string(&params).unwrap();
        assert_eq!(
            serde_json::from_str::<PrStatusRefreshParams>(&s).unwrap(),
            params
        );

        let result = PrStatusRefreshResult {
            status: PrStatus {
                ci: CiRollup::Pass,
                mergeable: Mergeable::Mergeable,
                state: MergeState::Merged,
            },
            transitioned_to_done: true,
        };
        let s = serde_json::to_string(&result).unwrap();
        assert_eq!(
            serde_json::from_str::<PrStatusRefreshResult>(&s).unwrap(),
            result
        );

        // An old-reader result (just the status, no flag) defaults the flag.
        let back: PrStatusRefreshResult = serde_json::from_str(
            r#"{"status":{"ci":"unknown","mergeable":"unknown","state":"unknown"}}"#,
        )
        .unwrap();
        assert!(!back.transitioned_to_done);
        // The all-unknown degrade result is the type default.
        assert_eq!(
            PrStatusRefreshResult::default(),
            PrStatusRefreshResult::default()
        );
        assert!(!PrStatusRefreshResult::default().transitioned_to_done);
    }

    /// A pre-priority snapshot (no `priority` key) still decodes: the field
    /// defaults to 0 (P3, routine), so an old daemon's `tasks_list` stays
    /// readable.
    #[test]
    fn p8_task_card_priority_defaults_when_absent() {
        let json = r#"{"id":"task-1","workspace_id":"ws-1","agent_id":"agent-1","issue_id":null,"status":"queued","created_at":1}"#;
        let row: TaskCardRow = serde_json::from_str(json).unwrap();
        assert_eq!(row.priority, 0, "absent priority decodes to the P3 default");
    }

    /// `IssueUpdateParams` round-trips, and the three-state nullable wrapper
    /// keeps "omitted" (`Keep`), "explicit null" (`Clear`), and "value" (`Set`)
    /// distinct on the wire (e38.8).
    #[test]
    fn e38_issue_update_params_roundtrip_and_three_state() {
        // A full edit: change state + reassign + bump priority + set a due date.
        let full = IssueUpdateParams {
            workspace_id: "ws-1".into(),
            issue_id: "issue-1".into(),
            state: Some("in_progress".into()),
            assignee: FieldUpdate::Set("agent:a1".into()),
            priority: Some(3),
            due_date: FieldUpdate::Set(1_700_000_000_000),
        };
        let s = serde_json::to_string(&full).unwrap();
        assert_eq!(serde_json::from_str::<IssueUpdateParams>(&s).unwrap(), full);

        // The minimal edit (only the row identity) omits every optional field, so
        // each decodes to its leave-unchanged variant.
        let minimal = r#"{"workspace_id":"ws-1","issue_id":"issue-1"}"#;
        let p: IssueUpdateParams = serde_json::from_str(minimal).unwrap();
        assert_eq!(p.state, None, "absent state leaves it unchanged");
        assert_eq!(p.priority, None, "absent priority leaves it unchanged");
        assert!(p.assignee.is_keep(), "absent assignee leaves it unchanged");
        assert!(p.due_date.is_keep(), "absent due_date leaves it unchanged");
        // Keep-valued fields are omitted on re-serialize (byte-minimal request).
        assert_eq!(serde_json::to_string(&p).unwrap(), minimal);

        // An explicit `null` is the CLEAR instruction, distinct from omission.
        let clear =
            r#"{"workspace_id":"ws-1","issue_id":"issue-1","assignee":null,"due_date":null}"#;
        let p: IssueUpdateParams = serde_json::from_str(clear).unwrap();
        assert_eq!(p.assignee, FieldUpdate::Clear, "null assignee → clear");
        assert_eq!(p.due_date, FieldUpdate::Clear, "null due_date → clear");
    }

    /// `CommentAddParams` round-trips through JSON with all four mandatory fields
    /// preserved (e38.5).
    #[test]
    fn e38_comment_add_params_roundtrip() {
        let params = CommentAddParams {
            workspace_id: "ws-1".into(),
            issue_id: "issue-1".into(),
            author: "member:alice".into(),
            body: "looks good to me".into(),
        };
        let s = serde_json::to_string(&params).unwrap();
        assert_eq!(
            serde_json::from_str::<CommentAddParams>(&s).unwrap(),
            params
        );
    }

    /// `AgentUpdateParams` round-trips, and the three-state nullable wrapper keeps
    /// "omitted" (`Keep`), "explicit null" (`Clear`), and "value" (`Set`) distinct
    /// for the text knobs (e38.15).
    #[test]
    fn e38_agent_update_params_roundtrip_and_three_state() {
        // A full edit: rename + set every config knob.
        let full = AgentUpdateParams {
            workspace_id: "ws-1".into(),
            agent_id: "agent-1".into(),
            name: Some("Builder Pro".into()),
            instructions: FieldUpdate::Set("Be precise.".into()),
            model: FieldUpdate::Set("claude-opus-4".into()),
            cli_args: Some(vec!["--verbose".into()]),
            mcp_config: FieldUpdate::Set(r#"{"servers":{}}"#.into()),
            thinking: FieldUpdate::Set("high".into()),
            agent_env: Some(vec![("FOO".into(), "bar".into())]),
        };
        let s = serde_json::to_string(&full).unwrap();
        assert_eq!(serde_json::from_str::<AgentUpdateParams>(&s).unwrap(), full);

        // The minimal edit (only the row identity) omits every optional field, so
        // each decodes to its leave-unchanged variant and re-serialises byte-minimal.
        let minimal = r#"{"workspace_id":"ws-1","agent_id":"agent-1"}"#;
        let p: AgentUpdateParams = serde_json::from_str(minimal).unwrap();
        assert_eq!(p.name, None, "absent name leaves it unchanged");
        assert!(p.instructions.is_keep(), "absent instructions leaves it");
        assert!(p.model.is_keep(), "absent model leaves it");
        assert_eq!(p.cli_args, None, "absent cli_args leaves it");
        assert!(p.mcp_config.is_keep(), "absent mcp_config leaves it");
        assert!(p.thinking.is_keep(), "absent thinking leaves it");
        assert_eq!(p.agent_env, None, "absent agent_env leaves it");
        assert_eq!(serde_json::to_string(&p).unwrap(), minimal);

        // An explicit `null` is the CLEAR instruction, distinct from omission.
        let clear = r#"{"workspace_id":"ws-1","agent_id":"agent-1","model":null,"thinking":null}"#;
        let p: AgentUpdateParams = serde_json::from_str(clear).unwrap();
        assert_eq!(p.model, FieldUpdate::Clear, "null model → clear");
        assert_eq!(p.thinking, FieldUpdate::Clear, "null thinking → clear");
    }

    /// `AgentArchiveParams` round-trips through JSON (e38.15).
    #[test]
    fn e38_agent_archive_params_roundtrip() {
        let params = AgentArchiveParams {
            workspace_id: "ws-1".into(),
            agent_id: "agent-1".into(),
            archived: true,
        };
        let s = serde_json::to_string(&params).unwrap();
        assert_eq!(
            serde_json::from_str::<AgentArchiveParams>(&s).unwrap(),
            params
        );
    }

    /// The e38.11 member-management envelopes round-trip through JSON.
    #[test]
    fn e38_member_envelopes_roundtrip() {
        let list = MembersListResult {
            members: vec![
                MemberWireRow {
                    user_id: "u-amy".into(),
                    email: "amy@x.io".into(),
                    role: "owner".into(),
                },
                MemberWireRow {
                    user_id: "u-bob".into(),
                    email: "bob@x.io".into(),
                    role: "admin".into(),
                },
            ],
        };
        let s = serde_json::to_string(&list).unwrap();
        assert_eq!(serde_json::from_str::<MembersListResult>(&s).unwrap(), list);

        let set_role = MemberSetRoleParams {
            workspace_id: "ws-1".into(),
            user_id: "u-bob".into(),
            role: "member".into(),
        };
        let s = serde_json::to_string(&set_role).unwrap();
        assert_eq!(
            serde_json::from_str::<MemberSetRoleParams>(&s).unwrap(),
            set_role
        );

        let remove = MemberRemoveParams {
            workspace_id: "ws-1".into(),
            user_id: "u-bob".into(),
        };
        let s = serde_json::to_string(&remove).unwrap();
        assert_eq!(
            serde_json::from_str::<MemberRemoveParams>(&s).unwrap(),
            remove
        );
    }

    /// The e38.17 squad envelopes round-trip through JSON.
    #[test]
    fn e38_squad_envelopes_roundtrip() {
        let list = SquadsListResult {
            squads: vec![SquadWireRow {
                id: "s1".into(),
                name: "alpha".into(),
                leader: "agent:a-lead".into(),
                members: vec!["agent:a-1".into(), "member:u-1".into()],
            }],
        };
        let s = serde_json::to_string(&list).unwrap();
        assert_eq!(serde_json::from_str::<SquadsListResult>(&s).unwrap(), list);

        let create = SquadCreateParams {
            workspace_id: "ws-1".into(),
            name: "alpha".into(),
            leader: "agent:a-lead".into(),
        };
        let s = serde_json::to_string(&create).unwrap();
        assert_eq!(
            serde_json::from_str::<SquadCreateParams>(&s).unwrap(),
            create
        );

        let member = SquadMemberParams {
            workspace_id: "ws-1".into(),
            squad_id: "s1".into(),
            member: "agent:a-1".into(),
        };
        let s = serde_json::to_string(&member).unwrap();
        assert_eq!(
            serde_json::from_str::<SquadMemberParams>(&s).unwrap(),
            member
        );

        let assign = SquadAssignParams {
            workspace_id: "ws-1".into(),
            squad_id: "s1".into(),
            issue_id: Some("issue-1".into()),
            work_dir: Some("/tmp/run".into()),
            priority: Some(2),
        };
        let s = serde_json::to_string(&assign).unwrap();
        assert_eq!(
            serde_json::from_str::<SquadAssignParams>(&s).unwrap(),
            assign
        );

        let assigned = SquadAssignResult {
            task_id: "task-1".into(),
            leader_agent_id: "a-lead".into(),
            runtime_id: "rt-lead".into(),
        };
        let s = serde_json::to_string(&assigned).unwrap();
        assert_eq!(
            serde_json::from_str::<SquadAssignResult>(&s).unwrap(),
            assigned
        );
    }

    /// `SquadAssignParams` omits its optional fields on the wire and defaults
    /// them to `None` when a caller leaves them out (only the required ids sent).
    #[test]
    fn squad_assign_params_optionals_default_to_none() {
        let minimal: SquadAssignParams =
            serde_json::from_str(r#"{"workspace_id":"ws-1","squad_id":"s1"}"#).unwrap();
        assert_eq!(minimal.issue_id, None);
        assert_eq!(minimal.work_dir, None);
        assert_eq!(minimal.priority, None);
        // The serialized form drops the absent optionals entirely.
        let s = serde_json::to_string(&minimal).unwrap();
        assert_eq!(s, r#"{"workspace_id":"ws-1","squad_id":"s1"}"#);
    }
}
