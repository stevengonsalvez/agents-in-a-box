//! Beads → Hangar inbound mirror (P2.4).
//!
//! [`InboundSync`] pulls the current state of Stevie's `bd` tracker and lands any
//! `bd`-side status change on the mirrored Hangar issue. It is the read half of
//! the two-way sync: where [`outbound`](super::outbound) pushes Hangar lifecycle
//! changes out to `bd`, this reconciles `bd`'s view back into Hangar.
//!
//! # Contract
//!
//! - **Scoped by mapping.** Only `bd` issues with a [`beads_mapping`] row are
//!   reconciled; a `bd` issue with no mapping is out-of-scope and skipped
//!   (logged at DEBUG) — Hangar does not adopt arbitrary `bd` issues here (that
//!   is the reconcile command's job in P2.5).
//! - **Source-gated.** A [`MappingSource::Swarm`] row is owned by a swarm
//!   leader's `bd` lifecycle (per `feedback_swarm_leader_owns_bd_tracking`) and
//!   is skipped — inbound reconciles only `Hangar`-sourced mappings.
//! - **Status only, last-writer-wins per field.** Labels and other `bd` fields
//!   are advisory; inbound touches only the Hangar issue's lifecycle `state`.
//! - **Idempotent.** A tick where `bd`'s status already matches Hangar produces
//!   no write (`updated == 0`), so the poll loop is safe to run every interval.
//!
//! The adapter holds borrows (`bd`, `mapping`, `pool`, `clock`) so the daemon
//! composes it per-operation from its long-lived service registry; it is
//! `Send + Sync`.
//!
//! [`beads_mapping`]: ainb_hangar_store::repo::beads_mapping

use ainb_hangar_core::clock::HangarClock;
use ainb_hangar_store::repo::beads_mapping::{BeadsMappingRepo, MappingSource};
use ainb_hangar_store::repo::issue::IssueRepo;
use chrono::{DateTime, TimeZone, Utc};
use sqlx::SqlitePool;

use crate::beads_adapter::{BdClient, BdListFilter, BdStatus};

use super::outbound::SyncError;

/// The Hangar issue lifecycle state a closed `bd` issue maps to.
const HANGAR_STATE_DONE: &str = "done";
/// The Hangar issue lifecycle state any non-closed `bd` issue maps to.
const HANGAR_STATE_OPEN: &str = "open";
/// The `bd` label the inbound poll filters on — only issues tagged for sync.
pub const SYNC_LABEL: &str = "hangar-v1";

/// Counters describing what one [`InboundSync::reconcile_once`] pass did.
///
/// `scanned` is every `bd` issue the list returned; `updated` is how many
/// Hangar issues actually changed state; `skipped` covers out-of-scope rows
/// (no mapping, swarm-sourced, or unknown Hangar issue); `errors` counts
/// per-issue failures that did not abort the whole pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InboundStats {
    /// Total `bd` issues the list returned.
    pub scanned: usize,
    /// Hangar issues whose state was changed this pass.
    pub updated: usize,
    /// `bd` issues skipped as out-of-scope (no/swarm mapping, missing issue).
    pub skipped: usize,
    /// Per-issue errors that did not abort the pass.
    pub errors: usize,
}

/// Reconciles `bd`-side status changes back into the mirrored Hangar issues.
pub struct InboundSync<'a> {
    bd: &'a BdClient,
    mapping: &'a BeadsMappingRepo<'a>,
    pool: &'a SqlitePool,
    clock: &'a dyn HangarClock,
}

impl<'a> InboundSync<'a> {
    /// Compose the adapter from its borrowed dependencies.
    #[must_use]
    pub const fn new(
        bd: &'a BdClient,
        mapping: &'a BeadsMappingRepo<'a>,
        pool: &'a SqlitePool,
        clock: &'a dyn HangarClock,
    ) -> Self {
        Self {
            bd,
            mapping,
            pool,
            clock,
        }
    }

    /// Pull `bd`'s current view (filtered to the [`SYNC_LABEL`]) and reconcile
    /// every mapped Hangar issue's lifecycle state with it.
    ///
    /// Returns [`InboundStats`] summarising the pass. A per-issue store error
    /// is counted in `errors` and logged, but does not abort the whole pass; a
    /// failure of the `bd list` call itself (binary missing, non-zero exit, bad
    /// JSON) aborts and surfaces as [`SyncError`] so the poll loop can back off.
    ///
    /// # Errors
    ///
    /// Returns [`SyncError::Bd`] if the `bd list` invocation fails, or
    /// [`SyncError::BadClock`] if the injected clock is out of range.
    pub async fn reconcile_once(&self) -> Result<InboundStats, SyncError> {
        let filter = BdListFilter {
            labels: vec![SYNC_LABEL.to_string()],
            since: None,
            // The inbound sync exists to detect `bd`-side closures and land them
            // on the mirrored Hangar issue. `bd list` hides closed issues by
            // default, so without `--all` a close is invisible and the issue
            // never reaches `done`.
            include_closed: true,
        };
        let bd_issues = self.bd.list(filter)?;
        let now = self.now()?;

        let mut stats = InboundStats {
            scanned: bd_issues.len(),
            ..InboundStats::default()
        };

        for bd_issue in &bd_issues {
            match self.reconcile_issue(bd_issue, now).await {
                Ok(true) => stats.updated += 1,
                Ok(false) => stats.skipped += 1,
                Err(e) => {
                    stats.errors += 1;
                    tracing::warn!(bd_id = %bd_issue.id, error = %e, "inbound reconcile failed for issue");
                }
            }
        }

        tracing::debug!(
            scanned = stats.scanned,
            updated = stats.updated,
            skipped = stats.skipped,
            errors = stats.errors,
            "inbound reconcile pass complete"
        );
        Ok(stats)
    }

