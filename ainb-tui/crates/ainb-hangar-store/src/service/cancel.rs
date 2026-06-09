//! The `CancelTask` service: `{queued|dispatched|running} -> cancelled`.
//!
//! A user may cancel a task at any non-terminal point in its lifecycle — before
//! it is claimed (`queued`), after claim but before start (`dispatched`), or
//! mid-run (`running`). [`CancelTaskService::cancel`] flips the row to
//! `cancelled` and stamps `finished_at`. Idempotent: a replayed cancel of an
//! already-`cancelled` row returns [`FinalizeOutcome::AlreadyTerminal`]; a row
//! that already reached `done` / `failed` returns
//! [`FinalizeError::TerminalMismatch`] — a completed run is not retroactively
//! cancellable.

use ainb_hangar_core::clock::HangarClock;
use ainb_hangar_core::task::state::TaskState;
use sqlx::SqlitePool;

use super::finalize::{FinalizeError, FinalizeOutcome, finalize_idempotent};

/// Stateless `{queued|dispatched|running} -> cancelled` service.
pub struct CancelTaskService;

impl CancelTaskService {
    /// Transition `task_id` to `cancelled`, stamping
    /// `finished_at = clock.now_ms()`. Legal from any non-terminal state.
    ///
    /// Emits a `task.cancel` span. The store-level cancel is source-agnostic
    /// (it takes no caller identity), so the span's `cancel_source` is the
    /// stable `"fsm"` token marking this FSM entrypoint; richer attribution
    /// (the user / sweeper that requested the cancel) is the caller's to log.
    ///
    /// # Errors
    ///
    /// - [`FinalizeError::TerminalMismatch`] if the row is already `done` /
    ///   `failed`.
    /// - [`FinalizeError::IllegalState`] if the row is absent.
    /// - [`FinalizeError::Db`] on an underlying database error.
    #[tracing::instrument(
        name = "task.cancel",
        skip(pool, clock),
        fields(task_id = %task_id, cancel_source = "fsm")
    )]
    pub async fn cancel(
        pool: &SqlitePool,
        task_id: &str,
        clock: &dyn HangarClock,
    ) -> Result<FinalizeOutcome, FinalizeError> {
        let now = clock.now_ms();
        finalize_idempotent(
            pool,
            task_id,
            TaskState::Cancelled,
            &[TaskState::Queued, TaskState::Dispatched, TaskState::Running],
            "UPDATE agent_task_queue \
             SET status = 'cancelled', finished_at = ?1 \
             WHERE id = ?2 AND status IN ('queued','dispatched','running')",
            move |q| q.bind(now).bind(task_id),
        )
        .await
    }
}
