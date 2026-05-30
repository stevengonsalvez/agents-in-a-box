//! Shared chrome: the top tab bar and bottom footer that wrap every screen.
//!
//! The chrome is the persistent frame around the Core 5 screens (P4.1). The
//! [top tab bar](render_top_bar) shows the four primary tabs, the active
//! workspace slug, and an online/offline presence dot; the
//! [footer](render_footer) shows the contextual key hints for the active
//! screen. Both are **width-aware**: they derive their sub-region widths from
//! the supplied area rather than hard-coding columns, so they degrade
//! gracefully from a wide terminal down to the 80×24 floor without overflowing
//! (`project_ainb_tui_width_aware_panels`).
//!
//! Rendering targets the SDK [`WireBuffer`]; the plugin's `render` handler
//! composes the bar (row 0), the active screen body, and the footer (last row).

use ainb_plugin_sdk::{Cell, Color, Coord, WireBuffer};

use crate::screen::Screen;

/// Tab-bar / footer accent for the active tab and key letters.
const GOLD: Color = Color::rgb(255, 215, 0);
/// Muted text for inactive tabs and hint descriptions.
const MUTED_GRAY: Color = Color::rgb(120, 120, 140);
/// Soft white for the workspace slug and primary chrome text.
const SOFT_WHITE: Color = Color::rgb(220, 220, 230);
/// Presence dot colour when the daemon link is up.
const ONLINE_GREEN: Color = Color::rgb(100, 200, 120);
/// Presence dot colour when the daemon link is down.
const OFFLINE_RED: Color = Color::rgb(220, 80, 80);

/// The four primary tabs rendered in the top bar, in display order, each with
/// its switch hotkey. `Task` (hotkey `2`) is intentionally part of the strip
/// even though it is only reachable with a selection — it keeps the tab
/// positions stable so the eye doesn't jump when a task is opened.
const PRIMARY_TABS: [(char, &str); 4] = [
    ('1', "Issues"),
    ('2', "Task"),
    ('4', "Skills"),
    (',', "Settings"),
];

/// Online/offline presence shown in the top-right of the tab bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    /// Daemon link is up.
    Online,
    /// Daemon link is down.
    Offline,
}

impl Presence {
    /// `(glyph, label, colour)` for the presence dot.
    const fn parts(self) -> (char, &'static str, Color) {
        match self {
            Self::Online => ('●', "online", ONLINE_GREEN),
            Self::Offline => ('○', "offline", OFFLINE_RED),
        }
    }
}

/// Render the top tab bar onto row 0 of `buf`.
///
/// Layout: `[1]Issues [2]Task [4]Skills [,]Settings` on the left, and
/// `<workspace> · <dot> <presence>` flushed to the right. The right cluster is
/// dropped first when width is too tight to fit both — the tabs always win the
/// space contest so navigation stays visible at the 80×24 floor.
pub fn render_top_bar(
    buf: &mut WireBuffer,
    area_w: u16,
    active: &Screen,
    ws_slug: &str,
    presence: Presence,
) {
    let row = 0;
    let mut x: u16 = 0;

    for (hotkey, label) in PRIMARY_TABS {
        let is_active = tab_is_active(active, hotkey);
        // `[k]Label` then a trailing space.
        x = put_str(buf, x, row, "[", MUTED_GRAY, area_w);
        x = put_str(buf, x, row, &hotkey.to_string(), GOLD, area_w);
        x = put_str(buf, x, row, "]", MUTED_GRAY, area_w);
        let label_color = if is_active { GOLD } else { MUTED_GRAY };
        x = put_str(buf, x, row, label, label_color, area_w);
        x = put_str(buf, x, row, "  ", MUTED_GRAY, area_w);
    }

    // Right cluster: `<slug> · <dot> <presence>`. Only drawn if it fits in the
    // space remaining after the tabs (width-aware: tabs take priority).
    let (dot, presence_label, dot_color) = presence.parts();
    let right = format!("{ws_slug} · {dot} {presence_label}");
    let right_w = display_w(&right);
    if let Some(start) = right_start(area_w, x, right_w) {
        // Render slug + separator in soft white, the dot in its presence
        // colour, the presence word in soft white.
        let mut rx = put_str(
            buf,
            start,
            row,
            &format!("{ws_slug} · "),
            SOFT_WHITE,
            area_w,
        );
        rx = put_str(buf, rx, row, &dot.to_string(), dot_color, area_w);
        put_str(
            buf,
            rx,
            row,
            &format!(" {presence_label}"),
            SOFT_WHITE,
            area_w,
        );
    }
}

