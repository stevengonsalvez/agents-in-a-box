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
/// Chrome band background (palette panel bg) — the tab bar and footer sit on
/// this so they read as fixed chrome, not floating text.
const BAND_BG: Color = Color::rgb(30, 30, 40);
/// Active-tab background block (palette list-highlight bg).
const ACTIVE_TAB_BG: Color = Color::rgb(40, 40, 60);

/// The primary tabs rendered in the top bar, in display order, each with its
/// switch hotkey. `Task` (hotkey `2`) is intentionally part of the strip even
/// though it is only reachable with a selection — it keeps the tab positions
/// stable so the eye doesn't jump when a task is opened.
///
/// The numbered hotkeys are **contiguous** (`1`→`4`, no gap): the old `[3]Agents`
/// tab was folded into the issue-list `[Agents]` filter chip, so `Skills` and
/// `Autopilots` shifted down to `3`/`4` to close the hole (e38.38). `Issues`/`Task`
/// keep their `1`/`2` muscle memory; only the two tabs that sat past the removed
/// `Agents` slot renumber, and only by one.
const PRIMARY_TABS: [(char, &str); 14] = [
    ('1', "Issues"),
    ('2', "Task"),
    ('3', "Skills"),
    ('4', "Autopilots"),
    ('K', "Kanban"),
    ('B', "Boards"),
    ('D', "Daemon"),
    ('U', "Usage"),
    ('L', "Logs"),
    ('I', "Inbox"),
    ('C', "Control"),
    ('S', "Squads"),
    ('P', "Profiles"),
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
/// Layout: `[1]Issues [2]Task [3]Skills [,]Settings` on the left, and
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

    // The bar sits on a full-width panel band so it reads as fixed chrome.
    fill_row(buf, row, area_w, BAND_BG);

    for (hotkey, label) in PRIMARY_TABS {
        let is_active = tab_is_active(active, hotkey);
        // `[k]Label` then a trailing space. The active tab gets a raised
        // highlight block + bold gold; inactive tabs recede in muted grey.
        let (bracket_c, key_c, label_c, bg) = if is_active {
            (GOLD, GOLD, GOLD, Some(ACTIVE_TAB_BG))
        } else {
            (MUTED_GRAY, GOLD, MUTED_GRAY, Some(BAND_BG))
        };
        x = put_str_ink(buf, x, row, "[", bracket_c, bg, is_active, area_w);
        x = put_str_ink(buf, x, row, &hotkey.to_string(), key_c, bg, true, area_w);
        x = put_str_ink(buf, x, row, "]", bracket_c, bg, is_active, area_w);
        x = put_str_ink(buf, x, row, label, label_c, bg, is_active, area_w);
        x = put_str_ink(buf, x, row, "  ", MUTED_GRAY, Some(BAND_BG), false, area_w);
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
    // Same panel band as the top bar, so the chrome frames the body top+bottom.
    fill_row(buf, row, area_w, BAND_BG);
    for (key, desc) in footer_hints(active) {
        x = put_str_ink(buf, x, row, key, GOLD, Some(BAND_BG), true, area_w);
        x = put_str_ink(buf, x, row, ":", MUTED_GRAY, Some(BAND_BG), false, area_w);
        x = put_str_ink(buf, x, row, desc, MUTED_GRAY, Some(BAND_BG), false, area_w);
        x = put_str_ink(buf, x, row, "  ", MUTED_GRAY, Some(BAND_BG), false, area_w);
        if x >= area_w {
            break;
        }
    }
}

/// The contextual + global key hints for `active`, in render order.
fn footer_hints(active: &Screen) -> Vec<(&'static str, &'static str)> {
    let mut hints: Vec<(&str, &str)> = match active {
        Screen::IssueList => {
            vec![
                ("a", "assign"),
                ("c", "create"),
                ("x", "delete"),
                ("/", "filter"),
            ]
        }
        Screen::TaskDetail(_) => vec![("R", "retry"), ("X", "cancel"), ("x", "delete")],
        Screen::AgentPicker(_) => vec![("enter", "assign"), ("esc", "close")],
        Screen::SkillManager => vec![("i", "import"), ("/", "filter")],
        Screen::Autopilots => vec![("a", "add"), ("r", "run"), ("d", "disable"), ("e", "edit")],
        Screen::Kanban => vec![("←→", "focus"), ("⇧←→", "move")],
        Screen::Boards => vec![
            ("↵", "run"),
            ("a", "attach"),
            ("n", "add col"),
            ("x", "del col"),
            ("m", "auto-move"),
        ],
        // Read-only panes with no footer hints: the daemon-health pane, the usage
        // dashboard (e38.35), and the logs pane (its level-filter chips
        // `a`/`i`/`w`/`e` carry their hints in the body next to each chip, not in
        // the footer).
        Screen::DaemonHealth | Screen::Usage | Screen::Logs => vec![],
        // The inbox surfaces its mark-all-read key (e38.14).
        Screen::Inbox => vec![("r", "mark read")],
        // The control center: navigate sessions + answer an ASK inline (P2). The
        // option / number-key answer hints render in the body next to the options.
        Screen::ControlCenter => vec![("j/k", "sessions"), ("enter", "answer")],
        // The squads screen: create a squad + edit membership + assign an issue
        // (P7). The `[c]/[a]/[d]/[x]` hints also render on the screen header row.
        Screen::Squads => vec![
            ("c", "create"),
            ("a", "add"),
            ("d", "remove"),
            ("x", "assign"),
        ],
        // The profile editor: navigate the roster + cycle the selected tier (P5).
        Screen::Profiles => vec![("j/k", "profiles"), ("t", "cycle tier")],
        Screen::Settings => vec![("n", "add key"), ("enter", "switch")],
        // The help overlay only needs the close hint; `?` is already pressed.
        Screen::Help => vec![("esc", "close")],
        // The command palette: Enter jumps, Esc closes (e38.13). `^P` is already
        // pressed to open it.
        Screen::CommandPalette => vec![("enter", "go"), ("esc", "close")],
    };
    // Global hints always trail. `^P` opens the command palette from any screen,
    // but not from within a modal (it would type a `p` into the palette query), so
    // suppress the hint there to avoid implying an unavailable binding.
    if !active.is_modal() {
        hints.push(("^P", "search"));
    }
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
        '3' => matches!(active, Screen::SkillManager),
        '4' => matches!(active, Screen::Autopilots),
        'K' => matches!(active, Screen::Kanban),
        'B' => matches!(active, Screen::Boards),
        'D' => matches!(active, Screen::DaemonHealth),
        'U' => matches!(active, Screen::Usage),
        'L' => matches!(active, Screen::Logs),
        'I' => matches!(active, Screen::Inbox),
        'C' => matches!(active, Screen::ControlCenter),
        'S' => matches!(active, Screen::Squads),
        'P' => matches!(active, Screen::Profiles),
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
    put_str_ink(buf, x, row, s, color, None, false, area_w)
}

/// [`put_str`] with full ink control: optional background and the BOLD wire
/// modifier (bit 1 — the runtime's ratatui interop maps it to
/// `Modifier::BOLD`).
#[allow(clippy::too_many_arguments)]
fn put_str_ink(
    buf: &mut WireBuffer,
    x: u16,
    row: u16,
    s: &str,
    color: Color,
    bg: Option<Color>,
    bold: bool,
    area_w: u16,
) -> u16 {
    let mut cx = x;
    for ch in s.chars() {
        if cx >= area_w {
            break;
        }
        let mut cell = Cell::new(ch.to_string());
        cell.fg = Some(color);
        cell.bg = bg;
        if bold {
            cell.modifier = 1;
        }
        buf.push(Coord::new(cx, row), cell);
        cx = cx.saturating_add(1);
    }
    cx
}

/// Fill an entire row with background-only spaces — the chrome band the tab
/// bar / footer text then paints over.
fn fill_row(buf: &mut WireBuffer, row: u16, area_w: u16, bg: Color) {
    for x in 0..area_w {
        let mut cell = Cell::new(" ");
        cell.bg = Some(bg);
        buf.push(Coord::new(x, row), cell);
    }
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
        assert!(tab_is_active(&Screen::SkillManager, '3'));
        assert!(tab_is_active(&Screen::Autopilots, '4'));
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

    /// The issue-list and task-detail footers both advertise the `x:delete`
    /// keybinding (63l.5) alongside their other hints.
    #[test]
    fn footer_advertises_delete_on_issue_list_and_detail() {
        assert!(
            footer_hints(&Screen::IssueList).contains(&("x", "delete")),
            "issue list footer must show x:delete"
        );
        let task = ainb_hangar_core::ids::TaskId::from_str("t1").unwrap();
        assert!(
            footer_hints(&Screen::TaskDetail(task)).contains(&("x", "delete")),
            "task-detail footer must show x:delete"
        );
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

    /// The top bar renders every tab hotkey + the workspace slug on row 0 at a
    /// realistic width. The six-tab strip (P8.4 added Kanban) needs >80 cols to
    /// also fit the right cluster; the slug-yields-to-tabs behaviour at the 80×24
    /// floor is covered by `chrome_renders_at_80x24_floor_without_overflow`.
    #[test]
    fn top_bar_renders_tabs_and_slug() {
        // Wide enough that the full fourteen-tab strip (P4 added `[B]Boards`, P7
        // `[S]Squads`, P5 `[P]Profiles` — ~155 cols) AND the right-side
        // workspace-slug cluster both fit; the tabs win width contention, so a
        // narrower buffer drops the slug (covered by the 80x24 floor smoke).
        let mut buf = WireBuffer::new(200, 24);
        render_top_bar(&mut buf, 200, &Screen::IssueList, "acme", Presence::Online);
        // Reconstruct row 0 text from the wire buffer cells.
        let row0 = row_text(&buf, 0, 200);
        assert!(row0.contains("Issues"), "row0 = {row0:?}");
        assert!(row0.contains("Skills"), "row0 = {row0:?}");
        assert!(row0.contains("Kanban"), "row0 = {row0:?}");
        assert!(row0.contains("Usage"), "row0 = {row0:?}");
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
            Screen::Autopilots,
            Screen::Kanban,
            Screen::Inbox,
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

    /// e38.38 — the numbered tabs render with NO numbering gap.
    ///
    /// The screenshotted bar showed `[1][2][4][5]…` — `[3]` was skipped after the
    /// old `[3]Agents` tab was folded into the issue-list filter chip, leaving a
    /// hole. This pins the fix: the digit hotkeys shown in the bar must form a
    /// contiguous run starting at `1` (so `[1][2][3][4]`, never a `[2]…[4]` jump).
    /// Re-introducing the gap (a `PRIMARY_TABS` digit that skips a value) makes the
    /// `expected` sequence diverge and the assertion fail.
    #[test]
    fn numbered_tabs_have_no_gap() {
        let mut buf = WireBuffer::new(120, 24);
        render_top_bar(&mut buf, 120, &Screen::IssueList, "acme", Presence::Online);
        let row0 = row_text(&buf, 0, 120);

        // Collect the digit hotkeys shown in `[N]` brackets, in display order.
        let digits = numbered_hotkeys(&row0);
        assert!(
            !digits.is_empty(),
            "no numbered tabs found in row0 = {row0:?}"
        );

        // They must be exactly 1, 2, 3, … with no gap (no `[2]` → `[4]` jump).
        let expected: Vec<u32> = (1..=digits.len())
            .map(|n| u32::try_from(n).expect("tab count fits u32"))
            .collect();
        assert_eq!(
            digits, expected,
            "numbered tabs are not contiguous (numbering gap) — row0 = {row0:?}"
        );
    }

    /// e38.38 — each digit shown in the bar still selects a real screen, and the
    /// screen it selects is the one labelled next to that digit in the bar.
    ///
    /// This is the "keep every tab reachable by its shown key" guard: renumbering
    /// the rendered label without remapping the routing key (or vice-versa) would
    /// land on the wrong screen (or no screen) and fail here.
    #[test]
    fn numbered_tab_keys_select_their_labelled_screen() {
        use crate::screen::{AppEvent, AppState, Screen as RouteScreen, reduce};
        use ainb_hangar_core::ids::{TaskId, WorkspaceId};

        let mut buf = WireBuffer::new(120, 24);
        render_top_bar(&mut buf, 120, &Screen::IssueList, "acme", Presence::Online);
        let row0 = row_text(&buf, 0, 120);

        for (digit, label) in numbered_tabs_with_labels(&row0) {
            let key = char::from_digit(digit, 10).unwrap();
            // Seed a selected task so the `2`→Task route is reachable; harmless
            // for the other digits.
            let mut state = AppState::new(WorkspaceId::from_str("default").unwrap());
            state.selected_task = Some(TaskId::from_str("task-1").unwrap());

            let out = reduce(&state, AppEvent::Key(key));
            let landed: &RouteScreen = &out.state.screen;
            assert_eq!(
                landed.tab_label(),
                label,
                "key `{key}` landed on `{}` but the bar labels it `{label}` — row0 = {row0:?}",
                landed.tab_label(),
            );
        }
    }

    /// Pull the digit hotkeys out of a rendered tab row in display order. A tab is
    /// `[N]Label`; this finds every `[<digit>]` and returns the digit values.
    fn numbered_hotkeys(row: &str) -> Vec<u32> {
        numbered_tabs_with_labels(row).into_iter().map(|(d, _)| d).collect()
    }

    /// Pull `(digit, label)` pairs out of a rendered tab row in display order,
    /// for every `[<digit>]Label` tab. The label runs until the next `[` or two
    /// trailing spaces (the tab separator).
    fn numbered_tabs_with_labels(row: &str) -> Vec<(u32, String)> {
        let chars: Vec<char> = row.chars().collect();
        let mut out = Vec::new();
        let mut i = 0;
        while i < chars.len() {
            // Look for `[<digit>]`.
            if chars[i] == '['
                && i + 2 < chars.len()
                && chars[i + 1].is_ascii_digit()
                && chars[i + 2] == ']'
            {
                let digit = chars[i + 1].to_digit(10).unwrap();
                // Read the label until the next `[` or a run of two spaces.
                let mut j = i + 3;
                let mut label = String::new();
                while j < chars.len() && chars[j] != '[' {
                    if chars[j] == ' ' && j + 1 < chars.len() && chars[j + 1] == ' ' {
                        break;
                    }
                    label.push(chars[j]);
                    j += 1;
                }
                out.push((digit, label.trim().to_string()));
                i = j;
            } else {
                i += 1;
            }
        }
        out
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
