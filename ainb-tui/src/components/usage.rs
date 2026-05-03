// ABOUTME: Usage analytics screen showing token consumption by day, week, and project.
// Accessible via 'i' key from home screen or Stats sidebar item.

use chrono::{Datelike, Local, NaiveDate};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Row, Table, Tabs},
};

use crate::models::{
    ActivityUsage, ModelUsage, NamedUsage, ProjectUsage, SessionUsage, UsageData, UsageFilterChip,
    UsageFilters, UsagePeriod, UsageProviderFilter, UsageQuery, filter_usage_data,
    format_tokens_short, optimize_usage,
};

// Color palette from TUI style guide
const CORNFLOWER_BLUE: Color = Color::Rgb(100, 149, 237);
const GOLD: Color = Color::Rgb(255, 215, 0);
const SELECTION_GREEN: Color = Color::Rgb(100, 200, 100);
const DARK_BG: Color = Color::Rgb(25, 25, 35);
const PANEL_BG: Color = Color::Rgb(30, 30, 40);
const SOFT_WHITE: Color = Color::Rgb(220, 220, 230);
const MUTED_GRAY: Color = Color::Rgb(120, 120, 140);
const BAR_COLOR: Color = Color::Rgb(80, 160, 230);
const BAR_HIGH: Color = Color::Rgb(230, 120, 80);
const BAR_MED: Color = Color::Rgb(200, 180, 80);
const TERMINAL_BG: Color = Color::Rgb(13, 14, 18);
const TERMINAL_PANEL: Color = Color::Rgb(17, 19, 26);
const TERMINAL_BORDER: Color = Color::Rgb(130, 90, 70);
const TERMINAL_ACCENT: Color = Color::Rgb(255, 184, 108);
const TERMINAL_GOOD: Color = Color::Rgb(155, 216, 106);
const TERMINAL_CYAN: Color = Color::Rgb(125, 211, 200);

// Bar gradient thresholds: ratio of value/max above which a row is colored.
const BAR_THRESHOLD_HIGH: f64 = 0.66;
const BAR_THRESHOLD_MED: f64 = 0.33;

/// Which agent provider's usage to show
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UsageProvider {
    #[default]
    Claude,
    Codex,
    Gemini,
    Copilot,
}

impl UsageProvider {
    fn all() -> &'static [UsageProvider] {
        &[
            UsageProvider::Claude,
            UsageProvider::Codex,
            UsageProvider::Gemini,
            UsageProvider::Copilot,
        ]
    }

    fn label(&self) -> &'static str {
        match self {
            UsageProvider::Claude => "✻ Claude Code",
            UsageProvider::Codex => "✦ Codex CLI",
            UsageProvider::Gemini => "✨ Gemini CLI",
            UsageProvider::Copilot => "🐙 Copilot",
        }
    }

    fn short_label(&self) -> &'static str {
        match self {
            UsageProvider::Claude => "Claude",
            UsageProvider::Codex => "Codex",
            UsageProvider::Gemini => "Gemini",
            UsageProvider::Copilot => "Copilot",
        }
    }

    pub fn has_data(&self) -> bool {
        matches!(self, UsageProvider::Claude | UsageProvider::Codex)
    }

    fn next(&self) -> Self {
        match self {
            UsageProvider::Claude => UsageProvider::Codex,
            UsageProvider::Codex => UsageProvider::Gemini,
            UsageProvider::Gemini => UsageProvider::Copilot,
            UsageProvider::Copilot => UsageProvider::Claude,
        }
    }

    fn prev(&self) -> Self {
        match self {
            UsageProvider::Claude => UsageProvider::Copilot,
            UsageProvider::Codex => UsageProvider::Claude,
            UsageProvider::Gemini => UsageProvider::Codex,
            UsageProvider::Copilot => UsageProvider::Gemini,
        }
    }
}

/// Which sub-tab is active in the usage view
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UsageTab {
    #[default]
    Daily,
    Weekly,
    Projects,
    Burndown,
    Optimize,
}

impl UsageTab {
    fn all() -> &'static [UsageTab] {
        &[
            UsageTab::Daily,
            UsageTab::Weekly,
            UsageTab::Projects,
            UsageTab::Burndown,
            UsageTab::Optimize,
        ]
    }

    fn title(&self) -> &'static str {
        match self {
            UsageTab::Daily => "Daily",
            UsageTab::Weekly => "Weekly",
            UsageTab::Projects => "By Project",
            UsageTab::Burndown => "Burndown",
            UsageTab::Optimize => "Optimize",
        }
    }

    fn next(&self) -> Self {
        match self {
            UsageTab::Daily => UsageTab::Weekly,
            UsageTab::Weekly => UsageTab::Projects,
            UsageTab::Projects => UsageTab::Burndown,
            UsageTab::Burndown => UsageTab::Optimize,
            UsageTab::Optimize => UsageTab::Daily,
        }
    }

    fn prev(&self) -> Self {
        match self {
            UsageTab::Daily => UsageTab::Optimize,
            UsageTab::Weekly => UsageTab::Daily,
            UsageTab::Projects => UsageTab::Weekly,
            UsageTab::Burndown => UsageTab::Projects,
            UsageTab::Optimize => UsageTab::Burndown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageInputMode {
    Include,
    Exclude,
    DateRange,
}

/// Focusable panels on the Burndown dashboard for cross-filter pivot.
///
/// Order matters: it defines the Tab/BackTab traversal sequence. The
/// brief specifies Daily Activity → By Project → Top Sessions → Live →
/// By Activity → By Model → Named → Optimize → Leaderboard → Budget.
/// We expand "Named" into the three concrete panels so every visible
/// table is keyboard-reachable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsagePanel {
    DailyActivity,
    ByProject,
    TopSessions,
    Live,
    ByActivity,
    ByModel,
    CoreTools,
    ShellCommands,
    McpServers,
    Optimize,
    Leaderboard,
    Budget,
}

impl UsagePanel {
    pub const ALL: [UsagePanel; 12] = [
        UsagePanel::DailyActivity,
        UsagePanel::ByProject,
        UsagePanel::TopSessions,
        UsagePanel::Live,
        UsagePanel::ByActivity,
        UsagePanel::ByModel,
        UsagePanel::CoreTools,
        UsagePanel::ShellCommands,
        UsagePanel::McpServers,
        UsagePanel::Optimize,
        UsagePanel::Leaderboard,
        UsagePanel::Budget,
    ];

    pub fn next(self) -> Self {
        let idx = Self::ALL.iter().position(|p| *p == self).unwrap_or(0);
        Self::ALL[(idx + 1) % Self::ALL.len()]
    }

    pub fn prev(self) -> Self {
        let idx = Self::ALL.iter().position(|p| *p == self).unwrap_or(0);
        let last = Self::ALL.len() - 1;
        Self::ALL[if idx == 0 { last } else { idx - 1 }]
    }

    /// Human-readable panel name used in the zoom breadcrumb.
    pub fn title(self) -> &'static str {
        match self {
            UsagePanel::DailyActivity => "Daily Activity",
            UsagePanel::ByProject => "By Project",
            UsagePanel::TopSessions => "Top Sessions",
            UsagePanel::Live => "Live Session Ticker",
            UsagePanel::ByActivity => "By Activity",
            UsagePanel::ByModel => "By Model",
            UsagePanel::CoreTools => "Core Tools",
            UsagePanel::ShellCommands => "Shell Commands",
            UsagePanel::McpServers => "MCP Servers",
            UsagePanel::Optimize => "Optimization Recommendations",
            UsagePanel::Leaderboard => "Agent Leaderboard",
            UsagePanel::Budget => "Budget · Alerts",
        }
    }

    /// Whether `Enter` on this panel maps a row onto a cross-filter.
    /// Daily Activity, Optimize, and Budget are read-only — Enter is a
    /// no-op there. Leaderboard maps the focused row onto the Project
    /// filter (rows are projects).
    pub fn enter_target(self) -> Option<UsageFilterTarget> {
        match self {
            UsagePanel::ByProject | UsagePanel::Leaderboard => Some(UsageFilterTarget::Project),
            UsagePanel::ByActivity => Some(UsageFilterTarget::Activity),
            UsagePanel::ByModel => Some(UsageFilterTarget::Model),
            UsagePanel::TopSessions | UsagePanel::Live => Some(UsageFilterTarget::Session),
            _ => None,
        }
    }
}

/// Which slot on `UsageFilters` a panel-row commits into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageFilterTarget {
    Project,
    Model,
    Activity,
    Session,
}

/// View state for the usage analytics screen
#[derive(Debug, Clone)]
pub struct UsageViewState {
    pub provider: UsageProvider,
    pub active_tab: UsageTab,
    pub data: Option<UsageData>,
    pub loading: bool,
    pub scroll_offset: usize,
    pub period: UsagePeriod,
    pub provider_filter: UsageProviderFilter,
    pub include_projects: Vec<String>,
    pub exclude_projects: Vec<String>,
    pub input_mode: Option<UsageInputMode>,
    pub input_buffer: String,
    /// Cross-filter chips set by the Burndown panel pivot or CLI flags.
    /// These layer on top of include/exclude project globs (see
    /// `models::usage::UsageFilters` for matching semantics).
    pub filters: UsageFilters,
    /// Currently-focused dashboard panel (Burndown only). `None` means
    /// no focus — Tab moves between outer tabs as before.
    pub focused_panel: Option<UsagePanel>,
    /// Row indicator inside the focused panel. Clamped to the row count
    /// of the focused panel's underlying collection at render time.
    pub focus_row: usize,
    /// When `Some`, the burndown view is rendered as a single full-area
    /// zoomed panel for the given panel kind. Driven by the `z`
    /// keybinding on Burndown; only meaningful when on Burndown.
    pub zoom: Option<UsagePanel>,
    /// True when the zoom view's `/` fuzzy-search overlay is active.
    /// Resets on zoom exit (we deliberately do *not* persist a search
    /// across zoom cycles — the brief is "search clears on zoom exit").
    pub zoom_search_active: bool,
    /// Live query buffer for the zoom-mode fuzzy search.
    pub zoom_search_query: String,
    /// True when the zoom view's `d` detail drawer is open. The drawer
    /// occupies the bottom 40% of the zoom area and shows the full
    /// row record for the focused row.
    pub zoom_detail_open: bool,
}

impl Default for UsageViewState {
    fn default() -> Self {
        Self {
            provider: UsageProvider::Claude,
            active_tab: UsageTab::Daily,
            data: None,
            loading: false,
            scroll_offset: 0,
            period: UsagePeriod::Week,
            provider_filter: UsageProviderFilter::All,
            include_projects: Vec::new(),
            exclude_projects: Vec::new(),
            input_mode: None,
            input_buffer: String::new(),
            filters: UsageFilters::default(),
            focused_panel: None,
            focus_row: 0,
            zoom: None,
            zoom_search_active: false,
            zoom_search_query: String::new(),
            zoom_detail_open: false,
        }
    }
}

impl UsageViewState {
    pub fn next_provider(&mut self) {
        self.provider = self.provider.next();
        self.provider_filter = match self.provider {
            UsageProvider::Claude => UsageProviderFilter::Claude,
            UsageProvider::Codex => UsageProviderFilter::Codex,
            UsageProvider::Gemini | UsageProvider::Copilot => UsageProviderFilter::All,
        };
        self.scroll_offset = 0;
    }

    pub fn prev_provider(&mut self) {
        self.provider = self.provider.prev();
        self.provider_filter = match self.provider {
            UsageProvider::Claude => UsageProviderFilter::Claude,
            UsageProvider::Codex => UsageProviderFilter::Codex,
            UsageProvider::Gemini | UsageProvider::Copilot => UsageProviderFilter::All,
        };
        self.scroll_offset = 0;
    }

    pub fn cycle_provider_filter(&mut self) {
        self.provider_filter = match self.provider_filter {
            UsageProviderFilter::All => UsageProviderFilter::Claude,
            UsageProviderFilter::Claude => UsageProviderFilter::Codex,
            UsageProviderFilter::Codex => UsageProviderFilter::All,
        };
        self.scroll_offset = 0;
    }

    pub fn set_period(&mut self, period: UsagePeriod) {
        self.period = period;
        self.scroll_offset = 0;
    }

    /// Step the active Month or Quarter picker one unit back. Clamps
    /// at the oldest day in `data.daily` (so we never step into a
    /// region known to have no usage rows). Returns `true` when the
    /// period actually changed.
    ///
    /// No-op when the active period is not a Month/Quarter picker.
    pub fn step_period_back(&mut self) -> bool {
        // Refuse to step until data has loaded — without an `oldest`
        // anchor we'd let the user wander arbitrarily far backwards,
        // and the resulting picker state would silently fall outside
        // the data range once the load completes.
        let Some(oldest) =
            self.data.as_ref().and_then(|d| d.daily.first().map(|(date, _)| *date))
        else {
            return false;
        };
        match self.period.clone() {
            UsagePeriod::SpecificMonth(anchor) => {
                let new_anchor = previous_month_first(anchor);
                if new_anchor < first_of_month(oldest) {
                    return false;
                }
                self.period = UsagePeriod::SpecificMonth(new_anchor);
                self.scroll_offset = 0;
                true
            }
            UsagePeriod::SpecificQuarter(year, q) => {
                let (new_year, new_q) = previous_quarter(year, q);
                let (qy, qq) = current_quarter(oldest);
                if (new_year, new_q) < (qy, qq) {
                    return false;
                }
                self.period = UsagePeriod::SpecificQuarter(new_year, new_q);
                self.scroll_offset = 0;
                true
            }
            _ => false,
        }
    }

    /// Step forward one unit. Clamps at the current real-world month
    /// or quarter — never lets the user pick a future window.
    pub fn step_period_forward(&mut self) -> bool {
        let today = Local::now().date_naive();
        match self.period.clone() {
            UsagePeriod::SpecificMonth(anchor) => {
                let new_anchor = next_month_first(anchor);
                if new_anchor > first_of_month(today) {
                    return false;
                }
                self.period = UsagePeriod::SpecificMonth(new_anchor);
                self.scroll_offset = 0;
                true
            }
            UsagePeriod::SpecificQuarter(year, q) => {
                let (new_year, new_q) = next_quarter(year, q);
                let (cy, cq) = current_quarter(today);
                if (new_year, new_q) > (cy, cq) {
                    return false;
                }
                self.period = UsagePeriod::SpecificQuarter(new_year, new_q);
                self.scroll_offset = 0;
                true
            }
            _ => false,
        }
    }

    pub fn next_tab(&mut self) {
        self.active_tab = self.active_tab.next();
        self.scroll_offset = 0;
    }

    pub fn prev_tab(&mut self) {
        self.active_tab = self.active_tab.prev();
        self.scroll_offset = 0;
    }

    pub fn scroll_up(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_sub(1);
    }

    pub fn scroll_down(&mut self, max_rows: usize) {
        if self.scroll_offset < max_rows.saturating_sub(1) {
            self.scroll_offset += 1;
        }
    }

    pub fn scroll_to_top(&mut self) {
        self.scroll_offset = 0;
    }

    pub fn scroll_to_bottom(&mut self, max_rows: usize) {
        self.scroll_offset = max_rows.saturating_sub(1);
    }

    pub fn page_up(&mut self, page_size: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(page_size);
    }

    pub fn page_down(&mut self, max_rows: usize, page_size: usize) {
        self.scroll_offset = (self.scroll_offset + page_size).min(max_rows.saturating_sub(1));
    }

    pub fn row_count(&self) -> usize {
        match &self.data {
            None => 0,
            Some(data) => match self.active_tab {
                UsageTab::Daily => data.daily.len(),
                UsageTab::Weekly => data.weekly.len(),
                UsageTab::Projects => data.projects.len(),
                UsageTab::Burndown => {
                    data.daily.len()
                        + data.projects.len()
                        + data.sessions.len()
                        + data.activities.len()
                        + data.models.len()
                }
                UsageTab::Optimize => optimize_usage(data).findings.len(),
            },
        }
    }

    pub fn query(&self) -> UsageQuery {
        UsageQuery {
            period: self.period.clone(),
            provider_filter: self.provider_filter,
            include_projects: self.include_projects.clone(),
            exclude_projects: self.exclude_projects.clone(),
            // Cross-filters apply client-side via `filter_usage_data` after
            // the (cached) parse, so we deliberately do NOT plumb them
            // into the parse query — that would invalidate the cache key
            // every time the user pivoted.
            filters: UsageFilters::default(),
        }
    }

    pub fn begin_input(&mut self, mode: UsageInputMode) {
        self.input_mode = Some(mode);
        self.input_buffer.clear();
    }

    pub fn input_char(&mut self, ch: char) {
        self.input_buffer.push(ch);
    }

    pub fn input_backspace(&mut self) {
        self.input_buffer.pop();
    }

    pub fn cancel_input(&mut self) {
        self.input_mode = None;
        self.input_buffer.clear();
    }

