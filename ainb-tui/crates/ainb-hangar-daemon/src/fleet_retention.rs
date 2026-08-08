//! Hourly retention sweep over the `fleet_event` ledger.
//!
//! Three stages, run in order, each batched:
//!
//! * **A — payload eviction at 48h.** Blanks aged hook bodies, keeps the rows.
//! * **B — row delete at 30 days.** Removes whole rows, guarded.
//! * **C — byte ceiling.** Re-runs A on progressively tighter cutoffs while the
//!   corpus is over budget.
//!
//! C is a CO-EQUAL control, not a backstop. The measured corpus is rate-driven:
//! 713 MB of 714 MB was under seven days old, at roughly 102 MB/day. An age
//! cutoff alone therefore bounds nothing — it fixes the WINDOW and lets the
//! rate pick the size. A seven-day cutoff would have held ~713 MB steady, which
//! is the problem; 48h holds ~204 MB; and if the rate doubles, only C reacts.
//!
//! Scheduling notes are load-bearing. `SQLite` has ONE writer, shared with the
//! Fleet reconciler and the ACP transcript writer (see
//! `ainb_hangar_store::repo::fleet_provider_event`'s own warning about that
//! contention). So this runs on its own task, never the 3s tick, with bounded
//! batches, a yield and a WAL checkpoint between them, and its sleep taken
//! AFTER the work rather than on a ticker.
//!
//! ## Backlog catch-up vs steady state
//!
//! These are the same code on two different clocks, because the only thing that
//! differs is how much is left to do. A pass evicts at most
//! [`MAX_ROWS_PER_PASS`] rows; if it hits that cap there is more backlog, and
//! the next pass follows in [`CATCHUP_PERIOD`] instead of [`SWEEP_PERIOD`].
//!
//! Measured reason the cap exists: the first production pass against the
//! un-swept 1.4 GB database peaked the daemon at **2,599 MB RSS** (204 MB
//! steady state) with CPU at 106%. Nothing was leaking — it drained back down —
//! but an unbounded first pass rewrites ~700 MB of payload in one continuous
//! burst, and a WAL only shrinks at a checkpoint the automatic checkpointer
//! never gets to run. The cap plus [`FleetRetentionRepo::checkpoint_wal`]
//! bounds bytes-in-flight per pass instead of per backlog.
//!
//! ## Resumability
//!
//! There is NO watermark to persist, because the tombstone already is one, and
//! a better one. `payload_evicted_at` is a per-row durable checkpoint written
//! in the same transaction as the eviction it records, and the sweep selects
//! through the `payload_evicted_at IS NULL` partial index — so a daemon killed
//! mid-backlog resumes exactly where it stopped, with no in-memory state, no
//! scalar cursor that can disagree with the rows, and no extra migration.
//! `evicted_backlog_resumes_from_the_tombstone_after_a_restart` proves that
//! across a real pool close and reopen.

use std::time::Duration;

use ainb_hangar_store::repo::fleet_retention::FleetRetentionRepo;
use sqlx::SqlitePool;

/// How long a hook event keeps its payload.
///
/// 48h, from measurement rather than taste: the live corpus accrues ~102 MB of
/// payload per day, so 7d would hold ~713 MB steady-state (the exact number
/// being fixed) while 48h holds ~204 MB.
const PAYLOAD_TTL_MS: i64 = 48 * 60 * 60 * 1000;

/// How long a `fleet_event` row survives at all.
const ROW_TTL_MS: i64 = 30 * 24 * 60 * 60 * 1000;

/// Payload budget before stage C tightens the eviction cutoff.
const PAYLOAD_CEILING_BYTES: i64 = 1024 * 1024 * 1024;

/// Progressively tighter eviction cutoffs stage C walks while over budget.
///
/// Bounded and fixed rather than computed: an unbounded search for a cutoff
/// that fits would, on a corpus that is entirely younger than any cutoff,
/// converge on deleting everything. The floor keeps two hours of hook payload
/// no matter how hard the ceiling is breached.
const CEILING_TTL_LADDER_MS: [i64; 3] =
    [24 * 60 * 60 * 1000, 6 * 60 * 60 * 1000, 2 * 60 * 60 * 1000];

