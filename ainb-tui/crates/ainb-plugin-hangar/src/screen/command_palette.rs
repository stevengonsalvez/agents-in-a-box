//! e38.13 — Global command palette: the pure reducer + centred modal render.
//!
//! Opened with `Ctrl+P` from any screen, the command palette is a modal overlay
//! (centred, dim background) over whatever screen launched it. It is a global
//! cross-entity search: the user types a query, the plugin fires `hangar/search`
//! ([`crate::screen::PaletteAction`]), and the daemon answers with ranked
//! [`SearchEntry`]s across issues / agents / skills / autopilots. Enter on a
//! selected result raises [`CommandPaletteIntent::Navigate`] — the plugin glue
//! jumps to that entity's screen. Esc closes without navigating.
//!
//! As with every Hangar screen the reducer ([`reduce_command_palette`]) is
//! **pure**: it folds a key (or a results-loaded event) into a new
//! [`CommandPaletteState`] plus an optional intent for the plugin glue. The plugin
//! owns zero domain data — the results come from the daemon and are cached here
//! only for render.

use ainb_hangar_proto::snapshots::SearchEntry;
use ainb_plugin_sdk::{Cell, Color, Coord, WireBuffer};

use super::Screen;

/// The nine screens crisp B5 §2.5 took off the tab strip, each with the word
/// that reaches it here. The ONE list: the `Go:` palette rows, the Settings
/// `More screens` section and `palette_reaches_every_demoted_screen` all read it,
/// so a screen cannot be advertised in one place and unreachable in another.
///
/// Carries the [`Screen`] itself, not a wire token: the daemon's `SearchEntry`
/// route goes through a `screen: String` that an unknown value silently drops on
/// the floor, and a typo there is a stranded screen with no compile error. These
/// are plugin-local rows, so they carry the destination.
pub const GO_SCREENS: [(&str, Screen); 9] = [
    ("skills", Screen::SkillManager),
    ("autopilots", Screen::Autopilots),
    ("daemon", Screen::DaemonHealth),
    ("usage", Screen::Usage),
    ("logs", Screen::Logs),
    ("control", Screen::ControlCenter),
    ("fleet", Screen::Fleet),
    ("squads", Screen::Squads),
    ("profiles", Screen::Profiles),
];

/// Cornflower-blue modal border (ainb-tui chrome accent).
const BORDER: Color = Color::rgb(100, 149, 237);
/// Gold modal title.
const TITLE: Color = Color::rgb(255, 215, 0);
/// Muted kind-tag text (`[issue]`, `[skill]`, …).
const KIND_TAG: Color = Color::rgb(120, 120, 140);
/// Selected-row highlight.
const SELECTED: Color = Color::rgb(255, 255, 255);
/// Body text for an unselected row.
const BODY: Color = Color::rgb(200, 200, 210);
/// Modal panel background (the ainb-tui chrome band colour), so the screen the
/// palette overlays does not read through between its rows.
const PANEL_BG: Color = Color::rgb(30, 30, 40);

/// The render-state cache for the command-palette modal.
///
/// Holds the typed query, the latest ranked results from `hangar/search`, the
/// current selection within the result list, and a closed flag the router reads
/// to dismiss the modal. All fields are private; tests and the renderer read
/// through accessors.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CommandPaletteState {
    /// The current search query (typed text).
    query: String,
    /// The latest ranked results from `hangar/search`.
    results: Vec<SearchEntry>,
    /// Selection index into [`Self::results`].
    selected: usize,
    /// Set once Esc closed the modal; the router dismisses it.
    closed: bool,
}

impl CommandPaletteState {
    /// A fresh, empty palette (no query, no results, selection at the top).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The current query text.
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// The current ranked results.
    #[must_use]
    pub fn results(&self) -> &[SearchEntry] {
        &self.results
    }

    /// The `Go:` rows matching the current query, in [`GO_SCREENS`] order.
    ///
    /// Matched client-side on a plain lowercase substring — nine fixed words, so
    /// a scan is the whole algorithm — and merged AHEAD of the daemon's results:
    /// `^P usage` must land on the Usage screen, not on an issue whose title
    /// mentions usage. A bare `^P` lists all nine, so the palette doubles as the
    /// index of what left the tab strip.
    #[must_use]
    pub fn go_rows(&self) -> Vec<(&'static str, Screen)> {
        let query = self.query.to_lowercase();
        GO_SCREENS.into_iter().filter(|(word, _)| word.contains(&query)).collect()
    }

