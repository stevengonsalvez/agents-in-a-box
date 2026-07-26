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

use ainb_hangar_core::channel::ChannelSet;
use serde::{Deserialize, Serialize};

use crate::events::{
    ActorRow, AgentSkillLinkRow, AttentionRow, AutopilotRow, AutopilotRunRow, InboxEntryRow,
    IssueRow, SkillFile, SkillRow, TaskCardRow,
};

/// `serde(default)` helper — an absent `is_answer` defaults to `true`.
const fn default_true() -> bool {
    true
}

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

/// Params for [`crate::methods::HANGAR_SKILL_SET_ENABLED`] (parity #24).
///
/// The same `(workspace, agent, skill)` triple `SkillAttachParams` carries, plus
/// the target state. Explicit rather than a flip so the call is idempotent and
/// two racing clients converge instead of ping-ponging.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillSetEnabledParams {
    /// The subscribed workspace both ids must belong to (tenant guard).
    pub workspace_id: String,
    /// The agent whose link is being toggled.
    pub agent_id: String,
    /// The skill whose link is being toggled.
    pub skill_id: String,
    /// The target state: `true` = the link materialises, `false` = it stays
    /// attached but is suppressed.
    pub enabled: bool,
}

/// Params for [`crate::methods::HANGAR_AGENT_SKILLS_LIST`] (parity #24).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSkillsListParams {
    /// The subscribed workspace the agent must belong to (tenant guard).
    pub workspace_id: String,
    /// The agent whose attachments are listed.
    pub agent_id: String,
}

/// Result of [`crate::methods::HANGAR_AGENT_SKILLS_LIST`]: one agent's skill
/// links, enabled and disabled alike, ordered by skill name (parity #24).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSkillsListResult {
    /// Every link on the agent — a disabled one is still listed, flagged.
    pub links: Vec<AgentSkillLinkRow>,
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

/// Params for [`crate::methods::ATTENTION_LIST`] (spec P2) — the scope of the
/// open-attention list to snapshot.
///
/// Three scopes, matching the store repo:
/// - `fleet = true` → EVERY open row across every workspace (and the
///   no-workspace host sessions). This is the converged control centre's
///   host-wide feed; `workspace_id` is ignored.
/// - `fleet = false`, `workspace_id = Some(ws)` → that workspace's open rows.
/// - `fleet = false`, `workspace_id = None` → the open rows owned by NO
///   workspace (hand-started host sessions).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttentionListParams {
    /// The workspace to scope to (ignored when `fleet`); `None` = the
    /// no-workspace host rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    /// `true` = host-wide feed (every workspace + host); `false` = the
    /// workspace-scoped list selected by `workspace_id`.
    #[serde(default)]
    pub fleet: bool,
}

/// Result of [`crate::methods::ATTENTION_LIST`] (spec P2): the open attention
/// rows for the requested scope, oldest-first (the longest-waiting request is
/// the most urgent).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttentionListResult {
    /// The open attention rows, oldest-first.
    pub attention: Vec<AttentionRow>,
}

/// Params for [`crate::methods::ATTENTION_SUBSCRIBE`] (spec P2) — open the
/// fleet-wide attention stream.
///
/// Unlike [`SubscribeParams`], attention is NOT workspace-partitioned: the
/// control centre answers for the whole host. `workspace_id = None` (the
/// default) subscribes to EVERY session's attention; a `Some(ws)` narrows the
/// live deltas to one workspace for a scoped surface. There is no `since_seq`:
/// the durable source is the `attention` table (not the event-log outbox), so a
/// reconnecting surface catches up from the snapshot this subscribe returns, not
/// a seq replay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttentionSubscribeParams {
    /// Narrow the live stream to one workspace, or `None` (default) for the
    /// fleet-wide stream every session raises into.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
}

/// Result of [`crate::methods::ATTENTION_SUBSCRIBE`] (spec P2): the initial open
/// snapshot, after which the daemon pushes `AttentionRaised` / `AttentionAnswered`
/// deltas live (the "snapshot then deltas" contract, mirroring
/// [`SubscribeSnapshot`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttentionSubscribeResult {
    /// The current open attention rows, oldest-first.
    pub attention: Vec<AttentionRow>,
}

/// Params for [`crate::methods::ATTENTION_ANSWER`] (spec P2) — answer one open
/// attention row from any surface.
///
/// The daemon runs the first-answer-wins guard (a conditional `open → answered`
/// flip) and, on the win, the C1 cwd-ambiguity guard before delivering `answer`
/// into the raising session via the one verified send path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnswerParams {
    /// The attention row to answer.
    pub attention_id: String,
    /// The answer text delivered into the session's open picker / prompt.
    pub answer: String,
    /// The surface/actor answering (`tui` / `web` / `bridge` / `atc` / a handle)
    /// — recorded as `answered_by` and carried on the `AttentionAnswered` event.
    pub answered_by: String,
    /// `true` (default) marks a safety-critical interview answer: on an ambiguous
    /// target the send is REFUSED rather than routed by a cwd guess (C1). A
    /// looser broadcast passes `false` but gets the same refusal — the safe call.
    #[serde(default = "default_true")]
    pub is_answer: bool,
}

/// Result of [`crate::methods::ATTENTION_ANSWER`] (spec P2): what happened to the
/// answer, tagged so every surface renders the right feedback.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum AnswerResult {
    /// The answer won the race and was delivered into the session.
    Delivered {
        /// A short delivery description, e.g. `tmux (session-name)`.
        via: String,
    },
    /// A prior answer already resolved this row (first-answer-wins loser). No
    /// second delivery happened.
    AlreadyAnswered {
        /// The surface/actor that won the earlier answer.
        by: String,
    },
    /// The target session was ambiguous (C1 guard) — the answer was REFUSED
    /// rather than risk mis-routing to the wrong agent.
    Ambiguous {
        /// Why the target could not be resolved unambiguously.
        reason: String,
    },
    /// No live session matched the row (the target may have exited).
    NoTarget {
        /// A human-readable explanation.
        reason: String,
    },
    /// The row was flipped to answered but the last-mile send failed; the send
    /// can be retried (the row stays answered — the winner is recorded).
    DeliveryFailed {
        /// Why the send did not land.
        reason: String,
    },
}

/// Params for [`crate::methods::ATC_REGISTER`] (spec P9, D12): register (or
/// re-register) an ATC instance on the daemon.
///
/// Only `name` is required; the rest carry the daemon defaults when omitted
/// (`*/2 * * * *` heartbeat, cap 3, idle-pause 60m), so `ainb fleet atc setup
/// <name>` maps to a minimal call. Re-registering the same name refreshes the
/// config + reschedules (idempotent).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AtcRegisterParams {
    /// The sanitized instance name (the registry key).
    pub name: String,
    /// The directory the ATC session drives from (empty when unset).
    #[serde(default)]
    pub cwd: String,
    /// The ATC session's tmux target, or `None` when not spawned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmux_session: Option<String>,
    /// The heartbeat cron (UTC); `None` uses the daemon default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heartbeat_cron: Option<String>,
    /// The per-session auto-`continue` cap; `None` uses the daemon default (3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub err_retry_cap: Option<i64>,
    /// The idle-pause threshold in minutes; `None` uses the daemon default (60).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_pause_min: Option<i64>,
}

/// Result of [`crate::methods::ATC_REGISTER`]: the persisted instance name + its
/// computed next heartbeat tick (epoch-ms, `None` when the cron has no future
/// match).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AtcRegisterResult {
    /// The registered instance name.
    pub name: String,
    /// The cached next heartbeat instant (epoch-ms), or `None`.
    pub next_tick_at: Option<i64>,
}

/// One registered ATC instance in the [`crate::methods::ATC_LIST`] result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AtcInstanceWire {
    /// The instance name.
    pub name: String,
    /// The directory the ATC session drives from.
    pub cwd: String,
    /// The ATC session's tmux target, or `None`.
    pub tmux_session: Option<String>,
    /// The heartbeat cron (UTC).
    pub heartbeat_cron: String,
    /// The per-session auto-`continue` cap.
    pub err_retry_cap: i64,
    /// The idle-pause threshold in minutes.
    pub idle_pause_min: i64,
    /// The cached next heartbeat instant (epoch-ms), or `None`.
    pub next_tick_at: Option<i64>,
    /// Whether the heartbeat cron considers this instance.
    pub enabled: bool,
    /// Epoch-ms of the last fired heartbeat, or `None`.
    pub last_heartbeat_at: Option<i64>,
}

/// Result of [`crate::methods::ATC_LIST`]: every registered ATC instance,
/// name-ordered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AtcListResult {
    /// The registered instances.
    pub instances: Vec<AtcInstanceWire>,
}

/// Params for [`crate::methods::ATC_ESCALATE`] (spec P9, D12): raise an ATC
/// escalation as an `escalation` attention row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AtcEscalateParams {
    /// The escalating ATC instance.
    pub instance_name: String,
    /// The monitored session the escalation is about.
    pub session_id: String,
    /// The session's working directory (empty when unknown) — carried so the
    /// answer router can correlate it.
    #[serde(default)]
    pub cwd: String,
    /// The owning workspace, or `None` for a host session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    /// Why the session is being escalated (rendered on the attention card).
    pub reason: String,
}

