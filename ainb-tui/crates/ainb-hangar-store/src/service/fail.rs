//! The `FailTask` service: `running -> failed` (and `queued -> failed` via TTL).
//!
//! [`FailTaskService::fail`] records a [`FailureReason`], stamps `finished_at`,
//! and flips the row to `failed`. Like the other finalize services it is
//! idempotent: a replayed fail of an already-`failed` row returns
//! [`FinalizeOutcome::AlreadyTerminal`]; a row that lost to `done` / `cancelled`
//! returns [`FinalizeError::TerminalMismatch`].
//!
//! `running -> failed` is the runner's path; `queued -> failed` is the P1.4
//! queued-TTL sweeper's path. Both are legal source states, so the failure
//! service accepts either.

use ainb_hangar_core::clock::HangarClock;
use ainb_hangar_core::task::state::TaskState;
use serde::Serialize;
use sqlx::SqlitePool;

use super::finalize::{FinalizeError, FinalizeOutcome, finalize_idempotent};

/// Why a task failed.
///
/// Serializes to the `snake_case` tokens Multica's `failure_reason` column uses,
/// so the stored value is wire-compatible. The initial set is the six P1.3
/// reasons; new code paths in P5+ may extend it (forbid wildcard match arms per
/// `reference_gated_by_variant_propagation`).
///
/// # Retry disposition
///
/// The reason drives the retry/resume taxonomy
/// ([`RetryService::retry_disposition`](crate::service::retry::RetryService::retry_disposition)):
/// infra failures ([`Self::RuntimeOffline`] / [`Self::RuntimeRecovery`]) resume
/// the session on retry, the conversation-poisoning terminals
/// ([`Self::IterationLimit`] / [`Self::ApiInvalidRequest`] /
/// [`Self::SemanticInactivity`]) retry FRESH (a new session — resuming a wedged
/// conversation would only re-fail, mirroring Multica's `GetLastTaskSession`
/// exclusion set), and the rest do not retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureReason {
    /// A TTL sweeper expired the task (queued / dispatched / running TTL).
    Timeout,
    /// The agent subprocess errored or gave up (user-facing failure).
    AgentError,
    /// The runtime hosting the agent went offline mid-run.
    RuntimeOffline,
    /// The task failed during daemon recovery (e.g. `recover-orphans`).
    RuntimeRecovery,
    /// A human cancelled the run (terminal by intent; not retried).
    UserCancel,
    /// The agent exhausted its per-run iteration budget without finishing — a
    /// conversation-poisoning terminal: the model wedged on the same context, so
    /// resuming it would only re-fail. Retried only as a *fresh* session.
    IterationLimit,
    /// The provider rejected the request as malformed (e.g. an Anthropic 400 /
    /// `invalid_request_error`), typically a context the conversation drove into
    /// an unrecoverable shape — conversation-poisoning, retried *fresh*.
    ApiInvalidRequest,
    /// The run stalled with no semantic progress (no new tool calls / output) —
    /// conversation-poisoning, retried *fresh* rather than resuming the stall.
    SemanticInactivity,
    /// An unclassified failure.
    Unknown,
}

impl FailureReason {
    /// The `snake_case` token persisted to `agent_task_queue.failure_reason`.
    ///
    /// Kept byte-identical to the [`Serialize`] `rename_all = "snake_case"`
    /// derive (the `as_db_str_matches_serde_for_all_variants` test guards the
    /// two against drift), but implemented as a direct `match` so the hot
    /// persist path needs no allocation or serializer.
    #[must_use]
    pub const fn as_db_str(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::AgentError => "agent_error",
            Self::RuntimeOffline => "runtime_offline",
            Self::RuntimeRecovery => "runtime_recovery",
            Self::UserCancel => "user_cancel",
            Self::IterationLimit => "iteration_limit",
            Self::ApiInvalidRequest => "api_invalid_request",
            Self::SemanticInactivity => "semantic_inactivity",
            Self::Unknown => "unknown",
        }
    }
}

/// Stateless `{running|queued} -> failed` service over `agent_task_queue`.
pub struct FailTaskService;

impl FailTaskService {
    /// Transition `task_id` to `failed`, recording `reason` and
    /// `finished_at = clock.now_ms()`. Legal source states are `running`
    /// (runner failure) and `queued` (queued-TTL sweep).
    ///
    /// # Errors
    ///
    /// - [`FinalizeError::TerminalMismatch`] if the row is already `done` /
    ///   `cancelled`.
    /// - [`FinalizeError::IllegalState`] if the row is `dispatched` or absent.
    /// - [`FinalizeError::Db`] on an underlying database error.
    #[tracing::instrument(
        name = "task.fail",
        skip(pool, clock),
        fields(task_id = %task_id, failure_reason = reason.as_db_str())
    )]
    pub async fn fail(
        pool: &SqlitePool,
        task_id: &str,
        reason: FailureReason,
        clock: &dyn HangarClock,
    ) -> Result<FinalizeOutcome, FinalizeError> {
        let now = clock.now_ms();
        let reason_str = reason.as_db_str();
        finalize_idempotent(
            pool,
            task_id,
            TaskState::Failed,
            &[TaskState::Running, TaskState::Queued],
            "UPDATE agent_task_queue \
             SET status = 'failed', failure_reason = ?1, finished_at = ?2 \
             WHERE id = ?3 AND status IN ('running','queued')",
            move |q| q.bind(reason_str).bind(now).bind(task_id),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::FailureReason;

    /// `as_db_str` must stay byte-identical to the `Serialize` `snake_case`
    /// derive for every variant, so the column and the wire value never drift.
    #[test]
    fn as_db_str_matches_serde_for_all_variants() {
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
            let serde_token = serde_json::to_value(reason)
                .expect("serialize")
                .as_str()
                .expect("string token")
                .to_string();
            assert_eq!(reason.as_db_str(), serde_token);
        }
    }
}
