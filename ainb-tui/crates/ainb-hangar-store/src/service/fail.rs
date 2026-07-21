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
/// Serializes to the `snake_case` tokens the reference's `failure_reason` column uses,
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
/// conversation would only re-fail, mirroring the reference's `GetLastTaskSession`
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
    /// The provider subprocess could not be spawned or its OS-level execution
    /// faulted — e.g. the configured `claude` / `codex` path does not resolve
    /// (ENOENT). Distinct from [`Self::AgentError`] (the agent ran and gave up):
    /// here the agent never started. Terminal (no retry): a misconfigured binary
    /// path will not self-heal on a re-dispatch with the same daemon config.
    SpawnError,
    /// The provider's structured event contract drifted from the pinned CLI
    /// shape: the runner ran a provider under its structured-output flag
    /// (claude `--output-format stream-json`, codex `exec --json`) but the stream
    /// carried NO recognised success/error terminal — an unknown non-error
    /// `result` subtype, or no terminal event at all despite a clean exit. This is
    /// held DISTINCT from [`Self::AgentError`] on purpose: it means "a future CLI
    /// renamed / added a terminal shape the parser does not know", NOT "the agent
    /// genuinely failed", so an operator can tell a parser-update need apart from a
    /// real agent give-up. Fail-closed (never silently `done` over a shape we
    /// cannot read) and terminal (no retry): the same CLI version re-drifts
    /// identically, so a retry only burns the chain — the fix is updating the
    /// parser, not re-running.
    ProviderContractDrift,
    /// The run could not be SET UP before the agent started — the pre-run
    /// provisioning failed (e.g. the card's `repo_ref` could not be cloned /
    /// worktree-added, or the isolated execenv could not be prepared). Distinct
    /// from [`Self::SpawnError`] (the provider binary itself is missing): here the
    /// working directory the agent needs never materialised, so no provider was
    /// even reached. Terminal (no retry): a provisioning fault is observed by the
    /// daemon deterministically (a bad repo path does not self-heal on a
    /// re-dispatch), so failing the row immediately with the real error beats the
    /// alternative it replaces — the row stranded `dispatched`, reclaimed past the
    /// 90s window, re-dispatched, and re-failing invisibly until the 5min dispatch
    /// TTL relabelled it `timeout` with no cause recorded.
    ProvisionError,
    /// The `running -> provider spawn` setup phase WEDGED past its umbrella bound
    /// (`SPAWN_SETUP_TIMEOUT`) — a step between marking the row `running` and
    /// spawning the provider (e.g. a headless keychain read that never returns, a
    /// pool deadlock, a materialise hang) blocked the run indefinitely. Distinct
    /// from [`Self::SpawnError`] (the provider binary is missing / unspawnable —
    /// the setup finished but the OS exec failed) and [`Self::Timeout`] (a TTL
    /// sweeper relabelled a stalled row with no cause): here the daemon caught the
    /// wedge itself, at the bound, and recorded WHY — so a forever-`running` black
    /// hole becomes a loud, immediate terminal. Terminal (no retry): a wedged
    /// environment does not self-heal on a re-dispatch with the same daemon.
    SpawnTimeout,
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
            Self::SpawnError => "spawn_error",
            Self::ProviderContractDrift => "provider_contract_drift",
            Self::ProvisionError => "provision_error",
            Self::SpawnTimeout => "spawn_timeout",
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

    /// Terminalise a task that faulted during PRE-RUN setup: `dispatched ->
    /// failed`, recording `reason`, a human-readable `message` (persisted into the
    /// `result` column so the task-detail surface shows WHY), and
    /// `finished_at = clock.now_ms()`.
    ///
    /// The daemon claims a task (`queued -> dispatched`) and then provisions its
    /// working directory BEFORE the `dispatched -> running` start transition. A
    /// fault in that window (a `repo_ref` that cannot be cloned, an execenv that
    /// cannot be prepared) leaves the row `dispatched` — a state [`Self::fail`]
    /// deliberately rejects (its source set is `running` / `queued`, since the
    /// stale-dispatch sweeper owns the *timeout* path). Without a dedicated seam
    /// such a fault propagated out of the run loop with the row still `dispatched`,
    /// so the sweeper reclaimed it past the 90s window and re-dispatched it into
    /// the same fault, looping invisibly. This seam finalises it AT ONCE, from
    /// `dispatched`, so the failure is terminal and visible.
    ///
    /// The `message` rides the `result` column as `{"content": message}` — the
    /// same [`TaskResult`](ainb_hangar_core::result::TaskResult) shape a completed
    /// run writes — so the existing detail rendering surfaces the setup error with
    /// no special-casing.
    ///
    /// Idempotent, like [`Self::fail`]: a replayed call on an already-`failed` row
    /// returns [`FinalizeOutcome::AlreadyTerminal`]; a row that a concurrent cancel
    /// won (`dispatched -> cancelled`) returns [`FinalizeError::TerminalMismatch`]
    /// so the caller can honour the cancel.
    ///
    /// # Errors
    ///
    /// - [`FinalizeError::TerminalMismatch`] if the row is already `done` /
    ///   `cancelled`.
    /// - [`FinalizeError::IllegalState`] if the row is not `dispatched` (e.g. it
    ///   already started, or the row is absent).
    /// - [`FinalizeError::Db`] on an underlying database error.
    #[tracing::instrument(
        name = "task.fail_setup",
        skip(pool, message, clock),
        fields(task_id = %task_id, failure_reason = reason.as_db_str())
    )]
    pub async fn fail_setup(
        pool: &SqlitePool,
        task_id: &str,
        reason: FailureReason,
        message: &str,
        clock: &dyn HangarClock,
    ) -> Result<FinalizeOutcome, FinalizeError> {
        let now = clock.now_ms();
        let reason_str = reason.as_db_str();
        // Persist the real error into `result` in the TaskResult shape so the
        // task-detail surface renders it (a killed/no-work run's `content`-only
        // JSON is a legal, round-tripping TaskResult).
        let result_json = serde_json::json!({ "content": message }).to_string();
        finalize_idempotent(
            pool,
            task_id,
            TaskState::Failed,
            &[TaskState::Dispatched],
            "UPDATE agent_task_queue \
             SET status = 'failed', failure_reason = ?1, result = ?2, finished_at = ?3 \
             WHERE id = ?4 AND status = 'dispatched'",
            move |q| q.bind(reason_str).bind(result_json).bind(now).bind(task_id),
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
            FailureReason::SpawnError,
            FailureReason::ProviderContractDrift,
            FailureReason::ProvisionError,
            FailureReason::SpawnTimeout,
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