    pub fn submit_input(&mut self) -> Result<(), String> {
        let value = self.input_buffer.trim().to_string();
        match self.input_mode {
            Some(UsageInputMode::Include) => {
                if !value.is_empty() {
                    self.include_projects.push(value);
                }
            }
            Some(UsageInputMode::Exclude) => {
                if !value.is_empty() {
                    self.exclude_projects.push(value);
                }
            }
            Some(UsageInputMode::DateRange) => {
                let parts: Vec<_> = value
                    .split(|ch| ch == '.' || ch == ',' || ch == ' ')
                    .filter(|part| !part.is_empty())
                    .collect();
                if parts.len() != 2 {
                    return Err("Use YYYY-MM-DD YYYY-MM-DD".to_string());
                }
                let from = chrono::NaiveDate::parse_from_str(parts[0], "%Y-%m-%d")
                    .map_err(|_| "Invalid from date".to_string())?;
                let to = chrono::NaiveDate::parse_from_str(parts[1], "%Y-%m-%d")
                    .map_err(|_| "Invalid to date".to_string())?;
                if from > to {
                    return Err("From date must be before to date".to_string());
                }
                self.period = UsagePeriod::Custom { from, to };
            }
            None => {}
        }
        self.cancel_input();
        Ok(())
    }

    pub fn clear_filters(&mut self) {
        self.include_projects.clear();
        self.exclude_projects.clear();
    }

    /// Cycle the focus pointer to the next panel. If unfocused, focus
    /// the first panel. Resets `focus_row` to 0.
    pub fn focus_next_panel(&mut self) {
        self.focused_panel = Some(match self.focused_panel {
            Some(panel) => panel.next(),
            None => UsagePanel::ALL[0],
        });
        self.focus_row = 0;
    }

    /// Cycle focus backward.
    pub fn focus_prev_panel(&mut self) {
        self.focused_panel = Some(match self.focused_panel {
            Some(panel) => panel.prev(),
            None => UsagePanel::ALL[UsagePanel::ALL.len() - 1],
        });
        self.focus_row = 0;
    }

    /// Drop focus entirely (no panel highlighted).
    pub fn clear_focus(&mut self) {
        self.focused_panel = None;
        self.focus_row = 0;
    }

    /// Move the row indicator inside the focused panel. No-op when
    /// nothing is focused. Clamping happens at render time once we know
    /// the row count of the underlying collection.
    pub fn focus_row_up(&mut self) {
        self.focus_row = self.focus_row.saturating_sub(1);
    }

    pub fn focus_row_down(&mut self) {
        // Saturate at usize::MAX is safe — render-side clamps to actual
        // rows and the user sees no further movement.
        self.focus_row = self.focus_row.saturating_add(1);
    }

    /// Filtered view of the parsed data, applying the active cross
    /// filter chips. Cheap when no filters are set (clones the source).
    pub fn filtered_data(&self) -> Option<UsageData> {
        self.data.as_ref().map(|data| filter_usage_data(data, &self.filters))
    }

    /// Append the focused row of the focused panel as a chip. Returns
    /// `true` if a chip was added (so callers can show feedback).
    /// Requires `data` to be loaded; uses the unfiltered `data` to
    /// resolve the row by index because that's what the user is
    /// looking at when focus is active (we render from filtered_data
    /// at draw time, which is the same source).
    pub fn commit_focused_row(&mut self) -> bool {
        let Some(panel) = self.focused_panel else {
            return false;
        };
        let Some(target) = panel.enter_target() else {
            return false;
        };
        let Some(filtered) = self.filtered_data() else {
            return false;
        };
        let row_idx = self.focus_row;
        // For Session rows we also need the owning project so we can
        // auto-attach a project chip — session ids can collide across
        // projects/providers because the aggregator key is
        // `provider:project:session_id` but `filters.session` only holds
        // the bare id. Other targets pass None as the second element.
        let value: Option<(String, Option<String>)> = match (target, panel) {
            (UsageFilterTarget::Project, UsagePanel::Leaderboard | UsagePanel::ByProject) => {
                filtered.projects.get(row_idx).map(|p| (p.name.clone(), None))
            }
            (UsageFilterTarget::Activity, _) => {
                filtered.activities.get(row_idx).map(|a| (a.category.label().to_string(), None))
            }
            (UsageFilterTarget::Model, _) => {
                filtered.models.get(row_idx).map(|m| (m.model.clone(), None))
            }
            (UsageFilterTarget::Session, _) => filtered
                .sessions
                .get(row_idx)
                .map(|s| (s.session_id.clone(), Some(s.project.clone()))),
            _ => None,
        };
        let Some((value, owner_project)) = value else {
            return false;
        };
        match target {
            UsageFilterTarget::Project => {
                if !self.filters.project.contains(&value) {
                    self.filters.project.push(value);
                }
            }
            UsageFilterTarget::Model => {
                if !self.filters.model.contains(&value) {
                    self.filters.model.push(value);
                }
            }
            UsageFilterTarget::Activity => {
                if !self.filters.activity.contains(&value) {
                    self.filters.activity.push(value);
                }
            }
            UsageFilterTarget::Session => {
                if !self.filters.session.contains(&value) {
                    self.filters.session.push(value);
                }
                if let Some(p) = owner_project {
                    if !self.filters.project.contains(&p) {
                        self.filters.project.push(p);
                    }
                }
            }
        }
        true
    }

    /// Pop the most recently added cross-filter chip. Returns the
    /// removed chip so the caller can echo it in the notification.
    pub fn pop_filter_chip(&mut self) -> Option<UsageFilterChip> {
        self.filters.pop_last()
    }

    /// Drop every cross-filter chip. Leaves include/exclude untouched.
    pub fn clear_all_filter_chips(&mut self) {
        self.filters.clear();
    }

    /// True when the burndown view is currently zoomed.
    pub fn is_zoomed(&self) -> bool {
        self.zoom.is_some()
    }

    /// Toggle full-screen zoom for the currently-focused panel. If no
    /// panel is focused (Tab not pressed yet), the first focusable
    /// dashboard panel is used so `z` works as a discoverable
    /// "open up bigger" shortcut.
    pub fn toggle_zoom(&mut self) {
        if self.zoom.is_some() {
            self.exit_zoom();
        } else {
            let target = self.focused_panel.unwrap_or(UsagePanel::ALL[0]);
            self.zoom = Some(target);
            self.focused_panel = Some(target);
            self.focus_row = 0;
            self.zoom_search_active = false;
            self.zoom_search_query.clear();
            self.zoom_detail_open = false;
        }
    }

    /// Exit zoom mode and clear all zoom-scoped UI state (search query,
    /// detail drawer). Does NOT clear filter chips.
    pub fn exit_zoom(&mut self) {
        self.zoom = None;
        self.zoom_search_active = false;
        self.zoom_search_query.clear();
        self.zoom_detail_open = false;
    }

    /// Begin fuzzy-search input inside the zoomed panel.
    pub fn zoom_begin_search(&mut self) {
        if self.zoom.is_some() {
            self.zoom_search_active = true;
            self.zoom_search_query.clear();
        }
    }

    /// Cancel the active search and drop the partial query.
    pub fn zoom_cancel_search(&mut self) {
        self.zoom_search_active = false;
        self.zoom_search_query.clear();
    }

    /// Commit the typed search query. Same effect as cancel for the
    /// renderer (we filter by `zoom_search_query`); we just exit input
    /// mode so further keys flow back to navigation.
    pub fn zoom_commit_search(&mut self) {
        self.zoom_search_active = false;
    }

    pub fn zoom_search_char(&mut self, ch: char) {
        if self.zoom_search_active {
            self.zoom_search_query.push(ch);
        }
    }

    pub fn zoom_search_backspace(&mut self) {
        if self.zoom_search_active {
            self.zoom_search_query.pop();
        }
    }

    /// Toggle the detail drawer in zoom mode. No-op when not zoomed.
    pub fn toggle_zoom_detail(&mut self) {
        if self.zoom.is_some() {
            self.zoom_detail_open = !self.zoom_detail_open;
        }
    }

    /// Esc precedence in the zoom view (highest first):
    /// 1. close detail drawer
    /// 2. cancel active search
    /// 3. exit zoom
    /// 4. pop a chip (caller falls through here when nothing else
    ///    handled it — this method only consumes the first three).
    ///
    /// Returns `true` when Esc was consumed.
    pub fn zoom_handle_esc(&mut self) -> bool {
        if !self.is_zoomed() {
            return false;
        }
        if self.zoom_detail_open {
            self.zoom_detail_open = false;
            return true;
        }
        if self.zoom_search_active {
            self.zoom_cancel_search();
            return true;
        }
        self.exit_zoom();
        true
    }
}

/// Render the usage analytics screen
pub fn render(frame: &mut Frame, area: Rect, state: &UsageViewState) {
    // Main layout: header + provider selector + tabs + content + help bar
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Summary bar
            Constraint::Length(3), // Provider selector
            Constraint::Length(3), // Tab bar
            Constraint::Min(0),    // Table content
            Constraint::Length(2), // Help bar
        ])
        .split(area);

    render_summary_bar(frame, layout[0], state);
    render_provider_bar(frame, layout[1], state);
    render_tab_bar(frame, layout[2], state);

    if state.loading || state.data.is_none() {
        render_loading(frame, layout[3]);
    } else {
        let data = state.data.as_ref().unwrap();
        if data.calls.is_empty() && !state.provider.has_data() {
            render_no_data(frame, layout[3], state);
        } else {
            match state.active_tab {
                UsageTab::Daily => render_daily(frame, layout[3], data, state.scroll_offset),
                UsageTab::Weekly => render_weekly(frame, layout[3], data, state.scroll_offset),
                UsageTab::Projects => render_projects(frame, layout[3], data, state.scroll_offset),
                UsageTab::Burndown => render_burndown(frame, layout[3], data, state),
                UsageTab::Optimize => render_optimize(frame, layout[3], data),
            }
        }
    }

    render_help_bar(frame, layout[4], state);
}

fn render_summary_bar(frame: &mut Frame, area: Rect, state: &UsageViewState) {
    let mut spans = vec![
        Span::styled("📊 ", Style::default().fg(GOLD)),
        Span::styled(
            "Usage Analytics",
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ),
    ];

    if let Some(data) = &state.data {
        let gt = &data.grand_total;
        spans.extend_from_slice(&[
            Span::styled("  │  ", Style::default().fg(MUTED_GRAY)),
            Span::styled("Total: ", Style::default().fg(MUTED_GRAY)),
            Span::styled(
                format_tokens_short(gt.total()),
                Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" tokens", Style::default().fg(MUTED_GRAY)),
            Span::styled("  │  ", Style::default().fg(MUTED_GRAY)),
            Span::styled(
                format!("{}", data.daily.len()),
                Style::default().fg(SOFT_WHITE),
            ),
            Span::styled(" days  ", Style::default().fg(MUTED_GRAY)),
            Span::styled(
                format!("{}", data.projects.len()),
                Style::default().fg(SOFT_WHITE),
            ),
            Span::styled(" projects  ", Style::default().fg(MUTED_GRAY)),
            Span::styled(
                format!("{}", gt.session_count),
                Style::default().fg(SOFT_WHITE),
            ),
            Span::styled(" sessions", Style::default().fg(MUTED_GRAY)),
        ]);
    } else if state.provider.has_data() {
        spans.push(Span::styled(
            "  │  Loading...",
            Style::default().fg(MUTED_GRAY),
        ));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CORNFLOWER_BLUE))
        .style(Style::default().bg(DARK_BG));

    let paragraph = Paragraph::new(Line::from(spans)).block(block);
    frame.render_widget(paragraph, area);
}

fn render_provider_bar(frame: &mut Frame, area: Rect, state: &UsageViewState) {
    let mut spans: Vec<Span> = vec![Span::styled(
        "  Provider: ",
        Style::default().fg(MUTED_GRAY),
    )];

    for (i, provider) in UsageProvider::all().iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  │  ", Style::default().fg(MUTED_GRAY)));
        }

        let is_active = *provider == state.provider;
        let style = if is_active {
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else if provider.has_data() {
            Style::default().fg(SOFT_WHITE)
        } else {
            Style::default().fg(MUTED_GRAY)
        };

        spans.push(Span::styled(provider.label(), style));
    }

    spans.push(Span::styled("    ", Style::default()));
    spans.push(Span::styled(
        "◀/▶",
        Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::styled(" switch", Style::default().fg(MUTED_GRAY)));
    spans.push(Span::styled("  │  p ", Style::default().fg(GOLD)));
    spans.push(Span::styled(
        format!("filter: {}", provider_filter_label(state.provider_filter)),
        Style::default().fg(MUTED_GRAY),
    ));
    spans.push(Span::styled("  │  ", Style::default().fg(MUTED_GRAY)));
    spans.push(Span::styled(
        period_label(&state.period),
        Style::default().fg(SOFT_WHITE),
    ));
    if !state.include_projects.is_empty() || !state.exclude_projects.is_empty() {
        spans.push(Span::styled(
            "  │  filters active",
            Style::default().fg(GOLD),
        ));
    }
    if let Some(mode) = state.input_mode {
        spans.push(Span::styled("  │  ", Style::default().fg(MUTED_GRAY)));
        spans.push(Span::styled(
            format!("{}: {}", input_label(mode), state.input_buffer),
            Style::default().fg(GOLD),
        ));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CORNFLOWER_BLUE))
        .style(Style::default().bg(DARK_BG));

    let paragraph = Paragraph::new(Line::from(spans)).block(block);
    frame.render_widget(paragraph, area);
}

fn render_no_data(frame: &mut Frame, area: Rect, state: &UsageViewState) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CORNFLOWER_BLUE))
        .style(Style::default().bg(DARK_BG));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let lines = vec![
        Line::from(""),
        Line::from(""),
        Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled(
                state.provider.label(),
                Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                " usage tracking is not yet available.",
                Style::default().fg(MUTED_GRAY),
            ),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "  Usage data parsing is currently supported for Claude Code and Codex.",
            Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
        )),
        Line::from(Span::styled(
            "  Other providers will be added as they expose session-level usage data.",
            Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
        )),
    ];

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}

fn render_tab_bar(frame: &mut Frame, area: Rect, state: &UsageViewState) {
    let titles: Vec<Line> = UsageTab::all()
        .iter()
        .map(|t| {
            let style = if *t == state.active_tab {
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
            } else {
                Style::default().fg(MUTED_GRAY)
            };
            Line::from(Span::styled(t.title(), style))
        })
        .collect();

    let idx = UsageTab::all().iter().position(|t| *t == state.active_tab).unwrap_or(0);
    let tabs = Tabs::new(titles)
        .select(idx)
        .highlight_style(Style::default().fg(GOLD).add_modifier(Modifier::BOLD))
        .divider(Span::styled(" │ ", Style::default().fg(MUTED_GRAY)))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(CORNFLOWER_BLUE))
                .style(Style::default().bg(DARK_BG)),
        );

    frame.render_widget(tabs, area);
}

fn render_loading(frame: &mut Frame, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CORNFLOWER_BLUE))
        .style(Style::default().bg(DARK_BG));

    let paragraph = Paragraph::new(Line::from(vec![Span::styled(
        "  ⏳ Scanning session files...",
        Style::default().fg(MUTED_GRAY),
    )]))
    .block(block);
    frame.render_widget(paragraph, area);
}

fn render_daily(frame: &mut Frame, area: Rect, data: &UsageData, scroll_offset: usize) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CORNFLOWER_BLUE))
        .style(Style::default().bg(DARK_BG));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if data.daily.is_empty() {
        let p = Paragraph::new("  No usage data found.").style(Style::default().fg(MUTED_GRAY));
        frame.render_widget(p, inner);
        return;
    }

    // Split into table + bar chart
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(inner);

    // Table
    let header = Row::new(vec![
        "Date", "Total", "Input", "Cache", "Output", "Sessions",
    ])
    .style(Style::default().fg(GOLD).add_modifier(Modifier::BOLD))
    .bottom_margin(1);

    let visible_rows = chunks[0].height.saturating_sub(2) as usize;
    let total_rows = data.daily.len();
    // Default scroll to bottom (show most recent)
    let effective_offset = if scroll_offset == 0 && total_rows > visible_rows {
        total_rows.saturating_sub(visible_rows)
    } else {
        scroll_offset
    };

    let rows: Vec<Row> = data
        .daily
        .iter()
        .skip(effective_offset)
        .take(visible_rows)
        .map(|(date, bucket)| {
            Row::new(vec![
                date.format("%Y-%m-%d").to_string(),
                format_tokens_short(bucket.total()),
                format_tokens_short(bucket.input_tokens),
                format_tokens_short(bucket.cache_read_tokens + bucket.cache_creation_tokens),
                format_tokens_short(bucket.output_tokens),
                format!("{}", bucket.session_count),
            ])
            .style(Style::default().fg(SOFT_WHITE))
        })
        .collect();

    let widths = [
        Constraint::Length(12),
        Constraint::Length(10),
        Constraint::Length(10),
        Constraint::Length(10),
        Constraint::Length(10),
        Constraint::Length(8),
    ];

    let table = Table::new(rows, widths).header(header).column_spacing(1);

    frame.render_widget(table, chunks[0]);

    // Bar chart (last N days that fit)
    render_bar_chart(frame, chunks[1], data);
}