/// Rows touched per statement.
///
/// 500, not 5,000. At the measured 8.3 KB mean payload a 5,000-row eviction
/// rewrites ~41 MB inside ONE statement, and every dirty page of it is pinned
/// in cache and in the WAL until the statement commits. 500 rows is ~4 MB, which
/// is a size the page cache and the checkpointer both absorb without growing.
const BATCH: i64 = 500;

/// Rows one pass may evict before it stops and waits for the next.
///
/// The bound that keeps a cold start on a large table from spiking RSS. At
/// [`BATCH`] this is 40 statements of ~4 MB, so a pass moves ~160 MB and then
/// yields the writer completely. A 70k-row backlog drains in ~4 passes, which
/// at [`CATCHUP_PERIOD`] is a few minutes, not one 2.6 GB burst.
const MAX_ROWS_PER_PASS: u64 = 20_000;

/// Rows one pass may delete before it stops. Separate from the eviction budget
/// because a deleted row costs a fraction of a rewritten one.
const MAX_DELETES_PER_PASS: u64 = 20_000;

/// Yield between batches so the reconciler and the ACP writer can interleave.
const BATCH_PAUSE: Duration = Duration::from_millis(50);

/// Gap between passes once the ledger is swept and only new events arrive.
const SWEEP_PERIOD: Duration = Duration::from_hours(1);

/// Gap between passes while a backlog remains. Short enough to drain a cold
/// start in minutes, long enough that the writer is idle between passes.
const CATCHUP_PERIOD: Duration = Duration::from_mins(1);

/// How long the sweep stays out of the way of daemon boot.
const FIRST_PASS_DELAY: Duration = Duration::from_mins(5);

/// What one [`run_retention_pass`] reclaimed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RetentionOutcome {
    /// Rows whose payload was blanked by stage A.
    pub evicted: u64,
    /// Rows deleted outright by stage B.
    pub deleted: u64,
    /// Extra rows blanked by stage C's tightened cutoffs.
    pub ceiling_evicted: u64,
    /// Live payload bytes after the pass.
    pub live_bytes: i64,
    /// A stage stopped on its per-pass budget rather than on running out of
    /// work, so there is more backlog and the next pass should follow soon.
    pub backlog_remaining: bool,
}

impl RetentionOutcome {
    /// How long to wait before the next pass.
    const fn next_pass_after(self) -> Duration {
        if self.backlog_remaining {
            CATCHUP_PERIOD
        } else {
            SWEEP_PERIOD
        }
    }

    const fn touched_anything(self) -> bool {
        self.evicted > 0 || self.deleted > 0 || self.ceiling_evicted > 0
    }
}

/// Run the retention sweep forever, catching up fast and then idling.
#[must_use]
pub fn spawn_retention_sweeper(pool: SqlitePool) -> tokio::task::JoinHandle<()> {
    use ainb_hangar_core::clock::{HangarClock as _, SystemClock};
    tokio::spawn(async move {
        let clock = SystemClock;
        // A plain sleep loop, not a ticker. The gap is decided by the pass's own
        // result (backlog or not), and sleeping AFTER the work is already
        // exactly `MissedTickBehavior::Delay` — a pass that overruns can never
        // be followed by a catch-up tick with no sleep in between. The first
        // sleep also keeps the heaviest write this daemon does off the boot
        // path, where it would starve the routes a fresh daemon must serve.
        tokio::time::sleep(FIRST_PASS_DELAY).await;
        loop {
            let wait = match run_retention_pass(&pool, clock.now_ms()).await {
                Ok(outcome) => {
                    if outcome.touched_anything() {
                        tracing::info!(
                            evicted = outcome.evicted,
                            deleted = outcome.deleted,
                            ceiling_evicted = outcome.ceiling_evicted,
                            live_bytes = outcome.live_bytes,
                            backlog_remaining = outcome.backlog_remaining,
                            "fleet_event retention pass"
                        );
                    }
                    outcome.next_pass_after()
                }
                Err(error) => {
                    tracing::warn!(error = %error, "fleet_event retention failed");
                    SWEEP_PERIOD
                }
            };
            tokio::time::sleep(wait).await;
        }
    })
}

