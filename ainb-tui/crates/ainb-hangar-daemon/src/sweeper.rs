//! TTL sweepers + stale-dispatch reclaim for the task FSM (P1.4).
//!
//! Three lifecycle states can get *stuck*: a `queued` task no runtime ever
//! claims, a `dispatched` task whose runtime crashed before it confirmed start,
//! and a `running` task whose agent hung. Each is bounded by a time-to-live;
//! when the TTL elapses the sweeper fails the row with
//! [`FailureReason::Timeout`](ainb_hangar_store::service::fail::FailureReason::Timeout).
//!
//! The dispatched state additionally has a **recovery window** (`task.go:82-85`):
//! the window must exceed the daemon's `StartTask` client timeout so an in-flight
//! claim is never reclaimed and double-dispatched. For the first 90s after
//! dispatch the task is therefore left **untouched** (the runtime may still be
//! confirming start). Once the dispatch outlives the window — `age >
//! reclaim_window` — the claim response is presumed lost and the task is
//! *reclaimed* (status back to `queued`, attempt unchanged) so a fresh runtime
//! can pick it up again. Only past the 5min dispatch TTL does a stuck dispatch
//! become a hard failure. A task that has already started (`started_at` stamped)
//! is never reclaimed — it is racing a live run.
//!
//! ```text
//!   dispatched_at        +90s            +5min
//!        │                │                │
//!   ─────┼────────────────┼────────────────┼──────────▶ age
//!        │ recovery window │ reclaim → queued │ fail → timeout
//!        │ (skip: in-flight) │ (lost response) │ (runtime crashed)
//! ```
//!
//! All thresholds key off an injected [`HangarClock`] and a [`SweeperConfig`]:
//! production uses the reference defaults, tests inject a frozen clock and tight
//! config so the suite is deterministic (no `tokio::time::sleep`).
//!
//! # Idempotency
//!
//! Every statement constrains the source `status` in its `WHERE` clause, so a
//! terminal (`done` / `failed` / `cancelled`) row is never touched and a second
//! pass over the same backlog is a no-op once the rows have moved.
//!
//! Reference control-plane source-line references:
//! - queued TTL = 2h    (`runtime_sweeper.go:52`)
//! - running TTL = 2.5h  (`runtime_sweeper.go:40`)
//! - dispatched reclaim window = 90s (`task.go:85`)

// The TTL constants below are intentionally expressed in seconds: every
// threshold is a "minutes / hours" quantity but `Duration::from_mins` /
// `from_hours` are still unstable, and a raw second count is the clearest stable
// spelling for a timeout (and matches the reference source values cited beside
// each one).
#![allow(clippy::duration_suboptimal_units)]

use std::time::Duration;

use ainb_hangar_core::clock::HangarClock;
use sqlx::SqlitePool;

/// How long a `queued` task may wait before it is failed. Reference:
/// `runtime_sweeper.go:52` (2 hours).
pub const QUEUED_TTL: Duration = Duration::from_secs(7_200);

/// How long a `dispatched` task may sit unconfirmed before it is failed.
/// 5 minutes — past this the runtime is presumed crashed.
pub const DISPATCHED_TTL: Duration = Duration::from_secs(300);

/// The window after dispatch in which a stale dispatch is *reclaimed* (returned
/// to `queued`) rather than failed, to tolerate a lost claim response. Reference:
/// `task.go:85` (90 seconds).
pub const RECLAIM_WINDOW: Duration = Duration::from_secs(90);

/// How long a `running` task may execute before it is failed. Reference:
/// `runtime_sweeper.go:40` (2.5 hours).
pub const RUNNING_TTL: Duration = Duration::from_secs(9_000);

/// Default per-pass row cap.
///
/// Bounds the size of each sweep's write transaction so a large backlog does not
/// hold one long-running `UPDATE` (matches the reference batch behaviour; for
/// `SQLite` WAL it keeps writer latency low).
pub const DEFAULT_BATCH_SIZE: i64 = 500;

/// Default interval between sweep passes (used by the daemon scheduler, not the
/// individual sweep functions).
pub const DEFAULT_SWEEP_INTERVAL: Duration = Duration::from_secs(60);

/// Default interval between workspace-GC passes (the on-disk orphan/Full reclaim
/// driven by [`crate::execenv::sweep_workspaces_gc`]).
///
/// Far longer than the row-sweep cadence: the 72h grace window means a leaked
/// dir is never near reclaim before hours have passed, so scanning the disk
/// hourly is ample (and keeps the IO cost negligible). Overridable via
/// `HANGAR_GC_INTERVAL_MS` for deterministic / tight-budget tests.
pub const DEFAULT_GC_INTERVAL: Duration = Duration::from_secs(3_600);

