//! The task lifecycle finite-state machine ([`TaskState`]).
//!
//! Mirrors the Multica `agent_task_queue.status` column verbatim:
//! `queued -> dispatched -> running -> done | failed | cancelled`
//! (`001_init.up.sql:132`). The serde and database string forms are both the
//! lowercase token so the JSON wire representation is byte-identical to the
//! `TEXT` value persisted by [`ainb_hangar_store`](../../../ainb_hangar_store/index.html).
//!
//! Unlike the P0 placeholder [`crate::task_status::TaskStatus`], this type owns
//! the legal-transition table and the [`TaskState::can_transition_to`] guard
//! the P1.2-P1.5 FSM services build on. No state-string literals should appear
//! anywhere outside this module.

use serde::{Deserialize, Serialize};

/// Lifecycle state of an `agent_task_queue` row.
///
/// The terminal set is `{Done, Failed, Cancelled}`; the pending (non-terminal,
/// not-yet-running) set is `{Queued, Dispatched}` — the partition the
/// `idx_one_pending_task_per_issue` partial unique index coalesces on
/// (migration 022).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskState {
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

/// The legal directed edges of the lifecycle FSM.
///
/// Seven edges per the Multica model:
/// 1. `queued -> dispatched` — claim by a runtime
/// 2. `dispatched -> running` — runtime confirmed start
/// 3. `dispatched -> queued` — reclaim of a stale dispatch (90s window)
/// 4. `running -> done` — successful completion
/// 5. `running -> failed` — execution error
/// 6. `running -> cancelled` — user cancel mid-run
/// 7. `queued -> failed` — queued-TTL sweep
const TRANSITIONS: &[(TaskState, TaskState)] = &[
    (TaskState::Queued, TaskState::Dispatched),
    (TaskState::Dispatched, TaskState::Running),
    (TaskState::Dispatched, TaskState::Queued),
    (TaskState::Running, TaskState::Done),
    (TaskState::Running, TaskState::Failed),
    (TaskState::Running, TaskState::Cancelled),
    (TaskState::Queued, TaskState::Failed),
];

/// Returned when a `(from, to)` transition is not in [`TRANSITIONS`].
///
/// `#[non_exhaustive]` so future fields (e.g. an actor / reason) can be added
/// without breaking downstream matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("illegal task transition: {from:?} -> {to:?}")]
#[non_exhaustive]
pub struct IllegalTransition {
    /// The source state of the attempted transition.
    pub from: TaskState,
    /// The (rejected) target state of the attempted transition.
    pub to: TaskState,
}

impl TaskState {
    /// The lowercase wire/DB token for this state.
    #[must_use]
    pub const fn as_db_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Dispatched => "dispatched",
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Parse a state from its lowercase DB/wire token.
    ///
    /// Case-sensitive and exhaustive; an unknown token is rejected rather than
    /// mapped to a default.
    ///
    /// # Errors
    /// Returns [`UnknownState`] when `s` is not one of the six lowercase tokens.
    pub fn from_db_str(s: &str) -> Result<Self, UnknownState> {
        match s {
            "queued" => Ok(Self::Queued),
            "dispatched" => Ok(Self::Dispatched),
            "running" => Ok(Self::Running),
            "done" => Ok(Self::Done),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(UnknownState(s.to_owned())),
        }
    }

    /// The complete set of legal `(from, to)` FSM edges.
    ///
    /// Exposes the private [`TRANSITIONS`] table so tests can drive the full
    /// negative-space assertion (every cartesian pair is `Ok` iff it is in this
    /// slice) off the single production source of truth, rather than maintaining
    /// a divergent copy of the legal set.
    #[must_use]
    pub const fn legal_transitions() -> &'static [(Self, Self)] {
        TRANSITIONS
    }

    /// Whether moving from `self` to `target` is a legal FSM edge.
    ///
    /// # Errors
    /// Returns [`IllegalTransition`] when `(self, target)` is not one of the
    /// seven edges in [`TRANSITIONS`]. Terminal states therefore reject every
    /// target.
    pub fn can_transition_to(self, target: Self) -> Result<(), IllegalTransition> {
        if TRANSITIONS.iter().any(|&(f, t)| f == self && t == target) {
            Ok(())
        } else {
            Err(IllegalTransition {
                from: self,
                to: target,
            })
        }
    }

    /// Whether this state is terminal (`Done` / `Failed` / `Cancelled`).
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Cancelled)
    }

    /// Whether this state is pending (`Queued` / `Dispatched`) — the set the
    /// `idx_one_pending_task_per_issue` partial unique index coalesces on.
    #[must_use]
    pub const fn is_pending(self) -> bool {
        matches!(self, Self::Queued | Self::Dispatched)
    }
}

/// Failure mode when parsing a [`TaskState`] from an unknown DB/wire token.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown task state token: {0:?}")]
pub struct UnknownState(pub String);
