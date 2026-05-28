//! Detect the external `witr` binary on the user's PATH and gate it on
//! the minimum supported version.
//!
//! The plugin shells out to `witr --json <target>` (cfx.3) under the
//! `spawn_subprocess` capability. Before that's safe, we need three
//! facts:
//!
//! 1. The binary exists somewhere on `PATH`.
//! 2. It's actually the witr we expect (and not some unrelated `witr`).
//! 3. Its version meets [`MIN_VERSION`] so the JSON contract we'll
//!    parse in [`crate::model`](super) is the one we tested against.
//!
//! Surface area:
//!
//! - [`detect_witr`] — async one-shot that does the `which`-+-exec
//!   handshake and returns a [`DetectResult`].
//! - [`parse_version_output`] — pure parser exposed for unit tests so
//!   the regex contract stays in one place. Same parser the async path
//!   calls.
//!
//! The version line shape comes from witr's release template at
//! `internal/app/app.go`:
//!
//! ```text
//! witr v0.3.2 (commit a1b2c3d, built 2025-04-10T12:00:00Z)
//! ```
//!
//! `go install` / Debian builds emit `commit = unknown` and/or
//! `built = unknown` — we accept those and surface them as `None`
//! rather than gating on them. See
//! `research/2026-05-19_12-13-09_witr-spec-open-questions.md` (Q2).

use std::path::PathBuf;
use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;
use semver::Version;
use tokio::process::Command;
use tokio::time::timeout;

/// Minimum witr version the plugin requires. Pinned to the current
/// release at the time of v1 spec lock — see
/// `plans/witr-plugin-spec.md` § Decisions Made.
pub static MIN_VERSION: LazyLock<Version> =
    LazyLock::new(|| Version::parse("0.3.2").expect("hardcoded minimum version is valid"));

/// Timeout for the `witr --version` exec at startup. Generous enough
/// for a cold filesystem cache; not so long that a misbehaving binary
/// can stall ainb.
const VERSION_EXEC_TIMEOUT: Duration = Duration::from_secs(5);

/// The version-line regex, compiled once. Matches witr's release
/// goreleaser template and the `go install` / Debian fallback (where
/// `commit` and/or `built` may be the literal string `unknown`).
///
/// The `ver` capture is constrained to the semver alphabet
/// (`A-Za-z0-9.+-`) so a stray glob like `witr v0.3.2abc!!!` is
/// rejected at the regex stage rather than slipping through to
/// `Version::parse` and being misclassified as a non-witr binary.
static VERSION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^witr v(?P<ver>\d+\.\d+\.\d+[A-Za-z0-9.+\-]*)\s+\(commit\s+(?P<commit>[0-9a-f]{7,40}|unknown),\s+built\s+(?P<built>[^\)]+)\)\s*$",
    )
    .expect("VERSION_RE regex is valid")
});

/// Soft cap on the diagnostic line we carry inside
/// [`MissingReason::UnparseableVersion`]. Witr emits a single short
/// line, but a misbehaving binary could spew multi-KB output — clamp
/// it before it ends up logged or echoed into a render banner.
const DIAGNOSTIC_MAX_BYTES: usize = 200;

fn clamp_diagnostic(s: &str) -> String {
    // Byte-truncate but only at a char boundary to keep the result
    // valid UTF-8. Avoids the [[reference_rust_utf8_truncate_trap]]
    // panic if upstream ever emits a multi-byte glyph.
    let limit = DIAGNOSTIC_MAX_BYTES.min(s.len());
    let mut end = limit;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = s[..end].to_string();
    if end < s.len() {
        out.push('…');
    }
    out
}

/// The shape of a detect call. The plugin maps this 1:1 onto the
/// `plugin/init` ready/missing/outdated banner state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetectResult {
    /// A compatible witr binary is on `PATH`.
    Ready {
        /// Absolute path to the binary (resolved via `which`).
        path: PathBuf,
        /// Parsed semver.
        version: Version,
        /// Short commit hash if the binary was built from a release
        /// pipeline. `None` when the upstream template emitted
        /// `commit = unknown` (go-install / Debian).
        commit: Option<String>,
        /// Build timestamp string, exactly as witr emits it. `None`
        /// when the upstream emitted `built = unknown`.
        built_at: Option<String>,
    },
    /// No usable witr binary. Carries the precise reason so the
    /// empty-state can render targeted install / repair hints (cfx.4).
    Missing(MissingReason),
    /// witr is on `PATH` but its version is below [`MIN_VERSION`].
    Outdated {
        /// The semver we parsed from the running binary.
        found_version: Version,
        /// The minimum required version (== [`MIN_VERSION`]).
        minimum: Version,
    },
}

