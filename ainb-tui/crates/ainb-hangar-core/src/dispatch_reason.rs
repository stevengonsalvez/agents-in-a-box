//! **Dispatch reason codes** — the admission-time decision vocabulary
//! (multica parity #12).
//!
//! multica carries this in a dedicated *leaf* package,
//! `server/internal/dispatch/reason.go` (MUL-4525), deliberately separate from
//! failure classification: [`crate::…`]-side `FailureReason`
//! (`ainb_hangar_store::service::fail::FailureReason`) answers "why did a run
//! that STARTED end badly"; [`DispatchReason`] answers "why did (or did not) a
//! trigger turn into a run at all". Keeping the two vocabularies apart is what
//! stops declined dispatches polluting the failure-rate signal — the same
//! reasoning migration `0057` used when it gave `autopilot_run` a non-failure
//! `skipped` status.
//!
//! # Two properties carried over verbatim from the reference
//!
//! 1. **Deliberately generic where enumeration-safety matters.**
//!    [`DispatchReason::InvocationNotAllowed`] does *not* distinguish "the
//!    agent is private" from "the agent does not exist", so the code can never
//!    be used as an existence oracle by a caller probing ids. The specifics
//!    live in the free-text `detail`, never in the code.
//! 2. **One vocabulary shared by the decider and the serializer.** The service
//!    layer that decides and the handler layer that writes it to the wire read
//!    the same enum, so the two cannot drift.
//!
//! # The mapping this vocabulary is applied through
//!
//! The normative `run_card`-outcome → code table lives on the daemon's
//! recorder (`ainb-hangar-daemon/src/rpc/mod.rs`, `record_dispatch_attempt`),
//! next to the code that applies it. Two hangar divergences from the reference
//! are documented there: `runtime_offline` still *enqueues* (hangar's runtime
//! rows are per-`(daemon, provider)` and flip `offline` on a heartbeat grace
//! timer, so refusing would turn a transient gap into a hard failure), and
//! `Blocked` maps to [`DispatchReason::Deferred`] because hangar genuinely
//! promotes it later when the last blocker finishes.
//!
//! # Reserved variants
//!
//! [`DispatchReason::AttributionBlocked`] ships with **no producer** at this
//! stage. That is deliberate, not an oversight: the vocabulary is a *wire
//! contract*, so a reader must already accept codes a future writer will emit.
//! It belongs with the `originator_user_id` attribution chain (multica 184/185),
//! which hangar has not started.
//!
//! [`DispatchReason::Coalesced`] and [`DispatchReason::SelfTriggerSuppressed`]
//! WERE reserved alongside it until the mention-routing layer (multica parity
//! #2-rest) shipped their producers:
//! `ainb_hangar_store::service::mention::route` emits `Coalesced` when a mention
//! folds into a pending task and `SelfTriggerSuppressed` when an agent's own
//! comment would have re-triggered itself. The
//! [`DispatchReason::RESERVED`] list pins the remaining set — a future change
//! that adds a producer has to remove its variant from that list deliberately.

use serde::{Deserialize, Serialize};

/// Why a trigger did — or did not — become a dispatched run.
///
/// Serializes to the `snake_case` tokens the reference's `dispatch.ReasonCode`
/// uses, which is also the exact token persisted to `dispatch_attempt.reason`.
///
/// The set is **append-only by design** and there is deliberately no `SQLite`
/// `CHECK` constraint on the column (`SQLite` cannot widen a `CHECK` without a
/// full table rebuild). The domain is enforced in Rust instead: [`Self::as_db_str`]
/// is the only writer, and [`Self::parse`] is the tolerant reader (an unknown
/// token decodes to `None` and renders as raw text rather than poisoning a read).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchReason {
    // --- dispatch happened, or will ---
    /// A task row was written; the claim loop will pick it up.
    Queued,
    /// Folded into an existing pending task rather than creating a second one.
    ///
    /// Produced by the mention router when the mentioned agent already holds a
    /// `queued`/`dispatched` task on the issue (multica's merge-into-pending).
    Coalesced,
    /// Admitted but parked: a later event promotes it. hangar's producer is a
    /// card blocked by unfinished dependencies, which `board::auto_run_dependent`
    /// fires once the last blocker finishes.
    Deferred,
    // --- declined ---
    /// The invoker may not run this target. Deliberately generic: it covers
    /// "the agent is private", "you are not on its allow-list", "the issue is
    /// cancelled" and "interactive mode is not supported for a squad" with one
    /// code, so it is never an existence oracle.
    InvocationNotAllowed,
    /// There is nothing to dispatch *to*: no agent in the workspace, no repo
    /// pinned on the card, or a provider that is not wired for dispatch.
    TargetUnavailable,
    /// The resolved agent's runtime is not `online`.
    ///
    /// In hangar this is **recorded, not refused** — the task row still exists
    /// (see the divergence note in the daemon recorder). It is nonetheless a
    /// *decline* for surfacing purposes: it is the answer to "why is my task
    /// sitting there doing nothing".
    RuntimeOffline,
    /// The trigger could not be attributed to an authorised actor.
    ///
    /// **Reserved** — no producer yet (see the module docs).
    AttributionBlocked,
    /// A run for this card is already active, so a second one was not started.
    AlreadyActive,
    /// The trigger was produced by the same agent it would have targeted, and
    /// was suppressed to stop a self-driving loop.
    ///
    /// Produced by the mention router when a comment's agent author mentions
    /// itself (multica `comment.go`'s `authorType == "agent" && authorID == m.ID`).
    SelfTriggerSuppressed,
    /// The admission path itself faulted (e.g. a database error).
    InternalError,
}

