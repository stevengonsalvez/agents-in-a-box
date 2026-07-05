//! Typed Hangar event stream payloads.
//!
//! The daemon pushes domain events to subscribed plugins as JSON-RPC
//! *notifications* (`{jsonrpc, method, params}`, no `id`).
//!
//! The `params` of an event notification is a serialised [`HangarEvent`]: an
//! internally-tagged enum whose `event` discriminant is the wire contract the
//! plugin's `StreamClient` keys on.
//!
//! These are **pure wire types** — `serde` + the IO-free id newtypes from
//! [`ainb_hangar_core`] + `chrono` timestamps, nothing host-side. The plugin
//! "owns zero domain data": it borrows these row shapes to render the rows it
//! pulls over RPC, but the source of truth is the daemon's `SQLite` store.
//!
//! Per `reference_msgpack_byte_determinism_vec_over_hashmap`, no field here is
//! a `HashMap` (whose iteration order varies per process and would break
//! byte-deterministic golden tests); every payload is a field-ordered struct.

use ainb_hangar_core::channel::ChannelSet;
use ainb_hangar_core::ids::{AgentId, CommentId, IssueId, TaskId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The JSON-RPC notification method carrying every [`HangarEvent`].
///
/// The event discriminant lives in the payload's `event` tag, not the method
/// name, so a single subscription channel carries all events.
pub const EVENT_METHOD: &str = "hangar/event";

/// A domain event pushed by the daemon over a subscribed event stream.
///
/// Internally tagged on `event`: the wire form is
/// `{"event":"task_started", ...payload fields...}`. The tag is deliberately
/// `event` and not `kind` to avoid colliding with [`HangarEvent::TaskMessage`]'s
/// own `kind` field (per `reference_serde_nested_tag_collision`). The plugin's
/// stream client decodes a notification's `params` into this enum; an unknown
/// `event` is a decode error (forward-compat surface, never a panic).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum HangarEvent {
    /// A new issue was created.
    IssueCreated(IssueRow),
    /// An existing issue's fields changed.
    IssueUpdated(IssueRow),
    /// An issue was deleted; carries only its id.
    ///
    /// A struct variant rather than a newtype: an internally-tagged enum cannot
    /// serialise a newtype variant that wraps a bare string (the id serialises
    /// transparently to a string, not a map), so the id rides in a named field.
    IssueDeleted {
        /// The deleted issue.
        issue_id: IssueId,
    },
    /// A task was enqueued against an issue for an agent to pick up.
    TaskQueued {
        /// The queued task.
        task_id: TaskId,
        /// The issue the task works on.
        issue_id: IssueId,
        /// The agent the task is assigned to.
        agent_id: AgentId,
    },
    /// An agent began executing a queued task.
    TaskStarted {
        /// The task that started.
        task_id: TaskId,
        /// Wall-clock start time.
        started_at: DateTime<Utc>,
    },
    /// Periodic progress heartbeat for a running task.
    TaskProgress {
        /// The running task.
        task_id: TaskId,
        /// Cumulative tool calls so far.
        tool_calls: u32,
        /// Elapsed run time in milliseconds.
        elapsed_ms: u64,
    },
    /// A transcript line emitted by a running task.
    TaskMessage {
        /// The task that produced the line.
        task_id: TaskId,
        /// Which of the 5-colour taxonomy lanes this line belongs to.
        kind: MessageKind,
        /// The line text.
        body: String,
    },
    /// A task reached a terminal state.
    TaskFinished {
        /// The finished task.
        task_id: TaskId,
        /// How it ended.
        result: TaskResult,
        /// Wall-clock end time.
        ended_at: DateTime<Utc>,
    },
    /// A comment was added to an issue.
    CommentAdded(CommentRow),
    /// An agent's presence changed.
    AgentPresence {
        /// The agent whose presence changed.
        agent_id: AgentId,
        /// Its new presence state.
        state: PresenceState,
    },
    /// A skill's curated source was updated remotely (the daemon pulled a newer
    /// version from `ainb-toolkit/skills/`).
    ///
    /// The skill-manager screen (P4.6) folds this into a conflict banner only
    /// when the local copy is dirty; a clean local copy refreshes silently.
    SkillUpdated {
        /// The slug of the skill whose source changed.
        skill: String,
        /// The remote update timestamp (epoch milliseconds).
        updated_at: i64,
    },
    /// An autopilot's fields changed (created / enabled toggled / next-tick
    /// recomputed) (P7.5).
    ///
    /// The manager screen folds this to refresh the row in place. Carries the
    /// full [`AutopilotRow`] so the screen needs no extra fetch.
    AutopilotUpdated(AutopilotRow),
    /// An autopilot fired (or skipped) a tick (P7.5).
    ///
    /// Emitted by the scheduler / fire path so the run-history pane can prepend a
    /// fresh run without re-fetching. Carries the affected autopilot's id and the
    /// run's terminal-or-running status (`running` / `completed` / `failed` /
    /// `cancelled` / `skipped`).
    AutopilotRunChanged {
        /// The autopilot the run belongs to.
        autopilot_id: String,
        /// The run's current status.
        status: String,
    },
    /// The host's active workspace changed (P5.5).
    ///
    /// Emitted when `host/workspace_set_active` switches the active workspace.
    /// Subscribed plugins re-fetch their workspace-scoped snapshots
    /// (`hangar/issues_list`, etc.) keyed on `to`. `from` is the previously
    /// active workspace id, or `None` when none was set (first activation).
    /// Both ids are the stable ULID workspace id, never the slug.
    WorkspaceChanged {
        /// The previously active workspace id, or `None` if unset before.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<String>,
        /// The newly active workspace id.
        to: String,
    },
    /// A session raised an input request — a fresh `open` [`AttentionRow`] now
    /// exists in the control-plane inbox (spec P2, store migration 0025).
    ///
    /// This is the FLEET-WIDE nudge every surface (control centre / web / bridge
    /// / ATC) reacts to by shuffling the raising session's card to the top and,
    /// if needed, re-pulling `attention/list`. Unlike the workspace-domain events
    /// this rides beside a `workspace_id` that is `None` for a hand-started host
    /// session that belongs to no ainb workspace — so it is delivered on the
    /// daemon's dedicated fleet-wide attention stream, not the workspace-scoped
    /// forwarder. The attention TABLE (not the event-log outbox) is its durable
    /// source: a reconnecting surface catches up via `attention/list`.
    AttentionRaised {
        /// The raised attention row's id (the answer RPC targets this).
        attention_id: String,
        /// The session that raised the request.
        session_id: String,
        /// The owning workspace, or `None` for a non-workspace host session.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace_id: Option<String>,
        /// The request family wire token (`ask_user_question` / `approval` / …).
        kind: String,
        /// `true` when sourced from the degraded pane-classifier fallback.
        #[serde(default)]
        degraded: bool,
        /// Ingest timestamp (epoch milliseconds).
        created_at: i64,
        /// The PUSH channels this attention was routed to (tcp T5), resolved once
        /// at raise time. A live consumer reacting to this nudge filters on this
        /// set; a reconnecting one re-pulls `attention/list` (which carries the
        /// same field). Empty = board-only. Additive: omitted by an older daemon.
        #[serde(default)]
        channels: ChannelSet,
    },
    /// An open attention row was answered — the first-answer-wins winner flipped
    /// it `answered` and the answer was delivered into the session (spec P2).
    ///
    /// Surfaces fold this to move the card to `answered(by=…)`; a surface that
    /// was mid-answer on the same row learns it lost the race.
    AttentionAnswered {
        /// The answered attention row's id.
        attention_id: String,
        /// The surface/actor that won the answer race (`tui` / `web` / `atc` / …).
        by: String,
    },
}

