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
