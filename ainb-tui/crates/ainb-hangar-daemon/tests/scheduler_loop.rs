//! P7.3 — autopilot scheduler loop behavioural tests.
//!
//! These exercise the *running loop* end to end against a real ephemeral
//! `SQLite`, driven by a controllable clock so there is no real wall-clock wait:
//! an autopilot whose `next_tick_at` equals the injected `now` is immediately
//! due (`sleep_delay` clamps to zero), so the loop fires on the first iteration.
//! A [`CancellationToken`] tears the loop down deterministically and the
//! event-sink channel reports the fire / skip decisions without scraping logs.
//!
//! The pure decision functions (`pick_earliest`, `sleep_delay`,
//! `recompute_next_tick`, skip-at-limit) are unit-tested in
//! `scheduler.rs::tests`; this file covers the wiring the unit tests cannot: a
//! real fire creates the run + task rows, a skip leaves the queue untouched,
//! a disabled autopilot is ignored, and shutdown exits promptly.

use std::sync::Arc;
use std::time::Duration;

use ainb_hangar_core::clock::{FixedClock, HangarClock};
use ainb_hangar_daemon::scheduler::{AutopilotScheduler, SchedulerEvent};
use ainb_hangar_store::Store;
use sqlx::{Row, SqlitePool};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// 2026-01-01T00:00:00Z in epoch-ms — the frozen `now` for these tests.
const T0: i64 = 1_767_225_600_000;

/// Seed the workspace + user + runtime + agent parent rows an autopilot needs.
async fn seed_parents(pool: &SqlitePool) {
    sqlx::query(
        "INSERT INTO workspace (id, slug, name, created_at) VALUES ('ws-1', 'test', 'Test', 0)",
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

/// Insert one autopilot row directly (the scheduler reads, fires, reschedules).
async fn insert_autopilot(
    pool: &SqlitePool,
    id: &str,
    cron: &str,
    next_tick_at: Option<i64>,
    max_concurrent_runs: i64,
    enabled: bool,
) {
    sqlx::query(
        "INSERT INTO autopilot \
         (id, workspace_id, agent_id, name, instructions, cron_expr, \
          max_concurrent_runs, next_tick_at, enabled, created_at) \
         VALUES (?, 'ws-1', 'ag-1', ?, NULL, ?, ?, ?, ?, 0)",
    )
    .bind(id)
    .bind(id)
    .bind(cron)
    .bind(max_concurrent_runs)
    .bind(next_tick_at)
    .bind(i64::from(enabled))
    .execute(pool)
    .await
    .expect("insert autopilot");
}

async fn count(pool: &SqlitePool, sql: &str) -> i64 {
    sqlx::query(sql).fetch_one(pool).await.expect("count query").get::<i64, _>(0)
}

/// Drain the event channel for up to `timeout`, returning the first event (or
/// `None` if none arrives). Avoids a fixed sleep racing the loop.
async fn recv_event(
    rx: &mut mpsc::UnboundedReceiver<SchedulerEvent>,
    timeout: Duration,
) -> Option<SchedulerEvent> {
    tokio::time::timeout(timeout, rx.recv()).await.ok().flatten()
}

#[tokio::test]
async fn single_autopilot_fires_at_next_tick() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let pool = store.pool().clone();
    seed_parents(&pool).await;
    // Due immediately: next_tick_at == clock now ⇒ sleep_delay == 0.
    insert_autopilot(&pool, "ap-fire", "*/5 * * * *", Some(T0), 1, true).await;

    let (tx, mut rx) = mpsc::unbounded_channel();
    let shutdown = CancellationToken::new();
    let clock: Arc<dyn HangarClock> = Arc::new(FixedClock(T0));
    let sched = AutopilotScheduler::new(pool.clone(), clock, shutdown.clone()).with_event_sink(tx);
    let handle = tokio::spawn(sched.run());

    let ev = recv_event(&mut rx, Duration::from_secs(2)).await;
    shutdown.cancel();
    handle.await.expect("loop joins");

    match ev {
        Some(SchedulerEvent::Fired { autopilot_id, .. }) => assert_eq!(autopilot_id, "ap-fire"),
        other => panic!("expected Fired, got {other:?}"),
    }
    // A run + a task row now exist, and next_tick_at was advanced past T0.
    assert_eq!(count(&pool, "SELECT count(*) FROM autopilot_run").await, 1);
    assert_eq!(
        count(&pool, "SELECT count(*) FROM agent_task_queue").await,
        1
    );
    let next: Option<i64> = sqlx::query("SELECT next_tick_at FROM autopilot WHERE id = 'ap-fire'")
        .fetch_one(&pool)
        .await
        .unwrap()
        .get(0);
    assert_eq!(
        next,
        Some(T0 + 5 * 60_000),
        "rescheduled to the next 5-min slot"
    );
}

#[tokio::test]
async fn skip_when_prior_run_in_flight() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let pool = store.pool().clone();
    seed_parents(&pool).await;
    insert_autopilot(&pool, "ap-skip", "*/5 * * * *", Some(T0), 1, true).await;
    // Pre-seed one in-flight run (completed_at NULL) so the autopilot is already
    // at its max_concurrent_runs=1 limit.
    sqlx::query(
        "INSERT INTO autopilot_run (id, autopilot_id, started_at, status) \
         VALUES ('run-existing', 'ap-skip', 0, 'running')",
    )
    .execute(&pool)
    .await
    .expect("seed in-flight run");

    let (tx, mut rx) = mpsc::unbounded_channel();
    let shutdown = CancellationToken::new();
    let clock: Arc<dyn HangarClock> = Arc::new(FixedClock(T0));
    let sched = AutopilotScheduler::new(pool.clone(), clock, shutdown.clone()).with_event_sink(tx);
    let handle = tokio::spawn(sched.run());

    let ev = recv_event(&mut rx, Duration::from_secs(2)).await;
    shutdown.cancel();
    handle.await.expect("loop joins");

    match ev {
        Some(SchedulerEvent::TickSkipped {
            autopilot_id,
            reason,
            in_flight,
        }) => {
            assert_eq!(autopilot_id, "ap-skip");
            assert_eq!(reason, "concurrency");
            assert_eq!(in_flight, 1);
        }
        other => panic!("expected TickSkipped, got {other:?}"),
    }
    // No new run, no new task — only the pre-seeded run remains.
    assert_eq!(count(&pool, "SELECT count(*) FROM autopilot_run").await, 1);
    assert_eq!(
        count(&pool, "SELECT count(*) FROM agent_task_queue").await,
        0
    );
    // But it was still rescheduled forward (rejoins the schedule next slot).
    let next: Option<i64> = sqlx::query("SELECT next_tick_at FROM autopilot WHERE id = 'ap-skip'")
        .fetch_one(&pool)
        .await
        .unwrap()
        .get(0);
    assert_eq!(next, Some(T0 + 5 * 60_000));
}

