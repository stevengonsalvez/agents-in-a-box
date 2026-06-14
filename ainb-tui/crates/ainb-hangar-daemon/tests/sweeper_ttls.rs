//! P1.4 TTL-sweeper integration tests.
//!
//! Exercises the three lifecycle sweepers against a real ephemeral `SQLite` WAL
//! database. Every test injects a [`FixedClock`] and advances "now" by
//! constructing fresh clocks at arbitrary offsets — there is **zero**
//! `tokio::time::sleep` here, so the suite is fully deterministic (a wall-clock
//! leak would break the offset assertions immediately).
//!
//! Reference control-plane source-line references (the invariants these tests pin):
//! - queued TTL = 2h            (`runtime_sweeper.go:52`)
//! - running TTL = 2.5h         (`runtime_sweeper.go:40`)
//! - dispatched reclaim window  = 90s for lost claim responses (`task.go:85`)
//! - dispatched TTL            = 5min before the stale dispatch fails

// The TTL `Duration`s are minutes/hours, so their millisecond count always fits
// `i64` with vast headroom; the `as i64` casts on `Duration::as_millis()` in the
// clock-offset arithmetic below are exact in practice.
#![allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]

use ainb_hangar_core::clock::FixedClock;
use ainb_hangar_daemon::sweeper::{
    DISPATCHED_TTL, QUEUED_TTL, RECLAIM_WINDOW, RUNNING_TTL, SweeperConfig,
    reclaim_orphaned_on_startup, reclaim_stale_dispatched, sweep_expired_queued,
    sweep_stale_dispatched, sweep_stale_running,
};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::task::{NewTask, TaskRepo};
use sqlx::{Row, SqlitePool};

/// A frozen base "now" all the offset arithmetic hangs off.
const BASE_MS: i64 = 1_700_000_000_000;

