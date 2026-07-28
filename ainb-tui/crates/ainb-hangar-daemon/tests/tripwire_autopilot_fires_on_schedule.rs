//! P7.6 e2e tripwire A — an autopilot fires on schedule under a fast-forwarded
//! clock.
//!
//! End-to-end, in-process: a real ephemeral `SQLite`, the real
//! [`AutopilotScheduler`] loop, and the P7.4 enqueue path, driven by an
//! [`AdvanceableClock`] so minutes pass instantly. There is no real wall-clock
//! wait and no provider subprocess — the agent runtime is a stub (we seed the
//! `agent`/`agent_runtime` rows the FK needs and then complete the enqueued task
//! through the real finalize service, exactly like P6.5's capstone asserts on
//! rows rather than on provider output).
//!
//! # The clock-advance / `sleep_until` reconciliation
//!
//! The scheduler computes its sleep from `(next_tick_at - clock.now_ms())` and
//! parks in `tokio::time::sleep`, which counts down against *real* time. Mutating
//! an injected clock while the loop is parked would not wake it. The fix (see
//! `scheduler.rs` module docs) is a [`WakeHandle`]: [`AdvanceableClock::advance`]
//! bumps the injected epoch-ms **and** fires the wake, the scheduler `select!`s
//! over `sleep + shutdown + wake`, and on a wake re-evaluates against the new
//! `now`. So `advance(5min)` makes the not-yet-due tick become due and fire
//! immediately and deterministically.
//!
//! Sequence:
//! ```text
//! create(cron "*/5 * * * *") → next_tick_at == t0+5min
//! advance(5min) + wake       → loop recomputes, tick now due, fires
//! poll ≤2s for the task row  → agent_task_queue has autopilot_run_id
//! complete the task          → autopilot_run.completed_at stamped, status done
//! ```

use std::sync::Arc;
use std::time::Duration;

use ainb_hangar_core::clock::HangarClock;
use ainb_hangar_core::ids::{AgentId, WorkspaceId};
use ainb_hangar_daemon::scheduler::{AdvanceableClock, AutopilotScheduler, SchedulerEvent};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::autopilot::{AutopilotRepo, NewAutopilot};
use ainb_hangar_store::service::complete::{CompleteParams, CompleteTaskService};
use sqlx::{Row, SqlitePool};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// 2026-01-01T00:00:00Z in epoch-ms — the frozen start instant.
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

