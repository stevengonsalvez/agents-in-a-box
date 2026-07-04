//! `gh`-backed PR status fetch behind an injectable seam (e38.34).
//!
//! The task-detail PR badge surfaces more than a URL: the CI check rollup, the
//! mergeability, and the merge state. This module fetches those three axes for a
//! captured PR URL by shelling out to `gh pr view <url> --json
//! statusCheckRollup,mergeable,state`, exactly as the `bd` adapter shells out to
//! `bd` — no GitHub App, no webhook, the v1 `gh`-CLI-only integration.
//!
//! # The seam
//!
//! Every fetch goes through the [`PrStatusProvider`] trait so the auto-Done
//! refresh path ([`crate::rpc::snapshots::refresh_pr_status`]) can be driven by a
//! test [`FakePrStatusProvider`] that returns a canned status — a unit/integration
//! test **never** spawns `gh` or touches the network. The real
//! [`GhPrStatusProvider`] is the only impl that runs a subprocess.
//!
//! # Graceful degrade
//!
//! `gh` may be absent, unauthenticated, or the URL may have no checks. Every such
//! case folds to [`PrStatus::default`] (all-`Unknown`) — the fetch **never** errors
//! or panics, so the badge renders a muted placeholder rather than a false state
//! and the auto-Done transition simply does not fire.

use std::future::Future;
use std::pin::Pin;
use std::process::Stdio;

use ainb_hangar_proto::pr_status::{CiRollup, MergeState, Mergeable, PrStatus};

/// An injectable source of [`PrStatus`] for a PR URL.
///
/// Dyn-compatible (the method returns a boxed future) so the refresh path takes a
/// `&dyn PrStatusProvider` and a test can substitute a [`FakePrStatusProvider`]
/// without a subprocess. A fetch never fails — an unreachable / absent `gh` is an
/// all-`Unknown` [`PrStatus`], not an error.
pub trait PrStatusProvider: Send + Sync {
    /// Fetch the CI + merge status for `pr_url`, degrading to
    /// [`PrStatus::default`] (all-`Unknown`) on any failure.
    fn fetch<'a>(&'a self, pr_url: &'a str) -> Pin<Box<dyn Future<Output = PrStatus> + Send + 'a>>;
}

/// The production provider: shells `gh pr view <url> --json
/// statusCheckRollup,mergeable,state` and folds the JSON into a [`PrStatus`].
///
/// The `gh` binary path defaults to `"gh"` (resolved on `$PATH`); override it for
/// a pinned install. A non-zero exit, a spawn failure (no `gh` on `$PATH`), or
/// unparseable output all degrade to the all-`Unknown` status.
#[derive(Debug, Clone)]
pub struct GhPrStatusProvider {
    /// The `gh` binary to invoke (default `"gh"`).
    bin: String,
}

impl Default for GhPrStatusProvider {
    fn default() -> Self {
        Self {
            bin: "gh".to_string(),
        }
    }
}

/// Env var overriding the `gh` binary the production provider invokes.
///
/// Unset → `gh` on `$PATH` (production). A test / e2e harness points it at a stub
/// script that prints canned `gh pr view` JSON, so the whole PR-status path can be
/// exercised end-to-end without touching real GitHub — mirroring
/// `HANGAR_CLAUDE_PATH` / `HANGAR_CODEX_PATH` for the provider binaries.
pub const GH_PATH_ENV: &str = "HANGAR_GH_PATH";

impl GhPrStatusProvider {
    /// A provider that invokes the `gh` binary resolved on `$PATH`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A provider honouring the [`GH_PATH_ENV`] override: the pinned stub `gh`
    /// when the env var is set (tests / e2e), else `gh` on `$PATH` (production).
    #[must_use]
    pub fn from_env() -> Self {
        std::env::var(GH_PATH_ENV)
            .ok()
            .filter(|s| !s.is_empty())
            .map_or_else(Self::new, Self::with_bin)
    }

    /// A provider pinned to a specific `gh` binary path (used by tests that point
    /// at a stub script; production uses [`Self::new`]).
    #[must_use]
    pub fn with_bin(bin: impl Into<String>) -> Self {
        Self { bin: bin.into() }
    }
}

