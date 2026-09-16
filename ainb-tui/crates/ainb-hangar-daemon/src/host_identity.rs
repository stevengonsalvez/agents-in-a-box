//! Adopting the `fleet_event` rows a pre-#1066 daemon wrote under `local`.
//!
//! The mint adopts `fleet_session` in its own transaction at boot, because that
//! table is small. `fleet_event` can be measured in gigabytes (migration 0099),
//! so its rows move here instead: one bounded batch per write transaction, with
//! a pause between batches so the Fleet reconciler and the attention ingest keep
//! the single `SQLite` writer. A home with nothing to adopt pays one indexed
//! read and the task ends.

use std::time::Duration;

use ainb_hangar_store::repo::daemon_identity::{DaemonIdentityRepo, EVENT_ADOPTION_BATCH};
use ainb_hangar_store::repo::fleet::{WRITE_LOCK_ATTEMPTS, is_lock_contention};
use sqlx::SqlitePool;

/// Pause between two adoption batches.
const BATCH_PAUSE: Duration = Duration::from_millis(50);

/// Spawn the task that moves every `local` `fleet_event` row to `host_id`.
///
/// Never fatal. A batch that loses the write lock is retried after
/// [`BATCH_PAUSE`], up to [`WRITE_LOCK_ATTEMPTS`] times in a row, because boot is
/// when the single writer is busiest. Any other fault stops the task and leaves
/// the rest `local` for the next boot, which starts over from the same query.
pub fn spawn_event_adoption(pool: SqlitePool, host_id: String) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut adopted: u64 = 0;
        let mut contended: u32 = 0;
        loop {
            match DaemonIdentityRepo::adopt_local_events(&pool, &host_id, EVENT_ADOPTION_BATCH)
                .await
            {
                Ok(0) => break,
                Ok(moved) => {
                    adopted += moved;
                    contended = 0;
                }
                Err(error) if is_lock_contention(&error) && contended < WRITE_LOCK_ATTEMPTS - 1 => {
                    contended += 1;
                }
                Err(error) => {
                    tracing::error!(%error, adopted, "fleet_event host adoption stopped");
                    return;
                }
            }
            tokio::time::sleep(BATCH_PAUSE).await;
        }
        if adopted > 0 {
            tracing::info!(adopted, %host_id, "fleet_event rows adopted by the minted host");
        }
    })
}
