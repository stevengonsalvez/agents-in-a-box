//! Retention sweep over the `mutation_ledger` (spec D18).
//!
//! The ledger stores a serialized reply per mutation, so it grows with every
//! write the daemon serves and bounds nothing on its own. D18 fixes the bound
//! at 7 days or 100k rows, whichever comes first, and
//! [`MutationLedgerRepo::retain`] implements it — but a retention policy with
//! no caller is a comment. The sibling tables are the evidence: `fleet_event`
//! reached 1.1M rows and 847 MB under no retention at all.
//!
//! Two stages, and the order is the whole point (see the migration's own note):
//!
//! ```text
//!   expire  7 d / 100k live rows ──▶ drop the reply, KEEP the key
//!   delete  14 d / 100k tombstones ──▶ remove the key
//! ```
//!
//! Stage one drops all the bulk while leaving a ~60-byte tombstone, so a retry
//! that arrives late is answered `unknown{op_expired}` instead of executed a
//! second time. A single hard delete would bound the storage and silently
//! reintroduce the double-execution this whole phase exists to stop.
//!
//! Scheduling follows `fleet_retention`'s shape for the same reason it does:
//! `SQLite` has one writer, shared with the Fleet reconciler and the transcript
//! writer, so this runs on its own task with the sleep taken AFTER the work,
//! never on a ticker that could stack passes.

use std::time::Duration;

use ainb_hangar_store::repo::mutation_ledger::{
    LOCAL_HOST_ID, MutationLedgerRepo, RetentionPolicy,
};
use sqlx::SqlitePool;

/// Gap between steady-state passes.
///
/// Hourly, like the sibling sweeps. The bound is 7 days and 100k rows, so an
/// hour of drift is nothing against either, and a cheap no-op pass on a quiet
/// daemon costs one indexed scan.
const SWEEP_PERIOD: Duration = Duration::from_secs(60 * 60);

/// How long after boot the first pass runs.
///
/// Deliberately not zero: boot is when a daemon must serve the routes a
/// reconnecting client is already asking for, and the heaviest write this task
/// does has no deadline at all.
const FIRST_PASS_DELAY: Duration = Duration::from_secs(9 * 60);

/// Spawn the ledger retention sweeper.
///
/// Never fatal: a failed pass is logged and retried on the next period, because
/// an unbounded ledger is a slow problem and a daemon that stops serving is an
/// immediate one.
pub fn spawn_mutation_retention_sweeper(pool: SqlitePool) -> tokio::task::JoinHandle<()> {
    use ainb_hangar_core::clock::{HangarClock as _, SystemClock};
    tokio::spawn(async move {
        let clock = SystemClock;
        tokio::time::sleep(FIRST_PASS_DELAY).await;
        loop {
            match MutationLedgerRepo::retain(
                &pool,
                LOCAL_HOST_ID,
                clock.now_ms(),
                RetentionPolicy::default(),
            )
            .await
            {
                Ok(report) if report.expired == 0 && report.deleted == 0 => {}
                Ok(report) => tracing::info!(
                    expired = report.expired,
                    deleted = report.deleted,
                    "mutation_ledger retention pass"
                ),
                Err(error) => {
                    tracing::warn!(error = %error, "mutation_ledger retention failed");
                }
            }
            tokio::time::sleep(SWEEP_PERIOD).await;
        }
    })
}