/// One full A → B → C pass, bounded by the production budgets.
///
/// # Errors
/// Propagates the `SQLite` failure from any stage.
pub async fn run_retention_pass(
    pool: &SqlitePool,
    now_ms: i64,
) -> Result<RetentionOutcome, sqlx::Error> {
    run_retention_pass_bounded(pool, now_ms, MAX_ROWS_PER_PASS, MAX_DELETES_PER_PASS).await
}

/// One pass with explicit budgets, so a test can prove boundedness without
/// seeding twenty thousand rows.
///
/// `evict_budget` is shared across stages A and C: the point of the budget is to
/// cap the BYTES one pass puts in flight, and the ceiling ladder rewrites the
/// same kind of row that stage A does.
///
/// # Errors
/// Propagates the `SQLite` failure from any stage.
pub async fn run_retention_pass_bounded(
    pool: &SqlitePool,
    now_ms: i64,
    evict_budget: u64,
    delete_budget: u64,
) -> Result<RetentionOutcome, sqlx::Error> {
    let mut outcome = RetentionOutcome::default();
    let mut evict_left = evict_budget;

    let (evicted, capped) = evict_bounded(
        pool,
        now_ms.saturating_sub(PAYLOAD_TTL_MS),
        now_ms,
        &mut evict_left,
    )
    .await?;
    outcome.evicted = evicted;
    outcome.backlog_remaining |= capped;

    let mut delete_left = delete_budget;
    let (deleted, capped) =
        delete_bounded(pool, now_ms.saturating_sub(ROW_TTL_MS), &mut delete_left).await?;
    outcome.deleted = deleted;
    outcome.backlog_remaining |= capped;

    outcome.live_bytes = FleetRetentionRepo::live_payload_bytes(pool).await?;
    for ttl_ms in CEILING_TTL_LADDER_MS {
        if outcome.live_bytes <= PAYLOAD_CEILING_BYTES || evict_left == 0 {
            break;
        }
        tracing::warn!(
            live_bytes = outcome.live_bytes,
            ceiling_bytes = PAYLOAD_CEILING_BYTES,
            tightened_ttl_ms = ttl_ms,
            "fleet_event payload over ceiling — pulling the eviction cutoff forward"
        );
        // Same four event types at every rung. Stage C tightens the WINDOW
        // only; widening the type set would break the disjointness that keeps
        // eviction off a live ask.
        let (evicted, capped) =
            evict_bounded(pool, now_ms.saturating_sub(ttl_ms), now_ms, &mut evict_left).await?;
        outcome.ceiling_evicted += evicted;
        outcome.backlog_remaining |= capped;
        outcome.live_bytes = FleetRetentionRepo::live_payload_bytes(pool).await?;
    }

    Ok(outcome)
}

/// Evict until the cutoff is exhausted or `budget_left` runs out.
///
/// Returns the rows evicted and whether it stopped on the BUDGET — the caller
/// needs those apart, because only the second one means "come back sooner".
async fn evict_bounded(
    pool: &SqlitePool,
    before_ms: i64,
    now_ms: i64,
    budget_left: &mut u64,
) -> Result<(u64, bool), sqlx::Error> {
    let mut total = 0;
    while *budget_left > 0 {
        let limit = BATCH.min(i64::try_from(*budget_left).unwrap_or(BATCH));
        let evicted =
            FleetRetentionRepo::evict_payloads_before(pool, before_ms, now_ms, limit).await?;
        total += evicted;
        *budget_left = budget_left.saturating_sub(evicted);
        if evicted == 0 {
            // Ran out of WORK, not budget: this cutoff is fully swept.
            return Ok((total, false));
        }
        yield_writer(pool).await?;
    }
    Ok((total, true))
}

/// Delete until the cutoff is exhausted or `budget_left` runs out.
///
/// Same shape and same reporting contract as [`evict_bounded`].
async fn delete_bounded(
    pool: &SqlitePool,
    before_ms: i64,
    budget_left: &mut u64,
) -> Result<(u64, bool), sqlx::Error> {
    let mut total = 0;
    while *budget_left > 0 {
        let limit = BATCH.min(i64::try_from(*budget_left).unwrap_or(BATCH));
        let deleted = FleetRetentionRepo::delete_events_before(pool, before_ms, limit).await?;
        total += deleted;
        *budget_left = budget_left.saturating_sub(deleted);
        if deleted == 0 {
            return Ok((total, false));
        }
        yield_writer(pool).await?;
    }
    Ok((total, true))
}

