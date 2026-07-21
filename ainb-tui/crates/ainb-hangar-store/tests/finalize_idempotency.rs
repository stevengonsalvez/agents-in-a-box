//! P1.3 finalize-service integration tests.
//!
//! Exercises the four FSM finalize services — Start / Complete / Fail / Cancel —
//! against a real ephemeral `SQLite` WAL database. Each test uses an isolated
//! tempdir store so parallel cargo threads do not share state, and a
//! [`FixedClock`] so the lifecycle timestamps (`started_at` / `finished_at`) are
//! deterministic.
//!
//! The finalize contract (mirrors the reference control plane `task.go:1010` idempotent finalize):
//! - a transition that mutates exactly one row returns
//!   [`FinalizeOutcome::Transitioned`],
//! - a replayed transition (0-row UPDATE) re-reads the row: if it is already in
//!   the target terminal state it returns [`FinalizeOutcome::AlreadyTerminal`]
//!   (idempotent success), if it is in a *different* terminal state it returns
//!   [`FinalizeError::TerminalMismatch`] (never silently flip a terminal row),
//! - a transition attempted from an illegal non-terminal state returns
//!   [`FinalizeError::IllegalState`].

use ainb_hangar_core::clock::FixedClock;
use ainb_hangar_core::task::state::TaskState;
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::task::{NewTask, TaskRepo};
use ainb_hangar_store::service::cancel::CancelTaskService;
use ainb_hangar_store::service::complete::{CompleteParams, CompleteTaskService};
use ainb_hangar_store::service::fail::{FailTaskService, FailureReason};
use ainb_hangar_store::service::finalize::{FinalizeError, FinalizeOutcome};
use ainb_hangar_store::service::start::StartTaskService;

/// Frozen "now" used for `started_at` / `finished_at` assertions.
const NOW_MS: i64 = 1_700_000_900_000;

/// Seed the workspace + user + runtime + agent rows the task FKs require.
async fn seed_graph(store: &Store) {
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
}

/// Enqueue one task as `queued` and force it into `status`.
async fn seed_task_in_state(store: &Store, id: &str, status: &str) {
    TaskRepo::insert(
        store.pool(),
        &NewTask {
            id: id.to_string(),
            workspace_id: "ws-1".to_string(),
            runtime_id: "rt-1".to_string(),
            agent_id: "agent-1".to_string(),
            issue_id: None,
            work_dir: None,
            priority: 0,
            created_at: 1,
            autopilot_run_id: None,
            generation: 0,
        },
    )
    .await
    .expect("enqueue");
    if status != "queued" {
        sqlx::query("UPDATE agent_task_queue SET status = ? WHERE id = ?")
            .bind(status)
            .bind(id)
            .execute(store.pool())
            .await
            .expect("force status");
    }
}

async fn open_seeded() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed_graph(&store).await;
    (dir, store)
}

// ---- StartTask -----------------------------------------------------------

#[tokio::test]
async fn start_dispatched_marks_running() {
    let (_dir, store) = open_seeded().await;
    seed_task_in_state(&store, "t1", "dispatched").await;
    let clock = FixedClock(NOW_MS);

    let outcome = StartTaskService::start(store.pool(), "t1", &clock).await.expect("start ok");
    assert_eq!(outcome, FinalizeOutcome::Transitioned);

    let row = TaskRepo::get_by_id(store.pool(), "t1").await.unwrap().unwrap();
    assert_eq!(row.status, "running");
    assert_eq!(row.started_at, Some(NOW_MS));
}

#[tokio::test]
async fn start_already_running_returns_already_started_err() {
    // Second start on a row already `running` errors; status unchanged.
    let (_dir, store) = open_seeded().await;
    seed_task_in_state(&store, "t1", "running").await;
    let clock = FixedClock(NOW_MS);

    let err = StartTaskService::start(store.pool(), "t1", &clock)
        .await
        .expect_err("starting an already-running task must error");
    assert!(matches!(err, FinalizeError::AlreadyStarted));

    let row = TaskRepo::get_by_id(store.pool(), "t1").await.unwrap().unwrap();
    assert_eq!(row.status, "running", "status must not change");
}