impl PrStatusProvider for GhPrStatusProvider {
    fn fetch<'a>(&'a self, pr_url: &'a str) -> Pin<Box<dyn Future<Output = PrStatus> + Send + 'a>> {
        Box::pin(async move {
            // A bare URL is never passed to `gh` — degrade immediately so an
            // empty / malformed input can't spin up a doomed subprocess.
            if pr_url.trim().is_empty() {
                return PrStatus::default();
            }
            let output = tokio::process::Command::new(&self.bin)
                .arg("pr")
                .arg("view")
                .arg(pr_url)
                .arg("--json")
                .arg("statusCheckRollup,mergeable,state")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .output()
                .await;
            match output {
                Ok(out) if out.status.success() => {
                    parse_gh_pr_view(&String::from_utf8_lossy(&out.stdout))
                }
                // A non-zero exit (no auth, no such PR) or a spawn failure (no
                // `gh`) both degrade — never a panic, never a false state.
                _ => PrStatus::default(),
            }
        })
    }
}

/// Parse `gh pr view --json statusCheckRollup,mergeable,state` output into a
/// [`PrStatus`], folding the rich `gh` JSON into the three badge axes.
///
/// Pure + total: any missing / unexpected field degrades that axis to `Unknown`,
/// so a `gh` version that adds or drops a field never panics. Unparseable JSON
/// yields the all-`Unknown` default.
///
/// # Field mapping (verified against `gh 2.x` output)
///
/// - `state`: `"MERGED"` → [`MergeState::Merged`], `"OPEN"` → `Open`, `"CLOSED"`
///   → `Closed`, anything else → `Unknown`.
/// - `mergeable`: `"MERGEABLE"` → [`Mergeable::Mergeable`], `"CONFLICTING"` →
///   `Conflicting`, anything else (incl. `"UNKNOWN"`) → `Unknown`.
/// - `statusCheckRollup`: an array of `CheckRun` (`conclusion`/`status`) and
///   `StatusContext` (`state`) entries, folded by [`fold_rollup`]: any failure →
///   [`CiRollup::Fail`], else any still-running → `Pending`, else (all green,
///   non-empty) → `Pass`, empty / absent → `Unknown`.
#[must_use]
pub fn parse_gh_pr_view(json: &str) -> PrStatus {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return PrStatus::default();
    };
    let state = match v.get("state").and_then(|s| s.as_str()) {
        Some("MERGED") => MergeState::Merged,
        Some("OPEN") => MergeState::Open,
        Some("CLOSED") => MergeState::Closed,
        _ => MergeState::Unknown,
    };
    let mergeable = match v.get("mergeable").and_then(|m| m.as_str()) {
        Some("MERGEABLE") => Mergeable::Mergeable,
        Some("CONFLICTING") => Mergeable::Conflicting,
        _ => Mergeable::Unknown,
    };
    let ci = v
        .get("statusCheckRollup")
        .and_then(|r| r.as_array())
        .map_or(CiRollup::Unknown, |rollup| fold_rollup(rollup));
    PrStatus {
        ci,
        mergeable,
        state,
    }
}

/// Fold a `gh` `statusCheckRollup` array into a single [`CiRollup`].
///
/// Each entry is either a `CheckRun` (carrying `status` + `conclusion`) or a
/// `StatusContext` (carrying `state`). The fold is failure-dominant: one failing
/// check makes the whole rollup [`CiRollup::Fail`]; absent a failure, one
/// still-running check makes it `Pending`; only an all-concluded-green, non-empty
/// rollup is `Pass`. An empty array is `Unknown` (no checks configured).
fn fold_rollup(rollup: &[serde_json::Value]) -> CiRollup {
    if rollup.is_empty() {
        return CiRollup::Unknown;
    }
    let mut any_pending = false;
    for entry in rollup {
        match check_outcome(entry) {
            CheckOutcome::Fail => return CiRollup::Fail,
            CheckOutcome::Pending => any_pending = true,
            CheckOutcome::Pass => {}
        }
    }
    if any_pending {
        CiRollup::Pending
    } else {
        CiRollup::Pass
    }
}

