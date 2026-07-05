//! The board auto-move dispatch hook (P4 / D8).
//!
//! When a claimed task walks its FSM (`dispatched → running → done | failed`),
//! the claim loop calls [`auto_move_after_transition`] with the new status. For
//! the task's issue, every board carrying it as a card moves the card to the
//! column whose `fsm_state` matches — but only when both the board's master
//! `auto_move` toggle and the target column's `auto_move` flag are on
//! ([`BoardRepo::auto_move_on_state`]). This is the D8 "column↔FSM-state mapping +
//! per-board auto-move toggle" made real: a card slides to `Done` (green) the
//! moment its work succeeds, without the user dragging anything.
//!
//! The TUI reflects the move on its next `hangar/boards_list` pull (which it
//! triggers off the already-emitted `TaskStarted` / `TaskFinished` events), so no
//! new event type is needed here — the hook's whole job is the durable card move.
//!
//! # Best-effort
//!
//! Every fault (a chat task with no issue, a malformed workspace id, a store
//! error) is logged and swallowed: the task's terminal FSM state has already
//! committed, and an auto-move failure must never down the claim loop. A task
//! with no matching auto-move column is a silent no-op.

use ainb_hangar_core::ids::WorkspaceId;
use ainb_hangar_store::repo::board::BoardRepo;
use ainb_hangar_store::repo::card_dependency::CardDependencyRepo;
use ainb_hangar_store::repo::task::Task;
use sqlx::SqlitePool;

/// Move the task's issue card to the `new_state`-matched auto-move column of every
/// board that carries it (P4 / D8). Best-effort; see the module docs.
///
/// A task with no `issue_id` (an ad-hoc / chat task) is skipped — there is no
/// card to move. `new_state` is a task-status token (`running` / `done` /
/// `failed` / …), matched against each column's `fsm_state`.
pub async fn auto_move_after_transition(pool: &SqlitePool, task: &Task, new_state: &str) {
    let Some(issue_id) = task.issue_id.as_deref() else {
        return;
    };
    let Ok(ws) = WorkspaceId::from_str(task.workspace_id.clone()) else {
        tracing::warn!(task_id = %task.id, "board auto-move: empty workspace id; skipping");
        return;
    };
    match BoardRepo::auto_move_on_state(pool, &ws, issue_id, new_state).await {
        Ok(moved) if !moved.is_empty() => {
            for m in &moved {
                tracing::info!(
                    task_id = %task.id,
                    issue_id = %issue_id,
                    board_id = %m.board_id,
                    column_id = %m.column_id,
                    state = new_state,
                    "board card auto-moved"
                );
            }
        }
        Ok(_) => {} // no board maps this state for this issue — a no-op.
        Err(e) => {
            tracing::warn!(error = %e, task_id = %task.id, "board auto-move failed; proceeding");
        }
    }
}

/// When a card's task reaches `done`, re-evaluate every card that DEPENDS on it
/// (tcp T4 / F7): a dependent whose LAST blocker just completed becomes RUNNABLE,
/// and — only if its `auto_run` flag is on and it has no active run — is
/// auto-launched via the shared [`run_card`](crate::rpc::run_card) core.
///
/// The RUNNABLE visual state needs no new event: the just-finished blocker already
/// pushed a `TaskFinished` (the plugin re-pulls `boards_list` on it), and the fresh
/// snapshot renders the dependent with an empty `blocked_by` (the 🔒 clears). This
/// hook only drives the OPTIONAL auto-run so the board never has to.
///
/// # Best-effort
///
/// Every fault is logged and swallowed — the blocker's terminal state has already
/// committed, and a dependency-unblock failure must never down the claim loop. A
/// dependent that is still blocked by another card, that has no `auto_run`, or that
/// already has an active run is a silent no-op. Only a task that reached `done`
/// should be passed here (a failed / cancelled blocker does not satisfy a
/// dependency).
pub async fn unblock_dependents_after_done(pool: &SqlitePool, task: &Task) {
    let Some(blocker_issue_id) = task.issue_id.as_deref() else {
        return;
    };
    let Ok(ws) = WorkspaceId::from_str(task.workspace_id.clone()) else {
        return;
    };

    let dependents = match CardDependencyRepo::dependents_of(pool, blocker_issue_id).await {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(error = %e, task_id = %task.id, "unblock: reading dependents failed");
            return;
        }
    };
    for dep in dependents {
        // Still blocked by another card? Then this completion did not unblock it.
        match CardDependencyRepo::unfinished_blockers_of(pool, &dep).await {
            Ok(remaining) if !remaining.is_empty() => continue,
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(error = %e, dependent = %dep, "unblock: reading blockers failed");
                continue;
            }
        }
        tracing::info!(dependent = %dep, blocker = %blocker_issue_id, "card unblocked (last blocker done)");

        // Auto-run only when the dependent opted in (default OFF: explicit run stays
        // the default). The claim loop's concurrency caps bound any chain of
        // auto-runs — enqueue is cheap, dispatch is capped.
        match CardDependencyRepo::get_auto_run(pool, &dep).await {
            Ok(true) => auto_run_dependent(pool, &ws, &dep).await,
            Ok(false) => {}
            Err(e) => tracing::warn!(error = %e, dependent = %dep, "unblock: reading auto_run failed"),
        }
    }
}

/// Auto-launch a now-runnable dependent card through the shared [`run_card`] core.
/// A benign refusal (still blocked, or already has an active run) is logged at
/// debug and skipped — the card is simply not launchable right now, not broken.
async fn auto_run_dependent(pool: &SqlitePool, ws: &WorkspaceId, issue_id: &str) {
    use ainb_hangar_store::repo::issue::IssueRepo;

    let issue = match IssueRepo::get_by_id(pool, issue_id).await {
        Ok(Some(i)) if i.workspace_id == ws.as_str() => i,
        Ok(_) => return,
        Err(e) => {
            tracing::warn!(error = %e, issue = %issue_id, "auto-run: loading issue failed");
            return;
        }
    };
    // Board-agnostic (the auto-run seam does not know which board); the F4 board
    // tier is skipped, the cascade still resolves the agent. Headless run.
    match crate::rpc::run_card(pool, ws, None, &issue, "headless", None, None).await {
        Ok(_) => tracing::info!(issue = %issue_id, "card auto-run launched (last blocker done)"),
        Err(crate::rpc::CardRunError::Blocked(_) | crate::rpc::CardRunError::ActiveRun(_)) => {
            tracing::debug!(issue = %issue_id, "auto-run skipped (not launchable right now)");
        }
        Err(_) => tracing::warn!(issue = %issue_id, "auto-run refused (no repo/agent or store fault)"),
    }
}