    /// The current selection index, over the `Go:` rows THEN the ranked results.
    #[must_use]
    pub const fn selected_index(&self) -> usize {
        self.selected
    }

    /// The selected `Go:` row's destination screen, if the selection sits on one.
    #[must_use]
    pub fn selected_go_screen(&self) -> Option<Screen> {
        self.go_rows().into_iter().nth(self.selected).map(|(_, screen)| screen)
    }

    /// The currently-selected daemon result, if the selection sits past the
    /// `Go:` rows and on one.
    #[must_use]
    pub fn selected_entry(&self) -> Option<&SearchEntry> {
        self.results.get(self.selected.checked_sub(self.go_rows().len())?)
    }

    /// Total rows on screen: the matching `Go:` rows plus the ranked results.
    fn row_count(&self) -> usize {
        self.go_rows().len() + self.results.len()
    }

    /// Whether the modal has been closed (Esc).
    #[must_use]
    pub const fn is_closed(&self) -> bool {
        self.closed
    }
}

/// An input the palette reducer folds into [`CommandPaletteState`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandPaletteEvent {
    /// A printable key (`'j'`-down/`'k'`-up are NOT modelled here — arrows move
    /// the selection; every printable char extends the query). `'\n'` navigates,
    /// `'\u{8}'` backspaces.
    Key(char),
    /// Move the selection down one result (Down arrow / `Ctrl+N`).
    SelectDown,
    /// Move the selection up one result (Up arrow / `Ctrl+P` repeat).
    SelectUp,
    /// Escape — closes the modal.
    Esc,
    /// Ranked results arrived from `hangar/search`; replace the cache and clamp
    /// the selection into the new list.
    ResultsLoaded(Vec<SearchEntry>),
}

/// A side-effect the plugin glue performs after a palette reduction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandPaletteIntent {
    /// The query changed; fire `hangar/search` with the new text (the plugin
    /// glue defers the RPC, then feeds the answer back as
    /// [`CommandPaletteEvent::ResultsLoaded`]).
    Search(String),
    /// Navigate to the selected entity (Enter on a result). Carries the
    /// jump-target screen token + the entity id + kind so the glue can switch the
    /// routing screen and select the row.
    Navigate {
        /// The screen-routing token to open (e.g. `"issue_list"`).
        screen: String,
        /// The selected entity's id.
        id: String,
        /// The selected entity's kind tag (e.g. `"issue"`).
        kind: String,
    },
    /// Enter on a `Go:` row: switch to that screen. No entity, no row to select —
    /// this is the replacement for the tab hotkey crisp B5 took away.
    GoToScreen(Screen),
}

/// The result of folding one [`CommandPaletteEvent`] into a [`CommandPaletteState`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandPaletteReduction {
    /// The next palette state.
    pub state: CommandPaletteState,
    /// A side-effect for the plugin glue, if any.
    pub intent: Option<CommandPaletteIntent>,
}

/// Fold one [`CommandPaletteEvent`] into `state`. Pure: no IO, no input mutation.
#[must_use]
pub fn reduce_command_palette(
    state: &CommandPaletteState,
    ev: CommandPaletteEvent,
) -> CommandPaletteReduction {
    match ev {
        CommandPaletteEvent::Esc => close(state),
        CommandPaletteEvent::SelectDown => move_down(state),
        CommandPaletteEvent::SelectUp => move_up(state),
        CommandPaletteEvent::ResultsLoaded(results) => load_results(state, results),
        CommandPaletteEvent::Key(c) => reduce_key(state, c),
    }
}

/// Handle a printable key: Enter navigates, Backspace trims (re-searching), any
/// other char extends the query (re-searching).
fn reduce_key(state: &CommandPaletteState, c: char) -> CommandPaletteReduction {
    match c {
        '\n' | '\r' => navigate(state),
        '\u{8}' | '\u{7f}' => {
            let mut next = state.clone();
            next.query.pop();
            search_after_edit(next)
        }
        _ => {
            let mut next = state.clone();
            next.query.push(c);
            search_after_edit(next)
        }
    }
}