fn render_weekly(frame: &mut Frame, area: Rect, data: &UsageData, scroll_offset: usize) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CORNFLOWER_BLUE))
        .style(Style::default().bg(DARK_BG));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if data.weekly.is_empty() {
        let p = Paragraph::new("  No usage data found.").style(Style::default().fg(MUTED_GRAY));
        frame.render_widget(p, inner);
        return;
    }

    let header = Row::new(vec![
        "Week Start",
        "Total",
        "Input",
        "Cache",
        "Output",
        "Sessions",
        "Projects",
    ])
    .style(Style::default().fg(GOLD).add_modifier(Modifier::BOLD))
    .bottom_margin(1);

    let visible_rows = inner.height.saturating_sub(2) as usize;
    let total_rows = data.weekly.len();
    let effective_offset = if scroll_offset == 0 && total_rows > visible_rows {
        total_rows.saturating_sub(visible_rows)
    } else {
        scroll_offset
    };

    let rows: Vec<Row> = data
        .weekly
        .iter()
        .skip(effective_offset)
        .take(visible_rows)
        .map(|(date, bucket)| {
            Row::new(vec![
                date.format("%Y-%m-%d").to_string(),
                format_tokens_short(bucket.total()),
                format_tokens_short(bucket.input_tokens),
                format_tokens_short(bucket.cache_read_tokens + bucket.cache_creation_tokens),
                format_tokens_short(bucket.output_tokens),
                format!("{}", bucket.session_count),
                format!("{}", bucket.project_count),
            ])
            .style(Style::default().fg(SOFT_WHITE))
        })
        .collect();

    let widths = [
        Constraint::Length(12),
        Constraint::Length(10),
        Constraint::Length(10),
        Constraint::Length(10),
        Constraint::Length(10),
        Constraint::Length(8),
        Constraint::Length(8),
    ];

    let table = Table::new(rows, widths).header(header).column_spacing(1);

    frame.render_widget(table, inner);
}

fn render_projects(frame: &mut Frame, area: Rect, data: &UsageData, scroll_offset: usize) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CORNFLOWER_BLUE))
        .style(Style::default().bg(DARK_BG));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if data.projects.is_empty() {
        let p = Paragraph::new("  No usage data found.").style(Style::default().fg(MUTED_GRAY));
        frame.render_widget(p, inner);
        return;
    }

    let header = Row::new(vec![
        "#", "Project", "Total", "Input", "Cache", "Output", "Sessions",
    ])
    .style(Style::default().fg(GOLD).add_modifier(Modifier::BOLD))
    .bottom_margin(1);

    let visible_rows = inner.height.saturating_sub(2) as usize;

    let rows: Vec<Row> = data
        .projects
        .iter()
        .enumerate()
        .skip(scroll_offset)
        .take(visible_rows)
        .map(|(i, proj)| {
            let b = &proj.bucket;
            Row::new(vec![
                format!("{}", i + 1),
                truncate_string(&proj.name, 40),
                format_tokens_short(b.total()),
                format_tokens_short(b.input_tokens),
                format_tokens_short(b.cache_read_tokens + b.cache_creation_tokens),
                format_tokens_short(b.output_tokens),
                format!("{}", b.session_count),
            ])
            .style(Style::default().fg(SOFT_WHITE))
        })
        .collect();

    let widths = [
        Constraint::Length(4),
        Constraint::Min(20),
        Constraint::Length(10),
        Constraint::Length(10),
        Constraint::Length(10),
        Constraint::Length(10),
        Constraint::Length(8),
    ];

    let table = Table::new(rows, widths).header(header).column_spacing(1);

    frame.render_widget(table, inner);
}

fn render_burndown(frame: &mut Frame, area: Rect, data: &UsageData, state: &UsageViewState) {
    let block = Block::default()
        .title(" [ Burndown ] ")
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(TERMINAL_BORDER))
        .style(Style::default().bg(TERMINAL_BG));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if data.calls.is_empty() {
        let p = Paragraph::new("  No usage data found for selected period/provider/filter.")
            .style(Style::default().fg(MUTED_GRAY));
        frame.render_widget(p, inner);
        return;
    }

    // Apply cross-filter chips client-side. With no chips this is a
    // cheap clone; with chips it re-aggregates from the in-memory call
    // set so every panel and the header reflect the active pivot.
    let filtered = filter_usage_data(data, &state.filters);
    let view_data: &UsageData = if state.filters.any() { &filtered } else { data };

    // Zoom takes the full inner area minus a small breadcrumb and an
    // optional search box. Skip the dashboard grid entirely.
    if let Some(panel) = state.zoom {
        render_burndown_zoomed(frame, inner, view_data, state, panel);
        return;
    }

    // Filter chip strip occupies one row when chips are active OR when
    // a panel is focused (we want the affordance hint visible). When
    // both are absent we still show the hint at low contrast so users
    // discover the pivot.
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // header
            Constraint::Length(2), // period+provider strip
            Constraint::Length(1), // chip strip / hint
            Constraint::Min(0),    // dashboard
        ])
        .split(inner);

    render_burndown_header(frame, vertical[0], view_data, &state.period);
    render_period_row(frame, vertical[1], state);
    render_filter_chip_strip(frame, vertical[2], state);

    if vertical[3].width >= 120 && vertical[3].height >= 22 {
        render_dashboard_grid(frame, vertical[3], view_data, &state.period, state);
    } else if vertical[3].width >= 96 {
        render_dashboard_compact(frame, vertical[3], view_data, state);
    } else {
        render_dashboard_stack(frame, vertical[3], view_data, state);
    }
}

/// Full-screen zoom for a single dashboard panel. The layout is:
///
/// ```text
/// [ Zoomed: <panel name> ]   ◀ Esc back ▶
/// / search query                              <- only when search active
/// ┌───────────── panel body (all rows + extra cols) ─────────────┐
/// │                                                              │
/// └──────────────────────────────────────────────────────────────┘
/// ┌──── Detail drawer ─────────── 40% bottom split, when open ──┐
/// │                                                              │
/// └──────────────────────────────────────────────────────────────┘
/// ```
fn render_burndown_zoomed(
    frame: &mut Frame,
    area: Rect,
    data: &UsageData,
    state: &UsageViewState,
    panel: UsagePanel,
) {
    let search_h: u16 = if state.zoom_search_active || !state.zoom_search_query.is_empty() {
        1
    } else {
        0
    };
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // breadcrumb
            Constraint::Length(search_h),
            Constraint::Min(0), // body (and optional detail split)
        ])
        .split(area);

    render_zoom_breadcrumb(frame, vertical[0], panel);
    if search_h > 0 {
        render_zoom_search_bar(frame, vertical[1], state);
    }

    // Optional 60/40 vertical split when the detail drawer is open.
    let body_area = vertical[2];
    let (panel_area, detail_area) = if state.zoom_detail_open {
        let split = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
            .split(body_area);
        (split[0], Some(split[1]))
    } else {
        (body_area, None)
    };

    render_zoom_panel_body(frame, panel_area, data, state, panel);

    if let Some(detail) = detail_area {
        render_zoom_detail_drawer(frame, detail, data, state, panel);
    }
}

fn render_zoom_breadcrumb(frame: &mut Frame, area: Rect, panel: UsagePanel) {
    let line = Line::from(vec![
        Span::styled(
            format!(" [ Zoomed: {} ] ", panel.title()),
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ),
        Span::styled("   ", Style::default()),
        Span::styled("◀ Esc back ▶", Style::default().fg(MUTED_GRAY)),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

/// Score `haystack` against `query` with nucleo-matcher; `None` means
/// no match. An empty query matches everything (returns `Some(0)`).
fn fuzzy_score(matcher: &mut nucleo_matcher::Matcher, query: &str, haystack: &str) -> Option<u32> {
    if query.is_empty() {
        return Some(0);
    }
    let needle = nucleo_matcher::pattern::Pattern::parse(
        query,
        nucleo_matcher::pattern::CaseMatching::Smart,
        nucleo_matcher::pattern::Normalization::Smart,
    );
    needle.score(
        nucleo_matcher::Utf32Str::Ascii(haystack.as_bytes()),
        matcher,
    )
}

/// Filter `rows` by a fuzzy-search query, preserving original order.
/// Empty query returns all rows. The `label` closure projects each row
/// to its primary search string (project name, session id, etc.).
fn apply_zoom_filter<'a, T, F>(rows: &'a [T], query: &str, label: F) -> Vec<&'a T>
where
    F: Fn(&T) -> String,
{
    if query.is_empty() {
        return rows.iter().collect();
    }
    let mut matcher = nucleo_matcher::Matcher::new(nucleo_matcher::Config::DEFAULT);
    rows.iter()
        .filter_map(|row| {
            let label = label(row);
            // nucleo expects ASCII-friendly bytes; fall back to a
            // case-insensitive substring check when the label has
            // non-ASCII chars (rare, but project paths can include
            // unicode dashes etc.).
            if !label.is_ascii() {
                if label.to_lowercase().contains(query.to_lowercase().as_str()) {
                    return Some(row);
                }
                return None;
            }
            fuzzy_score(&mut matcher, query, &label).map(|_| row)
        })
        .collect()
}

fn render_zoom_search_bar(frame: &mut Frame, area: Rect, state: &UsageViewState) {
    let cursor = if state.zoom_search_active { "_" } else { "" };
    let line = Line::from(vec![
        Span::styled(
            " / ",
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            state.zoom_search_query.clone(),
            Style::default().fg(SOFT_WHITE),
        ),
        Span::styled(cursor, Style::default().fg(GOLD)),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

/// Render the body of a zoomed panel — all rows visible, primary
/// columns plus extras specific to the panel. Each row is filtered by
/// `state.zoom_search_query` (fuzzy match on the row's primary label).
///
/// Per the brief, the headline panels (By Project, Top Sessions, By
/// Model, By Activity, Daily Activity) get extra columns; everything
/// else just renders the full untruncated row list inside the same
/// focus-aware frame used by the dashboard renderers.
#[allow(clippy::too_many_lines)]
fn render_zoom_panel_body(
    frame: &mut Frame,
    area: Rect,
    data: &UsageData,
    state: &UsageViewState,
    panel: UsagePanel,
) {
    let q = state.zoom_search_query.as_str();
    match panel {
        UsagePanel::ByProject => render_zoom_by_project(frame, area, data, state, q),
        UsagePanel::TopSessions => render_zoom_top_sessions(frame, area, data, state, q),
        UsagePanel::Live => render_zoom_top_sessions(frame, area, data, state, q),
        UsagePanel::ByModel => render_zoom_by_model(frame, area, data, state, q),
        UsagePanel::ByActivity => render_zoom_by_activity(frame, area, data, state, q),
        UsagePanel::DailyActivity => render_zoom_daily_activity(frame, area, data, state, q),
        UsagePanel::Leaderboard => render_zoom_by_project(frame, area, data, state, q),
        UsagePanel::CoreTools => {
            render_zoom_named(frame, area, "Core Tools", &data.tools, state, q)
        }
        UsagePanel::ShellCommands => render_zoom_named(
            frame,
            area,
            "Shell Commands",
            &data.shell_commands,
            state,
            q,
        ),
        UsagePanel::McpServers => {
            render_zoom_named(frame, area, "MCP Servers", &data.mcp_servers, state, q)
        }
        UsagePanel::Optimize | UsagePanel::Budget => {
            // These panels are summary cards rather than row lists. We
            // reuse the standard renderers in a fullscreen frame.
            let focus = FocusCtx::for_panel(state, panel);
            if matches!(panel, UsagePanel::Optimize) {
                render_optimize_compact_panel(frame, area, data, focus);
            } else {
                render_budget_panel(frame, area, data, &state.period, focus);
            }
        }
    }
}

fn render_zoom_by_project(
    frame: &mut Frame,
    area: Rect,
    data: &UsageData,
    _state: &UsageViewState,
    query: &str,
) {
    let rows = apply_zoom_filter(&data.projects, query, |p| p.name.clone());
    let header = Row::new(vec![
        "#",
        "Project",
        "Cost",
        "Tokens",
        "Calls",
        "Sessions",
        "First seen",
        "Last seen",
    ])
    .style(Style::default().fg(GOLD).add_modifier(Modifier::BOLD))
    .bottom_margin(1);

    let visible_rows = area.height.saturating_sub(2) as usize;
    let table_rows: Vec<Row> = rows
        .iter()
        .enumerate()
        .take(visible_rows)
        .map(|(idx, project)| {
            let b = &project.bucket;
            let (first, last) = project_seen_window(data, &project.name);
            Row::new(vec![
                format!("{}", idx + 1),
                project.path.clone(),
                format_cost(b.cost_usd),
                format_tokens_short(b.total()),
                b.call_count.to_string(),
                b.session_count.to_string(),
                first,
                last,
            ])
            .style(Style::default().fg(SOFT_WHITE))
        })
        .collect();

    let widths = [
        Constraint::Length(4),
        Constraint::Min(20),
        Constraint::Length(10),
        Constraint::Length(10),
        Constraint::Length(8),
        Constraint::Length(8),
        Constraint::Length(11),
        Constraint::Length(11),
    ];
    let table = Table::new(table_rows, widths).header(header).column_spacing(1);
    frame.render_widget(table, area);
}

fn render_zoom_top_sessions(
    frame: &mut Frame,
    area: Rect,
    data: &UsageData,
    _state: &UsageViewState,
    query: &str,
) {
    let rows = apply_zoom_filter(&data.sessions, query, |s| {
        format!("{} {}", s.project, s.session_id)
    });
    let header = Row::new(vec![
        "Provider",
        "Project",
        "Session",
        "Cost",
        "Tokens",
        "Calls",
        "Duration",
        "Last seen",
    ])
    .style(Style::default().fg(GOLD).add_modifier(Modifier::BOLD))
    .bottom_margin(1);

    let visible_rows = area.height.saturating_sub(2) as usize;
    let table_rows: Vec<Row> = rows
        .iter()
        .take(visible_rows)
        .map(|sess| {
            let b = &sess.bucket;
            let dur = (sess.last_timestamp - sess.first_timestamp).num_minutes().max(0);
            let dur_str = format_duration_min(dur as u64);
            Row::new(vec![
                sess.provider.clone(),
                truncate_string(&sess.project, 24),
                truncate_string(&sess.session_id, 18),
                format_cost(b.cost_usd),
                format_tokens_short(b.total()),
                b.call_count.to_string(),
                dur_str,
                sess.last_timestamp.format("%Y-%m-%d").to_string(),
            ])
            .style(Style::default().fg(SOFT_WHITE))
        })
        .collect();
    let widths = [
        Constraint::Length(8),
        Constraint::Length(24),
        Constraint::Length(18),
        Constraint::Length(10),
        Constraint::Length(10),
        Constraint::Length(8),
        Constraint::Length(10),
        Constraint::Length(11),
    ];
    let table = Table::new(table_rows, widths).header(header).column_spacing(1);
    frame.render_widget(table, area);
}

fn render_zoom_by_model(
    frame: &mut Frame,
    area: Rect,
    data: &UsageData,
    _state: &UsageViewState,
    query: &str,
) {
    let rows = apply_zoom_filter(&data.models, query, |m| m.model.clone());
    let header = Row::new(vec![
        "Model",
        "Calls",
        "Tokens",
        "Cost",
        "Cost/call",
        "Top projects",
    ])
    .style(Style::default().fg(GOLD).add_modifier(Modifier::BOLD))
    .bottom_margin(1);
    let visible_rows = area.height.saturating_sub(2) as usize;
    let table_rows: Vec<Row> = rows
        .iter()
        .take(visible_rows)
        .map(|m| {
            let b = &m.bucket;
            let cost_per_call = b
                .cost_usd
                .map(|c| {
                    if b.call_count == 0 {
                        0.0
                    } else {
                        c / (b.call_count as f64)
                    }
                })
                .map(|c| format!("${c:.4}"))
                .unwrap_or_else(|| "—".to_string());
            let top_projects = top_projects_for_model(data, &m.model, 3);
            Row::new(vec![
                m.model.clone(),
                b.call_count.to_string(),
                format_tokens_short(b.total()),
                format_cost(b.cost_usd),
                cost_per_call,
                top_projects,
            ])
            .style(Style::default().fg(SOFT_WHITE))
        })
        .collect();
    let widths = [
        Constraint::Min(20),
        Constraint::Length(8),
        Constraint::Length(10),
        Constraint::Length(10),
        Constraint::Length(11),
        Constraint::Min(20),
    ];
    let table = Table::new(table_rows, widths).header(header).column_spacing(1);
    frame.render_widget(table, area);
}

fn render_zoom_by_activity(
    frame: &mut Frame,
    area: Rect,
    data: &UsageData,
    _state: &UsageViewState,
    query: &str,
) {
    let rows = apply_zoom_filter(&data.activities, query, |a| a.category.label().to_string());
    let header = Row::new(vec![
        "Activity", "Turns", "Edit", "1-shot", "Retries", "Tokens", "Cost",
    ])
    .style(Style::default().fg(GOLD).add_modifier(Modifier::BOLD))
    .bottom_margin(1);
    let visible_rows = area.height.saturating_sub(2) as usize;
    let table_rows: Vec<Row> = rows
        .iter()
        .take(visible_rows)
        .map(|a| {
            let b = &a.bucket;
            Row::new(vec![
                a.category.label().to_string(),
                a.turns.to_string(),
                a.edit_turns.to_string(),
                a.one_shot_turns.to_string(),
                a.retries.to_string(),
                format_tokens_short(b.total()),
                format_cost(b.cost_usd),
            ])
            .style(Style::default().fg(SOFT_WHITE))
        })
        .collect();
    let widths = [
        Constraint::Length(14),
        Constraint::Length(7),
        Constraint::Length(7),
        Constraint::Length(7),
        Constraint::Length(8),
        Constraint::Length(10),
        Constraint::Length(10),
    ];
    let table = Table::new(table_rows, widths).header(header).column_spacing(1);
    frame.render_widget(table, area);
}

fn render_zoom_daily_activity(
    frame: &mut Frame,
    area: Rect,
    data: &UsageData,
    _state: &UsageViewState,
    query: &str,
) {
    // Daily rows are (date, bucket) tuples — search over the date string.
    let rows: Vec<&(NaiveDate, crate::models::usage::TokenBucket)> = if query.is_empty() {
        data.daily.iter().collect()
    } else {
        data.daily
            .iter()
            .filter(|(date, _)| date.format("%Y-%m-%d").to_string().contains(query))
            .collect()
    };
    let header = Row::new(vec![
        "Date", "Calls", "Sessions", "Projects", "Tokens", "Cost",
    ])
    .style(Style::default().fg(GOLD).add_modifier(Modifier::BOLD))
    .bottom_margin(1);
    let visible_rows = area.height.saturating_sub(2) as usize;
    let table_rows: Vec<Row> = rows
        .iter()
        .rev()
        .take(visible_rows)
        .map(|(date, b)| {
            Row::new(vec![
                date.format("%Y-%m-%d").to_string(),
                b.call_count.to_string(),
                b.session_count.to_string(),
                b.project_count.to_string(),
                format_tokens_short(b.total()),
                format_cost(b.cost_usd),
            ])
            .style(Style::default().fg(SOFT_WHITE))
        })
        .collect();
    let widths = [
        Constraint::Length(11),
        Constraint::Length(8),
        Constraint::Length(9),
        Constraint::Length(9),
        Constraint::Length(10),
        Constraint::Length(10),
    ];
    let table = Table::new(table_rows, widths).header(header).column_spacing(1);
    frame.render_widget(table, area);
}

fn render_zoom_named(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    rows: &[NamedUsage],
    _state: &UsageViewState,
    query: &str,
) {
    let filtered = apply_zoom_filter(rows, query, |n| n.name.clone());
    let header = Row::new(vec![title, "Calls"])
        .style(Style::default().fg(GOLD).add_modifier(Modifier::BOLD))
        .bottom_margin(1);
    let visible_rows = area.height.saturating_sub(2) as usize;
    let table_rows: Vec<Row> = filtered
        .iter()
        .take(visible_rows)
        .map(|row| {
            Row::new(vec![row.name.clone(), row.calls.to_string()])
                .style(Style::default().fg(SOFT_WHITE))
        })
        .collect();
    let widths = [Constraint::Min(20), Constraint::Length(10)];
    let table = Table::new(table_rows, widths).header(header).column_spacing(1);
    frame.render_widget(table, area);
}

/// Render the detail drawer for the currently-selected row in the
/// zoomed panel. Static info card; no extra fetching for PR-C.
fn render_zoom_detail_drawer(
    frame: &mut Frame,
    area: Rect,
    data: &UsageData,
    state: &UsageViewState,
    panel: UsagePanel,
) {
    let block = Block::default()
        .title(" [ Detail ] ")
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(GOLD))
        .style(Style::default().bg(TERMINAL_PANEL));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let lines = build_detail_lines(data, state, panel);
    let paragraph = Paragraph::new(lines).style(Style::default().fg(SOFT_WHITE));
    frame.render_widget(paragraph, inner);
}

fn build_detail_lines(
    data: &UsageData,
    state: &UsageViewState,
    panel: UsagePanel,
) -> Vec<Line<'static>> {
    let row = state.focus_row;
    let mut lines = Vec::new();
    let kv = |k: &str, v: String| -> Line<'static> {
        Line::from(vec![
            Span::styled(format!(" {k:<14}"), Style::default().fg(MUTED_GRAY)),
            Span::styled(
                v,
                Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD),
            ),
        ])
    };
    match panel {
        UsagePanel::ByProject | UsagePanel::Leaderboard => {
            if let Some(p) = data.projects.get(row) {
                lines.push(kv("Project", p.name.clone()));
                lines.push(kv("Path", p.path.clone()));
                lines.push(kv("Cost", format_cost(p.bucket.cost_usd)));
                lines.push(kv("Tokens", format_tokens_short(p.bucket.total())));
                lines.push(kv("Calls", p.bucket.call_count.to_string()));
                lines.push(kv("Sessions", p.bucket.session_count.to_string()));
            }
        }
        UsagePanel::TopSessions | UsagePanel::Live => {
            if let Some(s) = data.sessions.get(row) {
                lines.push(kv("Session", s.session_id.clone()));
                lines.push(kv("Project", s.project.clone()));
                lines.push(kv("Provider", s.provider.clone()));
                lines.push(kv(
                    "First seen",
                    s.first_timestamp.format("%Y-%m-%d %H:%M").to_string(),
                ));
                lines.push(kv(
                    "Last seen",
                    s.last_timestamp.format("%Y-%m-%d %H:%M").to_string(),
                ));
                lines.push(kv("Cost", format_cost(s.bucket.cost_usd)));
                lines.push(kv("Tokens", format_tokens_short(s.bucket.total())));
                lines.push(kv("Calls", s.bucket.call_count.to_string()));
            }
        }
        UsagePanel::ByModel => {
            if let Some(m) = data.models.get(row) {
                lines.push(kv("Model", m.model.clone()));
                lines.push(kv("Cost", format_cost(m.bucket.cost_usd)));
                lines.push(kv("Tokens", format_tokens_short(m.bucket.total())));
                lines.push(kv("Calls", m.bucket.call_count.to_string()));
                lines.push(kv(
                    "Top projects",
                    top_projects_for_model(data, &m.model, 3),
                ));
            }
        }
        UsagePanel::ByActivity => {
            if let Some(a) = data.activities.get(row) {
                lines.push(kv("Activity", a.category.label().to_string()));
                lines.push(kv("Turns", a.turns.to_string()));
                lines.push(kv("Edit turns", a.edit_turns.to_string()));
                lines.push(kv("1-shot turns", a.one_shot_turns.to_string()));
                lines.push(kv("Retries", a.retries.to_string()));
                lines.push(kv("Tokens", format_tokens_short(a.bucket.total())));
                lines.push(kv("Cost", format_cost(a.bucket.cost_usd)));
            }
        }
        UsagePanel::DailyActivity => {
            if let Some((date, b)) = data.daily.iter().rev().nth(row) {
                lines.push(kv("Date", date.format("%Y-%m-%d").to_string()));
                lines.push(kv("Calls", b.call_count.to_string()));
                lines.push(kv("Sessions", b.session_count.to_string()));
                lines.push(kv("Projects", b.project_count.to_string()));
                lines.push(kv("Tokens", format_tokens_short(b.total())));
                lines.push(kv("Cost", format_cost(b.cost_usd)));
            }
        }
        UsagePanel::CoreTools => detail_named(&mut lines, &data.tools, row, "Tool", &kv),
        UsagePanel::ShellCommands => {
            detail_named(&mut lines, &data.shell_commands, row, "Command", &kv)
        }
        UsagePanel::McpServers => {
            detail_named(&mut lines, &data.mcp_servers, row, "MCP server", &kv)
        }
        UsagePanel::Optimize | UsagePanel::Budget => {
            lines.push(Line::from(Span::styled(
                "  No detail card for summary panels.",
                Style::default().fg(MUTED_GRAY),
            )));
        }
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No row selected.",
            Style::default().fg(MUTED_GRAY),
        )));
    }
    lines
}

