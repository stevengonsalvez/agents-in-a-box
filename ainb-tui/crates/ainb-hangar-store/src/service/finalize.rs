//! The shared idempotent-finalize primitive.
//!
//! Every FSM finalize service in P1.3 (Start / Complete / Fail / Cancel) drives
//! exactly one transition on an `agent_task_queue` row.
//!
//! Each must behave identically when the transition is *replayed* — a
//! crashed-and-retried daemon, a WS-vs-HTTP race (arch review §6), or a sweeper
//! racing the runner. [`finalize_idempotent`] captures that one behaviour so the
//! four services share it verbatim.
//!
//! # The algorithm (Multica `task.go:1010` idempotent finalize)
//!
//! 1. Run a conditional UPDATE that only fires when the row is in one of
//!    `expected_from` and reports `rows_affected`.
//! 2. `rows_affected == 1` → the caller won the transition →
//!    [`FinalizeOutcome::Transitioned`].
//! 3. `rows_affected == 0` → the row was *not* in an expected source state.
//!    Re-read its current status and decide deterministically:
//!    - status == `target` → the transition already happened (idempotent
//!      replay) → [`FinalizeOutcome::AlreadyTerminal`] (success),
//!    - status is a *different* terminal state → another finalizer won with a
//!      different outcome → [`FinalizeError::TerminalMismatch`] (we must never
//!      silently flip one terminal state into another),
//!    - status is a non-terminal state we did not expect (e.g. starting a
//!      `queued` task, or the row vanished) → [`FinalizeError::IllegalState`].
//!
//! Because the conditional UPDATE is a single statement, `SQLite` serialises
//! concurrent writers: the first transactional UPDATE wins and the loser falls
//! through the `rows_affected == 0` branch above, re-reads, and resolves cleanly.

use ainb_hangar_core::task::state::TaskState;
use sqlx::{Row, SqlitePool};

/// The successful result of a finalize attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinalizeOutcome {
    /// This caller performed the transition (UPDATE affected one row).
    Transitioned,
    /// The row was already in the requested target state — an idempotent
    /// replay. Treated as success so a retried finalize never errors.
    AlreadyTerminal,
}

/// Failure modes of a finalize attempt.
///
/// Does not derive `Clone` / `PartialEq` because the [`FinalizeError::Db`]
/// variant wraps [`sqlx::Error`], which implements neither. `#[non_exhaustive]`
/// so future variants can be added without breaking downstream exhaustive
/// matches.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FinalizeError {
    /// `StartTask` was called on a task that is already past `dispatched`
    /// (its own dedicated, friendlier variant of an illegal-state error).
    #[error("task already started (not in a dispatched state)")]
    AlreadyStarted,
    /// The row landed in a *different* terminal state than the one requested
    /// (e.g. a `complete` losing to a concurrent `cancel`). The winning state
    /// is preserved; the request is rejected rather than overwriting it.
    #[error("terminal mismatch: row is {found:?}, expected to land in {target:?}")]
    TerminalMismatch {
        /// The terminal state the row is actually in.
        found: TaskState,
        /// The terminal state this finalize attempt wanted to reach.
        target: TaskState,
    },
    /// The row was in a non-terminal state from which this transition is not
    /// legal, or the row does not exist.
    #[error("illegal state for transition to {target:?}: row is {found:?}")]
    IllegalState {
        /// The current state of the row, or `None` if the row is absent.
        found: Option<TaskState>,
        /// The terminal state this finalize attempt wanted to reach.
        target: TaskState,
    },
    /// A database error escaped the underlying statement.
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// Apply one idempotent FSM transition to a single `agent_task_queue` row.
///
/// `update_sql` MUST be a conditional UPDATE whose `WHERE` clause restricts the
/// row to `id = ? AND status IN (expected_from...)` and sets `status = target`
/// plus whatever side columns the caller needs (timestamps, result, etc.). The
/// caller pre-binds those columns via `bind_update`; this helper binds nothing
/// itself, so the service owns the full statement shape and parameter order.
///
/// On a 0-row update the row's current `status` is re-read and classified per
/// the module-level algorithm (Multica `task.go:1010`).
///
/// `task_id`, `target`, `expected_from`, and the resolved outcome are emitted as
/// a `tracing` event so every transition is grep-able in the daemon log.
///
/// # Errors
///
/// Returns [`FinalizeError`] for a terminal mismatch, an illegal source state,
/// a missing row, or an underlying database failure.
pub async fn finalize_idempotent<'q>(
    pool: &SqlitePool,
    task_id: &str,
    target: TaskState,
    expected_from: &[TaskState],
    update_sql: &'q str,
    bind_update: impl FnOnce(
        sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>>,
    )
        -> sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>>,
) -> Result<FinalizeOutcome, FinalizeError> {
    let affected = bind_update(sqlx::query(update_sql)).execute(pool).await?.rows_affected();

    if affected == 1 {
        tracing::info!(
            task_id,
            to = target.as_db_str(),
            outcome = "transitioned",
            "task_finalize",
        );
        // Cascade: if this task was fired by an autopilot and just reached a
        // terminal state, stamp the parent run's completed_at + status. Runs on
        // the real complete / fail / cancel path (every finalize service routes
        // through here), not a separate caller-driven update.
        cascade_autopilot_run(pool, task_id, target).await?;
        return Ok(FinalizeOutcome::Transitioned);
    }

    // 0-row update: re-read and classify. The re-read is a separate query from
    // the UPDATE, not wrapped in one transaction with it, so the classification
    // reflects the *last-committed* state of the row rather than a snapshot
    // atomic with the failed UPDATE. That is exactly what idempotent finalize
    // wants (the loser of a race observes the winner's committed terminal state)
    // and matches Multica `task.go:1010`.
    let current = read_state(pool, task_id).await?;
    classify_no_op(task_id, target, expected_from, current)
}

