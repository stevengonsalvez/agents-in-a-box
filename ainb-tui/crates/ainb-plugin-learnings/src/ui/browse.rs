//! Browse tab — scrollable record list + typed filter chips.
//!
//! The chip bar shows one chip per facet
//! (`scope` / `conf` / `category` / `source` / `project`). v1 wires the
//! `scope` chip to the `f` key: each press cycles the active scope value
//! through `* → <facet values sorted> → *`, narrowing the list via the data
//! layer's [`Filter`]. The other chips render their `*` (all) state as
//! placeholders for the per-facet cycling that lands alongside Detail (P6).
//!
//! Selection (`↑↓` / `jk`) moves a `▶` indicator down the *filtered* list.
//! A status line reports `N notes failed to parse` when the last scan skipped
//! corrupt notes (the P4-review carry-in).

use ratatui::buffer::Buffer as RBuffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect as RRect};
use ratatui::style::{Modifier as RModifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Row, Table, Widget};

use ainb_plugin_sdk::KeyCode;

use super::{GOLD, LIST_HIGHLIGHT_BG, MUTED_GRAY, SELECTION_GREEN, SOFT_WHITE};
use crate::data::{Filter, FilterField, LearningRecord};

/// The facet the `f` key cycles in v1. Other facets render as `*` chips until
/// their cycling lands (P6).
pub(crate) const SCOPE_FACET: FilterField = FilterField::Scope;

/// Browse-tab UI state: the selected row + the active scope-filter cursor.
///
/// `scope_idx == 0` means no scope filter (chip shows `*`). Index `n > 0`
/// selects `scope_values[n - 1]` from the distinct, sorted scope facet values
/// derived from the records at cycle time. `active_scope` caches the resolved
/// value so [`Self::filter`] is a pure read.
#[derive(Debug, Default)]
pub struct BrowseState {
    /// Selected row index into the *filtered* list.
    selected: usize,
    /// Active scope-filter cursor (0 = all). See struct docs.
    scope_idx: usize,
    /// Resolved active scope value (`None` = all), kept in lock-step with
    /// `scope_idx` so `filter()` needs no record access.
    active_scope: Option<String>,
}

impl BrowseState {
    /// The active filter derived from the chip cursors. Empty (keeps all) when
    /// the scope cursor is at `*`.
    pub fn filter(&self) -> Filter {
        match &self.active_scope {
            Some(scope) => Filter::new().with(SCOPE_FACET, scope.clone()),
            None => Filter::new(),
        }
    }

    /// The record under the selection in the *filtered* list, if any. Used by
    /// the shell to open the Detail pane (`Enter`) on the highlighted row.
    /// Returns `None` when the filtered list is empty.
    #[must_use]
    pub fn selected_record<'a>(&self, records: &'a [LearningRecord]) -> Option<&'a LearningRecord> {
        let filtered = self.filter().apply(records);
        filtered.get(self.selected).copied()
    }

    /// Clamp the selection to the current visible row count (called after a
    /// filter change or a fresh scan shrinks the list).
    pub fn clamp_selection(&mut self, visible: usize) {
        if visible == 0 {
            self.selected = 0;
        } else if self.selected >= visible {
            self.selected = visible - 1;
        }
    }

    /// Route a Browse-tab key. Returns `true` when state changed.
    pub fn handle_key(&mut self, code: &KeyCode, records: &[LearningRecord]) -> bool {
        match code {
            // `f`: cycle the scope chip through `* → values… → *`.
            KeyCode::Char { ch: 'f' } => {
                self.cycle_scope(records);
                self.clamp_selection(self.filter().apply(records).len());
                true
            }
            // Selection down.
            KeyCode::Down | KeyCode::Char { ch: 'j' } => {
                let visible = self.filter().apply(records).len();
                if visible > 0 && self.selected + 1 < visible {
                    self.selected += 1;
                    return true;
                }
                false
            }
            // Selection up.
            KeyCode::Up | KeyCode::Char { ch: 'k' } => {
                if self.selected > 0 {
                    self.selected -= 1;
                    return true;
                }
                false
            }
            _ => false,
        }
    }

    /// Advance the scope cursor one step through the distinct sorted scope
    /// values present in `records`, wrapping back to "all" after the last.
    fn cycle_scope(&mut self, records: &[LearningRecord]) {
        let values = distinct_scopes(records);
        if values.is_empty() {
            self.scope_idx = 0;
            self.active_scope = None;
            return;
        }
        // Positions: 0 = all, 1..=len = values[idx-1].
        self.scope_idx = (self.scope_idx + 1) % (values.len() + 1);
        self.active_scope = if self.scope_idx == 0 {
            None
        } else {
            Some(values[self.scope_idx - 1].clone())
        };
        self.selected = 0;
    }
}