/// After a query edit, emit a [`CommandPaletteIntent::Search`] for the new text so
/// the glue re-fires `hangar/search`. A blanked query searches for `""` (the
/// daemon answers an empty result, which clears the list on the next load).
///
/// The edit also re-filters the `Go:` rows, so the selection is clamped here too:
/// narrowing nine rows to one while the cursor sat on the ninth would otherwise
/// leave Enter pointing at a row that is no longer painted.
fn search_after_edit(mut state: CommandPaletteState) -> CommandPaletteReduction {
    clamp_selection(&mut state);
    let query = state.query.clone();
    CommandPaletteReduction {
        state,
        intent: Some(CommandPaletteIntent::Search(query)),
    }
}

/// Replace the results cache and clamp the selection into the new list.
fn load_results(state: &CommandPaletteState, results: Vec<SearchEntry>) -> CommandPaletteReduction {
    let mut next = state.clone();
    next.results = results;
    clamp_selection(&mut next);
    no_intent(next)
}

/// Pull the selection back onto a painted row after the row list shrank.
fn clamp_selection(state: &mut CommandPaletteState) {
    let rows = state.row_count();
    if state.selected >= rows {
        state.selected = rows.saturating_sub(1);
    }
}

/// Navigate on Enter: a `Go:` row switches screen, a daemon result jumps to its
/// entity. A no-op when nothing is selected (nothing to jump to).
fn navigate(state: &CommandPaletteState) -> CommandPaletteReduction {
    if let Some(screen) = state.selected_go_screen() {
        return CommandPaletteReduction {
            state: state.clone(),
            intent: Some(CommandPaletteIntent::GoToScreen(screen)),
        };
    }
    state.selected_entry().map_or_else(
        || unchanged(state),
        |entry| CommandPaletteReduction {
            state: state.clone(),
            intent: Some(CommandPaletteIntent::Navigate {
                screen: entry.screen.clone(),
                id: entry.id.clone(),
                kind: format!("{:?}", entry.kind).to_lowercase(),
            }),
        },
    )
}

/// Move the selection down one row, clamped to the last row.
fn move_down(state: &CommandPaletteState) -> CommandPaletteReduction {
    let mut next = state.clone();
    let max = next.row_count().saturating_sub(1);
    next.selected = (next.selected + 1).min(max);
    no_intent(next)
}

/// Move the selection up one result, clamped to the first result.
fn move_up(state: &CommandPaletteState) -> CommandPaletteReduction {
    let mut next = state.clone();
    next.selected = next.selected.saturating_sub(1);
    no_intent(next)
}

/// Close the modal (Esc), no intent.
fn close(state: &CommandPaletteState) -> CommandPaletteReduction {
    let mut next = state.clone();
    next.closed = true;
    no_intent(next)
}

/// A reduction that changes state but emits no intent.
const fn no_intent(state: CommandPaletteState) -> CommandPaletteReduction {
    CommandPaletteReduction {
        state,
        intent: None,
    }
}

/// A no-op reduction: state cloned unchanged, no intent.
fn unchanged(state: &CommandPaletteState) -> CommandPaletteReduction {
    no_intent(state.clone())
}

// ---------------------------------------------------------------------------
// Width-aware modal render
// ---------------------------------------------------------------------------