/// The pass/fail/pending outcome of one rollup entry.
enum CheckOutcome {
    Pass,
    Fail,
    Pending,
}

/// Classify one rollup entry (`CheckRun` or `StatusContext`) into a [`CheckOutcome`].
///
/// A `CheckRun` is pending until its `status` is `"COMPLETED"`, then pass/fail by
/// `conclusion` (`"SUCCESS"`/`"NEUTRAL"`/`"SKIPPED"` pass; everything else fails).
/// A `StatusContext` classifies by `state` directly (`"SUCCESS"` passes,
/// `"PENDING"`/`"EXPECTED"` pend, anything else fails). An entry that carries
/// neither shape counts as pending (in-flight), never a silent pass.
fn check_outcome(entry: &serde_json::Value) -> CheckOutcome {
    // CheckRun: gate on completion, then conclusion.
    if let Some(status) = entry.get("status").and_then(|s| s.as_str()) {
        if !status.eq_ignore_ascii_case("COMPLETED") {
            return CheckOutcome::Pending;
        }
        return match entry.get("conclusion").and_then(|c| c.as_str()) {
            Some(c)
                if c.eq_ignore_ascii_case("SUCCESS")
                    || c.eq_ignore_ascii_case("NEUTRAL")
                    || c.eq_ignore_ascii_case("SKIPPED") =>
            {
                CheckOutcome::Pass
            }
            // FAILURE / CANCELLED / TIMED_OUT / ACTION_REQUIRED / STARTUP_FAILURE /
            // a missing conclusion on a completed run: all fail.
            _ => CheckOutcome::Fail,
        };
    }
    // StatusContext: classify by state.
    match entry.get("state").and_then(|s| s.as_str()) {
        Some(s) if s.eq_ignore_ascii_case("SUCCESS") => CheckOutcome::Pass,
        Some(s) if s.eq_ignore_ascii_case("PENDING") || s.eq_ignore_ascii_case("EXPECTED") => {
            CheckOutcome::Pending
        }
        Some(_) => CheckOutcome::Fail,
        // Neither a CheckRun nor a recognised StatusContext: treat as in-flight.
        None => CheckOutcome::Pending,
    }
}

/// A test double returning a canned [`PrStatus`] for any URL — the seam that lets
/// the auto-Done refresh path be driven without spawning `gh`.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Clone)]
pub struct FakePrStatusProvider {
    /// The status every [`PrStatusProvider::fetch`] call returns.
    status: PrStatus,
}

#[cfg(any(test, feature = "test-support"))]
impl FakePrStatusProvider {
    /// A fake that always answers `status`.
    #[must_use]
    pub const fn new(status: PrStatus) -> Self {
        Self { status }
    }
}