/// Render the footer key-hint bar onto the last row of `buf`.
///
/// Hints are screen-contextual: global hints (`?:help q:quit`) always show;
/// the active screen prepends its own (`feedback_keybinding_hints_near_control`
/// — letters next to the control they affect). Truncated on the right when the
/// area is too narrow rather than wrapping.
pub fn render_footer(buf: &mut WireBuffer, area_w: u16, area_h: u16, active: &Screen) {
    let row = area_h.saturating_sub(1);
    let mut x: u16 = 0;
    for (key, desc) in footer_hints(active) {
        x = put_str(buf, x, row, key, GOLD, area_w);
        x = put_str(buf, x, row, ":", MUTED_GRAY, area_w);
        x = put_str(buf, x, row, desc, MUTED_GRAY, area_w);
        x = put_str(buf, x, row, "  ", MUTED_GRAY, area_w);
        if x >= area_w {
            break;
        }
    }
}

/// The contextual + global key hints for `active`, in render order.
fn footer_hints(active: &Screen) -> Vec<(&'static str, &'static str)> {
    let mut hints: Vec<(&str, &str)> = match active {
        Screen::IssueList => vec![("a", "assign"), ("c", "create"), ("/", "filter")],
        Screen::TaskDetail(_) => vec![("R", "retry"), ("X", "cancel")],
        Screen::AgentPicker(_) => vec![("enter", "assign"), ("esc", "close")],
        Screen::SkillManager => vec![("i", "import"), ("/", "filter")],
        Screen::Settings => vec![("n", "add key"), ("enter", "switch")],
        // The help overlay only needs the close hint; `?` is already pressed.
        Screen::Help => vec![("esc", "close")],
    };
    // Global hints always trail.
    hints.push(("?", "help"));
    hints.push(("q", "quit"));
    hints
}

/// Whether `hotkey`'s tab is the active one. `Task` (`2`) highlights for any
/// task-detail screen; the agent-picker modal keeps the screen *under* it
/// highlighted, so it falls through to no match (no tab lit) which is correct —
/// the modal is an overlay, not a tab.
const fn tab_is_active(active: &Screen, hotkey: char) -> bool {
    match hotkey {
        '1' => matches!(active, Screen::IssueList),
        '2' => matches!(active, Screen::TaskDetail(_)),
        '4' => matches!(active, Screen::SkillManager),
        ',' => matches!(active, Screen::Settings),
        _ => false,
    }
}

/// Column at which the right cluster should start, or `None` if it doesn't fit
/// after the tabs (leaving at least one space of gap).
fn right_start(area_w: u16, tabs_end_x: u16, right_w: u16) -> Option<u16> {
    let start = area_w.checked_sub(right_w)?;
    // Require a one-column gap between the tabs and the right cluster.
    if start > tabs_end_x {
        Some(start)
    } else {
        None
    }
}

/// Display width of a string in terminal cells (char count; the chrome strings
/// are single-width glyphs, so this is exact for our inputs).
fn display_w(s: &str) -> u16 {
    u16::try_from(s.chars().count()).unwrap_or(u16::MAX)
}

