//! Hourly payload-eviction sweep over the `fleet_provider_event` source ledger.
//!
//! One stage, not three. `fleet_event`'s sweeper
//! ([`crate::fleet_retention`]) also deletes rows and carries a byte ceiling;
//! this one only blanks `raw_payload`, because the row itself is the thing this
//! table exists to keep. `ingest_order`, `event_id`, `raw_blake3` and
//! `projection_revision` all survive every sweep.
//!
//! ## Why this exists at all
//!
//! `fleet_provider_event` had NO automatic retention from 0071 to 0093, on the
//! documented assumption that projection-source rows grow ~1.3 MB per YEAR.
//! Measured on a real profile: **372,031 rows / 2,207 MB**, a ~5.9 KB mean
//! payload. At that size the table saturated the single `SQLite` writer and
//! session spawn started failing with `database is locked`. The store module's
//! header records the reversed decision; this is the mechanism.
//!
//! ## What it never touches
//!
//! Both refusals live in the SQL, not here, so no caller can opt out of them
//! (see `FleetProviderEventRepo::evict_projected_payloads_before`):
//!
//! * `projection_revision IS NULL` — unreduced events, including the pending
//!   recovery work the Codex manager replays at startup. Their payload IS the
//!   replay input.
//! * `source = 'acp'` — ACP transcripts reclaim through the operator's
//!   export-then-delete, which exports before it destroys.
//!
//! ## No tombstone, and no watermark either
//!
//! `fleet_event` needs `payload_evicted_at` because its `'{}'` sentinel cannot
//! be told apart from an event that carried no body, and because an unstamped
//! row would be re-selected by every later batch. Here the sentinel is `''`,
//! which no provider envelope can be, so `raw_payload <> ''` is self-converging:
//! an evicted row leaves the sweep's partial index the moment it is written.
//! That doubles as the resume point — a daemon killed mid-backlog restarts on
//! exactly the rows it had not reached, with no in-memory cursor and no column
//! to persist. `a_backlog_stops_on_its_budget_and_resumes_across_a_restart`
//! proves that across a real pool close and reopen.
//!
//! ## The database FILE does not shrink, and must not be made to
//!
//! Eviction returns pages to `SQLite`'s freelist, where later writes reuse
//! them; it does not return them to the filesystem. Measured on a copy of a
//! real 2.1 GB profile: this sweep reclaimed 1,159.8 MB of payload (1,506.0 MB
//! live payload down to 346.2 MB) and the file stayed 2.1 GB with 258,001 free
//! pages inside it. That is the intended outcome — the table stops GROWING —
//! and an operator watching `ls -lh hangar.db` should not read it as a failure.
//! `VACUUM` would rewrite the whole file under an exclusive lock, which is
//! precisely the writer stall this module exists to prevent.
//!
//! Scheduling discipline is inherited wholesale from [`crate::fleet_retention`]
//! and is load-bearing for the same reason: ONE writer, shared with the Fleet
//! reconciler and the ACP transcript writer. Own task, bounded batches, a yield
//! and a WAL checkpoint between them, and the sleep taken AFTER the work.

use std::time::Duration;

use ainb_hangar_store::repo::fleet_provider_event::FleetProviderEventRepo;
use ainb_hangar_store::repo::fleet_retention::FleetRetentionRepo;
use sqlx::SqlitePool;

/// How long a reduced provider envelope keeps its raw payload.
///
/// 7 days, from the measurement rather than taste. Replayed against a copy of a
/// real 245,295-row profile holding 1,506.0 MB of payload over a 23.6-day span:
/// this cutoff evicts 126,849 rows and reclaims 1,159.8 MB, leaving 346.2 MB
/// live — a steady state the single `SQLite` writer carries without contending
/// session spawn. (The larger 372,031-row / 2,207 MB figure quoted elsewhere is
/// the same table measured later; the ratio is what matters.)
///
/// Deliberately looser than `fleet_event`'s 48h: these are SOURCE envelopes,
/// and a week is the window in which re-reducing recent history is still
/// plausibly useful.
const PAYLOAD_TTL_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// Rows touched per statement.
///
/// 500, matching `fleet_event`'s batch for the same reason: at the measured
/// ~5.9 KB mean payload one statement rewrites ~3 MB, and every dirty page of
/// it stays pinned in cache and in the WAL until that statement commits.
const BATCH: i64 = 500;