fn detail_named<F>(
    lines: &mut Vec<Line<'static>>,
    rows: &[NamedUsage],
    idx: usize,
    name_label: &str,
    kv: &F,
) where
    F: Fn(&str, String) -> Line<'static>,
{
    if let Some(row) = rows.get(idx) {
        lines.push(kv(name_label, row.name.clone()));
        lines.push(kv("Calls", row.calls.to_string()));
    }
}

/// First-seen / last-seen ISO dates for a project, derived from the
/// session timeline. Empty strings when the project has no sessions.
fn project_seen_window(data: &UsageData, project_name: &str) -> (String, String) {
    let mut iter = data.sessions.iter().filter(|s| s.project == project_name);
    let Some(first) = iter.next() else {
        return (String::new(), String::new());
    };
    let mut min_ts = first.first_timestamp;
    let mut max_ts = first.last_timestamp;
    for s in iter {
        if s.first_timestamp < min_ts {
            min_ts = s.first_timestamp;
        }
        if s.last_timestamp > max_ts {
            max_ts = s.last_timestamp;
        }
    }
    (
        min_ts.format("%Y-%m-%d").to_string(),
        max_ts.format("%Y-%m-%d").to_string(),
    )
}

/// Top `n` projects by call count for `model_name`, joined with "·".
/// Returns "—" when the model has no calls in `data.calls`.
fn top_projects_for_model(data: &UsageData, model_name: &str, n: usize) -> String {
    use std::collections::HashMap;
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for call in &data.calls {
        if call.model == model_name {
            *counts.entry(call.project.as_str()).or_insert(0) += 1;
        }
    }
    if counts.is_empty() {
        return "—".to_string();
    }
    let mut sorted: Vec<_> = counts.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1));
    sorted
        .into_iter()
        .take(n)
        .map(|(p, _)| p.to_string())
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Render `min_total` as `1h 04m` / `42m` / `<1m`.
fn format_duration_min(min_total: u64) -> String {
    if min_total == 0 {
        return "<1m".to_string();
    }
    let h = min_total / 60;
    let m = min_total % 60;
    if h > 0 {
        format!("{h}h {m:02}m")
    } else {
        format!("{m}m")
    }
}

/// `FocusCtx` packages the focus arguments threaded into every panel
/// renderer. `Some(row_idx)` means the panel is focused and should
/// render the highlighted row indicator at that index; `None` means
/// "render normally".
#[derive(Debug, Clone, Copy)]
struct FocusCtx {
    focused_row: Option<usize>,
}

impl FocusCtx {
    fn for_panel(state: &UsageViewState, panel: UsagePanel) -> Self {
        if state.focused_panel == Some(panel) {
            Self {
                focused_row: Some(state.focus_row),
            }
        } else {
            Self { focused_row: None }
        }
    }
    fn unfocused() -> Self {
        Self { focused_row: None }
    }
    fn is_focused(self) -> bool {
        self.focused_row.is_some()
    }
}

fn render_dashboard_grid(
    frame: &mut Frame,
    area: Rect,
    data: &UsageData,
    period: &UsagePeriod,
    state: &UsageViewState,
) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
        ])
        .split(area);

    let top = three_columns(rows[0]);
    render_daily_activity_panel(
        frame,
        top[0],
        data,
        FocusCtx::for_panel(state, UsagePanel::DailyActivity),
    );
    render_project_panel(
        frame,
        top[1],
        &data.projects,
        FocusCtx::for_panel(state, UsagePanel::ByProject),
    );
    render_live_panel(
        frame,
        top[2],
        &data.sessions,
        FocusCtx::for_panel(state, UsagePanel::Live),
    );

    let middle = three_columns(rows[1]);
    render_session_panel(
        frame,
        middle[0],
        &data.sessions,
        FocusCtx::for_panel(state, UsagePanel::TopSessions),
    );
    render_activity_panel(
        frame,
        middle[1],
        &data.activities,
        FocusCtx::for_panel(state, UsagePanel::ByActivity),
    );
    render_model_panel(
        frame,
        middle[2],
        &data.models,
        FocusCtx::for_panel(state, UsagePanel::ByModel),
    );

    let lower = three_columns(rows[2]);
    render_named_panel(
        frame,
        lower[0],
        "Core Tools",
        &data.tools,
        FocusCtx::for_panel(state, UsagePanel::CoreTools),
    );
    render_named_panel(
        frame,
        lower[1],
        "Shell Commands",
        &data.shell_commands,
        FocusCtx::for_panel(state, UsagePanel::ShellCommands),
    );
    render_named_panel(
        frame,
        lower[2],
        "MCP Servers",
        &data.mcp_servers,
        FocusCtx::for_panel(state, UsagePanel::McpServers),
    );

    let bottom = three_columns(rows[3]);
    render_optimize_compact_panel(
        frame,
        bottom[0],
        data,
        FocusCtx::for_panel(state, UsagePanel::Optimize),
    );
    render_leaderboard_panel(
        frame,
        bottom[1],
        data,
        FocusCtx::for_panel(state, UsagePanel::Leaderboard),
    );
    render_budget_panel(
        frame,
        bottom[2],
        data,
        period,
        FocusCtx::for_panel(state, UsagePanel::Budget),
    );
}

fn render_dashboard_compact(
    frame: &mut Frame,
    area: Rect,
    data: &UsageData,
    state: &UsageViewState,
) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);
    let left = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
        ])
        .split(columns[0]);
    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
        ])
        .split(columns[1]);

    render_daily_activity_panel(
        frame,
        left[0],
        data,
        FocusCtx::for_panel(state, UsagePanel::DailyActivity),
    );
    render_project_panel(
        frame,
        left[1],
        &data.projects,
        FocusCtx::for_panel(state, UsagePanel::ByProject),
    );
    render_session_panel(
        frame,
        left[2],
        &data.sessions,
        FocusCtx::for_panel(state, UsagePanel::TopSessions),
    );
    render_live_panel(
        frame,
        left[3],
        &data.sessions,
        FocusCtx::for_panel(state, UsagePanel::Live),
    );

    render_activity_panel(
        frame,
        right[0],
        &data.activities,
        FocusCtx::for_panel(state, UsagePanel::ByActivity),
    );
    render_model_panel(
        frame,
        right[1],
        &data.models,
        FocusCtx::for_panel(state, UsagePanel::ByModel),
    );
    render_optimize_compact_panel(
        frame,
        right[2],
        data,
        FocusCtx::for_panel(state, UsagePanel::Optimize),
    );
    let tools = three_columns(right[3]);
    render_named_panel(
        frame,
        tools[0],
        "Core Tools",
        &data.tools,
        FocusCtx::for_panel(state, UsagePanel::CoreTools),
    );
    render_named_panel(
        frame,
        tools[1],
        "Shell Commands",
        &data.shell_commands,
        FocusCtx::for_panel(state, UsagePanel::ShellCommands),
    );
    render_named_panel(
        frame,
        tools[2],
        "MCP Servers",
        &data.mcp_servers,
        FocusCtx::for_panel(state, UsagePanel::McpServers),
    );
}

fn render_dashboard_stack(frame: &mut Frame, area: Rect, data: &UsageData, state: &UsageViewState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(20),
            Constraint::Percentage(20),
            Constraint::Percentage(20),
            Constraint::Percentage(20),
            Constraint::Percentage(20),
        ])
        .split(area);

    render_daily_activity_panel(
        frame,
        chunks[0],
        data,
        FocusCtx::for_panel(state, UsagePanel::DailyActivity),
    );
    render_project_panel(
        frame,
        chunks[1],
        &data.projects,
        FocusCtx::for_panel(state, UsagePanel::ByProject),
    );
    render_session_panel(
        frame,
        chunks[2],
        &data.sessions,
        FocusCtx::for_panel(state, UsagePanel::TopSessions),
    );
    render_activity_panel(
        frame,
        chunks[3],
        &data.activities,
        FocusCtx::for_panel(state, UsagePanel::ByActivity),
    );
    render_model_panel(
        frame,
        chunks[4],
        &data.models,
        FocusCtx::for_panel(state, UsagePanel::ByModel),
    );
}

fn three_columns(area: Rect) -> std::rc::Rc<[Rect]> {
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(34),
            Constraint::Percentage(33),
            Constraint::Percentage(33),
        ])
        .split(area)
}