/// Render the palette as a centred modal over an `area_w` × `area_h` area.
///
/// The modal is ~70% wide / ~60% tall (clamped to fit the 80×24 floor), drawn
/// with a cornflower-blue border, a gold title showing the query, then one row
/// per ranked result (`[kind] label`), the selection highlighted.
pub fn render_command_palette(
    buf: &mut WireBuffer,
    area_w: u16,
    area_h: u16,
    state: &CommandPaletteState,
) {
    // `.max().min()`, not `.clamp()`: `u16::clamp` PANICS when min > max, and a
    // pane under 40x8 makes it so. Pre-existing, but the palette went from an
    // optional extra to the only route to nine screens, so a panic here is a
    // navigation dead end rather than a missing convenience.
    //
    // The intermediate is `u32` for the same reason: `area_w * 7` overflows a
    // `u16` above 9362 columns. Unreachable on a real terminal, but "this
    // function does not panic" is worth being total rather than nearly so.
    let scaled =
        |v: u16, num: u32, den: u32| u16::try_from(u32::from(v) * num / den).unwrap_or(u16::MAX);
    let modal_w = scaled(area_w, 7, 10).max(40).min(area_w);
    let modal_h = scaled(area_h, 6, 10).max(8).min(area_h);
    let x0 = (area_w.saturating_sub(modal_w)) / 2;
    let y0 = (area_h.saturating_sub(modal_h)) / 2;

    // Blank the interior BEFORE the border and rows. The palette paints only the
    // cells it fills, so the screen it overlays showed through between its rows:
    // `[go] Go: logs│             │` reads as one string of nonsense on an 80×24
    // pane. Harmless while the palette was empty until you typed; crisp B5 made
    // it the only route to nine screens, and it opens with nine rows.
    super::fleet::fill_background(
        buf,
        x0,
        y0,
        x0.saturating_add(modal_w),
        y0.saturating_add(modal_h),
        PANEL_BG,
    );
    draw_border(buf, x0, y0, modal_w, modal_h);
    // Title carries the live query so the user sees what they typed.
    let title = format!(" Search: {}_ ", state.query());
    put_str(buf, x0 + 2, y0, &title, TITLE, x0 + modal_w);

    let inner_x = x0 + 2;
    let inner_w = modal_w.saturating_sub(4);
    let first_row = y0 + 1;
    let bottom = y0 + modal_h - 1;

    // `Go:` rows first, then the daemon's ranked results — the same order the
    // selection index runs in, so the highlighted row is the one Enter takes.
    let rows = state
        .go_rows()
        .into_iter()
        .map(|(word, _)| ("[go] ".to_string(), format!("Go: {word}")))
        .chain(state.results().iter().map(|entry| {
            (
                format!("[{}] ", format!("{:?}", entry.kind).to_lowercase()),
                entry.label.clone(),
            )
        }));

    for (i, (tag, label)) in rows.enumerate() {
        // The row position is the first body row plus the row index; stop once
        // we'd paint into (or past) the bottom border.
        let row = first_row.saturating_add(u16::try_from(i).unwrap_or(u16::MAX));
        if row >= bottom {
            break;
        }
        let selected = i == state.selected_index();
        // `[kind]` tag (muted) then the label.
        put_str(buf, inner_x, row, &tag, KIND_TAG, inner_x + inner_w);
        let tag_w = u16::try_from(tag.chars().count()).unwrap_or(0);
        let label_color = if selected { SELECTED } else { BODY };
        put_str(
            buf,
            inner_x + tag_w,
            row,
            &label,
            label_color,
            inner_x + inner_w,
        );
    }
}

/// Draw a rectangle border of `w` × `h` at `(x0, y0)`.
fn draw_border(buf: &mut WireBuffer, x0: u16, y0: u16, w: u16, h: u16) {
    let x1 = x0 + w - 1;
    let y1 = y0 + h - 1;
    for x in x0..=x1 {
        put_char(buf, x, y0, '─', BORDER);
        put_char(buf, x, y1, '─', BORDER);
    }
    for y in y0..=y1 {
        put_char(buf, x0, y, '│', BORDER);
        put_char(buf, x1, y, '│', BORDER);
    }
    put_char(buf, x0, y0, '╭', BORDER);
    put_char(buf, x1, y0, '╮', BORDER);
    put_char(buf, x0, y1, '╰', BORDER);
    put_char(buf, x1, y1, '╯', BORDER);
}

/// Write `s` at `(x, row)` in `color`, clipping at column `right`.
fn put_str(buf: &mut WireBuffer, x: u16, row: u16, s: &str, color: Color, right: u16) {
    let mut cx = x;
    for ch in s.chars() {
        if cx >= right {
            break;
        }
        let mut cell = Cell::new(ch.to_string());
        cell.fg = Some(color);
        buf.push(Coord::new(cx, row), cell);
        cx = cx.saturating_add(1);
    }
}

