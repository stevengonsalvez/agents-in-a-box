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
//! # Orphan reclaim (not time-based)
//!
//! Separately from the TTLs, a runtime's whole in-flight set is reclaimed the
//! moment its owning PROCESS is known to be gone — no TTL wait, because the
//! evidence is ownership, not age. [`reclaim_orphaned_for_runtime`] does the
//! work; two thin wrappers name the two pieces of evidence:
//! [`reclaim_orphans_on_restart`] (a different instance registered the runtime)
//! and [`reclaim_orphans_for_offline_runtimes`] (the presence sweep aged it out).
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
use ainb_hangar_proto::events::PresenceState;
use ainb_hangar_store::bootstrap::RuntimeArrival;
use ainb_hangar_store::repo::agent_runtime::{AgentRuntimeRepo, PresenceSweep, SweptRuntime};
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

/// Default interval between runtime-presence passes (heartbeat + availability
/// decay). Reference: `runtime_sweeper.go:21` (`sweepInterval` = 30s).
/// Overridable via `HANGAR_PRESENCE_SWEEP_MS` so a tripwire can land several
/// ticks inside its budget.
pub const DEFAULT_PRESENCE_INTERVAL: Duration = Duration::from_secs(30);

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
    /// Interval between runtime-presence passes (heartbeat + availability decay).
    pub presence_interval: Duration,
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
            presence_interval: DEFAULT_PRESENCE_INTERVAL,
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

/// One runtime-presence pass: beat for our own runtime, then age-sweep the rest.
///
/// Unlike the task sweepers this one has a WRITE side of its own: the daemon
/// stamps `own_runtime_id`'s heartbeat FIRST, so its own row can never be
/// decayed by its own sweep, then walks every other runtime through
/// `online → unstable → offline` by heartbeat age. The thresholds come from
/// [`PresenceState`], the single definition the read-side fold also uses.
///
/// `own_runtime_id` is `None` for a daemon that advertises no runtime of its own
/// (the claim-disabled "sweepers only" mode), in which case the pass is purely
/// the age sweep.
///
/// A runtime that lands in the `offline` band also has its orphaned in-flight
/// tasks reclaimed to `queued` — see [`reclaim_orphans_for_offline_runtimes`],
/// which owns the trigger's reasoning and swallows its own faults.
///
/// Returns the rows that moved so the caller can fan `AgentPresence` events out
/// (the reference publishes `EventDaemonRegister{stale_sweep}` for the same
/// reason: clients must re-read the runtime list).
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if the heartbeat or either sweep statement fails.
/// A failed *reclaim* is NOT an error here: it is logged and skipped so the
/// presence transitions are still reported and still announced.
pub async fn sweep_runtime_presence(
    pool: &SqlitePool,
    clock: &dyn HangarClock,
    own_runtime_id: Option<&str>,
) -> Result<PresenceSweep, sqlx::Error> {
    let now = clock.now_ms();
    if let Some(id) = own_runtime_id {
        AgentRuntimeRepo::heartbeat(pool, id, now).await?;
    }
    let sweep = AgentRuntimeRepo::sweep_presence_by_age(
        pool,
        now,
        PresenceState::UNSTABLE_AFTER_MS,
        PresenceState::OFFLINE_AFTER_MS,
    )
    .await?;
    if !sweep.is_empty() {
        tracing::info!(
            to_unstable = sweep.to_unstable.len(),
            to_offline = sweep.to_offline.len(),
            "runtime_presence_swept",
        );
    }
    // Going offline is itself the verdict that a runtime's in-flight work is
    // orphaned, so recover it here rather than stranding it until the multi-hour
    // running TTL. Faults are absorbed per runtime so the caller still gets the
    // full sweep to fan presence events from.
    reclaim_orphans_for_offline_runtimes(pool, &sweep.to_offline, own_runtime_id).await;
    Ok(sweep)
}

/// Reclaim one runtime's orphaned in-flight tasks back to `queued` (e38.25 crash
/// recovery).
///
/// When a daemon dies uncleanly (`kill -9`, OOM, panic) mid-task its claimed
/// rows are left frozen in `dispatched` or `running` — the FSM step that would
/// have finalised them never ran. The time-based sweepers eventually move them
/// (a `running` row only past the 2.5h running TTL; a `dispatched` row past the
/// 90s reclaim window), but once the owning process is known to be gone there is
/// no need to wait. So the caller reclaims every non-terminal in-flight row that
/// runtime owns — scoped to the one `runtime_id` so a sibling daemon's live runs
/// in a multi-runtime deployment are never disturbed — back to `queued`, clearing
/// `started_at` / `dispatched_at` so a fresh claim re-dispatches the work. The
/// `attempt` counter is left **unchanged** (a crash redelivery is not a retry,
/// mirroring [`reclaim_stale_dispatched`]).
///
/// This function is deliberately TRIGGER-NEUTRAL: deciding that the owning
/// process is gone is the caller's job, and there are two such callers, each a
/// thin wrapper naming its own evidence — [`reclaim_orphans_on_restart`] (a
/// different process instance registered this runtime) and
/// [`reclaim_orphans_for_offline_runtimes`] (the presence sweep aged the runtime
/// out). Never call it unconditionally: requeuing rows a LIVE process is still
/// executing double-dispatches that work.
///
/// Returns the number of rows reclaimed.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if the statement fails. A requeue can legitimately
/// fail: `queued` is a *pending* status, so returning a `running` row to it can
/// collide with the one-pending-task-per-(issue, agent) unique index when a
/// newer task for the same pair is already waiting. Both callers treat that as
/// non-fatal — the time-based sweepers still backstop the row.
pub async fn reclaim_orphaned_for_runtime(
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
            outcome = "reclaimed",
            count = reclaimed,
            runtime_id,
            "sweeper_orphan_reclaim"
        );
    }
    Ok(reclaimed)
}