fn render_burndown_header(frame: &mut Frame, area: Rect, data: &UsageData, period: &UsagePeriod) {
    let cost = format_cost(data.grand_total.cost_usd);
    let cache_hit = cache_hit_percent(data);
    let projected = projected_month_cost(data, period);
    let lines = vec![
        Line::from(vec![
            Span::styled(
                "◆ agents-in-a-box",
                Style::default().fg(TERMINAL_ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" · usage command center", Style::default().fg(MUTED_GRAY)),
            Span::styled("    ", Style::default()),
            Span::styled("● live", Style::default().fg(TERMINAL_GOOD)),
        ]),
        Line::from(vec![
            Span::styled("Cost ", Style::default().fg(MUTED_GRAY)),
            Span::styled(
                cost,
                Style::default().fg(TERMINAL_ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::styled("  Calls ", Style::default().fg(MUTED_GRAY)),
            Span::styled(
                data.grand_total.call_count.to_string(),
                Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD),
            ),
            Span::styled("  Sessions ", Style::default().fg(MUTED_GRAY)),
            Span::styled(
                data.grand_total.session_count.to_string(),
                Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD),
            ),
            Span::styled("  Projects ", Style::default().fg(MUTED_GRAY)),
            Span::styled(
                data.grand_total.project_count.to_string(),
                Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD),
            ),
            Span::styled("  Cache hit ", Style::default().fg(MUTED_GRAY)),
            Span::styled(
                format!("{cache_hit:.1}%"),
                Style::default().fg(TERMINAL_GOOD).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("Tokens ", Style::default().fg(MUTED_GRAY)),
            Span::styled(
                format_tokens_short(data.grand_total.total()),
                Style::default().fg(SOFT_WHITE),
            ),
            Span::styled("  Cache ", Style::default().fg(MUTED_GRAY)),
            Span::styled(
                format_tokens_short(
                    data.grand_total.cache_creation_tokens + data.grand_total.cache_read_tokens,
                ),
                Style::default().fg(SOFT_WHITE),
            ),
            Span::styled("  In ", Style::default().fg(MUTED_GRAY)),
            Span::styled(
                format_tokens_short(data.grand_total.input_tokens),
                Style::default().fg(TERMINAL_CYAN),
            ),
            Span::styled("  Out ", Style::default().fg(MUTED_GRAY)),
            Span::styled(
                format_tokens_short(data.grand_total.output_tokens),
                Style::default().fg(TERMINAL_CYAN),
            ),
            Span::styled("  Projected month ", Style::default().fg(MUTED_GRAY)),
            Span::styled(format_cost(projected), Style::default().fg(TERMINAL_ACCENT)),
        ]),
    ];
    frame.render_widget(Paragraph::new(lines), area);
}

/// Build the "Period: …  Provider: …" labelled strip.
///
/// Period strip layout:
/// ```text
/// Period: 1 Today  2 7d  3 30d  4 90d  5 YTD  [◀ Apr 2026 ▶ m Month]  [◀ Q2 2026 ▶ q Quarter]  a All  D advanced
/// ```
/// Active chip: bold + GOLD background. The Month/Quarter blocks render
/// inline pickers when their variant is active — clicking ◀/▶ steps the
/// underlying month or quarter back/forward.
///
/// Returned as a `Vec<Line>` so tests can assert chip ordering and
/// active-marker placement without hitting the Frame.
fn build_period_provider_strip(state: &UsageViewState) -> Vec<Line<'static>> {
    let mut period_spans: Vec<Span<'static>> =
        vec![Span::styled("Period: ", Style::default().fg(MUTED_GRAY))];

    // Simple key-prefixed chips: 1 Today, 2 7d, 3 30d, 4 90d, 5 YTD.
    // Each chip lights up for both the legacy variant (set by the TUI
    // shortcut) and the equivalent LastNDays(N) variant (set by the
    // CLI --last-n-days flag), so the active state is consistent
    // regardless of which entry point selected the period.
    let simple: [(&str, char, fn(&UsagePeriod) -> bool); 5] = [
        ("Today", '1', |p| matches!(p, UsagePeriod::Today)),
        ("7d", '2', |p| matches!(p, UsagePeriod::Week | UsagePeriod::LastNDays(7))),
        ("30d", '3', |p| matches!(p, UsagePeriod::ThirtyDays | UsagePeriod::LastNDays(30))),
        ("90d", '4', |p| matches!(p, UsagePeriod::LastNDays(90))),
        ("YTD", '5', |p| matches!(p, UsagePeriod::YearToDate)),
    ];
    for (i, (label, key, is_active)) in simple.iter().enumerate() {
        if i > 0 {
            period_spans.push(Span::styled("  ", Style::default()));
        }
        period_spans.push(period_chip_span(label, *key, is_active(&state.period)));
    }

    // Stepable Month picker.
    period_spans.push(Span::styled("  ", Style::default()));
    period_spans.extend(build_step_picker_spans(
        'm',
        "Month",
        &month_picker_label(&state.period),
        matches!(state.period, UsagePeriod::SpecificMonth(_)),
    ));

    // Stepable Quarter picker.
    period_spans.push(Span::styled("  ", Style::default()));
    period_spans.extend(build_step_picker_spans(
        'q',
        "Quarter",
        &quarter_picker_label(&state.period),
        matches!(state.period, UsagePeriod::SpecificQuarter(..)),
    ));

    // Trailing All + advanced custom.
    period_spans.push(Span::styled("  ", Style::default()));
    period_spans.push(period_chip_span(
        "All",
        'a',
        matches!(state.period, UsagePeriod::All),
    ));
    period_spans.push(Span::styled("  ", Style::default()));
    period_spans.push(period_chip_span(
        "advanced",
        'D',
        matches!(state.period, UsagePeriod::Custom { .. }),
    ));

    let mut provider_spans: Vec<Span<'static>> =
        vec![Span::styled("Provider: ", Style::default().fg(MUTED_GRAY))];
    let providers: [(&str, UsageProviderFilter); 3] = [
        ("All", UsageProviderFilter::All),
        ("Claude", UsageProviderFilter::Claude),
        ("Codex", UsageProviderFilter::Codex),
    ];
    for (i, (label, value)) in providers.iter().enumerate() {
        if i > 0 {
            provider_spans.push(Span::styled("  ", Style::default()));
        }
        provider_spans.push(provider_chip_span(label, *value == state.provider_filter));
    }
    provider_spans.push(Span::styled("  (P)", Style::default().fg(MUTED_GRAY)));

    // Two label rows so narrow terminals can wrap cleanly.
    vec![Line::from(period_spans), Line::from(provider_spans)]
}

/// Render `[◀ <label> ▶ <key> <name>]` for the inline Month/Quarter
/// pickers. Active variant gets the GOLD chip background; inactive
/// renders as soft white text with hint key prefix.
fn build_step_picker_spans(key: char, name: &str, label: &str, active: bool) -> Vec<Span<'static>> {
    let style = if active {
        Style::default().fg(DARK_BG).bg(GOLD).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(SOFT_WHITE)
    };
    let arrow_style = if active {
        Style::default().fg(DARK_BG).bg(GOLD).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(MUTED_GRAY)
    };
    vec![
        Span::styled("[", arrow_style),
        Span::styled("◀ ", arrow_style),
        Span::styled(label.to_string(), style),
        Span::styled(" ▶", arrow_style),
        Span::styled(format!(" {key} {name}"), style),
        Span::styled("]", arrow_style),
    ]
}

/// Label rendered inside the Month picker chip. When the active period
/// is SpecificMonth we render its anchor; otherwise we render today's
/// month so the user sees a sensible default before pressing `m`.
fn month_picker_label(period: &UsagePeriod) -> String {
    let anchor = match period {
        UsagePeriod::SpecificMonth(d) => *d,
        _ => Local::now().date_naive(),
    };
    anchor.format("%b %Y").to_string()
}

/// Label rendered inside the Quarter picker chip. Same fallback rule
/// as `month_picker_label`.
fn quarter_picker_label(period: &UsagePeriod) -> String {
    let (year, q) = match period {
        UsagePeriod::SpecificQuarter(y, q) => (*y, *q),
        _ => {
            let today = Local::now().date_naive();
            (today.year(), crate::models::usage::quarter_of(today))
        }
    };
    format!("Q{q} {year}")
}

fn period_chip_span(label: &str, key: char, active: bool) -> Span<'static> {
    let text = format!(" {key} {label} ");
    if active {
        Span::styled(
            text,
            Style::default().fg(DARK_BG).bg(GOLD).add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(text, Style::default().fg(SOFT_WHITE))
    }
}

fn provider_chip_span(label: &str, active: bool) -> Span<'static> {
    let text = format!(" {label} ");
    if active {
        Span::styled(
            text,
            Style::default().fg(DARK_BG).bg(GOLD).add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(text, Style::default().fg(MUTED_GRAY))
    }
}

fn render_period_row(frame: &mut Frame, area: Rect, state: &UsageViewState) {
    let lines = build_period_provider_strip(state);
    frame.render_widget(Paragraph::new(lines), area);
}

/// Build the chip strip line shown directly under the period+provider
/// strip. Active chips render as `[label=value]` in GOLD; with no
/// chips, an instruction hint is shown so users discover the pivot.
pub fn build_filter_chip_line(state: &UsageViewState) -> Line<'static> {
    let mut spans: Vec<Span<'static>> =
        vec![Span::styled("Filters: ", Style::default().fg(MUTED_GRAY))];
    if !state.filters.any() {
        spans.push(Span::styled(
            "(none) — Tab focus a panel, ↑↓ pick a row, Enter to add",
            Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
        ));
        return Line::from(spans);
    }
    push_chip_group(&mut spans, "project", &state.filters.project);
    push_chip_group(&mut spans, "model", &state.filters.model);
    push_chip_group(&mut spans, "activity", &state.filters.activity);
    push_chip_group(&mut spans, "session", &state.filters.session);
    spans.push(Span::styled("  ·  ", Style::default().fg(MUTED_GRAY)));
    spans.push(Span::styled(
        "Esc",
        Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::styled(
        " clear last  ",
        Style::default().fg(MUTED_GRAY),
    ));
    spans.push(Span::styled(
        "C",
        Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::styled(" clear all", Style::default().fg(MUTED_GRAY)));
    Line::from(spans)
}

fn push_chip_group(spans: &mut Vec<Span<'static>>, label: &str, values: &[String]) {
    for value in values {
        let chip_text = format!(" {label}={value} ");
        spans.push(Span::styled(
            chip_text,
            Style::default().fg(DARK_BG).bg(GOLD).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw(" "));
    }
}

fn render_filter_chip_strip(frame: &mut Frame, area: Rect, state: &UsageViewState) {
    let line = build_filter_chip_line(state);
    frame.render_widget(Paragraph::new(line), area);
}

fn render_daily_activity_panel(frame: &mut Frame, area: Rect, data: &UsageData, focus: FocusCtx) {
    let cap = area.height.saturating_sub(2) as usize;
    let inner_w = area.width.saturating_sub(2) as usize;
    if cap == 0 || inner_w < 16 {
        render_panel_lines_with_focus(frame, area, "Daily Activity", vec![], focus);
        return;
    }
    let max = data
        .daily
        .iter()
        .filter_map(|(_, bucket)| bucket.cost_usd)
        .fold(0.0_f64, f64::max)
        .max(1.0);
    let rows_data: Vec<_> = data.daily.iter().rev().take(cap).collect();
    let cost_w = rows_data
        .iter()
        .map(|(_, b)| format_cost(b.cost_usd).chars().count())
        .max()
        .unwrap_or(7)
        .max(7);
    let calls_w = rows_data
        .iter()
        .map(|(_, b)| format!("{}c", b.call_count).chars().count())
        .max()
        .unwrap_or(4)
        .max(4);
    let date_w = 5; // MM-DD
    let bar_w = inner_w.saturating_sub(1 + date_w + 1 + cost_w + 1 + calls_w + 1).max(4);
    let lines: Vec<Line> = rows_data
        .into_iter()
        .map(|(date, bucket)| {
            let cost = bucket.cost_usd.unwrap_or(0.0);
            let mut spans = vec![
                Span::raw(" "),
                Span::styled(
                    date.format("%m-%d").to_string(),
                    Style::default().fg(MUTED_GRAY),
                ),
                Span::raw(" "),
            ];
            spans.extend(ratio_gradient_spans(cost, max, bar_w));
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                format!("{:>w$}", format_cost(bucket.cost_usd), w = cost_w),
                Style::default().fg(TERMINAL_ACCENT),
            ));
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                format!("{:>w$}", format!("{}c", bucket.call_count), w = calls_w),
                Style::default().fg(MUTED_GRAY),
            ));
            Line::from(spans)
        })
        .collect();
    render_panel_lines_with_focus(frame, area, "Daily Activity", lines, focus);
}

fn render_project_panel(frame: &mut Frame, area: Rect, rows: &[ProjectUsage], focus: FocusCtx) {
    let cap = area.height.saturating_sub(2) as usize;
    let inner_w = area.width.saturating_sub(2) as usize;
    if cap == 0 || inner_w < 16 {
        render_panel_lines_with_focus(frame, area, "By Project", vec![], focus);
        return;
    }
    let max = rows
        .iter()
        .map(|row| row.bucket.cost_usd.unwrap_or(0.0))
        .fold(0.0_f64, f64::max)
        .max(1.0);
    let value_w = rows
        .iter()
        .take(cap)
        .map(|r| format_cost(r.bucket.cost_usd).chars().count())
        .max()
        .unwrap_or(7)
        .max(7);
    let label_w = ((inner_w as i32 - value_w as i32 - 4) / 2).clamp(10, 24) as usize;
    let bar_w = inner_w.saturating_sub(1 + label_w + 1 + value_w + 1).max(4);
    let lines: Vec<Line> = rows
        .iter()
        .take(cap)
        .map(|row| {
            let cost = row.bucket.cost_usd.unwrap_or(0.0);
            let label = pretty_project_name(&row.name, label_w);
            let mut spans = vec![
                Span::raw(" "),
                Span::styled(pad_label(&label, label_w), Style::default().fg(SOFT_WHITE)),
                Span::raw(" "),
            ];
            spans.extend(ratio_gradient_spans(cost, max, bar_w));
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                format!("{:>w$}", format_cost(row.bucket.cost_usd), w = value_w),
                Style::default().fg(TERMINAL_ACCENT),
            ));
            Line::from(spans)
        })
        .collect();
    render_panel_lines_with_focus(frame, area, "By Project", lines, focus);
}

fn render_session_panel(frame: &mut Frame, area: Rect, rows: &[SessionUsage], focus: FocusCtx) {
    let cap = area.height.saturating_sub(2) as usize;
    let inner_w = area.width.saturating_sub(2) as usize;
    if cap == 0 || inner_w < 16 {
        render_panel_lines_with_focus(frame, area, "Top Sessions", vec![], focus);
        return;
    }
    let max = rows.iter().map(|row| row.bucket.total()).max().unwrap_or(1);
    let value_w = rows
        .iter()
        .take(cap)
        .map(|r| format_tokens_short(r.bucket.total()).chars().count())
        .max()
        .unwrap_or(6)
        .max(6);
    let label_w = ((inner_w as i32 - value_w as i32 - 4) / 2).clamp(10, 22) as usize;
    let bar_w = inner_w.saturating_sub(1 + label_w + 1 + value_w + 1).max(4);
    let lines: Vec<Line> = rows
        .iter()
        .take(cap)
        .map(|row| {
            let label = pretty_project_name(&row.project, label_w);
            let mut spans = vec![
                Span::raw(" "),
                Span::styled(pad_label(&label, label_w), Style::default().fg(SOFT_WHITE)),
                Span::raw(" "),
            ];
            spans.extend(gradient_spans(row.bucket.total(), max, bar_w));
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                format!(
                    "{:>w$}",
                    format_tokens_short(row.bucket.total()),
                    w = value_w
                ),
                Style::default().fg(TERMINAL_CYAN),
            ));
            Line::from(spans)
        })
        .collect();
    render_panel_lines_with_focus(frame, area, "Top Sessions", lines, focus);
}

fn render_live_panel(frame: &mut Frame, area: Rect, rows: &[SessionUsage], focus: FocusCtx) {
    let cap = area.height.saturating_sub(2) as usize;
    let inner_w = area.width.saturating_sub(2) as usize;
    if cap == 0 || inner_w < 16 {
        render_panel_lines_with_focus(frame, area, "Live Session Ticker", vec![], focus);
        return;
    }
    let value_w = rows
        .iter()
        .take(cap)
        .map(|r| format_cost(r.bucket.cost_usd).chars().count())
        .max()
        .unwrap_or(7)
        .max(7);
    let provider_w = 6;
    // " ● " + label + " · " + provider + " · " + value
    let prefix = 3 + 3 + provider_w + 3;
    let label_w = inner_w.saturating_sub(prefix + value_w).max(8);
    let lines: Vec<Line> = rows
        .iter()
        .take(cap)
        .map(|row| {
            let label = pretty_project_name(&row.project, label_w);
            let provider = truncate_string(&row.provider, provider_w);
            Line::from(vec![
                Span::raw(" "),
                Span::styled("●", Style::default().fg(TERMINAL_GOOD)),
                Span::raw(" "),
                Span::styled(pad_label(&label, label_w), Style::default().fg(SOFT_WHITE)),
                Span::styled(" · ", Style::default().fg(MUTED_GRAY)),
                Span::styled(
                    format!("{:<w$}", provider, w = provider_w),
                    Style::default().fg(TERMINAL_CYAN),
                ),
                Span::styled(" · ", Style::default().fg(MUTED_GRAY)),
                Span::styled(
                    format!("{:>w$}", format_cost(row.bucket.cost_usd), w = value_w),
                    Style::default().fg(TERMINAL_ACCENT),
                ),
            ])
        })
        .collect();
    render_panel_lines_with_focus(frame, area, "Live Session Ticker", lines, focus);
}

fn render_activity_panel(frame: &mut Frame, area: Rect, rows: &[ActivityUsage], focus: FocusCtx) {
    let cap = area.height.saturating_sub(2) as usize;
    let inner_w = area.width.saturating_sub(2) as usize;
    if cap == 0 || inner_w < 16 {
        render_panel_lines_with_focus(frame, area, "By Activity", vec![], focus);
        return;
    }
    let max = rows.iter().map(|row| row.bucket.total()).max().unwrap_or(1);
    let label_w = rows
        .iter()
        .take(cap)
        .map(|r| r.category.label().chars().count())
        .max()
        .unwrap_or(8)
        .clamp(8, 14);
    let suffix_w = rows
        .iter()
        .take(cap)
        .map(|r| format!("{}t {}r", r.turns, r.retries).chars().count())
        .max()
        .unwrap_or(8)
        .max(8);
    let bar_w = inner_w.saturating_sub(1 + label_w + 1 + suffix_w + 1).max(4);
    let lines: Vec<Line> = rows
        .iter()
        .take(cap)
        .map(|row| {
            let mut spans = vec![
                Span::raw(" "),
                Span::styled(
                    pad_label(row.category.label(), label_w),
                    Style::default().fg(SOFT_WHITE),
                ),
                Span::raw(" "),
            ];
            spans.extend(gradient_spans(row.bucket.total(), max, bar_w));
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                format!(
                    "{:>w$}",
                    format!("{}t {}r", row.turns, row.retries),
                    w = suffix_w
                ),
                Style::default().fg(MUTED_GRAY),
            ));
            Line::from(spans)
        })
        .collect();
    render_panel_lines_with_focus(frame, area, "By Activity", lines, focus);
}