/// Tunable thresholds for the sweepers.
///
/// Production constructs [`SweeperConfig::default`] (the reference defaults);
/// tests override individual fields to drive deterministic time-based cases and
/// to exercise the batch cap with a small backlog.
#[derive(Debug, Clone, Copy)]
pub struct SweeperConfig {
    /// How long a `queued` task may wait before it is failed.
    pub queued_ttl: Duration,
    /// How long a `dispatched` task may sit unconfirmed before it is failed.
    pub dispatched_ttl: Duration,
    /// The post-dispatch reclaim window (stale dispatch → `queued`).
    pub reclaim_window: Duration,
    /// How long a `running` task may execute before it is failed.
    pub running_ttl: Duration,
    /// Interval between sweep passes (consumed by the daemon scheduler).
    pub sweep_interval: Duration,
    /// Interval between workspace-GC passes (on-disk orphan/Full reclaim).
    pub gc_interval: Duration,
    /// Maximum rows mutated per pass.
    pub batch_size: i64,
}

impl Default for SweeperConfig {
    fn default() -> Self {
        Self {
            queued_ttl: QUEUED_TTL,
            dispatched_ttl: DISPATCHED_TTL,
            reclaim_window: RECLAIM_WINDOW,
            running_ttl: RUNNING_TTL,
            sweep_interval: DEFAULT_SWEEP_INTERVAL,
            gc_interval: DEFAULT_GC_INTERVAL,
            batch_size: DEFAULT_BATCH_SIZE,
        }
    }
}

/// The outcome of one [`sweep_stale_dispatched`] pass.
///
/// A single dispatched-sweep does both jobs — reclaiming dispatches that
/// outlived the 90s recovery window and failing dispatches past the 5min TTL —
/// so it reports both counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DispatchSweepOutcome {
    /// Tasks returned to `queued` (past the recovery window, un-started).
    pub reclaimed: u64,
    /// Tasks failed with `timeout` (past the dispatch TTL).
    pub failed: u64,
}

/// Fail every `queued` task older than the queued TTL, up to the batch cap.
///
/// A task is expired when `created_at < clock.now_ms() - queued_ttl`. Expired
/// rows transition `queued -> failed` with `failure_reason = 'timeout'` and
/// `finished_at = clock.now_ms()`. Returns the number of rows failed in this
/// pass; a backlog larger than `batch_size` is drained over successive passes.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if the statement fails.
pub async fn sweep_expired_queued(
    pool: &SqlitePool,
    clock: &dyn HangarClock,
    cfg: &SweeperConfig,
) -> Result<u64, sqlx::Error> {
    let now = clock.now_ms();
    let cutoff = now - ms(cfg.queued_ttl);
    let failed = fail_batch(pool, "queued", "created_at", cutoff, now, cfg.batch_size).await?;
    if failed > 0 {
        tracing::info!(
            kind = "queued",
            outcome = "failed",
            count = failed,
            "sweeper_swept"
        );
    }
    Ok(failed)
}

/// Fail every `running` task older than the running TTL, up to the batch cap.
///
/// A task is expired when `started_at < clock.now_ms() - running_ttl`. Expired
/// rows transition `running -> failed` with `failure_reason = 'timeout'`.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if the statement fails.
pub async fn sweep_stale_running(
    pool: &SqlitePool,
    clock: &dyn HangarClock,
    cfg: &SweeperConfig,
) -> Result<u64, sqlx::Error> {
    let now = clock.now_ms();
    let cutoff = now - ms(cfg.running_ttl);
    let failed = fail_batch(pool, "running", "started_at", cutoff, now, cfg.batch_size).await?;
    if failed > 0 {
        tracing::info!(
            kind = "running",
            outcome = "failed",
            count = failed,
            "sweeper_swept"
        );
    }
    Ok(failed)
}

