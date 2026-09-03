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

/// EVERY delivery token the pool can write, under EVERY terminal state that can
/// carry it, resolves to something other than contract drift.
///
/// The hand-written table below cannot do this job: it lists the pairs someone
/// thought of, and the four it originally missed (`FAILED`+`adapter_exit`,
/// `FAILED`+`turn_deadline`, `FAILED`+`daemon_restart`,
/// `UNKNOWN`+`operator_stop`) were all reachable and all answered
/// `ProviderContractDrift`/NoRetry.
///
/// The vocabulary is read from [`DeliveryToken::ALL`], not copied into a list
/// here. An earlier version of this test DID copy it, which made the guarantee
/// it claimed to enforce imaginary: a reviewer added a real
/// `DELIVERY_RATE_LIMITED` write site to the pool and all four tests still
/// passed. `DeliveryToken`'s exhaustive `as_str` and `outcome_for_token`'s
/// wildcard-free match are what now make that a compile error; this test is the
/// runtime half, covering the states and the `resume=` suffix.
#[test]
fn no_state_and_token_the_pool_can_write_falls_through_to_drift() {
    use ainb_hangar_daemon::acp_pool::{ConvergeCause, DeliveryToken};

    // Every cause's token too. `detail()` still returns `&str`, so a new cause
    // whose token bypassed the enum would fail to parse and land on drift here.
    let tokens: Vec<String> = DeliveryToken::ALL
        .iter()
        .map(|token| token.as_str().to_string())
        .chain(
            [
                ConvergeCause::DaemonRestart,
                ConvergeCause::AdapterExit,
                ConvergeCause::TurnDeadline,
                ConvergeCause::OperatorStop,
            ]
            .into_iter()
            .map(|cause| cause.detail().to_string()),
        )
        .collect();

    // `drain_queue` resolves FAILED, `converge_dirty_session` UNKNOWN, and
    // `submit_prompt` REJECTED: all three from the same vocabulary.
    for state in ["FAILED", "UNKNOWN", "REJECTED"] {
        for token in &tokens {
            // `finish_turn` never writes `turn_failed` without a stop reason,
            // and the stop reason is what distinguishes a refusal from a
            // cancellation, so a bare one is genuinely drift.
            let base = if token == DeliveryToken::TurnFailed.as_str() {
                format!("{token}; stop=refusal")
            } else {
                token.clone()
            };
            for detail in [base.clone(), format!("{base}; resume=loaded")] {
                let got = outcome_for(state, Some(&detail), RunnerResult::default());
                assert_ne!(
                    classify(&got),
                    Want::Failed(FailureReason::ProviderContractDrift),
                    "state={state:?} detail={detail:?} has no mapping"
                );
                assert_ne!(
                    classify(&got),
                    Want::Success,
                    "state={state:?} detail={detail:?} must never read as success"
                );
            }
        }
    }
}

#[test]
fn every_leg_the_pool_can_write_maps_to_a_named_outcome() {
    // (state, detail, expected). Mirrors the table in
    // `docs/hangar/renovation/move1-acp-tasks.md` section 2f, plus the four
    // pairs 2f's own table omitted (marked below).
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
        // The four 2f's table omitted, all reachable, all previously drift.
        // `attach_with_one_requeue` writes adapter_exit on a FAILED leg when the
        // adapter cannot be spawned or the transport dies: the single most
        // likely ACP failure, and NoRetry would have burned the chain on it.
        (
            "FAILED",
            Some("adapter_exit; spawn refused"),
            Want::Failed(FailureReason::RuntimeOffline),
        ),
        // `drain_queue` resolves a still-QUEUED prompt FAILED with the converge
        // cause, so these two arrive under FAILED as well as UNKNOWN.
        (
            "FAILED",
            Some("turn_deadline"),
            Want::Failed(FailureReason::Timeout),
        ),
        (
            "FAILED",
            Some("daemon_restart"),
            Want::Failed(FailureReason::RuntimeRecovery),
        ),
        // An operator stopping the session while `await_leg` still polls: the
        // run is cancelled, not failed, so it neither retries nor moves the card
        // to a failed column.
        ("UNKNOWN", Some("operator_stop"), Want::Cancelled),
        // Refused at the door. The refusal token decides, exactly as it does on
        // a FAILED leg: a `breaker_open` door refusal is the same transient
        // condition whichever side of the queue it was seen from. Deliberate
        // deviation from 2f's "REJECTED | any | SpawnError".
        (
            "REJECTED",
            Some("queue_full"),
            Want::Failed(FailureReason::RuntimeOffline),
        ),
        (
            "REJECTED",
            Some("breaker_open"),
            Want::Failed(FailureReason::RuntimeOffline),
        ),
        (
            "REJECTED",
            Some("session_gone"),
            Want::Failed(FailureReason::ProvisionError),
        ),
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
        // Refused at the door with no reason: `submit_prompt` always names one,
        // so an empty refusal is the pool's contract changing.
        ("REJECTED", None),
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
        // The adapter would not start. THE most likely ACP failure, and the one
        // NoRetry would have burned the retry chain on.
        (
            "FAILED",
            Some("adapter_exit; spawn refused"),
            RetryDisposition::ResumeRetry,
        ),
        // Refused at the door for a transient reason: the door is not what
        // makes it terminal.
        (
            "REJECTED",
            Some("breaker_open"),
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