/// Result of [`crate::methods::ATC_ESCALATE`]: the raised attention id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AtcEscalateResult {
    /// The id of the raised `escalation` attention row.
    pub attention_id: String,
}

/// Params for [`crate::methods::ATC_UNREGISTER`] (spec P9, D12): disable a
/// registered ATC instance's heartbeat cron by name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AtcUnregisterParams {
    /// The sanitized instance name to disable.
    pub name: String,
}

/// Result of [`crate::methods::ATC_UNREGISTER`]: the instance name and whether it
/// was a registered instance the daemon disabled (`false` = unknown name, a
/// no-op).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AtcUnregisterResult {
    /// The instance name that was targeted.
    pub name: String,
    /// `true` when a registered instance was disabled; `false` for an unknown name.
    pub disabled: bool,
}

/// One indexed profile in the [`crate::methods::PROFILE_LIST`] result (spec P5):
/// the identity fields of a `~/.agents-in-a-box/profiles/<slug>.md` master, projected
/// from the daemon's fs-watch-maintained index. The body lives on disk, never on
/// the wire here — fetch it (and its compile previews) via
/// [`crate::methods::PROFILE_GET`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileRow {
    /// The profile slug (PK) — file stem + board-assignee slug (D16).
    pub slug: String,
    /// The logical model tier token (`premium` / `balanced` / `fast`).
    pub tier: String,
    /// The master file's last-modified time (epoch milliseconds).
    pub mtime: i64,
}

/// Result of [`crate::methods::PROFILE_LIST`] (spec P5): every indexed profile,
/// slug-ordered.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ProfileListResult {
    /// The indexed profiles, slug-ordered.
    pub profiles: Vec<ProfileRow>,
}

/// Params for [`crate::methods::PROFILE_GET`] (spec P5) — fetch one master + its
/// two compile previews by slug.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileGetParams {
    /// The profile slug to fetch.
    pub slug: String,
}

/// The lossy Codex compile preview carried in a [`ProfileGetResult`] (D14): the
/// `[profiles.<slug>]` config fragment, the prompt body, and one warning per
/// dropped Claude-only field.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CodexPreview {
    /// The `[profiles.<slug>]` fragment appended to `~/.codex/config.toml`.
    pub config_fragment: String,
    /// The `~/.codex/prompts/<slug>.md` body.
    pub prompt: String,
    /// One human-readable warning per dropped Claude-only field (`tools`,
    /// `color`); empty when nothing was dropped.
    pub warnings: Vec<String>,
}

/// Result of [`crate::methods::PROFILE_GET`] (spec P5): the parsed master fields
/// plus both compile previews, or `found = false` for an unknown slug.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ProfileGetResult {
    /// `false` when the slug is not indexed / has no master on disk; every other
    /// field is then at its default. A read, so a miss is not an error.
    pub found: bool,
    /// The profile slug.
    pub slug: String,
    /// The one-line description.
    pub description: String,
    /// The logical model tier token (`premium` / `balanced` / `fast`).
    pub tier: String,
    /// The Claude-only tool allowlist (comma-split).
    pub tools: Vec<String>,
    /// The Claude-only card color, or empty when unset.
    pub color: String,
    /// The system-prompt body.
    pub body: String,
    /// The lossless Claude subagent `.md` preview (tier resolved to a Claude
    /// model, every field preserved).
    pub claude_preview: String,
    /// The lossy Codex compile preview + dropped-field warnings.
    pub codex_preview: CodexPreview,
}

impl ProfileGetResult {
    /// The not-found result for an unknown slug (`found = false`, everything else
    /// default).
    #[must_use]
    pub fn not_found() -> Self {
        Self::default()
    }
}

/// Params for [`crate::methods::PROFILE_UPSERT`] (spec P5) — create or replace a
/// master on disk. The daemon serialises these fields into the canonical
/// `~/.agents-in-a-box/profiles/<slug>.md` and refreshes the index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileUpsertParams {
    /// The profile slug (validated kebab-case; rejected otherwise).
    pub slug: String,
    /// The one-line description.
    #[serde(default)]
    pub description: String,
    /// The logical model tier token (`premium` / `balanced` / `fast`).
    pub tier: String,
    /// The Claude-only tool allowlist.
    #[serde(default)]
    pub tools: Vec<String>,
    /// The Claude-only card color, or empty for none.
    #[serde(default)]
    pub color: String,
    /// The system-prompt body.
    #[serde(default)]
    pub body: String,
}

/// Result of [`crate::methods::PROFILE_UPSERT`] (spec P5): the written slug and
/// its resolved absolute master path (echoed so a surface can confirm where the
/// file landed).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileUpsertResult {
    /// The slug that was written.
    pub slug: String,
    /// The absolute path of the master file the daemon wrote.
    pub path: String,
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

/// One run-history row on the workspace timeline
/// ([`crate::methods::HANGAR_RUN_HISTORY`], P10 / D19): one finished provider run
/// with its provider / session / profile / outcome / duration and token-cost.
///
/// Carries an `f64` cost, so it is `PartialEq` only (no `Eq`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunHistoryRow {
    /// The run's id (fresh per run, never the task id).
    pub run_id: String,
    /// The task the run executed, or `None` for a task-less run.
    pub task_id: Option<String>,
    /// The provider session id the run used, or `None`.
    pub session_id: Option<String>,
    /// The provider that executed the run (`claude` / `codex`).
    pub provider: String,
    /// The agent profile slug the run launched under, or `None` until P5 wires it.
    pub profile: Option<String>,
    /// When the run started (epoch ms), or `None`.
    pub started_at: Option<i64>,
    /// When the run finished (epoch ms).
    pub finished_at: i64,
    /// Terminal FSM result: `success` | `failed`.
    pub outcome: String,
    /// Prompt/input tokens the run reported.
    pub input_tokens: i64,
    /// Completion/output tokens the run reported.
    pub output_tokens: i64,
    /// Total run cost in USD.
    pub cost_usd: f64,
    /// Lines added by the run's diff (0 until diff plumbing lands).
    pub diff_add: i64,
    /// Lines removed by the run's diff (0 until diff plumbing lands).
    pub diff_del: i64,
}

/// Params for [`crate::methods::HANGAR_RUN_HISTORY`] (P10 / D19): the workspace
/// to snapshot plus an optional row cap.
///
/// `limit` is the max number of newest-first rows to return; `None` lets the
/// daemon apply its default cap. A `WorkspaceScopedParams` extractor still reads
/// `workspace_id` off this shape (the extra `limit` field is ignored there).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunHistoryParams {
    /// The workspace whose run timeline to snapshot.
    pub workspace_id: String,
    /// Max newest-first rows to return, or `None` for the daemon default.
    #[serde(default)]
    pub limit: Option<i64>,
}

/// Result of [`crate::methods::HANGAR_RUN_HISTORY`] (P10 / D19): the workspace's
/// per-run observability timeline, newest finished first.
///
/// A workspace with no recorded runs answers an empty `runs` vec (the
/// empty-history state). Carries `f64` costs, so it is `PartialEq` only.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RunHistoryResult {
    /// The run rows, newest finished first (capped by the request limit).
    pub runs: Vec<RunHistoryRow>,
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

/// Params for [`crate::methods::HANGAR_TASK_RETRY`]: the workspace (tenant guard)
/// and the terminal task to force-requeue at an operator's explicit request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRetryParams {
    /// The subscribed workspace the task must belong to.
    pub workspace_id: String,
    /// The terminal task to requeue.
    pub task_id: String,
}

