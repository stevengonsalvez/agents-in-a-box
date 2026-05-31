//! Empty-state painter for [`DetectResult::Missing`] and
//! [`DetectResult::Outdated`].
//!
//! Rendered when the plugin's lifecycle gate (cfx.2 detect) refused
//! to declare witr ready. The painter writes a single static layout
//! into a [`WireBuffer`] — headline, platform-detected install
//! command, curl-bash fallback, project link, and a `press r to
//! re-check` hint. No interactive widgets — cfx.5 owns the live tab
//! UI, this module owns the "nothing-to-show-yet" surface.
//!
//! ## Platform detection
//!
//! Resolved via `std::env::consts::{OS, ARCH}` at runtime — `cfg!`
//! would inline a single branch and leave the `_ => unsupported`
//! arm dead on every supported CI. v1 support matrix per
//! `plans/witr-plugin-spec.md`:
//!
//! | Platform        | Primary command           | Fallback                |
//! |-----------------|---------------------------|-------------------------|
//! | macOS arm64     | `brew install witr`       | curl-bash installer     |
//! | Linux x86_64    | `brew install witr` / apt | curl-bash installer     |
//! | (anything else) | docs link                 | curl-bash installer     |
//!
//! Unsupported platforms still get the curl-bash fallback so the
//! plugin doesn't dead-end; it just notes "platform not officially
//! tested".
//!
//! ## Wire shape
//!
//! Cells are written via absolute (x,y) `Coord` pushes into the
//! buffer. The painter is best-effort on small viewports: lines that
//! would land below `viewport.height` are dropped silently. No panic
//! on a 1×1 buffer.

use ainb_plugin_sdk::{Cell, Coord, Viewport, WireBuffer};
use semver::Version;

use crate::detect::MissingReason;

/// One install-command hint, resolved for the current build target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallHint {
    /// Primary suggested command. The first command the user should
    /// try — package-manager native where available.
    pub primary: String,
    /// Curl-bash fallback. Always present; works when the primary
    /// fails or isn't installed.
    pub fallback: String,
    /// Human label for the target platform (e.g. `"macOS arm64"`).
    /// Surfaces in the empty state so the user can tell which install
    /// path the plugin assumed.
    pub platform_note: String,
}

/// Standard curl-bash installer from the witr release pipeline. Same
/// link upstream prints in its README installation section.
const CURL_BASH_FALLBACK: &str =
    "curl -sSL https://github.com/pranshuparmar/witr/releases/latest/download/install.sh | bash";

/// Public README URL. Shown verbatim in the empty state as the
/// canonical reference if the user wants to read more before
/// installing.
const README_URL: &str = "https://github.com/pranshuparmar/witr#installation";

/// Resolve an [`InstallHint`] for the current build target.
///
/// Pure — no I/O, no syscalls. Defers to [`install_hint`] with the
/// current `std::env::consts::OS` / `ARCH` so the unsupported-platform
/// branch stays testable on every runner. Match values come from
/// Rust's platform-name constants — no `cfg!` macro, so a single
/// build binary handles all paths.
#[must_use]
pub fn install_hint_for_current_platform() -> InstallHint {
    install_hint(std::env::consts::OS, std::env::consts::ARCH)
}

/// Resolve an [`InstallHint`] for an arbitrary `(os, arch)` pair.
///
/// Pure function — exposed crate-private so tests can pin the
/// "unsupported platform" branch (`install_hint("freebsd", "x86_64")`)
/// regardless of where they're running. Public callers should use
/// [`install_hint_for_current_platform`].
pub(crate) fn install_hint(os: &str, arch: &str) -> InstallHint {
    match (os, arch) {
        ("macos", "aarch64") => InstallHint {
            primary: "brew install witr".into(),
            fallback: CURL_BASH_FALLBACK.into(),
            platform_note: "macOS arm64".into(),
        },
        ("linux", "x86_64") => InstallHint {
            primary: "brew install witr  (or `apt install witr` on Ubuntu 26.04+)".into(),
            fallback: CURL_BASH_FALLBACK.into(),
            platform_note: "Linux x86_64".into(),
        },
        _ => InstallHint {
            primary: format!("see {README_URL}"),
            fallback: CURL_BASH_FALLBACK.into(),
            platform_note: "unsupported platform — best effort".into(),
        },
    }
}