#[tokio::test]
async fn disabled_autopilot_is_not_fired() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let pool = store.pool().clone();
    seed_parents(&pool).await;
    // Disabled + due: must be ignored (load_enabled filters enabled = 1).
    insert_autopilot(&pool, "ap-off", "*/5 * * * *", Some(T0), 1, false).await;

    let (tx, mut rx) = mpsc::unbounded_channel();
    let shutdown = CancellationToken::new();
    let clock: Arc<dyn HangarClock> = Arc::new(FixedClock(T0));
    let sched = AutopilotScheduler::new(pool.clone(), clock, shutdown.clone()).with_event_sink(tx);
    let handle = tokio::spawn(sched.run());

    // With no enabled autopilot the loop falls into the 60s re-poll; no event
    // should arrive in a short window.
    let ev = recv_event(&mut rx, Duration::from_millis(300)).await;
    shutdown.cancel();
    handle.await.expect("loop joins");

    assert!(ev.is_none(), "a disabled autopilot must not fire: {ev:?}");
    assert_eq!(count(&pool, "SELECT count(*) FROM autopilot_run").await, 0);
}

#[tokio::test]
async fn shutdown_token_exits_loop_promptly() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let pool = store.pool().clone();
    seed_parents(&pool).await;
    // No enabled autopilots ⇒ loop is parked in the 60s re-poll select. Shutdown
    // must still tear it down within the select, not after 60s.

    let shutdown = CancellationToken::new();
    let clock: Arc<dyn HangarClock> = Arc::new(FixedClock(T0));
    let sched = AutopilotScheduler::new(pool, clock, shutdown.clone());
    let handle = tokio::spawn(sched.run());

    shutdown.cancel();
    // Generous bound that is still far under the 60s re-poll; proves the select
    // honours cancellation rather than waiting out the sleep.
    let joined = tokio::time::timeout(Duration::from_secs(2), handle).await;
    assert!(
        joined.is_ok(),
        "loop did not exit within 2s of cancellation"
    );
    joined.unwrap().expect("loop joins cleanly");
}

#[tokio::test]
async fn two_autopilots_earliest_fires_first() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let pool = store.pool().clone();
    seed_parents(&pool).await;
    // "soon" is due now; "later" is an hour out. The loop must pick "soon".
    insert_autopilot(&pool, "soon", "*/5 * * * *", Some(T0), 1, true).await;
    insert_autopilot(&pool, "later", "0 * * * *", Some(T0 + 3_600_000), 1, true).await;

    let (tx, mut rx) = mpsc::unbounded_channel();
    let shutdown = CancellationToken::new();
    let clock: Arc<dyn HangarClock> = Arc::new(FixedClock(T0));
    let sched = AutopilotScheduler::new(pool.clone(), clock, shutdown.clone()).with_event_sink(tx);
    let handle = tokio::spawn(sched.run());

    let ev = recv_event(&mut rx, Duration::from_secs(2)).await;
    shutdown.cancel();
    handle.await.expect("loop joins");

    match ev {
        Some(SchedulerEvent::Fired { autopilot_id, .. }) => {
            assert_eq!(
                autopilot_id, "soon",
                "the earliest-due autopilot fires first"
            );
        }
        other => panic!("expected Fired(soon), got {other:?}"),
    }
}
