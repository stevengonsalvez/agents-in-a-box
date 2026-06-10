//! P1.5 retry-chain integration tests.
//!
//! Exercises [`RetryService::maybe_retry_failed`] against a real ephemeral
//! `SQLite` WAL database. When a task fails for a *retryable* reason and has
//! attempts remaining, the service spawns a fresh `queued` child row whose
//! `parent_task_id` points back at the failed task and whose `attempt` is the
//! parent's `attempt + 1`. Everything else (workspace / runtime / agent / issue
//! / `work_dir`) is inherited verbatim.
//!
//! Retry eligibility mirrors Multica migration 055: only `runtime_offline` and
//! `runtime_recovery` failures are retried automatically; `agent_error` (the LLM
//! mis-tooled / gave up) and `user_cancel` are terminal-by-intent and never spawn
//! a child. `attempt >= max_attempts` caps the chain regardless of reason.
//!
//! Each test uses an isolated tempdir store so parallel cargo threads do not
//! share state, and a [`FixedClock`] so the child's `created_at` is deterministic.

use ainb_hangar_core::clock::FixedClock;
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::task::{NewTask, Task, TaskRepo};
use ainb_hangar_store::service::fail::{FailTaskService, FailureReason};
use ainb_hangar_store::service::retry::{RetryDecision, RetryService};

/// Frozen "now" used for the retry child's `created_at` assertions.
const NOW_MS: i64 = 1_700_001_000_000;

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

/// Enqueue one task, optionally bound to an issue, then fail it for `reason` so
/// the row is `failed` with `failure_reason` populated. Optionally overrides the
/// task's `attempt` / `max_attempts` to exercise the chain cap. Returns the
/// re-read failed [`Task`].
async fn seed_failed_task(
    store: &Store,
    id: &str,
    issue_id: Option<&str>,
    work_dir: Option<&str>,
    attempt: i64,
    max_attempts: i64,
    reason: FailureReason,
) -> Task {
    TaskRepo::insert(
        store.pool(),
        &NewTask {
            id: id.to_string(),
            workspace_id: "ws-1".to_string(),
            runtime_id: "rt-1".to_string(),
            agent_id: "agent-1".to_string(),
            issue_id: issue_id.map(str::to_string),
            work_dir: work_dir.map(str::to_string),
            priority: 0,
            created_at: 1,
            autopilot_run_id: None,
        },
    )
    .await
    .expect("enqueue");
    // Force attempt/max_attempts and move to `running` so the fail service has a
    // legal source state.
    sqlx::query("UPDATE agent_task_queue SET attempt = ?, max_attempts = ?, status = 'running' WHERE id = ?")
        .bind(attempt)
        .bind(max_attempts)
        .bind(id)
        .execute(store.pool())
        .await
        .expect("force attempt/state");
    let clock = FixedClock(NOW_MS - 1);
    FailTaskService::fail(store.pool(), id, reason, &clock)
        .await
        .expect("fail seed task");
    TaskRepo::get_by_id(store.pool(), id).await.unwrap().unwrap()
}

async fn open_seeded() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed_graph(&store).await;
    (dir, store)
}

#[tokio::test]
async fn failure_reason_runtime_offline_spawns_child_row() {
    let (_dir, store) = open_seeded().await;
    let parent = seed_failed_task(
        &store,
        "t1",
        None,
        Some("/tmp/wd"),
        1,
        2,
        FailureReason::RuntimeOffline,
    )
    .await;
    let clock = FixedClock(NOW_MS);

    let decision = RetryService::maybe_retry_failed(store.pool(), &parent, "child-1", &clock)
        .await
        .expect("retry ok");
    assert_eq!(
        decision,
        RetryDecision::Spawned {
            new_task_id: "child-1".to_string()
        }
    );

    let child = TaskRepo::get_by_id(store.pool(), "child-1")
        .await
        .unwrap()
        .expect("child row exists");
    assert_eq!(child.parent_task_id.as_deref(), Some("t1"));
    assert_eq!(child.attempt, parent.attempt + 1);
    assert_eq!(child.status, "queued");
    assert_eq!(child.workspace_id, parent.workspace_id);
    assert_eq!(child.runtime_id, parent.runtime_id);
    assert_eq!(child.agent_id, parent.agent_id);
    assert_eq!(child.issue_id, parent.issue_id);
    assert_eq!(child.work_dir.as_deref(), Some("/tmp/wd"));
    assert_eq!(child.max_attempts, parent.max_attempts);
    assert_eq!(child.created_at, NOW_MS, "child queued_at = clock.now()");
    // Timestamps reset on the fresh attempt.
    assert_eq!(child.dispatched_at, None);
    assert_eq!(child.started_at, None);
    assert_eq!(child.finished_at, None);
    assert_eq!(child.result, None);
    assert_eq!(child.session_id, None);
    assert_eq!(child.failure_reason, None);
}

