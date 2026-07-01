//! Empty-state + ready-state painters for the abtop plugin.
//!
//! Two surfaces, both static text written into a [`WireBuffer`]:
//!
//! - [`render_missing`] — shown when abtop isn't on PATH. Platform-aware
//!   install hint: headline, primary install command, the universal
//!   `cargo install abtop` fallback, the repo URL, and a `press r to
//!   re-check` footer.
//! - [`render_ready`] — shown when abtop IS installed and the screen is
//!   navigated to directly. A short hint that the live full-screen view
//!   is opened from the menu (the real content is a host-side tmux
//!   attach, not this screen).
//!
//! ## Platform detection
//!
//! Resolved via `std::env::consts::{OS, ARCH}` at runtime — `cfg!`
//! would inline a single branch and leave the other arms dead on every
//! supported CI runner. Support matrix:
//!
//! | Platform   | Primary install command          |
//! |------------|----------------------------------|
//! | macOS      | `brew install graykode/tap/abtop`|
//! | Linux      | curl-bash installer script       |
//! | (other)    | `see <repo URL>`                 |
//!
//! Every platform additionally gets the universal `cargo install abtop`
//! fallback plus the repo URL so the hint never dead-ends.
//!
//! ## Wire shape
//!
//! Cells are written via absolute (x,y) `Coord` pushes. The painter is
//! best-effort on small viewports: lines that would land below
//! `viewport.height` are dropped silently. No panic on a 1×1 (or 0×0)
//! buffer.

use ainb_plugin_sdk::{Cell, Coord, Viewport, WireBuffer};

/// macOS Homebrew tap install command.
const BREW_INSTALL: &str = "brew install graykode/tap/abtop";

/// Linux installer-script one-liner from the abtop release pipeline.
/// Points at the canonical `graykode/abtop` releases — the published
/// distribution (crates.io crate + `graykode/tap` brew formula +
/// release installer all live there); forks carry no release assets.
const LINUX_INSTALLER: &str =
    "curl -sSL https://github.com/graykode/abtop/releases/latest/download/abtop-installer.sh | sh";

/// Universal fallback — works anywhere a Rust toolchain is present.
const CARGO_INSTALL: &str = "cargo install abtop";

/// Public repository URL. Shown verbatim as the canonical reference.
const REPO_URL: &str = "https://github.com/graykode/abtop";

/// ainb docsite page for the abtop plugin — shows what it looks like inside
/// ainb (screenshots). Full bare URL so terminals auto-linkify it and it stays
/// copyable; left-aligned painting keeps it from truncating at >=80 cols.
const DOCSITE_URL: &str = "https://stevengonsalvez.github.io/agents-in-a-box/plugins/abtop/";

/// One install-command hint, resolved for the current platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallHint {
    /// Primary suggested command — package-manager native where
    /// available, else the repo link.
    pub primary: String,
    /// Human label for the target platform (e.g. `"macOS"`). Surfaces
    /// so the user can tell which install path the plugin assumed.
    pub platform_note: String,
}

/// Resolve an [`InstallHint`] for the current platform.
///
/// Pure — no I/O. Defers to [`install_hint`] with the current
/// `std::env::consts::OS` so the unsupported-platform branch stays
/// testable on every runner.
#[must_use]
pub fn install_hint_for_current_platform() -> InstallHint {
    install_hint(std::env::consts::OS)
}

/// Resolve an [`InstallHint`] for an arbitrary `os` string.
///
/// Pure — exposed crate-private so tests can pin the
/// "unsupported platform" branch regardless of where they run. Public
/// callers should use [`install_hint_for_current_platform`].
pub(crate) fn install_hint(os: &str) -> InstallHint {
    match os {
        "macos" => InstallHint {
            primary: BREW_INSTALL.into(),
            platform_note: "macOS".into(),
        },
        "linux" => InstallHint {
            primary: LINUX_INSTALLER.into(),
            platform_note: "Linux".into(),
        },
        _ => InstallHint {
            primary: format!("see {REPO_URL}"),
            platform_note: "unsupported platform — best effort".into(),
        },
    }
}

/// Render the missing-abtop empty state into `buf`.
///
/// The layout is the same regardless of *why* abtop is missing today
/// (only `NotOnPath` exists); the `reason` argument is kept so a future
/// richer [`MissingReason`](crate::detect::MissingReason) can tune the
/// diagnostic line without reshaping the signature.
pub fn render_missing(
    buf: &mut WireBuffer,
    viewport: Viewport,
    _reason: &crate::detect::MissingReason,
) {
    let hint = install_hint_for_current_platform();
    let lines = compose_missing_layout(&hint);
    paint_lines(buf, viewport, &lines);
}

/// Render the ready hint into `buf` — shown when abtop IS installed and
/// the screen was navigated to directly. The real live view is a
/// host-side tmux full-screen attach (opened from the menu); this hint
/// just points the user there.
pub fn render_ready(buf: &mut WireBuffer, viewport: Viewport) {
    let lines = compose_ready_layout();
    paint_lines(buf, viewport, &lines);
}

fn compose_missing_layout(hint: &InstallHint) -> Vec<String> {
    vec![
        "abtop not found".to_string(),
        String::new(),
        "no `abtop` binary on PATH".to_string(),
        String::new(),
        format!("install ({}):", hint.platform_note),
        format!("  {}", hint.primary),
        String::new(),
        "or, with a Rust toolchain:".to_string(),
        format!("  {CARGO_INSTALL}"),
        String::new(),
        format!("install help: {REPO_URL}"),
        format!("docs: {DOCSITE_URL}"),
        String::new(),
        "press r to re-check".to_string(),
    ]
}

