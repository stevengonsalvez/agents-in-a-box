//! Spine A5 / T4: the ACP delivery leg -> `RunOutcome` mapping, table-driven.
//!
//! No daemon, no adapter, no database: `outcome_for` is the pure function that
//! decides whether an ACP run finalizes `done`, `failed` or `cancelled`, and
//! whether it is retried, so it is worth pinning on its own.
//!
//! ```text
//!   leg.state + leg.detail (a "; "-joined TOKEN SET)
//!            │
//!            ▼
//!   RunOutcome ──▶ finalize_success / finalize_failure / finalize_cancelled
//!                  RetryService::retry_disposition
//! ```
//!
//! Two properties matter more than any single row.
//!
//! **The detail is a SET, never a string.** A plain success can read
//! `resume=loaded`, a budget stop `stop=max_tokens; resume=reprimed`, and an
//! adapter exit appends raw error text after its token. Every equality-based
//! reader of this field would mis-read at least one of those.
//!
//! **An unmapped detail is contract DRIFT, not success.** A token the pool grew
//! and this table has not learned about must not finalize a task `done`.

use ainb_hangar_daemon::acp_task::outcome_for;
use ainb_hangar_daemon::runner::{RunOutcome, RunnerResult};
use ainb_hangar_store::service::fail::FailureReason;
use ainb_hangar_store::service::retry::RetryService;

/// The shorthand each row asserts against.
#[derive(Debug, PartialEq, Eq)]
enum Want {
    Success,
    Cancelled,
    Failed(FailureReason),
}

const fn classify(outcome: &RunOutcome) -> Want {
    match outcome {
        RunOutcome::Success(_) => Want::Success,
        RunOutcome::Cancelled(_) => Want::Cancelled,
        RunOutcome::Failed { reason, .. } => Want::Failed(*reason),
    }
}

#[test]
fn every_leg_the_pool_can_write_maps_to_a_named_outcome() {
    // (state, detail, expected). Mirrors the table in
    // `docs/hangar/renovation/move1-acp-tasks.md` section 2f.
    let table: &[(&str, Option<&str>, Want)] = &[
        // DELIVERED: no `stop=` token IS the ordinary EndTurn. The pool writes
        // the token only when the reason is worth naming.
        ("DELIVERED", None, Want::Success),
        ("DELIVERED", Some("resume=loaded"), Want::Success),
        ("DELIVERED", Some("resume=reprimed"), Want::Success),
        (
            "DELIVERED",
            Some("stop=max_tokens"),
            Want::Failed(FailureReason::IterationLimit),
        ),
        (
            "DELIVERED",
            Some("stop=max_tokens; resume=reprimed"),
            Want::Failed(FailureReason::IterationLimit),
        ),
        (
            "DELIVERED",
            Some("stop=max_turn_requests"),
            Want::Failed(FailureReason::IterationLimit),
        ),
        // FAILED.
        (
            "FAILED",
            Some("turn_failed; stop=refusal"),
            Want::Failed(FailureReason::AgentError),
        ),
        (
            "FAILED",
            Some("turn_failed; stop=cancelled"),
            Want::Cancelled,
        ),
        (
            "FAILED",
            Some("turn_failed; stop=cancelled; resume=loaded"),
            Want::Cancelled,
        ),
        ("FAILED", Some("operator_stop"), Want::Cancelled),
        (
            "FAILED",
            Some("spawn_failed"),
            Want::Failed(FailureReason::SpawnError),
        ),
        (
            "FAILED",
            Some("mode_unproven"),
            Want::Failed(FailureReason::SpawnError),
        ),
        (
            "FAILED",
            Some("turn_unrecorded"),
            Want::Failed(FailureReason::SpawnError),
        ),
        (
            "FAILED",
            Some("session_gone"),
            Want::Failed(FailureReason::ProvisionError),
        ),
        (
            "FAILED",
            Some("breaker_open"),
            Want::Failed(FailureReason::RuntimeOffline),
        ),
        (
            "FAILED",
            Some("provider_at_capacity"),
            Want::Failed(FailureReason::RuntimeOffline),
        ),
        (
            "FAILED",
            Some("queue_full"),
            Want::Failed(FailureReason::RuntimeOffline),
        ),
        // UNKNOWN: the request WAS issued, so the outcome is genuinely unknown.
        // `adapter_exit` carries the raw error text after its token, which is
        // exactly why the detail is read as a set.
        (
            "UNKNOWN",
            Some("adapter_exit; transport closed while a turn was open"),
            Want::Failed(FailureReason::RuntimeOffline),
        ),
        (
            "UNKNOWN",
            Some("turn_deadline"),
            Want::Failed(FailureReason::Timeout),
        ),
        (
            "UNKNOWN",
            Some("daemon_restart"),
            Want::Failed(FailureReason::RuntimeRecovery),
        ),
        // Refused at the door: nothing reached the adapter.
        (
            "REJECTED",
            Some("queue_full"),
            Want::Failed(FailureReason::SpawnError),
        ),
        ("REJECTED", None, Want::Failed(FailureReason::SpawnError)),
    ];

    for (state, detail, want) in table {
        let got = outcome_for(state, *detail, RunnerResult::default());
        assert_eq!(
            &classify(&got),
            want,
            "leg state={state:?} detail={detail:?} mapped to {got:?}"
        );
    }
}

