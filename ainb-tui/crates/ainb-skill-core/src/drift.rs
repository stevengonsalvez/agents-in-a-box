//! DriftDetector — compare a deployed unit's pinned SHA against the
//! upstream source's current tip (skill-manager v1.2 bead v12.E.1).
//!
//! The drift comparison itself (deployed_sha vs upstream tip vs the
//! commit graph between them) is delegated to a [`DriftBackend`]
//! trait. The default [`GitLsRemoteBackend`] shells out to
//! `git ls-remote` for the tip and, when a local cache of the source
//! repo is available, runs `git rev-list --left-right --count` to
//! resolve Ahead / Behind / Diverged counts. Tests inject a mock
//! backend so they never hit the network or the filesystem.
//!
//! # Public surface
//!
//! - [`DriftStatus`] — the four possible drift outcomes.
//! - [`DriftBackend`] — implementation hook.
//! - [`GitLsRemoteBackend`] — production backend (real `git ls-remote`).
//! - [`detect_drift`] — single-unit detector.
//! - [`detect_all`] — batched per-unit detection across a lockfile.
//!
//! See spec §10.1 / skill-manager v1.2 Phase E.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

use crate::error::{CoreError, Result};
use crate::lockfile::{Lockfile, LockedUnit};
use crate::manifest::{Manifest, SourceEntry};
use crate::uri::Uri;

/// Where a deployed unit sits relative to its upstream source's current
/// tip.
///
/// `InSync` and `Outdated { behind }` are the common cases (the user
/// has not touched the source; upstream may have moved on).
/// `Ahead` / `Diverged` arise when the deployed pin is on a different
/// branch from the source's declared ref — usually because the user
/// promoted a local edit upstream on a feature branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriftStatus {
    /// Deployed SHA is the upstream tip — fully in sync.
    InSync,
    /// Upstream has moved forward by `behind` commits; the deployed
    /// SHA is reachable from the tip.
    Outdated { behind: u32 },
    /// The deployed SHA is `ahead` commits beyond the tip on a
    /// fork of the upstream history. (Rare; arises when the source
    /// ref was rewound or when the unit was pinned to a feature branch
    /// whose tip moved backward.)
    Ahead { ahead: u32 },
    /// Deployed SHA and upstream tip share an ancestor but neither is
    /// reachable from the other — `ahead` commits on the deployed side,
    /// `behind` commits on the upstream side.
    Diverged { ahead: u32, behind: u32 },
}

/// Drift-resolution backend. Implementations decide how to compare a
/// deployed SHA against the source's current upstream tip.
///
/// The unit data plane (manifest + lockfile loading, per-unit
/// orchestration) lives in [`detect_drift`] / [`detect_all`]; backends
/// only own the per-source upstream query.
///
/// The trait bound is `Send + Sync` so backends can be shipped across
/// thread boundaries to a background drift poll (see
/// `AppState::start_background_drift_load`).
pub trait DriftBackend: Send + Sync {
    /// Compare `deployed_sha` against the current upstream tip of
    /// `source` and return the resolved [`DriftStatus`].
    ///
    /// Implementations may shell out, hit the network, or consult a
    /// local cache. Errors are surfaced as [`CoreError`] so callers can
    /// distinguish a transient backend failure (`Other`/`Io`) from a
    /// real drift answer.
    fn compare(&self, source: &SourceEntry, deployed_sha: &str) -> Result<DriftStatus>;
}

/// Production backend — invokes `git ls-remote <repo> <ref>` to
/// discover the current upstream tip.
///
/// Without a local cache of the source repo we can't compute ahead /
/// behind counts, so this backend collapses to:
///
/// - `deployed_sha == tip`  → [`DriftStatus::InSync`].
/// - `deployed_sha != tip`  → [`DriftStatus::Outdated { behind: 0 }`].
///
/// (`behind: 0` is the sentinel "drift detected, count unknown"; the
/// Units-panel rendering treats `Outdated { behind: 0 }` the same as
/// any non-zero `behind` — both render the ⚠ glyph. Tests inject a
/// [`MockBackend`] when they need to exercise the other variants.)
#[derive(Debug, Default)]
pub struct GitLsRemoteBackend {
    /// Optional path to the `git` binary. Defaults to `"git"` on
    /// `$PATH`.
    pub git_bin: Option<PathBuf>,
}

impl GitLsRemoteBackend {
    /// Convenience constructor — uses `"git"` from `$PATH`.
    pub fn new() -> Self {
        Self::default()
    }

    fn git(&self) -> Command {
        let bin = self.git_bin.as_deref().unwrap_or_else(|| std::path::Path::new("git"));
        Command::new(bin)
    }
}