/// Reclaim dispatches that outlived the 90s recovery window: stale dispatch ->
/// `queued`.
///
/// A dispatch is reclaimable when it is `dispatched`, has not yet started
/// (`started_at IS NULL`), and `age > reclaim_window`, where
/// `age = now - dispatched_at`. The 90s window is the grace period in which a
/// freshly-claimed task is left alone because the runtime may still be confirming
/// start (the window must exceed the daemon's `StartTask` client timeout, else a
/// healthy in-flight task is reclaimed and double-dispatched — `task.go:82-85`).
/// Once a dispatch outlives the window its claim *response* is presumed lost, so
/// the task is redelivered: reclaimed rows go back to `queued` with
/// `dispatched_at` cleared (a re-claim re-stamps it) and the `attempt` counter
/// **unchanged** (a reclaim is a redelivery, not a retry). A task that has
/// already started is skipped — it is racing a live run. Past the dispatch TTL a
/// stuck dispatch is failed by [`sweep_stale_dispatched`]. Mirrors the reference
/// control plane `agent.sql.go:1979 ReclaimStaleDispatchedTaskForRuntime`.
///
/// A `dispatched` row with a NULL `dispatched_at` (a claim whose timestamp was
/// never stamped) is also reclaimed here: with no timestamp it cannot be inside
/// the protection window, and leaving it would make it immortal — invisible to
/// every time-based pass and thus a permanent block on its issue's delete.
///
/// Returns the number of rows reclaimed in this pass.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if the statement fails.
pub async fn reclaim_stale_dispatched(
    pool: &SqlitePool,
    clock: &dyn HangarClock,
    cfg: &SweeperConfig,
) -> Result<u64, sqlx::Error> {
    let now = clock.now_ms();
    // A dispatch is *reclaimable* once it has outlived the recovery window but
    // not yet the dispatch TTL — the reclaim band is `reclaim_window < age <=
    // dispatched_ttl`, i.e. `now - dispatched_ttl <= dispatched_at < now -
    // reclaim_window`. Past the TTL the dispatch is a hard failure, not a
    // redelivery, so the fail step ([`sweep_stale_dispatched`]) owns it; the
    // upper bound here keeps the two steps disjoint regardless of order.
    // (Mirrors the reference `agent.sql.go:1979`
    // `ReclaimStaleDispatchedTaskForRuntime`.)
    let window_cutoff = now - ms(cfg.reclaim_window);
    let ttl_cutoff = now - ms(cfg.dispatched_ttl);
    // A NULL `dispatched_at` is a *stranded* claim: the row is `dispatched` but its
    // dispatch timestamp was never stamped (a lost/never-recorded claim response),
    // so the time-based band can never see it and it is immortal. Treat it as
    // reclaimable — it is by definition outside the 90s protection window (there is
    // no fresh in-flight timestamp to protect) — so it is redelivered to `queued`
    // like any dispatch past the window. Stamped rows keep the exact band
    // (`reclaim_window < age <= dispatched_ttl`): a fresh (<90s) dispatch stays
    // protected and a past-TTL dispatch is still left to the fail step.
    let reclaimed = sqlx::query(
        "UPDATE agent_task_queue \
         SET status = 'queued', dispatched_at = NULL \
         WHERE id IN ( \
             SELECT id FROM agent_task_queue \
             WHERE status = 'dispatched' \
               AND started_at IS NULL \
               AND ( dispatched_at IS NULL \
                     OR (dispatched_at < ?1 AND dispatched_at >= ?2) ) \
             ORDER BY dispatched_at \
             LIMIT ?3 \
         )",
    )
    .bind(window_cutoff)
    .bind(ttl_cutoff)
    .bind(cfg.batch_size)
    .execute(pool)
    .await?
    .rows_affected();
    if reclaimed > 0 {
        tracing::info!(
            kind = "dispatched",
            outcome = "reclaimed",
            count = reclaimed,
            "sweeper_swept",
        );
    }
    Ok(reclaimed)
}

/// One dispatched-sweep pass: reclaim past the 90s window, fail past the TTL.
///
/// Runs [`reclaim_stale_dispatched`] (which redelivers every un-started dispatch
/// older than the 90s recovery window) then fails every dispatch older than the
/// dispatch TTL (`dispatched_at < now - dispatched_ttl`), returning both counts.
/// A dispatch still inside the recovery window (`age <= reclaim_window`) is
/// touched by neither and stays `dispatched`; one past the window is reclaimed,
/// and the reclaim clears `dispatched_at` so the same pass's fail step never
/// sees it.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if either statement fails.
pub async fn sweep_stale_dispatched(
    pool: &SqlitePool,
    clock: &dyn HangarClock,
    cfg: &SweeperConfig,
) -> Result<DispatchSweepOutcome, sqlx::Error> {
    let reclaimed = reclaim_stale_dispatched(pool, clock, cfg).await?;

    let now = clock.now_ms();
    let cutoff = now - ms(cfg.dispatched_ttl);
    let failed = fail_batch(
        pool,
        "dispatched",
        "dispatched_at",
        cutoff,
        now,
        cfg.batch_size,
    )
    .await?;
    if failed > 0 {
        tracing::info!(
            kind = "dispatched",
            outcome = "failed",
            count = failed,
            "sweeper_swept",
        );
    }
    Ok(DispatchSweepOutcome { reclaimed, failed })
}