#[cfg(any(test, feature = "test-support"))]
impl PrStatusProvider for FakePrStatusProvider {
    fn fetch<'a>(
        &'a self,
        _pr_url: &'a str,
    ) -> Pin<Box<dyn Future<Output = PrStatus> + Send + 'a>> {
        let status = self.status;
        Box::pin(async move { status })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real `gh pr view` output shape (verified live) folds to all-green +
    /// open + unknown-mergeable: every check `SUCCESS`, `state: OPEN`,
    /// `mergeable: UNKNOWN`.
    #[test]
    fn parses_real_gh_open_all_green() {
        let json = r#"{"mergeable":"UNKNOWN","state":"OPEN","statusCheckRollup":[
            {"__typename":"CheckRun","conclusion":"SUCCESS","name":"Rustfmt","status":"COMPLETED"},
            {"__typename":"CheckRun","conclusion":"SUCCESS","name":"Test","status":"COMPLETED"},
            {"__typename":"StatusContext","context":"CodeRabbit","state":"SUCCESS"}
        ]}"#;
        let s = parse_gh_pr_view(json);
        assert_eq!(s.ci, CiRollup::Pass);
        assert_eq!(s.mergeable, Mergeable::Unknown);
        assert_eq!(s.state, MergeState::Open);
    }

    /// A merged PR with a conflict-free merge folds to merged + mergeable.
    #[test]
    fn parses_merged_mergeable() {
        let json = r#"{"mergeable":"MERGEABLE","state":"MERGED","statusCheckRollup":[
                {"__typename":"CheckRun","conclusion":"SUCCESS","status":"COMPLETED"}]}"#;
        let s = parse_gh_pr_view(json);
        assert_eq!(s.state, MergeState::Merged);
        assert!(s.is_merged());
        assert_eq!(s.mergeable, Mergeable::Mergeable);
        assert_eq!(s.ci, CiRollup::Pass);
    }

    /// A failing check makes the whole rollup `Fail`, even mixed with greens, and
    /// a conflicting mergeable is surfaced.
    #[test]
    fn one_failure_makes_rollup_fail_and_conflict_surfaced() {
        let json = r#"{"mergeable":"CONFLICTING","state":"OPEN","statusCheckRollup":[
            {"__typename":"CheckRun","conclusion":"SUCCESS","status":"COMPLETED"},
            {"__typename":"CheckRun","conclusion":"FAILURE","status":"COMPLETED"}
        ]}"#;
        let s = parse_gh_pr_view(json);
        assert_eq!(s.ci, CiRollup::Fail);
        assert_eq!(s.mergeable, Mergeable::Conflicting);
        assert_eq!(s.state, MergeState::Open);
    }

    /// A still-running check (no failure) makes the rollup `Pending`.
    #[test]
    fn in_progress_check_is_pending() {
        let json = r#"{"mergeable":"MERGEABLE","state":"OPEN","statusCheckRollup":[
            {"__typename":"CheckRun","conclusion":"SUCCESS","status":"COMPLETED"},
            {"__typename":"CheckRun","conclusion":null,"status":"IN_PROGRESS"}
        ]}"#;
        let s = parse_gh_pr_view(json);
        assert_eq!(s.ci, CiRollup::Pending);
    }

    /// An empty rollup (no checks configured) is `Unknown`, not a false `Pass`.
    #[test]
    fn empty_rollup_is_unknown() {
        let json = r#"{"mergeable":"MERGEABLE","state":"OPEN","statusCheckRollup":[]}"#;
        assert_eq!(parse_gh_pr_view(json).ci, CiRollup::Unknown);
    }

    /// Unparseable output degrades to the all-`Unknown` default (never a panic).
    #[test]
    fn garbage_degrades_to_default() {
        assert_eq!(parse_gh_pr_view("not json at all"), PrStatus::default());
        assert_eq!(parse_gh_pr_view(""), PrStatus::default());
    }

    /// Missing fields degrade per-axis: a `gh` that dropped `statusCheckRollup`
    /// still yields a valid state + mergeable with `ci: Unknown`.
    #[test]
    fn missing_rollup_field_degrades_only_that_axis() {
        let json = r#"{"mergeable":"CONFLICTING","state":"CLOSED"}"#;
        let s = parse_gh_pr_view(json);
        assert_eq!(s.ci, CiRollup::Unknown);
        assert_eq!(s.mergeable, Mergeable::Conflicting);
        assert_eq!(s.state, MergeState::Closed);
    }

    /// The fake provider returns its canned status for any URL, without `gh`.
    #[tokio::test]
    async fn fake_provider_returns_canned_status() {
        let want = PrStatus {
            ci: CiRollup::Fail,
            mergeable: Mergeable::Conflicting,
            state: MergeState::Merged,
        };
        let provider = FakePrStatusProvider::new(want);
        let got = provider.fetch("https://example.com/pr/1").await;
        assert_eq!(got, want);
    }

    /// The real provider degrades to all-`Unknown` when the `gh` binary is absent
    /// (a non-existent bin name) — it shells out but never panics.
    #[tokio::test]
    async fn real_provider_degrades_when_gh_absent() {
        let provider = GhPrStatusProvider::with_bin("gh-definitely-not-installed-xyz");
        let got = provider.fetch("https://github.com/o/r/pull/1").await;
        assert_eq!(got, PrStatus::default());
    }

    /// A blank URL degrades immediately without spawning a subprocess.
    #[tokio::test]
    async fn real_provider_degrades_on_blank_url() {
        let provider = GhPrStatusProvider::new();
        assert_eq!(provider.fetch("   ").await, PrStatus::default());
    }
}
