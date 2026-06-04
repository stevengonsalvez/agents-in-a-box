//! Tabbed UI shell for the learnings browser.
//!
//! Owns the tab state ([`Tab`]: Browse | Search | Graph), the parsed-record
//! store, and the key routing that the plugin's `handle_key` delegates to.
//! Browse is implemented here (P5); Search (P7) and Graph (P8) render a
//! "coming soon" placeholder behind their stable [`Tab`] arms so this
//! dispatch table never has to change when they land.
//!
//! Render path mirrors burndown: paint locally into a ratatui `Buffer`
//! through [`render`], then the plugin converts the buffer to a `WireBuffer`
//! cell stream for the host. Input flows through [`LearningsUi::handle_key`],
//! which returns `true` when the key mutated state (so the plugin bumps its
//! render generation).

mod browse;
mod detail;

use ratatui::buffer::Buffer as RBuffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect as RRect};
use ratatui::style::{Color as RColor, Modifier as RModifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Tabs, Widget};

use ainb_plugin_sdk::KeyCode;

use crate::data::LearningRecord;

use browse::BrowseState;
use detail::DetailState;

/// TUI palette — see `../.claude/skills/tui-screen/SKILL.md`.
pub(crate) const GOLD: RColor = RColor::Rgb(255, 215, 0);
pub(crate) const CORNFLOWER_BLUE: RColor = RColor::Rgb(100, 149, 237);
pub(crate) const SELECTION_GREEN: RColor = RColor::Rgb(100, 200, 100);
pub(crate) const SOFT_WHITE: RColor = RColor::Rgb(220, 220, 230);
pub(crate) const MUTED_GRAY: RColor = RColor::Rgb(120, 120, 140);
pub(crate) const DARK_BG: RColor = RColor::Rgb(25, 25, 35);
pub(crate) const LIST_HIGHLIGHT_BG: RColor = RColor::Rgb(40, 40, 60);

/// Title token painted in the outer panel header. Kept padded with the brain
/// emoji so the P3 tripwire's exact `🧠 Learnings` token still matches.
pub(crate) const TITLE_TOKEN: &str = " 🧠 Learnings ";

/// The three top-level tabs. Browse is live (P5); Search (P7) and Graph (P8)
/// render a placeholder until their phases land — the arms exist now so their
/// modules slot in behind a stable enum without touching this dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    /// Scrollable record list + filter chips (P5).
    Browse,
    /// Semantic search via `qmd` (P7 — placeholder for now).
    Search,
    /// Entity / community graph explorer (P8 — placeholder for now).
    Graph,
}

impl Tab {
    /// Tabs in display order.
    const ALL: [Tab; 3] = [Tab::Browse, Tab::Search, Tab::Graph];

    /// Tab-strip label.
    fn label(self) -> &'static str {
        match self {
            Tab::Browse => "Browse",
            Tab::Search => "Search",
            Tab::Graph => "Graph",
        }
    }

    /// Next tab (wraps), for the `Tab` key.
    fn next(self) -> Self {
        let idx = Self::ALL.iter().position(|t| *t == self).unwrap_or(0);
        Self::ALL[(idx + 1) % Self::ALL.len()]
    }
}

/// View state for the learnings screen: the parsed KB plus per-tab UI state.
#[derive(Debug, Default)]
pub struct LearningsUi {
    /// Active top-level tab.
    tab: TabState,
    /// All parsed records, sorted by id (the source the Browse tab filters).
    records: Vec<LearningRecord>,
    /// Count of corrupt notes the last scan skipped — surfaced in the Browse
    /// status line as `N notes failed to parse`.
    failed_count: usize,
    /// Browse-tab UI state (selection + active filter cursor).
    browse: BrowseState,
    /// Detail / read-pane open-state. When open it renders over the active
    /// tab's body and consumes keys until closed (`Backspace`). Tab-agnostic —
    /// any tab opens it the same way via [`Self::open_detail_for_selection`].
    detail: DetailState,
}

/// Wrapper so [`Tab`] can derive a sensible [`Default`] (Browse) without
/// hand-implementing it on the public enum.
#[derive(Debug, Clone, Copy)]
struct TabState(Tab);

impl Default for TabState {
    fn default() -> Self {
        Self(Tab::Browse)
    }
}

impl LearningsUi {
    /// Load the parsed records + corrupt-note count into the view. Called by
    /// the plugin after it scans the configured `learnings_dir`.
    pub fn set_records(&mut self, records: Vec<LearningRecord>, failed_count: usize) {
        self.records = records;
        self.failed_count = failed_count;
        self.browse.clamp_selection(self.visible_count());
    }

    /// The currently-active tab (exposed for the plugin's debug / tests).
    #[must_use]
    pub fn tab(&self) -> Tab {
        self.tab.0
    }

    /// Number of records visible under the active Browse filter.
    fn visible_count(&self) -> usize {
        self.browse.filter().apply(&self.records).len()
    }

