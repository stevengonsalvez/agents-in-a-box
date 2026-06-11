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
    /// version from `toolkit/packages/skills/`).
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
}

/// The 5-colour transcript taxonomy (Multica UX §7 verbatim).
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

/// Three-state agent presence (Multica UX §12.2).
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
/// renders them in a single flat list (Multica UX §12.1 polymorphic-actor
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