/// Why detection failed to find a usable witr binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MissingReason {
    /// `which witr` returned no match — binary not on `PATH`.
    NotOnPath,
    /// The binary was found but `witr --version` failed (non-zero
    /// exit, timeout, I/O error). Carries a short diagnostic for the
    /// empty-state banner.
    VersionExecFailed(String),
    /// `witr --version` returned output that didn't match
    /// [`VERSION_RE`]. Carries the offending line for logging.
    UnparseableVersion(String),
}

/// Parsed components of a `witr --version` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedVersion {
    /// Semver parsed from the line.
    pub version: Version,
    /// Commit hash (`None` if the line said `unknown`).
    pub commit: Option<String>,
    /// Build timestamp string (`None` if the line said `unknown`).
    pub built_at: Option<String>,
}

/// Pure parser for `witr --version` output. Exposed so tests can pin
/// the regex contract without spawning subprocesses.
///
/// Scans every line of `raw` and returns the first one that matches
/// [`VERSION_RE`]. This tolerates witr (or any Go binary) emitting a
/// `GODEBUG` notice / deprecation warning before the version line —
/// which is rare in practice but cheap to handle here. Empty input
/// or input where no line matches returns
/// `Err(MissingReason::UnparseableVersion)`.
///
/// Returns `Err(MissingReason::UnparseableVersion)` for both regex
/// miss and semver-parse failure on a matched line. The latter is
/// defensive — the tightened regex (semver alphabet only) should
/// already exclude semver-invalid `ver` captures — but it keeps the
/// bucket lie out: if we matched-but-couldn't-parse, the line is
/// shaped like witr's but isn't actually witr.
pub fn parse_version_output(raw: &str) -> Result<ParsedVersion, MissingReason> {
    // First matching line wins. Most callers pass a one-liner; for
    // pathological multi-line stdout (warnings then version) we walk
    // until we find the real version line.
    let candidate =
        raw.lines()
            .map(|l| l.trim_end())
            .find(|l| VERSION_RE.is_match(l))
            .ok_or_else(|| {
                // No line matched. Carry a short diagnostic from the
                // first non-empty line for the empty-state log surface.
                let preview =
                    raw.lines().map(|l| l.trim_end()).find(|l| !l.is_empty()).unwrap_or("");
                MissingReason::UnparseableVersion(clamp_diagnostic(preview))
            })?;

    let caps = VERSION_RE.captures(candidate).expect("matched line above must capture");

    let ver_str = &caps["ver"];
    let version = Version::parse(ver_str).map_err(|e| {
        MissingReason::UnparseableVersion(clamp_diagnostic(&format!(
            "regex matched but semver parse failed for `{ver_str}`: {e}",
        )))
    })?;

    let commit = match &caps["commit"] {
        "unknown" => None,
        hex => Some(hex.to_string()),
    };

    let built_raw = caps["built"].trim();
    let built_at = if built_raw == "unknown" {
        None
    } else {
        Some(built_raw.to_string())
    };

    Ok(ParsedVersion {
        version,
        commit,
        built_at,
    })
}

/// Classify a parsed version against [`MIN_VERSION`]. Pure — exposed
/// for unit tests so the ready-vs-outdated boundary has its own gate.
#[must_use]
pub fn classify_version(version: &Version) -> VersionClass {
    if version < &MIN_VERSION {
        VersionClass::Outdated
    } else {
        VersionClass::Ready
    }
}

/// Outcome of [`classify_version`]. The async path turns this into
/// `DetectResult::Ready` or `DetectResult::Outdated`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionClass {
    /// Version meets [`MIN_VERSION`].
    Ready,
    /// Version is below [`MIN_VERSION`].
    Outdated,
}