#[tokio::test]
async fn child_inherits_parent_priority() {
    // A retried urgent task must stay urgent: the child row inherits the
    // parent's `priority` (0..3 = P3..P0, higher = more urgent) so it keeps
    // its place in the claim ordering (`priority DESC, created_at, id`).
    let (_dir, store) = open_seeded().await;
    let parent_seed = seed_failed_task(
        &store,
        "t-urgent",
        None,
        None,
        1,
        2,
        FailureReason::RuntimeOffline,
    )
    .await;
    sqlx::query("UPDATE agent_task_queue SET priority = 3 WHERE id = ?")
        .bind(&parent_seed.id)
        .execute(store.pool())
        .await
        .expect("escalate parent to P0");
    let parent = TaskRepo::get_by_id(store.pool(), &parent_seed.id)
        .await
        .unwrap()
        .expect("parent re-reads");
    assert_eq!(parent.priority, 3, "parent escalated to P0");

    let clock = FixedClock(NOW_MS);
    let decision = RetryService::maybe_retry_failed(store.pool(), &parent, "child-urgent", &clock)
        .await
        .expect("retry ok");
    assert_eq!(
        decision,
        RetryDecision::Spawned {
            new_task_id: "child-urgent".to_string()
        }
    );

    let child = TaskRepo::get_by_id(store.pool(), "child-urgent")
        .await
        .unwrap()
        .expect("child row exists");
    assert_eq!(child.priority, 3, "child inherits the parent's priority");
}

#[tokio::test]
async fn child_row_is_created_atomically_with_retry_columns_set() {
    // Regression guard for the non-atomic two-statement child creation: the child
    // row must be inserted with attempt = parent.attempt + 1 and
    // parent_task_id = parent.id in a SINGLE statement, never first written with
    // the schema defaults (attempt=1, parent_task_id=NULL) and patched afterwards.
    //
    // A correctly-atomic insert is "correct-or-absent": there is no intermediate
    // state where the row exists with attempt=1 / parent_task_id=NULL. We assert
    // the only-ever-observable row already carries the retry columns, and that a
    // *failing* insert (per-issue UNIQUE collision) leaves NO orphan row at all —
    // which a two-statement path could not guarantee because the bare INSERT would
    // have already landed before the UPDATE ran.
    let (_dir, store) = open_seeded().await;
    let issue = seed_issue(&store, "issue-1").await;
    let parent = seed_failed_task(
        &store,
        "t1",
        Some(&issue),
        None,
        1,
        2,
        FailureReason::RuntimeOffline,
    )
    .await;

    // Happy path: the child carries the retry columns, never the bare defaults.
    let clock = FixedClock(NOW_MS);
    RetryService::maybe_retry_failed(store.pool(), &parent, "child-1", &clock)
        .await
        .expect("retry ok");
    let child = TaskRepo::get_by_id(store.pool(), "child-1")
        .await
        .unwrap()
        .expect("child exists");
    assert_eq!(
        child.attempt, 2,
        "child attempt set in the insert, not defaulted to 1"
    );
    assert_eq!(
        child.parent_task_id.as_deref(),
        Some("t1"),
        "child parent_task_id set in the insert, not defaulted to NULL"
    );

    // Move the child to a terminal state so it stops holding the per-issue slot,
    // fail t1's lineage again into a fresh failed parent on the same issue, then
    // make a *new* pending task hold the slot so the next retry insert MUST fail.
    sqlx::query("UPDATE agent_task_queue SET status='done', finished_at=? WHERE id='child-1'")
        .bind(NOW_MS)
        .execute(store.pool())
        .await
        .unwrap();
    // A fresh pending task grabs the per-issue slot.
    TaskRepo::insert(
        store.pool(),
        &NewTask {
            id: "blocker".to_string(),
            workspace_id: "ws-1".to_string(),
            runtime_id: "rt-1".to_string(),
            agent_id: "agent-1".to_string(),
            issue_id: Some(issue.clone()),
            work_dir: None,
            priority: 0,
            created_at: 6,
            autopilot_run_id: None,
        },
    )
    .await
    .expect("blocker holds the slot");

    // The retry insert now collides. Because it is a single statement, it must be
    // all-or-nothing: the failed insert leaves NO orphan "orphan-id" row.
    let _ = RetryService::maybe_retry_failed(store.pool(), &parent, "orphan-id", &clock)
        .await
        .expect_err("retry insert must collide with the pending-per-issue index");
    assert!(
        TaskRepo::get_by_id(store.pool(), "orphan-id").await.unwrap().is_none(),
        "a failed atomic insert must leave no orphan child row"
    );
}

