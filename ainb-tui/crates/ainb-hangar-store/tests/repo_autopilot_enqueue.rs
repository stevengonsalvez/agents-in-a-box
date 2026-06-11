//! Autopilot tick → enqueue path (P7.4).
//!
//! [`fire_autopilot_tick`] is the single-transaction fire path the scheduler
//! (P7.3) and the manager screen's "run now" (P7.5) both call: it inserts one
//! `autopilot_run` (`running`) row and one `agent_task_queue` row linked back to
//! it, atomically. The autopilot task carries `issue_id IS NULL` (it has no
//! originating issue) and `autopilot_run_id IS NOT NULL` — that link column is
//! the discriminator that marks a queue row as an autopilot run rather than an
//! issue / chat task.
//!
//! Completing (or failing / cancelling) a task that carries an
//! `autopilot_run_id` cascades through the shared finalize primitive to stamp
//! the parent `autopilot_run.completed_at` and flip its status — so the run
//! history reflects the real terminal outcome of its task without a separate
//! call.

use ainb_hangar_core::clock::FixedClock;
use ainb_hangar_core::ids::{AgentId, WorkspaceId};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::autopilot::{AutopilotRepo, NewAutopilot};
use ainb_hangar_store::repo::task::TaskRepo;
use ainb_hangar_store::service::complete::{CompleteParams, CompleteTaskService};
use ainb_hangar_store::service::fail::{FailTaskService, FailureReason};
use sqlx::Row;

/// Fixed clock instant all tests fire at (epoch-ms, 2026-01-01T00:00:00Z).
const T0: i64 = 1_767_225_600_000;

/// Seed the workspace + user + runtime + agent FK chain every autopilot run
/// needs. Returns `(workspace_id, runtime_id, agent_id)`.
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

/// Create one autopilot bound to the seeded agent and read it back.
async fn seed_autopilot(store: &Store) -> ainb_hangar_store::repo::autopilot::Autopilot {
    let clock = FixedClock(T0);
    let id = AutopilotRepo::create(
        store.pool(),
        &clock,
        &NewAutopilot {
            workspace_id: WorkspaceId::from_str("ws-1").unwrap(),
            agent_id: AgentId::from_str("agent-1").unwrap(),
            name: "daily".to_string(),
            instructions: Some("do the thing".to_string()),
            cron_expr: "0 9 * * *".to_string(),
            max_concurrent_runs: 1,
            execution_mode: ainb_hangar_store::repo::autopilot::ExecutionMode::default(),
            concurrency_policy: ainb_hangar_store::repo::autopilot::ConcurrencyPolicy::default(),
        },
    )
    .await
    .expect("create autopilot");
    AutopilotRepo::get(store.pool(), &WorkspaceId::from_str("ws-1").unwrap(), &id)
        .await
        .expect("get autopilot")
        .expect("autopilot present")
}

#[tokio::test]
async fn fire_creates_run_and_task_atomically() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed_graph(&store).await;
    let autopilot = seed_autopilot(&store).await;
    let clock = FixedClock(T0);

    let (run_id, task_id) = ainb_hangar_store::repo::autopilot_run::fire_autopilot_tick(
        store.pool(),
        &clock,
        &autopilot,
    )
    .await
    .expect("fire tick");

    // The run row is visible, in-flight (running), linked to the autopilot.
    let run_row = sqlx::query(
        "SELECT autopilot_id, started_at, completed_at, status FROM autopilot_run WHERE id = ?",
    )
    .bind(run_id.as_str())
    .fetch_one(store.pool())
    .await
    .expect("run row present after commit");
    assert_eq!(run_row.get::<String, _>("autopilot_id"), autopilot.id);
    assert_eq!(run_row.get::<i64, _>("started_at"), T0);
    assert!(run_row.get::<Option<i64>, _>("completed_at").is_none());
    assert_eq!(run_row.get::<String, _>("status"), "running");

    // The task row is visible, queued, issue-less, and links back to the run.
    let task = TaskRepo::get_by_id(store.pool(), task_id.as_str())
        .await
        .expect("get task")
        .expect("task present after commit");
    assert_eq!(task.status, "queued");
    assert_eq!(task.issue_id, None, "autopilot task carries no issue");
    assert_eq!(task.workspace_id, autopilot.workspace_id);
    assert_eq!(task.agent_id, autopilot.agent_id);

    let link: Option<String> =
        sqlx::query_scalar("SELECT autopilot_run_id FROM agent_task_queue WHERE id = ?")
            .bind(task_id.as_str())
            .fetch_one(store.pool())
            .await
            .expect("read link column");
    assert_eq!(
        link.as_deref(),
        Some(run_id.as_str()),
        "task.autopilot_run_id must equal the created run id"
    );
}