/// The 5-colour transcript taxonomy (reference UX §7 verbatim).
///
/// Each variant maps to one colour + glyph lane in the task-detail transcript
/// renderer (P4.4). The wire form is `snake_case`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    /// Agent prose output (emerald `▌`).
    Agent,
    /// Agent reasoning / thinking (violet `*`).
    Thinking,
    /// A tool invocation (blue `→`).
    ToolCall,
    /// A tool's result (slate `←`).
    ToolResult,
    /// An error line (red `!`).
    Error,
}

/// Terminal outcome of a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskResult {
    /// The task completed successfully.
    Success,
    /// The task failed.
    Failure,
    /// The task was cancelled before completion.
    Cancelled,
}

/// Three-state agent presence (reference UX §12.2).
///
/// `Unstable` (amber dot) means the runtime is *degraded* — not merely that the
/// agent is queueing work; see the daemon-side presence derivation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresenceState {
    /// Healthy and reachable (`● online`).
    Online,
    /// Runtime degraded (`◐ unstable`, amber).
    Unstable,
    /// Not reachable (`○ offline`).
    Offline,
}

/// A wire-side issue row.
///
/// This is the daemon's read model carried to the plugin — distinct from the
/// store's `Issue` (which carries sqlx/`ActorRef` types). Polymorphic actors
/// are flattened to their `member:<id>` / `agent:<id>` string form for the
/// wire; the plugin renders them, the daemon owns their integrity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueRow {
    /// Primary key.
    pub id: IssueId,
    /// The human-facing display id (`HGR-<n>`): the issue's 1-based per-workspace
    /// creation ordinal, prefixed with the workspace's configured `issue_prefix`
    /// or the `HGR` default (63l.3). The daemon derives this read-side via
    /// `IssueRepo::workspace_seq` + `issue_display_id`; the plugin renders it
    /// beside the title. `None` only on a pre-63l.3 snapshot — omitted from the
    /// wire when absent (`skip_serializing_if`) so the shape only grows for a
    /// reader that supplies it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_id: Option<String>,
    /// Owning workspace.
    pub workspace_id: String,
    /// Issue title.
    pub title: String,
    /// Free-form description; `None` when unset.
    pub description: Option<String>,
    /// Lifecycle state (e.g. `"open"`).
    pub state: String,
    /// Assigned actor in `type:id` form, or `None` when unassigned.
    pub assignee: Option<String>,
    /// Creating actor in `type:id` form (mandatory).
    pub creator: String,
    /// Creation timestamp (epoch milliseconds).
    pub created_at: i64,
    /// Urgency: `0..3` mapping `P3..P0` (HIGHER = MORE URGENT; default `0` =
    /// P3, routine) — the same scale as `TaskCardRow::priority` (migration
    /// 0014). `#[serde(default)]` keeps a pre-e38.9 snapshot decodable.
    #[serde(default)]
    pub priority: i64,
    /// Optional deadline as epoch milliseconds; `None` (the default) when unset.
    /// Omitted from the wire when absent (additive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_date: Option<i64>,
    /// Free-form labels (e.g. `["bug", "p0"]`). Empty by default; omitted from
    /// the wire when empty (additive) so a pre-e38.9 snapshot decodes to `[]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    /// The PR URL captured from this issue's latest completed task's
    /// `result.pr_url` (P9.1 capture, P9.2 surface), or `None` when no task on
    /// the issue opened a PR. Omitted from the JSON entirely when `None`
    /// (`skip_serializing_if`) so the wire shape only grows when a task actually
    /// produced a PR — a pre-P9.2 reader never sees a new `"pr_url": null` key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_url: Option<String>,
}