#[test]
fn an_unmapped_detail_is_contract_drift_and_never_success() {
    // The pool's taxonomy growing a token this table has not learned about must
    // fail the task loudly, not finalize it `done` on a turn nobody read. Fail
    // closed, exactly as the runner's own unknown-terminal rule does.
    let drifted: &[(&str, Option<&str>)] = &[
        // A stop reason upstream added, or renamed.
        ("DELIVERED", Some("stop=quota_exhausted")),
        // `refusal` on a DELIVERED leg contradicts `turn_succeeded`.
        ("DELIVERED", Some("stop=refusal")),
        // A FAILED leg carrying nothing at all.
        ("FAILED", None),
        ("FAILED", Some("something_new")),
        ("FAILED", Some("turn_failed")),
        ("UNKNOWN", Some("something_new")),
        // A state the delivery CHECK constraint does not even allow.
        ("PENDING", None),
        ("MISSING", None),
    ];
    for (state, detail) in drifted {
        let got = outcome_for(state, *detail, RunnerResult::default());
        assert_eq!(
            classify(&got),
            Want::Failed(FailureReason::ProviderContractDrift),
            "leg state={state:?} detail={detail:?} should be contract drift"
        );
    }
}

#[test]
fn the_retry_disposition_of_each_outcome_is_the_one_the_plan_promised() {
    // The mapping is only half the contract: which failures COME BACK is the
    // other half, and it is decided elsewhere. A row that mapped to a plausible
    // reason with the wrong disposition would either wedge a card in Todo or
    // retry a refusal forever.
    use ainb_hangar_store::service::retry::RetryDisposition;

    let cases: &[(&str, Option<&str>, RetryDisposition)] = &[
        // The agent ran out of budget: a fresh run has a chance.
        (
            "DELIVERED",
            Some("stop=max_tokens"),
            RetryDisposition::FreshRetry,
        ),
        // The agent refused: running it again refuses again.
        (
            "FAILED",
            Some("turn_failed; stop=refusal"),
            RetryDisposition::NoRetry,
        ),
        // The provider is down, not the work: come back to the same session.
        (
            "FAILED",
            Some("breaker_open"),
            RetryDisposition::ResumeRetry,
        ),
        (
            "UNKNOWN",
            Some("adapter_exit; pipe closed"),
            RetryDisposition::ResumeRetry,
        ),
        (
            "UNKNOWN",
            Some("daemon_restart"),
            RetryDisposition::ResumeRetry,
        ),
        // A misconfigured adapter will not self-heal on a re-dispatch.
        ("REJECTED", None, RetryDisposition::NoRetry),
        ("FAILED", Some("mode_unproven"), RetryDisposition::NoRetry),
        // Drift is a bug in this table, not a transient fault.
        ("FAILED", Some("something_new"), RetryDisposition::NoRetry),
    ];

    for (state, detail, want) in cases {
        let RunOutcome::Failed { reason, .. } =
            outcome_for(state, *detail, RunnerResult::default())
        else {
            panic!("state={state:?} detail={detail:?} should be a failure");
        };
        assert_eq!(
            RetryService::retry_disposition(reason),
            *want,
            "state={state:?} detail={detail:?} reason={reason:?}"
        );
    }
}
