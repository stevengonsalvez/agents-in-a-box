//! e38.19 — the three concurrency policies ACTUALLY change scheduler behavior
//! under contention (not just that the enum is stored).
//!
//! Each test drives the real [`AutopilotScheduler`] loop against an ephemeral
//! `SQLite`, an [`AdvanceableClock`] (so the scheduled slots pass instantly), and
//! `max_concurrent_runs = 1`. The first tick fires and its run stays IN FLIGHT
//! (its task is never completed), so the second tick comes due AT the in-flight
//! limit — the contention every policy resolves differently:
//!
//! ```text
//!            advance #1 (fires)        advance #2 (at the limit)
//! skip    ─▶ 1 run / 1 task      ─▶  STILL 1 run / 1 task   (second dropped)
//! queue   ─▶ 1 run / 1 task      ─▶  2 runs / 2 tasks       (fires anyway)
//! replace ─▶ 1 run (running)     ─▶  2 runs total, run #1 is now `cancelled`,
//!                                    a fresh run #2 is `running`  (superseded)
//! ```
//!
//! The behavioral difference is asserted against the REAL row state (run counts,
//! task counts, run #1's status), not the emitted event alone — proving the
//! policy genuinely changed what the scheduler did.

use std::sync::Arc;
use std::time::Duration;

use ainb_hangar_core::clock::{FixedClock, HangarClock};
use ainb_hangar_core::ids::{AgentId, WorkspaceId};
use ainb_hangar_daemon::scheduler::{AdvanceableClock, AutopilotScheduler, SchedulerEvent};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::autopilot::{ConcurrencyPolicy, ExecutionMode, NewAutopilot};
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

/// Create one `max_concurrent_runs = 1` autopilot under the given policy.
async fn seed_autopilot_with_policy(
    pool: &SqlitePool,
    policy: ConcurrencyPolicy,
) -> ainb_hangar_core::ids::AutopilotId {
    let req = NewAutopilot {
        workspace_id: WorkspaceId::from_str("ws-1").unwrap(),
        agent_id: AgentId::from_str("ag-1").unwrap(),
        name: "policy".to_string(),
        instructions: None,
        cron_expr: "*/5 * * * *".to_string(),
        max_concurrent_runs: 1,
        execution_mode: ExecutionMode::RunOnly,
        concurrency_policy: policy,
        api_trigger_enabled: false,
    };
    let create_clock = FixedClock(T0);
    ainb_hangar_store::repo::autopilot::AutopilotRepo::create(pool, &create_clock, &req)
        .await
        .expect("create autopilot")
}

/// Spawn the scheduler loop wired to an advanceable clock + event sink.
fn spawn_scheduler(
    pool: &SqlitePool,
    clock: &AdvanceableClock,
    shutdown: &CancellationToken,
) -> (
    mpsc::UnboundedReceiver<SchedulerEvent>,
    tokio::task::JoinHandle<()>,
) {
    let (tx, rx) = mpsc::unbounded_channel();
    let clock_dyn: Arc<dyn HangarClock> = Arc::new(clock.clone());
    let sched = AutopilotScheduler::new(pool.clone(), clock_dyn, shutdown.clone())
        .with_event_sink(tx)
        .with_wake(clock.wake_handle());
    let handle = tokio::spawn(sched.run());
    (rx, handle)
}

async fn next_event(
    rx: &mut mpsc::UnboundedReceiver<SchedulerEvent>,
    timeout: Duration,
) -> Option<SchedulerEvent> {
    tokio::time::timeout(timeout, rx.recv()).await.ok().flatten()
}

#[tokio::test]
async fn skip_policy_drops_the_second_tick_under_contention() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let pool = store.pool().clone();
    seed_parents(&pool).await;
    seed_autopilot_with_policy(&pool, ConcurrencyPolicy::Skip).await;

    let clock = AdvanceableClock::new(T0);
    let shutdown = CancellationToken::new();
    let (mut rx, handle) = spawn_scheduler(&pool, &clock, &shutdown);

    // advance #1: first tick fires.
    clock.advance(FIVE_MIN_MS);
    let ev = next_event(&mut rx, Duration::from_secs(2)).await;
    assert!(
        matches!(ev, Some(SchedulerEvent::Fired { .. })),
        "first tick must fire, got {ev:?}"
    );
    assert_eq!(count(&pool, "SELECT count(*) FROM autopilot_run").await, 1);

    // advance #2: at the limit (run #1 still in flight) → SKIPPED: no new task,
    // and the decline recorded as a terminal `skipped` run (migration 0057).
    clock.advance(FIVE_MIN_MS);
    let ev = next_event(&mut rx, Duration::from_secs(2)).await;
    assert!(
        matches!(
            ev,
            Some(SchedulerEvent::TickSkipped {
                reason: "concurrency",
                ..
            })
        ),
        "second tick at the limit must be skipped, got {ev:?}"
    );
    // The skip enqueues NO work (the invariant that matters) …
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM autopilot_run WHERE status <> 'skipped'"
        )
        .await,
        1,
        "skip must not create a second LIVE run"
    );
    assert_eq!(
        count(&pool, "SELECT count(*) FROM agent_task_queue").await,
        1,
        "skip must not enqueue a second task"
    );
    // … but since migration 0057 it IS recorded: a terminal `skipped` run with
    // its trigger source and the admission reason, so a declined dispatch is
    // visible to every read path instead of existing only as a log line.
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM autopilot_run \
             WHERE status = 'skipped' AND source = 'schedule' \
               AND completed_at IS NOT NULL \
               AND failure_reason LIKE 'concurrency limit%'"
        )
        .await,
        1,
        "the declined dispatch must be persisted as a skipped run"
    );

    shutdown.cancel();
    handle.await.expect("scheduler joins");
}