#[tokio::test]
async fn start_queued_returns_illegal_state_err() {
    // illegal-state: a still-`queued` task has not been claimed/dispatched yet,
    // so it has no legal `dispatched -> running` transition. classify_no_op
    // re-reads `Queued`, the AlreadyStarted special-case (re-found `Running`)
    // does not apply, so it falls through to IllegalState.
    let (_dir, store) = open_seeded().await;
    seed_task_in_state(&store, "t1", "queued").await;
    let clock = FixedClock(NOW_MS);

    let err = StartTaskService::start(store.pool(), "t1", &clock)
        .await
        .expect_err("starting a queued task is illegal");
    assert!(
        matches!(err, FinalizeError::IllegalState { found: Some(s), target } if s == TaskState::Queued && target == TaskState::Running),
        "expected IllegalState{{found: Some(Queued), target: Running}}, got {err:?}"
    );

    let row = TaskRepo::get_by_id(store.pool(), "t1").await.unwrap().unwrap();
    assert_eq!(row.status, "queued", "status must not change");
}

#[tokio::test]
async fn start_done_returns_terminal_mismatch_err() {
    // terminal-replay: starting a row that already reached a terminal state
    // (`done`) re-reads a *different* terminal and must reject rather than
    // resurrect a finished task.
    let (_dir, store) = open_seeded().await;
    seed_task_in_state(&store, "t1", "done").await;
    let clock = FixedClock(NOW_MS);

    let err = StartTaskService::start(store.pool(), "t1", &clock)
        .await
        .expect_err("starting a done task must mismatch");
    assert!(
        matches!(err, FinalizeError::TerminalMismatch { found, target } if found == TaskState::Done && target == TaskState::Running),
        "expected TerminalMismatch{{found: Done, target: Running}}, got {err:?}"
    );
}

#[tokio::test]
async fn start_absent_returns_illegal_state_none() {
    // absent-row: starting a task whose row does not exist re-reads `None` and
    // returns IllegalState{found: None}.
    let (_dir, store) = open_seeded().await;
    let clock = FixedClock(NOW_MS);

    let err = StartTaskService::start(store.pool(), "ghost", &clock)
        .await
        .expect_err("starting an absent task is illegal");
    assert!(
        matches!(err, FinalizeError::IllegalState { found: None, target } if target == TaskState::Running),
        "expected IllegalState{{found: None, target: Running}}, got {err:?}"
    );
}

// ---- CompleteTask --------------------------------------------------------

#[tokio::test]
async fn complete_running_marks_done_with_result() {
    let (_dir, store) = open_seeded().await;
    seed_task_in_state(&store, "t1", "running").await;
    let clock = FixedClock(NOW_MS);

    let params = CompleteParams {
        result: serde_json::json!({ "content": "ok" }),
        session_id: Some("sess-9".to_string()),
        work_dir: Some("/tmp/wd".to_string()),
    };
    let outcome = CompleteTaskService::complete(store.pool(), "t1", params, &clock)
        .await
        .expect("complete ok");
    assert_eq!(outcome, FinalizeOutcome::Transitioned);

    let row = TaskRepo::get_by_id(store.pool(), "t1").await.unwrap().unwrap();
    assert_eq!(row.status, "done");
    assert_eq!(row.session_id.as_deref(), Some("sess-9"));
    assert_eq!(row.work_dir.as_deref(), Some("/tmp/wd"));
    assert_eq!(row.finished_at, Some(NOW_MS));
    let stored: serde_json::Value =
        serde_json::from_str(row.result.as_deref().expect("result json")).unwrap();
    assert_eq!(stored["content"], "ok");
}