#[tokio::test]
async fn fire_rolls_back_on_task_insert_failure() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed_graph(&store).await;
    let mut autopilot = seed_autopilot(&store).await;
    let clock = FixedClock(T0);

    // Point the autopilot at an agent that does not exist. PRAGMA foreign_keys
    // is OFF in this crate, so a raw task insert with a dangling agent_id would
    // silently succeed and orphan the run — the fire path guards against this by
    // resolving the agent's runtime inside the transaction and aborting when the
    // agent row is absent. The abort drops the transaction, rolling back the
    // already-inserted autopilot_run row: true single-tx atomicity.
    autopilot.agent_id = "agent-does-not-exist".to_string();

    let err = ainb_hangar_store::repo::autopilot_run::fire_autopilot_tick(
        store.pool(),
        &clock,
        &autopilot,
    )
    .await
    .expect_err("a bad agent must abort the whole transaction");
    assert!(
        matches!(
            err,
            ainb_hangar_store::repo::autopilot_run::FireError::AgentNotFound { .. }
        ),
        "expected AgentNotFound, got {err:?}"
    );

    // No autopilot_run row persisted — the run insert was rolled back with the task.
    let run_count: i64 = sqlx::query_scalar("SELECT count(*) FROM autopilot_run")
        .fetch_one(store.pool())
        .await
        .expect("count runs");
    assert_eq!(run_count, 0, "rollback must leave zero autopilot_run rows");

    // No task row persisted either.
    let task_count: i64 = sqlx::query_scalar("SELECT count(*) FROM agent_task_queue")
        .fetch_one(store.pool())
        .await
        .expect("count tasks");
    assert_eq!(
        task_count, 0,
        "rollback must leave zero agent_task_queue rows"
    );
}

#[tokio::test]
async fn task_kind_is_autopilot() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed_graph(&store).await;
    let autopilot = seed_autopilot(&store).await;
    let clock = FixedClock(T0);

    let (run_id, task_id) = ainb_hangar_store::repo::autopilot_run::fire_autopilot_tick(
        store.pool(),
        &clock,
        &autopilot,
    )
    .await
    .expect("fire tick");

    // The discriminator that marks a queue row as an autopilot task is
    // `autopilot_run_id IS NOT NULL` paired with `issue_id IS NULL`. Assert the
    // real columns rather than a synthetic `kind` token.
    let row = sqlx::query("SELECT issue_id, autopilot_run_id FROM agent_task_queue WHERE id = ?")
        .bind(task_id.as_str())
        .fetch_one(store.pool())
        .await
        .expect("read discriminator columns");
    assert_eq!(row.get::<Option<String>, _>("issue_id"), None);
    assert_eq!(
        row.get::<Option<String>, _>("autopilot_run_id").as_deref(),
        Some(run_id.as_str()),
        "an autopilot task is identified by a non-null autopilot_run_id"
    );

    // And a plain issue task is NOT an autopilot task: its link column is null.
    sqlx::query(
        "INSERT INTO issue (id, workspace_id, title, state, creator_type, creator_id, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind("issue-1")
    .bind("ws-1")
    .bind("An issue")
    .bind("open")
    .bind("member")
    .bind("user-1")
    .bind(0_i64)
    .execute(store.pool())
    .await
    .expect("insert issue");
    TaskRepo::insert(
        store.pool(),
        &ainb_hangar_store::repo::task::NewTask {
            id: "task-issue".to_string(),
            workspace_id: "ws-1".to_string(),
            runtime_id: "rt-1".to_string(),
            agent_id: "agent-1".to_string(),
            issue_id: Some("issue-1".to_string()),
            work_dir: None,
            priority: 0,
            created_at: T0,
            autopilot_run_id: None,
        },
    )
    .await
    .expect("insert issue task");
    let issue_link: Option<String> =
        sqlx::query_scalar("SELECT autopilot_run_id FROM agent_task_queue WHERE id = ?")
            .bind("task-issue")
            .fetch_one(store.pool())
            .await
            .expect("read issue task link");
    assert_eq!(issue_link, None, "an issue task is not an autopilot task");
}