/// Seed the workspace + user + runtime + agent rows the task FKs require.
async fn seed_graph(pool: &SqlitePool) {
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

/// Enqueue one task as `queued`, then force it into `status`. When `status` is
/// `dispatched` the `dispatched_at` column is stamped to `dispatched_at_ms`.
async fn seed_task(
    pool: &SqlitePool,
    id: &str,
    status: &str,
    created_at: i64,
    dispatched_at: Option<i64>,
) {
    TaskRepo::insert(
        pool,
        &NewTask {
            id: id.to_string(),
            workspace_id: "ws-1".to_string(),
            runtime_id: "rt-1".to_string(),
            agent_id: "agent-1".to_string(),
            issue_id: None,
            work_dir: None,
            priority: 0,
            created_at,
            autopilot_run_id: None,
        },
    )
    .await
    .expect("enqueue");
    if status != "queued" {
        sqlx::query("UPDATE agent_task_queue SET status = ?, dispatched_at = ? WHERE id = ?")
            .bind(status)
            .bind(dispatched_at)
            .bind(id)
            .execute(pool)
            .await
            .expect("force status");
    } else if let Some(d) = dispatched_at {
        sqlx::query("UPDATE agent_task_queue SET dispatched_at = ? WHERE id = ?")
            .bind(d)
            .bind(id)
            .execute(pool)
            .await
            .expect("force dispatched_at");
    }
}

/// Read back `(status, failure_reason, attempt, dispatched_at)` for assertions.
async fn read_task(pool: &SqlitePool, id: &str) -> (String, Option<String>, i64, Option<i64>) {
    let row = sqlx::query(
        "SELECT status, failure_reason, attempt, dispatched_at FROM agent_task_queue WHERE id = ?",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .expect("read task");
    (
        row.try_get("status").unwrap(),
        row.try_get("failure_reason").unwrap(),
        row.try_get("attempt").unwrap(),
        row.try_get("dispatched_at").unwrap(),
    )
}

async fn open_seeded() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed_graph(store.pool()).await;
    (dir, store)
}

// ---- queued TTL (2h) ------------------------------------------------------

/// `runtime_sweeper.go:52` — a queued task older than 2h is failed with
/// `failure_reason = timeout`.
#[tokio::test]
async fn queued_ttl_2h_fails_task() {
    let (_dir, store) = open_seeded().await;
    // created_at = BASE; clock advanced past the 2h TTL by 1s.
    seed_task(store.pool(), "q1", "queued", BASE_MS, None).await;
    let clock = FixedClock(BASE_MS + QUEUED_TTL.as_millis() as i64 + 1_000);

    let swept = sweep_expired_queued(store.pool(), &clock, &SweeperConfig::default())
        .await
        .expect("sweep ok");
    assert_eq!(swept, 1, "exactly one queued task swept");

    let (status, reason, _, _) = read_task(store.pool(), "q1").await;
    assert_eq!(status, "failed");
    assert_eq!(reason.as_deref(), Some("timeout"));
}

/// A queued task still inside the 2h window is untouched.
#[tokio::test]
async fn queued_under_ttl_untouched() {
    let (_dir, store) = open_seeded().await;
    seed_task(store.pool(), "q1", "queued", BASE_MS, None).await;
    // +1h59m — one minute short of the TTL.
    let clock = FixedClock(BASE_MS + QUEUED_TTL.as_millis() as i64 - 60_000);

    let swept = sweep_expired_queued(store.pool(), &clock, &SweeperConfig::default())
        .await
        .expect("sweep ok");
    assert_eq!(swept, 0, "nothing swept inside the TTL");

    let (status, reason, _, _) = read_task(store.pool(), "q1").await;
    assert_eq!(status, "queued");
    assert_eq!(reason, None);
}

// ---- dispatched TTL (5min) + reclaim window (90s) -------------------------

/// A dispatched task older than 5min is failed with `failure_reason = timeout`.
#[tokio::test]
async fn dispatched_ttl_5min_fails_task() {
    let (_dir, store) = open_seeded().await;
    let dispatched_at = BASE_MS;
    seed_task(
        store.pool(),
        "d1",
        "dispatched",
        BASE_MS,
        Some(dispatched_at),
    )
    .await;
    // +5min+1s past the dispatch.
    let clock = FixedClock(dispatched_at + DISPATCHED_TTL.as_millis() as i64 + 1_000);

    let outcome = sweep_stale_dispatched(store.pool(), &clock, &SweeperConfig::default())
        .await
        .expect("sweep ok");
    assert_eq!(outcome.failed, 1);
    assert_eq!(outcome.reclaimed, 0);

    let (status, reason, _, _) = read_task(store.pool(), "d1").await;
    assert_eq!(status, "failed");
    assert_eq!(reason.as_deref(), Some("timeout"));
}

/// **Key reference invariant** (`task.go:85`, `agent.sql.go:1979`
/// `ReclaimStaleDispatchedTaskForRuntime`): a dispatch is reclaimed once it is
/// *older* than the 90s recovery window — `dispatched_at < now - reclaim_window`
/// — because the daemon's `StartTask` should have confirmed within that window;
/// past it the claim response is presumed lost and the task is redelivered
/// (status back to `queued`, attempt unchanged). A 100s-old dispatch (inside the
/// 90s..5min reclaim band) is reclaimed. Mirrors the reference
/// `daemon_test.go:196 TestClaimTaskByRuntime_ReclaimsStaleDispatchedTask`
/// (120s-old dispatch → reclaimed).
#[tokio::test]
async fn dispatched_past_90s_window_reclaimed_not_failed() {
    let (_dir, store) = open_seeded().await;
    let dispatched_at = BASE_MS;
    seed_task(
        store.pool(),
        "d1",
        "dispatched",
        BASE_MS,
        Some(dispatched_at),
    )
    .await;
    let (_, _, attempt_before, _) = read_task(store.pool(), "d1").await;
    // 100s after dispatch — past the 90s window, well short of the 5min TTL.
    let clock = FixedClock(dispatched_at + 100_000);

    let reclaimed = reclaim_stale_dispatched(store.pool(), &clock, &SweeperConfig::default())
        .await
        .expect("reclaim ok");
    assert_eq!(reclaimed, 1, "one stale dispatched task reclaimed");

    let (status, reason, attempt_after, dispatched_after) = read_task(store.pool(), "d1").await;
    assert_eq!(status, "queued", "reclaimed back to queued, not failed");
    assert_eq!(reason, None, "reclaim records no failure");
    assert_eq!(
        attempt_after, attempt_before,
        "attempt unchanged by reclaim"
    );
    assert_eq!(
        dispatched_after, None,
        "dispatched_at cleared so a re-claim re-stamps it"
    );
}

/// A *fresh* dispatch (inside the 90s recovery window) is NOT reclaimed — the
/// daemon may still be mid-`StartTask`, and reclaiming it would double-dispatch
/// a healthy in-flight task (`task.go:82-85`: the recovery window must exceed
/// the daemon client timeout). Mirrors the reference
/// `daemon_test.go:223 DoesNotReclaimFreshDispatchedTask` (75s-old → untouched).
#[tokio::test]
async fn dispatched_within_90s_window_not_reclaimed() {
    let (_dir, store) = open_seeded().await;
    let dispatched_at = BASE_MS;
    seed_task(
        store.pool(),
        "d1",
        "dispatched",
        BASE_MS,
        Some(dispatched_at),
    )
    .await;
    // 75s after dispatch — still inside the 90s recovery window.
    let clock = FixedClock(dispatched_at + 75_000);

    let reclaimed = reclaim_stale_dispatched(store.pool(), &clock, &SweeperConfig::default())
        .await
        .expect("reclaim ok");
    assert_eq!(
        reclaimed, 0,
        "fresh dispatch inside the window is left alone"
    );

    let (status, _, _, dispatched_after) = read_task(store.pool(), "d1").await;
    assert_eq!(
        status, "dispatched",
        "stays dispatched inside the recovery window"
    );
    assert_eq!(
        dispatched_after,
        Some(dispatched_at),
        "dispatched_at untouched"
    );
}

/// Boundary: a dispatch exactly `RECLAIM_WINDOW + 1s` old is reclaimed (just
/// past the window) and exactly `RECLAIM_WINDOW - 1s` old is not. Pins the
/// `dispatched_at < now - reclaim_window` comparison direction.
#[tokio::test]
async fn reclaim_window_boundary_is_age_greater_than_window() {
    let (_dir, store) = open_seeded().await;
    let dispatched_at = BASE_MS;
    // Just inside the window: not reclaimed.
    seed_task(
        store.pool(),
        "fresh",
        "dispatched",
        BASE_MS,
        Some(dispatched_at),
    )
    .await;
    // Just past the window: reclaimed.
    seed_task(
        store.pool(),
        "stale",
        "dispatched",
        BASE_MS,
        Some(dispatched_at),
    )
    .await;

    let just_inside = FixedClock(dispatched_at + RECLAIM_WINDOW.as_millis() as i64 - 1_000);
    assert_eq!(
        reclaim_stale_dispatched(store.pool(), &just_inside, &SweeperConfig::default())
            .await
            .expect("reclaim ok"),
        0,
        "age just under the window is left dispatched"
    );

    let just_past = FixedClock(dispatched_at + RECLAIM_WINDOW.as_millis() as i64 + 1_000);
    assert_eq!(
        reclaim_stale_dispatched(store.pool(), &just_past, &SweeperConfig::default())
            .await
            .expect("reclaim ok"),
        2,
        "age just over the window is reclaimed"
    );
}

/// A stale dispatch that has already been *started* (`started_at` stamped) is
/// NOT reclaimed even past the window — it is racing a live run and re-queuing
/// it would double-dispatch. Mirrors the reference
/// `daemon_test.go:250 TestClaimTaskByRuntime_DoesNotReclaimAlreadyStartedTask`
/// and the `started_at IS NULL` guard in `agent.sql.go:1977`.
#[tokio::test]
async fn dispatched_with_started_at_not_reclaimed() {
    let (_dir, store) = open_seeded().await;
    let dispatched_at = BASE_MS;
    seed_task(
        store.pool(),
        "d1",
        "dispatched",
        BASE_MS,
        Some(dispatched_at),
    )
    .await;
    // Stamp started_at: the run is live even though status still reads dispatched.
    sqlx::query("UPDATE agent_task_queue SET started_at = ? WHERE id = ?")
        .bind(dispatched_at + 10_000)
        .bind("d1")
        .execute(store.pool())
        .await
        .expect("set started_at");
    // 120s after dispatch — well past the window, but started_at guards it.
    let clock = FixedClock(dispatched_at + 120_000);

    let reclaimed = reclaim_stale_dispatched(store.pool(), &clock, &SweeperConfig::default())
        .await
        .expect("reclaim ok");
    assert_eq!(
        reclaimed, 0,
        "an already-started dispatch is never reclaimed"
    );

    let (status, _, _, _) = read_task(store.pool(), "d1").await;
    assert_eq!(
        status, "dispatched",
        "started dispatch stays dispatched, not re-queued"
    );
}

// ---- running TTL (2.5h) ---------------------------------------------------

/// `runtime_sweeper.go:40` — a running task older than 2.5h is failed with
/// `failure_reason = timeout`.
#[tokio::test]
async fn running_ttl_2_5h_fails_task() {
    let (_dir, store) = open_seeded().await;
    // The running TTL keys off started_at; seed it directly.
    seed_task(store.pool(), "r1", "running", BASE_MS, None).await;
    sqlx::query("UPDATE agent_task_queue SET started_at = ? WHERE id = ?")
        .bind(BASE_MS)
        .bind("r1")
        .execute(store.pool())
        .await
        .expect("set started_at");
    let clock = FixedClock(BASE_MS + RUNNING_TTL.as_millis() as i64 + 1_000);

    let swept = sweep_stale_running(store.pool(), &clock, &SweeperConfig::default())
        .await
        .expect("sweep ok");
    assert_eq!(swept, 1);

    let (status, reason, _, _) = read_task(store.pool(), "r1").await;
    assert_eq!(status, "failed");
    assert_eq!(reason.as_deref(), Some("timeout"));
}

// ---- batch cap + idempotency ---------------------------------------------

/// The sweeper caps each pass at `batch_size` rows so a long backlog never holds
/// one giant write transaction. 600 expired queued → 500 this pass, 100 next.
#[tokio::test]
async fn sweeper_batch_caps_at_500() {
    let (_dir, store) = open_seeded().await;
    for i in 0..600 {
        seed_task(store.pool(), &format!("q{i:04}"), "queued", BASE_MS, None).await;
    }
    let clock = FixedClock(BASE_MS + QUEUED_TTL.as_millis() as i64 + 1_000);
    let cfg = SweeperConfig::default();
    assert_eq!(
        cfg.batch_size, 500,
        "default batch size matches the reference"
    );

    let first = sweep_expired_queued(store.pool(), &clock, &cfg).await.expect("first pass");
    assert_eq!(first, 500, "first pass caps at the batch size");

    let second = sweep_expired_queued(store.pool(), &clock, &cfg).await.expect("second pass");
    assert_eq!(second, 100, "second pass takes the remainder");
}

/// A sweeper never flips a terminal row back: an already-`done` task is left
/// alone even when its timestamps are ancient.
#[tokio::test]
async fn sweeper_idempotent_on_already_terminal_rows() {
    let (_dir, store) = open_seeded().await;
    seed_task(store.pool(), "t1", "done", BASE_MS, None).await;
    let clock = FixedClock(BASE_MS + QUEUED_TTL.as_millis() as i64 + 1_000_000);

    let swept = sweep_expired_queued(store.pool(), &clock, &SweeperConfig::default())
        .await
        .expect("sweep ok");
    assert_eq!(swept, 0, "terminal rows are not swept");

    let (status, _, _, _) = read_task(store.pool(), "t1").await;
    assert_eq!(status, "done", "terminal state preserved");
}

// ---- startup crash-recovery reclaim (e38.25) ------------------------------

/// Seed a second runtime + agent (`rt-2` / `agent-2`) so the startup reclaim's
/// per-runtime scoping can be asserted against a sibling runtime's rows.
async fn seed_second_runtime(pool: &SqlitePool) {
    sqlx::query(
        "INSERT INTO agent_runtime (id, workspace_id, daemon_id, provider, runtime_mode) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind("rt-2")
    .bind("ws-1")
    .bind("daemon-rt-2")
    .bind("claude")
    .bind("local")
    .execute(pool)
    .await
    .expect("insert runtime rt-2");
    sqlx::query(
        "INSERT INTO agent \
         (id, workspace_id, name, runtime_id, visibility, owner_id, max_concurrent_tasks) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind("agent-2")
    .bind("ws-1")
    .bind("Agent2")
    .bind("rt-2")
    .bind("workspace")
    .bind("user-1")
    .bind(5_i64)
    .execute(pool)
    .await
    .expect("insert agent-2");
}

/// Force a task onto `rt-2` / `agent-2` in `status`, stamping `started_at` for a
/// `running` row so the reclaim's timestamp-clear can be asserted.
async fn seed_task_rt2(pool: &SqlitePool, id: &str, status: &str) {
    sqlx::query(
        "INSERT INTO agent_task_queue \
         (id, workspace_id, runtime_id, agent_id, status, created_at, started_at) \
         VALUES (?, 'ws-1', 'rt-2', 'agent-2', ?, ?, ?)",
    )
    .bind(id)
    .bind(status)
    .bind(BASE_MS)
    .bind((status == "running").then_some(BASE_MS))
    .execute(pool)
    .await
    .expect("insert rt-2 task");
}

/// A daemon restarting reclaims its own orphaned `running` row back to `queued`,
/// clearing `started_at` and leaving `attempt` unchanged — no TTL wait needed.
#[tokio::test]
async fn startup_reclaims_own_running_orphan_to_queued() {
    let (_dir, store) = open_seeded().await;
    // A `running` orphan with `started_at` stamped (the post-crash state).
    seed_task(store.pool(), "r1", "queued", BASE_MS, None).await;
    sqlx::query("UPDATE agent_task_queue SET status='running', started_at=? WHERE id='r1'")
        .bind(BASE_MS)
        .execute(store.pool())
        .await
        .expect("force running");

    let reclaimed = reclaim_orphaned_on_startup(store.pool(), "rt-1").await.expect("reclaim ok");
    assert_eq!(reclaimed, 1, "the running orphan is reclaimed");

    let (status, reason, attempt, _) = read_task(store.pool(), "r1").await;
    assert_eq!(status, "queued", "running orphan returned to queued");
    assert_eq!(reason, None, "a crash redelivery is not a failure");
    assert_eq!(attempt, 1, "attempt unchanged (redelivery, not retry)");
    let started_at: Option<i64> =
        sqlx::query("SELECT started_at FROM agent_task_queue WHERE id='r1'")
            .fetch_one(store.pool())
            .await
            .expect("read started_at")
            .try_get("started_at")
            .unwrap();
    assert_eq!(
        started_at, None,
        "started_at cleared so a fresh claim re-runs it"
    );
}

/// The startup reclaim also redelivers an orphaned `dispatched` row regardless
/// of age (no 90s window wait — the owning process is gone).
#[tokio::test]
async fn startup_reclaims_own_dispatched_orphan_to_queued() {
    let (_dir, store) = open_seeded().await;
    seed_task(store.pool(), "d1", "dispatched", BASE_MS, Some(BASE_MS)).await;

    let reclaimed = reclaim_orphaned_on_startup(store.pool(), "rt-1").await.expect("reclaim ok");
    assert_eq!(reclaimed, 1, "the dispatched orphan is reclaimed");

    let (status, _, _, dispatched_at) = read_task(store.pool(), "d1").await;
    assert_eq!(status, "queued", "dispatched orphan returned to queued");
    assert_eq!(
        dispatched_at, None,
        "dispatched_at cleared so a fresh claim re-stamps it"
    );
}

/// The startup reclaim is scoped to its own runtime: a sibling runtime's live
/// `running`/`dispatched` rows are NEVER touched, and terminal rows are immune.
#[tokio::test]
async fn startup_reclaim_is_runtime_scoped_and_skips_terminal() {
    let (_dir, store) = open_seeded().await;
    seed_second_runtime(store.pool()).await;

    // rt-1: one running orphan (will be reclaimed) + one already-`done`.
    seed_task(store.pool(), "own-run", "queued", BASE_MS, None).await;
    sqlx::query("UPDATE agent_task_queue SET status='running', started_at=? WHERE id='own-run'")
        .bind(BASE_MS)
        .execute(store.pool())
        .await
        .expect("force own running");
    seed_task(store.pool(), "own-done", "done", BASE_MS, None).await;

    // rt-2 (sibling): a live running + a live dispatched — must be untouched.
    seed_task_rt2(store.pool(), "sib-run", "running").await;
    seed_task_rt2(store.pool(), "sib-disp", "dispatched").await;

    let reclaimed = reclaim_orphaned_on_startup(store.pool(), "rt-1").await.expect("reclaim ok");
    assert_eq!(
        reclaimed, 1,
        "only this runtime's single in-flight orphan is reclaimed"
    );

    let (own_run, ..) = read_task(store.pool(), "own-run").await;
    assert_eq!(own_run, "queued", "own running orphan reclaimed");
    let (own_done, ..) = read_task(store.pool(), "own-done").await;
    assert_eq!(own_done, "done", "own terminal row untouched");
    let (sib_run, ..) = read_task(store.pool(), "sib-run").await;
    assert_eq!(
        sib_run, "running",
        "sibling runtime's live running row untouched"
    );
    let (sib_disp, ..) = read_task(store.pool(), "sib-disp").await;
    assert_eq!(
        sib_disp, "dispatched",
        "sibling runtime's live dispatched row untouched"
    );
}