#[tokio::test]
async fn complete_already_done_returns_success() {
    // result_idempotent: a 0-row UPDATE re-reads the row; finding it already
    // `done` returns Ok(AlreadyTerminal). Verbatim reference task.go:1010.
    let (_dir, store) = open_seeded().await;
    seed_task_in_state(&store, "t1", "done").await;
    let clock = FixedClock(NOW_MS);

    let params = CompleteParams {
        result: serde_json::json!({ "content": "ok" }),
        session_id: None,
        work_dir: None,
    };
    let outcome = CompleteTaskService::complete(store.pool(), "t1", params, &clock)
        .await
        .expect("idempotent re-complete is success");
    assert_eq!(outcome, FinalizeOutcome::AlreadyTerminal);
}

#[tokio::test]
async fn complete_already_failed_returns_terminal_mismatch_err() {
    // Re-read finds `failed` (a different terminal): return TerminalMismatch.
    // Never silently flip one terminal state to another.
    let (_dir, store) = open_seeded().await;
    seed_task_in_state(&store, "t1", "failed").await;
    let clock = FixedClock(NOW_MS);

    let params = CompleteParams {
        result: serde_json::json!({}),
        session_id: None,
        work_dir: None,
    };
    let err = CompleteTaskService::complete(store.pool(), "t1", params, &clock)
        .await
        .expect_err("completing a failed task must mismatch");
    assert!(matches!(err, FinalizeError::TerminalMismatch { .. }));
}

#[tokio::test]
async fn complete_dispatched_returns_illegal_state_err() {
    // illegal-state: a `dispatched` task has been claimed but the agent never
    // confirmed start, so `running -> done` is illegal. classify_no_op re-reads
    // `Dispatched` (non-terminal, unexpected) -> IllegalState.
    let (_dir, store) = open_seeded().await;
    seed_task_in_state(&store, "t1", "dispatched").await;
    let clock = FixedClock(NOW_MS);

    let params = CompleteParams {
        result: serde_json::json!({}),
        session_id: None,
        work_dir: None,
    };
    let err = CompleteTaskService::complete(store.pool(), "t1", params, &clock)
        .await
        .expect_err("completing a dispatched task is illegal");
    assert!(
        matches!(err, FinalizeError::IllegalState { found: Some(s), target } if s == TaskState::Dispatched && target == TaskState::Done),
        "expected IllegalState{{found: Some(Dispatched), target: Done}}, got {err:?}"
    );

    let row = TaskRepo::get_by_id(store.pool(), "t1").await.unwrap().unwrap();
    assert_eq!(row.status, "dispatched", "status must not change");
}

#[tokio::test]
async fn complete_absent_returns_illegal_state_none() {
    // absent-row: completing a task whose row does not exist re-reads `None`.
    let (_dir, store) = open_seeded().await;
    let clock = FixedClock(NOW_MS);

    let params = CompleteParams {
        result: serde_json::json!({}),
        session_id: None,
        work_dir: None,
    };
    let err = CompleteTaskService::complete(store.pool(), "ghost", params, &clock)
        .await
        .expect_err("completing an absent task is illegal");
    assert!(
        matches!(err, FinalizeError::IllegalState { found: None, target } if target == TaskState::Done),
        "expected IllegalState{{found: None, target: Done}}, got {err:?}"
    );
}

// ---- FailTask ------------------------------------------------------------

#[tokio::test]
async fn fail_running_with_reason() {
    let (_dir, store) = open_seeded().await;
    seed_task_in_state(&store, "t1", "running").await;
    let clock = FixedClock(NOW_MS);

    let outcome = FailTaskService::fail(store.pool(), "t1", FailureReason::Timeout, &clock)
        .await
        .expect("fail ok");
    assert_eq!(outcome, FinalizeOutcome::Transitioned);

    let row = TaskRepo::get_by_id(store.pool(), "t1").await.unwrap().unwrap();
    assert_eq!(row.status, "failed");
    assert_eq!(row.failure_reason.as_deref(), Some("timeout"));
    assert_eq!(row.finished_at, Some(NOW_MS));
}