/// Poll for the first autopilot task row to appear, up to `timeout`. Returns
/// `(task_id, autopilot_run_id)` once one exists; panics on timeout so a stuck
/// scheduler fails loudly rather than hanging.
async fn await_autopilot_task(pool: &SqlitePool, timeout: Duration) -> (String, String) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let row = sqlx::query(
            "SELECT id, autopilot_run_id FROM agent_task_queue \
             WHERE autopilot_run_id IS NOT NULL LIMIT 1",
        )
        .fetch_optional(pool)
        .await
        .expect("poll task");
        if let Some(r) = row {
            let task_id: String = r.get("id");
            let run_id: String = r.get("autopilot_run_id");
            return (task_id, run_id);
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no autopilot task enqueued within {timeout:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn autopilot_fires_on_schedule_after_clock_advance() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let pool = store.pool().clone();
    seed_parents(&pool).await;

    // Create the autopilot through the real repo (cron validated before insert).
    // Use a fixed instant for the create so next_tick_at is deterministic; the
    // scheduler then runs on the advanceable clock starting at the same instant.
    let req = NewAutopilot {
        workspace_id: WorkspaceId::from_str("ws-1").unwrap(),
        agent_id: AgentId::from_str("ag-1").unwrap(),
        name: "smoke".to_string(),
        instructions: Some("say hi".to_string()),
        cron_expr: "*/5 * * * *".to_string(),
        max_concurrent_runs: 1,
        execution_mode: ainb_hangar_store::repo::autopilot::ExecutionMode::default(),
        concurrency_policy: ainb_hangar_store::repo::autopilot::ConcurrencyPolicy::default(),
        api_trigger_enabled: false,
    };
    let create_clock = ainb_hangar_core::clock::FixedClock(T0);
    let autopilot_id = AutopilotRepo::create(&pool, &create_clock, &req)
        .await
        .expect("create autopilot");

    // next_tick_at is the next 5-min slot strictly after t0 → t0+5min.
    let next: Option<i64> = sqlx::query("SELECT next_tick_at FROM autopilot WHERE id = ?")
        .bind(autopilot_id.as_str())
        .fetch_one(&pool)
        .await
        .unwrap()
        .get(0);
    assert_eq!(
        next,
        Some(T0 + FIVE_MIN_MS),
        "next tick is the next 5-min slot"
    );

    // Boot the scheduler on the advanceable clock + wake handle.
    let clock = AdvanceableClock::new(T0);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let shutdown = CancellationToken::new();
    let clock_dyn: Arc<dyn HangarClock> = Arc::new(clock.clone());
    let sched = AutopilotScheduler::new(pool.clone(), clock_dyn, shutdown.clone())
        .with_event_sink(tx)
        .with_wake(clock.wake_handle());
    let handle = tokio::spawn(sched.run());

    // Nothing should fire yet — the tick is 5 minutes out and only ~ms of real
    // time has passed.
    assert!(
        tokio::time::timeout(Duration::from_millis(200), rx.recv()).await.is_err(),
        "autopilot must not fire before its tick is due"
    );

    // Fast-forward 5 minutes: the tick becomes due, the wake unparks the loop.
    clock.advance(FIVE_MIN_MS);

    // The fire event arrives (deterministic — the wake makes the tick due now).
    let ev = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("a fire event within 2s of advance")
        .expect("event channel open");
    let SchedulerEvent::Fired {
        autopilot_id: fired_id,
        ..
    } = ev
    else {
        panic!("expected Fired, got {ev:?}");
    };
    assert_eq!(fired_id, autopilot_id.as_str());

    // The task row exists, linked to the run.
    let (task_id, run_id) = await_autopilot_task(&pool, Duration::from_secs(2)).await;

    // 0056 / multica parity #21 — the REAL-DAEMON provenance leg: the task the
    // live scheduler fired carries ('autopilot', <autopilot.id>) in sqlite. The
    // id is the RULE, not the run (multica `service/autopilot.go:145` binds
    // `ap.ID`), which is what makes "which issues did THIS autopilot create" a
    // stable query across runs.
    let origin = sqlx::query("SELECT origin_type, origin_id FROM agent_task_queue WHERE id = ?")
        .bind(&task_id)
        .fetch_one(&pool)
        .await
        .expect("task origin");
    assert_eq!(
        origin.get::<Option<String>, _>("origin_type").as_deref(),
        Some("autopilot"),
        "a scheduler-fired task records its provenance kind"
    );
    assert_eq!(
        origin.get::<Option<String>, _>("origin_id").as_deref(),
        Some(autopilot_id.as_str()),
        "origin_id is the autopilot id"
    );
    assert_ne!(
        origin.get::<Option<String>, _>("origin_id").as_deref(),
        Some(run_id.as_str()),
        "never the run id"
    );

    shutdown.cancel();
    handle.await.expect("scheduler loop joins");

    // Drive the task to done through the real finalize path so the run cascade
    // fires (the stub "agent runtime": no provider, we just complete the row).
    sqlx::query("UPDATE agent_task_queue SET status = 'running' WHERE id = ?")
        .bind(&task_id)
        .execute(&pool)
        .await
        .expect("force running");
    let complete_clock = ainb_hangar_core::clock::FixedClock(T0 + FIVE_MIN_MS + 60_000);
    CompleteTaskService::complete(
        &pool,
        &task_id,
        CompleteParams {
            result: serde_json::json!({"ok": true}),
            session_id: None,
            work_dir: None,
        },
        &complete_clock,
    )
    .await
    .expect("complete task");

    // The autopilot_run completed_at is stamped and status flipped to completed.
    let run_row = sqlx::query("SELECT completed_at, status FROM autopilot_run WHERE id = ?")
        .bind(&run_id)
        .fetch_one(&pool)
        .await
        .expect("run row");
    assert!(
        run_row.get::<Option<i64>, _>("completed_at").is_some(),
        "completing the task must stamp the run's completed_at"
    );
    assert_eq!(
        run_row.get::<String, _>("status"),
        "completed",
        "a done task completes its run"
    );

    // `autopilot list` (via the repo's run history) now shows a completed last run.
    let runs = AutopilotRepo::list_runs(
        &pool,
        &WorkspaceId::from_str("ws-1").unwrap(),
        &autopilot_id,
        1,
    )
    .await
    .expect("list runs");
    assert_eq!(runs.len(), 1, "exactly one run recorded");
    assert_eq!(runs[0].status, "completed", "last run is ok/completed");
}