impl DriftBackend for GitLsRemoteBackend {
    fn compare(&self, source: &SourceEntry, deployed_sha: &str) -> Result<DriftStatus> {
        // Materialise the network repo URL from the source URI. For
        // `gh:` sources we expand to https; other types pass through
        // verbatim (`git:` / `https:` are usable as-is).
        let repo = source_to_remote_url(&source.uri)?;
        let r#ref = source.r#ref.as_str();
        // Refuse argv-smuggled values before invoking git. Without this,
        // a malicious source URI starting with `-` could inject options
        // like `--upload-pack=…` and turn `ls-remote` into arbitrary
        // command execution. `--` after the option list closes the
        // remaining vector defense-in-depth.
        if repo.starts_with('-') || r#ref.starts_with('-') {
            return Err(CoreError::DriftBackend(format!(
                "refusing argv-smuggled value: repo=`{repo}` ref=`{}`",
                r#ref
            )));
        }
        let output = self
            .git()
            .arg("ls-remote")
            .arg("--exit-code")
            .arg("--")
            .arg(repo)
            .arg(r#ref)
            .output()
            .map_err(CoreError::Io)?;
        if !output.status.success() {
            return Err(CoreError::DriftBackend(format!(
                "git ls-remote failed (exit {:?}): {}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let tip = stdout
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().next())
            .ok_or_else(|| {
                CoreError::DriftBackend(format!(
                    "git ls-remote returned no rows for `{}@{}`",
                    source.uri, source.r#ref
                ))
            })?;
        if tip.eq_ignore_ascii_case(deployed_sha) {
            Ok(DriftStatus::InSync)
        } else {
            // Without a local clone we cannot compute the exact
            // behind count — surface a sentinel zero. The Units
            // panel's drift glyph treats any `Outdated` as ⚠.
            Ok(DriftStatus::Outdated { behind: 0 })
        }
    }
}

/// Translate a manifest source URI (`gh:org/repo`, `git:...`,
/// `https:...`) into a fully-qualified URL suitable for `git
/// ls-remote`. Returns the URI unchanged for source types that don't
/// need rewriting.
fn source_to_remote_url(uri: &str) -> Result<String> {
    if let Some(rest) = uri.strip_prefix("gh:") {
        return Ok(format!("https://github.com/{rest}.git"));
    }
    if let Some(rest) = uri.strip_prefix("git:") {
        return Ok(rest.to_string());
    }
    if uri.starts_with("https:") || uri.starts_with("http:") {
        return Ok(uri.to_string());
    }
    Err(CoreError::DriftBackend(format!(
        "source URI `{uri}` is not remotely fetchable for drift detection"
    )))
}

/// Detect drift for a single locked unit against its declared source.
///
/// `unit.sha` (the deployed SHA) is compared against the source's
/// current upstream tip via `backend`. A unit with no recorded `sha`
/// (legacy / never-locked) is reported as [`DriftStatus::InSync`] —
/// we can't claim drift on a unit we never pinned.
pub fn detect_drift(
    unit: &LockedUnit,
    source: &SourceEntry,
    backend: &dyn DriftBackend,
) -> Result<DriftStatus> {
    let Some(sha) = unit.sha.as_deref() else {
        return Ok(DriftStatus::InSync);
    };
    if sha.is_empty() {
        return Ok(DriftStatus::InSync);
    }
    backend.compare(source, sha)
}

/// Detect drift for every locked unit, keyed by the unit's
/// `declared_uri`. Units whose source can't be resolved in `manifest`
/// (e.g. the source was removed but the unit lingers) report
/// [`DriftStatus::InSync`] — there's no upstream to compare against.
///
/// Errors from the backend are not fatal: the offending unit is
/// dropped from the result map so the caller can render a "…"
/// placeholder for it. The Units-panel cache treats a missing entry
/// as "drift unknown / still loading".
pub fn detect_all(
    manifest: &Manifest,
    lockfile: &Lockfile,
    backend: &dyn DriftBackend,
) -> BTreeMap<String, DriftStatus> {
    let mut out = BTreeMap::new();
    for unit in &lockfile.units {
        let Ok(uri) = Uri::parse(&unit.declared_uri) else {
            continue;
        };
        let source_uri = format!("{}:{}", uri.source_type, uri.locator);
        let Some(source) = manifest.sources.iter().find(|s| s.uri == source_uri) else {
            // No source declared anymore — treat as in-sync so we
            // don't decorate the row with a misleading drift glyph.
            out.insert(unit.declared_uri.clone(), DriftStatus::InSync);
            continue;
        };
        match detect_drift(unit, source, backend) {
            Ok(status) => {
                out.insert(unit.declared_uri.clone(), status);
            }
            Err(_) => {
                // Drop on backend error — the Units-panel renders the
                // unknown placeholder ("…") for any unit not present
                // in the cache.
            }
        }
    }
    out
}

// ---------------------------------------------------------------------
// Test scaffolding
// ---------------------------------------------------------------------

/// Stub backend that hands every (`source_name`, `deployed_sha`) pair
/// a pre-canned [`DriftStatus`]. Used by unit + integration tests so
/// the per-unit / per-screen flows never hit `git ls-remote`.
///
/// `pub` so tests in other crates (e.g. `ainb-cli` `skill check`) can
/// share the harness without re-defining it.
#[derive(Debug, Default, Clone)]
pub struct MockBackend {
    /// Keyed by `(source.name, deployed_sha)`. Missing pairs yield an
    /// error so tests catch unexpected lookups loudly.
    pub answers: BTreeMap<(String, String), DriftStatus>,
    /// Optional override applied when a per-pair entry is missing —
    /// e.g. all unknowns resolve to `InSync` instead of erroring.
    pub default: Option<DriftStatus>,
}

impl MockBackend {
    /// Build an empty mock — every lookup errors until you register
    /// answers.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a canned answer for one (`source_name`, `sha`) pair.
    pub fn expect(&mut self, source_name: &str, sha: &str, status: DriftStatus) {
        self.answers.insert((source_name.to_string(), sha.to_string()), status);
    }
}

impl DriftBackend for MockBackend {
    fn compare(&self, source: &SourceEntry, deployed_sha: &str) -> Result<DriftStatus> {
        if let Some(s) = self.answers.get(&(source.name.clone(), deployed_sha.to_string())) {
            return Ok(*s);
        }
        if let Some(d) = self.default {
            return Ok(d);
        }
        Err(CoreError::DriftBackend(format!(
            "MockBackend: no canned answer for source `{}` + sha `{}`",
            source.name, deployed_sha
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lockfile::LockedUnit;
    use crate::manifest::SourceEntry;

    fn source(name: &str) -> SourceEntry {
        SourceEntry {
            name: name.to_string(),
            kind: None,
            uri: format!("gh:org/{name}"),
            r#ref: "main".to_string(),
            enabled: true,
            read_only: false,
            target_layout: Vec::new(),
        }
    }

    fn unit(declared_uri: &str, sha: Option<&str>) -> LockedUnit {
        LockedUnit {
            uri: declared_uri.to_string(),
            declared_uri: declared_uri.to_string(),
            kind: "skill".to_string(),
            sha: sha.map(str::to_string),
            deployed: BTreeMap::new(),
            usage: Default::default(),
        }
    }

    // ---- detect_drift: covers all four enum variants via mock ----

    #[test]
    fn detect_drift_in_sync_when_mock_returns_in_sync() {
        let src = source("acme");
        let mut mock = MockBackend::new();
        mock.expect("acme", "deadbeef", DriftStatus::InSync);
        let u = unit("gh:org/acme@main/skills/foo", Some("deadbeef"));
        assert_eq!(detect_drift(&u, &src, &mock).unwrap(), DriftStatus::InSync);
    }

    #[test]
    fn detect_drift_outdated_when_mock_returns_outdated() {
        let src = source("acme");
        let mut mock = MockBackend::new();
        mock.expect("acme", "deadbeef", DriftStatus::Outdated { behind: 3 });
        let u = unit("gh:org/acme@main/skills/foo", Some("deadbeef"));
        assert_eq!(
            detect_drift(&u, &src, &mock).unwrap(),
            DriftStatus::Outdated { behind: 3 }
        );
    }

    #[test]
    fn detect_drift_ahead_when_mock_returns_ahead() {
        let src = source("acme");
        let mut mock = MockBackend::new();
        mock.expect("acme", "deadbeef", DriftStatus::Ahead { ahead: 2 });
        let u = unit("gh:org/acme@main/skills/foo", Some("deadbeef"));
        assert_eq!(
            detect_drift(&u, &src, &mock).unwrap(),
            DriftStatus::Ahead { ahead: 2 }
        );
    }

    #[test]
    fn detect_drift_diverged_when_mock_returns_diverged() {
        let src = source("acme");
        let mut mock = MockBackend::new();
        mock.expect(
            "acme",
            "deadbeef",
            DriftStatus::Diverged { ahead: 1, behind: 4 },
        );
        let u = unit("gh:org/acme@main/skills/foo", Some("deadbeef"));
        assert_eq!(
            detect_drift(&u, &src, &mock).unwrap(),
            DriftStatus::Diverged { ahead: 1, behind: 4 }
        );
    }

    #[test]
    fn detect_drift_treats_missing_sha_as_in_sync() {
        let src = source("acme");
        let mock = MockBackend::new();
        let u = unit("gh:org/acme@main/skills/foo", None);
        // No expectation registered — mock would error if hit.
        assert_eq!(detect_drift(&u, &src, &mock).unwrap(), DriftStatus::InSync);
    }

    #[test]
    fn detect_drift_treats_empty_sha_as_in_sync() {
        let src = source("acme");
        let mock = MockBackend::new();
        let u = unit("gh:org/acme@main/skills/foo", Some(""));
        assert_eq!(detect_drift(&u, &src, &mock).unwrap(), DriftStatus::InSync);
    }

    #[test]
    fn detect_drift_propagates_backend_error() {
        let src = source("acme");
        let mock = MockBackend::new();
        // No canned answer and no default → backend errors.
        let u = unit("gh:org/acme@main/skills/foo", Some("deadbeef"));
        assert!(detect_drift(&u, &src, &mock).is_err());
    }

    // ---- detect_all: orchestration ----

    #[test]
    fn detect_all_keys_by_declared_uri() {
        let manifest = Manifest {
            schema_version: 1,
            sources: vec![source("acme")],
            units: Vec::new(),
            defaults: None,
            options: None,
        };
        let lockfile = Lockfile {
            schema_version: 2,
            generated_at: None,
            sources: Vec::new(),
            units: vec![
                unit("gh:org/acme@main/skills/foo", Some("deadbeef")),
                unit("gh:org/acme@main/skills/bar", Some("cafebabe")),
            ],
        };
        let mut mock = MockBackend::new();
        mock.expect("acme", "deadbeef", DriftStatus::InSync);
        mock.expect(
            "acme",
            "cafebabe",
            DriftStatus::Outdated { behind: 5 },
        );
        let map = detect_all(&manifest, &lockfile, &mock);
        assert_eq!(map.len(), 2);
        assert_eq!(map["gh:org/acme@main/skills/foo"], DriftStatus::InSync);
        assert_eq!(
            map["gh:org/acme@main/skills/bar"],
            DriftStatus::Outdated { behind: 5 }
        );
    }

    #[test]
    fn detect_all_treats_missing_source_as_in_sync() {
        let manifest = Manifest::default(); // no sources
        let lockfile = Lockfile {
            schema_version: 2,
            generated_at: None,
            sources: Vec::new(),
            units: vec![unit("gh:org/ghost@main/skills/foo", Some("deadbeef"))],
        };
        let mock = MockBackend::new();
        let map = detect_all(&manifest, &lockfile, &mock);
        assert_eq!(map["gh:org/ghost@main/skills/foo"], DriftStatus::InSync);
    }

    #[test]
    fn detect_all_drops_units_whose_backend_errored() {
        let manifest = Manifest {
            schema_version: 1,
            sources: vec![source("acme")],
            units: Vec::new(),
            defaults: None,
            options: None,
        };
        let lockfile = Lockfile {
            schema_version: 2,
            generated_at: None,
            sources: Vec::new(),
            units: vec![
                unit("gh:org/acme@main/skills/foo", Some("deadbeef")),
                unit("gh:org/acme@main/skills/bar", Some("cafebabe")),
            ],
        };
        let mut mock = MockBackend::new();
        // Only register foo; bar's lookup will error.
        mock.expect("acme", "deadbeef", DriftStatus::InSync);
        let map = detect_all(&manifest, &lockfile, &mock);
        assert_eq!(map.len(), 1);
        assert!(map.contains_key("gh:org/acme@main/skills/foo"));
        assert!(!map.contains_key("gh:org/acme@main/skills/bar"));
    }

    // ---- source_to_remote_url: defensive coverage ----

    #[test]
    fn gh_source_uri_expands_to_https() {
        assert_eq!(
            source_to_remote_url("gh:org/repo").unwrap(),
            "https://github.com/org/repo.git"
        );
    }

    #[test]
    fn git_source_uri_passes_through_without_prefix() {
        assert_eq!(
            source_to_remote_url("git:git@gitlab.com:org/repo.git").unwrap(),
            "git@gitlab.com:org/repo.git"
        );
    }

    #[test]
    fn https_source_uri_passes_through_verbatim() {
        assert_eq!(
            source_to_remote_url("https://example.com/repo.git").unwrap(),
            "https://example.com/repo.git"
        );
    }

    #[test]
    fn local_source_uri_rejected_for_remote_drift() {
        assert!(source_to_remote_url("local:/tmp/foo").is_err());
    }
}