/// A wire-side actor row for the agent-picker snapshot (`hangar/agents_list`).
///
/// Polymorphic: a member (human) and an agent share this one shape so the picker
/// renders them in a single flat list (reference UX §12.1 polymorphic-actor
/// model). The `kind` discriminates the two; `presence` is only meaningful for
/// agents (a member is rendered as plainly available / offline), but the daemon
/// supplies it uniformly so the plugin never branches on kind to read a field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActorRow {
    /// The actor reference in canonical `member:<id>` / `agent:<id>` form.
    pub actor_ref: String,
    /// Display name (e.g. `alice`, `claude-agent`).
    pub display_name: String,
    /// A short subtitle (e.g. `backend dev`, `agent · gpt5`).
    pub subtitle: String,
    /// Current presence (drives the inline 3-state dot).
    pub presence: PresenceState,
    /// Whether this actor is an agent (`true`) or a human member (`false`).
    pub is_agent: bool,
    /// Recent-use rank: `Some(n)` pins the actor in the `RECENT` section (lower
    /// `n` = more recent); `None` falls into the alphabetical body.
    pub recent_rank: Option<u32>,
}

/// A wire-side skill row for the skill-manager list (`hangar/skills_list`).
///
/// A skill is a curated directory (a `SKILL.md` plus child files). The list pane
/// (P4.6) renders these; `used` drives the `Used` / `Unused` filter chips, and
/// `updated_at` is compared against the locally cached stamp to surface the
/// remote-conflict banner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillRow {
    /// The skill slug (its directory name, the stable id).
    pub slug: String,
    /// Human-readable skill name.
    pub name: String,
    /// Whether any agent currently references this skill (`false` = orphan).
    pub used: bool,
    /// The remote update timestamp (epoch milliseconds).
    pub updated_at: i64,
}

