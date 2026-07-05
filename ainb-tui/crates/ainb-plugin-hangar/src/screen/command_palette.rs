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

    /// The current selection index (into [`Self::results`]).
    #[must_use]
    pub const fn selected_index(&self) -> usize {
        self.selected
    }

    /// The currently-selected result, if any.
    #[must_use]
    pub fn selected_entry(&self) -> Option<&SearchEntry> {
        self.results.get(self.selected)
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
fn search_after_edit(state: CommandPaletteState) -> CommandPaletteReduction {
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
    if next.selected >= next.results.len() {
        next.selected = next.results.len().saturating_sub(1);
    }
    no_intent(next)
}

/// Navigate to the selected result (Enter), emitting the navigate intent. A no-op
/// when there is no result selected (nothing to jump to).
fn navigate(state: &CommandPaletteState) -> CommandPaletteReduction {
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

/// Move the selection down one result, clamped to the last result.
fn move_down(state: &CommandPaletteState) -> CommandPaletteReduction {
    let mut next = state.clone();
    let max = next.results.len().saturating_sub(1);
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
    let modal_w = (area_w * 7 / 10).clamp(40, area_w);
    let modal_h = (area_h * 6 / 10).clamp(8, area_h);
    let x0 = (area_w.saturating_sub(modal_w)) / 2;
    let y0 = (area_h.saturating_sub(modal_h)) / 2;

    draw_border(buf, x0, y0, modal_w, modal_h);
    // Title carries the live query so the user sees what they typed.
    let title = format!(" Search: {}_ ", state.query());
    put_str(buf, x0 + 2, y0, &title, TITLE, x0 + modal_w);

    let inner_x = x0 + 2;
    let inner_w = modal_w.saturating_sub(4);
    let first_row = y0 + 1;
    let bottom = y0 + modal_h - 1;

    for (i, entry) in state.results().iter().enumerate() {
        // The row position is the first body row plus the result index; stop once
        // we'd paint into (or past) the bottom border.
        let row = first_row.saturating_add(u16::try_from(i).unwrap_or(u16::MAX));
        if row >= bottom {
            break;
        }
        let selected = i == state.selected_index();
        // `[kind]` tag (muted) then the label.
        let tag = format!("[{}] ", format!("{:?}", entry.kind).to_lowercase());
        put_str(buf, inner_x, row, &tag, KIND_TAG, inner_x + inner_w);
        let tag_w = u16::try_from(tag.chars().count()).unwrap_or(0);
        let label_color = if selected { SELECTED } else { BODY };
        put_str(
            buf,
            inner_x + tag_w,
            row,
            &entry.label,
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

    /// Loaded results populate the cache; the selection clamps into the new list.
    #[test]
    fn results_loaded_populates_and_clamps() {
        let mut s = CommandPaletteState::new();
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
        let mut s = CommandPaletteState::new();
        s = reduce_command_palette(
            &s,
            CommandPaletteEvent::ResultsLoaded(vec![
                entry(SearchEntryKind::Issue, "i1", "Refactor API"),
                entry(SearchEntryKind::Skill, "s1", "commit"),
            ]),
        )
        .state;
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

    /// Enter with no results is a no-op (nothing to jump to).
    #[test]
    fn enter_with_no_results_is_noop() {
        let s = CommandPaletteState::new();
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
}