/// Result of [`crate::methods::HANGAR_TASK_RETRY`]: the id of the freshly-queued
/// child attempt, or `None` when the task was not terminal (nothing to requeue).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRetryResult {
    /// The `parent_task_id`-chained child that was enqueued, or `None` when the
    /// task was not in a terminal state.
    pub new_task_id: Option<String>,
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
    /// New issue title (F6 card edit); `None` leaves it unchanged. A blank/
    /// whitespace title is rejected by the daemon, mirroring `issue_create`.
    /// Append-only field: an old client omits it (title unchanged) and an old
    /// daemon ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// New card repo (F6 card edit): an absolute checkout path or the literal
    /// `scratch`. `None` leaves it. Paired with [`Self::agent`] — the card-edit
    /// overlay re-submits both from its prefill. Persisted on the issue via the
    /// card-parity accessor (mirrors `board_card_create`), so a later run/rerun
    /// provisions the right worktree. Append-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_ref: Option<String>,
    /// New card provider agent token (F6 card edit): `claude` / `codex` /
    /// `copilot`. `None` leaves it; an unrecognised token is dropped (the F4
    /// cascade then decides), never an error. Append-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// New SOURCE branch the run branches FROM (migration 0042); `None` leaves
    /// it. Resolved at dispatch (`main` default when never set). Append-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_branch: Option<String>,
    /// New TARGET branch a future PR lands INTO (migration 0042); `None` leaves
    /// it. Stored now, consumed by later PR automation. Append-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_branch: Option<String>,
    /// New upstream-issue reference (a URL or `owner/repo#123`, migration 0043);
    /// `None` leaves it unchanged. Persisted on the issue for traceability and
    /// appended to the dispatched brief. Append-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_ref: Option<String>,
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
    /// Optional upstream-issue reference (a URL or `owner/repo#123`) linking this
    /// hangar issue to a GitHub/Jira issue for traceability (migration 0043);
    /// omitted when unset. Persisted on the created issue and appended to the
    /// dispatched brief so the agent resolves the link itself (ainb never fetches
    /// it). Append-only field: an old client omits it, an old daemon ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_ref: Option<String>,
    /// Optional parent issue id: when set, the created issue is a **sub-issue** of
    /// that parent (migration 0046). The daemon validates the parent exists in the
    /// same workspace and rejects a foreign/unknown parent. Append-only field: an
    /// old client omits it, an old daemon ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_issue_id: Option<String>,
    /// Ordered acceptance-criteria strings authored in the create wizard/CLI
    /// (migration 0048, multica parity): one criterion per element. Persisted on
    /// the created issue and rendered on the detail card. Append-only field: an
    /// old client omits it, an old daemon ignores it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub acceptance_criteria: Vec<String>,
    /// Ordered context-reference strings (URL / `owner/repo#123` / note) authored
    /// in the create wizard/CLI (migration 0048, multica parity): one per element.
    /// Persisted on the created issue and rendered on the detail card. Append-only
    /// field: an old client omits it, an old daemon ignores it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context_refs: Vec<String>,
    /// New issue urgency `0..3` (P3..P0, HIGHER = MORE URGENT, migration 0014).
    /// `None` = the schema default 0 (P3). An out-of-range value is REJECTED by
    /// the daemon (multica's `validateIssueEnum` contract), never clamped.
    /// Append-only field: an old client omits it, an old daemon ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i64>,
    /// Optional deadline as epoch milliseconds at UTC midnight (migration 0014).
    /// Clients author a `YYYY-MM-DD` calendar day and convert with
    /// [`crate::dates::parse_calendar_date_ms`]; `None` = no due date.
    /// Append-only field: an old client omits it, an old daemon ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_date: Option<i64>,
    /// Label NAMES to attach at create (migration 0016): each is resolve-or-created
    /// in the workspace and joined to the new issue through the `label` /
    /// `issue_label` tables (the join is the source of truth; `issue.labels` stays
    /// the derived read-cache). Append-only field: an old client omits it, an old
    /// daemon ignores it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
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
    /// New token budget (rtk/headroom); omitted leaves it, `null` clears it
    /// (back to unlimited), a value sets it (migration 0042).
    #[serde(default, skip_serializing_if = "FieldUpdate::is_keep")]
    pub token_budget: FieldUpdate<i64>,
    /// New description, ≤255 CHARACTERS; `None` leaves it unchanged (the column
    /// is NOT NULL — its cleared state is `""`, so no [`FieldUpdate`]).
    /// Append-only field: an old client omits it, an old daemon ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// New avatar token; omitted leaves it, `null` clears it, a value sets it
    /// (migration 0050).
    #[serde(default, skip_serializing_if = "FieldUpdate::is_keep")]
    pub avatar_url: FieldUpdate<String>,
    /// New Codex service tier; omitted leaves it, `null` clears it (back to
    /// inheriting the local config), a value sets it (migration 0050).
    #[serde(default, skip_serializing_if = "FieldUpdate::is_keep")]
    pub service_tier: FieldUpdate<String>,
}

/// Params for [`crate::methods::HANGAR_AGENT_CREATE`]: create one agent from
/// scratch on the fresh-home path.
///
/// The daemon fills every FK behind the scenes (default workspace + owner +
/// runtime), so the caller supplies only the human `name`. `workspace_id` is
/// optional — absent/empty means "the default workspace" (the daemon ensures it),
/// so the TUI never has to surface an id. `provider` is optional
/// (`claude`/`codex`/`copilot`); absent defaults to the runtime's advertised
/// `claude`. `instructions` is the optional free-form system prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AgentCreateParams {
    /// The target workspace; absent/empty = the default workspace (ensured by
    /// the daemon). Kept optional so the TUI create prompt never surfaces an id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    /// The new agent's human name (required, non-empty).
    pub name: String,
    /// Optional provider (`claude`/`codex`/`copilot`); absent = the runtime's
    /// advertised default (`claude`). Recorded on the agent row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Optional free-form system prompt / instructions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    /// Optional per-agent model override (e.g. `sonnet`, `gpt-5-codex`); absent =
    /// the provider default. Applied as a create-time config follow-up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Optional token budget (rtk/headroom); absent = unlimited (migration 0042).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<i64>,
    /// Optional short blurb, ≤255 CHARACTERS (migration 0050); absent = `""`.
    /// The daemon rejects an over-long value with `INVALID_PARAMS`.
    /// Append-only field: an old client omits it, an old daemon ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Optional avatar token (e.g. `"emoji:🦊"`); absent/blank makes the daemon
    /// mint a random emoji so an agent is never avatar-less (migration 0050).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
    /// Optional Codex service tier (runtime-native catalog id, e.g. `"priority"`);
    /// absent = inherit the local Codex config (migration 0050). Stored +
    /// surfaced only — no dispatch-time override reads it yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    // NOTE: `kind` / `system_key` are DELIBERATELY absent from the create wire.
    // A `system` agent is a hidden internal carrier minted by the agent-builder
    // (gap #9-rest), never by a client, so exposing it here would let any peer
    // manufacture an agent no roster can see.
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
    /// The user id recorded as `archived_by` (migration 0052, parity #26).
    /// APPEND-ONLY: omitted (`None`) defaults to the workspace OWNER — the
    /// ordinary single-operator archive — mirroring
    /// [`SquadAssignParams::invoker_user_id`]. An old client omits it; an old
    /// daemon ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_by_user_id: Option<String>,
}

/// Params for [`crate::methods::HANGAR_AGENT_DELETE`]: delete one named agent
/// from a workspace (the Agents screen `x` remove, slice 2).
///
/// `workspace_id` is the tenant-isolation guard — the daemon resolves it and
/// scopes the delete by `(agent_id, workspace_id)` so a foreign-tenant agent id
/// deletes nothing. The daemon refuses the delete while the agent has an active
/// task or still carries FK-pinned run history (see
/// [`crate::methods::HANGAR_AGENT_DELETE`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AgentDeleteParams {
    /// The subscribed workspace the agent must belong to (tenant guard).
    pub workspace_id: String,
    /// The agent to delete (`agent.id`).
    pub agent_id: String,
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
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
    /// `true` when the squad is archived (migration 0052, parity #26).
    /// APPEND-ONLY: absent on the wire when `false`, so a pre-0052 producer's
    /// payload still parses and a consumer sees the active default.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub archived: bool,
    /// When the squad was archived (epoch ms), absent when active / unstamped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<i64>,
    /// Who archived the squad, as a canonical actor-ref; empty when unknown.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub archived_by: String,
}

/// Params for [`crate::methods::HANGAR_SQUAD_ARCHIVE`] (parity #26): archive or
/// un-archive one squad, recording who + when.
///
/// `workspace_id` is the tenant-isolation guard — the daemon resolves it and
/// scopes the flip by `(squad_id, workspace_id)` so a foreign-tenant squad id
/// archives nothing. `archived: true` removes the squad from the active list and
/// makes it refuse new assignments; `false` restores it and CLEARS the audit
/// pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SquadArchiveParams {
    /// The subscribed workspace the squad must belong to (tenant guard).
    pub workspace_id: String,
    /// The squad to (un)archive (`squad.id`).
    pub squad_id: String,
    /// `true` archives; `false` restores.
    pub archived: bool,
    /// The user id recorded as `archived_by`. APPEND-ONLY: omitted (`None`)
    /// defaults to the workspace OWNER, same resolution as
    /// [`AgentArchiveParams::archived_by_user_id`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_by_user_id: Option<String>,
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
    /// A run-time INVOKER override (gap #8): the user id the invocation-permission
    /// gate judges the assignment by. APPEND-ONLY: omitted (`None`) defaults to the
    /// workspace owner (the ordinary single-operator assign, which the gate always
    /// admits). A multi-user caller names a non-owner member here to be gated
    /// against the leader's / each member's allow-list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invoker_user_id: Option<String>,
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

/// One fanned-out member task in a [`SquadFanoutResult`] (P7): the enqueued row
/// plus the member agent / runtime it routed to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SquadMemberDispatchRow {
    /// The enqueued `agent_task_queue` row id.
    pub task_id: String,
    /// The member agent the task was routed to (`agent.id`).
    pub agent_id: String,
    /// The runtime the task was keyed to (the member agent's `runtime_id`).
    pub runtime_id: String,
}