/// A wire-side file entry within a skill's directory (`hangar/skill_files`).
///
/// Flat list of the skill's files relative to its root; the file-tree widget
/// (P4.6) renders them as a tree by splitting `path` on `/`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillFile {
    /// The file path relative to the skill root (e.g. `SKILL.md`, `assets/x.md`).
    pub path: String,
}

/// A wire-side autopilot row for the manager list (`hangar/autopilots_list`).
///
/// A cron-scheduled autopilot (P7). The manager table (P7.5) renders these; the
/// daemon flattens its rich store row (typed ids, epoch-ms `next_tick_at`) into
/// this flat shape. The plugin owns zero domain data — it only renders the row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutopilotRow {
    /// The autopilot id (ULID string, the stable id the table rows carry).
    pub id: String,
    /// Owning workspace id.
    pub workspace_id: String,
    /// The agent dispatched to at each tick.
    pub agent_id: String,
    /// Display name (unique within the workspace).
    pub name: String,
    /// The validated UTC cron expression (e.g. `"0 9 * * 1-5"`).
    pub cron_expr: String,
    /// Cached next-firing instant (epoch-ms); `None` when no future match or
    /// while disabled with no recompute pending.
    pub next_tick_at: Option<i64>,
    /// Whether the scheduler currently considers this autopilot.
    pub enabled: bool,
    /// The most recent run's status (`completed` / `failed` / `running` /
    /// `skipped` / `cancelled`), or `None` when the autopilot has never run.
    /// Drives the `LAST RUN` column.
    pub last_run_status: Option<String>,
    /// The most recent run's start instant (epoch-ms), or `None` when never run.
    pub last_run_at: Option<i64>,
}

/// A wire-side autopilot run row for the history pane (`hangar/autopilot_runs`).
///
/// One firing of an autopilot. The run-history pane (P7.5) renders these
/// latest-first below the selected autopilot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutopilotRunRow {
    /// The run id (ULID string).
    pub id: String,
    /// The autopilot this run belongs to.
    pub autopilot_id: String,
    /// When the run started (epoch-ms).
    pub started_at: i64,
    /// When the run finished (epoch-ms); `None` while in flight.
    pub completed_at: Option<i64>,
    /// Lifecycle status (`running` / `completed` / `failed` / `cancelled` /
    /// `skipped`).
    pub status: String,
}

/// A wire-side task card row for the Kanban board (`hangar/tasks_list`, P8.4).
///
/// One `agent_task_queue` row flattened for the board. The plugin buckets these
/// into the four board columns by their raw [`status`](TaskCardRow::status) — one
/// of the six [`ainb_hangar_core::task_status::TaskStatus`] wire tokens
/// (`queued` / `dispatched` / `running` / `done` / `failed` / `cancelled`). The
/// plugin owns zero domain data: the daemon's `SQLite` store is the source of
/// truth; this is only the render shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskCardRow {
    /// The task id (ULID string, the stable id the card carries).
    pub id: TaskId,
    /// Owning workspace id.
    pub workspace_id: String,
    /// The agent executing the task (`agent.id`).
    pub agent_id: String,
    /// The originating issue id, or `None` for chat / autopilot tasks.
    pub issue_id: Option<String>,
    /// Raw lifecycle status — one of the six `TaskStatus` wire tokens. The board
    /// buckets these into its four columns client-side.
    pub status: String,
    /// Claim urgency: 0..3 mapping P3..P0 — higher = more urgent (store
    /// migration 0013). The claim loop drains `priority DESC, created_at, id`;
    /// `0` (P3) is the routine default. `#[serde(default)]` keeps snapshots
    /// from a pre-priority daemon decodable.
    #[serde(default)]
    pub priority: i64,
    /// Creation (queued-at) timestamp (epoch milliseconds) — drives the card age.
    pub created_at: i64,
    /// The worktree branch (`ainb/<slug>`) the run committed on (tcp T2), or
    /// `None` when the run made no commits / was not a worktree run. Recorded at
    /// finalize (store migration 0033); the durable artifact surviving teardown.
    /// Omitted from the wire when absent (`skip_serializing_if`) so the shape only
    /// grows for a run that produced a branch — a pre-T2 reader is unaffected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// The PR URL captured from the run's `result.pr_url` (P9.1), or `None` when
    /// the run opened no PR. Surfaces the same PR the backing issue shows, on the
    /// card (tcp T2). Omitted from the wire when absent (additive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_url: Option<String>,
    /// The PR's CI + merge status (tcp T2), fetched daemon-side via the injectable
    /// `gh` seam only for a card that HAS a `pr_url`; `None` otherwise. Carries the
    /// same three axes the issue task-detail badge renders. Omitted from the wire
    /// when absent (additive) so a pre-T2 reader never sees the new key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_status: Option<crate::pr_status::PrStatus>,
}