/// Write `s` at `(x, row)` in `color`, clipping at `area_w`. Returns the next
/// free column. Safe on multi-byte chars (iterates `char`s, not bytes).
fn put_str(buf: &mut WireBuffer, x: u16, row: u16, s: &str, color: Color, area_w: u16) -> u16 {
    let mut cx = x;
    for ch in s.chars() {
        if cx >= area_w {
            break;
        }
        let mut cell = Cell::new(ch.to_string());
        cell.fg = Some(color);
        buf.push(Coord::new(cx, row), cell);
        cx = cx.saturating_add(1);
    }
    cx
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Active-tab detection lights exactly the matching tab and nothing for a
    /// modal overlay (the underlying tab stays lit, the modal isn't a tab).
    #[test]
    fn active_tab_detection() {
        assert!(tab_is_active(&Screen::IssueList, '1'));
        assert!(!tab_is_active(&Screen::IssueList, '2'));
        assert!(tab_is_active(&Screen::SkillManager, '4'));
        assert!(tab_is_active(&Screen::Settings, ','));
        let issue = ainb_hangar_core::ids::IssueId::from_str("i1").unwrap();
        assert!(!tab_is_active(&Screen::AgentPicker(issue), '1'));
    }

    /// Footer always ends with the global `?:help q:quit` hints regardless of
    /// the active screen.
    #[test]
    fn footer_always_has_global_hints() {
        for active in [Screen::IssueList, Screen::SkillManager, Screen::Settings] {
            let hints = footer_hints(&active);
            assert!(hints.contains(&("?", "help")));
            assert!(hints.contains(&("q", "quit")));
        }
    }

    /// The right cluster yields to the tabs: when there isn't room after the
    /// tabs it is dropped entirely (returns `None`), never overlapping.
    #[test]
    fn right_cluster_yields_to_tabs_when_tight() {
        // Plenty of room: cluster starts near the right edge.
        assert_eq!(right_start(80, 30, 18), Some(62));
        // No room (tabs already consume past where the cluster would start).
        assert_eq!(right_start(40, 30, 18), None);
        // Exactly touching (no gap) is rejected.
        assert_eq!(right_start(40, 22, 18), None);
    }

    /// The top bar renders the tab hotkeys onto row 0 without panicking at the
    /// 80×24 floor, and the issue-list tab label is present.
    #[test]
    fn top_bar_renders_at_floor_width() {
        let mut buf = WireBuffer::new(80, 24);
        render_top_bar(&mut buf, 80, &Screen::IssueList, "acme", Presence::Online);
        // Reconstruct row 0 text from the wire buffer cells.
        let row0 = row_text(&buf, 0, 80);
        assert!(row0.contains("Issues"), "row0 = {row0:?}");
        assert!(row0.contains("Skills"), "row0 = {row0:?}");
        assert!(row0.contains("acme"), "row0 = {row0:?}");
    }

    /// 80×24 floor smoke (P4.md:537): the chrome renders bar + footer at the
    /// minimum supported size without writing a single cell outside the area
    /// bounds. This is the explicit floor guard the cross-cutting requirement
    /// mandates — it would catch any future hint/tab string that overflowed at
    /// the floor, not just substrings at one width.
    #[test]
    fn chrome_renders_at_80x24_floor_without_overflow() {
        const FLOOR_W: u16 = 80;
        const FLOOR_H: u16 = 24;

        let mut buf = WireBuffer::new(FLOOR_W, FLOOR_H);
        // The cross-cutting min-size contract: the area we render into is at or
        // above the 80×24 floor (asserted on the buffer, not bare literals).
        assert!(buf.width >= 80 && buf.height >= 24);
        // Exercise every screen variant so each footer-hint set is floor-checked,
        // including the longest (modal/help) hint strings.
        let issue = ainb_hangar_core::ids::IssueId::from_str("i1").unwrap();
        let task = ainb_hangar_core::ids::TaskId::from_str("t1").unwrap();
        for screen in [
            Screen::IssueList,
            Screen::TaskDetail(task),
            Screen::AgentPicker(issue),
            Screen::SkillManager,
            Screen::Settings,
            Screen::Help,
        ] {
            render_top_bar(&mut buf, FLOOR_W, &screen, "acme", Presence::Online);
            render_footer(&mut buf, FLOOR_W, FLOOR_H, &screen);
        }

        // No cell may land outside the 80×24 area.
        for (coord, _) in &buf.cells {
            assert!(
                coord.x < FLOOR_W && coord.y < FLOOR_H,
                "chrome wrote out-of-bounds cell at ({}, {})",
                coord.x,
                coord.y,
            );
        }
    }

    /// Presence parts map online → green dot, offline → red dot.
    #[test]
    fn presence_parts_map_state() {
        assert_eq!(Presence::Online.parts().2, ONLINE_GREEN);
        assert_eq!(Presence::Offline.parts().2, OFFLINE_RED);
    }

    /// Read a row's text back out of a `WireBuffer` for assertion. Cells not
    /// written render as spaces.
    fn row_text(buf: &WireBuffer, row: u16, width: u16) -> String {
        let mut out = vec![' '; width as usize];
        for (coord, cell) in &buf.cells {
            if coord.y == row && coord.x < width {
                if let Some(ch) = cell.symbol.chars().next() {
                    out[coord.x as usize] = ch;
                }
            }
        }
        out.into_iter().collect()
    }
}