#[tokio::test]
async fn failure_reason_agent_error_does_not_retry() {
    // agent_error is a user-facing failure (LLM mis-tooled, gave up). No retry
    // row. (Multica migration 055 behaviour.)
    let (_dir, store) = open_seeded().await;
    let parent = seed_failed_task(&store, "t1", None, None, 1, 2, FailureReason::AgentError).await;
    let clock = FixedClock(NOW_MS);

    let decision = RetryService::maybe_retry_failed(store.pool(), &parent, "child-1", &clock)
        .await
        .expect("decision ok");
    assert_eq!(decision, RetryDecision::DoNotRetry);
    assert!(TaskRepo::get_by_id(store.pool(), "child-1").await.unwrap().is_none());
}

#[tokio::test]
async fn failure_reason_user_cancel_does_not_retry() {
    // user_cancel is terminal-by-intent.
    let (_dir, store) = open_seeded().await;
    let parent = seed_failed_task(&store, "t1", None, None, 1, 2, FailureReason::UserCancel).await;
    let clock = FixedClock(NOW_MS);

    let decision = RetryService::maybe_retry_failed(store.pool(), &parent, "child-1", &clock)
        .await
        .expect("decision ok");
    assert_eq!(decision, RetryDecision::DoNotRetry);
    assert!(TaskRepo::get_by_id(store.pool(), "child-1").await.unwrap().is_none());
}

#[tokio::test]
async fn failure_reason_runtime_recovery_retries_once() {
    // After daemon recovery via recover-orphans, eligible reasons spawn a child.
    let (_dir, store) = open_seeded().await;
    let parent = seed_failed_task(
        &store,
        "t1",
        None,
        None,
        1,
        2,
        FailureReason::RuntimeRecovery,
    )
    .await;
    let clock = FixedClock(NOW_MS);

    let decision = RetryService::maybe_retry_failed(store.pool(), &parent, "child-1", &clock)
        .await
        .expect("retry ok");
    assert_eq!(
        decision,
        RetryDecision::Spawned {
            new_task_id: "child-1".to_string()
        }
    );

    let child = TaskRepo::get_by_id(store.pool(), "child-1")
        .await
        .unwrap()
        .expect("child row exists");
    assert_eq!(child.attempt, 2);
    assert_eq!(child.parent_task_id.as_deref(), Some("t1"));
}

#[tokio::test]
async fn max_attempts_caps_retry_chain() {
    // attempt=2, max_attempts=2: failing with a retryable reason yields no child
    // row; the parent stays `failed`. Chain length is bounded.
    let (_dir, store) = open_seeded().await;
    let parent = seed_failed_task(
        &store,
        "t1",
        None,
        None,
        2,
        2,
        FailureReason::RuntimeOffline,
    )
    .await;
    let clock = FixedClock(NOW_MS);

    let decision = RetryService::maybe_retry_failed(store.pool(), &parent, "child-1", &clock)
        .await
        .expect("decision ok");
    assert_eq!(decision, RetryDecision::DoNotRetry);
    assert!(TaskRepo::get_by_id(store.pool(), "child-1").await.unwrap().is_none());

    let still = TaskRepo::get_by_id(store.pool(), "t1").await.unwrap().unwrap();
    assert_eq!(still.status, "failed");
}