/// A wire-side aggregated inbox row for the notification inbox
/// (`hangar/inbox_list`, e38.14).
///
/// One `inbox_entry` row (store migration 0021) flattened for the inbox screen.
/// The daemon's aggregator folds live issue / comment / task events into these
/// durable rows; the plugin renders the list + an unread badge. `read_at` is the
/// whole unread model: `None` = unread, `Some(ms)` = read. The plugin owns zero
/// domain data — the daemon's `SQLite` store is the source of truth; this is only
/// the render shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxEntryRow {
    /// The inbox entry id (ULID string, the stable id the row carries).
    pub id: String,
    /// The entity family the entry is about (`issue` / `comment` / `task`).
    pub kind: String,
    /// The wire event discriminant that produced the entry (e.g. `issue_created`,
    /// `comment_added`, `task_queued`).
    pub event: String,
    /// The id of the issue / comment / task the entry addresses (deep-link target).
    pub subject_id: String,
    /// A short pre-rendered human line for the list row.
    pub summary: String,
    /// Creation timestamp (epoch milliseconds) — drives ordering + age.
    pub created_at: i64,
    /// When the entry was marked read (epoch milliseconds), or `None` when UNREAD.
    /// Omitted from the wire when unread (additive) so an unread entry is just an
    /// absent key, not a `"read_at": null`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_at: Option<i64>,
}

/// A wire-side attention row for the control-centre inbox
/// (`attention/list` / `attention/subscribe`, spec P2).
///
/// One `attention` row (store migration 0025) flattened for the surfaces. The
/// daemon's ingest producer folds every session's input request into a durable
/// row; the surfaces render an answerable card and route the answer back through
/// the one `answer` RPC. Carries no answered fields — the list/subscribe feeds
/// are the OPEN set only; a row leaves the feed the instant it is answered. The
/// plugin owns zero domain data — the daemon's `SQLite` store is the source of
/// truth; this is only the render shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttentionRow {
    /// The attention id (ULID string) — the answer RPC's target.
    pub id: String,
    /// The session that raised the request.
    pub session_id: String,
    /// The raising session's working directory (empty when unknown).
    pub cwd: String,
    /// The owning workspace, or `None` for a non-workspace host session. Omitted
    /// from the wire when absent (additive) so a fleet row is just a missing key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    /// The request family wire token (`ask_user_question` / `approval` /
    /// `codex_request_user` / `error` / `waiting` / `escalation`).
    pub kind: String,
    /// The full serialised request-context JSON the card renders.
    pub payload: String,
    /// `true` when sourced from the degraded pane-classifier fallback (unhooked
    /// session) — the surfaces badge it so the human knows the source is a
    /// heuristic. Omitted from the wire when `false` (additive).
    #[serde(default)]
    pub degraded: bool,
    /// Ingest timestamp (epoch milliseconds) — drives ordering + card age.
    pub created_at: i64,
    /// The PUSH channels this attention was routed to, resolved once at raise
    /// time from the notify rules (tcp T5). Consumers (bridge/web/os/atc) filter
    /// on this set rather than re-resolving, so a rule edit in flight can never
    /// split-brain the fan-out. The EMPTY set is board-only, never a dropped row.
    /// Defaults to empty on the wire (additive: a legacy row / older daemon that
    /// omits it reads as board-only).
    #[serde(default)]
    pub channels: ChannelSet,
}

/// A wire-side comment row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommentRow {
    /// Primary key.
    pub id: CommentId,
    /// The issue this comment belongs to.
    pub issue_id: IssueId,
    /// Authoring actor in `type:id` form.
    pub author: String,
    /// Comment body.
    pub body: String,
    /// Creation timestamp (epoch milliseconds).
    pub created_at: i64,
}
