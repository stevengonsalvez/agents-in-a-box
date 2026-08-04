//! The plain-task issue lifecycle: `hangar/issues_list` `state` must walk
//! `todo -> in_progress -> done` as a dispatched task walks its FSM.
//!
//! The default hangar issue board buckets cards by `issue.state` alone (the
//! `issues_list` snapshot serves it verbatim), while the board auto-move only
//! touches durable `board_card` rows the default screen never materialises. So a
//! plain dispatched task used to walk its FSM (`running -> done`) with its issue
//! frozen at `open`/`todo`, stranding its card in Todo through the whole run and
//! after it finished. The fix advances `issue.state` at the same FSM seams the
//! board auto-move fires — [`board::advance_issue_lifecycle_after_transition`] on
//! the `running` transition, [`board::advance_issue_lifecycle_after_terminal`] on
//! the terminal drain — so the snapshot renders the right column.
//!
//! These tests assert the SNAPSHOT the plugin actually reads (not the raw row),
//! prove the advance-only guard never regresses a terminal state, and prove a
//! FAILED task never marks its issue `done`. Removing the `IssueRepo::update_state`
//! call in `board::advance_issue_lifecycle_after_transition` turns the first test
//! red (the mutation spot-check).

use ainb_hangar_daemon::board::{
    advance_issue_lifecycle_after_terminal, advance_issue_lifecycle_after_transition,
};
use ainb_hangar_daemon::rpc::snapshots::issues_list;
use ainb_hangar_daemon::seed::{WS_ID, seed_p4_fixture};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::task::{Task, TaskRepo};
use sqlx::SqlitePool;

/// The snapshot `state` the plugin's board buckets `issue-1` by.
async fn issue1_state(pool: &SqlitePool) -> String {
    let issues = issues_list(pool, WS_ID).await.expect("issues_list snapshot");
    issues
        .iter()
        .find(|i| i.id.as_str() == "issue-1")
        .expect("issue-1 present in snapshot")
        .state
        .clone()
}

/// Re-read `task-1` as a [`Task`] (the board seams take `&Task`).
async fn task1(pool: &SqlitePool) -> Task {
    TaskRepo::get_by_id(pool, "task-1")
        .await
        .expect("task get")
        .expect("task-1 present")
}

/// Force `task-1`'s queue status to `new_status` so the aggregate-terminal read
/// the terminal seam performs sees the FSM outcome under test.
async fn set_task1_status(pool: &SqlitePool, new_status: &str) {
    sqlx::query("UPDATE agent_task_queue SET status = ? WHERE id = 'task-1'")
        .bind(new_status)
        .execute(pool)
        .await
        .expect("update task status");
}

#[tokio::test]
async fn issue_state_walks_todo_in_progress_done_across_task_fsm() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    seed_p4_fixture(store.pool()).await.unwrap();
    let pool = store.pool();

    // Pre: the seed leaves issue-1 in the legacy `open` state, which the board
    // buckets under Todo — the exact "stranded in Todo" starting point.
    assert_eq!(
        issue1_state(pool).await,
        "open",
        "seed starts issue-1 in the Todo bucket"
    );

    // running: the dispatched task started → issue promotes to in_progress.
    let task = task1(pool).await;
    advance_issue_lifecycle_after_transition(pool, &task, "running").await;
    assert_eq!(
        issue1_state(pool).await,
        "in_progress",
        "the running transition promotes the issue to In Progress"
    );

    // terminal success: the task's whole active set drained `done` → issue done.
    set_task1_status(pool, "done").await;
    let task = task1(pool).await;
    advance_issue_lifecycle_after_terminal(pool, &task).await;
    assert_eq!(
        issue1_state(pool).await,
        "done",
        "terminal success advances the issue to Done"
    );

    // advance-only: a stray re-run's `running` transition must never drag an
    // already-Done issue back to In Progress.
    advance_issue_lifecycle_after_transition(pool, &task, "running").await;
    assert_eq!(
        issue1_state(pool).await,
        "done",
        "advance-only guard never regresses a terminal issue"
    );
}

#[tokio::test]
async fn failed_task_never_marks_its_issue_done() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    seed_p4_fixture(store.pool()).await.unwrap();
    let pool = store.pool();

    // Drive to in_progress, then fail the run.
    let task = task1(pool).await;
    advance_issue_lifecycle_after_transition(pool, &task, "running").await;
    assert_eq!(issue1_state(pool).await, "in_progress");

    set_task1_status(pool, "failed").await;
    let task = task1(pool).await;
    advance_issue_lifecycle_after_terminal(pool, &task).await;

    // A failed run has no `done` aggregate → the issue stays In Progress, never Done.
    assert_eq!(
        issue1_state(pool).await,
        "in_progress",
        "a failed task must not mark its issue Done"
    );
}

/// Force `issue-1` into `state` directly (the human/agent move the board's
/// `Move to ▸` submenu drives through the RPC).
async fn set_issue1_state(pool: &SqlitePool, state: &str) {
    sqlx::query("UPDATE issue SET state = ? WHERE id = 'issue-1'")
        .bind(state)
        .execute(pool)
        .await
        .expect("update issue state");
}