/// Render the missing-witr empty state into `buf`.
///
/// `reason` tunes the second-line diagnostic (path-not-found vs
/// exec-failed vs version-line-unparseable). The install command and
/// hint footer are the same across all three.
pub fn render_missing(buf: &mut WireBuffer, viewport: Viewport, reason: &MissingReason) {
    let hint = install_hint_for_current_platform();
    let diagnostic = diagnostic_for_missing(reason, viewport.width);
    let lines = compose_missing_layout(&hint, &diagnostic);
    paint_lines(buf, viewport, &lines);
}

/// Render the outdated-witr empty state into `buf`.
///
/// Shown when the binary was found but `Version::parse` returned a
/// value below [`crate::detect::MIN_VERSION`]. The upgrade command
/// is the same package-manager-aware string as the install command —
/// `brew upgrade` is materially the same UX as `brew install` from
/// the user's POV.
pub fn render_outdated(
    buf: &mut WireBuffer,
    viewport: Viewport,
    found: &Version,
    minimum: &Version,
) {
    let hint = install_hint_for_current_platform();
    let lines = compose_outdated_layout(&hint, found, minimum);
    paint_lines(buf, viewport, &lines);
}

fn diagnostic_for_missing(reason: &MissingReason, viewport_width: u16) -> String {
    match reason {
        MissingReason::NotOnPath => "no `witr` binary on PATH".to_string(),
        MissingReason::VersionExecFailed(msg) => {
            let prefix = "`witr --version` failed: ";
            format!(
                "{prefix}{}",
                clip_to_width(msg, viewport_width, prefix.len())
            )
        }
        MissingReason::UnparseableVersion(line) => {
            let prefix = "`witr --version` output not understood: ";
            format!(
                "{prefix}{}",
                clip_to_width(line, viewport_width, prefix.len())
            )
        }
    }
}

/// Clip `tail` to the bytes that still fit on a row of `viewport_width`
/// cells after `prefix_len` chars of headline. Appends `…` when
/// truncation happens. Operates on `char`s (one cell each) — the
/// renderer's invariant for cfx.4 (ASCII-only install commands).
///
/// Without this, `paint_text` clips silently at the viewport edge
/// and the user loses the tail of a long diagnostic with no
/// indication. The upstream byte-clamp in detect.rs (200 bytes)
/// protects the log channel; the render channel needs its own
/// width-aware trim because viewport widths vary per terminal.
fn clip_to_width(tail: &str, viewport_width: u16, prefix_len: usize) -> String {
    let width = viewport_width as usize;
    if width == 0 || prefix_len >= width {
        return String::new();
    }
    let budget = width - prefix_len;
    if tail.chars().count() <= budget {
        return tail.to_string();
    }
    if budget == 0 {
        return String::new();
    }
    // Reserve one cell for the trailing ellipsis.
    let keep = budget.saturating_sub(1);
    let mut out: String = tail.chars().take(keep).collect();
    out.push('…');
    out
}

fn compose_missing_layout(hint: &InstallHint, diagnostic: &str) -> Vec<String> {
    vec![
        "witr not found".to_string(),
        String::new(),
        diagnostic.to_string(),
        String::new(),
        format!("install ({}):", hint.platform_note),
        format!("  {}", hint.primary),
        format!("  {}", hint.fallback),
        String::new(),
        format!("see {README_URL}"),
        String::new(),
        "press r to re-check".to_string(),
    ]
}

fn compose_outdated_layout(hint: &InstallHint, found: &Version, minimum: &Version) -> Vec<String> {
    // We use the same `brew install` / `apt install` command for the
    // upgrade path. Homebrew's `brew install <pkg>` on an outdated
    // package actually upgrades; `apt install <pkg>` does the same.
    // The upgrade UX is unified with install — one command to remember,
    // not two.
    vec![
        format!("witr {found} is too old"),
        String::new(),
        format!("minimum required: {minimum}"),
        String::new(),
        format!("upgrade ({}):", hint.platform_note),
        format!("  {}", hint.primary),
        format!("  {}", hint.fallback),
        String::new(),
        format!("see {README_URL}"),
        String::new(),
        "press r to re-check".to_string(),
    ]
}