/// Result of [`crate::methods::HANGAR_SQUAD_FANOUT`] (P7): the LEADER's brief task
/// plus one dispatch per distinct `agent` member, all on the same issue.
///
/// The `leader` is identical to a [`SquadAssignResult`]; `members` carries the
/// fanned-out member dispatches (the leader's agent and any human member
/// excluded), ordered by member agent id. The per-(issue, agent) claim guard
/// (migration `0012`) is what lets all of them coexist as pending tasks on the one
/// issue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SquadFanoutResult {
    /// The leader's brief task (row id + leader agent/runtime).
    pub leader: SquadAssignResult,
    /// One dispatch per fanned-out `agent` member, ordered by agent id.
    pub members: Vec<SquadMemberDispatchRow>,
}

// ---------------------------------------------------------------------------
// P4 — user-defined kanban boards (D8).
// ---------------------------------------------------------------------------

/// One card on a board: an issue placed in a column, enriched for the render
/// ([`crate::methods::HANGAR_BOARDS_LIST`], P4).
///
/// `issue_id` is the placed issue; `title` + `display_id` are folded in from the
/// `issue` row so the tile paints without a second lookup; `state` is the issue's
/// LATEST task status (`queued`/`running`/`done`/…) or `None` when no task has
/// run yet — the board turns a card green when `state == "done"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardCardWireRow {
    /// The placed issue's id (`issue.id`).
    pub issue_id: String,
    /// The issue title (the card's label).
    pub title: String,
    /// The short issue id rendered on the card header (`#<display_id>`).
    pub display_id: String,
    /// The issue's latest task status, or `None` when no task has run. `"done"`
    /// turns the card green.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// The exact tmux session name the issue's latest task spawned when launched
    /// in `interactive` mode (`tmux_hangar-<task_id>`, ccc / D6), or `None` for a
    /// headless task / no run. Append-only field: surfaced by the attach-from-card
    /// affordance as a copyable `tmux attach -t <session_name>` — the honest
    /// contract, since a plugin cannot drive a host terminal attach directly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    /// The card's persisted repo (an absolute checkout path or `scratch`), or
    /// `None` when never set (F2/F4). Append-only field the F6 card-edit overlay
    /// prefills its repo pick from so an edit re-submits the current value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_ref: Option<String>,
    /// The card's persisted provider agent token (`claude` / `codex` / `copilot`),
    /// or `None` when unset (the run resolves via the F4 cascade). Append-only
    /// field the F6 card-edit overlay prefills its agent chip from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// The card's assigned SQUAD (`squad.id`), or `None` for a single-agent card
    /// (tcp T4 / F7). APPEND-ONLY: a set squad makes a run fan out across the whole
    /// squad; the board renders one member chip per fanned-out task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub squad_id: Option<String>,
    /// One chip per squad member's task on this card (tcp T4 / F7). Empty for a
    /// single-agent card, or a squad card that has not run yet. APPEND-ONLY: lets
    /// the board render N per-member states on one card.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub member_states: Vec<CardMemberChip>,
    /// The DISPLAY IDS of this card's UNFINISHED blocker cards (tcp T4 / F7).
    /// Non-empty ⇒ the card is BLOCKED (renders 🔒 + these refs) and refuses to
    /// run; empty ⇒ runnable. APPEND-ONLY.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_by: Vec<String>,
    /// Whether this card auto-launches when its last blocker completes (tcp T4 /
    /// F7). APPEND-ONLY: default `false` (explicit run stays the default).
    #[serde(default, skip_serializing_if = "is_false")]
    pub auto_run: bool,
}

/// One squad member's task chip on a fanned-out card (tcp T4 / F7): which agent
/// and that member task's latest status. The board paints one per squad member so
/// a squad card shows N states at once, not a single collapsed one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CardMemberChip {
    /// The member agent (`agent.id`) this chip's task routed to.
    pub agent_id: String,
    /// The member agent's display name (falls back to the id when unnamed).
    pub agent_name: String,
    /// The member task's latest status (`queued`/`running`/`done`/…), or `None`
    /// when that member has no task yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
}

/// serde `skip_serializing_if` helper: omit a `false` bool so a pre-T4 card row is
/// byte-identical on the wire (the field only appears when auto-run is on).
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(b: &bool) -> bool {
    !*b
}

/// One user-defined board column with the cards bucketed into it
/// ([`crate::methods::HANGAR_BOARDS_LIST`], P4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardColumnWireRow {
    /// The column's stable surrogate id (what a reorder / card-move key off).
    pub id: String,
    /// The column's display name.
    pub name: String,
    /// The column's left-to-right position (contiguous `0..n`).
    pub ord: i64,
    /// The task-status this column maps to (the auto-move target key), or `None`
    /// for a purely manual column.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fsm_state: Option<String>,
    /// Whether a task reaching this column's `fsm_state` auto-moves the card here.
    pub auto_move: bool,
    /// The cards currently in this column (in board order).
    pub cards: Vec<BoardCardWireRow>,
}

/// One board with its ordered columns and its cards
/// ([`crate::methods::HANGAR_BOARDS_LIST`], P4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardWireRow {
    /// The board's id.
    pub id: String,
    /// The board's name (unique within its workspace).
    pub name: String,
    /// The per-board auto-move master toggle.
    pub auto_move: bool,
    /// The board's columns, left-to-right.
    pub columns: Vec<BoardColumnWireRow>,
    /// Cards whose column was deleted — parked unmapped (no data loss). The board
    /// renders these in a fallback pool so they never disappear.
    pub unmapped: Vec<BoardCardWireRow>,
}

/// Result of [`crate::methods::HANGAR_BOARDS_LIST`] and the refreshed view every
/// `board_*` mutation answers with (P4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardsListResult {
    /// The workspace's boards, ordered by name.
    pub boards: Vec<BoardWireRow>,
}

/// Params for [`crate::methods::HANGAR_BOARD_CREATE`] (P4): create one empty board.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct BoardCreateParams {
    /// The subscribed workspace the board belongs to (tenant guard).
    pub workspace_id: String,
    /// The board name (unique within the workspace).
    pub name: String,
}

/// Params for [`crate::methods::HANGAR_BOARD_UPDATE`] (P4): rename a board and/or
/// flip its auto-move master toggle. Omitted fields are left unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct BoardUpdateParams {
    /// The subscribed workspace the board belongs to (tenant guard).
    pub workspace_id: String,
    /// The board to update.
    pub board_id: String,
    /// The new name, or `None` to leave it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The new auto-move master toggle, or `None` to leave it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_move: Option<bool>,
}

/// Params for [`crate::methods::HANGAR_BOARD_DELETE`] (P4): a board id in a
/// workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct BoardIdParams {
    /// The subscribed workspace the board belongs to (tenant guard).
    pub workspace_id: String,
    /// The board to delete.
    pub board_id: String,
}

/// Params for [`crate::methods::HANGAR_BOARD_COLUMN_ADD`] (P4): append a column.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct BoardColumnAddParams {
    /// The subscribed workspace the board belongs to (tenant guard).
    pub workspace_id: String,
    /// The board to append the column to.
    pub board_id: String,
    /// The new column's display name.
    pub name: String,
    /// The task-status the column maps to (the auto-move target key), or `None`
    /// for a purely manual column.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fsm_state: Option<String>,
    /// Whether the column auto-moves; omitted defaults to `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_move: Option<bool>,
}

/// Params for [`crate::methods::HANGAR_BOARD_COLUMN_UPDATE`] (P4).
///
/// An OMITTED `fsm_state` leaves the mapping unchanged; an EMPTY-STRING
/// `fsm_state` clears it (a manual column). `name` / `auto_move` omitted are
/// unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct BoardColumnUpdateParams {
    /// The subscribed workspace the board belongs to (tenant guard).
    pub workspace_id: String,
    /// The board the column belongs to.
    pub board_id: String,
    /// The column to update.
    pub column_id: String,
    /// The new name, or `None` to leave it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// `None` leaves the mapping; `Some("")` clears it; `Some(s)` sets it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fsm_state: Option<String>,
    /// The new auto-move flag, or `None` to leave it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_move: Option<bool>,
}

/// Params for [`crate::methods::HANGAR_BOARD_COLUMN_DELETE`] (P4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct BoardColumnDeleteParams {
    /// The subscribed workspace the board belongs to (tenant guard).
    pub workspace_id: String,
    /// The board the column belongs to.
    pub board_id: String,
    /// The column to delete (its cards park unmapped).
    pub column_id: String,
}

/// Params for [`crate::methods::HANGAR_BOARD_COLUMN_REORDER`] (P4): the board's
/// columns in their new order. `column_ids` must be exactly the current columns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct BoardColumnReorderParams {
    /// The subscribed workspace the board belongs to (tenant guard).
    pub workspace_id: String,
    /// The board whose columns to reorder.
    pub board_id: String,
    /// The column ids in their new left-to-right order (a permutation of the
    /// board's current columns).
    pub column_ids: Vec<String>,
}

