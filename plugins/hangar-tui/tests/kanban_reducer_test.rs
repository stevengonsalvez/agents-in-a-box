//! P8.4 — Kanban board reducer behaviour.
//!
//! Pure-reducer tests over [`reduce_kanban`]: `←/→` focus moves columns, `↑/↓`
//! scrolls within a column, `Shift+←/→` raises a [`KanbanIntent::MoveCard`] for
//! the destination column's drop status, and a host `TaskStarted` /
//! `TaskFinished` event re-buckets the matching card within one tick.

use ainb_hangar_proto::events::{HangarEvent, TaskCardRow, TaskResult};
use ainb_plugin_hangar::screen::kanban::{
    reduce_kanban, BoardColumn, KanbanEvent, KanbanIntent, KanbanState,
};

const NOW: i64 = 1_700_000_600_000;

fn task(id: &str, status: &str) -> TaskCardRow {
    TaskCardRow {
        id: ainb_hangar_core::ids::TaskId::from_str(id).unwrap(),
        workspace_id: "default".into(),
        agent_id: "agent-1".into(),
        issue_id: Some("issue-1".into()),
        status: status.into(),
        priority: 0,
        created_at: NOW - 60_000,
    }
}

/// A board with one card per column.
fn one_per_column() -> KanbanState {
    KanbanState::from_tasks(
        &[
            task("t-queued", "queued"),
            task("t-running", "running"),
            task("t-done", "done"),
            task("t-failed", "failed"),
        ],
        NOW,
    )
}

/// `→` moves focus to the next column (clamped at the right edge).
#[test]
fn focus_right_moves_columns() {
    let s = one_per_column();
    assert_eq!(s.focused_col(), 0);
    let s = reduce_kanban(&s, KanbanEvent::FocusRight).state;
    assert_eq!(s.focused_col(), 1);
    let s = reduce_kanban(&s, KanbanEvent::FocusRight).state;
    let s = reduce_kanban(&s, KanbanEvent::FocusRight).state;
    assert_eq!(s.focused_col(), 3);
    // Clamped: a fourth right press stays on the last column.
    let s = reduce_kanban(&s, KanbanEvent::FocusRight).state;
    assert_eq!(s.focused_col(), 3);
}

/// `←` moves focus left (clamped at the left edge).
#[test]
fn focus_left_clamps_at_zero() {
    let s = one_per_column();
    let s = reduce_kanban(&s, KanbanEvent::FocusLeft).state;
    assert_eq!(s.focused_col(), 0, "already at left edge stays put");
}

/// `↑/↓` move the row selection within a column (clamped to its card count).
#[test]
fn focus_up_down_scrolls_within_column() {
    // Three queued cards in column 0.
    let s = KanbanState::from_tasks(
        &[
            task("q-1", "queued"),
            task("q-2", "queued"),
            task("q-3", "queued"),
        ],
        NOW,
    );
    assert_eq!(s.focused_row(), 0);
    let s = reduce_kanban(&s, KanbanEvent::FocusDown).state;
    assert_eq!(s.focused_row(), 1);
    let s = reduce_kanban(&s, KanbanEvent::FocusDown).state;
    assert_eq!(s.focused_row(), 2);
    // Clamped at the bottom.
    let s = reduce_kanban(&s, KanbanEvent::FocusDown).state;
    assert_eq!(s.focused_row(), 2);
    let s = reduce_kanban(&s, KanbanEvent::FocusUp).state;
    assert_eq!(s.focused_row(), 1);
}

/// `Shift+→` fires a move-card intent targeting the next column's drop status
/// (queued → running). The reducer leaves state unchanged (the daemon owns the
/// real move; the board reconciles on the next snapshot).
#[test]
fn shift_right_fires_move_to_running() {
    let s = one_per_column();
    let out = reduce_kanban(&s, KanbanEvent::MoveCardRight);
    assert_eq!(
        out.intent,
        Some(KanbanIntent::MoveCard {
            task_id: "t-queued".into(),
            to_status: "running".into(),
        }),
        "Shift+→ on the queued column must target the running column's drop status"
    );
}

/// `Shift+←` from the failed column targets the done column's drop status.
#[test]
fn shift_left_fires_move_to_done() {
    let mut s = one_per_column();
    // Focus the failed column (index 3).
    for _ in 0..3 {
        s = reduce_kanban(&s, KanbanEvent::FocusRight).state;
    }
    assert_eq!(s.focused_col(), 3);
    let out = reduce_kanban(&s, KanbanEvent::MoveCardLeft);
    assert_eq!(
        out.intent,
        Some(KanbanIntent::MoveCard {
            task_id: "t-failed".into(),
            to_status: "done".into(),
        })
    );
}

/// `Shift+→` at the right edge is a no-op (no intent).
#[test]
fn shift_right_at_edge_is_noop() {
    let mut s = one_per_column();
    for _ in 0..3 {
        s = reduce_kanban(&s, KanbanEvent::FocusRight).state;
    }
    let out = reduce_kanban(&s, KanbanEvent::MoveCardRight);
    assert!(
        out.intent.is_none(),
        "move past the right edge must be a no-op"
    );
}

/// The drop status for each column is the canonical re-bucket target: failed
/// column drops to `cancelled` (the user terminal), not `failed`.
#[test]
fn failed_column_drop_status_is_cancelled() {
    assert_eq!(BoardColumn::Failed.drop_status().as_str(), "cancelled");
    assert_eq!(BoardColumn::Queued.drop_status().as_str(), "queued");
    assert_eq!(BoardColumn::Running.drop_status().as_str(), "running");
    assert_eq!(BoardColumn::Done.drop_status().as_str(), "done");
}

/// A host `TaskStarted` event moves the matching card from queued → running
/// within one tick (no re-fetch needed).
#[test]
fn task_started_event_rebuckets_to_running() {
    let s = KanbanState::from_tasks(&[task("t-1", "queued")], NOW);
    assert_eq!(s.columns()[0].cards.len(), 1, "starts in queued");
    assert_eq!(s.columns()[1].cards.len(), 0);
    let event = HangarEvent::TaskStarted {
        task_id: ainb_hangar_core::ids::TaskId::from_str("t-1").unwrap(),
        started_at: chrono::Utc::now(),
    };
    let s = reduce_kanban(&s, KanbanEvent::Event(event)).state;
    assert_eq!(s.columns()[0].cards.len(), 0, "left queued");
    assert_eq!(s.columns()[1].cards.len(), 1, "now running");
    assert_eq!(s.columns()[1].cards[0].status, "running");
}

/// A host `TaskFinished{Failure}` event moves a running card to the failed
/// column within one tick.
#[test]
fn task_finished_failure_event_rebuckets_to_failed() {
    let s = KanbanState::from_tasks(&[task("t-9", "running")], NOW);
    let event = HangarEvent::TaskFinished {
        task_id: ainb_hangar_core::ids::TaskId::from_str("t-9").unwrap(),
        result: TaskResult::Failure,
        ended_at: chrono::Utc::now(),
    };
    let s = reduce_kanban(&s, KanbanEvent::Event(event)).state;
    assert_eq!(s.columns()[3].status, BoardColumn::Failed);
    assert_eq!(s.columns()[3].cards.len(), 1, "moved to failed column");
    assert_eq!(s.columns()[3].cards[0].status, "failed");
}