#[tokio::test]
async fn queue_policy_fires_the_second_tick_anyway_under_contention() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let pool = store.pool().clone();
    seed_parents(&pool).await;
    seed_autopilot_with_policy(&pool, ConcurrencyPolicy::Queue).await;

    let clock = AdvanceableClock::new(T0);
    let shutdown = CancellationToken::new();
    let (mut rx, handle) = spawn_scheduler(&pool, &clock, &shutdown);

    // advance #1: first tick fires.
    clock.advance(FIVE_MIN_MS);
    let ev = next_event(&mut rx, Duration::from_secs(2)).await;
    assert!(
        matches!(ev, Some(SchedulerEvent::Fired { .. })),
        "first tick must fire, got {ev:?}"
    );
    assert_eq!(count(&pool, "SELECT count(*) FROM autopilot_run").await, 1);

    // advance #2: at the limit, but the QUEUE policy fires anyway — a second run
    // + task are enqueued (the claim queue runs them in order). The behavioral
    // difference vs skip: 2 runs / 2 tasks, not 1 / 1.
    clock.advance(FIVE_MIN_MS);
    let ev = next_event(&mut rx, Duration::from_secs(2)).await;
    assert!(
        matches!(ev, Some(SchedulerEvent::Fired { .. })),
        "queue policy must fire the second tick, got {ev:?}"
    );
    assert_eq!(
        count(&pool, "SELECT count(*) FROM autopilot_run").await,
        2,
        "queue must create the second run (vs skip dropping it)"
    );
    assert_eq!(
        count(&pool, "SELECT count(*) FROM agent_task_queue").await,
        2,
        "queue must enqueue the second task"
    );
    // BOTH runs are in flight — the queue serializes them at CLAIM time, not by
    // dropping the fire.
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM autopilot_run WHERE completed_at IS NULL"
        )
        .await,
        2,
        "queue leaves both runs in flight (drained in order by the claim queue)"
    );

    shutdown.cancel();
    handle.await.expect("scheduler joins");
}

#[tokio::test]
async fn replace_policy_supersedes_the_in_flight_run_under_contention() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let pool = store.pool().clone();
    seed_parents(&pool).await;
    seed_autopilot_with_policy(&pool, ConcurrencyPolicy::Replace).await;

    let clock = AdvanceableClock::new(T0);
    let shutdown = CancellationToken::new();
    let (mut rx, handle) = spawn_scheduler(&pool, &clock, &shutdown);

    // advance #1: first tick fires; capture run #1's id + its task.
    clock.advance(FIVE_MIN_MS);
    let ev = next_event(&mut rx, Duration::from_secs(2)).await;
    let run1_id = match ev {
        Some(SchedulerEvent::Fired { run_id, .. }) => run_id,
        other => panic!("first tick must fire, got {other:?}"),
    };
    assert_eq!(count(&pool, "SELECT count(*) FROM autopilot_run").await, 1);
    let task1_status_before: String =
        sqlx::query_scalar("SELECT status FROM agent_task_queue LIMIT 1")
            .fetch_one(&pool)
            .await
            .expect("task #1 status");
    assert_eq!(task1_status_before, "queued", "run #1's task is queued");

    // advance #2: at the limit, the REPLACE policy supersedes run #1 (cancels it
    // + its task) and fires a fresh run #2. The behavioral difference vs skip:
    // run #1 is now `cancelled` with completed_at set, and a brand-new `running`
    // run exists.
    clock.advance(FIVE_MIN_MS);
    let ev = next_event(&mut rx, Duration::from_secs(2)).await;
    assert!(
        matches!(ev, Some(SchedulerEvent::Fired { .. })),
        "replace policy must fire a fresh tick, got {ev:?}"
    );
    assert_eq!(
        count(&pool, "SELECT count(*) FROM autopilot_run").await,
        2,
        "replace creates a fresh run #2 alongside the superseded run #1"
    );

    // Run #1 is now superseded: cancelled, with completed_at stamped.
    let run1 = sqlx::query("SELECT status, completed_at FROM autopilot_run WHERE id = ?")
        .bind(&run1_id)
        .fetch_one(&pool)
        .await
        .expect("run #1 row");
    assert_eq!(
        run1.get::<String, _>("status"),
        "cancelled",
        "replace must cancel the superseded run #1"
    );
    assert!(
        run1.get::<Option<i64>, _>("completed_at").is_some(),
        "the superseded run #1 must have completed_at stamped"
    );

    // Exactly ONE run is now in flight — the fresh run #2.
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM autopilot_run WHERE completed_at IS NULL"
        )
        .await,
        1,
        "after replace, only the fresh run is in flight"
    );

    // Run #1's task was cancelled too (the work is abandoned).
    let cancelled_tasks = count(
        &pool,
        "SELECT count(*) FROM agent_task_queue WHERE status = 'cancelled'",
    )
    .await;
    assert_eq!(
        cancelled_tasks, 1,
        "replace cancels the superseded run's task"
    );

    shutdown.cancel();
    handle.await.expect("scheduler joins");
}
