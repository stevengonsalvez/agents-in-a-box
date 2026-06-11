//! Detect the external `abtop` binary on the user's PATH.
//!
//! Unlike witr, abtop has no machine-readable version contract worth
//! gating on (no `--json`, and `--version` is informational only). The
//! plugin only ever execs `abtop --once` and forwards its text, so the
//! only fact detection needs is **present or absent**:
//!
//! - The binary resolves somewhere on `PATH` → [`DetectResult::Ready`].
//! - `which abtop` finds nothing → [`DetectResult::Missing`].
//!
//! There is deliberately no version probe, no `--version` exec, and no
//! semver classification — see the abtop-plugin integration spec
//! (Phase 1) § Architecture decisions. That keeps detection a single
//! cheap `which` lookup with no subprocess spawn.

use std::path::PathBuf;

/// The shape of a detect call. The plugin maps this 1:1 onto the
/// `plugin/init` ready/missing banner state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetectResult {
    /// An `abtop` binary is on `PATH`.
    Ready {
        /// Absolute path to the binary (resolved via `which`).
        path: PathBuf,
    },
    /// No usable abtop binary. Carries the precise reason so the
    /// empty-state can render a targeted install hint.
    Missing(MissingReason),
}

/// Why detection failed to find a usable abtop binary.
///
/// Only one variant today (`which abtop` miss). Kept as an enum so the
/// empty-state painter and a future richer detect can extend it without
/// reshaping the [`DetectResult::Missing`] signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MissingReason {
    /// `which abtop` returned no match — binary not on `PATH`.
    NotOnPath,
}

/// One-shot detect of an abtop binary. Returns a [`DetectResult`] —
/// never panics, never spawns a subprocess.
///
/// Async only to match the witr-style call site + leave room for a
/// future probe; the body is a synchronous `which::which` lookup.
pub async fn detect_abtop() -> DetectResult {
    match which::which("abtop") {
        Ok(path) => {
            tracing::debug!(?path, "resolved abtop binary via which");
            DetectResult::Ready { path }
        }
        Err(_) => {
            tracing::warn!("abtop not found on PATH");
            DetectResult::Missing(MissingReason::NotOnPath)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_reason_is_distinct_from_ready() {
        let ready = DetectResult::Ready {
            path: PathBuf::from("/usr/local/bin/abtop"),
        };
        let missing = DetectResult::Missing(MissingReason::NotOnPath);
        assert_ne!(ready, missing);
    }
}