#[tokio::test]
async fn fail_running_with_spawn_timeout_terminalizes() {
    // Doctrine hardening (D-e2e-3): when the `running -> provider spawn` setup
    // phase wedges past its umbrella bound, the daemon terminalises the row
    // `running -> failed` with `SpawnTimeout` — the SAME finalize path a real
    // spawn failure takes — so a wedged setup becomes a loud, terminal, attributed
    // failure instead of a forever-`running` row that only the multi-hour running
    // TTL would ever relabel (and then only as a causeless `timeout`).
    let (_dir, store) = open_seeded().await;
    seed_task_in_state(&store, "t1", "running").await;
    let clock = FixedClock(NOW_MS);

    let outcome = FailTaskService::fail(store.pool(), "t1", FailureReason::SpawnTimeout, &clock)
        .await
        .expect("fail ok");
    assert_eq!(outcome, FinalizeOutcome::Transitioned);

    let row = TaskRepo::get_by_id(store.pool(), "t1").await.unwrap().unwrap();
    assert_eq!(row.status, "failed");
    // The cause is persisted and distinct from a causeless TTL `timeout`, so the
    // board + detail can show WHY the setup wedged.
    assert_eq!(row.failure_reason.as_deref(), Some("spawn_timeout"));
    assert_eq!(row.finished_at, Some(NOW_MS));
}

#[tokio::test]
async fn fail_reason_serializes_snake_case() {
    // FailureReason serializes to the snake_case tokens the reference's
    // failure_reason column uses.
    let (_dir, store) = open_seeded().await;
    seed_task_in_state(&store, "t1", "running").await;
    let clock = FixedClock(NOW_MS);

    FailTaskService::fail(store.pool(), "t1", FailureReason::RuntimeOffline, &clock)
        .await
        .expect("fail ok");
    let row = TaskRepo::get_by_id(store.pool(), "t1").await.unwrap().unwrap();
    assert_eq!(row.failure_reason.as_deref(), Some("runtime_offline"));
}

#[tokio::test]
async fn fail_already_failed_returns_success() {
    let (_dir, store) = open_seeded().await;
    seed_task_in_state(&store, "t1", "failed").await;
    let clock = FixedClock(NOW_MS);

    let outcome = FailTaskService::fail(store.pool(), "t1", FailureReason::Unknown, &clock)
        .await
        .expect("idempotent re-fail is success");
    assert_eq!(outcome, FinalizeOutcome::AlreadyTerminal);
}

#[tokio::test]
async fn fail_queued_via_ttl_path_marks_failed() {
    // The P1.4 queued-TTL sweeper's path: a still-`queued` task that exceeded
    // its TTL is failed directly from `queued`. `queued` is the second legal
    // source state in fail.rs's expected_from / WHERE clause.
    let (_dir, store) = open_seeded().await;
    seed_task_in_state(&store, "t1", "queued").await;
    let clock = FixedClock(NOW_MS);

    let outcome = FailTaskService::fail(store.pool(), "t1", FailureReason::Timeout, &clock)
        .await
        .expect("fail from queued ok");
    assert_eq!(outcome, FinalizeOutcome::Transitioned);

    let row = TaskRepo::get_by_id(store.pool(), "t1").await.unwrap().unwrap();
    assert_eq!(row.status, "failed");
    assert_eq!(row.failure_reason.as_deref(), Some("timeout"));
    assert_eq!(row.finished_at, Some(NOW_MS));
}