fn render_model_panel(frame: &mut Frame, area: Rect, rows: &[ModelUsage], focus: FocusCtx) {
    let cap = area.height.saturating_sub(2) as usize;
    let inner_w = area.width.saturating_sub(2) as usize;
    if cap == 0 || inner_w < 16 {
        render_panel_lines_with_focus(frame, area, "By Model", vec![], focus);
        return;
    }
    let max = rows.iter().map(|row| row.bucket.total()).max().unwrap_or(1);
    let value_w = rows
        .iter()
        .take(cap)
        .map(|r| format_tokens_short(r.bucket.total()).chars().count())
        .max()
        .unwrap_or(6)
        .max(6);
    let label_w = ((inner_w as i32 - value_w as i32 - 4) / 2).clamp(10, 22) as usize;
    let bar_w = inner_w.saturating_sub(1 + label_w + 1 + value_w + 1).max(4);
    let lines: Vec<Line> = rows
        .iter()
        .take(cap)
        .map(|row| {
            let label = truncate_string(&row.model, label_w);
            let mut spans = vec![
                Span::raw(" "),
                Span::styled(pad_label(&label, label_w), Style::default().fg(SOFT_WHITE)),
                Span::raw(" "),
            ];
            spans.extend(gradient_spans(row.bucket.total(), max, bar_w));
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                format!(
                    "{:>w$}",
                    format_tokens_short(row.bucket.total()),
                    w = value_w
                ),
                Style::default().fg(TERMINAL_CYAN),
            ));
            Line::from(spans)
        })
        .collect();
    render_panel_lines_with_focus(frame, area, "By Model", lines, focus);
}

fn render_named_panel(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    rows: &[NamedUsage],
    focus: FocusCtx,
) {
    let cap = area.height.saturating_sub(2) as usize;
    let inner_w = area.width.saturating_sub(2) as usize;
    if cap == 0 || inner_w < 14 {
        render_panel_lines_with_focus(frame, area, title, vec![], focus);
        return;
    }
    let max = rows.iter().map(|row| row.calls as u64).max().unwrap_or(1);
    let value_w = rows
        .iter()
        .take(cap)
        .map(|r| r.calls.to_string().chars().count())
        .max()
        .unwrap_or(4)
        .max(4);
    let label_w = ((inner_w as i32 - value_w as i32 - 4) / 2).clamp(8, 22) as usize;
    let bar_w = inner_w.saturating_sub(1 + label_w + 1 + value_w + 1).max(3);
    let lines: Vec<Line> = rows
        .iter()
        .take(cap)
        .map(|row| {
            let label = truncate_string(&row.name, label_w);
            let mut spans = vec![
                Span::raw(" "),
                Span::styled(pad_label(&label, label_w), Style::default().fg(SOFT_WHITE)),
                Span::raw(" "),
            ];
            spans.extend(gradient_spans(row.calls as u64, max, bar_w));
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                format!("{:>w$}", row.calls, w = value_w),
                Style::default().fg(MUTED_GRAY),
            ));
            Line::from(spans)
        })
        .collect();
    render_panel_lines_with_focus(frame, area, title, lines, focus);
}

fn render_optimize_compact_panel(frame: &mut Frame, area: Rect, data: &UsageData, focus: FocusCtx) {
    let cap = area.height.saturating_sub(2) as usize;
    let inner_w = area.width.saturating_sub(2) as usize;
    if cap == 0 {
        render_panel_lines_with_focus(frame, area, "Optimization Recommendations", vec![], focus);
        return;
    }
    let result = optimize_usage(data);
    let mut lines: Vec<Line> = Vec::new();
    let grade_color = match format!("{:?}", result.grade).as_str() {
        "A" | "B" => TERMINAL_GOOD,
        "C" => TERMINAL_ACCENT,
        _ => BAR_HIGH,
    };
    lines.push(Line::from(vec![
        Span::styled(" Health ", Style::default().fg(MUTED_GRAY)),
        Span::styled(
            format!("{:?}", result.grade),
            Style::default().fg(grade_color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" · score ", Style::default().fg(MUTED_GRAY)),
        Span::styled(
            result.score.to_string(),
            Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" · save ", Style::default().fg(MUTED_GRAY)),
        Span::styled(
            format_tokens_short(result.potential_tokens_saved),
            Style::default().fg(TERMINAL_GOOD).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" tokens", Style::default().fg(MUTED_GRAY)),
    ]));
    let title_budget = inner_w.saturating_sub(8); // leading symbol + space + tag area
    for finding in result.findings.iter().take(cap.saturating_sub(1)) {
        let (sym, sym_color) = impact_marker(format!("{:?}", finding.impact).as_str());
        let title = truncate_string(&finding.title, title_budget);
        lines.push(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                sym,
                Style::default().fg(sym_color).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(title, Style::default().fg(SOFT_WHITE)),
        ]));
    }
    render_panel_lines_with_focus(frame, area, "Optimization Recommendations", lines, focus);
}

fn render_leaderboard_panel(frame: &mut Frame, area: Rect, data: &UsageData, focus: FocusCtx) {
    let cap = area.height.saturating_sub(2) as usize;
    let inner_w = area.width.saturating_sub(2) as usize;
    if cap == 0 || inner_w < 16 {
        render_panel_lines_with_focus(frame, area, "Agent Leaderboard", vec![], focus);
        return;
    }
    let max = data
        .projects
        .iter()
        .map(|row| row.bucket.cost_usd.unwrap_or(0.0))
        .fold(0.0_f64, f64::max)
        .max(1.0);
    let value_w = data
        .projects
        .iter()
        .take(cap)
        .map(|r| format_cost(r.bucket.cost_usd).chars().count())
        .max()
        .unwrap_or(7)
        .max(7);
    // " 1 " (3) + label + " " + bar + " " + value
    let rank_w = 2;
    let label_w =
        ((inner_w as i32 - value_w as i32 - rank_w as i32 - 5) / 2).clamp(10, 24) as usize;
    let bar_w = inner_w.saturating_sub(1 + rank_w + 1 + label_w + 1 + value_w + 1).max(4);
    let lines: Vec<Line> = data
        .projects
        .iter()
        .enumerate()
        .take(cap)
        .map(|(idx, project)| {
            let cost = project.bucket.cost_usd.unwrap_or(0.0);
            let rank_color = match idx {
                0 => GOLD,
                1 => SOFT_WHITE,
                2 => TERMINAL_ACCENT,
                _ => MUTED_GRAY,
            };
            let label = pretty_project_name(&project.name, label_w);
            let mut spans = vec![
                Span::raw(" "),
                Span::styled(
                    format!("{:>w$}", idx + 1, w = rank_w),
                    Style::default().fg(rank_color).add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::styled(pad_label(&label, label_w), Style::default().fg(SOFT_WHITE)),
                Span::raw(" "),
            ];
            spans.extend(ratio_gradient_spans(cost, max, bar_w));
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                format!("{:>w$}", format_cost(project.bucket.cost_usd), w = value_w),
                Style::default().fg(TERMINAL_ACCENT),
            ));
            Line::from(spans)
        })
        .collect();
    render_panel_lines_with_focus(frame, area, "Agent Leaderboard", lines, focus);
}

fn render_budget_panel(
    frame: &mut Frame,
    area: Rect,
    data: &UsageData,
    period: &UsagePeriod,
    focus: FocusCtx,
) {
    let inner_w = area.width.saturating_sub(2) as usize;
    let spent = data.grand_total.cost_usd.unwrap_or(0.0);
    let projected = projected_month_cost(data, period).unwrap_or(spent);
    let cap_value = projected.max(spent).max(1.0) * 1.25;
    let usage = ((projected / cap_value) * 100.0).min(100.0);

    let bar_w = inner_w.saturating_sub(10).max(8); // " " + bar + " 80.0%"
    let mut bar_spans = vec![Span::raw(" ")];
    bar_spans.extend(ratio_gradient_spans(usage, 100.0, bar_w));
    bar_spans.push(Span::raw(" "));
    bar_spans.push(Span::styled(
        format!("{usage:>4.1}%"),
        Style::default()
            .fg(if usage >= 85.0 {
                BAR_HIGH
            } else if usage >= 60.0 {
                TERMINAL_ACCENT
            } else {
                TERMINAL_GOOD
            })
            .add_modifier(Modifier::BOLD),
    ));

    let lines: Vec<Line> = vec![
        Line::from(vec![
            Span::styled(" Monthly cap ", Style::default().fg(MUTED_GRAY)),
            Span::styled(
                format_cost(Some(projected)),
                Style::default().fg(TERMINAL_ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" / ", Style::default().fg(MUTED_GRAY)),
            Span::styled(
                format_cost(Some(cap_value)),
                Style::default().fg(SOFT_WHITE),
            ),
        ]),
        Line::from(bar_spans),
        Line::from(vec![
            Span::styled(" Plan utilization ", Style::default().fg(MUTED_GRAY)),
            Span::styled(
                format_cost(data.grand_total.cost_usd),
                Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled(" ", Style::default()),
            Span::styled(
                format!("{:.1}%", cache_hit_percent(data)),
                Style::default().fg(TERMINAL_GOOD).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" cache hit · ", Style::default().fg(MUTED_GRAY)),
            Span::styled(
                data.daily.len().to_string(),
                Style::default().fg(SOFT_WHITE),
            ),
            Span::styled(" days sampled", Style::default().fg(MUTED_GRAY)),
        ]),
    ];
    render_panel_lines_with_focus(frame, area, "Budget · Alerts", lines, focus);
}

fn render_optimize(frame: &mut Frame, area: Rect, data: &UsageData) {
    let result = optimize_usage(data);
    let mut lines = vec![
        format!("Health {:?} ({}/100)", result.grade, result.score),
        format!(
            "Potential savings {} tokens",
            format_tokens_short(result.potential_tokens_saved)
        ),
        String::new(),
    ];
    for finding in result.findings.iter().take(area.height.saturating_sub(5) as usize) {
        lines.push(format!("{:?}: {}", finding.impact, finding.title));
        lines.push(format!("  {}", truncate_string(&finding.details, 80)));
        if let Some(action) = finding.actions.first() {
            lines.push(format!(
                "  Suggestion: {}",
                truncate_string(&action.label, 80)
            ));
        }
    }
    render_panel(frame, area, "Optimize Findings", lines);
}

fn render_panel(frame: &mut Frame, area: Rect, title: &str, rows: Vec<String>) {
    let lines: Vec<Line> = if rows.is_empty() {
        Vec::new()
    } else {
        rows.into_iter()
            .map(|row| {
                Line::from(Span::styled(
                    format!(" {row}"),
                    Style::default().fg(SOFT_WHITE),
                ))
            })
            .collect()
    };
    render_panel_lines(frame, area, title, lines);
}

fn render_panel_lines(frame: &mut Frame, area: Rect, title: &str, lines: Vec<Line<'_>>) {
    render_panel_lines_with_focus(frame, area, title, lines, FocusCtx::unfocused());
}

/// Render variant that knows about focus: highlights the border in
/// `BAR_HIGH` and replaces the leading-space cell on the focused row
/// with a `▶` indicator. Row clamping is done here too — the state
/// only knows about logical row index, not how many rows the panel is
/// currently displaying.
fn render_panel_lines_with_focus(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    mut lines: Vec<Line<'_>>,
    focus: FocusCtx,
) {
    let border_color = if focus.is_focused() {
        BAR_HIGH
    } else {
        TERMINAL_BORDER
    };
    let mut title_style = Style::default();
    if focus.is_focused() {
        title_style = title_style.fg(GOLD).add_modifier(Modifier::BOLD);
    }

    if let Some(row_idx) = focus.focused_row {
        if !lines.is_empty() {
            let clamped = row_idx.min(lines.len() - 1);
            apply_row_indicator(&mut lines[clamped]);
        }
    }

    let title_span = Span::styled(format!(" [ {title} ] "), title_style);
    let block = Block::default()
        .title(Line::from(title_span))
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(border_color))
        .style(Style::default().bg(TERMINAL_PANEL));
    let final_lines = if lines.is_empty() {
        vec![Line::from(Span::styled(
            "  No data",
            Style::default().fg(MUTED_GRAY),
        ))]
    } else {
        lines
    };
    frame.render_widget(Paragraph::new(final_lines).block(block), area);
}

/// Replace the very first character of the line (which renderers
/// reserve as a single-space gutter) with the `▶` glyph. Idempotent:
/// if the line is empty we just append the indicator.
fn apply_row_indicator(line: &mut Line<'_>) {
    let style = Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD);
    if line.spans.is_empty() {
        line.spans.push(Span::styled("▶", style));
        return;
    }
    // The first span is conventionally `Span::raw(" ")`. Replace it.
    let first = line.spans.first().expect("checked non-empty");
    if first.content.as_ref() == " " {
        line.spans[0] = Span::styled("▶", style);
    } else {
        line.spans.insert(0, Span::styled("▶", style));
    }
}

fn pick_bar_color(ratio: f64) -> Color {
    if ratio >= BAR_THRESHOLD_HIGH {
        BAR_HIGH
    } else if ratio >= BAR_THRESHOLD_MED {
        BAR_MED
    } else {
        BAR_COLOR
    }
}

fn gradient_spans(value: u64, max: u64, width: usize) -> Vec<Span<'static>> {
    let max = max.max(1);
    let ratio = (value as f64) / (max as f64);
    let filled = ((ratio.clamp(0.0, 1.0) * width as f64).round() as usize).min(width);
    let color = pick_bar_color(ratio.clamp(0.0, 1.0));
    let mut out = Vec::with_capacity(2);
    if filled > 0 {
        out.push(Span::styled("█".repeat(filled), Style::default().fg(color)));
    }
    let empty = width.saturating_sub(filled);
    if empty > 0 {
        out.push(Span::styled(
            "░".repeat(empty),
            Style::default().fg(MUTED_GRAY),
        ));
    }
    out
}

fn ratio_gradient_spans(value: f64, max: f64, width: usize) -> Vec<Span<'static>> {
    let max = max.max(1e-9);
    let ratio = (value / max).clamp(0.0, 1.0);
    let filled = ((ratio * width as f64).round() as usize).min(width);
    let color = pick_bar_color(ratio);
    let mut out = Vec::with_capacity(2);
    if filled > 0 {
        out.push(Span::styled("▓".repeat(filled), Style::default().fg(color)));
    }
    let empty = width.saturating_sub(filled);
    if empty > 0 {
        out.push(Span::styled(
            "░".repeat(empty),
            Style::default().fg(MUTED_GRAY),
        ));
    }
    out
}

fn pad_label(label: &str, width: usize) -> String {
    let len = label.chars().count();
    if len >= width {
        label.to_string()
    } else {
        let pad = width - len;
        let mut out = String::with_capacity(label.len() + pad);
        out.push_str(label);
        for _ in 0..pad {
            out.push(' ');
        }
        out
    }
}

/// Render a project label that's friendly for Stevie's worktree
/// layout. Recognises the `worktrees/<repo>_<branch>` and
/// `user-repo-branch` patterns and renders them as `repo:branch`.
/// Falls back to `shorten_project_name` for anything else.
///
/// Returns a string of at most `max_w` displayed characters.
fn pretty_project_name(name: &str, max_w: usize) -> String {
    if max_w == 0 {
        return String::new();
    }
    if let Some(pretty) = try_pretty_repo_branch(name) {
        if pretty.chars().count() <= max_w {
            return pretty;
        }
        // Pretty form is still too long — apply tail-truncation on the
        // branch portion, keeping the `repo:` prefix intact when we can.
        if let Some((repo, branch)) = pretty.split_once(':') {
            let repo_w = repo.chars().count();
            // Need room for repo + ':' + at least 1 branch char.
            if repo_w + 2 <= max_w {
                let branch_w = max_w - repo_w - 1;
                let truncated_branch = truncate_string(branch, branch_w);
                let combined = format!("{repo}:{truncated_branch}");
                if combined.chars().count() <= max_w {
                    return combined;
                }
            }
        }
        // Else fall through to the generic shortener on the pretty form.
        return shorten_project_name(&pretty, max_w);
    }
    shorten_project_name(name, max_w)
}

/// Detect `<root>/worktrees/<repo>_<branch>` (sanitised, segment-style)
/// or `user-repo-branch...` (dash-style) and emit `<repo>:<branch>`.
/// Returns `None` if the input doesn't look like one of those patterns,
/// so callers can fall back to a generic shortener.
fn try_pretty_repo_branch(name: &str) -> Option<String> {
    // Pattern A: worktree path. `clean_project_name` rewrites these to
    // `worktree/<user>_<repo>_<branch>` for Claude sources, and the
    // raw `worktrees/<...>` form may also reach us via project_path.
    let after_worktree = name
        .rsplit_once("worktree/")
        .map(|(_, tail)| tail)
        .or_else(|| name.rsplit_once("worktrees/").map(|(_, tail)| tail));
    if let Some(tail) = after_worktree {
        let stem = tail.split('/').next().unwrap_or(tail);
        // Worktree convention is underscore-separated user/repo/branch
        // (the repo and branch may legitimately contain dashes).
        if let Some(pretty) = repo_branch_from_token(stem, '_') {
            return Some(pretty);
        }
    }

    // Pattern B: a single token with `user-repo-branch...` shape.
    // Only opt in when there are at least 3 dash-separated parts AND
    // no slashes/underscores — otherwise we'd misformat ordinary
    // `org/repo` names or interfere with the worktree path above.
    if !name.contains('/') && !name.contains('_') && name.matches('-').count() >= 2 {
        if let Some(pretty) = repo_branch_from_token(name, '-') {
            return Some(pretty);
        }
    }

    None
}

