//! The `CompleteTask` service: `running -> done`.
//!
//! [`CompleteTaskService::complete`] records the agent's structured result,
//! pins the provider `session_id` and `work_dir`, stamps `finished_at`, and
//! flips the row to `done`. It is idempotent: a replayed completion of an
//! already-`done` row returns [`FinalizeOutcome::AlreadyTerminal`]; a row that
//! lost to a concurrent `fail`/`cancel` returns
//! [`FinalizeError::TerminalMismatch`] (verbatim Multica `task.go:1010`).

use ainb_hangar_core::clock::HangarClock;
use ainb_hangar_core::task::state::TaskState;
use serde_json::Value;
use sqlx::SqlitePool;

use super::finalize::{finalize_idempotent, FinalizeError, FinalizeOutcome};

/// The successful-completion payload persisted onto the task row.
#[derive(Debug, Clone)]
pub struct CompleteParams {
    /// The agent's structured result, stored as JSON in the `result` column.
    pub result: Value,
    /// Provider session id the run used, pinned for resume / audit, or `None`.
    pub session_id: Option<String>,
    /// Working directory the run executed in, or `None`.
    pub work_dir: Option<String>,
}

/// Stateless `running -> done` service over `agent_task_queue`.
pub struct CompleteTaskService;

impl CompleteTaskService {
    /// Transition `task_id` from `running` to `done`, writing `params.result`
    /// (serialized JSON), `session_id`, `work_dir`, and
    /// `finished_at = clock.now_ms()`.
    ///
    /// # Errors
    ///
    /// - [`FinalizeError::TerminalMismatch`] if the row is already `failed` /
    ///   `cancelled`.
    /// - [`FinalizeError::IllegalState`] if the row is non-terminal-but-unexpected
    ///   (e.g. still `queued` / `dispatched`) or absent.
    /// - [`FinalizeError::Db`] on a serialize or database failure.
    #[tracing::instrument(
        name = "task.complete",
        skip(pool, params, clock),
        fields(task_id = %task_id, outcome = tracing::field::Empty)
    )]
    pub async fn complete(
        pool: &SqlitePool,
        task_id: &str,
        params: CompleteParams,
        clock: &dyn HangarClock,
    ) -> Result<FinalizeOutcome, FinalizeError> {
        let now = clock.now_ms();
        let result_json = serde_json::to_string(&params.result)
            .map_err(|e| FinalizeError::Db(sqlx::Error::Encode(Box::new(e))))?;
        let outcome = finalize_idempotent(
            pool,
            task_id,
            TaskState::Done,
            &[TaskState::Running],
            "UPDATE agent_task_queue \
             SET status = 'done', result = ?1, session_id = ?2, work_dir = ?3, finished_at = ?4 \
             WHERE id = ?5 AND status = 'running'",
            move |q| {
                q.bind(result_json)
                    .bind(params.session_id)
                    .bind(params.work_dir)
                    .bind(now)
                    .bind(task_id)
            },
        )
        .await?;
        // The terminal state this transition lands in, recorded once known so the
        // span carries the resolved outcome (`done`) regardless of whether this
        // call won the transition or replayed an already-done row.
        tracing::Span::current().record("outcome", TaskState::Done.as_db_str());
        Ok(outcome)
    }
}
