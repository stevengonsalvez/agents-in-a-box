//! The retry service: spawn a child task row when a failed task is eligible.
//!
//! When a task lands in `failed` the daemon asks
//! [`RetryService::maybe_retry_failed`] whether the failure warrants another
//! attempt. A *retryable* failure (the runtime went away, not the agent) with
//! attempts remaining spawns a fresh `queued` child row whose `parent_task_id`
//! chains back to the failed task and whose `attempt` is `parent.attempt + 1`.
//! Everything else (workspace / runtime / agent / issue / work_dir / max_attempts)
//! is inherited verbatim; all per-run timestamps and outputs reset.
//!
//! # Retry eligibility (Multica migration 055)
//!
//! Only [`FailureReason::RuntimeOffline`] and [`FailureReason::RuntimeRecovery`]
//! retry automatically — both mean the *infrastructure* failed, not the agent.
//! [`FailureReason::AgentError`] (the LLM mis-tooled or gave up) and
//! [`FailureReason::UserCancel`] are terminal-by-intent and never spawn a child,
//! and [`FailureReason::Timeout`] / [`FailureReason::Unknown`] are likewise not
//! retried. `attempt >= max_attempts` caps the chain regardless of reason.
//!
//! The child is written in a **single** `INSERT` that sets `status='queued'`
//! alongside `attempt`, `max_attempts`, and `parent_task_id` — mirroring the
//! all-or-nothing discipline of [`ClaimTaskService`](super::claim) so the child
//! row is *correct-or-absent*: there is no two-statement window in which an
//! orphan exists with the schema defaults (`attempt=1`, `parent_task_id=NULL`),
//! which would silently reset the chain cap and break parent-linkage if the
//! process died mid-retry. That single insert is subject to the
//! `idx_one_pending_task_per_issue_agent` partial unique index (migration 0012):
//! if another pending task already holds the per-(issue, agent) slot, the insert
//! raises a UNIQUE error which this service propagates (Multica raises + logs;
//! we mirror that rather than silently swallowing the collision).

use ainb_hangar_core::clock::HangarClock;

use sqlx::SqlitePool;

use super::fail::FailureReason;
use crate::repo::task::Task;

/// Failure reasons that warrant an automatic retry.
///
/// Both denote an infrastructure failure rather than an agent / user decision,
/// so re-dispatching the same work is safe (Multica migration 055).
const RETRYABLE_REASONS: &[FailureReason] = &[
    FailureReason::RuntimeOffline,
    FailureReason::RuntimeRecovery,
];

/// The outcome of a retry evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetryDecision {
    /// A child retry row was inserted, carrying this new task id.
    Spawned {
        /// The id of the freshly-enqueued child task.
        new_task_id: String,
    },
    /// The task is not eligible for retry (non-retryable reason or chain capped).
    DoNotRetry,
}

/// Single atomic INSERT that spawns the retry child row.
///
/// Writes `status='queued'` together with the retry bookkeeping
/// (`attempt`, `max_attempts`, `parent_task_id`) in one statement, so the child
/// is correct-or-absent rather than transiently carrying the schema defaults.
/// Every per-run column (`result`, `session_id`, `failure_reason`,
/// `started_at`, `finished_at`, `dispatched_at`) resets by being omitted (NULL).
/// Binds, in order: `id`, `workspace_id`, `runtime_id`, `agent_id`, `issue_id`,
/// `work_dir`, `attempt`, `max_attempts`, `parent_task_id`, `created_at`.
const SPAWN_CHILD_SQL: &str = "\
INSERT INTO agent_task_queue \
 (id, workspace_id, runtime_id, agent_id, issue_id, status, work_dir, \
  attempt, max_attempts, parent_task_id, created_at) \
 VALUES (?, ?, ?, ?, ?, 'queued', ?, ?, ?, ?, ?)";

/// Stateless retry service over `agent_task_queue`.
pub struct RetryService;

impl RetryService {
    /// Evaluate `failed_task` for retry and, if eligible, insert a child row.
    ///
    /// `new_id` is the id to mint for the child (caller supplies it via the P0
    /// [`IdGen`](ainb_hangar_core::idgen::IdGen) seam so the value is
    /// deterministic under test). `clock` stamps the child's `created_at`
    /// (queued-at) so the fresh attempt's timing is injectable.
    ///
    /// Returns [`RetryDecision::Spawned`] with the child id when a row was
    /// inserted, or [`RetryDecision::DoNotRetry`] when the failure reason is not
    /// retryable or `attempt >= max_attempts`.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the child insert fails — notably the
    /// `idx_one_pending_task_per_issue_agent` UNIQUE violation when another
    /// pending task already holds the per-(issue, agent) slot.
    pub async fn maybe_retry_failed(
        pool: &SqlitePool,
        failed_task: &Task,
        new_id: &str,
        clock: &dyn HangarClock,
    ) -> Result<RetryDecision, sqlx::Error> {
        if !Self::is_retryable(failed_task) {
            return Ok(RetryDecision::DoNotRetry);
        }

        let attempt = failed_task.attempt + 1;
        // One atomic INSERT: the child lands fully-formed (status='queued',
        // attempt / max_attempts / parent_task_id all set) or not at all. There
        // is no intermediate row with the schema defaults, so a crash here can
        // never strand an orphan that resets the chain cap or loses its parent
        // linkage. All per-run columns (result / session_id / failure_reason /
        // started_at / finished_at / dispatched_at) reset by being omitted (NULL).
        sqlx::query(SPAWN_CHILD_SQL)
            .bind(new_id)
            .bind(&failed_task.workspace_id)
            .bind(&failed_task.runtime_id)
            .bind(&failed_task.agent_id)
            .bind(&failed_task.issue_id)
            .bind(&failed_task.work_dir)
            .bind(attempt)
            .bind(failed_task.max_attempts)
            .bind(&failed_task.id)
            .bind(clock.now_ms())
            .execute(pool)
            .await?;

        // `is_retryable` already guaranteed `failure_reason` is `Some` (it
        // early-returns `DoNotRetry` otherwise), so the parent always has a reason
        // to log here.
        tracing::info!(
            parent_task_id = %failed_task.id,
            new_task_id = new_id,
            attempt,
            reason = failed_task.failure_reason.as_deref().unwrap_or_default(),
            "task_retry_spawned",
        );

        Ok(RetryDecision::Spawned {
            new_task_id: new_id.to_string(),
        })
    }

    /// Whether `task` is eligible for an automatic retry: a retryable failure
    /// reason AND attempts remaining (`attempt < max_attempts`).
    fn is_retryable(task: &Task) -> bool {
        if task.attempt >= task.max_attempts {
            return false;
        }
        let Some(reason) = task.failure_reason.as_deref() else {
            return false;
        };
        RETRYABLE_REASONS.iter().any(|r| r.as_db_str() == reason)
    }
}
