//! `ClaimTaskService` integration tests (P1.2).
//!
//! Exercises the atomic `queued -> dispatched` claim against a real ephemeral
//! `SQLite` WAL database. Every test uses an isolated tempdir store so parallel
//! cargo test threads do not share state. Time is injected via [`FixedClock`]
//! so `dispatched_at` assertions are deterministic.
//!
//! The claim contract (mirrors Multica `task.go`):
//! - claim flips exactly one oldest `queued` row for the runtime to
//!   `dispatched` and stamps `dispatched_at = clock.now_ms()`,
//! - an empty queue returns `Ok(None)` (the daemon sleeps + retries; not an
//!   error),
//! - a runtime never claims another runtime's tasks,
//! - the partial unique index `idx_one_pending_task_per_issue` (migration 0004)
//!   blocks a second pending task per issue,
//! - `agent.max_concurrent_tasks` caps how many of an agent's tasks may run at
//!   once (`task.go:761` `CountRunningTasks` parity),
//! - concurrent claims are race-safe: at most one claimant wins any given row.

use ainb_hangar_core::clock::FixedClock;
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::task::{NewTask, TaskRepo};
use ainb_hangar_store::service::claim::ClaimTaskService;

/// Frozen "now" used for `dispatched_at` assertions.
const NOW_MS: i64 = 1_700_000_500_000;

/// Seed the workspace + user + runtime + agent rows the task FKs require.
///
/// `agent.max_concurrent_tasks` defaults to 1 unless `max_concurrent` overrides
/// it. Returns `(workspace_id, runtime_id, agent_id)`.
async fn seed_graph(
    store: &Store,
    runtime_id: &str,
    agent_id: &str,
    max_concurrent: i64,
) -> (String, String, String) {
    let pool = store.pool();
    sqlx::query("INSERT OR IGNORE INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
        .bind("ws-1")
        .bind("alpha")
        .bind("Alpha")
        .bind(0_i64)
        .execute(pool)
        .await
        .expect("insert workspace");
    sqlx::query("INSERT OR IGNORE INTO user (id, email, created_at) VALUES (?, ?, ?)")
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
    .bind(runtime_id)
    .bind("ws-1")
    .bind(format!("daemon-{runtime_id}"))
    .bind("claude")
    .bind("local")
    .execute(pool)
    .await
    .expect("insert runtime");
    sqlx::query(
        "INSERT INTO agent \
         (id, workspace_id, name, runtime_id, visibility, owner_id, max_concurrent_tasks) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(agent_id)
    .bind("ws-1")
    .bind("Agent")
    .bind(runtime_id)
    .bind("workspace")
    .bind("user-1")
    .bind(max_concurrent)
    .execute(pool)
    .await
    .expect("insert agent");
    (
        "ws-1".to_string(),
        runtime_id.to_string(),
        agent_id.to_string(),
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

/// Build a `NewTask` bound to the given runtime/agent, with an optional issue.
fn new_task(
    id: &str,
    runtime_id: &str,
    agent_id: &str,
    issue_id: Option<&str>,
    created_at: i64,
) -> NewTask {
    NewTask {
        id: id.to_string(),
        workspace_id: "ws-1".to_string(),
        runtime_id: runtime_id.to_string(),
        agent_id: agent_id.to_string(),
        issue_id: issue_id.map(str::to_string),
        work_dir: None,
        created_at,
        autopilot_run_id: None,
    }
}

#[tokio::test]
async fn claim_queued_task_marks_dispatched_atomic() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed_graph(&store, "rt-1", "agent-1", 1).await;
    TaskRepo::insert(
        store.pool(),
        &new_task("task-1", "rt-1", "agent-1", None, 1),
    )
    .await
    .expect("enqueue");

    let clock = FixedClock(NOW_MS);
    let claimed = ClaimTaskService::claim_for_runtime(store.pool(), "rt-1", &clock)
        .await
        .expect("claim ok")
        .expect("a task is claimed");

    assert_eq!(claimed.id, "task-1");
    assert_eq!(claimed.runtime_id, "rt-1");
    assert_eq!(claimed.agent_id, "agent-1");
    assert_eq!(
        claimed.dispatched_at, NOW_MS,
        "dispatched_at = clock.now_ms()"
    );

    let row = TaskRepo::get_by_id(store.pool(), "task-1")
        .await
        .expect("read back")
        .expect("row exists");
    assert_eq!(row.status, "dispatched");
    assert_eq!(row.dispatched_at, Some(NOW_MS));
}

#[tokio::test]
async fn claim_returns_none_when_no_queued_for_runtime() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed_graph(&store, "rt-1", "agent-1", 1).await;

    let clock = FixedClock(NOW_MS);
    let claimed = ClaimTaskService::claim_for_runtime(store.pool(), "rt-1", &clock)
        .await
        .expect("empty queue is Ok, not an error");
    assert!(claimed.is_none(), "empty queue yields None");
}

#[tokio::test]
async fn claim_skips_tasks_for_other_runtimes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed_graph(&store, "rt-1", "agent-1", 1).await;
    seed_graph(&store, "rt-2", "agent-2", 1).await;
    TaskRepo::insert(
        store.pool(),
        &new_task("task-a", "rt-1", "agent-1", None, 1),
    )
    .await
    .expect("enqueue a");
    TaskRepo::insert(
        store.pool(),
        &new_task("task-b", "rt-2", "agent-2", None, 1),
    )
    .await
    .expect("enqueue b");

    let clock = FixedClock(NOW_MS);
    let a = ClaimTaskService::claim_for_runtime(store.pool(), "rt-1", &clock)
        .await
        .expect("claim rt-1")
        .expect("rt-1 has a task");
    let b = ClaimTaskService::claim_for_runtime(store.pool(), "rt-2", &clock)
        .await
        .expect("claim rt-2")
        .expect("rt-2 has a task");

    assert_eq!(a.id, "task-a");
    assert_eq!(b.id, "task-b");
}

#[tokio::test]
async fn claim_takes_oldest_queued_first() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed_graph(&store, "rt-1", "agent-1", 5).await;
    // Insert newest first to prove ordering is by created_at, not insert order.
    TaskRepo::insert(
        store.pool(),
        &new_task("task-new", "rt-1", "agent-1", None, 200),
    )
    .await
    .expect("enqueue new");
    TaskRepo::insert(
        store.pool(),
        &new_task("task-old", "rt-1", "agent-1", None, 100),
    )
    .await
    .expect("enqueue old");

    let clock = FixedClock(NOW_MS);
    let claimed = ClaimTaskService::claim_for_runtime(store.pool(), "rt-1", &clock)
        .await
        .expect("claim ok")
        .expect("a task is claimed");
    assert_eq!(claimed.id, "task-old", "oldest queued_at wins");
}