/// Reclaim a crashed daemon's orphaned in-flight tasks back to `queued` at
/// startup (e38.25 crash recovery).
///
/// When a daemon dies uncleanly (`kill -9`, OOM, panic) mid-task its claimed
/// rows are left frozen in `dispatched` or `running` — the FSM step that would
/// have finalised them never ran. The time-based sweepers eventually move them
/// (a `running` row only past the 2.5h running TTL; a `dispatched` row past the
/// 90s reclaim window), but on a fresh boot there is no need to wait: the
/// process that *was* executing those tasks is, by definition, gone. So at
/// startup the newly-booted daemon reclaims every non-terminal in-flight row it
/// owns — scoped to **its own** `runtime_id` so a sibling daemon's live runs in
/// a multi-runtime deployment are never disturbed — back to `queued`, clearing
/// `started_at` / `dispatched_at` so a fresh claim re-dispatches the work. The
/// `attempt` counter is left **unchanged** (a crash redelivery is not a retry,
/// mirroring [`reclaim_stale_dispatched`]).
///
/// This is the redelivery the daemon-restart path needs: the orphan leaves its
/// `running`/`dispatched` limbo immediately on restart rather than stranding the
/// work for hours. Mirrors the reference's runtime-scoped reclaim-on-boot.
///
/// Returns the number of rows reclaimed.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if the statement fails.
pub async fn reclaim_orphaned_on_startup(
    pool: &SqlitePool,
    runtime_id: &str,
) -> Result<u64, sqlx::Error> {
    let reclaimed = sqlx::query(
        "UPDATE agent_task_queue \
         SET status = 'queued', started_at = NULL, dispatched_at = NULL \
         WHERE runtime_id = ?1 \
           AND status IN ('dispatched', 'running')",
    )
    .bind(runtime_id)
    .execute(pool)
    .await?
    .rows_affected();
    if reclaimed > 0 {
        tracing::info!(
            kind = "startup",
            outcome = "reclaimed",
            count = reclaimed,
            runtime_id,
            "sweeper_startup_reclaim"
        );
    }
    Ok(reclaimed)
}

/// Fail up to `batch_size` rows in `from_status` whose `age_column` is older
/// than `cutoff`, setting `failure_reason = 'timeout'` and `finished_at = now`.
///
/// The `from_status` and `age_column` arguments are fixed string literals chosen
/// by the three callers (never user input), so interpolating them into the SQL
/// is injection-safe; the time bounds and batch cap are parameter-bound.
///
/// A NULL `age_column` is treated as *infinitely old* (`age_column IS NULL` is
/// matched explicitly, keeping the predicate index-friendly), so a `running` row
/// whose `started_at` was never stamped — or a `dispatched`
/// row with a NULL `dispatched_at` the reclaim step did not already redeliver — is
/// failed rather than skipped forever. Without this a NULL-timestamp row is
/// immortal (the old `IS NOT NULL` guard excluded it from every pass), which is
/// exactly what strands a task and blocks its issue's delete.
async fn fail_batch(
    pool: &SqlitePool,
    from_status: &str,
    age_column: &str,
    cutoff: i64,
    now: i64,
    batch_size: i64,
) -> Result<u64, sqlx::Error> {
    let sql = format!(
        "UPDATE agent_task_queue \
         SET status = 'failed', failure_reason = 'timeout', finished_at = ?1 \
         WHERE id IN ( \
             SELECT id FROM agent_task_queue \
             WHERE status = '{from_status}' \
               AND ( {age_column} IS NULL OR {age_column} < ?2 ) \
             ORDER BY {age_column} \
             LIMIT ?3 \
         )"
    );
    Ok(sqlx::query(&sql)
        .bind(now)
        .bind(cutoff)
        .bind(batch_size)
        .execute(pool)
        .await?
        .rows_affected())
}

/// Convert a [`Duration`] to whole milliseconds as `i64` (the column unit).
///
/// All Hangar TTLs are seconds/minutes/hours, so the millisecond count fits
/// `i64` with vast headroom; a saturating cast is therefore exact in practice
/// and merely defensive against an absurd config value.
#[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
const fn ms(d: Duration) -> i64 {
    d.as_millis() as i64
}
