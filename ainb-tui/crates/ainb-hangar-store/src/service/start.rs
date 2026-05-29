//! The `StartTask` service: `dispatched -> running`.
//!
//! After a runtime claims a task ([`crate::service::claim`]) and the runner
//! confirms the agent subprocess is live, [`StartTaskService::start`] flips the
//! row to `running` and stamps `started_at`. Unlike the three finalize-to-
//! terminal services, starting is *not* idempotent the same way: a second start
//! of an already-`running` task is a programming/race error surfaced as
//! [`FinalizeError::AlreadyStarted`] rather than a silent success, because two
//! live runs of one task is a real bug we want loud.

use ainb_hangar_core::clock::HangarClock;
use ainb_hangar_core::task::state::TaskState;
use sqlx::SqlitePool;

use super::finalize::{finalize_idempotent, FinalizeError, FinalizeOutcome};

/// Stateless `dispatched -> running` service over `agent_task_queue`.
pub struct StartTaskService;

impl StartTaskService {
    /// Transition `task_id` from `dispatched` to `running`, stamping
    /// `started_at = clock.now_ms()`.
    ///
    /// Returns [`FinalizeOutcome::Transitioned`] on success.
    ///
    /// # Errors
    ///
    /// - [`FinalizeError::AlreadyStarted`] if the row is already `running`.
    /// - [`FinalizeError::TerminalMismatch`] if the row is already terminal.
    /// - [`FinalizeError::IllegalState`] if the row is `queued` or absent.
    /// - [`FinalizeError::Db`] on an underlying database error.
    pub async fn start(
        pool: &SqlitePool,
        task_id: &str,
        clock: &dyn HangarClock,
    ) -> Result<FinalizeOutcome, FinalizeError> {
        let now = clock.now_ms();
        finalize_idempotent(
            pool,
            task_id,
            TaskState::Running,
            &[TaskState::Dispatched],
            "UPDATE agent_task_queue SET status = 'running', started_at = ?1 \
             WHERE id = ?2 AND status = 'dispatched'",
            move |q| q.bind(now).bind(task_id),
        )
        .await
    }
}