/// One-shot detect of a usable witr binary. Returns a [`DetectResult`]
/// — never panics. Bounded by [`VERSION_EXEC_TIMEOUT`].
///
/// 1. Use `which::which("witr")` to resolve the binary path.
/// 2. `tokio::process::Command::new(<path>).arg("--version")` with a
///    closed stdin and a 5-second timeout.
/// 3. Parse stdout via [`parse_version_output`].
/// 4. Compare against [`MIN_VERSION`] via [`classify_version`].
///
/// The async path is thin glue. The interesting decisions
/// (regex shape, `unknown` handling, version gate) live in
/// [`parse_version_output`] + [`classify_version`] and are unit-tested.
pub async fn detect_witr() -> DetectResult {
    let path = match which::which("witr") {
        Ok(p) => {
            tracing::debug!(?p, "resolved witr binary via which");
            p
        }
        Err(_) => {
            tracing::warn!("witr not found on PATH");
            return DetectResult::Missing(MissingReason::NotOnPath);
        }
    };

    let exec = Command::new(&path).arg("--version").stdin(std::process::Stdio::null()).output();

    let output = match timeout(VERSION_EXEC_TIMEOUT, exec).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            let msg = clamp_diagnostic(&format!("spawn failed: {e}"));
            tracing::warn!(error = %msg, "witr --version spawn failed");
            return DetectResult::Missing(MissingReason::VersionExecFailed(msg));
        }
        Err(_) => {
            let msg = format!(
                "witr --version timed out after {}s",
                VERSION_EXEC_TIMEOUT.as_secs()
            );
            tracing::warn!(error = %msg, "witr --version timed out");
            return DetectResult::Missing(MissingReason::VersionExecFailed(msg));
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let msg = clamp_diagnostic(&format!(
            "witr --version exited {:?}: {stderr}",
            output.status.code()
        ));
        tracing::warn!(error = %msg, "witr --version non-zero exit");
        return DetectResult::Missing(MissingReason::VersionExecFailed(msg));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed = match parse_version_output(&stdout) {
        Ok(p) => p,
        Err(reason) => {
            tracing::warn!(?reason, "could not parse witr --version output");
            return DetectResult::Missing(reason);
        }
    };

    match classify_version(&parsed.version) {
        VersionClass::Ready => {
            tracing::debug!(version = %parsed.version, "witr ready");
            DetectResult::Ready {
                path,
                version: parsed.version,
                commit: parsed.commit,
                built_at: parsed.built_at,
            }
        }
        VersionClass::Outdated => {
            tracing::warn!(
                found = %parsed.version,
                minimum = %*MIN_VERSION,
                "witr below minimum version",
            );
            DetectResult::Outdated {
                found_version: parsed.version,
                minimum: MIN_VERSION.clone(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_release_line() {
        let raw = "witr v0.3.2 (commit a1b2c3d, built 2025-04-10T12:00:00Z)\n";
        let p = parse_version_output(raw).expect("release line parses");
        assert_eq!(p.version, Version::parse("0.3.2").unwrap());
        assert_eq!(p.commit.as_deref(), Some("a1b2c3d"));
        assert_eq!(p.built_at.as_deref(), Some("2025-04-10T12:00:00Z"));
    }

    #[test]
    fn parses_go_install_unknown_commit_and_built() {
        let raw = "witr v0.3.5 (commit unknown, built unknown)\n";
        let p = parse_version_output(raw).expect("go-install line parses");
        assert_eq!(p.version, Version::parse("0.3.5").unwrap());
        assert_eq!(p.commit, None, "`unknown` commit maps to None");
        assert_eq!(p.built_at, None, "`unknown` built_at maps to None");
    }

    #[test]
    fn parses_prerelease_suffix() {
        let raw = "witr v0.4.0-rc.1 (commit deadbee, built 2026-01-01T00:00:00Z)\n";
        let p = parse_version_output(raw).expect("prerelease line parses");
        assert_eq!(p.version, Version::parse("0.4.0-rc.1").unwrap());
    }

    #[test]
    fn parses_long_commit_hash() {
        let raw =
            "witr v0.3.2 (commit a1b2c3d4e5f6789012345678901234567890abcd, built 2025-04-10)\n";
        let p = parse_version_output(raw).expect("long-hash line parses");
        assert_eq!(p.commit.as_deref().unwrap().len(), 40);
    }

    #[test]
    fn rejects_line_without_v_prefix() {
        let raw = "witr 0.3.2 (commit a1b2c3d, built 2025-04-10T12:00:00Z)\n";
        let err = parse_version_output(raw).expect_err("missing v prefix must fail");
        assert!(
            matches!(err, MissingReason::UnparseableVersion(_)),
            "expected UnparseableVersion, got {err:?}",
        );
    }

    #[test]
    fn rejects_random_text() {
        let err = parse_version_output("hello").expect_err("random text must fail");
        assert!(matches!(err, MissingReason::UnparseableVersion(_)));
    }

    #[test]
    fn rejects_empty_input() {
        let err = parse_version_output("").expect_err("empty input must fail");
        assert!(matches!(err, MissingReason::UnparseableVersion(_)));
    }

    #[test]
    fn rejects_short_commit_hash() {
        // 6-char hash — below the regex's 7-40 floor. The current
        // upstream goreleaser default is 7; capturing this guards
        // against a future "drop one char" drift surfacing as a
        // silent unknown.
        let raw = "witr v0.3.2 (commit a1b2c3, built 2025-04-10T12:00:00Z)\n";
        let err = parse_version_output(raw).expect_err("short hash must fail");
        assert!(matches!(err, MissingReason::UnparseableVersion(_)));
    }

    #[test]
    fn min_version_is_032() {
        assert_eq!(*MIN_VERSION, Version::parse("0.3.2").unwrap());
    }

    #[test]
    fn classifies_below_minimum_as_outdated() {
        assert_eq!(
            classify_version(&Version::parse("0.3.1").unwrap()),
            VersionClass::Outdated,
        );
        assert_eq!(
            classify_version(&Version::parse("0.2.99").unwrap()),
            VersionClass::Outdated,
        );
    }

    #[test]
    fn classifies_at_or_above_minimum_as_ready() {
        assert_eq!(
            classify_version(&Version::parse("0.3.2").unwrap()),
            VersionClass::Ready,
        );
        assert_eq!(
            classify_version(&Version::parse("0.3.3").unwrap()),
            VersionClass::Ready,
        );
        assert_eq!(
            classify_version(&Version::parse("1.0.0").unwrap()),
            VersionClass::Ready,
        );
    }

    #[test]
    fn parses_version_line_after_warning_prefix() {
        // Some Go binaries leak `GODEBUG` notices or deprecation
        // warnings to stdout before their primary output. We accept
        // that by walking lines and picking the first regex hit.
        let raw = concat!(
            "WARN: GODEBUG legacy mode active\n",
            "witr v0.3.2 (commit a1b2c3d, built 2025-04-10T12:00:00Z)\n",
        );
        let p = parse_version_output(raw).expect("warning-then-version parses");
        assert_eq!(p.version, Version::parse("0.3.2").unwrap());
        assert_eq!(p.commit.as_deref(), Some("a1b2c3d"));
    }

    #[test]
    fn parses_version_line_with_leading_blank_line() {
        let raw = "\nwitr v0.3.2 (commit a1b2c3d, built 2025-04-10T12:00:00Z)\n";
        let p = parse_version_output(raw).expect("blank-leading parses");
        assert_eq!(p.version, Version::parse("0.3.2").unwrap());
    }

    #[test]
    fn parses_version_line_with_crlf() {
        let raw = "witr v0.3.2 (commit a1b2c3d, built 2025-04-10T12:00:00Z)\r\n";
        let p = parse_version_output(raw).expect("CRLF parses");
        assert_eq!(p.version, Version::parse("0.3.2").unwrap());
    }

    #[test]
    fn rejects_alphanumeric_pollution_after_patch() {
        // Tightened regex (semver alphabet only). Garbage like
        // `0.3.2abc!!!` no longer slips past the regex and gets
        // misclassified as a malformed-semver internal error.
        let raw = "witr v0.3.2!!! (commit a1b2c3d, built 2025-04-10T12:00:00Z)\n";
        let err = parse_version_output(raw).expect_err("garbage suffix must fail");
        assert!(
            matches!(err, MissingReason::UnparseableVersion(_)),
            "expected UnparseableVersion, got {err:?}",
        );
    }

    #[test]
    fn clamps_long_diagnostic_with_ellipsis() {
        let huge = "x".repeat(1_000);
        let clamped = clamp_diagnostic(&huge);
        assert!(
            clamped.len() <= DIAGNOSTIC_MAX_BYTES + 3,
            "clamped len {} should be <= {} + ellipsis bytes",
            clamped.len(),
            DIAGNOSTIC_MAX_BYTES,
        );
        assert!(clamped.ends_with('…'));
    }

    #[test]
    fn clamp_diagnostic_preserves_short_input_unchanged() {
        let short = "witr v0.3.2 — short line";
        assert_eq!(clamp_diagnostic(short), short);
    }

    #[test]
    fn clamp_diagnostic_respects_utf8_char_boundary() {
        // The em-dash `—` (U+2014) is 3 bytes. If we'd byte-truncate
        // at an arbitrary cut, we'd panic. The clamper walks back to
        // a char boundary first.
        let s = format!("{}—tail", "x".repeat(DIAGNOSTIC_MAX_BYTES - 1));
        let clamped = clamp_diagnostic(&s);
        // Must be valid UTF-8 — `String` invariant — and end in the
        // ellipsis we appended.
        assert!(clamped.ends_with('…'));
    }
}
