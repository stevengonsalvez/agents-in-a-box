//! Hangar → Beads outbound mirror (P2.3).
//!
//! [`OutboundSync`] mirrors a Hangar [`Issue`]'s lifecycle into `bd`:
//! [`mirror_create`](OutboundSync::mirror_create) creates the matching `bd`
//! issue and records the id correlation in `beads_mapping`;
//! [`mirror_close`](OutboundSync::mirror_close) closes the `bd` side when the
//! Hangar issue is resolved.
//!
//! # Contract
//!
//! - **Source-gated.** Only [`MappingSource::Hangar`] issues mirror; a
//!   `Swarm`-sourced issue is owned by a swarm leader's own `bd` lifecycle and
//!   is skipped outright (per `feedback_swarm_leader_owns_bd_tracking`), leaving
//!   no mapping row.
//! - **Idempotent.** A replayed `mirror_create` short-circuits via
//!   [`BeadsMappingRepo::find_by_hangar`] — it never issues a second `bd create`.
//! - **Non-fatal failure.** A `bd` failure surfaces a [`SyncError`] but writes
//!   **no** mapping row, so the Hangar issue (already committed before the
//!   mirror runs) stays intact and reconcile can re-attempt later.
//! - **Assignee crosswalk.** The polymorphic Hangar assignee is translated to
//!   `bd`'s flat string via [`assignee_crosswalk`] (agent ids prefixed
//!   `agent:`, member ids raw).
//!
//! The adapter holds borrows (`bd`, `mapping`, `clock`) so the daemon composes
//! it per-operation from its long-lived service registry; it is `Send + Sync`.

use ainb_hangar_core::assignee_crosswalk;
use ainb_hangar_core::clock::HangarClock;
use ainb_hangar_store::repo::beads_mapping::{
    BeadsMappingRepo, BeadsMappingRow, MappingKind, MappingSource,
};
use ainb_hangar_store::repo::issue::Issue;
use chrono::{DateTime, TimeZone, Utc};

use crate::beads_adapter::{BdClient, BdCreateArgs, BdError, BdId};

/// The outcome of an outbound mirror call.
///
/// Lets the caller distinguish a real `bd` write from the two short-circuits
/// (swarm-gated, already-mirrored) without inspecting side effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MirrorOutcome {
    /// A `bd` write happened and a mapping row was created/updated.
    Mirrored,
    /// The issue was `Swarm`-sourced; no `bd` call, no mapping row.
    SkippedSwarm,
    /// A mapping row already existed; the call short-circuited (idempotent).
    AlreadyMirrored,
}

/// Failure modes of the outbound mirror.
///
/// All variants are **non-fatal at the call site** — the caller logs and
/// continues, leaving the Hangar issue intact (it was committed before the
/// mirror ran). No mapping row is written on failure.
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    /// The underlying `bd` adapter call failed (binary missing, non-zero exit,
    /// bad JSON, lock timeout).
    #[error("bd adapter error: {0}")]
    Bd(#[from] BdError),
    /// Persisting (or reading) the `beads_mapping` correlation failed.
    #[error("beads_mapping store error: {0}")]
    Store(#[from] sqlx::Error),
    /// A `mirror_close` was asked to close an issue that was never mirrored
    /// (no `beads_mapping` row), so there is no `bd` id to close.
    #[error("cannot mirror-close issue {0}: no beads_mapping row")]
    NotMirrored(String),
    /// The Hangar clock produced a value outside the representable timestamp
    /// range (should be impossible for any real wall clock).
    #[error("clock produced an out-of-range timestamp: {0}ms")]
    BadClock(i64),
    /// A reconcile resolved a drifted pair in Hangar's favour, but the winning
    /// Hangar state cannot be mirrored onto `bd` because `bd` exposes no
    /// `reopen` verb. This happens only for the open-Hangar-wins-over-closed-`bd`
    /// case: the pair stays drifted, the resolution is *not* applied, and the
    /// mapping's `last_synced` is deliberately left untouched so the next pass
    /// can re-detect (rather than masking the drift behind a bumped timestamp).
    #[error(
        "cannot repair drift for {hangar_id}/{bd_id}: Hangar issue is open but bd is closed and bd has no reopen verb"
    )]
    Unreconcilable {
        /// The Hangar issue id of the unrepairable pair.
        hangar_id: String,
        /// The `bd` issue id of the unrepairable pair.
        bd_id: String,
    },
}

/// Mirrors Hangar issue lifecycle changes into `bd` and records the id
/// correlation in `beads_mapping`.
pub struct OutboundSync<'a> {
    bd: &'a BdClient,
    mapping: &'a BeadsMappingRepo<'a>,
    clock: &'a dyn HangarClock,
}

