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
