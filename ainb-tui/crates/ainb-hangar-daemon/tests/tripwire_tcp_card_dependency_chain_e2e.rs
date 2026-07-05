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
    newest_active_task_for_issue, prepare_pipeline_dep_chain, prepare_pipeline_squad_dep_chain,
    skip, task_count_for_issue, task_status_by_id,
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

/// tcp T4 / FANOUT-SEMANTICS — a MANUAL Kanban move to `done` fires the dependency
/// auto-run, exactly like the finalize seam.
///
/// The finalize-driven path (above) auto-runs a dependent when its blocker completes
/// through the claim loop. But a human can also complete a card by dragging it to the
/// Done column (`hangar/task_transition`), and that path used to NOT re-evaluate
/// dependents — so an auto-run dependent silently never fired. This proves the fix:
/// running A, then MANUALLY transitioning A's task to `done` (never releasing the
/// sentinel), auto-launches B.
#[test]
fn manual_done_transition_auto_runs_dependent() {
    if daemon_bin().is_none() || !git_available() {
        skip("tcp_card_dependency_manual_done_e2e");
        return;
    }

    let pipe = prepare_pipeline_dep_chain();
    let scale = budget_scale();
    let mut rpc = DaemonRpc::connect_and_auth(pipe.home());

    // Run A (single-agent blocker); the run result carries A's task id.
    let run_a = rpc.call(ainb_hangar_proto::methods::HANGAR_BOARD_CARD_RUN, run_params(T4_DEP_BLOCKER_ISSUE));
    assert!(run_a["error"].is_null(), "blocker A must run: {run_a}");
    let a_task = run_a["result"]["task_id"].as_str().unwrap_or("").to_string();
    assert!(!a_task.is_empty(), "A's run must carry a task id: {run_a}");

    // B has no task yet — A is not done.
    assert_eq!(task_count_for_issue(pipe.home(), T4_DEP_DEPENDENT_ISSUE), 0, "B blocked until A done");

    // MANUALLY move A's task to `done` (a hand-drag on the Kanban), NOT via the
    // sentinel-driven finalize. This is the path that used to skip the unblock hook.
    let done = rpc.call(
        ainb_hangar_proto::methods::HANGAR_TASK_TRANSITION,
        serde_json::json!({
            "workspace_id": ainb_hangar_daemon::seed::WS_SLUG,
            "task_id": a_task,
            "to_status": "done",
        }),
    );
    assert!(done["error"].is_null(), "manual done transition must ack: {done}");
    assert_eq!(
        task_status_by_id(pipe.home(), &a_task).as_deref(),
        Some("done"),
        "A's task must be done after the manual move"
    );

    // B must AUTO-RUN off the manual completion — it gains a task.
    let autorun_deadline = Instant::now() + Duration::from_secs(45 * scale);
    let b_ran = poll_until(autorun_deadline, || {
        task_count_for_issue(pipe.home(), T4_DEP_DEPENDENT_ISSUE) >= 1
    });

    // Release so the blocked runs exit promptly, then kill the daemon by its handle.
    std::fs::write(pipe.home().join(INTERACTIVE_RELEASE_SENTINEL), "go").expect("write release sentinel");
    drop(rpc);
    drop(pipe);

    assert!(b_ran, "a MANUAL `done` on A must fire B's dependency auto-run");
}

/// tcp T4 / FANOUT-SEMANTICS — a dependent waits for the blocker's WHOLE squad, not
/// just its latest member.
///
/// Blocker A is a SQUAD card (leader + two members → three tasks on one issue). The
/// old logic keyed "A finished" on A's LATEST task, so B unblocked the instant the
/// newest member finished while the leader / other member still ran. This proves the
/// whole-set contract against the REAL daemon: transitioning A's NEWEST active task
/// to `done` while OLDER siblings still run leaves B blocked (latest-wins would have
/// unblocked it there); only when A's ENTIRE set has drained with a success does B
/// become runnable and auto-run.
#[test]
fn dependent_waits_for_the_whole_squad_blocker() {
    if daemon_bin().is_none() || !git_available() {
        skip("tcp_card_dependency_whole_squad_e2e");
        return;
    }

    let pipe = prepare_pipeline_squad_dep_chain();
    let scale = budget_scale();
    let mut rpc = DaemonRpc::connect_and_auth(pipe.home());

    // Fan A out; assign_fanout inserts all three tasks before the RPC returns, so A
    // now carries three active tasks on one issue.
    let run_a = rpc.call(ainb_hangar_proto::methods::HANGAR_BOARD_CARD_RUN, run_params(T4_DEP_BLOCKER_ISSUE));
    assert!(run_a["error"].is_null(), "the squad blocker A must run: {run_a}");
    let member_count = run_a["result"]["member_task_ids"].as_array().map_or(0, Vec::len);
    assert_eq!(member_count, 2, "A must fan out to a leader + two members: {run_a}");

    // B has no task — A's set has not drained.
    assert_eq!(task_count_for_issue(pipe.home(), T4_DEP_DEPENDENT_ISSUE), 0, "B blocked while A runs");

    // Transition A's NEWEST active task to `done` while OLDER siblings still run.
    let newest = newest_active_task_for_issue(pipe.home(), T4_DEP_BLOCKER_ISSUE)
        .expect("A must have an active task to complete");
    let done = rpc.call(
        ainb_hangar_proto::methods::HANGAR_TASK_TRANSITION,
        serde_json::json!({
            "workspace_id": ainb_hangar_daemon::seed::WS_SLUG,
            "task_id": newest,
            "to_status": "done",
        }),
    );
    assert!(done["error"].is_null(), "completing the newest member must ack: {done}");

    // KEY DISCRIMINATOR: the newest member is done but older siblings still run — the
    // old latest-wins logic would have unblocked B here. The whole-set contract must
    // keep it blocked. The unblock hook fired synchronously inside the RPC above, so
    // if B were going to gain a task it already would have.
    assert_eq!(
        task_count_for_issue(pipe.home(), T4_DEP_DEPENDENT_ISSUE),
        0,
        "B must stay blocked while ANY squad member of A still runs (newest-done is not enough)"
    );

    // Drain the rest of A's set to `done`, newest-first, until nothing is active.
    while let Some(t) = newest_active_task_for_issue(pipe.home(), T4_DEP_BLOCKER_ISSUE) {
        let d = rpc.call(
            ainb_hangar_proto::methods::HANGAR_TASK_TRANSITION,
            serde_json::json!({
                "workspace_id": ainb_hangar_daemon::seed::WS_SLUG,
                "task_id": t,
                "to_status": "done",
            }),
        );
        assert!(d["error"].is_null(), "draining a squad member must ack: {d}");
    }

    // A's WHOLE set has now drained with a success → B becomes runnable and auto-runs.
    let autorun_deadline = Instant::now() + Duration::from_secs(45 * scale);
    let b_ran = poll_until(autorun_deadline, || {
        task_count_for_issue(pipe.home(), T4_DEP_DEPENDENT_ISSUE) >= 1
    });

    std::fs::write(pipe.home().join(INTERACTIVE_RELEASE_SENTINEL), "go").expect("write release sentinel");
    drop(rpc);
    drop(pipe);

    assert!(b_ran, "B must auto-run only once A's ENTIRE squad set has drained with a done");
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