/// Canonical scope value cycled first when present. `universal` learnings are
/// the broadest, most-common facet, so the first `f` press narrows to them —
/// matching the design mock (`scope[univ]`).
const PRIMARY_SCOPE: &str = "universal";

/// Distinct scope facet values present in the records, in a stable cycle
/// order: [`PRIMARY_SCOPE`] first (when present), then the remaining values
/// alphabetically. Deterministic so the chip cycle is reproducible across runs
/// and the tripwire can lock an exact `scope[<value>]` token.
fn distinct_scopes(records: &[LearningRecord]) -> Vec<String> {
    let mut scopes: Vec<String> = records.iter().map(|r| r.scope.clone()).collect();
    scopes.sort();
    scopes.dedup();
    // Hoist the primary scope to the front so a single `f` lands on it.
    if let Some(pos) = scopes.iter().position(|s| s == PRIMARY_SCOPE) {
        let primary = scopes.remove(pos);
        scopes.insert(0, primary);
    }
    scopes
}

/// Render the Browse tab: chip bar, then the filtered list, then the status
/// line.
pub fn render(
    buf: &mut RBuffer,
    area: RRect,
    records: &[LearningRecord],
    failed_count: usize,
    state: &BrowseState,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // chip bar
            Constraint::Min(1),    // list
            Constraint::Length(1), // status line
            Constraint::Length(1), // help / key-hint bar
        ])
        .split(area);

    render_chip_bar(buf, rows[0], state);

    let filtered: Vec<&LearningRecord> = state.filter().apply(records);
    render_list(buf, rows[1], &filtered, state.selected);

    render_status(buf, rows[2], failed_count, filtered.len());
    render_help_bar(buf, rows[3]);
}

/// Render the typed filter-chip bar:
///   `filters: scope[<v>] conf[*] category[*] source[*] project[*]`
/// The active scope chip shows its concrete value (gold); inactive chips show
/// `*` (muted). The `scope[<v>]` token is asserted verbatim by the tests.
fn render_chip_bar(buf: &mut RBuffer, area: RRect, state: &BrowseState) {
    let scope_val = state.active_scope.as_deref().unwrap_or("*");
    let scope_active = state.active_scope.is_some();

    let mut spans = vec![Span::styled("filters: ", Style::default().fg(MUTED_GRAY))];
    spans.push(chip_span("scope", scope_val, scope_active));
    spans.push(Span::raw(" "));
    spans.push(chip_span("conf", "*", false));
    spans.push(Span::raw(" "));
    spans.push(chip_span("category", "*", false));
    spans.push(Span::raw(" "));
    spans.push(chip_span("source", "*", false));
    spans.push(Span::raw(" "));
    spans.push(chip_span("project", "*", false));

    Paragraph::new(Line::from(spans)).render(area, buf);
}

