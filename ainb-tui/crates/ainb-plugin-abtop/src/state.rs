//! Plugin state — lifecycle + UI mode for the abtop plugin.
//!
//! Deliberately minimal compared to witr: abtop has no data plane (no
//! `--json`, no parsed snapshot tabs), so there is no snapshot cache,
//! no scan gate, and no tab enum. Two orthogonal pieces of state
//! remain:
//!
//! 1. **Lifecycle** ([`Lifecycle`]) — whether abtop was detected on
//!    PATH. Drives the empty-state install hint vs the ready hint.
//! 2. **UI mode** ([`UiMode`]) — only [`UiMode::Browsing`] today; kept
//!    as an enum so a future interactive surface can extend it without
//!    reshaping the plugin struct.
//!
//! No persistence — flushed at shutdown.

use crate::detect::DetectResult;

/// Whether the plugin can talk to abtop right now. Driven by detect;
/// consumed by the render dispatcher (paint the ready hint only when
/// `Ready`, the install hint when `Missing`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lifecycle {
    /// Detect hasn't run yet (or was abandoned). Plugin renders a
    /// transient "checking abtop…" line.
    Unknown,
    /// abtop is on PATH. Ready hint active.
    Ready,
    /// Detect returned a `Missing` variant. Empty-state painter shows
    /// install instructions.
    Missing(crate::detect::MissingReason),
}

impl Lifecycle {
    /// Construct a [`Lifecycle`] from a [`DetectResult`]. Pure
    /// translation — no side effects.
    #[must_use]
    pub fn from_detect(result: DetectResult) -> Self {
        match result {
            DetectResult::Ready { .. } => Self::Ready,
            DetectResult::Missing(reason) => Self::Missing(reason),
        }
    }
}

/// What the user is currently doing inside the abtop screen.
///
/// Only [`UiMode::Browsing`] exists today — the live agent view is a
/// host-side tmux full-screen attach, not an in-plugin interactive
/// surface. The enum is the seam a future in-plugin mode would extend.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum UiMode {
    /// Normal browsing — the ready/empty hint is shown; `r` re-detects
    /// when not Ready.
    #[default]
    Browsing,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::MissingReason;

    #[test]
    fn lifecycle_from_detect_maps_ready() {
        let result = DetectResult::Ready {
            path: std::path::PathBuf::from("/usr/local/bin/abtop"),
        };
        assert_eq!(Lifecycle::from_detect(result), Lifecycle::Ready);
    }

    #[test]
    fn lifecycle_from_detect_maps_missing() {
        let result = DetectResult::Missing(MissingReason::NotOnPath);
        assert!(matches!(
            Lifecycle::from_detect(result),
            Lifecycle::Missing(MissingReason::NotOnPath)
        ));
    }

    #[test]
    fn ui_mode_default_is_browsing() {
        assert_eq!(UiMode::default(), UiMode::Browsing);
    }
}