impl DispatchReason {
    /// Every variant, in declaration order. Kept exhaustive by
    /// [`Self::as_db_str`]'s wildcard-free `match` plus the
    /// `all_covers_every_variant` test.
    pub const ALL: [Self; 10] = [
        Self::Queued,
        Self::Coalesced,
        Self::Deferred,
        Self::InvocationNotAllowed,
        Self::TargetUnavailable,
        Self::RuntimeOffline,
        Self::AttributionBlocked,
        Self::AlreadyActive,
        Self::SelfTriggerSuppressed,
        Self::InternalError,
    ];

    /// Variants that ship in the vocabulary with **no producer** in this
    /// codebase (see the module docs). Pinned by a test so adding a producer is
    /// a deliberate edit here, and so the list can never quietly grow.
    pub const RESERVED: [Self; 1] = [Self::AttributionBlocked];

    /// The `snake_case` token persisted to `dispatch_attempt.reason` and put on
    /// the wire.
    ///
    /// Kept byte-identical to the [`Serialize`] `rename_all = "snake_case"`
    /// derive (guarded by `as_db_str_matches_serde_for_all_variants`), but
    /// written as a direct `match` so the persist path needs no serializer.
    /// No wildcard arm: a future variant breaks the build here first.
    #[must_use]
    pub const fn as_db_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Coalesced => "coalesced",
            Self::Deferred => "deferred",
            Self::InvocationNotAllowed => "invocation_not_allowed",
            Self::TargetUnavailable => "target_unavailable",
            Self::RuntimeOffline => "runtime_offline",
            Self::AttributionBlocked => "attribution_blocked",
            Self::AlreadyActive => "already_active",
            Self::SelfTriggerSuppressed => "self_trigger_suppressed",
            Self::InternalError => "internal_error",
        }
    }

    /// Wire/DB token → variant. Unknown tokens return `None` (the reader stays
    /// tolerant of a newer writer's vocabulary).
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.as_db_str() == s)
    }

    /// Did a task row actually get written (or folded into one)?
    ///
    /// Note [`Self::RuntimeOffline`] is deliberately **not** dispatched for this
    /// predicate even though hangar does write the row: the surfacing layers key
    /// "tell the user why nothing is happening" off `!is_dispatched()`, and an
    /// offline runtime is exactly that case.
    #[must_use]
    pub const fn is_dispatched(self) -> bool {
        matches!(self, Self::Queued | Self::Coalesced)
    }

    /// A short human phrase for the TUI detail card and the CLI.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Coalesced => "coalesced into a pending run",
            Self::Deferred => "waiting on blockers",
            Self::InvocationNotAllowed => "invocation not allowed",
            Self::TargetUnavailable => "no dispatch target",
            Self::RuntimeOffline => "runtime offline",
            Self::AttributionBlocked => "attribution blocked",
            Self::AlreadyActive => "a run is already active",
            Self::SelfTriggerSuppressed => "self-trigger suppressed",
            Self::InternalError => "internal error",
        }
    }
}