/// [`reclaim_orphaned_for_runtime`], but only when the boot registration says a
/// DIFFERENT process instance owned this runtime (migration 0092).
///
/// "This daemon just booted" is a weaker signal than it looks: the same process
/// can register a runtime more than once, and a reclaim on a
/// [`RuntimeArrival::Reconnect`] would requeue tasks that are still executing
/// under a live process — the exact corruption the unconditional reclaim risks
/// once anything re-registers. Keying off the arrival makes the reconcile follow
/// the ownership change that actually orphans work, rather than the boot that
/// usually accompanies it.
///
/// Returns the number of rows reclaimed (`0` on a reconnect, without touching
/// the database).
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if the reclaim statement fails.
pub async fn reclaim_orphans_on_restart(
    pool: &SqlitePool,
    runtime_id: &str,
    arrival: &RuntimeArrival,
) -> Result<u64, sqlx::Error> {
    if !arrival.is_restart() {
        tracing::info!(
            kind = "startup",
            outcome = "skipped",
            runtime_id,
            "same instance reconnected; its in-flight tasks are live, not orphans"
        );
        return Ok(0);
    }
    reclaim_orphaned_for_runtime(pool, runtime_id).await
}

/// [`reclaim_orphaned_for_runtime`] for every runtime the presence sweep just
/// aged out to `offline`.
///
/// The restart wrapper above only recovers a dead daemon's work when that daemon
/// COMES BACK. Nothing brings back a machine that was shut down, so without this
/// its in-flight rows sit `dispatched`/`running` until the multi-hour
/// [`RUNNING_TTL`] — hours in which the tasks look alive, block their issue, and
/// are claimable by nobody. Crossing [`PresenceState::OFFLINE_AFTER_MS`] with no
/// heartbeat is the second piece of evidence that the owning process is gone, so
/// the same runtime-scoped reclaim runs on that transition.
///
/// Only `to_offline` — never `to_unstable`. Unstable is the GRACE window whose
/// whole purpose is to let a briefly-stalled daemon (a GC pause, a slow disk, a
/// laptop lid) keep the run it is still executing; reclaiming there would rob it.
///
/// Faults are swallowed per runtime, deliberately: presence is observability, and
/// one runtime's failed requeue (most plausibly the pending-task unique-index
/// collision described on [`reclaim_orphaned_for_runtime`]) must not stop the
/// next runtime's reclaim, nor the caller's `AgentPresence` fan-out. The
/// time-based sweepers remain the backstop for whatever is skipped. Returns the
/// total rows reclaimed across all runtimes.
///
/// # Residual hazard (read before changing this)
///
/// A daemon that is ALIVE but wedged for >10min gets its rows requeued while it
/// may still be executing them; if it later un-wedges, that work can run twice.
/// The 10-minute threshold IS the mitigation — it is 20 missed 30s beats.
///
/// `agent_runtime.instance_id` (migration 0092) does NOT help here, despite
/// being the right tool for the restart wrapper: the offline row's `instance_id`
/// is still the WEDGED process's own id, because nothing else has registered.
/// A dead daemon and a wedged one are byte-identical in the schema — same
/// instance, stale `last_seen_at` — so gating on the instance would reject both
/// or accept both. Closing this properly needs a fencing token (stamp the owning
/// instance on the task row at dispatch and make the terminal write conditional
/// on it), which is a schema change and out of scope here.
pub async fn reclaim_orphans_for_offline_runtimes(
    pool: &SqlitePool,
    to_offline: &[SweptRuntime],
    own_runtime_id: Option<&str>,
) -> u64 {
    let mut total = 0;
    for rt in to_offline {
        // Defensive: `sweep_runtime_presence` beats for our own runtime before it
        // sweeps, so our id cannot legitimately appear here. A backwards clock
        // jump or a process paused past the grace window could in principle
        // produce it, and self-reclaiming would requeue OUR OWN live runs — the
        // exact double-dispatch the restart wrapper's reconnect check exists to
        // prevent. Cheaper to guard than to reason about.
        if own_runtime_id == Some(rt.id.as_str()) {
            tracing::warn!(
                runtime_id = %rt.id,
                "presence sweep aged out our own runtime; skipping self-reclaim"
            );
            continue;
        }
        match reclaim_orphaned_for_runtime(pool, &rt.id).await {
            Ok(n) => total += n,
            Err(e) => tracing::error!(
                error = %e,
                runtime_id = %rt.id,
                "offline orphan reclaim failed"
            ),
        }
    }
    total
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
