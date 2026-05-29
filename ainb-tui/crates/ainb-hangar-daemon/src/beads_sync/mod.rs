//! Hangar ↔ Beads sync (P2).
//!
//! Hangar mirrors user-created issues into Stevie's existing `bd` tracker so the
//! daily `bd` workflow keeps working while Hangar backfills. This module owns
//! the *outbound* half (Hangar → `bd`); the inbound poll loop and reconcile
//! command land in later P2 sub-beads.
//!
//! The outbound adapter ([`outbound::OutboundSync`]) is deliberately decoupled
//! from the persistence write site: a Hangar issue is committed first, then the
//! mirror runs as a best-effort follow-on. A `bd` failure is therefore
//! **non-fatal** — it surfaces a [`SyncError`] the caller logs and continues
//! past, leaving the Hangar issue intact and no half-written mapping row behind
//! (reconcile re-attempts the mirror later).
//!
//! The *inbound* half ([`inbound::InboundSync`]) reads `bd`'s current view and
//! lands status changes back on the mirrored Hangar issues; [`sync_loop::
//! run_inbound_loop`] drives it on a poll interval with failure backoff until a
//! [`CancellationToken`](tokio_util::sync::CancellationToken) cancels it (P2.4).

pub mod inbound;
pub mod outbound;
pub mod reconcile;
pub mod sync_loop;

pub use inbound::{InboundStats, InboundSync};
pub use outbound::{MirrorOutcome, OutboundSync, SyncError};
pub use reconcile::{
    decide_winner, ConflictRecord, ConflictWinner, ReconcileOpts, ReconcileReport, ReconcileService,
};
pub use sync_loop::run_inbound_loop;
