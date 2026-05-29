//! P1.3 concurrent-finalize race test (arch review §6: WS-vs-HTTP race).
//!
//! Two callers race to finalize the same `running` task: one `CompleteTask`, one
//! `CancelTask`. Under the atomic conditional UPDATE, the first transactional
//! UPDATE wins (the row lands in exactly one terminal state); the loser's UPDATE
//! affects 0 rows, re-reads, and resolves deterministically:
//! - if the winner happened to land the loser's own target state →
//!   [`FinalizeOutcome::AlreadyTerminal`] (cannot happen here — the two targets
//!   differ — but the path is exercised by `finalize_idempotency.rs`),
//! - otherwise → [`FinalizeError::TerminalMismatch`].
//!
//! No row corruption, no double-state: the row is `done` XOR `cancelled` after
//! the race, never both, never something else.

use ainb_hangar_core::clock::FixedClock;
use ainb_hangar_store::repo::task::{NewTask, TaskRepo};
use ainb_hangar_store::service::cancel::CancelTaskService;
use ainb_hangar_store::service::complete::{CompleteParams, CompleteTaskService};
use ainb_hangar_store::service::finalize::{FinalizeError, FinalizeOutcome};
use ainb_hangar_store::Store;

const NOW_MS: i64 = 1_700_001_000_000;

async fn seed(store: &Store) {
    let pool = store.pool();
    sqlx::query("INSERT OR IGNORE INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
        .bind("ws-1")
        .bind("alpha")
        .bind("Alpha")
        .bind(0_i64)
        .execute(pool)
        .await
        .expect("ws");
    sqlx::query("INSERT OR IGNORE INTO user (id, email, created_at) VALUES (?, ?, ?)")
        .bind("user-1")
        .bind("a@example.com")
        .bind(0_i64)
        .execute(pool)
        .await
        .expect("user");
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
    .expect("runtime");
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
    .expect("agent");
    TaskRepo::insert(
        pool,
        &NewTask {
            id: "t1".to_string(),
            workspace_id: "ws-1".to_string(),
            runtime_id: "rt-1".to_string(),
            agent_id: "agent-1".to_string(),
            issue_id: None,
            work_dir: None,
            created_at: 1,
        },
    )
    .await
    .expect("enqueue");
    sqlx::query("UPDATE agent_task_queue SET status = 'running' WHERE id = 't1'")
        .execute(pool)
        .await
        .expect("force running");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_complete_vs_cancel_first_wins_loser_resolves() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed(&store).await;

    let p1 = store.pool().clone();
    let p2 = store.pool().clone();
    let clock = FixedClock(NOW_MS);

    let (rc, rcancel) = tokio::join!(
        async move {
            CompleteTaskService::complete(
                &p1,
                "t1",
                CompleteParams {
                    result: serde_json::json!({ "content": "ok" }),
                    session_id: Some("s".to_string()),
                    work_dir: None,
                },
                &clock,
            )
            .await
        },
        async move { CancelTaskService::cancel(&p2, "t1", &clock).await },
    );

    // Exactly one of the two must have won the transition; the other must have
    // lost cleanly (re-read found the OTHER terminal -> TerminalMismatch).
    let winners = [
        matches!(rc, Ok(FinalizeOutcome::Transitioned)),
        matches!(rcancel, Ok(FinalizeOutcome::Transitioned)),
    ]
    .into_iter()
    .filter(|w| *w)
    .count();
    assert_eq!(winners, 1, "exactly one finalizer must win the race");

    let losers = [
        matches!(rc, Err(FinalizeError::TerminalMismatch { .. })),
        matches!(rcancel, Err(FinalizeError::TerminalMismatch { .. })),
    ]
    .into_iter()
    .filter(|l| *l)
    .count();
    assert_eq!(losers, 1, "the loser must re-read and report TerminalMismatch");

    // The row landed in exactly one terminal state, never corrupted.
    let row = TaskRepo::get_by_id(store.pool(), "t1")
        .await
        .unwrap()
        .unwrap();
    assert!(
        row.status == "done" || row.status == "cancelled",
        "row must be done XOR cancelled, got {}",
        row.status
    );
    assert_eq!(row.finished_at, Some(NOW_MS));
}