/// Which trigger surface made a dispatch attempt.
///
/// Mirrors `autopilot_run.source` from migration 0057 (`schedule|manual|webhook|api`),
/// widened to the card-dispatch surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchSource {
    /// A human pressed run (`hangar/issue_run`, `hangar/board_card_run`).
    Manual,
    /// An assignee was set, which re-dispatches the card.
    Assign,
    /// The board's dependency cascade fired it (blocker finished / child done).
    AutoRun,
    /// A cron / webhook autopilot fired it.
    Autopilot,
    /// Squad fan-out.
    Squad,
    /// A programmatic API caller.
    Api,
}

impl DispatchSource {
    /// Every variant, in declaration order.
    pub const ALL: [Self; 6] = [
        Self::Manual,
        Self::Assign,
        Self::AutoRun,
        Self::Autopilot,
        Self::Squad,
        Self::Api,
    ];

    /// The `snake_case` token persisted to `dispatch_attempt.source`.
    ///
    /// Byte-identical to the serde derive (guarded by a test); no wildcard arm.
    #[must_use]
    pub const fn as_db_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Assign => "assign",
            Self::AutoRun => "auto_run",
            Self::Autopilot => "autopilot",
            Self::Squad => "squad",
            Self::Api => "api",
        }
    }

    /// Wire/DB token → variant; unknown → `None`.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.as_db_str() == s)
    }
}

#[cfg(test)]
mod tests {
    use super::{DispatchReason, DispatchSource};

    fn serde_token(r: DispatchReason) -> String {
        serde_json::to_value(r).expect("serialize").as_str().expect("string").to_owned()
    }

    #[test]
    fn as_db_str_matches_serde_for_all_variants() {
        for r in DispatchReason::ALL {
            assert_eq!(serde_token(r), r.as_db_str(), "drift on {r:?}");
        }
        for s in DispatchSource::ALL {
            let tok =
                serde_json::to_value(s).expect("serialize").as_str().expect("string").to_owned();
            assert_eq!(tok, s.as_db_str(), "drift on {s:?}");
        }
    }

    #[test]
    fn parse_roundtrips_every_variant() {
        for r in DispatchReason::ALL {
            assert_eq!(DispatchReason::parse(r.as_db_str()), Some(r));
        }
        for s in DispatchSource::ALL {
            assert_eq!(DispatchSource::parse(s.as_db_str()), Some(s));
        }
        assert_eq!(DispatchReason::parse("nonsense_code"), None);
        assert_eq!(DispatchSource::parse("nonsense_source"), None);
    }

    #[test]
    fn all_covers_every_variant() {
        // A new variant that is not added to ALL makes this fail, because the
        // token set would be short of the match arms in `as_db_str`.
        let mut tokens: Vec<&str> = DispatchReason::ALL.iter().map(|r| r.as_db_str()).collect();
        tokens.sort_unstable();
        tokens.dedup();
        assert_eq!(tokens.len(), DispatchReason::ALL.len(), "duplicate token");
        assert_eq!(DispatchReason::ALL.len(), 10);
        assert_eq!(DispatchSource::ALL.len(), 6);
    }

    #[test]
    fn is_dispatched_only_for_queued_and_coalesced() {
        for r in DispatchReason::ALL {
            let expect = matches!(r, DispatchReason::Queued | DispatchReason::Coalesced);
            assert_eq!(r.is_dispatched(), expect, "on {r:?}");
        }
        // The acceptance case: an offline runtime DOES write a task row but is
        // still surfaced as "not dispatched".
        assert!(!DispatchReason::RuntimeOffline.is_dispatched());
    }

    #[test]
    fn every_variant_has_a_documented_producer_or_is_listed_reserved() {
        // Pins the reserved set: adding a producer means deliberately removing
        // the variant from `RESERVED` here.
        assert_eq!(
            DispatchReason::RESERVED,
            [DispatchReason::AttributionBlocked],
            "the mention router (parity #2-rest) ships the Coalesced and \
             SelfTriggerSuppressed producers, so only the attribution-chain \
             code stays reserved"
        );
        for r in DispatchReason::RESERVED {
            assert!(
                DispatchReason::ALL.contains(&r),
                "reserved variant {r:?} missing from ALL"
            );
        }
    }

    #[test]
    fn label_is_non_empty_and_distinct() {
        let mut labels: Vec<&str> = DispatchReason::ALL.iter().map(|r| r.label()).collect();
        assert!(labels.iter().all(|l| !l.is_empty()));
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), DispatchReason::ALL.len());
    }
}
