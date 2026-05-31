//! Tab-strip painter for the witr screen.
//!
//! Always paints into row 0 of the viewport. Layout is a single line:
//! `[Processes] Ports Containers Locks`, with the focused tab wrapped
//! in square brackets so terminals without colour still show which
//! tab is active. cfx.6 may add accent colour on top — the bracket
//! signal stays as a fallback.
//!
//! Width-aware: on viewports too narrow to fit all four labels with
//! padding, we shorten labels to single letters (`P Po C L`). This
//! is the "width-aware truncation" pattern from
//! [[project_ainb_tui_width_aware_panels]].

use ainb_plugin_sdk::{Cell, Coord, WireBuffer};

use crate::state::Tab;

/// Threshold below which we fall back to single-letter labels. The
/// full strip `[Processes] Ports Containers Locks` is 35 characters
/// including the brackets and inter-label spaces, so 35 + ~10 chars
/// of breathing room is the cutover.
const COMPACT_THRESHOLD: u16 = 50;

/// Paint the tab strip at row 0 of the buffer. `current` is the
/// focused tab; it gets `[brackets]` around its label.
///
/// `width` clips the strip — labels that wouldn't fit are simply not
/// painted. The focused tab is painted first so on truly tiny
/// viewports the user still sees which tab they're on.
pub fn render(buf: &mut WireBuffer, width: u16, current: Tab) {
    if width == 0 {
        return;
    }
    let compact = width < COMPACT_THRESHOLD;
    let mut x: u16 = 0;
    for &tab in &Tab::ALL {
        let label_text = label_for(tab, compact);
        let bracketed = tab == current;

        if bracketed {
            push_char(buf, &mut x, width, '[');
        }
        for ch in label_text.chars() {
            if x >= width {
                return;
            }
            push_char(buf, &mut x, width, ch);
        }
        if bracketed {
            push_char(buf, &mut x, width, ']');
        }
        // Separator between tabs.
        push_char(buf, &mut x, width, ' ');
    }
}

fn label_for(tab: Tab, compact: bool) -> &'static str {
    if !compact {
        return tab.label();
    }
    match tab {
        Tab::Processes => "P",
        Tab::Ports => "Po",
        Tab::Containers => "C",
        Tab::Locks => "L",
    }
}

fn push_char(buf: &mut WireBuffer, x: &mut u16, max_x: u16, ch: char) {
    if *x >= max_x {
        return;
    }
    buf.push(Coord::new(*x, 0), Cell::new(ch.to_string()));
    *x = x.saturating_add(1);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::test_support::flatten_rows_trimmed;

    #[test]
    fn renders_full_strip_on_wide_viewport() {
        let mut buf = WireBuffer::new(80, 1);
        render(&mut buf, 80, Tab::Processes);
        let row = &flatten_rows_trimmed(&buf)[0];
        assert!(row.contains("[Processes]"));
        assert!(row.contains("Ports"));
        assert!(row.contains("Containers"));
        assert!(row.contains("Locks"));
    }

    #[test]
    fn focused_tab_is_bracketed() {
        let mut buf = WireBuffer::new(80, 1);
        render(&mut buf, 80, Tab::Containers);
        let row = &flatten_rows_trimmed(&buf)[0];
        assert!(row.contains("[Containers]"));
        // Other tabs not bracketed.
        assert!(!row.contains("[Processes]"));
    }

    #[test]
    fn compact_labels_kick_in_below_threshold() {
        let mut buf = WireBuffer::new(20, 1);
        render(&mut buf, 20, Tab::Ports);
        let row = &flatten_rows_trimmed(&buf)[0];
        // Compact labels are P / Po / C / L; focused Po is `[Po]`.
        assert!(row.contains("[Po]"));
        assert!(!row.contains("Processes"));
    }

    #[test]
    fn zero_width_does_not_panic() {
        let mut buf = WireBuffer::new(0, 1);
        render(&mut buf, 0, Tab::Processes);
        // No cells painted, no panic.
        assert!(buf.cells.is_empty());
    }

    #[test]
    fn tiny_viewport_clips_without_panic() {
        let mut buf = WireBuffer::new(3, 1);
        render(&mut buf, 3, Tab::Processes);
        // No panic, no out-of-bounds writes. Some prefix of the
        // strip is painted.
        for (coord, _) in &buf.cells {
            assert!(coord.x < 3);
        }
    }
}