/// Stamp the autopilot run a just-finalized task belongs to, if any.
///
/// When a task carrying a non-null `autopilot_run_id` reaches a *terminal*
/// state, its parent `autopilot_run` is marked finished: `completed_at` is set
/// to the task's own `finished_at` (the finalize UPDATE stamped it in the same
/// call) and the run `status` is mapped from the task terminal state
/// (`done -> completed`, `failed -> failed`, `cancelled -> cancelled`).
///
/// A no-op when `target` is non-terminal or the task is not an autopilot task
/// (the `WHERE` clause matches no run row). The single correlated UPDATE is
/// idempotent: re-running it re-writes the same values.
async fn cascade_autopilot_run(
    pool: &SqlitePool,
    task_id: &str,
    target: TaskState,
) -> Result<(), FinalizeError> {
    // Only terminal task states close a run; map onto the run status vocabulary.
    let run_status = match target {
        TaskState::Done => "completed",
        TaskState::Failed => "failed",
        TaskState::Cancelled => "cancelled",
        // Non-terminal transitions (e.g. Start's `running`) never touch a run.
        TaskState::Queued | TaskState::Dispatched | TaskState::Running => return Ok(()),
    };
    sqlx::query(
        "UPDATE autopilot_run \
         SET completed_at = (SELECT finished_at FROM agent_task_queue WHERE id = ?1), \
             status = ?2 \
         WHERE id = (SELECT autopilot_run_id FROM agent_task_queue WHERE id = ?1)",
    )
    .bind(task_id)
    .bind(run_status)
    .execute(pool)
    .await?;
    Ok(())
}

/// Record the task's `workspace_id` onto the current `tracing` span, if known.
///
/// The FSM finalize services take only a `task_id`, but P8.3 wants the owning
/// `workspace_id` on the span so an operator can scope a query to one workspace.
/// This is a single indexed-PK `SELECT` on a per-run (not per-loop) transition,
/// so the cost is negligible. Best-effort: a missing row or DB hiccup just
/// leaves the field unset rather than failing the transition — observability
/// must never break the FSM.
pub(crate) async fn record_workspace_id(pool: &SqlitePool, task_id: &str) {
    if let Ok(Some(row)) = sqlx::query("SELECT workspace_id FROM agent_task_queue WHERE id = ?")
        .bind(task_id)
        .fetch_optional(pool)
        .await
    {
        if let Ok(workspace_id) = row.try_get::<String, _>("workspace_id") {
            tracing::Span::current().record("workspace_id", workspace_id.as_str());
        }
    }
}

/// Re-read the current [`TaskState`] of a task, or `None` if the row is absent.
async fn read_state(pool: &SqlitePool, task_id: &str) -> Result<Option<TaskState>, FinalizeError> {
    let row = sqlx::query("SELECT status FROM agent_task_queue WHERE id = ?")
        .bind(task_id)
        .fetch_optional(pool)
        .await?;
    match row {
        None => Ok(None),
        Some(r) => {
            let s: String = r.try_get("status")?;
            // An unrecognised status string is a hard data-integrity bug, not a
            // routine illegal-state — surface it as a DB error.
            TaskState::from_db_str(&s)
                .map(Some)
                .map_err(|e| FinalizeError::Db(sqlx::Error::Decode(Box::new(e))))
        }
    }
}

/// Decide the outcome of a 0-row UPDATE given the row's freshly-read state.
fn classify_no_op(
    task_id: &str,
    target: TaskState,
    expected_from: &[TaskState],
    current: Option<TaskState>,
) -> Result<FinalizeOutcome, FinalizeError> {
    match current {
        // A re-read landing in the target is an idempotent replay — but only a
        // *terminal* target is "already done". A non-terminal target (Start's
        // `running`) re-found in that same state means a second live start: that
        // is a race/bug surfaced below as `AlreadyStarted`, not a silent
        // success.
        Some(state) if state == target && target.is_terminal() => {
            tracing::info!(
                task_id,
                to = target.as_db_str(),
                outcome = "already_terminal",
                "task_finalize",
            );
            Ok(FinalizeOutcome::AlreadyTerminal)
        }
        Some(state) if state.is_terminal() => {
            tracing::warn!(
                task_id,
                found = state.as_db_str(),
                target = target.as_db_str(),
                outcome = "terminal_mismatch",
                "task_finalize",
            );
            Err(FinalizeError::TerminalMismatch {
                found: state,
                target,
            })
        }
        // A non-terminal state we did not expect, or a vanished row: the
        // `StartTask` case (target=running, expected_from=[dispatched]) maps an
        // already-`running` row to the friendlier `AlreadyStarted`.
        other => {
            if target == TaskState::Running
                && expected_from == [TaskState::Dispatched]
                && other == Some(TaskState::Running)
            {
                return Err(FinalizeError::AlreadyStarted);
            }
            tracing::warn!(
                task_id,
                found = other.map_or("<absent>", TaskState::as_db_str),
                target = target.as_db_str(),
                outcome = "illegal_state",
                "task_finalize",
            );
            Err(FinalizeError::IllegalState {
                found: other,
                target,
            })
        }
    }
}