#[tokio::test]
async fn run_completes_when_task_completes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed_graph(&store).await;
    let autopilot = seed_autopilot(&store).await;
    let fire_clock = FixedClock(T0);

    let (run_id, task_id) = ainb_hangar_store::repo::autopilot_run::fire_autopilot_tick(
        store.pool(),
        &fire_clock,
        &autopilot,
    )
    .await
    .expect("fire tick");

    // Drive the task through the real FSM: queued -> dispatched -> running ->
    // done, so the completion exercises the production finalize path.
    sqlx::query("UPDATE agent_task_queue SET status = 'running' WHERE id = ?")
        .bind(task_id.as_str())
        .execute(store.pool())
        .await
        .expect("force running for the test");

    let complete_clock = FixedClock(T0 + 60_000);
    CompleteTaskService::complete(
        store.pool(),
        task_id.as_str(),
        CompleteParams {
            result: serde_json::json!({"ok": true}),
            session_id: None,
            work_dir: None,
        },
        &complete_clock,
    )
    .await
    .expect("complete task");

    // Completing the task carrying autopilot_run_id stamps the run's
    // completed_at and flips status to 'completed'.
    let run_row = sqlx::query("SELECT completed_at, status FROM autopilot_run WHERE id = ?")
        .bind(run_id.as_str())
        .fetch_one(store.pool())
        .await
        .expect("run row present");
    assert_eq!(
        run_row.get::<Option<i64>, _>("completed_at"),
        Some(T0 + 60_000),
        "run.completed_at must be stamped at the task's finish time"
    );
    assert_eq!(
        run_row.get::<String, _>("status"),
        "completed",
        "a done task completes its run"
    );
}

#[tokio::test]
async fn run_fails_when_task_fails() {
    // The cascade is on the shared finalize primitive, so a *failed* task closes
    // its run as `failed` too — proving the hook is on the real terminal path,
    // not the complete service alone.
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed_graph(&store).await;
    let autopilot = seed_autopilot(&store).await;
    let fire_clock = FixedClock(T0);

    let (run_id, task_id) = ainb_hangar_store::repo::autopilot_run::fire_autopilot_tick(
        store.pool(),
        &fire_clock,
        &autopilot,
    )
    .await
    .expect("fire tick");

    sqlx::query("UPDATE agent_task_queue SET status = 'running' WHERE id = ?")
        .bind(task_id.as_str())
        .execute(store.pool())
        .await
        .expect("force running for the test");

    let fail_clock = FixedClock(T0 + 30_000);
    FailTaskService::fail(
        store.pool(),
        task_id.as_str(),
        FailureReason::AgentError,
        &fail_clock,
    )
    .await
    .expect("fail task");

    let run_row = sqlx::query("SELECT completed_at, status FROM autopilot_run WHERE id = ?")
        .bind(run_id.as_str())
        .fetch_one(store.pool())
        .await
        .expect("run row present");
    assert_eq!(
        run_row.get::<Option<i64>, _>("completed_at"),
        Some(T0 + 30_000)
    );
    assert_eq!(run_row.get::<String, _>("status"), "failed");
}