/// Hand the single `SQLite` writer back between batches.
///
/// Both halves matter. The sleep lets the reconciler and the ACP writer take
/// the lock; the checkpoint folds this batch's WAL frames back into the
/// database file so the log cannot grow for the length of a backlog sweep,
/// which is what turned the first production pass into a 2.6 GB RSS spike.
async fn yield_writer(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    tokio::time::sleep(BATCH_PAUSE).await;
    FleetRetentionRepo::checkpoint_wal(pool).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_hangar_store::Store;

    const DAY_MS: i64 = 24 * 60 * 60 * 1000;

    async fn seed(pool: &SqlitePool) {
        sqlx::query(
            "INSERT INTO fleet_session \
             (session_key, cwd, discovered_at, last_observed_at, current_request_fingerprint) \
             VALUES ('claude:s-1', '/tmp', 0, 0, 'fp-live')",
        )
        .execute(pool)
        .await
        .unwrap();
    }

    async fn event(
        pool: &SqlitePool,
        event_id: &str,
        observed_at: i64,
        event_type: &str,
        payload: &str,
        fingerprint: Option<&str>,
    ) {
        sqlx::query(
            "INSERT INTO fleet_event \
             (event_id, session_key, observed_at, authority, event_type, payload, \
              request_fingerprint, session_version, applied) \
             VALUES (?, 'claude:s-1', ?, 'authoritative', ?, ?, ?, 1, 1)",
        )
        .bind(event_id)
        .bind(observed_at)
        .bind(event_type)
        .bind(payload)
        .bind(fingerprint)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn payload(pool: &SqlitePool, event_id: &str) -> Option<String> {
        sqlx::query_scalar("SELECT payload FROM fleet_event WHERE event_id = ?")
            .bind(event_id)
            .fetch_optional(pool)
            .await
            .unwrap()
    }

    /// One pass, all three stages: an aged hook body goes, a fresh one stays, a
    /// month-old row disappears entirely, and the live ask survives both.
    #[tokio::test]
    async fn one_pass_evicts_aged_bodies_deletes_ancient_rows_and_spares_the_live_ask() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        let now = 60 * DAY_MS;
        seed(pool).await;

        event(
            pool,
            "e-ancient",
            now - 40 * DAY_MS,
            "PostToolUse",
            r#"{"x":1}"#,
            None,
        )
        .await;
        event(
            pool,
            "e-aged",
            now - 3 * DAY_MS,
            "PostToolUse",
            r#"{"x":1}"#,
            None,
        )
        .await;
        event(
            pool,
            "e-live-ask",
            now - 40 * DAY_MS,
            "AskUserQuestion",
            r#"{"q":1}"#,
            Some("fp-live"),
        )
        .await;
        event(
            pool,
            "e-fresh",
            now - 3_600_000,
            "PostToolUse",
            r#"{"x":1}"#,
            None,
        )
        .await;

        let outcome = run_retention_pass(pool, now).await.unwrap();

        assert_eq!(outcome.deleted, 1, "only the 40-day-old unreferenced row");
        assert_eq!(payload(pool, "e-ancient").await, None, "deleted outright");
        assert_eq!(
            payload(pool, "e-aged").await.unwrap(),
            "{}",
            "body evicted at 48h"
        );
        assert_eq!(
            payload(pool, "e-fresh").await.unwrap(),
            r#"{"x":1}"#,
            "inside the 48h window, untouched"
        );
        assert_eq!(
            payload(pool, "e-live-ask").await.unwrap(),
            r#"{"q":1}"#,
            "a 40-day-old event backing a CURRENT request survives both stages"
        );
        assert_eq!(outcome.ceiling_evicted, 0, "far under the byte ceiling");
    }

    /// A second pass on an already-swept ledger must do nothing. A retention
    /// sweep that rewrites rows every hour is itself a write-amplifier.
    #[tokio::test]
    async fn a_settled_ledger_costs_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        let now = 60 * DAY_MS;
        seed(pool).await;
        event(
            pool,
            "e-aged",
            now - 3 * DAY_MS,
            "PostToolUse",
            r#"{"x":1}"#,
            None,
        )
        .await;
        event(pool, "e-head", now, "PostToolUse", r#"{"x":1}"#, None).await;

        run_retention_pass(pool, now).await.unwrap();
        let second = run_retention_pass(pool, now).await.unwrap();

        assert_eq!(
            (second.evicted, second.deleted, second.ceiling_evicted),
            (0, 0, 0),
            "a converged sweep must not touch a single row"
        );
        assert!(
            !second.backlog_remaining,
            "a converged sweep must not ask to be re-run in a minute"
        );
    }

    /// Rows still holding a payload, oldest first — the backlog the sweep has
    /// yet to reach.
    async fn unevicted(pool: &SqlitePool) -> Vec<String> {
        sqlx::query_scalar(
            "SELECT event_id FROM fleet_event WHERE payload_evicted_at IS NULL ORDER BY revision",
        )
        .fetch_all(pool)
        .await
        .unwrap()
    }

    /// The backlog pass is BOUNDED and RESUMABLE ACROSS A RESTART.
    ///
    /// Seeds more evictable rows than one pass may touch, then drives three
    /// passes with the pool CLOSED AND REOPENED between each — a real daemon
    /// restart, not a loop iteration. Each pass must stop exactly on its budget,
    /// flag the remaining backlog, and the next must resume on the rows the
    /// previous one did not reach rather than restarting or redoing work.
    ///
    /// This is the guarantee that replaces a persisted watermark: the resume
    /// point lives in `payload_evicted_at`, per row, committed with the
    /// eviction it records, so no in-memory cursor can survive or disagree.
    #[tokio::test]
    async fn evicted_backlog_resumes_from_the_tombstone_after_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let now = 60 * DAY_MS;
        {
            let store = Store::open_in(dir.path()).await.unwrap();
            seed(store.pool()).await;
            for n in 0..25 {
                event(
                    store.pool(),
                    &format!("e-{n:02}"),
                    now - 3 * DAY_MS,
                    "PostToolUse",
                    r#"{"body":"aaaaaaaa"}"#,
                    None,
                )
                .await;
            }
            store.pool().close().await;
        }

        let mut seen = Vec::new();
        let mut outcomes = Vec::new();
        for _ in 0..3 {
            // Reopen from disk: whatever this pass resumes from must be durable.
            let store = Store::open_in(dir.path()).await.unwrap();
            let before = unevicted(store.pool()).await;
            let outcome = run_retention_pass_bounded(store.pool(), now, 10, 10).await.unwrap();
            let after = unevicted(store.pool()).await;
            seen.push(before.iter().filter(|id| !after.contains(id)).cloned().collect::<Vec<_>>());
            outcomes.push(outcome);
            store.pool().close().await;
        }

        assert_eq!(
            outcomes.iter().map(|o| o.evicted).collect::<Vec<_>>(),
            vec![10, 10, 5],
            "each pass must stop on its 10-row budget, never run the backlog down in one"
        );
        assert_eq!(
            outcomes.iter().map(|o| o.backlog_remaining).collect::<Vec<_>>(),
            vec![true, true, false],
            "a capped pass must ask to be re-run soon; the pass that finishes must not"
        );
        assert_eq!(
            seen[0],
            (0..10).map(|n| format!("e-{n:02}")).collect::<Vec<_>>()
        );
        assert_eq!(
            seen[1],
            (10..20).map(|n| format!("e-{n:02}")).collect::<Vec<_>>(),
            "the second pass must resume past the first, not redo it"
        );
        assert_eq!(
            seen[2],
            (20..25).map(|n| format!("e-{n:02}")).collect::<Vec<_>>()
        );

        let store = Store::open_in(dir.path()).await.unwrap();
        assert!(
            unevicted(store.pool()).await.is_empty(),
            "three bounded passes must between them leave no backlog"
        );
    }
}