/// Render the bottom help / key-hint bar (mandatory ainb-tui style-guide
/// pattern; design mock C3 shows it verbatim).
///
/// Mirrors the burndown footer-span shape — `<key> <description>` pairs — and
/// stays **honest**: every key here is now wired, so all render gold (live):
/// `↑↓ move`, `f filter`, `Tab pane`, `⏎ open` (P6), `/ search` (P7), and
/// `g graph` (P8 — `g` jumps to the Graph tab + focuses the entity view).
fn render_help_bar(buf: &mut RBuffer, area: RRect) {
    let mut spans = vec![Span::raw(" ")];
    // Live keys (gold key + muted description).
    spans.extend(help_key("↑↓", "move"));
    spans.extend(help_key("f", "filter"));
    spans.extend(help_key("Tab", "pane"));
    // `⏎ open` is live (P6): Enter opens the Detail pane on the selected row.
    spans.extend(help_key("⏎", "open"));
    // `/ search` is live (P7): `/` jumps to the Search tab + query box.
    spans.extend(help_key("/", "search"));
    // `g graph` is live now (P8): `g` jumps to the Graph tab + entity focus.
    spans.extend(help_key("g", "graph"));
    Paragraph::new(Line::from(spans)).render(area, buf);
}

/// A live help-bar entry: gold key glyph + muted description, joined by a
/// single space so the render carries the exact `<key> <desc>` token the
/// tests + tripwire assert (e.g. `f filter`).
fn help_key(key: &str, desc: &str) -> [Span<'static>; 3] {
    [
        Span::styled(
            key.to_string(),
            Style::default().fg(GOLD).add_modifier(RModifier::BOLD),
        ),
        Span::styled(format!(" {desc}"), Style::default().fg(MUTED_GRAY)),
        Span::styled("  ", Style::default().fg(MUTED_GRAY)),
    ]
}

/// One `name[value]` chip. Active chips are gold-bold; inactive are muted.
fn chip_span(name: &str, value: &str, active: bool) -> Span<'static> {
    let style = if active {
        Style::default().fg(GOLD).add_modifier(RModifier::BOLD)
    } else {
        Style::default().fg(MUTED_GRAY)
    };
    Span::styled(format!("{name}[{value}]"), style)
}

/// Render the (filtered) record list as a 3-column table: selection marker,
/// id, confidence. The selected row carries a `▶` marker + highlight.
fn render_list(buf: &mut RBuffer, area: RRect, filtered: &[&LearningRecord], selected: usize) {
    if filtered.is_empty() {
        Paragraph::new(Line::from(Span::styled(
            "  (no learnings match the active filter)",
            Style::default().fg(MUTED_GRAY).add_modifier(RModifier::ITALIC),
        )))
        .render(area, buf);
        return;
    }

    let rows: Vec<Row> = filtered
        .iter()
        .enumerate()
        .map(|(i, rec)| {
            let is_sel = i == selected;
            let marker = if is_sel { "▶" } else { " " };
            let style = if is_sel {
                Style::default()
                    .fg(SELECTION_GREEN)
                    .bg(LIST_HIGHLIGHT_BG)
                    .add_modifier(RModifier::BOLD)
            } else {
                Style::default().fg(SOFT_WHITE)
            };
            Row::new(vec![
                Span::styled(marker, style),
                Span::styled(rec.id.clone(), style),
                Span::styled(format!("{:.1}", rec.confidence), style),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(2),
            Constraint::Min(20),
            Constraint::Length(5),
        ],
    );
    Widget::render(table, area, buf);
}

/// Render the bottom status line. Shows `N notes failed to parse` when the
/// last scan skipped corrupt notes; otherwise the visible / count summary.
fn render_status(buf: &mut RBuffer, area: RRect, failed_count: usize, visible: usize) {
    let mut spans = vec![Span::styled(
        format!("  {visible} shown"),
        Style::default().fg(MUTED_GRAY),
    )];
    if failed_count > 0 {
        let noun = if failed_count == 1 { "note" } else { "notes" };
        spans.push(Span::styled("  ·  ", Style::default().fg(MUTED_GRAY)));
        spans.push(Span::styled(
            format!("{failed_count} {noun} failed to parse"),
            Style::default().fg(GOLD),
        ));
    }
    Paragraph::new(Line::from(spans)).render(area, buf);
}