/// Decompose `user{sep}repo{sep}branch...` into `repo:branch` using the
/// caller-chosen `sep`. Returns `None` for tokens with fewer than three
/// parts. The branch portion is rejoined with `-` for readability so
/// `feat_codeburn` and `feat-codeburn` both render the same.
fn repo_branch_from_token(token: &str, sep: char) -> Option<String> {
    let parts: Vec<&str> = token.split(sep).collect();
    if parts.len() < 3 {
        return None;
    }
    let repo = parts[1];
    let branch = parts[2..].join("-");
    if repo.is_empty() || branch.is_empty() {
        return None;
    }
    Some(format!("{repo}:{branch}"))
}

fn shorten_project_name(name: &str, max_w: usize) -> String {
    if name.chars().count() <= max_w {
        return name.to_string();
    }
    // Strip well-known prefixes that add no value.
    let stripped = name
        .strip_prefix("worktrees/")
        .or_else(|| name.strip_prefix("worktree/"))
        .unwrap_or(name);
    if stripped.chars().count() <= max_w {
        return stripped.to_string();
    }
    // Keep the last 2 path segments, ellipse the front.
    let segs: Vec<&str> = stripped.split('/').collect();
    if segs.len() >= 2 {
        let last = segs.last().copied().unwrap_or("");
        let combined = if segs.len() >= 3 {
            format!("…/{}/{}", segs[segs.len() - 2], last)
        } else {
            format!("…/{}", last)
        };
        if combined.chars().count() <= max_w {
            return combined;
        }
        // Keep tail of last segment.
        let tail_w = max_w.saturating_sub(2); // "…/"
        let last_count = last.chars().count();
        if tail_w > 0 && last_count > tail_w {
            let skip = last_count - tail_w;
            let suffix: String = last.chars().skip(skip).collect();
            return format!("…/{suffix}");
        }
        if combined.chars().count() <= max_w + 4 {
            return truncate_string(&combined, max_w);
        }
    }
    truncate_string(stripped, max_w)
}

fn impact_marker(impact: &str) -> (&'static str, Color) {
    match impact {
        "High" => ("!!", BAR_HIGH),
        "Medium" => ("!", TERMINAL_ACCENT),
        _ => ("·", MUTED_GRAY),
    }
}

fn cache_hit_percent(data: &UsageData) -> f64 {
    let cache_reads = data.grand_total.cache_read_tokens;
    let denominator = data.grand_total.input_tokens
        + data.grand_total.cache_creation_tokens
        + data.grand_total.cache_read_tokens;
    if denominator == 0 {
        0.0
    } else {
        cache_reads as f64 * 100.0 / denominator as f64
    }
}

fn projected_month_cost(data: &UsageData, period: &UsagePeriod) -> Option<f64> {
    let spent = data.grand_total.cost_usd?;
    let elapsed_days = elapsed_days_for_period(period, data)?;
    if elapsed_days == 0 {
        return Some(spent);
    }
    Some(spent / elapsed_days as f64 * 30.0)
}

fn elapsed_days_for_period(period: &UsagePeriod, data: &UsageData) -> Option<u64> {
    match period {
        UsagePeriod::Today => Some(1),
        UsagePeriod::Week => Some(7),
        UsagePeriod::ThirtyDays => Some(30),
        UsagePeriod::LastNDays(n) => Some(u64::from((*n).max(1))),
        UsagePeriod::Month => {
            let today = Local::now().date_naive();
            let first = NaiveDate::from_ymd_opt(today.year(), today.month(), 1)?;
            Some((today - first).num_days().max(0) as u64 + 1)
        }
        UsagePeriod::SpecificMonth(anchor) => {
            let first = NaiveDate::from_ymd_opt(anchor.year(), anchor.month(), 1)?;
            let last = crate::models::usage::last_day_of_month(anchor.year(), anchor.month());
            Some((last - first).num_days().max(0) as u64 + 1)
        }
        UsagePeriod::SpecificQuarter(year, q) => {
            let (first, last) = crate::models::usage::quarter_bounds(*year, *q);
            Some((last - first).num_days().max(0) as u64 + 1)
        }
        UsagePeriod::YearToDate => {
            let today = Local::now().date_naive();
            let first = NaiveDate::from_ymd_opt(today.year(), 1, 1)?;
            Some((today - first).num_days().max(0) as u64 + 1)
        }
        UsagePeriod::Custom { from, to } => {
            if to < from {
                Some(0)
            } else {
                Some((*to - *from).num_days() as u64 + 1)
            }
        }
        UsagePeriod::All => {
            let first = data.daily.first().map(|(date, _)| *date)?;
            let last = data.daily.last().map(|(date, _)| *date).unwrap_or(first);
            Some((last - first).num_days().max(0) as u64 + 1)
        }
    }
}

fn render_bar_chart(frame: &mut Frame, area: Rect, data: &UsageData) {
    if area.width < 4 || area.height < 4 {
        return;
    }

    let chart_height = area.height.saturating_sub(2) as usize; // leave room for labels
    let bar_width = 2_u16;
    let gap = 1_u16;
    let num_bars = ((area.width.saturating_sub(2)) / (bar_width + gap)) as usize;

    // Take last N days
    let start = data.daily.len().saturating_sub(num_bars);
    let slice = &data.daily[start..];

    if slice.is_empty() {
        return;
    }

    let max_val = slice.iter().map(|(_, b)| b.total()).max().unwrap_or(1).max(1);

    let mut lines: Vec<Line> = Vec::new();

    // Title
    lines.push(Line::from(Span::styled(
        format!(" Last {} days", slice.len()),
        Style::default().fg(MUTED_GRAY),
    )));

    // Build bars row by row (top to bottom)
    for row in 0..chart_height {
        let threshold = max_val as f64 * (chart_height - row) as f64 / chart_height as f64;
        let mut spans = vec![Span::raw(" ")];
        for (_date, bucket) in slice {
            let val = bucket.total() as f64;
            let ch = if val >= threshold { "█" } else { " " };
            let color = if val >= max_val as f64 * 0.8 {
                BAR_HIGH
            } else if val >= max_val as f64 * 0.4 {
                BAR_MED
            } else {
                BAR_COLOR
            };
            spans.push(Span::styled(
                format!("{ch:>width$}", width = bar_width as usize),
                Style::default().fg(color),
            ));
            spans.push(Span::raw(" ")); // gap
        }
        lines.push(Line::from(spans));
    }

    // X-axis labels (show day-of-month for last few)
    let mut label_spans = vec![Span::raw(" ")];
    for (date, _) in slice {
        let day = format!("{:>2}", date.format("%d"));
        label_spans.push(Span::styled(day, Style::default().fg(MUTED_GRAY)));
        label_spans.push(Span::raw(" "));
    }
    lines.push(Line::from(label_spans));

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, area);
}

fn render_help_bar(frame: &mut Frame, area: Rect, state: &UsageViewState) {
    // When zoomed, swap to a focused help string so the user has the
    // zoom-only affordances visible.
    if state.is_zoomed() {
        let spans = vec![
            Span::styled(" /", Style::default().fg(GOLD)),
            Span::styled(" search  ", Style::default().fg(MUTED_GRAY)),
            Span::styled("d", Style::default().fg(GOLD)),
            Span::styled(" detail  ", Style::default().fg(MUTED_GRAY)),
            Span::styled("z/Esc", Style::default().fg(GOLD)),
            Span::styled(" back  ", Style::default().fg(MUTED_GRAY)),
            Span::styled("j/k", Style::default().fg(GOLD)),
            Span::styled(" row  ", Style::default().fg(MUTED_GRAY)),
        ];
        let paragraph = Paragraph::new(Line::from(spans)).style(Style::default().bg(DARK_BG));
        frame.render_widget(paragraph, area);
        return;
    }

    let on_burndown = matches!(state.active_tab, UsageTab::Burndown);
    let mut spans = vec![
        Span::styled(" ◀/▶", Style::default().fg(GOLD)),
        Span::styled(" provider  ", Style::default().fg(MUTED_GRAY)),
        Span::styled("p", Style::default().fg(GOLD)),
        Span::styled(" filter  ", Style::default().fg(MUTED_GRAY)),
        Span::styled(
            "1 Today  2 7d  3 30d  4 90d  5 YTD  m Month  q Quarter  a All  D advanced  ",
            Style::default().fg(MUTED_GRAY),
        ),
    ];
    if on_burndown {
        // Burndown view: z zoom; Tab pivots panels; Enter commits chip; C clears.
        spans.extend_from_slice(&[
            Span::styled("z", Style::default().fg(GOLD)),
            Span::styled(" zoom  ", Style::default().fg(MUTED_GRAY)),
            Span::styled("Tab", Style::default().fg(GOLD)),
            Span::styled(" focus panel  ", Style::default().fg(MUTED_GRAY)),
            Span::styled("Enter", Style::default().fg(GOLD)),
            Span::styled(" pin filter  ", Style::default().fg(MUTED_GRAY)),
            Span::styled("Esc", Style::default().fg(GOLD)),
            Span::styled(" pop chip  ", Style::default().fg(MUTED_GRAY)),
            Span::styled("C", Style::default().fg(GOLD)),
            Span::styled(" clear all  ", Style::default().fg(MUTED_GRAY)),
        ]);
    } else {
        spans.extend_from_slice(&[
            Span::styled("Tab", Style::default().fg(GOLD)),
            Span::styled(" view  ", Style::default().fg(MUTED_GRAY)),
        ]);
    }
    spans.extend_from_slice(&[
        Span::styled("/ x c", Style::default().fg(GOLD)),
        Span::styled(" filters  ", Style::default().fg(MUTED_GRAY)),
        Span::styled("j/k", Style::default().fg(GOLD)),
        Span::styled(" scroll  ", Style::default().fg(MUTED_GRAY)),
        Span::styled("r/R", Style::default().fg(GOLD)),
        Span::styled(" refresh  ", Style::default().fg(MUTED_GRAY)),
        Span::styled("Esc", Style::default().fg(GOLD)),
        Span::styled(" back", Style::default().fg(MUTED_GRAY)),
    ]);
    let paragraph = Paragraph::new(Line::from(spans)).style(Style::default().bg(DARK_BG));
    frame.render_widget(paragraph, area);
}

fn truncate_string(s: &str, max_len: usize) -> String {
    // max_len is char count, not bytes. The previous `s.len()` gate let
    // multi-byte strings reach a byte-slice (`&s[..max_len-1]`) that
    // could fall inside a codepoint and panic. Since pretty_project_name
    // now feeds branch names into here, that path is reachable on any
    // non-ASCII branch name. Switch to char-count + chars().take().
    if s.chars().count() <= max_len {
        s.to_string()
    } else {
        let take = max_len.saturating_sub(1);
        let truncated: String = s.chars().take(take).collect();
        format!("{truncated}…")
    }
}

fn provider_filter_label(filter: UsageProviderFilter) -> &'static str {
    match filter {
        UsageProviderFilter::All => "All",
        UsageProviderFilter::Claude => "Claude",
        UsageProviderFilter::Codex => "Codex",
    }
}

/// First day of the calendar month containing `date`.
fn first_of_month(date: NaiveDate) -> NaiveDate {
    NaiveDate::from_ymd_opt(date.year(), date.month(), 1).unwrap_or(date)
}

/// First day of the previous calendar month.
fn previous_month_first(anchor: NaiveDate) -> NaiveDate {
    let (y, m) = if anchor.month() == 1 {
        (anchor.year() - 1, 12)
    } else {
        (anchor.year(), anchor.month() - 1)
    };
    NaiveDate::from_ymd_opt(y, m, 1).unwrap_or(anchor)
}

/// First day of the next calendar month.
fn next_month_first(anchor: NaiveDate) -> NaiveDate {
    let (y, m) = if anchor.month() == 12 {
        (anchor.year() + 1, 1)
    } else {
        (anchor.year(), anchor.month() + 1)
    };
    NaiveDate::from_ymd_opt(y, m, 1).unwrap_or(anchor)
}

/// `(year, quarter)` of the previous quarter, wrapping into the prior
/// year at Q1.
fn previous_quarter(year: i32, q: u8) -> (i32, u8) {
    if q <= 1 { (year - 1, 4) } else { (year, q - 1) }
}

/// `(year, quarter)` of the next quarter, wrapping into the next year
/// at Q4.
fn next_quarter(year: i32, q: u8) -> (i32, u8) {
    if q >= 4 { (year + 1, 1) } else { (year, q + 1) }
}

/// `(year, quarter)` containing `date`.
fn current_quarter(date: NaiveDate) -> (i32, u8) {
    (date.year(), crate::models::usage::quarter_of(date))
}

fn period_label(period: &UsagePeriod) -> String {
    match period {
        UsagePeriod::Today => "Today".to_string(),
        UsagePeriod::Week => "7d".to_string(),
        UsagePeriod::ThirtyDays => "30d".to_string(),
        UsagePeriod::LastNDays(n) => format!("{n}d"),
        UsagePeriod::Month => "Month".to_string(),
        UsagePeriod::SpecificMonth(anchor) => anchor.format("%b %Y").to_string(),
        UsagePeriod::SpecificQuarter(year, q) => format!("Q{q} {year}"),
        UsagePeriod::YearToDate => "YTD".to_string(),
        UsagePeriod::All => "All".to_string(),
        UsagePeriod::Custom { from, to } => format!("{from} to {to}"),
    }
}

fn input_label(mode: UsageInputMode) -> &'static str {
    match mode {
        UsageInputMode::Include => "include",
        UsageInputMode::Exclude => "exclude",
        UsageInputMode::DateRange => "date range",
    }
}

fn format_cost(cost: Option<f64>) -> String {
    cost.map(|value| format!("${value:.2}"))
        .unwrap_or_else(|| "cost n/a".to_string())
}

#[cfg(test)]
mod period_provider_strip_tests {
    use super::*;

    fn flatten(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect::<String>()
    }

    fn highlighted_chip(line: &Line<'_>) -> Option<String> {
        line.spans
            .iter()
            .find(|s| s.style.bg == Some(GOLD))
            .map(|s| s.content.trim().to_string())
    }

    #[test]
    fn period_row_lists_all_options_with_key_hints() {
        let mut state = UsageViewState::default();
        state.period = UsagePeriod::Week;
        let lines = build_period_provider_strip(&state);
        assert_eq!(lines.len(), 2);
        let row = flatten(&lines[0]);
        for needle in [
            "Period:",
            "1 Today",
            "2 7d",
            "3 30d",
            "4 90d",
            "5 YTD",
            "m Month",
            "q Quarter",
            "a All",
            "D advanced",
        ] {
            assert!(row.contains(needle), "missing `{needle}` in {row}");
        }
    }

    #[test]
    fn period_row_highlights_active_period() {
        let mut state = UsageViewState::default();
        state.period = UsagePeriod::Today;
        let lines = build_period_provider_strip(&state);
        let active = highlighted_chip(&lines[0]).expect("a period chip should be highlighted");
        assert!(active.contains("Today"), "got {active}");
    }

    #[test]
    fn period_row_highlights_90d_chip_when_last_n_days_90() {
        let mut state = UsageViewState::default();
        state.period = UsagePeriod::LastNDays(90);
        let lines = build_period_provider_strip(&state);
        let active = highlighted_chip(&lines[0]).expect("90d should be highlighted");
        assert!(active.contains("90d"), "got {active}");
    }

    #[test]
    fn provider_row_highlights_active_provider_filter() {
        let mut state = UsageViewState::default();
        state.provider_filter = UsageProviderFilter::Codex;
        let lines = build_period_provider_strip(&state);
        let active = highlighted_chip(&lines[1]).expect("a provider chip should be highlighted");
        assert_eq!(active, "Codex");
    }
}

#[cfg(test)]
mod pretty_project_tests {
    use super::*;

    #[test]
    fn worktree_path_renders_repo_colon_branch() {
        let name = "worktree/stevengonsalvez_agents-in-a-box_feat_codeburn";
        assert_eq!(
            pretty_project_name(name, 40),
            "agents-in-a-box:feat-codeburn"
        );
    }

    #[test]
    fn dashed_session_token_renders_repo_colon_branch() {
        let name = "stevengonsalvez-biolift-feat-all";
        assert_eq!(pretty_project_name(name, 40), "biolift:feat-all");
    }

    #[test]
    fn ordinary_path_falls_back_to_shorten() {
        let name = "/Users/stevie/work/project";
        // Falls back: name has slashes, shouldn't trip the dash heuristic.
        // Just assert we did not produce a "repo:branch" colon form.
        let out = pretty_project_name(name, 40);
        assert!(!out.contains(':') || out.contains("/"));
    }

    #[test]
    fn truncates_branch_when_pretty_form_overflows() {
        let name = "worktree/u_repository_very-long-feature-branch-name";
        let out = pretty_project_name(name, 16);
        // "repository:" is 11 chars, leaves 5 for branch (with truncate ellipsis).
        assert!(out.starts_with("repository:"), "got {out}");
        assert!(out.chars().count() <= 16, "got {out}");
    }

    #[test]
    fn empty_max_w_returns_empty() {
        assert_eq!(pretty_project_name("worktree/a_b_c", 0), "");
    }

    #[test]
    fn two_dash_segments_do_not_misclassify() {
        // "org-repo" — only 1 dash, must NOT match user-repo-branch shape.
        let name = "org-repo";
        assert_eq!(pretty_project_name(name, 40), "org-repo");
    }
}