/// Params for [`crate::methods::HANGAR_BOARD_CARD_ADD`] and
/// [`crate::methods::HANGAR_BOARD_CARD_MOVE`] (P4): an issue placement on a board.
///
/// Omit `column_id` to place / park the card unmapped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct BoardCardParams {
    /// The subscribed workspace the board belongs to (tenant guard).
    pub workspace_id: String,
    /// The board to place / move the card on.
    pub board_id: String,
    /// The issue the card represents.
    pub issue_id: String,
    /// The column to place / move the card into, or `None` for unmapped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column_id: Option<String>,
}

/// Params for [`crate::methods::HANGAR_BOARD_CARD_CREATE`] (ccc / D8, D16):
/// create an issue from a card and place it on a board.
///
/// Omit `column_id` to place the card unmapped; omit `assignee_profile` to leave
/// the issue unassigned (the run then falls back to the workspace's agent).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct BoardCardCreateParams {
    /// The subscribed workspace the board belongs to (tenant guard).
    pub workspace_id: String,
    /// The board to place the new card on.
    pub board_id: String,
    /// The column to place the card in, or `None` for unmapped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column_id: Option<String>,
    /// The new issue's title (the card label).
    pub title: String,
    /// The assignee profile slug (D16: the agent named for it runs the card), or
    /// `None` to leave the issue unassigned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee_profile: Option<String>,
    /// The card's repo (spec F2/F3): an absolute checkout path, or the literal
    /// `scratch`. APPEND-ONLY: omitted by a pre-parity caller (`None`), leaving
    /// the card without a repo. The daemon persists it on the issue so a run /
    /// rerun provisions the right worktree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_ref: Option<String>,
    /// The card's chosen provider agent (spec F1/F4): `claude` / `codex` /
    /// `copilot`. APPEND-ONLY: omitted (`None`) leaves the run to resolve the
    /// agent via the F4 cascade. An unrecognised token is ignored (cascade
    /// decides), never a hard reject — the wire stays forward-compatible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// The SOURCE branch a run branches FROM (migration 0042). APPEND-ONLY:
    /// omitted (`None`) resolves to `main` at dispatch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_branch: Option<String>,
    /// The TARGET branch a future PR lands INTO (migration 0042). APPEND-ONLY:
    /// stored now, consumed by later PR automation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_branch: Option<String>,
}

/// Params for [`crate::methods::HANGAR_ISSUE_RUN`]: enqueue a run of one issue
/// WITHOUT a board (the Issues create-wizard dispatch).
///
/// Same override semantics as [`BoardCardRunParams`], minus the board identity.
/// Result shape: [`BoardCardRunResult`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct IssueRunParams {
    /// The subscribed workspace the issue belongs to (tenant guard).
    pub workspace_id: String,
    /// The issue to launch.
    pub issue_id: String,
    /// The launch mode (`headless` or `interactive`).
    pub mode: String,
    /// A run-time REPO override; omitted uses the issue's persisted `repo_ref`
    /// (repo REQUIRED at run, like `board_card_run`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_ref: Option<String>,
    /// A run-time AGENT override (`claude`/`codex`/`copilot`); omitted resolves
    /// via the F4 cascade (board tier skipped — no board).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// A run-time SOURCE-BRANCH override (0042); omitted uses the issue's
    /// persisted `source_branch`, else the repo's default branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_branch: Option<String>,
    /// A run-time ASSIGNEE override (`agent:<id>`) naming the NAMED workspace
    /// agent the run dispatches under (V3-F3). APPEND-ONLY: omitted (`None`)
    /// resolves the run agent from the issue's persisted `assignee`, else the
    /// workspace's alphabetically-first agent. Passed ALONGSIDE the persisting
    /// `issue_update{assignee}` (mirroring the repo/branch override discipline) so
    /// dispatch never depends on the persist landing first — the create wizard
    /// targets a named agent by carrying it here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    /// A run-time INVOKER override (gap #8): the user id the invocation-permission
    /// gate judges the run by. APPEND-ONLY: omitted (`None`) defaults to the
    /// workspace owner (the ordinary single-operator Run, which the gate always
    /// admits — so this is invisible to existing callers). A multi-user caller
    /// names a non-owner member here to be gated against the agent's allow-list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invoker_user_id: Option<String>,
}

/// Params for [`crate::methods::HANGAR_ISSUE_DELETE`] (63d): delete one issue and
/// all its history from a workspace.
///
/// Workspace-scoped like every mutation: the daemon rejects a mistyped
/// `workspace_id` with `INVALID_PARAMS`, and an `issue_id` owned by another tenant
/// resolves to no row (also rejected, never a cross-tenant delete).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct IssueDeleteParams {
    /// The subscribed workspace the issue belongs to (tenant guard).
    pub workspace_id: String,
    /// The issue to delete.
    pub issue_id: String,
}

/// Params for [`crate::methods::HANGAR_ISSUE_CANCEL_ACTIVE`]: cancel every active
/// task on one issue (the board-less "cancel the run(s) & delete" affordance).
///
/// Workspace-scoped like every mutation: the daemon rejects a mistyped
/// `workspace_id` with `INVALID_PARAMS`. The issue carries no board coordinates —
/// the daemon resolves the active set from the issue id alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct IssueCancelActiveParams {
    /// The subscribed workspace the issue belongs to (tenant guard).
    pub workspace_id: String,
    /// The issue whose active run(s) to cancel.
    pub issue_id: String,
}

/// Result of [`crate::methods::HANGAR_ISSUE_CANCEL_ACTIVE`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct IssueCancelActiveResult {
    /// How many of the issue's active tasks this call transitioned to
    /// `cancelled`. `0` when the issue had no active task (a clean no-op the
    /// caller surfaces as a note, never an error).
    pub cancelled: u64,
}

/// Params for [`crate::methods::HANGAR_BOARD_CARD_RUN`] (ccc / D6, D16): launch a
/// card's issue on its assignee profile now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct BoardCardRunParams {
    /// The subscribed workspace the board belongs to (tenant guard).
    pub workspace_id: String,
    /// The board the card sits on.
    pub board_id: String,
    /// The card's issue to launch.
    pub issue_id: String,
    /// The launch mode (`headless` or `interactive`, D6 `Run ▾`). Both dispatch
    /// through the one provider-runner path today; the value is carried for the D6
    /// launch surface and echoed in the result.
    pub mode: String,
    /// A run-time REPO override (spec F5): an absolute path or `scratch`.
    /// APPEND-ONLY: omitted (`None`) uses the card's persisted `repo_ref`. Lets a
    /// run pin a repo the card was created without (Repo REQUIRED at run).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_ref: Option<String>,
    /// A run-time AGENT override (spec F4): `claude` / `codex` / `copilot`.
    /// APPEND-ONLY: omitted (`None`) uses the card's persisted agent, else the F4
    /// cascade. An unrecognised token is ignored (cascade decides).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// A run-time SOURCE-BRANCH override (migration 0042). APPEND-ONLY: omitted
    /// (`None`) uses the issue's persisted `source_branch`, else `main`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_branch: Option<String>,
    /// A run-time INVOKER override (gap #8): the user id the invocation-permission
    /// gate judges the run by — for the single-agent enqueue AND for every target of
    /// a SQUAD fan-out. APPEND-ONLY: omitted (`None`) defaults to the workspace owner
    /// (the ordinary single-operator TUI Run, which the gate always admits — so this
    /// is invisible to existing callers). Mirrors
    /// [`IssueRunParams::invoker_user_id`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invoker_user_id: Option<String>,
}

/// Result of [`crate::methods::HANGAR_BOARD_CARD_RUN`] (ccc / D6): the enqueued
/// run's identity so the caller can report who the card landed on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct BoardCardRunResult {
    /// The enqueued `agent_task_queue` row id.
    pub task_id: String,
    /// The agent (`agent.id`) the run routed to.
    pub agent_id: String,
    /// The runtime (`agent_runtime.id`) the task was keyed to.
    pub runtime_id: String,
    /// The echoed launch mode (`headless` / `interactive`).
    pub mode: String,
    /// For a SQUAD card (tcp T4 / F7): the fanned-out MEMBER task ids (`task_id`
    /// above is the LEADER brief). Empty for a single-agent card. APPEND-ONLY, so a
    /// pre-T4 single-agent run serializes byte-identically.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub member_task_ids: Vec<String>,
}

/// Params for [`crate::methods::HANGAR_BOARD_CARD_CANCEL`] (tcp T3 / F6): cancel
/// a card's in-flight run.
///
/// Card = issue: the daemon resolves the issue's single active (queued /
/// dispatched / running) task and cancels it, so the caller only carries the
/// card's coordinates — never a task id it does not hold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct BoardCardCancelParams {
    /// The subscribed workspace the board belongs to (tenant guard).
    pub workspace_id: String,
    /// The board the card sits on.
    pub board_id: String,
    /// The card's issue whose active run to cancel.
    pub issue_id: String,
}

/// Result of [`crate::methods::HANGAR_BOARD_CARD_CANCEL`] (tcp T3 / F6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct BoardCardCancelResult {
    /// The task that was cancelled, or `None` when the card had no active task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// Whether an active task was found and cancelled. `false` for a card whose
    /// latest task is already terminal (`done` / `failed` / `cancelled`) or that
    /// never ran — the cancel is a no-op the caller surfaces as a note.
    pub cancelled: bool,
}