impl<'a> OutboundSync<'a> {
    /// Compose the adapter from its borrowed dependencies.
    #[must_use]
    pub const fn new(
        bd: &'a BdClient,
        mapping: &'a BeadsMappingRepo<'a>,
        clock: &'a dyn HangarClock,
    ) -> Self {
        Self { bd, mapping, clock }
    }

    /// Mirror a freshly-created Hangar issue into `bd`.
    ///
    /// `source` gates the mirror: a [`MappingSource::Swarm`] issue is skipped.
    /// `labels` are attached to the `bd` issue (a single comma-joined `--labels`
    /// flag per `bd create`). On success a `beads_mapping` row correlating the
    /// two ids is inserted with `last_synced = clock.now()`.
    ///
    /// # Errors
    ///
    /// Returns [`SyncError::Bd`] if the `bd create` fails, [`SyncError::Store`]
    /// if reading or writing `beads_mapping` fails, or [`SyncError::BadClock`]
    /// if the injected clock is out of range. On any error **no** mapping row is
    /// written.
    #[tracing::instrument(
        name = "beads.push",
        skip(self, issue, source, labels),
        fields(hangar_id = %issue.id, bd_id = tracing::field::Empty)
    )]
    pub async fn mirror_create(
        &self,
        issue: &Issue,
        source: MappingSource,
        labels: &[String],
    ) -> Result<MirrorOutcome, SyncError> {
        if source == MappingSource::Swarm {
            tracing::debug!(issue_id = %issue.id, "skipping swarm-sourced issue outbound mirror");
            return Ok(MirrorOutcome::SkippedSwarm);
        }

        // Idempotent replay: a row already correlates this issue — do nothing.
        if self.mapping.find_by_hangar(&issue.id).await?.is_some() {
            tracing::debug!(issue_id = %issue.id, "issue already mirrored to bd; short-circuiting");
            return Ok(MirrorOutcome::AlreadyMirrored);
        }

        let bd_issue = self.bd.create(BdCreateArgs {
            title: issue.title.clone(),
            description: issue.description.clone(),
            labels: labels.to_vec(),
            assignee: issue.assignee.as_ref().map(assignee_crosswalk::hangar_to_bd),
        })?;

        // The `bd` id is only known after the create call returns; record it onto
        // the `beads.push` span now so the span carries the full id correlation.
        tracing::Span::current().record("bd_id", bd_issue.id.to_string().as_str());

        self.mapping
            .insert(&BeadsMappingRow {
                hangar_id: issue.id.clone(),
                bd_id: bd_issue.id.to_string(),
                hangar_kind: MappingKind::Issue,
                bd_kind: MappingKind::Issue,
                source,
                last_synced: self.now()?,
            })
            .await?;

        tracing::info!(issue_id = %issue.id, bd_id = %bd_issue.id, "mirrored issue create to bd");
        Ok(MirrorOutcome::Mirrored)
    }

    /// Mirror a Hangar issue closure into a `bd close`.
    ///
    /// Looks up the existing `beads_mapping` row to find the `bd` id, closes it,
    /// and bumps `last_synced`.
    ///
    /// # Errors
    ///
    /// Returns [`SyncError::NotMirrored`] if no mapping row exists for the issue,
    /// [`SyncError::Bd`] if the `bd close` fails, [`SyncError::Store`] on a
    /// store error, or [`SyncError::BadClock`].
    pub async fn mirror_close(&self, issue: &Issue) -> Result<MirrorOutcome, SyncError> {
        let row = self
            .mapping
            .find_by_hangar(&issue.id)
            .await?
            .ok_or_else(|| SyncError::NotMirrored(issue.id.clone()))?;

        // Swarm-sourced rows are never mirrored on close either.
        if row.source == MappingSource::Swarm {
            tracing::debug!(issue_id = %issue.id, "skipping swarm-sourced issue outbound close");
            return Ok(MirrorOutcome::SkippedSwarm);
        }

        self.bd.close(&BdId::from(row.bd_id.as_str()), None)?;
        self.mapping.update_last_synced(&issue.id, self.now()?).await?;

        tracing::info!(issue_id = %issue.id, bd_id = %row.bd_id, "mirrored issue close to bd");
        Ok(MirrorOutcome::Mirrored)
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

#[cfg(test)]
mod tests {
    use super::OutboundSync;

    /// [`OutboundSync`] must stay `Send + Sync` so the daemon can hold it in an
    /// `Arc<>` on its shared service registry (P2.3 REFACTOR). This generic
    /// helper only compiles while the bound holds, failing the build the instant
    /// the adapter stops being thread-safe.
    #[test]
    fn outbound_sync_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<OutboundSync<'static>>();
    }
}