/// Write one `ch` at `(x, row)` in `color`.
fn put_char(buf: &mut WireBuffer, x: u16, row: u16, ch: char, color: Color) {
    let mut cell = Cell::new(ch.to_string());
    cell.fg = Some(color);
    buf.push(Coord::new(x, row), cell);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_hangar_proto::snapshots::SearchEntryKind;

    fn entry(kind: SearchEntryKind, id: &str, label: &str) -> SearchEntry {
        SearchEntry {
            kind,
            id: id.into(),
            label: label.into(),
            screen: kind.target_screen().into(),
        }
    }

    /// Typing a char extends the query and raises a `Search` intent for the new
    /// text so the glue re-fires `hangar/search`.
    #[test]
    fn typing_extends_query_and_searches() {
        let s = CommandPaletteState::new();
        let out = reduce_command_palette(&s, CommandPaletteEvent::Key('a'));
        assert_eq!(out.state.query(), "a");
        assert_eq!(
            out.intent,
            Some(CommandPaletteIntent::Search("a".to_string()))
        );
    }

    /// Backspace trims the query and re-searches.
    #[test]
    fn backspace_trims_and_searches() {
        let mut s = CommandPaletteState::new();
        s = reduce_command_palette(&s, CommandPaletteEvent::Key('a')).state;
        s = reduce_command_palette(&s, CommandPaletteEvent::Key('b')).state;
        let out = reduce_command_palette(&s, CommandPaletteEvent::Key('\u{8}'));
        assert_eq!(out.state.query(), "a");
        assert_eq!(
            out.intent,
            Some(CommandPaletteIntent::Search("a".to_string()))
        );
    }

    /// A query no `Go:` word matches, so the reducer tests below address the
    /// daemon's results at index 0 the way they did before the `Go:` family.
    fn results_only(results: Vec<SearchEntry>) -> CommandPaletteState {
        let mut s = CommandPaletteState::new();
        for ch in "refactor".chars() {
            s = reduce_command_palette(&s, CommandPaletteEvent::Key(ch)).state;
        }
        assert!(s.go_rows().is_empty(), "`refactor` must match no Go row");
        reduce_command_palette(&s, CommandPaletteEvent::ResultsLoaded(results)).state
    }

    /// Loaded results populate the cache; the selection clamps into the new list.
    #[test]
    fn results_loaded_populates_and_clamps() {
        let mut s = results_only(vec![]);
        s = reduce_command_palette(
            &s,
            CommandPaletteEvent::ResultsLoaded(vec![
                entry(SearchEntryKind::Issue, "i1", "Refactor API"),
                entry(SearchEntryKind::Skill, "s1", "commit"),
            ]),
        )
        .state;
        assert_eq!(s.results().len(), 2);
        // Move down twice, then load a shorter list — selection clamps.
        s = reduce_command_palette(&s, CommandPaletteEvent::SelectDown).state;
        assert_eq!(s.selected_index(), 1);
        s = reduce_command_palette(
            &s,
            CommandPaletteEvent::ResultsLoaded(vec![entry(
                SearchEntryKind::Issue,
                "i1",
                "Refactor API",
            )]),
        )
        .state;
        assert_eq!(
            s.selected_index(),
            0,
            "selection clamps to the shorter list"
        );
    }

    /// Enter on a selected result raises a `Navigate` intent carrying the
    /// entity's screen, id, and kind.
    #[test]
    fn enter_navigates_to_selected_entity() {
        let mut s = results_only(vec![
            entry(SearchEntryKind::Issue, "i1", "Refactor API"),
            entry(SearchEntryKind::Skill, "s1", "commit"),
        ]);
        s = reduce_command_palette(&s, CommandPaletteEvent::SelectDown).state;
        let out = reduce_command_palette(&s, CommandPaletteEvent::Key('\n'));
        assert_eq!(
            out.intent,
            Some(CommandPaletteIntent::Navigate {
                screen: "skill_manager".to_string(),
                id: "s1".to_string(),
                kind: "skill".to_string(),
            }),
            "Enter jumps to the selected skill's screen"
        );
    }

    /// Enter with nothing selected is a no-op (nothing to jump to). A query that
    /// matches no `Go:` row and has no results is the only way to get there now:
    /// a bare palette always offers the nine screens.
    #[test]
    fn enter_with_no_rows_is_noop() {
        let s = results_only(vec![]);
        let out = reduce_command_palette(&s, CommandPaletteEvent::Key('\n'));
        assert_eq!(out.intent, None);
        assert_eq!(out.state, s);
    }

    /// Esc closes the modal.
    #[test]
    fn esc_closes() {
        let s = CommandPaletteState::new();
        let out = reduce_command_palette(&s, CommandPaletteEvent::Esc);
        assert!(out.state.is_closed());
        assert_eq!(out.intent, None);
    }

    /// The renderer paints the query in the title and one row per result, the
    /// selected row included.
    #[test]
    fn render_shows_query_and_results() {
        let mut s = CommandPaletteState::new();
        s = reduce_command_palette(&s, CommandPaletteEvent::Key('a')).state;
        s = reduce_command_palette(
            &s,
            CommandPaletteEvent::ResultsLoaded(vec![entry(
                SearchEntryKind::Issue,
                "i1",
                "Refactor API",
            )]),
        )
        .state;
        let mut buf = WireBuffer::new(80, 24);
        render_command_palette(&mut buf, 80, 24, &s);
        let text = buf_text(&buf);
        assert!(text.contains("Search: a"), "query in title: {text}");
        assert!(text.contains("Refactor API"), "result rendered: {text}");
        assert!(text.contains("[issue]"), "kind tag rendered: {text}");
    }

    /// Flatten a [`WireBuffer`] into a single string for substring assertions.
    fn buf_text(buf: &WireBuffer) -> String {
        buf.cells.iter().map(|(_, cell)| cell.symbol.clone()).collect()
    }

    /// Replay a [`WireBuffer`]'s paint order into a `w`×`h` grid of rows, so a
    /// test can assert on WHERE a glyph landed rather than merely that it exists.
    fn grid_of(buf: &WireBuffer, w: usize, h: usize) -> Vec<String> {
        let mut grid = vec![vec![' '; w]; h];
        for (coord, cell) in &buf.cells {
            let (x, y) = (coord.x as usize, coord.y as usize);
            if x < w && y < h {
                grid[y][x] = cell.symbol.chars().next().unwrap_or(' ');
            }
        }
        grid.into_iter().map(|row| row.into_iter().collect()).collect()
    }

    /// COVERAGE (crisp B5 §2.5): every screen with no tab is reachable by typing
    /// its word and pressing Enter.
    ///
    /// The demoted set is derived from the `Screen` enum and
    /// [`crate::chrome::tab_hotkey`] — "non-modal, and no tab" — NOT from
    /// [`GO_SCREENS`], which would make this tautological. Demote a tenth screen
    /// and it fails until it has a row; a count or a `len() >= 9` would pass while
    /// stranding one screen and advertising another twice.
    ///
    /// Driven through the reducer, not read off the table: the row has to survive
    /// the query filter, own the selection, and produce the intent that lands on
    /// THAT screen. `^P skills` reaching the Usage screen would pass a table read.
    #[test]
    fn palette_reaches_every_demoted_screen() {
        for screen in crate::test_support::every_screen() {
            if screen.is_modal() || crate::chrome::tab_hotkey(&screen).is_some() {
                continue;
            }
            let word = GO_SCREENS
                .into_iter()
                .find(|(_, target)| *target == screen)
                .unwrap_or_else(|| {
                    panic!("{screen:?} has no tab and no `Go:` row — it is stranded")
                })
                .0;
            // The word must NAME the screen, not merely exist. The lookup above
            // finds the row BY SCREEN, so swapping `("usage", Screen::Logs)` and
            // `("logs", Screen::Usage)` satisfies everything below while `^P usage`
            // opens Logs. The tab label is the name the rest of the UI already uses.
            assert_eq!(
                word,
                screen.tab_label().to_lowercase(),
                "the `Go:` word for {screen:?} does not name it"
            );
            // Type the word, then Enter, exactly as an operator would.
            let mut s = CommandPaletteState::new();
            for ch in word.chars() {
                s = reduce_command_palette(&s, CommandPaletteEvent::Key(ch)).state;
            }
            let out = reduce_command_palette(&s, CommandPaletteEvent::Key('\n'));
            assert_eq!(
                out.intent,
                Some(CommandPaletteIntent::GoToScreen(screen.clone())),
                "`^P {word}` must land on {screen:?}"
            );
        }
    }

    /// A bare `^P` lists all nine `Go:` rows above the results, so the palette is
    /// also the index of what left the tab strip — an operator who does not know a
    /// screen exists cannot type its name.
    #[test]
    fn an_empty_query_lists_every_go_row_ahead_of_the_results() {
        let mut s = CommandPaletteState::new();
        s = reduce_command_palette(
            &s,
            CommandPaletteEvent::ResultsLoaded(vec![entry(
                SearchEntryKind::Issue,
                "i1",
                "Refactor API",
            )]),
        )
        .state;
        assert_eq!(s.go_rows().len(), GO_SCREENS.len());

        let mut buf = WireBuffer::new(80, 24);
        render_command_palette(&mut buf, 80, 24, &s);
        let text = buf_text(&buf);
        for (word, _) in GO_SCREENS {
            assert!(
                text.contains(&format!("Go: {word}")),
                "missing `Go: {word}`"
            );
        }
        assert!(
            text.contains("Refactor API"),
            "results still render: {text}"
        );

        // Nothing of the screen underneath reads through BETWEEN the rows. The
        // palette paints `│` only on its two border columns, so an interior `│`
        // is the board's column rule showing through (it did, until the modal
        // learned to blank its own rectangle).
        let mut under = WireBuffer::new(80, 24);
        super::super::issue_list::render_issue_list(
            &mut under,
            80,
            1,
            23,
            &super::super::issue_list::IssueListState::default(),
            0,
        );
        assert!(
            grid_of(&under, 80, 24).iter().any(|row| row.contains('│')),
            "the board under the palette must have column rules to bleed"
        );
        render_command_palette(&mut under, 80, 24, &s);
        let modal_w = (80_u16 * 7 / 10).clamp(40, 80) as usize;
        let modal_h = (24_u16 * 6 / 10).clamp(8, 24) as usize;
        let (mx, my) = ((80 - modal_w) / 2, (24 - modal_h) / 2);
        for row in grid_of(&under, 80, 24).iter().skip(my + 1).take(modal_h - 2) {
            let interior: String = row.chars().skip(mx + 1).take(modal_w - 2).collect();
            assert!(
                !interior.contains('│'),
                "the board bled through the palette: {interior:?}"
            );
        }

        // The selection starts on the first Go row, and Enter takes it — the Go
        // family is merged AHEAD of the daemon's results, not after them.
        assert_eq!(
            reduce_command_palette(&s, CommandPaletteEvent::Key('\n')).intent,
            Some(CommandPaletteIntent::GoToScreen(GO_SCREENS[0].1.clone()))
        );
    }

    /// A pane below the modal's own minimum renders instead of panicking.
    ///
    /// `u16::clamp` panics when min > max, so `(w * 7 / 10).clamp(40, w)` blew up
    /// on anything under 40 columns. Below the 80×24 floor, but the palette is
    /// the only route to nine screens now, so it must degrade rather than take
    /// the process with it.
    #[test]
    fn a_pane_below_the_modal_minimum_renders_instead_of_panicking() {
        let s = CommandPaletteState::new();
        // Both ends: under the modal's own 40x8 minimum, where `clamp` panicked,
        // and past 9362 columns / 10922 rows, where the `u16` scale multiply
        // overflowed. One axis at a time — a pane huge on both would paint tens
        // of millions of cells and the test would OOM rather than assert.
        for (w, h) in [
            (1, 1),
            (20, 4),
            (39, 7),
            (40, 8),
            (80, 24),
            (9400, 24),
            (80, 11000),
        ] {
            let mut buf = WireBuffer::new(w, h);
            render_command_palette(&mut buf, w, h, &s);
            for (coord, _) in &buf.cells {
                assert!(coord.x < w && coord.y < h, "{w}x{h} painted out of bounds");
            }
        }
    }

    /// Narrowing the query past the selected row pulls the cursor back onto a
    /// painted one, so Enter never fires at a row that is no longer there.
    #[test]
    fn narrowing_the_query_clamps_the_selection_onto_a_painted_row() {
        let mut s = CommandPaletteState::new();
        for _ in 0..8 {
            s = reduce_command_palette(&s, CommandPaletteEvent::SelectDown).state;
        }
        assert_eq!(s.selected_index(), 8, "the ninth Go row");
        // `usage` matches exactly one row.
        for ch in "usage".chars() {
            s = reduce_command_palette(&s, CommandPaletteEvent::Key(ch)).state;
        }
        assert_eq!(s.go_rows().len(), 1);
        assert_eq!(s.selected_index(), 0);
        assert_eq!(
            reduce_command_palette(&s, CommandPaletteEvent::Key('\n')).intent,
            Some(CommandPaletteIntent::GoToScreen(Screen::Usage))
        );
    }
}