/// Rows one pass may evict before it stops and waits for the next.
///
/// ~126 MB of rewritten payload per pass at the measured 6.3 KB mean. The bound
/// exists because the first pass against an un-swept multi-gigabyte table is the
/// dangerous one: `fleet_event`'s equivalent cold start peaked the daemon at
/// 2,599 MB RSS before this cap and its WAL checkpoint were added. Replayed on
/// the real profile the whole 126,849-row backlog took 254 statements across 7
/// passes, so at [`CATCHUP_PERIOD`] a cold start drains in about seven minutes
/// rather than one burst.
const MAX_ROWS_PER_PASS: u64 = 20_000;

/// Yield between batches so the reconciler and the ACP writer can interleave.
const BATCH_PAUSE: Duration = Duration::from_millis(50);

/// Gap between passes once the ledger is swept and only new events arrive.
const SWEEP_PERIOD: Duration = Duration::from_hours(1);

/// Gap between passes while a backlog remains.
const CATCHUP_PERIOD: Duration = Duration::from_mins(1);

/// How long the sweep stays out of the way of daemon boot.
///
/// Seven minutes, not the five [`crate::fleet_retention`] uses, so the two
/// janitors' FIRST passes do not land on the same minute: both rewrite payload
/// through the SAME single writer, and starting two cold drains together
/// doubles the bytes in flight against the lock session spawn needs.
///
/// The stagger separates only that first pass. On a cold backlog both re-arm at
/// [`CATCHUP_PERIOD`], so the drains overlap from roughly t+7min until the
/// shorter finishes. That is tolerated, not prevented: each pass is bounded by
/// [`MAX_ROWS_PER_PASS`], yields between batches and checkpoints, so overlapping
/// passes share the writer rather than monopolising it. Serialising them would
/// need a shared lock the two modules do not have, and the measured cost does
/// not justify one.
const FIRST_PASS_DELAY: Duration = Duration::from_mins(7);

/// What one [`run_provider_retention_pass`] reclaimed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProviderRetentionOutcome {
    /// Rows whose `raw_payload` was blanked.
    pub evicted: u64,
    /// The pass stopped on its per-pass budget rather than on running out of
    /// work, so there is more backlog and the next pass should follow soon.
    pub backlog_remaining: bool,
}

impl ProviderRetentionOutcome {
    /// How long to wait before the next pass.
    const fn next_pass_after(self) -> Duration {
        if self.backlog_remaining {
            CATCHUP_PERIOD
        } else {
            SWEEP_PERIOD
        }
    }
}

/// Run the provider-payload sweep forever, catching up fast and then idling.
#[must_use]
pub fn spawn_provider_retention_sweeper(pool: SqlitePool) -> tokio::task::JoinHandle<()> {
    use ainb_hangar_core::clock::{HangarClock as _, SystemClock};
    tokio::spawn(async move {
        let clock = SystemClock;
        // A plain sleep loop, not a ticker: the gap is decided by the pass's own
        // result, and sleeping AFTER the work means an overrunning pass can
        // never be followed by a catch-up tick with no sleep in between.
        tokio::time::sleep(FIRST_PASS_DELAY).await;
        loop {
            let wait = match run_provider_retention_pass(&pool, clock.now_ms()).await {
                Ok(outcome) => {
                    if outcome.evicted > 0 {
                        tracing::info!(
                            evicted = outcome.evicted,
                            backlog_remaining = outcome.backlog_remaining,
                            "fleet_provider_event retention pass"
                        );
                    }
                    outcome.next_pass_after()
                }
                Err(error) => {
                    tracing::warn!(error = %error, "fleet_provider_event retention failed");
                    SWEEP_PERIOD
                }
            };
            tokio::time::sleep(wait).await;
        }
    })
}