#[tokio::test]
async fn retry_chain_walk_via_parent_task_id() {
    // Fail+retry twice, then walk parent_task_id from leaf to root: a chain of 3
    // rows in attempt order. Verifies the linkage is genuinely traversable.
    let (_dir, store) = open_seeded().await;
    let clock = FixedClock(NOW_MS);

    // Root attempt fails (runtime_offline) -> spawns mid.
    let root = seed_failed_task(
        &store,
        "root",
        None,
        None,
        1,
        3,
        FailureReason::RuntimeOffline,
    )
    .await;
    let d1 = RetryService::maybe_retry_failed(store.pool(), &root, "mid", &clock)
        .await
        .expect("retry root");
    assert_eq!(
        d1,
        RetryDecision::Spawned {
            new_task_id: "mid".to_string()
        }
    );

    // Mid attempt fails -> spawns leaf. Move mid to running then fail it.
    sqlx::query("UPDATE agent_task_queue SET status='running' WHERE id='mid'")
        .execute(store.pool())
        .await
        .unwrap();
    FailTaskService::fail(store.pool(), "mid", FailureReason::RuntimeOffline, &clock)
        .await
        .unwrap();
    let mid = TaskRepo::get_by_id(store.pool(), "mid").await.unwrap().unwrap();
    let d2 = RetryService::maybe_retry_failed(store.pool(), &mid, "leaf", &clock)
        .await
        .expect("retry mid");
    assert_eq!(
        d2,
        RetryDecision::Spawned {
            new_task_id: "leaf".to_string()
        }
    );

    // Walk leaf -> mid -> root via parent_task_id.
    let mut chain = Vec::new();
    let mut cursor = Some("leaf".to_string());
    while let Some(id) = cursor {
        let row = TaskRepo::get_by_id(store.pool(), &id).await.unwrap().unwrap();
        cursor = row.parent_task_id.clone();
        chain.push(row);
    }
    let ids: Vec<&str> = chain.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, vec!["leaf", "mid", "root"]);
    // Attempts ascend root(1) -> mid(2) -> leaf(3).
    let attempts: Vec<i64> = chain.iter().rev().map(|t| t.attempt).collect();
    assert_eq!(attempts, vec![1, 2, 3]);
}

#[tokio::test]
async fn partial_unique_index_blocks_retry_when_existing_pending() {
    // The failed task carries an issue_id; a *new* manually-enqueued queued task
    // already exists for the same issue AND the same agent. The retry insert
    // collides with idx_one_pending_task_per_issue_agent and surfaces the UNIQUE
    // error (Multica raises + logs; we mirror by propagating the DB error).
    let (_dir, store) = open_seeded().await;
    let issue = seed_issue(&store, "issue-1").await;
    let parent = seed_failed_task(
        &store,
        "t1",
        Some(&issue),
        None,
        1,
        2,
        FailureReason::RuntimeOffline,
    )
    .await;

    // A fresh pending task already holds the per-issue slot.
    TaskRepo::insert(
        store.pool(),
        &NewTask {
            id: "manual".to_string(),
            workspace_id: "ws-1".to_string(),
            runtime_id: "rt-1".to_string(),
            agent_id: "agent-1".to_string(),
            issue_id: Some(issue.clone()),
            work_dir: None,
            priority: 0,
            created_at: 5,
            autopilot_run_id: None,
        },
    )
    .await
    .expect("manual enqueue holds the slot");

    let clock = FixedClock(NOW_MS);
    let err = RetryService::maybe_retry_failed(store.pool(), &parent, "child-1", &clock)
        .await
        .expect_err("retry insert must collide with the pending-per-issue index");
    let msg = err.to_string();
    assert!(
        msg.contains("idx_one_pending_task_per_issue") || msg.to_lowercase().contains("unique"),
        "expected UNIQUE constraint error, got: {msg}"
    );
}