#[tokio::test]
async fn fail_dispatched_returns_illegal_state_err() {
    // illegal-state: `dispatched` is neither a legal fail source (only `running`
    // / `queued`) nor terminal, so failing it is illegal.
    let (_dir, store) = open_seeded().await;
    seed_task_in_state(&store, "t1", "dispatched").await;
    let clock = FixedClock(NOW_MS);

    let err = FailTaskService::fail(store.pool(), "t1", FailureReason::AgentError, &clock)
        .await
        .expect_err("failing a dispatched task is illegal");
    assert!(
        matches!(err, FinalizeError::IllegalState { found: Some(s), target } if s == TaskState::Dispatched && target == TaskState::Failed),
        "expected IllegalState{{found: Some(Dispatched), target: Failed}}, got {err:?}"
    );

    let row = TaskRepo::get_by_id(store.pool(), "t1").await.unwrap().unwrap();
    assert_eq!(row.status, "dispatched", "status must not change");
}

// ---- CancelTask ----------------------------------------------------------

#[tokio::test]
async fn cancel_dispatched_or_running() {
    for state in ["dispatched", "running"] {
        let (_dir, store) = open_seeded().await;
        let id = format!("t-{state}");
        seed_task_in_state(&store, &id, state).await;
        let clock = FixedClock(NOW_MS);

        let outcome = CancelTaskService::cancel(store.pool(), &id, &clock)
            .await
            .unwrap_or_else(|e| panic!("cancel from {state} should succeed: {e}"));
        assert_eq!(outcome, FinalizeOutcome::Transitioned);

        let row = TaskRepo::get_by_id(store.pool(), &id).await.unwrap().unwrap();
        assert_eq!(row.status, "cancelled");
        assert_eq!(row.finished_at, Some(NOW_MS));
    }
}

#[tokio::test]
async fn cancel_queued_also_legal() {
    // User-cancel-before-claim path: a still-queued task may be cancelled.
    let (_dir, store) = open_seeded().await;
    seed_task_in_state(&store, "t1", "queued").await;
    let clock = FixedClock(NOW_MS);

    let outcome = CancelTaskService::cancel(store.pool(), "t1", &clock)
        .await
        .expect("cancel queued ok");
    assert_eq!(outcome, FinalizeOutcome::Transitioned);

    let row = TaskRepo::get_by_id(store.pool(), "t1").await.unwrap().unwrap();
    assert_eq!(row.status, "cancelled");
}

#[tokio::test]
async fn cancel_already_cancelled_returns_success() {
    // Same idempotent re-read pattern as complete.
    let (_dir, store) = open_seeded().await;
    seed_task_in_state(&store, "t1", "cancelled").await;
    let clock = FixedClock(NOW_MS);

    let outcome = CancelTaskService::cancel(store.pool(), "t1", &clock)
        .await
        .expect("idempotent re-cancel is success");
    assert_eq!(outcome, FinalizeOutcome::AlreadyTerminal);
}

#[tokio::test]
async fn cancel_done_returns_terminal_mismatch_err() {
    // A successfully-completed task must not be flipped to cancelled.
    let (_dir, store) = open_seeded().await;
    seed_task_in_state(&store, "t1", "done").await;
    let clock = FixedClock(NOW_MS);

    let err = CancelTaskService::cancel(store.pool(), "t1", &clock)
        .await
        .expect_err("cancelling a done task must mismatch");
    assert!(matches!(err, FinalizeError::TerminalMismatch { .. }));
}

#[tokio::test]
async fn cancel_absent_returns_illegal_state_none() {
    // absent-row: cancelling a task whose row does not exist re-reads `None` and
    // returns IllegalState{found: None}.
    let (_dir, store) = open_seeded().await;
    let clock = FixedClock(NOW_MS);

    let err = CancelTaskService::cancel(store.pool(), "ghost", &clock)
        .await
        .expect_err("cancelling an absent task is illegal");
    assert!(
        matches!(err, FinalizeError::IllegalState { found: None, target } if target == TaskState::Cancelled),
        "expected IllegalState{{found: None, target: Cancelled}}, got {err:?}"
    );
}
