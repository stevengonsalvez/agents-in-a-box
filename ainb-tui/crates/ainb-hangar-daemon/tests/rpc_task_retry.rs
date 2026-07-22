//! Integration: the `hangar/task_retry` RPC force-requeues a terminal task at an
//! operator's explicit request (the Task Kanban failed-column / task-detail `R`).
//!
//! The bite: a terminal `agent_error` task NEVER auto-retries (its
//! `RetryDisposition` is `NoRetry`), so the run-loop retry seam leaves it dead.
//! This handler is the HUMAN override — it must spawn a fresh `parent_task_id`-
//! chained `queued` child anyway, and publish a `TaskQueued` event so boards
//! re-pull and the new attempt card appears. We call `rpc::dispatch` directly (it
//! is IO-free) against a store seeded with a failed `agent_error` task and assert:
//! (a) the reply carries a `new_task_id`, (b) a fresh queued child exists chained
//! to the parent, (c) a `TaskQueued` event fired, and (d) a foreign task id is a
//! rejection, never a silent no-op.

use ainb_hangar_daemon::events::EventBroker;
use ainb_hangar_daemon::health_stats::HealthStats;
use ainb_hangar_daemon::rpc::{self, DaemonHealth};
use ainb_hangar_proto::events::HangarEvent;
use ainb_hangar_proto::{RpcId, RpcRequest, methods};
use ainb_hangar_store::Store;
use std::sync::Arc;
use std::time::Instant;

fn health() -> DaemonHealth {
    DaemonHealth {
        socket_path: "/tmp/task-retry.sock".into(),
        pid: 1,
        started_at: Instant::now(),
        version: "0.1.0".into(),
        stats: Arc::new(HealthStats::default()),
    }
}

/// Seed the workspace + user + runtime + agent + issue rows the task FKs require.
async fn seed_graph(store: &Store) {
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
    .bind("daemon-rt-1")
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
    .bind("agent-1")
    .bind("ws-1")
    .bind("Agent")
    .bind("rt-1")
    .bind("workspace")
    .bind("user-1")
    .bind(5_i64)
    .execute(pool)
    .await
    .expect("insert agent");
    sqlx::query(
        "INSERT INTO issue \
         (id, workspace_id, title, state, creator_type, creator_id, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind("issue-1")
    .bind("ws-1")
    .bind("An issue")
    .bind("open")
    .bind("member")
    .bind("user-1")
    .bind(0_i64)
    .execute(pool)
    .await
    .expect("insert issue");
}

/// Insert a task and drive it to `failed` with `agent_error` — the terminal that
/// the automatic retry path refuses. `attempt = max_attempts` so this doubly
/// proves the manual override (past both the reason gate AND the chain cap).
async fn seed_failed_agent_error(store: &Store) {
    use ainb_hangar_store::repo::task::{NewTask, TaskRepo};
    use ainb_hangar_store::service::fail::{FailTaskService, FailureReason};
    TaskRepo::insert(
        store.pool(),
        &NewTask {
            id: "t1".to_string(),
            workspace_id: "ws-1".to_string(),
            runtime_id: "rt-1".to_string(),
            agent_id: "agent-1".to_string(),
            issue_id: Some("issue-1".to_string()),
            work_dir: Some("/tmp/wd".to_string()),
            priority: 0,
            created_at: 1,
            autopilot_run_id: None,
            generation: 0,
        },
    )
    .await
    .expect("enqueue");
    sqlx::query(
        "UPDATE agent_task_queue SET attempt = 2, max_attempts = 2, status = 'running' WHERE id = 't1'",
    )
    .execute(store.pool())
    .await
    .expect("force attempt/state");
    FailTaskService::fail(
        store.pool(),
        "t1",
        FailureReason::AgentError,
        &ainb_hangar_core::clock::SystemClock,
    )
    .await
    .expect("fail seed task");
}

/// The queued child rows (other than the parent) for the workspace.
async fn queued_children(store: &Store) -> Vec<(String, Option<String>, i64)> {
    sqlx::query_as::<_, (String, Option<String>, i64)>(
        "SELECT id, parent_task_id, attempt FROM agent_task_queue \
         WHERE status = 'queued' ORDER BY id",
    )
    .fetch_all(store.pool())
    .await
    .expect("select queued children")
}

#[tokio::test]
async fn task_retry_force_requeues_terminal_agent_error() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    seed_graph(&store).await;
    seed_failed_agent_error(&store).await;

    let broker = EventBroker::new();
    let sink = broker.sink();
    let mut rx = broker.subscribe();
    let health = health();

    // Precondition: no queued children yet.
    assert!(
        queued_children(&store).await.is_empty(),
        "no children before retry"
    );

    let req = RpcRequest {
        jsonrpc: ainb_hangar_proto::jsonrpc_version(),
        id: RpcId::Number(1),
        method: methods::HANGAR_TASK_RETRY.into(),
        params: serde_json::json!({ "workspace_id": "ws-1", "task_id": "t1" }),
    };
    let resp = rpc::dispatch(store.pool(), &req, &health, &sink).await;
    assert!(
        resp.error.is_none(),
        "task_retry must ack: {:?}",
        resp.error
    );
    let new_task_id = resp
        .result
        .expect("result")
        .get("new_task_id")
        .and_then(|v| v.as_str())
        .expect("new_task_id present")
        .to_string();

    // A fresh queued child exists, chained to the parent, attempt past the cap.
    let children = queued_children(&store).await;
    assert_eq!(children.len(), 1, "exactly one requeued child");
    let (id, parent, attempt) = &children[0];
    assert_eq!(id, &new_task_id, "reply id matches the inserted row");
    assert_eq!(
        parent.as_deref(),
        Some("t1"),
        "child chains to the failed parent"
    );
    assert_eq!(
        *attempt, 3,
        "manual override grows attempt past max_attempts=2"
    );

    // A TaskQueued event fired so boards re-pull and the queued card appears.
    let scoped = rx.try_recv().expect("a TaskQueued event was published");
    assert_eq!(scoped.workspace_id, "ws-1");
    assert!(
        matches!(scoped.event, HangarEvent::TaskQueued { .. }),
        "expected TaskQueued, got {:?}",
        scoped.event
    );
}

#[tokio::test]
async fn task_retry_rejects_foreign_task_id() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    seed_graph(&store).await;
    let broker = EventBroker::new();
    let sink = broker.sink();
    let health = health();

    // A task id that does not exist must be rejected, never a silent no-op.
    let req = RpcRequest {
        jsonrpc: ainb_hangar_proto::jsonrpc_version(),
        id: RpcId::Number(2),
        method: methods::HANGAR_TASK_RETRY.into(),
        params: serde_json::json!({ "workspace_id": "ws-1", "task_id": "does-not-exist" }),
    };
    let resp = rpc::dispatch(store.pool(), &req, &health, &sink).await;
    assert!(resp.error.is_some(), "a foreign task id must be rejected");
}
