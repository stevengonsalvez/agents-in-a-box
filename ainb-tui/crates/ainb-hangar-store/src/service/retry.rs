//! The retry service: spawn a child task row when a failed task is eligible.
//!
//! When a task lands in `failed` the daemon asks
//! [`RetryService::maybe_retry_failed`] whether the failure warrants another
//! attempt. A *retryable* failure (the runtime went away, not the agent) with
//! attempts remaining spawns a fresh `queued` child row whose `parent_task_id`
//! chains back to the failed task and whose `attempt` is `parent.attempt + 1`.
//! Everything else (workspace / runtime / agent / issue / work_dir / priority /
//! max_attempts / **repo_ref / agent_kind**) is inherited verbatim so the retry
//! re-provisions the SAME repo's worktree under the SAME provider (tcp 19n); all
//! per-run timestamps, outputs, and the recorded `branch` reset.
//!
//! # Retry/resume taxonomy (reference migration 055 + `GetLastTaskSession`)
//!
//! The terminal [`FailureReason`] classifies into one of three
//! [`RetryDisposition`]s ([`RetryService::retry_disposition`]):
//!
//! - **[`RetryDisposition::ResumeRetry`]** —
//!   [`FailureReason::RuntimeOffline`] / [`FailureReason::RuntimeRecovery`]: the
//!   *infrastructure* failed, not the agent, so the retry **resumes** the
//!   parent's provider session (`session_id` is carried onto the child).
//! - **[`RetryDisposition::FreshRetry`]** — the conversation-poisoning terminals
//!   [`FailureReason::IterationLimit`] / [`FailureReason::ApiInvalidRequest`] /
//!   [`FailureReason::SemanticInactivity`]: the model wedged on its own context,
//!   so the retry **starts fresh** — the child's `session_id` is cleared so a
//!   *new* session begins instead of resuming the wedged one. This mirrors
//!   the reference's `GetLastTaskSession`, which excludes poisoned terminal states
//!   from the resume hint so an auto-retry never inherits a stuck conversation.
//! - **[`RetryDisposition::NoRetry`]** — [`FailureReason::AgentError`] (the LLM
//!   mis-tooled / gave up), [`FailureReason::UserCancel`] (terminal by intent),
//!   and [`FailureReason::Timeout`] / [`FailureReason::Unknown`]: no child row.
//!
//! `attempt >= max_attempts` caps the chain regardless of disposition.
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
//! raises a UNIQUE error which this service propagates (the reference raises + logs;
//! we mirror that rather than silently swallowing the collision).

use ainb_hangar_core::clock::HangarClock;

use sqlx::SqlitePool;

use super::fail::FailureReason;
use crate::repo::task::Task;

/// How a failed task's terminal reason classifies for retry, and — when it
/// retries — whether the child resumes or starts a fresh provider session.
///
/// This is the poisoned-terminal taxonomy (e38.24): a reason maps to exactly
/// one disposition, and that disposition decides both *whether* a child spawns
/// and *whether the child inherits the parent's `session_id`*. See
/// [`RetryService::retry_disposition`] for the per-reason mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryDisposition {
    /// Retry **and resume** the parent's provider session: the failure was
    /// infrastructure (the runtime went away), so the conversation is intact and
    /// the child carries the parent's `session_id` to continue it.
    ResumeRetry,
    /// Retry but **start fresh**: the parent's conversation is poisoned (the
    /// model wedged), so the child clears `session_id` and a *new* session
    /// begins. Resuming the wedged conversation would only re-fail.
    FreshRetry,
    /// Do not retry: an agent-error / user-cancel / sweeper-timeout / unknown
    /// terminal that another attempt cannot productively re-run.
    NoRetry,
}

impl RetryDisposition {
    /// Whether this disposition spawns a retry child at all (`ResumeRetry` or
    /// `FreshRetry`, i.e. anything but [`Self::NoRetry`]).
    #[must_use]
    const fn retries(self) -> bool {
        !matches!(self, Self::NoRetry)
    }