/// Params for [`crate::methods::HANGAR_BOARD_CARD_REORDER`] (tcp T3 / F6): the new
/// order of one column's cards.
///
/// `issue_ids` must be exactly the cards currently in `column_id` (a permutation);
/// omit `column_id` to reorder the unmapped pool. A pure `board_card.ord` rewrite
/// within the column — no card changes column.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct BoardCardReorderParams {
    /// The subscribed workspace the board belongs to (tenant guard).
    pub workspace_id: String,
    /// The board the cards sit on.
    pub board_id: String,
    /// The column whose cards to reorder, or `None` for the unmapped pool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column_id: Option<String>,
    /// The card issue ids in their new top-to-bottom order (a permutation of the
    /// column's current cards).
    pub issue_ids: Vec<String>,
}

/// Result of [`crate::methods::HANGAR_BOARD_CARD_TIMELINE`] (tcp T3 / F6): the
/// card's latest run transcript, raw, for the plugin to parse + render.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct BoardCardTimelineResult {
    /// The task whose transcript this is, or `None` when the card never ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// The provider whose log was read (`claude` / `codex`), or `None` when no log
    /// exists yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// The RAW provider stream-json (a bounded tail of `claude.jsonl` /
    /// `codex.jsonl`). Empty when the card never ran or the log is gone — the
    /// plugin renders "no transcript yet", never an error.
    pub jsonl: String,
}

/// Params for [`crate::methods::HANGAR_BOARD_CARD_ASSIGN_SQUAD`] (tcp T4 / F7):
/// assign (or clear) a squad as a card's assignee. Omit / null `squad_id` to
/// clear (revert the card to a single-agent run).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct BoardCardAssignSquadParams {
    /// The subscribed workspace the board belongs to (tenant guard).
    pub workspace_id: String,
    /// The board the card sits on.
    pub board_id: String,
    /// The card's issue to assign the squad to.
    pub issue_id: String,
    /// The squad to assign (`squad.id`), or `None` to clear the assignment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub squad_id: Option<String>,
}

/// Params for [`crate::methods::HANGAR_BOARD_CARD_DEP_ADD`] and
/// [`crate::methods::HANGAR_BOARD_CARD_DEP_REMOVE`] (tcp T4 / F7): a `depends-on`
/// edge between two cards on the board. The DEPENDENT is blocked until the BLOCKER
/// finishes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct BoardCardDepParams {
    /// The subscribed workspace the board belongs to (tenant guard).
    pub workspace_id: String,
    /// The board both cards sit on.
    pub board_id: String,
    /// The DEPENDENT card's issue (the one that gets blocked).
    pub dependent_issue_id: String,
    /// The BLOCKER card's issue (must finish before the dependent runs).
    pub blocker_issue_id: String,
}

/// Params for [`crate::methods::HANGAR_BOARD_CARD_SET_AUTO_RUN`] (tcp T4 / F7):
/// flip a card's auto-run flag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct BoardCardAutoRunParams {
    /// The subscribed workspace the board belongs to (tenant guard).
    pub workspace_id: String,
    /// The board the card sits on.
    pub board_id: String,
    /// The card's issue whose auto-run flag to flip.
    pub issue_id: String,
    /// The new auto-run state (`true` = auto-launch when the last blocker
    /// completes; `false` = explicit run only, the default).
    pub auto_run: bool,
}

/// One pickable repository in the card-create `@` roster
/// ([`crate::methods::HANGAR_REPO_LIST`], spec F3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RepoWireRow {
    /// The display name: a favorite's alias, or a scanned repo's name.
    pub name: String,
    /// The local checkout path, when known (scanned repos always carry one; a
    /// favorite migrated to a remote indicator carries `None`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The remote indicator (`owner/repo` shorthand or a URL) for a favorite, or
    /// `None` for a scan-only entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
    /// Whether this is a ★ favorite (pinned first, ahead of scanned repos).
    pub is_favorite: bool,
    /// Recency (epoch ms) from a favorite's `stats.last_used`, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_ms: Option<i64>,
}

/// Result of [`crate::methods::HANGAR_REPO_LIST`] (spec F3): the card-create repo
/// roster, favorites-first + scanned-second, ready for plugin-side fuzzy filter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RepoListResult {
    /// The roster rows in pick order (★ favorites by recency, then scanned).
    pub repos: Vec<RepoWireRow>,
}

/// One routing-rule row in the [`crate::methods::HANGAR_NOTIFY_RULES_LIST`]
/// result (tcp T5): an attention kind, its EFFECTIVE push-channel set for the
/// scope, and whether that value is a per-workspace override (vs the inherited
/// global default). The settings grid renders one of these per kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyRuleWireRow {
    /// The attention kind wire token (`ask_user_question` / `approval` / … /
    /// `escalation`) this rule governs.
    pub kind: String,
    /// The effective push channels for the scope (empty = board-only).
    pub channels: ChannelSet,
    /// `true` when a per-workspace override supplied `channels`; `false` when
    /// inherited from the global default. Always `false` for the global scope.
    #[serde(default)]
    pub overridden: bool,
}

/// Params for [`crate::methods::HANGAR_NOTIFY_RULES_LIST`] (tcp T5): the scope to
/// read the routing grid for. `workspace_id = None` reads the GLOBAL rows; a
/// `Some(ws)` reads that workspace's effective rows (override where set, global
/// otherwise).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct NotifyRulesListParams {
    /// The workspace to scope to, or `None` for the global rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
}

/// Result of [`crate::methods::HANGAR_NOTIFY_RULES_LIST`] (tcp T5): one row per
/// attention kind, in declaration order (the settings-grid row order).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct NotifyRulesListResult {
    /// The routing rules, one per kind.
    pub rules: Vec<NotifyRuleWireRow>,
    /// The scope this reply answers — echoes the request's `workspace_id`
    /// (`None` = the global rows, `Some(ws)` = that workspace's effective rows).
    /// A settings grid that flips its edit scope (`g`, agents-in-a-box-cqh) while
    /// a list request is in flight uses this echo to DROP a reply for the scope it
    /// just left, so a stale reply can't briefly repopulate the grid with the
    /// wrong scope's rows. Append-only + `default` so an older daemon that omits
    /// it round-trips as `None` (the global scope, the pre-echo behaviour).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
}

/// Params for [`crate::methods::HANGAR_NOTIFY_RULE_SET`] (tcp T5): set one rule.
/// `workspace_id = None` writes the GLOBAL row; `Some(ws)` writes a per-workspace
/// override. The `kind` is the wire token; `channels` is the new push-channel set
/// (empty = board-only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyRuleSetParams {
    /// The workspace to scope to, or `None` for the global rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    /// The attention kind wire token to set the rule for.
    pub kind: String,
    /// The new push-channel set (empty = board-only).
    pub channels: ChannelSet,
}

/// Result of [`crate::methods::HANGAR_NOTIFY_RULE_SET`] (tcp T5): the scope + kind
/// that was written and its stored channel set (echoed for the caller to fold
/// back into its grid without a re-list).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyRuleSetResult {
    /// The kind wire token that was written.
    pub kind: String,
    /// The stored channel set after the write.
    pub channels: ChannelSet,
}

/// Params for [`crate::methods::HANGAR_DAEMON_CONFIG_GET`] (D13): the
/// `daemon_config` key to read (e.g. `autostandup.enabled`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonConfigGetParams {
    /// The `daemon_config` key to read.
    pub key: String,
}

/// Result of [`crate::methods::HANGAR_DAEMON_CONFIG_GET`] (D13): the key and its
/// stored value, or `None` when the key has no row (the caller applies its coded
/// default).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonConfigGetResult {
    /// The key that was read (echoed).
    pub key: String,
    /// The stored value, or `None` when no row exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

/// Params for [`crate::methods::HANGAR_DAEMON_CONFIG_SET`] (D13): the
/// `daemon_config` key + the value to persist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonConfigSetParams {
    /// The `daemon_config` key to write.
    pub key: String,
    /// The value to persist under `key`.
    pub value: String,
}

/// Result of [`crate::methods::HANGAR_DAEMON_CONFIG_SET`] (D13): the key + stored
/// value echoed back so the caller can fold it in without a re-read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonConfigSetResult {
    /// The key that was written (echoed).
    pub key: String,
    /// The stored value after the write.
    pub value: String,
}

/// One entry in a [`DaemonConfigListResult`]: a registry key and its currently
/// stored value (`None` when the key has no row — the caller applies the coded
/// default from the descriptor).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonConfigEntry {
    /// The `daemon_config` registry key.
    pub key: String,
    /// The stored value, or `None` when no row exists (use the coded default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

