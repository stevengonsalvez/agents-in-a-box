//! tcp T4 (agents-in-a-box-aau.4) — the F7 CARD-DEPENDENCY chain tripwire.
//!
//! A card can `depend-on` another. This tripwire proves the acceptance end-to-end
//! against the REAL daemon binary + claim loop: card B depends-on card A, so B
//! REFUSES to run while A is unfinished; the moment A completes, B becomes runnable
//! and — because B opted into auto-run — the finalize seam AUTO-LAUNCHES it.
//!
//! ```text
//!  board_card_run(B) ──▶ REFUSED ("blocked by unfinished cards")   [A not done]
//!         │
//!  board_card_run(A) ──▶ A runs ──release──▶ A done ──▶ finalize seam
//!         │                                                 │
//!         ▼                                                 ▼
//!   B has NO task yet                        unblock_dependents: B runnable
//!                                            + auto_run on ──▶ B auto-launched
//! ```
//!
//! Drives the daemon directly (a framed socket RPC). The REFUSE proof is a pure
//! run-handler behaviour; the auto-run proof needs the claim loop + the finalize
//! seam, so a real claim-enabled daemon runs A to `done`. The NEGATIVE (B has no
//! task before A completes) is asserted at refuse time, so B gaining a task can
//! only be the auto-run firing. SKIPs cleanly when the daemon binary / git are
//! absent. Exact-name kills only (the `Pipeline` owns its one child).

use std::time::{Duration, Instant};

#[path = "tripwire_p4_common.rs"]
mod common;
use common::{
    BOARD_RUN_BOARD, DaemonRpc, INTERACTIVE_RELEASE_SENTINEL, T4_DEP_BLOCKER_ISSUE,
    T4_DEP_DEPENDENT_ISSUE, budget_scale, daemon_bin, git_available, latest_task_status_for_issue,
    prepare_pipeline_dep_chain, skip, task_count_for_issue,
};

#[test]
fn dependent_card_refuses_until_blocker_done_then_auto_runs() {
    if daemon_bin().is_none() || !git_available() {
        skip("tcp_card_dependency_chain_e2e");
        return;
    }

    let pipe = prepare_pipeline_dep_chain();
    let scale = budget_scale();
    let mut rpc = DaemonRpc::connect_and_auth(pipe.home());

    // REFUSE: B depends-on A, and A has not run, so running B is refused with the
    // F7 blocked message — and B is NOT dispatched (no task row).
    let refused = rpc.call(
        ainb_hangar_proto::methods::HANGAR_BOARD_CARD_RUN,
        run_params(T4_DEP_DEPENDENT_ISSUE),
    );
    assert!(!refused["error"].is_null(), "running a blocked card must be refused: {refused}");
    let msg = refused["error"]["message"].as_str().unwrap_or("");
    assert!(
        msg.contains("blocked"),
        "the refusal must name the block ({msg:?}): {refused}"
    );
    // NEGATIVE: the refused run enqueued nothing for B.
    assert_eq!(
        task_count_for_issue(pipe.home(), T4_DEP_DEPENDENT_ISSUE),
        0,
        "a blocked card must not be dispatched"
    );

    // COMPLETE A: A has no blockers, so it runs. The blocking fake-claude holds it
    // until we release the sentinel; then A finalizes to `done`.
    let run_a = rpc.call(
        ainb_hangar_proto::methods::HANGAR_BOARD_CARD_RUN,
        run_params(T4_DEP_BLOCKER_ISSUE),
    );
    assert!(run_a["error"].is_null(), "the unblocked blocker A must run: {run_a}");

    // Wait until A is actually running (claimed) before releasing, so the release
    // can never precede the claim.
    let claimed_deadline = Instant::now() + Duration::from_secs(30 * scale);
    let a_claimed = poll_until(claimed_deadline, || {
        matches!(
            latest_task_status_for_issue(pipe.home(), T4_DEP_BLOCKER_ISSUE).as_deref(),
            Some("dispatched" | "running")
        )
    });
    assert!(a_claimed, "the claim loop must pick up A before we release it");

    // B still has no task — nothing has unblocked it yet.
    assert_eq!(
        task_count_for_issue(pipe.home(), T4_DEP_DEPENDENT_ISSUE),
        0,
        "B must gain no task until A completes"
    );

    // Release: A finalizes to `done`, the finalize seam re-evaluates B (its last
    // blocker is now done) and — auto_run on — AUTO-LAUNCHES it. The same sentinel
    // is now present, so B's auto-run also completes.
    std::fs::write(pipe.home().join(INTERACTIVE_RELEASE_SENTINEL), "go").expect("write release sentinel");

    // AUTO-RUN: B gains a task ONLY after A completes — the finalize auto-run fired.
    let autorun_deadline = Instant::now() + Duration::from_secs(45 * scale);
    let b_ran = poll_until(autorun_deadline, || {
        task_count_for_issue(pipe.home(), T4_DEP_DEPENDENT_ISSUE) >= 1
    });

    // A must have reached `done` (the trigger for B's auto-run).
    let a_done = poll_until(Instant::now() + Duration::from_secs(15 * scale), || {
        latest_task_status_for_issue(pipe.home(), T4_DEP_BLOCKER_ISSUE).as_deref() == Some("done")
    });

    drop(rpc);
    drop(pipe);

    assert!(a_done, "the blocker A must finalize to done");
    assert!(
        b_ran,
        "B must auto-run once its last blocker (A) completed (auto_run flag on)"
    );
}

/// The `board_card_run` params for a card on the fixture board (headless).
fn run_params(issue_id: &str) -> serde_json::Value {
    serde_json::json!({
        "workspace_id": ainb_hangar_daemon::seed::WS_SLUG,
        "board_id": BOARD_RUN_BOARD,
        "issue_id": issue_id,
        "mode": "headless",
    })
}

/// Poll `pred` every 150ms until it holds or `deadline` passes.
fn poll_until(deadline: Instant, pred: impl Fn() -> bool) -> bool {
    loop {
        if pred() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}