    /// Whether a spawned child carries the parent's `session_id` (resume) rather
    /// than starting a fresh session. Only [`Self::ResumeRetry`] resumes.
    #[must_use]
    const fn resumes_session(self) -> bool {
        matches!(self, Self::ResumeRetry)
    }
}

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
/// `session_id` is bound explicitly per the retry/resume taxonomy: the parent's
/// session for a [`RetryDisposition::ResumeRetry`] (so the child *resumes* the
/// conversation), or `NULL` for a [`RetryDisposition::FreshRetry`] (so a *new*
/// session begins instead of resuming a poisoned conversation).
///
/// `repo_ref` and `agent_kind` are copied from the parent (tcp 19n): without
/// them a RuntimeOffline-retried card lost its repo (`repo_ref` fell to NULL)
/// and its provider (`agent_kind` reset to the `claude` column default), so the
/// re-dispatched attempt ran in the in-tree fallback dir under the wrong
/// provider instead of re-provisioning the SAME repo's worktree. The remaining
/// per-run columns (`result`, `failure_reason`, `started_at`, `finished_at`,
/// `dispatched_at`, `branch`) reset by being omitted (NULL) — `branch` is per-run
/// (a fresh child mints a fresh worktree and records its own); `priority` is
/// inherited so a retried urgent task stays urgent in the claim ordering.
/// Binds, in order: `id`, `workspace_id`, `runtime_id`, `agent_id`, `issue_id`,
/// `work_dir`, `priority`, `attempt`, `max_attempts`, `parent_task_id`,
/// `session_id`, `repo_ref`, `agent_kind`, `created_at`.
const SPAWN_CHILD_SQL: &str = "\
INSERT INTO agent_task_queue \
 (id, workspace_id, runtime_id, agent_id, issue_id, status, work_dir, priority, \
  attempt, max_attempts, parent_task_id, session_id, repo_ref, agent_kind, created_at) \
 VALUES (?, ?, ?, ?, ?, 'queued', ?, ?, ?, ?, ?, ?, ?, ?, ?)";

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
    /// When a child *is* spawned, its `session_id` follows the taxonomy: a
    /// [`RetryDisposition::ResumeRetry`] carries the parent's `session_id` (the
    /// new attempt resumes the conversation); a [`RetryDisposition::FreshRetry`]
    /// clears it (the new attempt starts a fresh session, not resuming the
    /// poisoned one).
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
        // Classify the terminal reason once. `attempt >= max_attempts` caps the
        // chain regardless of disposition.
        let Some(disposition) = Self::task_disposition(failed_task) else {
            return Ok(RetryDecision::DoNotRetry);
        };
        if !disposition.retries() || failed_task.attempt >= failed_task.max_attempts {
            return Ok(RetryDecision::DoNotRetry);
        }

        // Resume → carry the parent's session_id; fresh → clear it so a new
        // session begins rather than resuming a poisoned conversation.
        let child_session_id: Option<&str> = if disposition.resumes_session() {
            failed_task.session_id.as_deref()
        } else {
            None
        };

        let attempt = failed_task.attempt + 1;
        // One atomic INSERT: the child lands fully-formed (status='queued',
        // attempt / max_attempts / parent_task_id / session_id all set) or not at
        // all. There is no intermediate row with the schema defaults, so a crash
        // here can never strand an orphan that resets the chain cap or loses its
        // parent linkage. The remaining per-run columns (result / failure_reason /
        // started_at / finished_at / dispatched_at) reset by being omitted (NULL).
        sqlx::query(SPAWN_CHILD_SQL)
            .bind(new_id)
            .bind(&failed_task.workspace_id)
            .bind(&failed_task.runtime_id)
            .bind(&failed_task.agent_id)
            .bind(&failed_task.issue_id)
            .bind(&failed_task.work_dir)
            .bind(failed_task.priority)
            .bind(attempt)
            .bind(failed_task.max_attempts)
            .bind(&failed_task.id)
            .bind(child_session_id)
            .bind(&failed_task.repo_ref)
            .bind(&failed_task.agent_kind)
            .bind(clock.now_ms())
            .execute(pool)
            .await?;

        // `task_disposition` already guaranteed `failure_reason` is `Some` (it
        // returns `None` otherwise), so the parent always has a reason to log.
        tracing::info!(
            parent_task_id = %failed_task.id,
            new_task_id = new_id,
            attempt,
            reason = failed_task.failure_reason.as_deref().unwrap_or_default(),
            resumed = disposition.resumes_session(),
            "task_retry_spawned",
        );

        Ok(RetryDecision::Spawned {
            new_task_id: new_id.to_string(),
        })
    }

    /// Classify a [`FailureReason`] into its [`RetryDisposition`] — the
    /// poisoned-terminal taxonomy at the heart of e38.24.
    ///
    /// Exhaustive (no wildcard arm, per
    /// `reference_gated_by_variant_propagation`): a newly-added `FailureReason`
    /// variant must be deliberately placed into a disposition here, so a reason
    /// can never silently default into resuming a wedged session.
    #[must_use]
    pub const fn retry_disposition(reason: FailureReason) -> RetryDisposition {
        match reason {
            // Infrastructure failed, not the agent — resume the conversation.
            FailureReason::RuntimeOffline | FailureReason::RuntimeRecovery => {
                RetryDisposition::ResumeRetry
            }
            // Conversation-poisoning terminals — retry, but in a fresh session.
            FailureReason::IterationLimit
            | FailureReason::ApiInvalidRequest
            | FailureReason::SemanticInactivity => RetryDisposition::FreshRetry,
            // Agent / user / sweeper / unclassified terminals — no retry.
            FailureReason::AgentError
            | FailureReason::UserCancel
            | FailureReason::Timeout
            | FailureReason::Unknown => RetryDisposition::NoRetry,
        }
    }

    /// Resolve a failed task's stored `failure_reason` string to its
    /// [`RetryDisposition`], or `None` when the row carries no recognised reason
    /// (no `failure_reason`, or a token outside the enum). `None` is treated as
    /// non-retryable by the caller.
    fn task_disposition(task: &Task) -> Option<RetryDisposition> {
        let reason = task.failure_reason.as_deref()?;
        FAILURE_REASONS
            .iter()
            .find(|r| r.as_db_str() == reason)
            .map(|r| Self::retry_disposition(*r))
    }
}