/// A BLOCKED issue is not "past done": unblocking it by starting a run still
/// promotes it to In Progress.
///
/// RED without the `advance_rank` guard — `blocked` is appended at column 5, so
/// the old `target.order() <= current.order()` comparison read `in_progress`
/// (column 2) as "behind" and froze the issue in Blocked forever.
#[tokio::test]
async fn blocked_issue_still_advances_to_in_progress() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    seed_p4_fixture(store.pool()).await.unwrap();
    let pool = store.pool();

    set_issue1_state(pool, "blocked").await;
    assert_eq!(
        issue1_state(pool).await,
        "blocked",
        "blocked seeds and lists"
    );

    let task = task1(pool).await;
    advance_issue_lifecycle_after_transition(pool, &task, "running").await;
    assert_eq!(
        issue1_state(pool).await,
        "in_progress",
        "a run on a blocked issue promotes it — blocked ranks with todo, not past done"
    );

    // …and the rest of the walk still works from there.
    set_task1_status(pool, "done").await;
    let task = task1(pool).await;
    advance_issue_lifecycle_after_terminal(pool, &task).await;
    assert_eq!(issue1_state(pool).await, "done");
}

/// A CANCELLED issue is terminal alongside `done`: no run transition revives it.
#[tokio::test]
async fn cancelled_issue_is_never_advanced() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    seed_p4_fixture(store.pool()).await.unwrap();
    let pool = store.pool();

    set_issue1_state(pool, "cancelled").await;

    let task = task1(pool).await;
    advance_issue_lifecycle_after_transition(pool, &task, "running").await;
    assert_eq!(
        issue1_state(pool).await,
        "cancelled",
        "a running transition never revives a cancelled issue"
    );

    set_task1_status(pool, "done").await;
    let task = task1(pool).await;
    advance_issue_lifecycle_after_terminal(pool, &task).await;
    assert_eq!(
        issue1_state(pool).await,
        "cancelled",
        "even a successful terminal drain leaves a cancelled issue cancelled"
    );
}

#[tokio::test]
async fn null_issue_chat_task_advance_is_a_noop() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    seed_p4_fixture(store.pool()).await.unwrap();
    let pool = store.pool();

    // A chat task carries no issue: the seam resolves no issue_id and must simply
    // do nothing (no panic, no cross-issue write).
    let mut task = task1(pool).await;
    task.issue_id = None;
    advance_issue_lifecycle_after_transition(pool, &task, "running").await;
    advance_issue_lifecycle_after_terminal(pool, &task).await;

    assert_eq!(
        issue1_state(pool).await,
        "open",
        "a null-issue chat task leaves every issue untouched"
    );
}

/// Provision the default six-stage pipeline in the fixture's workspace and park
/// `issue-1`'s card in the column named `column`.
async fn park_issue1_in_pipeline(pool: &SqlitePool, column: &str) {
    use ainb_hangar_core::clock::FixedClock;
    use ainb_hangar_core::idgen::SystemIdGen;
    use ainb_hangar_core::ids::WorkspaceId;
    use ainb_hangar_store::service::pipeline::PipelineService;

    let ws = WorkspaceId::from_str(WS_ID.to_string()).expect("workspace id");
    PipelineService::provision_default(pool, &ws, &SystemIdGen, &FixedClock(0))
        .await
        .expect("provision the pipeline");
    let (board_id, column_id): (String, String) = sqlx::query_as(
        "SELECT b.id, c.id FROM board AS b JOIN board_column AS c ON c.board_id = b.id \
          WHERE b.workspace_id = ?1 AND c.name = ?2",
    )
    .bind(WS_ID)
    .bind(column)
    .fetch_one(pool)
    .await
    .expect("the pipeline column exists");
    sqlx::query("DELETE FROM board_card WHERE issue_id = 'issue-1'")
        .execute(pool)
        .await
        .expect("clear any seeded card");
    sqlx::query(
        "INSERT INTO board_card (board_id, issue_id, column_id, added_at, ord) \
         VALUES (?1, 'issue-1', ?2, 0, 0)",
    )
    .bind(&board_id)
    .bind(&column_id)
    .execute(pool)
    .await
    .expect("park the card");
}

/// A card MID-PIPELINE must not render as Done.
///
/// The lifecycle advance promoted the issue to `done` the moment the FIRST
/// stage's aggregate landed, and `is_terminal()` then froze it there. So a card
/// that had only finished Implement, with Review and QA still to run, already
/// rendered Done on the default Kanban and in `hangar/issues_list`. Worse, the
/// terminal freeze meant the remaining stages could never move it again.
#[tokio::test]
async fn a_card_mid_pipeline_does_not_render_done() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    seed_p4_fixture(store.pool()).await.unwrap();
    let pool = store.pool();
    // The Implement stage has just finished, so the card has ADVANCED into
    // Review. Two stages remain.
    park_issue1_in_pipeline(pool, "Review").await;

    let task = task1(pool).await;
    advance_issue_lifecycle_after_transition(pool, &task, "running").await;
    set_task1_status(pool, "done").await;
    let task = task1(pool).await;
    advance_issue_lifecycle_after_terminal(pool, &task).await;

    assert_eq!(
        issue1_state(pool).await,
        "in_progress",
        "Review and QA are still to run, so the issue is not Done"
    );
}

/// The other side of the same rule: a card that has reached the pipeline's
/// TERMINAL column has no stage left, so its issue does advance to Done.
#[tokio::test]
async fn a_card_at_the_end_of_the_pipeline_does_render_done() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    seed_p4_fixture(store.pool()).await.unwrap();
    let pool = store.pool();
    // QA finished and the card advanced into the ungated Done column.
    park_issue1_in_pipeline(pool, "Done").await;

    let task = task1(pool).await;
    advance_issue_lifecycle_after_transition(pool, &task, "running").await;
    set_task1_status(pool, "done").await;
    let task = task1(pool).await;
    advance_issue_lifecycle_after_terminal(pool, &task).await;

    assert_eq!(
        issue1_state(pool).await,
        "done",
        "the card walked the whole pipeline, so the issue is Done"
    );
}