/// Result of [`crate::methods::HANGAR_DAEMON_CONFIG_LIST`]: every user-config
/// knob's current stored value, one entry per registry descriptor in registry
/// order. Lets a surface read the whole configurable set in one round trip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonConfigListResult {
    /// One entry per [`ainb_hangar_core::daemon_config::DAEMON_CONFIG_REGISTRY`]
    /// descriptor, in registry order.
    pub entries: Vec<DaemonConfigEntry>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{PresenceState, Workload};

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
                branch: None,
                repo_ref: None,
                agent: None,
                source_branch: None,
                target_branch: None,
                external_ref: None,
                run_count: 0,
                last_run_status: None,
                last_run_at: None,
                parent_id: None,
                child_total: 0,
                child_done: 0,
                acceptance_criteria: Vec::new(),
                context_refs: Vec::new(),
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
                workload: Workload::Working,
                is_agent: true,
                recent_rank: Some(0),
                ..Default::default()
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

    /// The parity-#24 per-agent skill-toggle envelopes round-trip through JSON,
    /// and an old peer's row (no `enabled` key) reads back as ENABLED.
    #[test]
    fn skill_toggle_envelopes_roundtrip() {
        let toggle = SkillSetEnabledParams {
            workspace_id: "ws-1".into(),
            agent_id: "agent-1".into(),
            skill_id: "skill-review".into(),
            enabled: false,
        };
        let s = serde_json::to_string(&toggle).unwrap();
        assert_eq!(
            serde_json::from_str::<SkillSetEnabledParams>(&s).unwrap(),
            toggle
        );

        let params = AgentSkillsListParams {
            workspace_id: "ws-1".into(),
            agent_id: "agent-1".into(),
        };
        let s = serde_json::to_string(&params).unwrap();
        assert_eq!(
            serde_json::from_str::<AgentSkillsListParams>(&s).unwrap(),
            params
        );

        let result = AgentSkillsListResult {
            links: vec![
                AgentSkillLinkRow {
                    skill_id: "s-1".into(),
                    name: "commit".into(),
                    enabled: true,
                },
                AgentSkillLinkRow {
                    skill_id: "s-2".into(),
                    name: "review".into(),
                    enabled: false,
                },
            ],
        };
        let s = serde_json::to_string(&result).unwrap();
        assert_eq!(
            serde_json::from_str::<AgentSkillsListResult>(&s).unwrap(),
            result
        );

        // Append-only tolerance: a peer that predates the toggle concept omits
        // the field entirely. It MUST read back as enabled — defaulting to
        // `false` would render every skill on an old peer as disabled.
        let legacy: AgentSkillLinkRow =
            serde_json::from_str(r#"{"skill_id":"s","name":"n"}"#).unwrap();
        assert!(
            legacy.enabled,
            "an omitted `enabled` means the peer has no toggle concept = ENABLED"
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
                branch: Some("ainb/task-1".into()),
                pr_url: Some("https://github.com/o/r/pull/1".into()),
                pr_status: Some(crate::pr_status::PrStatus::default()),
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
        // A full edit: change state + reassign + bump priority + set a due date,
        // plus the F6 card-edit fields (title + repo + agent).
        let full = IssueUpdateParams {
            workspace_id: "ws-1".into(),
            issue_id: "issue-1".into(),
            state: Some("in_progress".into()),
            assignee: FieldUpdate::Set("agent:a1".into()),
            priority: Some(3),
            due_date: FieldUpdate::Set(1_700_000_000_000),
            title: Some("Renamed card".into()),
            repo_ref: Some("/repos/app".into()),
            agent: Some("codex".into()),
            source_branch: Some("develop".into()),
            target_branch: Some("main".into()),
            external_ref: Some("acme/api#42".into()),
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
        // The F6 card-edit fields are append-only: an old client that omits them
        // leaves the title / repo / agent unchanged (all decode to `None`).
        assert_eq!(p.title, None, "absent title leaves it unchanged");
        assert_eq!(p.repo_ref, None, "absent repo leaves it unchanged");
        assert_eq!(p.agent, None, "absent agent leaves it unchanged");
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
            token_budget: FieldUpdate::Set(500_000),
            description: Some("ships the backend".into()),
            avatar_url: FieldUpdate::Set("emoji:\u{1F98A}".into()),
            service_tier: FieldUpdate::Set("priority".into()),
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
        // Migration-0050 metadata: a PRE-0050 producer sends none of these keys,
        // and each must decode to leave-unchanged (the append-only proof).
        assert_eq!(p.description, None, "absent description leaves it");
        assert!(p.avatar_url.is_keep(), "absent avatar_url leaves it");
        assert!(p.service_tier.is_keep(), "absent service_tier leaves it");
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
            archived_by_user_id: Some("user-2".into()),
        };
        let s = serde_json::to_string(&params).unwrap();
        assert_eq!(
            serde_json::from_str::<AgentArchiveParams>(&s).unwrap(),
            params
        );
    }

    /// The parity-#26 archive-audit fields are APPEND-ONLY on the wire: a LEGACY
    /// payload without them still parses (defaulting to "unattributed"), and a
    /// value-less row does not emit the keys — so an old peer's shape is
    /// unchanged in both directions.
    #[test]
    fn archive_audit_fields_are_append_only_on_the_wire() {
        // A pre-0052 client payload (no `archived_by_user_id`) still parses.
        let legacy = r#"{"workspace_id":"ws-1","agent_id":"agent-1","archived":true}"#;
        let p: AgentArchiveParams = serde_json::from_str(legacy).unwrap();
        assert_eq!(
            p.archived_by_user_id, None,
            "an omitted archiver defaults to None (the daemon then falls back to the owner)"
        );
        assert!(
            !serde_json::to_string(&p).unwrap().contains("archived_by_user_id"),
            "an unset archiver is not emitted"
        );

        // The squad params carry the same append-only field.
        let legacy = r#"{"workspace_id":"ws-1","squad_id":"s1","archived":true}"#;
        let p: SquadArchiveParams = serde_json::from_str(legacy).unwrap();
        assert_eq!(p.archived_by_user_id, None);
        assert!(p.archived);

        // A round-trip with the field present preserves it.
        let full = SquadArchiveParams {
            workspace_id: "ws-1".into(),
            squad_id: "s1".into(),
            archived: true,
            archived_by_user_id: Some("user-2".into()),
        };
        let s = serde_json::to_string(&full).unwrap();
        assert_eq!(serde_json::from_str::<SquadArchiveParams>(&s).unwrap(), full);

        // An ACTIVE squad row emits none of the three audit keys, so a pre-0052
        // consumer sees byte-identical JSON to what it saw before.
        let active = SquadWireRow {
            id: "s1".into(),
            name: "alpha".into(),
            leader: "agent:a-lead".into(),
            ..SquadWireRow::default()
        };
        let s = serde_json::to_string(&active).unwrap();
        for key in ["archived", "archived_at", "archived_by"] {
            assert!(!s.contains(key), "active row must not emit `{key}`: {s}");
        }

        // An ARCHIVED row carries all three, and a legacy payload without them
        // parses back to the active default.
        let archived = SquadWireRow {
            archived: true,
            archived_at: Some(1_700_000_000_000),
            archived_by: "member:user-1".into(),
            ..active.clone()
        };
        let s = serde_json::to_string(&archived).unwrap();
        assert_eq!(serde_json::from_str::<SquadWireRow>(&s).unwrap(), archived);
        let legacy_row = r#"{"id":"s1","name":"alpha","leader":"agent:a-lead","members":[]}"#;
        assert_eq!(
            serde_json::from_str::<SquadWireRow>(legacy_row).unwrap(),
            active
        );
    }

    /// `AgentCreateParams` round-trips, and the optional fields are omitted when
    /// absent so a minimal `{ "name": ... }` payload deserializes (the fresh-home
    /// create needs neither an id nor a provider).
    #[test]
    fn agent_create_params_roundtrip_and_minimal() {
        let full = AgentCreateParams {
            workspace_id: Some("ws-1".into()),
            name: "reviewer".into(),
            provider: Some("codex".into()),
            instructions: Some("be terse".into()),
            model: Some("gpt-5-codex".into()),
            token_budget: Some(250_000),
            description: Some("reviews every PR".into()),
            avatar_url: Some("emoji:\u{1F98A}".into()),
            service_tier: Some("priority".into()),
        };
        let s = serde_json::to_string(&full).unwrap();
        assert_eq!(serde_json::from_str::<AgentCreateParams>(&s).unwrap(), full);

        // A name-only payload deserializes with every optional field defaulted.
        let minimal: AgentCreateParams = serde_json::from_str(r#"{"name":"claude"}"#).unwrap();
        assert_eq!(minimal.name, "claude");
        assert!(minimal.workspace_id.is_none());
        assert!(minimal.provider.is_none());
        assert!(minimal.instructions.is_none());
        assert!(minimal.model.is_none());
        // Migration-0050 metadata: a PRE-0050 payload carries none of these keys
        // and still deserializes, each defaulting to "unset" (append-only proof).
        assert!(minimal.description.is_none());
        assert!(minimal.avatar_url.is_none());
        assert!(minimal.service_tier.is_none());
        // The optional fields are omitted from the serialized form when absent.
        assert_eq!(
            serde_json::to_string(&minimal).unwrap(),
            r#"{"name":"claude"}"#
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
                ..SquadWireRow::default()
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
            invoker_user_id: Some("user-1".into()),
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

        // P7 — the fan-out envelope round-trips leader + members.
        let fanout = SquadFanoutResult {
            leader: SquadAssignResult {
                task_id: "task-lead".into(),
                leader_agent_id: "a-lead".into(),
                runtime_id: "rt-lead".into(),
            },
            members: vec![
                SquadMemberDispatchRow {
                    task_id: "task-m1".into(),
                    agent_id: "a-m1".into(),
                    runtime_id: "rt-m1".into(),
                },
                SquadMemberDispatchRow {
                    task_id: "task-m2".into(),
                    agent_id: "a-m2".into(),
                    runtime_id: "rt-m2".into(),
                },
            ],
        };
        let s = serde_json::to_string(&fanout).unwrap();
        assert_eq!(
            serde_json::from_str::<SquadFanoutResult>(&s).unwrap(),
            fanout
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
        // gap #8: the invoker override is append-only — a pre-gap-#8 caller omits
        // it and the service falls back to the workspace owner.
        assert_eq!(minimal.invoker_user_id, None);
        // The serialized form drops the absent optionals entirely.
        let s = serde_json::to_string(&minimal).unwrap();
        assert_eq!(s, r#"{"workspace_id":"ws-1","squad_id":"s1"}"#);
    }

    /// The P4 board result round-trips, and a column-update's `fsm_state`
    /// distinguishes OMITTED (unchanged) from EMPTY-STRING (clear).
    #[test]
    fn board_envelopes_roundtrip_and_fsm_state_tri_state() {
        let result = BoardsListResult {
            boards: vec![BoardWireRow {
                id: "b1".into(),
                name: "Sprint".into(),
                auto_move: true,
                columns: vec![BoardColumnWireRow {
                    id: "c1".into(),
                    name: "Done".into(),
                    ord: 0,
                    fsm_state: Some("done".into()),
                    auto_move: true,
                    cards: vec![BoardCardWireRow {
                        issue_id: "issue-1".into(),
                        title: "Ship it".into(),
                        display_id: "sue-1".into(),
                        state: Some("done".into()),
                        session_name: None,
                        repo_ref: Some("/repos/app".into()),
                        agent: Some("codex".into()),
                        squad_id: None,
                        member_states: Vec::new(),
                        blocked_by: Vec::new(),
                        auto_run: false,
                    }],
                }],
                unmapped: Vec::new(),
            }],
        };
        let s = serde_json::to_string(&result).unwrap();
        assert_eq!(
            serde_json::from_str::<BoardsListResult>(&s).unwrap(),
            result
        );

        // Omitted fsm_state => None (leave the mapping unchanged).
        let omitted: BoardColumnUpdateParams = serde_json::from_str(
            r#"{"workspace_id":"ws-1","board_id":"b1","column_id":"c1","name":"Doing"}"#,
        )
        .unwrap();
        assert_eq!(omitted.fsm_state, None, "omitted = leave unchanged");
        // Empty-string fsm_state => Some("") (clear to a manual column).
        let cleared: BoardColumnUpdateParams = serde_json::from_str(
            r#"{"workspace_id":"ws-1","board_id":"b1","column_id":"c1","fsm_state":""}"#,
        )
        .unwrap();
        assert_eq!(cleared.fsm_state.as_deref(), Some(""), "empty = clear");
    }

    /// The T4 card-orchestration wire shapes round-trip: a squad card carries its
    /// squad + member chips + blocked refs + auto-run, while a pre-T4 single-agent
    /// card omits every new field (append-only, byte-identical to the old wire).
    #[test]
    fn t4_card_orchestration_fields_roundtrip_and_omit_when_default() {
        // A squad card, blocked, auto-run on, with two member chips.
        let squad_card = BoardCardWireRow {
            issue_id: "issue-1".into(),
            title: "Ship it".into(),
            display_id: "sue-1".into(),
            state: None,
            session_name: None,
            repo_ref: Some("scratch".into()),
            agent: None,
            squad_id: Some("sq-1".into()),
            member_states: vec![
                CardMemberChip {
                    agent_id: "a-lead".into(),
                    agent_name: "lead".into(),
                    state: Some("running".into()),
                },
                CardMemberChip {
                    agent_id: "a-m1".into(),
                    agent_name: "m1".into(),
                    state: Some("queued".into()),
                },
            ],
            blocked_by: vec!["ock-2".into()],
            auto_run: true,
        };
        let s = serde_json::to_string(&squad_card).unwrap();
        assert_eq!(
            serde_json::from_str::<BoardCardWireRow>(&s).unwrap(),
            squad_card
        );
        assert!(
            s.contains("squad_id")
                && s.contains("member_states")
                && s.contains("blocked_by")
                && s.contains("auto_run")
        );

        // A single-agent card leaves every T4 field at its default → the wire omits
        // them all (a pre-T4 reader sees the exact old shape).
        let plain = BoardCardWireRow {
            issue_id: "i".into(),
            title: "t".into(),
            display_id: "i".into(),
            state: None,
            session_name: None,
            repo_ref: None,
            agent: None,
            squad_id: None,
            member_states: Vec::new(),
            blocked_by: Vec::new(),
            auto_run: false,
        };
        let s = serde_json::to_string(&plain).unwrap();
        for k in ["squad_id", "member_states", "blocked_by", "auto_run"] {
            assert!(
                !s.contains(k),
                "default T4 field {k} must be omitted, got {s}"
            );
        }

        // The run result carries the fanned-out member task ids for a squad card.
        let run = BoardCardRunResult {
            task_id: "t-lead".into(),
            agent_id: "a-lead".into(),
            runtime_id: "rt-lead".into(),
            mode: "headless".into(),
            member_task_ids: vec!["t-m1".into(), "t-m2".into()],
        };
        let s = serde_json::to_string(&run).unwrap();
        assert_eq!(serde_json::from_str::<BoardCardRunResult>(&s).unwrap(), run);
    }

    /// The F3 repo roster result round-trips, omitting the absent optionals.
    #[test]
    fn repo_list_result_roundtrips() {
        let result = RepoListResult {
            repos: vec![
                RepoWireRow {
                    name: "claude-code".into(),
                    path: None,
                    remote: Some("anthropics/claude-code".into()),
                    is_favorite: true,
                    last_used_ms: Some(1_700_000_000_000),
                },
                RepoWireRow {
                    name: "beta".into(),
                    path: Some("/repos/beta".into()),
                    remote: None,
                    is_favorite: false,
                    last_used_ms: None,
                },
            ],
        };
        let s = serde_json::to_string(&result).unwrap();
        assert_eq!(serde_json::from_str::<RepoListResult>(&s).unwrap(), result);
        // A scan-only row drops path-less/remote-less optionals.
        assert!(!s.contains("\"remote\":null"));
        assert!(!s.contains("\"last_used_ms\":null"));
    }

    /// APPEND-ONLY: a pre-parity card-create frame (no repo_ref / agent) still
    /// decodes, defaulting the new fields to None; a parity frame carries them.
    #[test]
    fn board_card_create_params_are_append_only() {
        let legacy: BoardCardCreateParams =
            serde_json::from_str(r#"{"workspace_id":"ws-1","board_id":"b1","title":"Fix it"}"#)
                .unwrap();
        assert_eq!(legacy.repo_ref, None);
        assert_eq!(legacy.agent, None);

        let parity: BoardCardCreateParams = serde_json::from_str(
            r#"{"workspace_id":"ws-1","board_id":"b1","title":"Fix it","repo_ref":"scratch","agent":"codex"}"#,
        )
        .unwrap();
        assert_eq!(parity.repo_ref.as_deref(), Some("scratch"));
        assert_eq!(parity.agent.as_deref(), Some("codex"));
    }

    /// APPEND-ONLY: a pre-parity card-run frame still decodes; a run override
    /// carries repo_ref + agent.
    #[test]
    fn board_card_run_params_are_append_only() {
        let legacy: BoardCardRunParams = serde_json::from_str(
            r#"{"workspace_id":"ws-1","board_id":"b1","issue_id":"i1","mode":"headless"}"#,
        )
        .unwrap();
        assert_eq!(legacy.repo_ref, None);
        assert_eq!(legacy.agent, None);
        // gap #8: the invoker override is append-only too — a legacy frame omits it
        // and `run_card` falls back to the workspace owner.
        assert_eq!(legacy.invoker_user_id, None);

        let over: BoardCardRunParams = serde_json::from_str(
            r#"{"workspace_id":"ws-1","board_id":"b1","issue_id":"i1","mode":"interactive","repo_ref":"/repos/app","agent":"claude","invoker_user_id":"bob"}"#,
        )
        .unwrap();
        assert_eq!(over.repo_ref.as_deref(), Some("/repos/app"));
        assert_eq!(over.agent.as_deref(), Some("claude"));
        assert_eq!(over.invoker_user_id.as_deref(), Some("bob"));
    }
}