/// Every [`FailureReason`] variant, used to resolve a stored `failure_reason`
/// token back to its typed reason (and thence its [`RetryDisposition`]). Kept in
/// sync with the enum by the `failure_reasons_covers_all_variants` test below.
const FAILURE_REASONS: &[FailureReason] = &[
    FailureReason::Timeout,
    FailureReason::AgentError,
    FailureReason::RuntimeOffline,
    FailureReason::RuntimeRecovery,
    FailureReason::UserCancel,
    FailureReason::IterationLimit,
    FailureReason::ApiInvalidRequest,
    FailureReason::SemanticInactivity,
    FailureReason::Unknown,
];

#[cfg(test)]
mod tests {
    use super::{FAILURE_REASONS, FailureReason, RetryDisposition, RetryService};

    /// `FAILURE_REASONS` must list every enum variant, so `task_disposition`
    /// can resolve any stored token. A new variant added without extending the
    /// list would fail to round-trip and silently fall to non-retryable.
    #[test]
    fn failure_reasons_covers_all_variants() {
        // Exhaustive match: adding a variant forces a compile error here until
        // its token is asserted present in FAILURE_REASONS.
        for reason in [
            FailureReason::Timeout,
            FailureReason::AgentError,
            FailureReason::RuntimeOffline,
            FailureReason::RuntimeRecovery,
            FailureReason::UserCancel,
            FailureReason::IterationLimit,
            FailureReason::ApiInvalidRequest,
            FailureReason::SemanticInactivity,
            FailureReason::Unknown,
        ] {
            assert!(
                FAILURE_REASONS.contains(&reason),
                "FAILURE_REASONS is missing {reason:?}",
            );
        }
        assert_eq!(
            FAILURE_REASONS.len(),
            9,
            "FAILURE_REASONS length drifted from the FailureReason variant count",
        );
    }

    /// The poisoned terminals must classify as `FreshRetry`, the infra terminals
    /// as `ResumeRetry`, and the rest as `NoRetry`. This pins the taxonomy that
    /// `maybe_retry_failed` keys session inheritance off.
    #[test]
    fn retry_disposition_taxonomy() {
        use FailureReason::*;
        use RetryDisposition::*;
        assert_eq!(RetryService::retry_disposition(RuntimeOffline), ResumeRetry);
        assert_eq!(
            RetryService::retry_disposition(RuntimeRecovery),
            ResumeRetry
        );
        assert_eq!(RetryService::retry_disposition(IterationLimit), FreshRetry);
        assert_eq!(
            RetryService::retry_disposition(ApiInvalidRequest),
            FreshRetry
        );
        assert_eq!(
            RetryService::retry_disposition(SemanticInactivity),
            FreshRetry
        );
        assert_eq!(RetryService::retry_disposition(AgentError), NoRetry);
        assert_eq!(RetryService::retry_disposition(UserCancel), NoRetry);
        assert_eq!(RetryService::retry_disposition(Timeout), NoRetry);
        assert_eq!(RetryService::retry_disposition(Unknown), NoRetry);
    }
}