fn compose_ready_layout() -> Vec<String> {
    vec![
        "abtop is installed".to_string(),
        String::new(),
        "open it from the menu (press t) to view agents full-screen".to_string(),
        String::new(),
        format!("docs: {DOCSITE_URL}"),
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
/// Operates on Unicode `char`s as 1-cell-each. Correct for the ASCII
/// install commands here. Never panics on a tiny or zero-width
/// viewport — the `x >= max_x` guard short-circuits before any push.
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
    use crate::detect::MissingReason;
    use crate::render::test_support::{buffer_contains, flatten_rows_trimmed, painted_cell_count};

    fn vp(w: u16, h: u16) -> Viewport {
        Viewport {
            width: w,
            height: h,
        }
    }

    #[test]
    fn install_hint_is_populated_with_a_platform_note() {
        let hint = install_hint_for_current_platform();
        assert!(!hint.primary.is_empty());
        assert!(!hint.platform_note.is_empty());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_uses_brew_tap_primary() {
        let hint = install_hint_for_current_platform();
        assert_eq!(hint.primary, "brew install graykode/tap/abtop");
        assert!(hint.platform_note.contains("macOS"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_uses_installer_script_primary() {
        let hint = install_hint_for_current_platform();
        assert!(hint.primary.contains("abtop-installer.sh"));
        assert!(hint.platform_note.contains("Linux"));
    }

    #[test]
    fn install_hint_macos_via_private_helper() {
        let hint = install_hint("macos");
        assert_eq!(hint.primary, "brew install graykode/tap/abtop");
        assert!(hint.platform_note.contains("macOS"));
    }

    #[test]
    fn install_hint_linux_via_private_helper() {
        let hint = install_hint("linux");
        assert!(hint.primary.contains("abtop-installer.sh"));
        assert!(hint.primary.contains("github.com/graykode/abtop"));
        assert!(hint.platform_note.contains("Linux"));
    }

    #[test]
    fn install_hint_unsupported_platform_yields_repo_link() {
        // Pin the `_ => ...` branch directly so it isn't dead code on
        // every supported CI runner.
        let hint = install_hint("freebsd");
        assert!(
            hint.primary.contains("github.com/graykode/abtop"),
            "unsupported-platform primary should be the repo link, got {}",
            hint.primary,
        );
        assert!(hint.platform_note.contains("unsupported"));
    }

    #[test]
    fn render_missing_paints_headline_install_and_fallback() {
        let mut buf = WireBuffer::new(100, 20);
        render_missing(&mut buf, vp(100, 20), &MissingReason::NotOnPath);
        assert!(buffer_contains(&buf, "abtop not found"));
        assert!(buffer_contains(&buf, "no `abtop` binary on PATH"));
        // Universal cargo fallback is always present, regardless of OS.
        assert!(buffer_contains(&buf, "cargo install abtop"));
        // Repo URL is always present.
        assert!(buffer_contains(&buf, "github.com/graykode/abtop"));
        assert!(buffer_contains(&buf, "press r to re-check"));
        assert_eq!(buf.width, 100);
        assert_eq!(buf.height, 20);
    }

    #[test]
    fn render_ready_points_user_to_the_menu() {
        let mut buf = WireBuffer::new(80, 20);
        render_ready(&mut buf, vp(80, 20));
        assert!(buffer_contains(&buf, "abtop is installed"));
        assert!(buffer_contains(&buf, "press t"));
        assert!(buffer_contains(&buf, "full-screen"));
        // Docsite link present + untruncated at the 80-col minimum.
        assert!(buffer_contains(&buf, DOCSITE_URL));
    }

    #[test]
    fn empty_states_show_full_docsite_url_at_80_cols() {
        let mut miss = WireBuffer::new(80, 20);
        render_missing(&mut miss, vp(80, 20), &MissingReason::NotOnPath);
        assert!(
            buffer_contains(&miss, DOCSITE_URL),
            "docsite URL truncated/missing on abtop missing-state at 80 cols"
        );
    }

    #[test]
    fn render_missing_clamps_to_tiny_viewport_without_panic() {
        let mut buf = WireBuffer::new(10, 1);
        render_missing(&mut buf, vp(10, 1), &MissingReason::NotOnPath);
        assert_eq!(buf.width, 10);
        assert_eq!(buf.height, 1);
        let rows = flatten_rows_trimmed(&buf);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].starts_with("abtop not"));
    }

    #[test]
    fn render_missing_clamps_to_zero_height_no_panic() {
        let mut buf = WireBuffer::new(80, 0);
        render_missing(&mut buf, vp(80, 0), &MissingReason::NotOnPath);
        assert_eq!(painted_cell_count(&buf), 0, "0-height paints nothing");
    }

    #[test]
    fn render_missing_clamps_to_zero_width_no_panic() {
        let mut buf = WireBuffer::new(0, 20);
        render_missing(&mut buf, vp(0, 20), &MissingReason::NotOnPath);
        assert_eq!(painted_cell_count(&buf), 0, "0-width paints nothing");
    }

    #[test]
    fn render_ready_clamps_to_zero_viewport_no_panic() {
        let mut buf = WireBuffer::new(0, 0);
        render_ready(&mut buf, vp(0, 0));
        assert_eq!(painted_cell_count(&buf), 0, "0x0 paints nothing");
    }
}