    /// Route one key. Returns `true` when state changed (so the plugin bumps
    /// its render generation). Host-reserved keys (Esc, etc.) are NOT forwarded
    /// by the host, so a return of `false` for an unhandled key is correct.
    ///
    /// Routing precedence:
    /// 1. **Detail pane open** — it consumes the key (`Backspace`/`Esc` close;
    ///    everything else is swallowed so the list behind can't move). The
    ///    pane is modal over the whole shell, so this short-circuits `Tab` too.
    /// 2. **`Enter`** — opens the Detail pane on the active tab's selected row.
    /// 3. **`Tab`** — switches the top-level tab.
    /// 4. **Per-tab routing** — Browse handles its own keys; Search/Graph are
    ///    P7/P8 placeholders.
    pub fn handle_key(&mut self, code: &KeyCode) -> bool {
        // 1. Modal Detail pane takes precedence over everything.
        if self.detail.is_open() {
            return self.detail.handle_key(code);
        }

        // 2. `Enter` opens the Detail pane on the selected record.
        if matches!(code, KeyCode::Enter) {
            return self.open_detail_for_selection();
        }

        // 3. `Tab` switches the top-level tab.
        if matches!(code, KeyCode::Tab) {
            self.tab.0 = self.tab.0.next();
            return true;
        }

        // 4. Per-tab routing. `/` (search) and `g` (graph) are reserved for
        // P7/P8 and are intentionally inert here so a stray press is a clean
        // no-op rather than a swallowed key.
        match self.tab.0 {
            Tab::Browse => self.browse.handle_key(code, &self.records),
            Tab::Search | Tab::Graph => false,
        }
    }

    /// Open the Detail pane on the active tab's currently-selected record.
    /// Returns `true` when a record was selectable (pane opened), `false` when
    /// the list is empty (nothing to open — a clean no-op). The Browse
    /// selection is left untouched so closing the pane returns to the same row.
    fn open_detail_for_selection(&mut self) -> bool {
        // Only Browse exposes a selection today; Search/Graph (P7/P8) wire
        // their own selectors behind their arms when they land.
        let selected = match self.tab.0 {
            Tab::Browse => self.browse.selected_record(&self.records),
            Tab::Search | Tab::Graph => None,
        };
        match selected {
            Some(record) => {
                self.detail.open(record);
                true
            }
            None => false,
        }
    }

    /// `true` while the Detail pane is open (exposed for tests).
    #[must_use]
    pub fn detail_open(&self) -> bool {
        self.detail.is_open()
    }
}

/// Render the tabbed shell into `area`. Outer rounded panel titled
/// ` 🧠 Learnings `; a tab strip; then the active tab's body.
pub fn render(buf: &mut RBuffer, area: RRect, ui: &LearningsUi) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let outer = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CORNFLOWER_BLUE))
        .title(Span::styled(
            TITLE_TOKEN,
            Style::default().fg(GOLD).add_modifier(RModifier::BOLD),
        ))
        .style(Style::default().bg(DARK_BG));
    let inner = outer.inner(area);
    outer.render(area, buf);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // Detail pane is modal: when open it takes the whole inner region (over the
    // tab strip + body) for a clean, full-width read view (design mock C3).
    if ui.detail.is_open() {
        detail::render(buf, inner, &ui.detail);
        return;
    }

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // tab strip
            Constraint::Min(0),    // tab body
        ])
        .split(inner);

    render_tab_strip(buf, rows[0], ui.tab.0);

    match ui.tab.0 {
        Tab::Browse => {
            browse::render(buf, rows[1], &ui.records, ui.failed_count, &ui.browse);
        }
        Tab::Search => render_placeholder(buf, rows[1], "Search", "P7"),
        Tab::Graph => render_placeholder(buf, rows[1], "Graph", "P8"),
    }
}

/// Paint the `Browse │ Search │ Graph` tab strip with the active tab in gold.
fn render_tab_strip(buf: &mut RBuffer, area: RRect, active: Tab) {
    let titles: Vec<Line> = Tab::ALL
        .iter()
        .map(|t| {
            let style = if *t == active {
                Style::default().fg(GOLD).add_modifier(RModifier::BOLD | RModifier::UNDERLINED)
            } else {
                Style::default().fg(MUTED_GRAY)
            };
            Line::from(Span::styled(t.label(), style))
        })
        .collect();
    let idx = Tab::ALL.iter().position(|t| *t == active).unwrap_or(0);
    let tabs = Tabs::new(titles)
        .select(idx)
        .divider(Span::styled(" │ ", Style::default().fg(MUTED_GRAY)));
    tabs.render(area, buf);
}

/// A "coming in P7/P8" placeholder for the not-yet-implemented tabs.
fn render_placeholder(buf: &mut RBuffer, area: RRect, name: &str, phase: &str) {
    let line = Line::from(vec![
        Span::styled(format!("  {name} "), Style::default().fg(SOFT_WHITE)),
        Span::styled(
            format!("— coming in {phase}"),
            Style::default().fg(MUTED_GRAY).add_modifier(RModifier::ITALIC),
        ),
    ]);
    Paragraph::new(line).render(area, buf);
}