/// One pass, bounded by the production budget.
///
/// # Errors
/// Propagates the `SQLite` failure.
pub async fn run_provider_retention_pass(
    pool: &SqlitePool,
    now_ms: i64,
) -> Result<ProviderRetentionOutcome, sqlx::Error> {
    run_provider_retention_pass_bounded(pool, now_ms, MAX_ROWS_PER_PASS).await
}

/// One pass with an explicit budget, so a test can prove boundedness without
/// seeding twenty thousand rows.
///
/// # Errors
/// Propagates the `SQLite` failure.
pub async fn run_provider_retention_pass_bounded(
    pool: &SqlitePool,
    now_ms: i64,
    evict_budget: u64,
) -> Result<ProviderRetentionOutcome, sqlx::Error> {
    let before_ms = now_ms.saturating_sub(PAYLOAD_TTL_MS);
    let mut outcome = ProviderRetentionOutcome::default();
    let mut budget_left = evict_budget;
    while budget_left > 0 {
        let limit = BATCH.min(i64::try_from(budget_left).unwrap_or(BATCH));
        let evicted =
            FleetProviderEventRepo::evict_projected_payloads_before(pool, before_ms, limit).await?;
        outcome.evicted += evicted;
        budget_left = budget_left.saturating_sub(evicted);
        if evicted == 0 {
            // Ran out of WORK, not budget: the cutoff is fully swept.
            return Ok(outcome);
        }
        yield_writer(pool).await?;
    }
    outcome.backlog_remaining = true;
    Ok(outcome)
}

