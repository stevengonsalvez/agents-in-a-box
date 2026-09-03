//! Shared chrome: the top tab bar and bottom footer that wrap every screen.
//!
//! The chrome is the persistent frame around the Core 5 screens (P4.1). The
//! [top tab bar](render_top_bar) shows the seven primary tabs, the active
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
/// SEVEN, down from sixteen (crisp B5 §2.5). The strip was 138 columns and never
/// fitted the 80×24 floor, advertising a screen for every noun in the daemon;
/// these seven are the loop an operator actually runs (author, run, answer, read
/// the result). It measures 72 columns of ink (74 with the trailing separator),
/// pinned by `the_seven_tab_strip_fits_the_eighty_column_floor`.
///
/// The numbered hotkeys stay **contiguous** (`1`→`2`, no gap) and keep their
/// muscle memory; the nine demoted screens are reached with `^P` + their word
/// ([`crate::screen::command_palette::GO_SCREENS`]) and listed under Settings.
const PRIMARY_TABS: [(char, &str); 7] = [
    ('1', "Issues"),
    ('2', "Task"),
    ('K', "Runs"),
    ('B', "Boards"),
    ('I', "Inbox"),
    ('A', "Agents"),
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
/// Layout: `[1]Issues [2]Task [K]Runs … [,]Settings` on the left, and
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
///
/// Grammar (crisp B2 §2.6): at most FIVE contextual hints, then the globals —
/// `^P:search ?:help q:quit`, or the last two inside a modal, where `^P` would
/// type a `p` into the query. One `key:verb` pair per hint, the verb a single
/// lowercase word, and no two hints on a screen sharing a verb. Everything past
/// five goes to `?` — never to a second hint bar (the Boards screen carried 24
/// hints across two bars, which is how the audit found this).
pub(crate) fn footer_hints(active: &Screen) -> Vec<(&'static str, &'static str)> {
    let mut hints: Vec<(&str, &str)> = match active {
        // Nine to five: `y` activity, `s` sub-issue, `d` done and `f` facets keep
        // their bindings and their `?` lines, but stop competing for the eye with
        // the four verbs the board is actually for.
        Screen::IssueList => {
            vec![
                ("c", "create"),
                ("↵", "open"),
                ("a", "assign"),
                ("/", "filter"),
                ("x", "delete"),
            ]
        }
        // Five to four: `x` delete moves to the `task` row of `HELP_LINES`.
        // `X` cancel and `x` delete sat next to each other on one bar, one
        // keystroke apart, one of them irreversible.
        Screen::TaskDetail(_) => vec![
            ("↵", "expand"),
            ("R", "retry"),
            ("X", "cancel"),
            ("o", "PR"),
            ("a", "criterion"),
        ],
        Screen::AgentPicker(_) => vec![("enter", "assign"), ("esc", "close")],
        Screen::ActivityTimeline(_) => vec![("j/k", "scroll"), ("r", "refresh"), ("esc", "close")],
        Screen::SkillManager => vec![("i", "import"), ("/", "filter")],
        Screen::Autopilots => vec![("a", "add"), ("r", "run"), ("d", "disable"), ("e", "edit")],
        Screen::Kanban => vec![("←→", "focus"), ("⇧←→", "move"), ("R", "retry")],
        // The card verbs, and only those. Boards used to paint these five at the
        // bottom AND a sixteen-hint band at the top: two bars, overlapping
        // subsets, different keys for the same act. The band is gone (crisp B2
        // §2.6) and its column verbs live in `?`.
        Screen::Boards => vec![
            ("↵", "run"),
            ("a", "attach"),
            ("t", "timeline"),
            ("c", "card"),
            ("X", "cancel"),
        ],
        // Read-only panes with no footer hints: the daemon-health pane, the usage
        // dashboard (e38.35), and the logs pane (its level-filter chips
        // `a`/`i`/`w`/`e` carry their hints in the body next to each chip, not in
        // the footer).
        Screen::DaemonHealth | Screen::Usage | Screen::Logs => vec![],
        // The one attention surface (crisp B3 §2.4): answer an ASK inline, clear
        // the badge, narrow the list. The `h/l` option cursor and the `1`-`9`
        // direct picks render in the body next to the options they move.
        Screen::Inbox => vec![("enter", "answer"), ("r", "mark read"), ("f", "filter")],
        // The control center: navigate sessions + answer an ASK inline (P2). The
        // option / number-key answer hints render in the body next to the options.
        Screen::ControlCenter => vec![("j/k", "sessions"), ("enter", "answer")],
        Screen::Fleet => vec![
            ("j/k", "sessions"),
            ("b", "broadcast"),
            ("→/a", "attach"),
            ("s/r", "stop/restart"),
        ],
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
        // The Agents roster: create + delete a named agent (slice 2). The
        // `[n]/[x]` hints also render on the screen header row.
        Screen::Agents => vec![("n", "create"), ("x", "delete"), ("j/k", "agents")],
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

/// The tab-strip hotkey that lights up for `screen`, or `None` for a screen with
/// no tab: the nine crisp B5 demoted, and the four modals (which overlay a tab
/// rather than being one, so nothing lights while one is open).
///
/// Keyed off the SCREEN, not off the hotkey, so it is the one place that decides
/// what is on the strip. `tab_is_active` reads it, and
/// `every_tab_on_the_strip_is_reachable_and_labelled` pins it against both
/// [`PRIMARY_TABS`] and `ROUTER_KEYS` — the previous shape was a second
/// hotkey→screen table that could keep an arm for a tab the strip had dropped.
pub(crate) const fn tab_hotkey(screen: &Screen) -> Option<char> {
    match screen {
        Screen::IssueList => Some('1'),
        Screen::TaskDetail(_) => Some('2'),
        Screen::Kanban => Some('K'),
        Screen::Boards => Some('B'),
        Screen::Inbox => Some('I'),
        Screen::Agents => Some('A'),
        Screen::Settings => Some(','),
        // Demoted off the strip by crisp B5 §2.5; reached through `^P`.
        Screen::SkillManager
        | Screen::Autopilots
        | Screen::DaemonHealth
        | Screen::Usage
        | Screen::Logs
        | Screen::ControlCenter
        | Screen::Fleet
        | Screen::Squads
        | Screen::Profiles => None,
        // Modals overlay a tab; they are not one.
        Screen::AgentPicker(_) | Screen::ActivityTimeline(_) | Screen::Help
        | Screen::CommandPalette => None,
    }
}

/// Whether `hotkey`'s tab is the active one. `Task` (`2`) highlights for any
/// task-detail screen; the agent-picker modal keeps the screen *under* it
/// highlighted, so it falls through to no match (no tab lit) which is correct —
/// the modal is an overlay, not a tab.
const fn tab_is_active(active: &Screen, hotkey: char) -> bool {
    matches!(tab_hotkey(active), Some(key) if key == hotkey)
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
        assert!(tab_is_active(&Screen::Kanban, 'K'));
        assert!(tab_is_active(&Screen::Inbox, 'I'));
        assert!(tab_is_active(&Screen::Settings, ','));
        // A demoted screen lights no tab at all — the strip has none for it.
        assert!(!tab_is_active(&Screen::SkillManager, '3'));
        let issue = ainb_hangar_core::ids::IssueId::from_str("i1").unwrap();
        assert!(!tab_is_active(&Screen::AgentPicker(issue), '1'));
    }

    /// #450 HYGIENE: no per-screen footer hint may advertise a char the global
    /// router (or the host) eats first — such a hint is a lie: pressing it
    /// navigates away instead of doing what the footer says.
    ///
    /// The trailing GLOBAL hints (`^P`, `?`, `q`) are exempt: those ARE the
    /// reserved keys, advertised by the layer that owns them.
    ///
    /// Fails on `main`: the Fleet footer advertised `→/A:attach`, but bare `A`
    /// switches to the Agents tab.
    #[test]
    fn footer_hint_keys_never_collide_with_reserved_router_keys() {
        use crate::screen::router::is_reserved_key;
        // The globals the footer appends unconditionally — reserved BY DESIGN.
        let global = [("^P", "search"), ("?", "help"), ("q", "quit")];
        for active in every_screen() {
            for hint in footer_hints(&active) {
                if global.contains(&hint) {
                    continue;
                }
                // A hint key may be a compound label (`j/k`, `→/a`, `enter`);
                // check every single ASCII char token inside it.
                for token in hint.0.split('/') {
                    let mut chars = token.chars();
                    let (Some(ch), None) = (chars.next(), chars.next()) else {
                        continue;
                    };
                    assert!(
                        !is_reserved_key(ch),
                        "{active:?} footer advertises `{}` but `{ch}` is a reserved \
                         router/host key — the screen never sees it",
                        hint.0
                    );
                }
            }
        }
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

    /// The issue-list footer advertises `x:delete` (63l.5); the task-detail
    /// footer no longer does.
    ///
    /// Crisp B2 §2.6 keeps `x` in the issue list's five — deleting an issue is a
    /// board verb an operator reaches for — and drops it from task detail, where
    /// it sat one keystroke from `X:cancel` on the same bar. `x` still deletes
    /// there; it is documented in `?` instead of advertised beside the key that
    /// looks like it.
    #[test]
    fn footer_advertises_delete_on_the_issue_list_only() {
        assert!(
            footer_hints(&Screen::IssueList).contains(&("x", "delete")),
            "issue list footer must show x:delete"
        );
        let task = ainb_hangar_core::ids::TaskId::from_str("t1").unwrap();
        let detail = footer_hints(&Screen::TaskDetail(task));
        assert!(
            !detail.iter().any(|(key, _)| *key == "x"),
            "task detail must not advertise `x` beside `X:cancel`, got {detail:?}"
        );
        assert!(
            detail.contains(&("X", "cancel")),
            "the cancel it sat next to stays: {detail:?}"
        );
    }

    /// The issue-list footer is the FIVE board verbs (crisp B2 §2.6).
    ///
    /// `f:facets` was pinned here before and is deliberately not any more: it is
    /// the sixth of nine hints on a five-hint bar, it stays bound, and it stays
    /// documented in `HELP_LINES` (`help_overlay_screen_rows_pair_every_label_with_a_key`
    /// keeps that honest). The bar is what an operator scans mid-task; the panel
    /// is discovered once.
    #[test]
    fn issue_list_footer_is_the_five_board_verbs() {
        assert_eq!(
            footer_hints(&Screen::IssueList)
                .into_iter()
                .take_while(|hint| *hint != ("^P", "search"))
                .collect::<Vec<_>>(),
            vec![
                ("c", "create"),
                ("↵", "open"),
                ("a", "assign"),
                ("/", "filter"),
                ("x", "delete"),
            ],
        );
    }

    /// The hint-bar grammar (crisp B2 §2.6), over EVERY screen: at most five
    /// contextual hints, then exactly the three globals, and no two hints on one
    /// screen sharing a verb.
    ///
    /// A screen that grows a sixth hint fails here rather than shipping a bar an
    /// operator reads as noise — the audit counted twenty-four hints on Boards.
    #[test]
    fn every_footer_obeys_the_hint_grammar() {
        for active in every_screen() {
            let hints = footer_hints(&active);
            let globals: Vec<(&str, &str)> =
                hints.iter().copied().filter(|h| matches!(h.0, "^P" | "?" | "q")).collect();
            let contextual: Vec<(&str, &str)> =
                hints.iter().copied().filter(|h| !globals.contains(h)).collect();
            assert!(
                contextual.len() <= 5,
                "{active:?} paints {} contextual hints, the bar holds five: {contextual:?}",
                contextual.len()
            );
            // The globals trail, in order, and are the last thing on the bar.
            assert_eq!(
                &hints[contextual.len()..],
                globals.as_slice(),
                "{active:?} must end with its globals"
            );
            let mut verbs: Vec<&str> = contextual.iter().map(|h| h.1).collect();
            verbs.sort_unstable();
            let unique = verbs.len();
            verbs.dedup();
            assert_eq!(
                verbs.len(),
                unique,
                "{active:?} advertises one verb twice: {contextual:?}"
            );
        }
    }

    /// The three screens crisp B2 §2.6 rewrites hold the FULL grammar: every verb
    /// is one bare word, lowercase unless it is an acronym (`o:PR`).
    ///
    /// Scoped to those three on purpose. Three untouched screens still carry a
    /// two-word hint (`,` add key, `I` mark read, `P` cycle tier); they are named
    /// here rather than silently exempted, and sweeping them belongs with the B5
    /// help/keys pass that touches those screens.
    #[test]
    fn rewritten_footers_use_one_bare_word_per_verb() {
        let task = ainb_hangar_core::ids::TaskId::from_str("t1").unwrap();
        for active in [Screen::IssueList, Screen::TaskDetail(task), Screen::Boards] {
            for (key, verb) in footer_hints(&active) {
                if matches!(key, "^P" | "?" | "q") {
                    continue;
                }
                assert!(
                    !verb.contains(char::is_whitespace),
                    "{active:?} hint {key}:{verb} is not one word"
                );
                assert!(
                    verb == verb.to_lowercase() || verb == verb.to_uppercase(),
                    "{active:?} hint {key}:{verb} is neither lowercase nor an acronym"
                );
            }
        }
    }

    use crate::test_support::every_screen;

    /// COVERAGE (crisp B5 §2.5): the strip, the active-tab map and the router key
    /// set name exactly the same seven tabs.
    ///
    /// A tab is a promise of three things at once — it is painted, it lights up
    /// when you are on it, and its key gets you there. This walks every `Screen`
    /// variant and both directions of [`PRIMARY_TABS`], so shrinking the strip
    /// without shrinking [`tab_hotkey`] (a tab that never highlights), or without
    /// shrinking `ROUTER_KEYS` (a key the router no longer claims painted as if it
    /// worked), fails here. A count assertion would have passed for both.
    #[test]
    fn every_tab_on_the_strip_is_reachable_and_labelled() {
        use crate::screen::router::ROUTER_KEYS;
        for screen in every_screen() {
            let Some(hotkey) = tab_hotkey(&screen) else {
                assert!(
                    !PRIMARY_TABS.iter().any(|(_, label)| *label == screen.tab_label()),
                    "{screen:?} is painted on the strip but lights no tab"
                );
                continue;
            };
            assert!(
                PRIMARY_TABS.iter().any(|(key, _)| *key == hotkey),
                "{screen:?} claims tab `{hotkey}` but the strip does not paint it"
            );
            assert!(
                ROUTER_KEYS.contains(&hotkey),
                "the strip paints `{hotkey}` for {screen:?} but the router no longer claims it"
            );
        }
        // The other direction: no tab is painted for a screen that dropped out.
        for (hotkey, label) in PRIMARY_TABS {
            let owner = every_screen().into_iter().find(|s| tab_hotkey(s) == Some(hotkey));
            assert_eq!(
                owner.as_ref().map(Screen::tab_label),
                Some(label),
                "the strip paints `[{hotkey}]{label}` but no screen answers to it"
            );
        }
    }

    /// The seven-tab strip measures 74 columns and clears the 80×24 floor with
    /// every label intact — the sixteen-tab strip was 138 and truncated four tabs
    /// off the right edge at the floor (crisp B5 §2.5).
    ///
    /// The right-hand cluster still yields at 80, which §2.5 predicted it would
    /// not: the strip is 72 columns of ink and the cursor lands at 74 (each tab
    /// carries a two-space separator), so `default · ● online` at 18 columns needs
    /// 93. The tabs winning that contest is the documented rule
    /// (`right_cluster_yields_to_tabs_when_tight`); pinned here at both widths so
    /// the trade is a measured number rather than an assumption.
    #[test]
    fn the_seven_tab_strip_fits_the_eighty_column_floor() {
        let mut buf = WireBuffer::new(80, 24);
        render_top_bar(&mut buf, 80, &Screen::IssueList, "default", Presence::Online);
        let row0 = row_text(&buf, 0, 80);
        for (hotkey, label) in PRIMARY_TABS {
            assert!(
                row0.contains(&format!("[{hotkey}]{label}")),
                "`[{hotkey}]{label}` is cut off at the 80-column floor: {row0:?}"
            );
        }
        assert_eq!(row0.trim_end().chars().count(), 72, "row0 = {row0:?}");
        assert!(
            !row0.contains("online"),
            "the right cluster does not fit at 80 and must yield: {row0:?}"
        );

        // 93 columns is where it fits, and there it paints.
        let mut wide = WireBuffer::new(93, 24);
        render_top_bar(&mut wide, 93, &Screen::IssueList, "default", Presence::Online);
        let wide0 = row_text(&wide, 0, 93);
        assert!(
            wide0.contains("default · ● online"),
            "the cluster paints once there is room: {wide0:?}"
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
    /// realistic width. The width the strip needs is pinned separately by
    /// `the_seven_tab_strip_fits_the_eighty_column_floor`.
    #[test]
    fn top_bar_renders_tabs_and_slug() {
        // Wide enough that the seven-tab strip (74 cols, crisp B5 §2.5) AND the
        // right-side workspace-slug cluster both fit; the tabs win width
        // contention, so a narrower buffer drops the slug (covered by the 80x24
        // floor smoke).
        let mut buf = WireBuffer::new(200, 24);
        render_top_bar(&mut buf, 200, &Screen::IssueList, "acme", Presence::Online);
        // Reconstruct row 0 text from the wire buffer cells.
        let row0 = row_text(&buf, 0, 200);
        assert!(row0.contains("Issues"), "row0 = {row0:?}");
        assert!(row0.contains("Inbox"), "row0 = {row0:?}");
        assert!(row0.contains("Runs"), "row0 = {row0:?}");
        assert!(row0.contains("Settings"), "row0 = {row0:?}");
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