#[tokio::test]
async fn partial_unique_index_blocks_second_pending_per_issue() {
    // Asserts the migration 0004 partial unique index is wired (Multica
    // migration 022 invariant): a second *pending* task for the same issue
    // raises `UNIQUE constraint failed: idx_one_pending_task_per_issue`.
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed_graph(&store, "rt-1", "agent-1", 1).await;
    seed_issue(&store, "issue-1").await;

    TaskRepo::insert(
        store.pool(),
        &new_task("task-1", "rt-1", "agent-1", Some("issue-1"), 1),
    )
    .await
    .expect("first pending task inserts");

    let err = TaskRepo::insert(
        store.pool(),
        &new_task("task-2", "rt-1", "agent-1", Some("issue-1"), 2),
    )
    .await
    .expect_err("second pending task for same issue must violate the partial unique index");

    let db_err = err.as_database_error().expect("expected a database constraint error");
    let msg = db_err.message().to_lowercase();
    assert!(
        msg.contains("unique") || msg.contains("constraint"),
        "expected a UNIQUE constraint failure, got: {err}"
    );
}

#[tokio::test]
async fn claim_respects_per_agent_max_concurrent() {
    // agent.max_concurrent_tasks = 1, one task already running -> the queued
    // task is NOT claimed even though it exists (task.go:761 parity).
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed_graph(&store, "rt-1", "agent-1", 1).await;

    TaskRepo::insert(
        store.pool(),
        &new_task("task-running", "rt-1", "agent-1", None, 1),
    )
    .await
    .expect("enqueue running");
    sqlx::query("UPDATE agent_task_queue SET status = 'running' WHERE id = ?")
        .bind("task-running")
        .execute(store.pool())
        .await
        .expect("force running");
    TaskRepo::insert(
        store.pool(),
        &new_task("task-queued", "rt-1", "agent-1", None, 2),
    )
    .await
    .expect("enqueue queued");

    let clock = FixedClock(NOW_MS);
    let claimed = ClaimTaskService::claim_for_runtime(store.pool(), "rt-1", &clock)
        .await
        .expect("claim ok");
    assert!(
        claimed.is_none(),
        "agent at max_concurrent_tasks must not claim another task"
    );
}

#[tokio::test]
async fn claim_allows_when_under_max_concurrent() {
    // agent.max_concurrent_tasks = 2, one running, one queued -> the queued one
    // IS claimable because running count (1) < max (2).
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed_graph(&store, "rt-1", "agent-1", 2).await;

    TaskRepo::insert(
        store.pool(),
        &new_task("task-running", "rt-1", "agent-1", None, 1),
    )
    .await
    .expect("enqueue running");
    sqlx::query("UPDATE agent_task_queue SET status = 'running' WHERE id = ?")
        .bind("task-running")
        .execute(store.pool())
        .await
        .expect("force running");
    TaskRepo::insert(
        store.pool(),
        &new_task("task-queued", "rt-1", "agent-1", None, 2),
    )
    .await
    .expect("enqueue queued");

    let clock = FixedClock(NOW_MS);
    let claimed = ClaimTaskService::claim_for_runtime(store.pool(), "rt-1", &clock)
        .await
        .expect("claim ok")
        .expect("under max_concurrent, the queued task is claimable");
    assert_eq!(claimed.id, "task-queued");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_claim_only_one_wins() {
    // tokio::join! two claims on a single queued task. Under the atomic
    // UPDATE...RETURNING, exactly one returns Some(task), the other Some(other)
    // or None -- never the SAME row twice.
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed_graph(&store, "rt-1", "agent-1", 5).await;
    TaskRepo::insert(
        store.pool(),
        &new_task("task-1", "rt-1", "agent-1", None, 1),
    )
    .await
    .expect("enqueue");

    let clock = FixedClock(NOW_MS);
    let p1 = store.pool().clone();
    let p2 = store.pool().clone();
    let (r1, r2) = tokio::join!(
        async move { ClaimTaskService::claim_for_runtime(&p1, "rt-1", &clock).await },
        async move { ClaimTaskService::claim_for_runtime(&p2, "rt-1", &clock).await },
    );
    let r1 = r1.expect("claim 1 ok");
    let r2 = r2.expect("claim 2 ok");

    let ids: Vec<String> = [r1, r2].into_iter().flatten().map(|c| c.id).collect();
    // The single task must be claimed by AT MOST one winner -- never duplicated.
    assert!(
        ids.iter().filter(|id| *id == "task-1").count() <= 1,
        "task-1 must not be claimed by both racers, got {ids:?}"
    );
    assert!(
        !ids.is_empty(),
        "at least one racer should claim the single available task"
    );
}