/// Hand the single `SQLite` writer back between batches.
///
/// The sleep lets the reconciler and the ACP writer take the lock; the
/// checkpoint folds this batch's WAL frames back into the database file, so the
/// log cannot grow for the length of a backlog sweep. Shared with
/// [`crate::fleet_retention`] rather than reimplemented — the pragma is a
/// property of the connection, not of either table.
async fn yield_writer(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    tokio::time::sleep(BATCH_PAUSE).await;
    FleetRetentionRepo::checkpoint_wal(pool).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_hangar_store::Store;
    use ainb_hangar_store::repo::fleet_provider_event::NewFleetProviderEvent;

    const DAY_MS: i64 = 24 * 60 * 60 * 1000;

    fn envelope(event_id: &str, source: &str, observed_at: i64) -> NewFleetProviderEvent {
        NewFleetProviderEvent {
            event_id: event_id.to_string(),
            provider: "codex".to_string(),
            source: source.to_string(),
            session_key: Some("codex:s-1".to_string()),
            provider_session_id: Some("s-1".to_string()),
            observed_at,
            received_at: observed_at,
            event_type: "codex/event".to_string(),
            raw_payload: format!(r#"{{"envelope":"{event_id}"}}"#),
        }
    }

    /// Mint one real `fleet_event` revision. `projection_revision` is a foreign
    /// key and `PRAGMA foreign_keys` is ON.
    async fn seed_revision(pool: &SqlitePool, event_id: &str) -> i64 {
        sqlx::query(
            "INSERT OR IGNORE INTO fleet_session \
             (session_key, cwd, discovered_at, last_observed_at) \
             VALUES ('codex:s-1', '/tmp', 0, 0)",
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO fleet_event \
             (event_id, session_key, observed_at, authority, event_type, payload, \
              session_version, applied) \
             VALUES (?, 'codex:s-1', 0, 'authoritative', 'PostToolUse', '{}', 1, 1)",
        )
        .bind(event_id)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    /// Append an envelope and, unless it is pending, mark it reduced.
    async fn seed(
        pool: &SqlitePool,
        event_id: &str,
        source: &str,
        observed_at: i64,
        reduced: bool,
    ) {
        FleetProviderEventRepo::append(pool, &envelope(event_id, source, observed_at))
            .await
            .unwrap();
        if reduced {
            let revision = seed_revision(pool, &format!("fe-{event_id}")).await;
            FleetProviderEventRepo::mark_projected(pool, event_id, revision).await.unwrap();
        }
    }

    async fn payload(pool: &SqlitePool, event_id: &str) -> String {
        FleetProviderEventRepo::get(pool, event_id).await.unwrap().unwrap().raw_payload
    }

    /// One pass at the real TTL: a reduced envelope past the window loses its
    /// bytes, and each of the three protected shapes keeps them.
    #[tokio::test]
    async fn one_pass_evicts_reduced_envelopes_and_spares_pending_acp_and_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        let now = 60 * DAY_MS;
        seed(pool, "aged", "codex_app_server", now - 30 * DAY_MS, true).await;
        seed(
            pool,
            "pending",
            "codex_app_server",
            now - 30 * DAY_MS,
            false,
        )
        .await;
        seed(pool, "acp", "acp", now - 30 * DAY_MS, true).await;
        seed(pool, "fresh", "codex_app_server", now - DAY_MS, true).await;

        let outcome = run_provider_retention_pass(pool, now).await.unwrap();

        assert_eq!(outcome.evicted, 1, "only the reduced, aged envelope");
        assert!(!outcome.backlog_remaining);
        assert_eq!(payload(pool, "aged").await, "", "past the 7-day window");
        assert_eq!(
            payload(pool, "pending").await,
            r#"{"envelope":"pending"}"#,
            "unreduced rows are the Codex manager's startup replay input"
        );
        assert_eq!(
            payload(pool, "acp").await,
            r#"{"envelope":"acp"}"#,
            "ACP transcripts reclaim through the operator's export-then-delete"
        );
        assert_eq!(
            payload(pool, "fresh").await,
            r#"{"envelope":"fresh"}"#,
            "inside the window, untouched"
        );
    }

    /// A second pass on an already-swept ledger must do nothing. A retention
    /// sweep that rewrites rows every hour is itself a write-amplifier.
    #[tokio::test]
    async fn a_settled_ledger_costs_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        let now = 60 * DAY_MS;
        seed(pool, "aged", "codex_app_server", now - 30 * DAY_MS, true).await;
        seed(pool, "fresh", "codex_app_server", now, true).await;

        run_provider_retention_pass(pool, now).await.unwrap();
        let second = run_provider_retention_pass(pool, now).await.unwrap();

        assert_eq!(
            second,
            ProviderRetentionOutcome {
                evicted: 0,
                backlog_remaining: false
            },
            "a converged sweep must touch nothing and must not ask to be re-run in a minute"
        );
    }

    /// Rows still holding a payload, oldest first — the backlog the sweep has
    /// yet to reach.
    async fn unevicted(pool: &SqlitePool) -> Vec<String> {
        sqlx::query_scalar(
            "SELECT event_id FROM fleet_provider_event \
             WHERE raw_payload <> '' ORDER BY ingest_order",
        )
        .fetch_all(pool)
        .await
        .unwrap()
    }

    /// The pass is BOUNDED and RESUMABLE ACROSS A RESTART.
    ///
    /// Drives three passes with the pool CLOSED AND REOPENED between each — a
    /// real daemon restart, not a loop iteration. Each must stop on its budget,
    /// flag the backlog, and resume on the rows the previous one did not reach.
    /// This is the guarantee that replaces a persisted watermark: the resume
    /// point is the blanked payload itself, committed with the eviction.
    #[tokio::test]
    async fn a_backlog_stops_on_its_budget_and_resumes_across_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let now = 60 * DAY_MS;
        {
            let store = Store::open_in(dir.path()).await.unwrap();
            for index in 0..25 {
                seed(
                    store.pool(),
                    &format!("e-{index:02}"),
                    "codex_app_server",
                    now - 30 * DAY_MS + index,
                    true,
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
            let outcome = run_provider_retention_pass_bounded(store.pool(), now, 10).await.unwrap();
            let after = unevicted(store.pool()).await;
            seen.push(before.iter().filter(|id| !after.contains(id)).cloned().collect::<Vec<_>>());
            outcomes.push(outcome);
            store.pool().close().await;
        }

        assert_eq!(
            outcomes.iter().map(|o| o.evicted).collect::<Vec<_>>(),
            vec![10, 10, 5],
            "each pass must stop on its 10-row budget, never drain the backlog in one"
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
