//! Tabbed UI shell for the learnings browser.
//!
//! Owns the tab state ([`Tab`]: Browse | Search | Graph), the parsed-record
//! store, and the key routing that the plugin's `handle_key` delegates to.
//! Browse (P5), the Detail read-pane (P6), Search (P7), and the Graph explorer
//! (P8) are all live.
//!
//! Render path mirrors burndown: paint locally into a ratatui `Buffer`
//! through [`render`], then the plugin converts the buffer to a `WireBuffer`
//! cell stream for the host. Input flows through [`LearningsUi::handle_key`],
//! which returns `true` when the key mutated state (so the plugin bumps its
//! render generation).

mod browse;
mod detail;
mod graph;
mod search;

use ratatui::buffer::Buffer as RBuffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect as RRect};
use ratatui::style::{Color as RColor, Modifier as RModifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Tabs, Widget};

use ainb_plugin_sdk::KeyCode;

use crate::data::{Community, LearningRecord};

use browse::BrowseState;
use detail::DetailState;
use graph::GraphState;
pub use search::SearchContext;
use search::SearchState;

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

/// The three top-level tabs. Browse (P5), Search (P7), and Graph (P8) are all
/// live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    /// Scrollable record list + filter chips (P5).
    Browse,
    /// Semantic search via `qmd` — live query box + ranked results (P7).
    Search,
    /// Entity-neighborhood / community-cluster graph explorer (P8).
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
    /// Search-tab UI state (query box + ranked results + selection).
    search: SearchState,
    /// Graph-tab UI state (typed entity adjacency + community clusters +
    /// selection + focus).
    graph: GraphState,
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
    ///
    /// Also rebuilds the Graph tab's typed entity adjacency from the records'
    /// aggregated `relationships[]` so the Graph view stays in lock-step with
    /// the scanned KB.
    pub fn set_records(&mut self, records: Vec<LearningRecord>, failed_count: usize) {
        self.records = records;
        self.failed_count = failed_count;
        self.browse.clamp_selection(self.visible_count());
        self.graph.set_records(&self.records);
    }

    /// Load the parsed community clusters (from `kv_store_community_reports.json`)
    /// into the Graph tab's community view. Called by the plugin after it reads
    /// the configured `graph_cache`. A read failure simply leaves the community
    /// view empty (its honest empty state) rather than failing the screen.
    pub fn set_communities(&mut self, communities: Vec<Community>) {
        self.graph.set_communities(communities);
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
    /// `ctx` carries the injected `qmd` runner + the resolved collection/index
    /// the Search tab needs to actually run a query (the plugin builds it fresh
    /// from its config each call so the UI stays pure view state).
    ///
    /// Routing precedence:
    /// 1. **Detail pane open** — it consumes the key (`Backspace`/`Esc` close;
    ///    everything else is swallowed so the list behind can't move). The pane
    ///    is modal over the whole shell, so this short-circuits `Tab` too.
    /// 2. **Graph focused** — when `g` has focused the Graph tab, `Backspace`
    ///    releases focus (the in-plugin "back"; Esc is host-reserved) and the
    ///    graph claims `↑↓`/`jk`/`c` for navigation + the entity⇄community
    ///    toggle. `Tab` and `/` still fall through (they switch tab / view), so
    ///    a focused graph never traps the user on the screen.
    /// 3. **`/`** — switch to the Search tab and focus its query box (a global
    ///    shortcut, available from any tab, mirroring the design mock footer).
    /// 4. **`g`** — switch to the Graph tab and focus it (a global shortcut,
    ///    available from any tab; the design mock footer: `g graph`). Suppressed
    ///    while the Search query box is focused so a user can type `g` into a
    ///    query (search for `git`, `golang`, …) — the same rule that keeps
    ///    `j`/`k` typeable in the Search box.
    /// 5. **`Tab`** — switches the top-level tab.
    /// 6. **Per-tab routing**:
    ///    - **Search** — the query box / results consume keys (incl. `Enter`,
    ///      which submits a query or opens a resolved result's Detail pane).
    ///    - **Browse** — `Enter` opens the Detail pane on the selected row;
    ///      otherwise Browse handles its own keys.
    ///    - **Graph** — when not focused, navigation keys are inert (press `g`
    ///      to focus first).
    pub fn handle_key(&mut self, code: &KeyCode, ctx: &SearchContext<'_>) -> bool {
        // 1. Modal Detail pane takes precedence over everything.
        if self.detail.is_open() {
            return self.detail.handle_key(code);
        }

        // 2. Graph focus: `Backspace` releases focus; the graph claims its own
        // navigation keys. `Tab`/`/`/`g` fall through to the global shortcuts
        // below so the user can always leave a focused graph.
        if self.tab.0 == Tab::Graph && self.graph.is_focused() {
            if matches!(code, KeyCode::Backspace) {
                self.graph.blur();
                return true;
            }
            if !matches!(code, KeyCode::Tab | KeyCode::Char { ch: '/' | 'g' })
                && self.graph.handle_key(code)
            {
                return true;
            }
        }

        // 3. `/` is a global shortcut to the Search tab + query box, from any
        // tab (design mock footer: `/ search`). It does NOT type a literal `/`
        // into the query — that's the focus action.
        if matches!(code, KeyCode::Char { ch: '/' }) {
            self.tab.0 = Tab::Search;
            self.search.focus();
            return true;
        }

        // 4. `g` is a global shortcut to the Graph tab + entity focus, from any
        // tab (design mock footer: `g graph`) — EXCEPT while the Search query
        // box is focused, where `g` must type into the query (so a user can
        // search for `git`, `golang`, …). Same rationale as the `j`/`k` guard.
        let search_box_focused = self.tab.0 == Tab::Search && self.search.is_focused();
        if matches!(code, KeyCode::Char { ch: 'g' }) && !search_box_focused {
            self.tab.0 = Tab::Graph;
            self.graph.focus();
            return true;
        }

        // 5. `Tab` switches the top-level tab.
        if matches!(code, KeyCode::Tab) {
            self.tab.0 = self.tab.0.next();
            return true;
        }

        // 6. Per-tab routing.
        match self.tab.0 {
            Tab::Search => {
                let outcome = self.search.handle_key(code, ctx, &self.records);
                if let Some(record) = outcome.open_record {
                    self.detail.open(&record);
                }
                outcome.changed
            }
            Tab::Browse => {
                if matches!(code, KeyCode::Enter) {
                    return self.open_detail_for_selection();
                }
                self.browse.handle_key(code, &self.records)
            }
            // Graph navigation only fires while focused (handled in step 2);
            // an unfocused Graph press is a clean no-op (press `g` to focus).
            Tab::Graph => false,
        }
    }

    /// Open the Detail pane on the Browse tab's currently-selected record.
    /// Returns `true` when a record was selectable (pane opened), `false` when
    /// the list is empty (nothing to open — a clean no-op). The Browse
    /// selection is left untouched so closing the pane returns to the same row.
    fn open_detail_for_selection(&mut self) -> bool {
        match self.browse.selected_record(&self.records) {
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
        Tab::Search => search::render(buf, rows[1], &ui.search),
        Tab::Graph => graph::render(buf, rows[1], &ui.graph),
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
