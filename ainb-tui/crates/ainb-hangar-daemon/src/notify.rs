//! Raise-time notification-channel resolution (tcp T5).
//!
//! The routing decision for an attention row is made ONCE, at the moment it is
//! raised — the "compute-once-at-emit" design. [`resolve_channels`] reads the
//! notify rules (`notify_rule`, migration 0037) for the row's `(kind,
//! workspace_id)` and returns the [`ChannelSet`] the daemon then stamps onto both
//! the durable row and the `AttentionRaised` event. Every fan-out consumer
//! (bridge → phone, web push, notifyd → OS, ATC feed) filters on that stamped
//! set rather than re-resolving, so a rule edit that lands WHILE an attention is
//! in flight can never split-brain the delivery (one consumer sending, another
//! suppressing the SAME row).
//!
//! Best-effort, like the rest of the ingest path: a transient DB fault at resolve
//! time never downs the raise. It degrades to [`coded_fallback`] — escalation
//! stays loud (a human is being paged), every other kind falls back to
//! board-only — logged so the fault is visible, never silent.

use ainb_hangar_core::channel::ChannelSet;
use ainb_hangar_store::repo::attention::AttentionKind;
use ainb_hangar_store::repo::notify_rule::{NotifyRuleRepo, coded_fallback};
use sqlx::SqlitePool;

/// Resolve the push-channel set for an attention of `kind` in `workspace_id`'s
/// scope, at raise time. Never errors: a DB fault degrades to the kind's coded
/// fallback (logged), so a raise is never blocked on the routing lookup.
pub async fn resolve_channels(
    pool: &SqlitePool,
    kind: AttentionKind,
    workspace_id: Option<&str>,
) -> ChannelSet {
    match NotifyRuleRepo::resolve(pool, kind, workspace_id).await {
        Ok(set) => set,
        Err(e) => {
            tracing::warn!(
                error = %e,
                kind = kind.as_str(),
                "notify-rule resolve failed at raise time; using coded fallback"
            );
            coded_fallback(kind)
        }
    }
}
