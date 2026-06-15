//! The task lifecycle status enum.
//!
//! Mirrors the `status` `CHECK` constraint on `agent_task_queue`:
//! `queued -> dispatched -> running -> done | failed | cancelled`. The serde
//! wire form is snake_case so the JSON representation is byte-identical to the
//! `TEXT` values stored by the persistence layer and carried over the RPC wire.
//!
//! P0 only ever writes [`TaskStatus::Queued`]; the typed state transitions land
//! in P1.

use serde::{Deserialize, Serialize};

/// Lifecycle status of an `agent_task_queue` row.
///
/// The terminal set is `{Done, Failed, Cancelled}`; the pending (non-terminal,
/// not-yet-running) set is `{Queued, Dispatched}` — the same partition the
/// `idx_one_pending_task_per_issue` partial unique index enforces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Enqueued, awaiting dispatch to a runtime.
    Queued,
    /// Handed to a runtime but not yet confirmed running.
    Dispatched,
    /// Actively executing.
    Running,
    /// Completed successfully (terminal).
    Done,
    /// Completed with an error (terminal).
    Failed,
    /// Cancelled before completion (terminal).
    Cancelled,
}

impl TaskStatus {
    /// The lowercase wire token stored in the `status` column.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Dispatched => "dispatched",
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Whether this status is terminal (`Done` / `Failed` / `Cancelled`).
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Cancelled)
    }

    /// Whether this status is pending (`Queued` / `Dispatched`) — the set the
    /// partial unique index coalesces on.
    #[must_use]
    pub const fn is_pending(self) -> bool {
        matches!(self, Self::Queued | Self::Dispatched)
    }
}
