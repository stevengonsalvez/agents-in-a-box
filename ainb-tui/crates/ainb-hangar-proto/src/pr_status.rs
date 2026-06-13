//! PR check + merge status wire types (e38.34).
//!
//! The task-detail PR badge surfaces more than a bare URL: it shows the CI check
//! rollup (pass / fail / pending), whether the PR is mergeable or has a conflict,
//! and the merge state (open / merged / closed). These are produced daemon-side by
//! a `gh`-backed fetch behind an injectable seam and carried to the plugin only on
//! the refresh result — they are **not** widened into [`crate::events::IssueRow`],
//! so the wide snapshot shape is unchanged and every `IssueRow` construction site
//! stays as-is.
//!
//! Every variant degrades to a safe "unknown" so an absent / unauthenticated `gh`
//! never panics the daemon nor blanks the badge — it renders a muted `…` instead.
//! These are **pure wire types** (`serde` only, no host deps) like the rest of
//! `ainb-hangar-proto`.

use serde::{Deserialize, Serialize};

/// The aggregate CI check-run rollup for a PR (the `statusCheckRollup` summary).
///
/// Mirrors `gh`'s rollup states folded to three buckets the badge renders: every
/// required check green (`Pass`), at least one failing/errored (`Fail`), or still
/// running/queued with none failed (`Pending`). `Unknown` is the degrade case —
/// `gh` absent, unauthenticated, or a PR with no checks configured — and renders a
/// muted placeholder, never a false green.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CiRollup {
    /// Every check concluded successfully.
    Pass,
    /// At least one check failed, errored, or was cancelled.
    Fail,
    /// Checks are still running / queued; none have failed yet.
    Pending,
    /// No checks, or the status could not be determined (the degrade default).
    #[default]
    Unknown,
}

/// Whether a PR can be merged cleanly (`gh`'s `mergeable` field).
///
/// `Mergeable` is a clean merge, `Conflicting` is a merge conflict the badge flags
/// distinctly, and `Unknown` is the in-flight / undetermined case GitHub reports
/// while it computes mergeability (also the degrade default when `gh` is absent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Mergeable {
    /// The branch merges without conflicts.
    Mergeable,
    /// The branch has a merge conflict.
    Conflicting,
    /// GitHub has not finished computing mergeability (the degrade default).
    #[default]
    Unknown,
}

/// The PR's lifecycle state (`gh`'s `state` field).
///
/// Only [`MergeState::Merged`] drives the auto-transition-to-Done: a `Merged` PR
/// moves its backing issue to `done`; an `Open` or `Closed` (un-merged) PR leaves
/// the issue where it is. `Unknown` is the degrade default and never transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MergeState {
    /// The PR is open (not yet merged or closed).
    Open,
    /// The PR was merged — the only state that auto-moves the issue to Done.
    Merged,
    /// The PR was closed without merging.
    Closed,
    /// The state could not be determined (the degrade default).
    #[default]
    Unknown,
}

/// A fetched PR status: the CI rollup, mergeability, and merge state for one PR.
///
/// `Default` is the all-`Unknown` degrade value the daemon answers when no PR URL
/// is bound or `gh` could not be reached — the badge then renders a muted `…`
/// rather than blanking or claiming a false state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PrStatus {
    /// The aggregate CI check rollup.
    #[serde(default)]
    pub ci: CiRollup,
    /// Whether the PR merges cleanly.
    #[serde(default)]
    pub mergeable: Mergeable,
    /// The PR's lifecycle state (drives the auto-Done transition).
    #[serde(default)]
    pub state: MergeState,
}

impl PrStatus {
    /// `true` when the PR is merged — the trigger for the auto-transition to Done.
    #[must_use]
    pub const fn is_merged(&self) -> bool {
        matches!(self.state, MergeState::Merged)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A populated status round-trips through JSON, preserving all three axes.
    #[test]
    fn pr_status_roundtrips() {
        let status = PrStatus {
            ci: CiRollup::Fail,
            mergeable: Mergeable::Conflicting,
            state: MergeState::Open,
        };
        let s = serde_json::to_string(&status).unwrap();
        assert_eq!(serde_json::from_str::<PrStatus>(&s).unwrap(), status);
    }

    /// The default is the all-`Unknown` degrade value, and it is not "merged".
    #[test]
    fn pr_status_default_is_all_unknown_and_not_merged() {
        let d = PrStatus::default();
        assert_eq!(d.ci, CiRollup::Unknown);
        assert_eq!(d.mergeable, Mergeable::Unknown);
        assert_eq!(d.state, MergeState::Unknown);
        assert!(
            !d.is_merged(),
            "an unknown-state PR must not count as merged"
        );
    }

    /// Only `MergeState::Merged` reports merged.
    #[test]
    fn is_merged_only_for_merged_state() {
        for (state, want) in [
            (MergeState::Open, false),
            (MergeState::Merged, true),
            (MergeState::Closed, false),
            (MergeState::Unknown, false),
        ] {
            let s = PrStatus {
                state,
                ..Default::default()
            };
            assert_eq!(s.is_merged(), want, "state {state:?}");
        }
    }

    /// A pre-e38.34 reader (an empty object) decodes to the all-`Unknown` degrade
    /// value — every axis is `#[serde(default)]`, so the wire stays back-compatible.
    #[test]
    fn empty_object_decodes_to_default() {
        let back: PrStatus = serde_json::from_str("{}").unwrap();
        assert_eq!(back, PrStatus::default());
    }

    /// The enum tokens serialise in `snake_case` so the wire is stable + readable.
    #[test]
    fn enum_tokens_are_snake_case() {
        assert_eq!(serde_json::to_string(&CiRollup::Pass).unwrap(), "\"pass\"");
        assert_eq!(
            serde_json::to_string(&Mergeable::Conflicting).unwrap(),
            "\"conflicting\""
        );
        assert_eq!(
            serde_json::to_string(&MergeState::Merged).unwrap(),
            "\"merged\""
        );
    }
}
