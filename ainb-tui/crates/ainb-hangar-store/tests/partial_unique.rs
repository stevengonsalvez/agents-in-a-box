//! Partial-unique-index behaviour for `agent_task_queue`.
//!
//! The index `idx_one_pending_task_per_issue` enforces *at most one* non-terminal
//! (`queued` or `dispatched`) task per `issue_id`, coalescing duplicate fires
//! (per Multica `022_task_lifecycle_guards.up.sql`). These tests prove the three
//! invariants the index must hold:
//!
//! 1. a second pending task for the same issue is rejected,
//! 2. once the first task leaves the pending set, a fresh pending task is allowed,
//! 3. tasks with a `NULL` `issue_id` (chat / autopilot placeholders at v1) never
//!    collide.

use ainb_hangar_store::Store;
use ainb_hangar_store::repo::task::{NewTask, TaskRepo};

/// Seed the workspace + runtime + agent rows every task row's FKs require, and
/// optionally one issue. Returns `(workspace_id, runtime_id, agent_id)`.
async fn seed_graph(store: &Store) -> (String, String, String) {
    let pool = store.pool();
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
        .bind("ws-1")
        .bind("alpha")
        .bind("Alpha")
        .bind(0_i64)
        .execute(pool)
        .await
        .expect("insert workspace");
    sqlx::query("INSERT INTO user (id, email, created_at) VALUES (?, ?, ?)")
        .bind("user-1")
        .bind("a@example.com")
        .bind(0_i64)
        .execute(pool)
        .await
        .expect("insert user");
    sqlx::query(
        "INSERT INTO agent_runtime (id, workspace_id, daemon_id, provider, runtime_mode) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind("rt-1")
    .bind("ws-1")
    .bind("daemon-1")
    .bind("claude")
    .bind("local")
    .execute(pool)
    .await
    .expect("insert runtime");
    sqlx::query(
        "INSERT INTO agent (id, workspace_id, name, runtime_id, visibility, owner_id) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind("agent-1")
    .bind("ws-1")
    .bind("Agent")
    .bind("rt-1")
    .bind("workspace")
    .bind("user-1")
    .execute(pool)
    .await
    .expect("insert agent");
    (
        "ws-1".to_string(),
        "rt-1".to_string(),
        "agent-1".to_string(),
    )
}

/// Seed one issue row and return its id.
async fn seed_issue(store: &Store, id: &str) -> String {
    sqlx::query(
        "INSERT INTO issue \
         (id, workspace_id, title, state, creator_type, creator_id, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind("ws-1")
    .bind("An issue")
    .bind("open")
    .bind("member")
    .bind("user-1")
    .bind(0_i64)
    .execute(store.pool())
    .await
    .expect("insert issue");
    id.to_string()
}

/// Build a `NewTask` with the shared FK graph and an optional issue.
fn new_task(id: &str, issue_id: Option<&str>) -> NewTask {
    NewTask {
        id: id.to_string(),
        workspace_id: "ws-1".to_string(),
        runtime_id: "rt-1".to_string(),
        agent_id: "agent-1".to_string(),
        issue_id: issue_id.map(str::to_string),
        work_dir: None,
        created_at: 1_700_000_000_000,
        autopilot_run_id: None,
    }
}

#[tokio::test]
async fn inserting_second_pending_task_for_same_issue_violates_unique() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed_graph(&store).await;
    seed_issue(&store, "issue-1").await;

    TaskRepo::insert(store.pool(), &new_task("task-1", Some("issue-1")))
        .await
        .expect("first pending task inserts");

    let err = TaskRepo::insert(store.pool(), &new_task("task-2", Some("issue-1")))
        .await
        .expect_err("second pending task for same issue must violate the partial unique index");

    // sqlx surfaces the SQLite UNIQUE constraint violation as a Database error.
    let db_err = err.as_database_error().expect("expected a database constraint error");
    let msg = db_err.message().to_lowercase();
    assert!(
        msg.contains("unique") || msg.contains("constraint"),
        "expected a UNIQUE constraint failure, got: {err}"
    );
}

#[tokio::test]
async fn transitioning_first_to_done_then_inserting_second_pending_succeeds() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed_graph(&store).await;
    seed_issue(&store, "issue-1").await;

    TaskRepo::insert(store.pool(), &new_task("task-1", Some("issue-1")))
        .await
        .expect("first pending task inserts");

    // Move the first task out of the pending set (P1 will own typed transitions;
    // here we drive the column directly to prove the partial index only blocks
    // while the prior task is non-terminal).
    sqlx::query("UPDATE agent_task_queue SET status = 'done' WHERE id = ?")
        .bind("task-1")
        .execute(store.pool())
        .await
        .expect("mark first task done");

    TaskRepo::insert(store.pool(), &new_task("task-2", Some("issue-1")))
        .await
        .expect("second pending task allowed once the first is terminal");
}

#[tokio::test]
async fn null_issue_id_tasks_do_not_collide() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed_graph(&store).await;

    // Two pending tasks with no issue (chat / autopilot placeholders) coexist
    // because the partial index excludes NULL issue_id rows.
    TaskRepo::insert(store.pool(), &new_task("task-1", None))
        .await
        .expect("first null-issue task inserts");
    TaskRepo::insert(store.pool(), &new_task("task-2", None))
        .await
        .expect("second null-issue task must not collide with the first");

    let pending = TaskRepo::list_pending_for_runtime(store.pool(), "rt-1")
        .await
        .expect("list pending");
    assert_eq!(pending.len(), 2, "both null-issue pending tasks are listed");
}