fn paint_lines(buf: &mut WireBuffer, viewport: Viewport, lines: &[String]) {
    let max_y = viewport.height;
    let max_x = viewport.width;
    for (row_idx, line) in lines.iter().enumerate() {
        let y = row_idx as u16;
        if y >= max_y {
            break;
        }
        paint_text(buf, 0, y, line, max_x);
    }
}

/// Paint `text` left-aligned at `(x0, y)` clipped to `max_x`.
///
/// Operates on Unicode `char`s as 1-cell-each. **Correct for the
/// ASCII install commands cfx.4 paints**; cfx.5's tab UI will need
/// to honour grapheme cluster width (East-Asian wide chars / emoji)
/// for user-supplied data like process names — use `unicode-width`
/// at that point.
fn paint_text(buf: &mut WireBuffer, x0: u16, y: u16, text: &str, max_x: u16) {
    let mut x = x0;
    for ch in text.chars() {
        if x >= max_x {
            break;
        }
        buf.push(Coord::new(x, y), Cell::new(ch.to_string()));
        x = x.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::test_support::{buffer_contains, flatten_rows_trimmed, painted_cell_count};

    fn vp(w: u16, h: u16) -> Viewport {
        Viewport {
            width: w,
            height: h,
        }
    }

    #[test]
    fn install_hint_is_populated_with_a_fallback() {
        let hint = install_hint_for_current_platform();
        assert!(!hint.primary.is_empty());
        assert!(hint.fallback.contains("curl"));
        assert!(hint.fallback.contains("witr"));
        assert!(!hint.platform_note.is_empty());
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn macos_arm64_uses_brew_primary() {
        let hint = install_hint_for_current_platform();
        assert!(hint.primary.starts_with("brew install witr"));
        assert!(hint.platform_note.contains("macOS"));
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn linux_x86_64_mentions_apt_or_brew() {
        let hint = install_hint_for_current_platform();
        assert!(
            hint.primary.contains("brew install witr") || hint.primary.contains("apt install witr"),
        );
        assert!(hint.platform_note.contains("Linux"));
    }

    #[test]
    fn render_missing_paints_headline_and_install_command() {
        let mut buf = WireBuffer::new(80, 20);
        render_missing(&mut buf, vp(80, 20), &MissingReason::NotOnPath);
        assert!(buffer_contains(&buf, "witr not found"));
        assert!(buffer_contains(&buf, "no `witr` binary on PATH"));
        assert!(buffer_contains(&buf, "press r to re-check"));
        // Curl-bash fallback is always present, regardless of platform.
        assert!(buffer_contains(&buf, "curl"));
        // Buffer dimensions are preserved exactly as requested.
        assert_eq!(buf.width, 80);
        assert_eq!(buf.height, 20);
    }

    #[test]
    fn render_missing_includes_specific_diagnostic_for_version_exec_failure() {
        let mut buf = WireBuffer::new(80, 20);
        render_missing(
            &mut buf,
            vp(80, 20),
            &MissingReason::VersionExecFailed("permission denied".into()),
        );
        assert!(buffer_contains(&buf, "permission denied"));
    }

    #[test]
    fn render_missing_includes_unparseable_line() {
        let mut buf = WireBuffer::new(80, 20);
        render_missing(
            &mut buf,
            vp(80, 20),
            &MissingReason::UnparseableVersion("witr 0.3.2".into()),
        );
        assert!(buffer_contains(&buf, "not understood"));
        assert!(buffer_contains(&buf, "witr 0.3.2"));
    }

    #[test]
    fn render_outdated_paints_found_and_minimum() {
        let found = Version::parse("0.3.1").unwrap();
        let min = Version::parse("0.3.2").unwrap();
        let mut buf = WireBuffer::new(80, 20);
        render_outdated(&mut buf, vp(80, 20), &found, &min);
        assert!(buffer_contains(&buf, "witr 0.3.1 is too old"));
        assert!(buffer_contains(&buf, "minimum required: 0.3.2"));
        assert!(buffer_contains(&buf, "press r to re-check"));
    }

    #[test]
    fn render_missing_clamps_to_tiny_viewport_without_panic() {
        // Sub-headline viewport: only the first row + a few chars fit.
        let mut buf = WireBuffer::new(10, 1);
        render_missing(&mut buf, vp(10, 1), &MissingReason::NotOnPath);
        // No panic; we painted at most the first row, clipped at x=10.
        assert_eq!(buf.width, 10);
        assert_eq!(buf.height, 1);
        // First row must contain the leading characters of the headline.
        let rows = flatten_rows_trimmed(&buf);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].starts_with("witr not f"));
    }

    #[test]
    fn render_missing_clamps_to_zero_height_no_panic() {
        let mut buf = WireBuffer::new(80, 0);
        render_missing(&mut buf, vp(80, 0), &MissingReason::NotOnPath);
        assert_eq!(
            painted_cell_count(&buf),
            0,
            "0-height viewport paints nothing"
        );
    }

    #[test]
    fn render_missing_clamps_to_zero_width_no_panic() {
        let mut buf = WireBuffer::new(0, 20);
        render_missing(&mut buf, vp(0, 20), &MissingReason::NotOnPath);
        assert_eq!(
            painted_cell_count(&buf),
            0,
            "0-width viewport paints nothing"
        );
    }

    #[test]
    fn install_hint_fallback_link_points_to_pranshuparmar_witr() {
        let hint = install_hint_for_current_platform();
        assert!(
            hint.fallback.contains("github.com/pranshuparmar/witr"),
            "fallback URL must point to the upstream repo: {}",
            hint.fallback,
        );
    }

    #[test]
    fn install_hint_unsupported_platform_yields_docs_link() {
        // Pin the platform-agnostic branch directly via the private
        // helper. Without this, the `_ => ...` arm of `install_hint`
        // is dead code on every supported CI runner.
        let hint = install_hint("freebsd", "x86_64");
        assert!(
            hint.primary.contains("github.com/pranshuparmar/witr"),
            "unsupported-platform primary should be the docs link, got {}",
            hint.primary,
        );
        assert!(hint.fallback.contains("curl"));
        assert!(hint.platform_note.contains("unsupported"));
    }

    #[test]
    fn install_hint_macos_arm64_via_private_helper() {
        let hint = install_hint("macos", "aarch64");
        assert_eq!(hint.primary, "brew install witr");
        assert!(hint.platform_note.contains("macOS"));
    }

    #[test]
    fn install_hint_linux_x86_64_via_private_helper() {
        let hint = install_hint("linux", "x86_64");
        assert!(hint.primary.contains("brew install witr"));
        assert!(hint.primary.contains("apt install witr"));
        assert!(hint.platform_note.contains("Linux"));
    }

    #[test]
    fn clip_to_width_passes_short_input_unchanged() {
        let r = clip_to_width("short", 80, 10);
        assert_eq!(r, "short");
    }

    #[test]
    fn clip_to_width_truncates_with_ellipsis_when_overflowing() {
        // Viewport width 20, prefix 10, budget = 10 cells. Tail of
        // 12 chars must truncate to 9 chars + `…` so the whole row
        // (prefix + truncated tail) fits in 20.
        let r = clip_to_width("abcdefghijkl", 20, 10);
        assert_eq!(r.chars().count(), 10);
        assert!(r.ends_with('…'));
        assert_eq!(r, "abcdefghi…");
    }

    #[test]
    fn clip_to_width_handles_zero_budget_gracefully() {
        // Prefix already consumes the entire row — nothing to paint
        // for the tail.
        assert_eq!(clip_to_width("x", 10, 10), "");
        assert_eq!(clip_to_width("x", 10, 11), "");
    }

    #[test]
    fn render_missing_clamps_long_unparseable_diagnostic_to_viewport() {
        // 1000-char garbage line through a 60-wide viewport must show
        // a trailing `…` rather than silently clipping at column 60.
        let huge_line = "x".repeat(1_000);
        let mut buf = WireBuffer::new(60, 20);
        render_missing(
            &mut buf,
            vp(60, 20),
            &MissingReason::UnparseableVersion(huge_line),
        );
        let rows = flatten_rows_trimmed(&buf);
        let diag_row = rows
            .iter()
            .find(|r| r.contains("not understood"))
            .expect("diagnostic row painted");
        assert!(
            diag_row.contains('…'),
            "diagnostic should end with ellipsis when clipped: {diag_row:?}",
        );
        assert!(
            diag_row.chars().count() <= 60,
            "diagnostic row fits viewport width: {} chars",
            diag_row.chars().count(),
        );
    }
}