    /// Reconcile one `bd` issue against its mapped Hangar issue.
    ///
    /// Returns `Ok(true)` when the Hangar issue's state was changed, `Ok(false)`
    /// when the row was skipped (no mapping, swarm-sourced, missing Hangar issue,
    /// or already in sync), or a [`SyncError`] on a store failure.
    async fn reconcile_issue(
        &self,
        bd_issue: &crate::beads_adapter::BdIssue,
        now: DateTime<Utc>,
    ) -> Result<bool, SyncError> {
        // Out of scope: a bd issue Hangar never mirrored.
        let Some(row) = self.mapping.find_by_bd(bd_issue.id.as_str()).await? else {
            tracing::debug!(bd_id = %bd_issue.id, "unmapped bd issue; out of scope, skipping");
            return Ok(false);
        };

        // Swarm-sourced rows belong to a leader's bd lifecycle — never reconcile.
        if row.source == MappingSource::Swarm {
            tracing::debug!(bd_id = %bd_issue.id, "swarm-sourced mapping; skipping inbound");
            return Ok(false);
        }

        let desired_state = hangar_state_for(&bd_issue.status);

        // The Hangar issue must still exist; if it was deleted out from under the
        // mapping, treat as out-of-scope (reconcile command handles dangling rows).
        let Some(issue) = IssueRepo::get_by_id(self.pool, &row.hangar_id).await? else {
            tracing::debug!(hangar_id = %row.hangar_id, "mapped Hangar issue missing; skipping");
            return Ok(false);
        };

        // Idempotent: no write when the state already matches.
        if issue.state == desired_state {
            return Ok(false);
        }

        IssueRepo::update_state(self.pool, &row.hangar_id, desired_state).await?;
        self.mapping
            .update_last_synced(&row.hangar_id, now)
            .await?;
        tracing::info!(
            hangar_id = %row.hangar_id,
            bd_id = %bd_issue.id,
            from = %issue.state,
            to = desired_state,
            "reconciled bd status change into Hangar"
        );
        Ok(true)
    }

    /// The injected clock's current instant as a UTC timestamp.
    fn now(&self) -> Result<DateTime<Utc>, SyncError> {
        let ms = self.clock.now_ms();
        match Utc.timestamp_millis_opt(ms) {
            chrono::LocalResult::Single(dt) => Ok(dt),
            _ => Err(SyncError::BadClock(ms)),
        }
    }
}

/// Map a `bd` lifecycle status onto the Hangar issue state string.
///
/// `bd`'s terminal `closed` maps to Hangar `done`; every other status (open,
/// in-progress, blocked, or any unknown token) maps to `open` — Hangar's issue
/// model is binary (open / done) for sync purposes.
const fn hangar_state_for(status: &BdStatus) -> &'static str {
    match status {
        BdStatus::Closed => HANGAR_STATE_DONE,
        _ => HANGAR_STATE_OPEN,
    }
}

#[cfg(test)]
mod tests {
    use super::{hangar_state_for, InboundSync, HANGAR_STATE_DONE, HANGAR_STATE_OPEN};
    use crate::beads_adapter::BdStatus;

    #[test]
    fn closed_maps_to_done_others_to_open() {
        assert_eq!(hangar_state_for(&BdStatus::Closed), HANGAR_STATE_DONE);
        assert_eq!(hangar_state_for(&BdStatus::Open), HANGAR_STATE_OPEN);
        assert_eq!(hangar_state_for(&BdStatus::InProgress), HANGAR_STATE_OPEN);
        assert_eq!(hangar_state_for(&BdStatus::Blocked), HANGAR_STATE_OPEN);
        assert_eq!(
            hangar_state_for(&BdStatus::Other("triaged".into())),
            HANGAR_STATE_OPEN
        );
    }

    /// [`InboundSync`] must stay `Send + Sync` so the daemon can hold it in an
    /// `Arc<>` and drive the poll loop from a spawned task.
    #[test]
    fn inbound_sync_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<InboundSync<'static>>();
    }
}