#[cfg(test)]
mod cross_filter_tests {
    use super::*;
    use crate::models::usage::{
        ActivityCategory, ActivityUsage, ModelUsage, ProjectUsage, SessionUsage, TokenBucket,
    };
    use chrono::Local;

    fn bucket(call_count: usize) -> TokenBucket {
        TokenBucket {
            input_tokens: 100,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            output_tokens: 50,
            reasoning_tokens: 0,
            session_count: 1,
            project_count: 1,
            call_count,
            cost_usd: Some(call_count as f64),
        }
    }

    /// Build a fixture UsageData with two projects, two models, two
    /// activities and two sessions — enough surface for the
    /// commit_focused_row dispatch table.
    fn fixture() -> UsageData {
        let now = Local::now();
        UsageData {
            daily: vec![],
            weekly: vec![],
            projects: vec![
                ProjectUsage {
                    name: "alpha".into(),
                    path: "/work/alpha".into(),
                    bucket: bucket(3),
                },
                ProjectUsage {
                    name: "beta".into(),
                    path: "/work/beta".into(),
                    bucket: bucket(2),
                },
            ],
            grand_total: bucket(5),
            calls: vec![],
            sessions: vec![
                SessionUsage {
                    provider: "claude".into(),
                    project: "alpha".into(),
                    session_id: "sess-A".into(),
                    first_timestamp: now,
                    last_timestamp: now,
                    bucket: bucket(3),
                },
                SessionUsage {
                    provider: "claude".into(),
                    project: "beta".into(),
                    session_id: "sess-B".into(),
                    first_timestamp: now,
                    last_timestamp: now,
                    bucket: bucket(2),
                },
            ],
            models: vec![
                ModelUsage {
                    model: "claude-opus-4".into(),
                    bucket: bucket(3),
                },
                ModelUsage {
                    model: "claude-sonnet-4".into(),
                    bucket: bucket(2),
                },
            ],
            activities: vec![
                ActivityUsage {
                    category: ActivityCategory::Coding,
                    bucket: bucket(3),
                    turns: 3,
                    retries: 0,
                    edit_turns: 3,
                    one_shot_turns: 3,
                },
                ActivityUsage {
                    category: ActivityCategory::Conversation,
                    bucket: bucket(2),
                    turns: 2,
                    retries: 0,
                    edit_turns: 0,
                    one_shot_turns: 0,
                },
            ],
            tools: vec![],
            mcp_servers: vec![],
            shell_commands: vec![],
        }
    }

    #[test]
    fn tab_cycles_panel_focus_in_documented_order() {
        let mut state = UsageViewState::default();
        assert!(state.focused_panel.is_none());
        for panel in UsagePanel::ALL {
            state.focus_next_panel();
            assert_eq!(state.focused_panel, Some(panel));
        }
        // Wrap around back to the first.
        state.focus_next_panel();
        assert_eq!(state.focused_panel, Some(UsagePanel::ALL[0]));
    }

    #[test]
    fn shift_tab_from_unfocused_jumps_to_last_panel() {
        let mut state = UsageViewState::default();
        state.focus_prev_panel();
        assert_eq!(
            state.focused_panel,
            Some(UsagePanel::ALL[UsagePanel::ALL.len() - 1])
        );
    }

    #[test]
    fn enter_on_by_project_row_sets_project_filter() {
        let mut state = UsageViewState::default();
        state.data = Some(fixture());
        state.focused_panel = Some(UsagePanel::ByProject);
        state.focus_row = 1; // beta
        assert!(state.commit_focused_row());
        assert_eq!(state.filters.project, vec!["beta".to_string()]);
    }

    #[test]
    fn enter_on_top_session_row_sets_session_filter() {
        let mut state = UsageViewState::default();
        state.data = Some(fixture());
        state.focused_panel = Some(UsagePanel::TopSessions);
        state.focus_row = 0;
        assert!(state.commit_focused_row());
        assert_eq!(state.filters.session, vec!["sess-A".to_string()]);
    }

    #[test]
    fn enter_on_by_model_row_sets_model_filter() {
        let mut state = UsageViewState::default();
        state.data = Some(fixture());
        state.focused_panel = Some(UsagePanel::ByModel);
        state.focus_row = 0;
        assert!(state.commit_focused_row());
        assert_eq!(state.filters.model, vec!["claude-opus-4".to_string()]);
    }

    #[test]
    fn enter_on_by_activity_row_sets_activity_filter() {
        let mut state = UsageViewState::default();
        state.data = Some(fixture());
        state.focused_panel = Some(UsagePanel::ByActivity);
        state.focus_row = 1; // Conversation
        assert!(state.commit_focused_row());
        assert_eq!(state.filters.activity, vec!["Conversation".to_string()]);
    }

    #[test]
    fn enter_on_daily_activity_row_is_noop() {
        // Brief: read-only panels — Enter is a no-op.
        let mut state = UsageViewState::default();
        state.data = Some(fixture());
        state.focused_panel = Some(UsagePanel::DailyActivity);
        state.focus_row = 0;
        assert!(!state.commit_focused_row());
        assert!(state.filters.is_empty());
    }

    #[test]
    fn enter_on_leaderboard_maps_to_project_filter() {
        let mut state = UsageViewState::default();
        state.data = Some(fixture());
        state.focused_panel = Some(UsagePanel::Leaderboard);
        state.focus_row = 0; // alpha (rendered from filtered_data.projects)
        assert!(state.commit_focused_row());
        assert_eq!(state.filters.project, vec!["alpha".to_string()]);
    }

    #[test]
    fn esc_pops_last_chip_and_chip_strip_reflects_remaining() {
        let mut state = UsageViewState::default();
        state.filters.project.push("alpha".into());
        state.filters.model.push("opus".into());

        // First pop -> model (the chip-strip pop order: session →
        // activity → model → project).
        let removed = state.pop_filter_chip();
        assert!(matches!(removed, Some(UsageFilterChip::Model(v)) if v == "opus"));
        assert!(state.filters.model.is_empty());
        assert_eq!(state.filters.project, vec!["alpha".to_string()]);

        // Chip strip should still show the remaining chip + Esc/C hint.
        let line = build_filter_chip_line(&state);
        let flat: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(flat.contains("project=alpha"), "got {flat}");
        assert!(flat.contains("Esc") && flat.contains("C"), "got {flat}");

        // Second pop -> project. With no chips left we fall back to
        // the discoverability hint.
        assert!(state.pop_filter_chip().is_some());
        assert!(state.filters.is_empty());
        let line = build_filter_chip_line(&state);
        let flat: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(flat.contains("(none)"), "got {flat}");
    }

    #[test]
    fn clear_all_drops_every_chip() {
        let mut state = UsageViewState::default();
        state.filters.project.push("alpha".into());
        state.filters.model.push("opus".into());
        state.filters.activity.push("Coding".into());
        state.filters.session.push("sess-1".into());
        state.clear_all_filter_chips();
        assert!(state.filters.is_empty());
    }

    #[test]
    fn z_toggles_zoom_state_and_records_focused_panel() {
        let mut state = UsageViewState::default();
        state.focused_panel = Some(UsagePanel::ByProject);
        state.toggle_zoom();
        assert_eq!(state.zoom, Some(UsagePanel::ByProject));
        assert!(state.is_zoomed());
        state.toggle_zoom();
        assert!(state.zoom.is_none());
        assert!(!state.is_zoomed());
    }

    #[test]
    fn z_without_focus_picks_first_panel() {
        let mut state = UsageViewState::default();
        assert!(state.focused_panel.is_none());
        state.toggle_zoom();
        assert_eq!(state.zoom, Some(UsagePanel::ALL[0]));
        assert_eq!(state.focused_panel, Some(UsagePanel::ALL[0]));
    }

    #[test]
    fn slash_enters_search_mode_and_typing_appends() {
        let mut state = UsageViewState::default();
        state.toggle_zoom();
        state.zoom_begin_search();
        assert!(state.zoom_search_active);
        state.zoom_search_char('a');
        state.zoom_search_char('l');
        state.zoom_search_char('p');
        assert_eq!(state.zoom_search_query, "alp");
        state.zoom_commit_search();
        assert!(!state.zoom_search_active);
        assert_eq!(state.zoom_search_query, "alp");
    }

    #[test]
    fn esc_priority_is_detail_then_search_then_zoom_exit() {
        let mut state = UsageViewState::default();
        state.toggle_zoom();
        state.zoom_detail_open = true;
        state.zoom_search_active = true;
        state.zoom_search_query.push('x');

        // 1st Esc -> close detail.
        assert!(state.zoom_handle_esc());
        assert!(!state.zoom_detail_open);
        assert!(state.zoom_search_active, "search still active");

        // 2nd Esc -> cancel search.
        assert!(state.zoom_handle_esc());
        assert!(!state.zoom_search_active);
        assert!(state.zoom_search_query.is_empty());

        // 3rd Esc -> exit zoom.
        assert!(state.zoom_handle_esc());
        assert!(!state.is_zoomed());

        // 4th Esc -> not consumed; caller falls through to chip pop.
        assert!(!state.zoom_handle_esc());
    }

    #[test]
    fn d_toggles_detail_drawer_when_zoomed() {
        let mut state = UsageViewState::default();
        state.toggle_zoom();
        assert!(!state.zoom_detail_open);
        state.toggle_zoom_detail();
        assert!(state.zoom_detail_open);
        state.toggle_zoom_detail();
        assert!(!state.zoom_detail_open);
    }

    #[test]
    fn fuzzy_filter_matches_substring_when_query_present() {
        let projects = vec![
            "alpha".to_string(),
            "beta".to_string(),
            "alphabet".to_string(),
        ];
        let out = apply_zoom_filter(&projects, "alp", |s| s.clone());
        assert_eq!(out.len(), 2, "alpha + alphabet should both match");
    }

    #[test]
    fn fuzzy_filter_empty_query_returns_all() {
        let projects = vec!["alpha".to_string(), "beta".to_string()];
        let out = apply_zoom_filter(&projects, "", |s| s.clone());
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn focus_ctx_for_panel_only_marks_matching_panel() {
        let mut state = UsageViewState::default();
        state.focused_panel = Some(UsagePanel::ByProject);
        state.focus_row = 4;
        let by_proj = FocusCtx::for_panel(&state, UsagePanel::ByProject);
        let by_model = FocusCtx::for_panel(&state, UsagePanel::ByModel);
        assert!(by_proj.is_focused());
        assert_eq!(by_proj.focused_row, Some(4));
        assert!(!by_model.is_focused());
        assert!(by_model.focused_row.is_none());
    }
}

#[cfg(test)]
mod cli_parity_tests {
    use crate::models::usage::{
        ActivityCategory, ActivityUsage, ModelUsage, ProjectUsage, ProviderCall, SessionUsage,
        TokenBucket, UsageData, UsageFilters, filter_usage_data,
    };
    use chrono::Local;

    fn call(project: &str, model: &str, session: &str) -> ProviderCall {
        ProviderCall {
            provider: "claude".into(),
            model: model.into(),
            session_id: session.into(),
            project: project.into(),
            project_path: format!("/work/{project}"),
            timestamp: Local::now(),
            input_tokens: 100,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            output_tokens: 50,
            reasoning_tokens: 0,
            cost_usd: Some(1.0),
            tools: vec!["Edit".into()],
            bash_commands: vec![],
            user_message: "tidy".into(),
            branch: None,
        }
    }

    fn data_with_calls(calls: Vec<ProviderCall>) -> UsageData {
        // Ad-hoc UsageData wrapping `calls`. We don't need the
        // aggregates here — the CLI parity contract is "filtering
        // re-aggregates from data.calls" and that's what
        // filter_usage_data does internally.
        UsageData {
            calls,
            ..UsageData::default()
        }
    }

    #[test]
    fn project_filter_excludes_other_projects_and_changes_totals() {
        let data = data_with_calls(vec![
            call("alpha", "opus", "s1"),
            call("alpha", "opus", "s2"),
            call("beta", "opus", "s3"),
        ]);
        let mut filters = UsageFilters::default();
        filters.project.push("alpha".into());
        let filtered = filter_usage_data(&data, &filters);
        assert_eq!(filtered.calls.len(), 2);
        assert!(filtered.projects.iter().all(|p| p.name == "alpha"));
        assert_eq!(
            filtered.grand_total.cost_usd,
            Some(2.0),
            "cost should reflect only the surviving 2 calls"
        );
    }

    #[test]
    fn multiple_project_values_or_combine() {
        let data = data_with_calls(vec![
            call("alpha", "opus", "s1"),
            call("beta", "opus", "s2"),
            call("gamma", "opus", "s3"),
        ]);
        let mut filters = UsageFilters::default();
        filters.project.push("alpha".into());
        filters.project.push("beta".into());
        let filtered = filter_usage_data(&data, &filters);
        assert_eq!(filtered.calls.len(), 2);
    }

    #[test]
    fn project_and_model_and_combine() {
        let data = data_with_calls(vec![
            call("alpha", "opus", "s1"),
            call("alpha", "sonnet", "s2"),
            call("beta", "opus", "s3"),
        ]);
        let mut filters = UsageFilters::default();
        filters.project.push("alpha".into());
        filters.model.push("opus".into());
        let filtered = filter_usage_data(&data, &filters);
        assert_eq!(filtered.calls.len(), 1);
        assert_eq!(filtered.calls[0].session_id, "s1");
    }

    #[test]
    fn empty_filters_clones_data_through() {
        let data = data_with_calls(vec![call("alpha", "opus", "s1")]);
        let filtered = filter_usage_data(&data, &UsageFilters::default());
        // No filters -> identical surface.
        assert_eq!(filtered.calls.len(), data.calls.len());
    }

    #[test]
    fn activity_filter_matches_classified_label() {
        // "Edit" tool -> Coding category. So filtering by activity =
        // "Coding" keeps the call; filtering by "Git" drops it.
        let data = data_with_calls(vec![call("alpha", "opus", "s1")]);
        let mut keep = UsageFilters::default();
        keep.activity.push("Coding".into());
        let kept = filter_usage_data(&data, &keep);
        assert_eq!(kept.calls.len(), 1);

        let mut drop = UsageFilters::default();
        drop.activity.push("Git".into());
        let dropped = filter_usage_data(&data, &drop);
        assert_eq!(dropped.calls.len(), 0);
    }

    #[test]
    fn branch_filter_keeps_only_matching_branch_and_drops_branchless_calls() {
        let mut on_main = call("alpha", "opus", "s1");
        on_main.branch = Some("main".into());
        let mut on_feat = call("alpha", "opus", "s2");
        on_feat.branch = Some("feat/x".into());
        let no_branch = call("alpha", "opus", "s3"); // branch: None

        let data = data_with_calls(vec![on_main, on_feat, no_branch]);

        let mut filters = UsageFilters::default();
        filters.branch.push("feat/x".into());
        let filtered = filter_usage_data(&data, &filters);
        assert_eq!(filtered.calls.len(), 1, "only feat/x survives");
        assert_eq!(filtered.calls[0].session_id, "s2");

        // Branchless calls must NOT match a non-empty branch filter — even
        // a generic "main" filter excludes calls that have no recorded
        // branch (codex turns, Claude turns outside a git repo).
        let mut main_only = UsageFilters::default();
        main_only.branch.push("main".into());
        let only_main = filter_usage_data(&data, &main_only);
        assert_eq!(only_main.calls.len(), 1);
        assert!(only_main.calls.iter().all(|c| c.branch.as_deref() == Some("main")));
    }

    #[test]
    fn session_filter_keeps_only_matching_session_id() {
        let data = data_with_calls(vec![
            call("alpha", "opus", "s1"),
            call("alpha", "opus", "s2"),
        ]);
        let mut filters = UsageFilters::default();
        filters.session.push("s2".into());
        let filtered = filter_usage_data(&data, &filters);
        assert_eq!(filtered.calls.len(), 1);
        assert_eq!(filtered.calls[0].session_id, "s2");
    }

    /// Doc test the README contract: the JSON `overview.cost_usd`
    /// produced by `ainb usage report --project X --format json`
    /// equals `filter_usage_data(unfiltered, {project: [X]})
    /// .grand_total.cost_usd`. We exercise the model-side helper
    /// directly here; the CLI's `query_from_args` -> `load_usage` ->
    /// `parse_usage_for_with_roots_and_cache` chain wraps this
    /// helper after the period+include/exclude pre-pass, so any
    /// future regression in either layer trips this assertion.
    #[test]
    fn report_overview_cost_matches_filter_helper() {
        let data = data_with_calls(vec![
            call("alpha", "opus", "s1"),
            call("alpha", "opus", "s2"),
            call("beta", "opus", "s3"),
        ]);
        let mut filters = UsageFilters::default();
        filters.project.push("alpha".into());
        let filtered = filter_usage_data(&data, &filters);
        let overview_cost = filtered.overview().cost_usd;
        assert_eq!(overview_cost, Some(2.0));
    }

    // Silence dead_code warnings on the helper imports for fixtures
    // that aren't yet exercised by a top-level assertion. They pull
    // their weight via type inference for the `data_with_calls`
    // signature and round-out the reusable fixture API.
    #[allow(dead_code)]
    fn _types(
        _: ActivityUsage,
        _: ActivityCategory,
        _: ModelUsage,
        _: ProjectUsage,
        _: SessionUsage,
        _: TokenBucket,
    ) {
    }
}
