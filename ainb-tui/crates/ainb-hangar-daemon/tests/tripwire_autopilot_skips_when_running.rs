//! P7.6 e2e tripwire B — a tick is skipped while a prior run is in flight.
//!
//! Exercises the `max_concurrent_runs = 1` concurrency policy end-to-end against
//! a real ephemeral `SQLite`, the real [`AutopilotScheduler`] loop, and an
//! [`AdvanceableClock`] so the three scheduled slots pass instantly (see tripwire
//! A's module docs for the clock-advance / `sleep_until` reconciliation).
//!
//! Sequence:
//! ```text
//! advance #1 → task #1 enqueued (1 run, 1 task)         — fires normally
//! advance #2 → SKIPPED, reason "concurrency"            — task #1 still running
//!              (still 1 run, still 1 task; TickSkipped observed)
//! complete #1
//! advance #3 → task #2 enqueued (2 runs, 2 tasks)       — concurrency freed
//! ```
//!
//! The skip is REAL: after advance #2 the queue is genuinely untouched (the row
//! counts are asserted, not just the event), proving the scheduler did not fire
//! a second run while one was in flight.

use std::sync::Arc;
use std::time::Duration;

use ainb_hangar_core::clock::{FixedClock, HangarClock};
use ainb_hangar_core::ids::{AgentId, WorkspaceId};
use ainb_hangar_daemon::scheduler::{AdvanceableClock, AutopilotScheduler, SchedulerEvent};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::autopilot::{AutopilotRepo, NewAutopilot};
use ainb_hangar_store::service::complete::{CompleteParams, CompleteTaskService};
use sqlx::{Row, SqlitePool};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const T0: i64 = 1_767_225_600_000;
const FIVE_MIN_MS: i64 = 5 * 60_000;

async fn seed_parents(pool: &SqlitePool) {
    sqlx::query(
        "INSERT INTO workspace (id, slug, name, created_at) VALUES ('ws-1', 'default', 'D', 0)",
    )
    .execute(pool)
    .await
    .expect("seed workspace");
    sqlx::query("INSERT INTO user (id, email, created_at) VALUES ('u-1', 'a@b.c', 0)")
        .execute(pool)
        .await
        .expect("seed user");
    sqlx::query(
        "INSERT INTO agent_runtime (id, workspace_id, daemon_id, provider, runtime_mode, status) \
         VALUES ('rt-1', 'ws-1', 'd-1', 'claude', 'local', 'online')",
    )
    .execute(pool)
    .await
    .expect("seed runtime");
    sqlx::query(
        "INSERT INTO agent (id, workspace_id, name, runtime_id, visibility, owner_id) \
         VALUES ('ag-1', 'ws-1', 'Tester', 'rt-1', 'workspace', 'u-1')",
    )
    .execute(pool)
    .await
    .expect("seed agent");
}

async fn count(pool: &SqlitePool, sql: &str) -> i64 {
    sqlx::query(sql).fetch_one(pool).await.expect("count").get::<i64, _>(0)
}

/// Drain one event within `timeout`, or `None`.
async fn next_event(
    rx: &mut mpsc::UnboundedReceiver<SchedulerEvent>,
    timeout: Duration,
) -> Option<SchedulerEvent> {
    tokio::time::timeout(timeout, rx.recv()).await.ok().flatten()
}

#[tokio::test]
async fn autopilot_skips_tick_when_prior_run_in_flight() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let pool = store.pool().clone();
    seed_parents(&pool).await;

    let req = NewAutopilot {
        workspace_id: WorkspaceId::from_str("ws-1").unwrap(),
        agent_id: AgentId::from_str("ag-1").unwrap(),
        name: "skipper".to_string(),
        instructions: None,
        cron_expr: "*/5 * * * *".to_string(),
        max_concurrent_runs: 1,
    };
    let create_clock = FixedClock(T0);
    let autopilot_id = AutopilotRepo::create(&pool, &create_clock, &req)
        .await
        .expect("create autopilot");

    let clock = AdvanceableClock::new(T0);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let shutdown = CancellationToken::new();
    let clock_dyn: Arc<dyn HangarClock> = Arc::new(clock.clone());
    let sched = AutopilotScheduler::new(pool.clone(), clock_dyn, shutdown.clone())
        .with_event_sink(tx)
        .with_wake(clock.wake_handle());
    let handle = tokio::spawn(sched.run());

    // ── advance #1: tick fires normally ────────────────────────────────────
    clock.advance(FIVE_MIN_MS);
    let ev = next_event(&mut rx, Duration::from_secs(2)).await;
    assert!(
        matches!(ev, Some(SchedulerEvent::Fired { .. })),
        "first tick must fire, got {ev:?}"
    );
    assert_eq!(count(&pool, "SELECT count(*) FROM autopilot_run").await, 1);
    assert_eq!(
        count(&pool, "SELECT count(*) FROM agent_task_queue").await,
        1
    );

    // Capture run #1's task so we can complete it later. It stays in flight
    // (completed_at NULL) so the autopilot is at its max_concurrent_runs limit.
    let task1_id: String = sqlx::query("SELECT id FROM agent_task_queue LIMIT 1")
        .fetch_one(&pool)
        .await
        .unwrap()
        .get(0);

    // ── advance #2: at the limit → skipped, no new rows ─────────────────────
    clock.advance(FIVE_MIN_MS);
    let ev = next_event(&mut rx, Duration::from_secs(2)).await;
    match ev {
        Some(SchedulerEvent::TickSkipped {
            reason, in_flight, ..
        }) => {
            assert_eq!(reason, "concurrency", "skip reason must be concurrency");
            assert_eq!(in_flight, 1, "one run in flight at the limit");
        }
        other => panic!("expected TickSkipped(concurrency), got {other:?}"),
    }
    // The skip is REAL — the queue and run table are genuinely untouched.
    assert_eq!(
        count(&pool, "SELECT count(*) FROM autopilot_run").await,
        1,
        "skip must not create a second run"
    );
    assert_eq!(
        count(&pool, "SELECT count(*) FROM agent_task_queue").await,
        1,
        "skip must not enqueue a second task"
    );

    // ── complete #1, freeing the concurrency slot ───────────────────────────
    sqlx::query("UPDATE agent_task_queue SET status = 'running' WHERE id = ?")
        .bind(&task1_id)
        .execute(&pool)
        .await
        .expect("force running");
    let complete_clock = FixedClock(T0 + 2 * FIVE_MIN_MS + 60_000);
    CompleteTaskService::complete(
        &pool,
        &task1_id,
        CompleteParams {
            result: serde_json::json!({"ok": true}),
            session_id: None,
            work_dir: None,
        },
        &complete_clock,
    )
    .await
    .expect("complete task #1");
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM autopilot_run WHERE completed_at IS NULL"
        )
        .await,
        0,
        "run #1 is now completed (no in-flight runs)"
    );

    // ── advance #3: concurrency freed → task #2 enqueues normally ───────────
    clock.advance(FIVE_MIN_MS);
    let ev = next_event(&mut rx, Duration::from_secs(2)).await;
    assert!(
        matches!(ev, Some(SchedulerEvent::Fired { .. })),
        "with the slot freed, the next tick fires; got {ev:?}"
    );
    assert_eq!(
        count(&pool, "SELECT count(*) FROM autopilot_run").await,
        2,
        "a second run now exists"
    );
    assert_eq!(
        count(&pool, "SELECT count(*) FROM agent_task_queue").await,
        2,
        "task #2 enqueued"
    );

    shutdown.cancel();
    handle.await.expect("scheduler loop joins");

    let _ = autopilot_id;
}
