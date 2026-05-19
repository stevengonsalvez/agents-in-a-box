// ABOUTME: Usage analytics screen showing token consumption by day, week, and project.
// Accessible via 'i' key from home screen or Stats sidebar item.

use chrono::{Datelike, Local, NaiveDate};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Gauge, Paragraph, Row, Table, Tabs},
};

use ainb_plugin_types_sessions::ScanProgressEvent;

use crate::data::usage::{
    BranchUsage, current_quarter, first_of_month, next_month_first, next_quarter,
    previous_month_first, previous_quarter,
};
use crate::data::{
    ActivityUsage, ModelUsage, NamedUsage, ProjectUsage, SessionUsage, UsageData, UsageFilterChip,
    UsageFilters, UsagePeriod, UsageProviderFilter, UsageQuery, filter_usage_data_full,
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
    Burndown,
    Daily,
    Weekly,
    Projects,
    Optimize,
}

impl UsageTab {
    fn all() -> &'static [UsageTab] {
        &[
            UsageTab::Burndown,
            UsageTab::Daily,
            UsageTab::Weekly,
            UsageTab::Projects,
            UsageTab::Optimize,
        ]
    }

    fn title(&self) -> &'static str {
        match self {
            UsageTab::Burndown => "Burndown",
            UsageTab::Daily => "Daily",
            UsageTab::Weekly => "Weekly",
            UsageTab::Projects => "By Project",
            UsageTab::Optimize => "Optimize",
        }
    }

    fn next(&self) -> Self {
        match self {
            UsageTab::Burndown => UsageTab::Daily,
            UsageTab::Daily => UsageTab::Weekly,
            UsageTab::Weekly => UsageTab::Projects,
            UsageTab::Projects => UsageTab::Optimize,
            UsageTab::Optimize => UsageTab::Burndown,
        }
    }

    fn prev(&self) -> Self {
        match self {
            UsageTab::Burndown => UsageTab::Optimize,
            UsageTab::Daily => UsageTab::Burndown,
            UsageTab::Weekly => UsageTab::Daily,
            UsageTab::Projects => UsageTab::Weekly,
            UsageTab::Optimize => UsageTab::Projects,
        }
    }
}

/// Live free-text input mode on the usage screen. Only `DateRange`
/// remains — include/exclude project prompts were replaced by the
/// picker-style chip strip (Enter / Shift+X) per the "no free text in
/// TUI" principle. The variant is retained as a single-arm enum for
/// forward compatibility (eg. CLI-driven advanced filters that may
/// reintroduce a typed input later).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageInputMode {
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
    ByBranch,
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
    pub const ALL: [UsagePanel; 13] = [
        UsagePanel::DailyActivity,
        UsagePanel::ByProject,
        UsagePanel::ByBranch,
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
            UsagePanel::ByBranch => "By Branch",
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
    /// filter (rows are projects). Core Tools, Shell Commands, and
    /// MCP Servers are read-only too: a single call uses many tools,
    /// so "filter calls where tool = X" doesn't map cleanly onto the
    /// per-call filter contract.
    pub fn enter_target(self) -> Option<UsageFilterTarget> {
        match self {
            UsagePanel::ByProject | UsagePanel::Leaderboard => Some(UsageFilterTarget::Project),
            UsagePanel::ByActivity => Some(UsageFilterTarget::Activity),
            UsagePanel::ByModel => Some(UsageFilterTarget::Model),
            UsagePanel::ByBranch => Some(UsageFilterTarget::Branch),
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
    Branch,
}

/// View state for the usage analytics screen.
///
// TODO(refactor): collapse the 4 zoom-related fields (zoom,
// zoom_search_active, zoom_search_query, zoom_detail_open) into a
// single `Option<ZoomState>` so the "in zoom" invariant lives in the
// type system. Currently every zoom-related callsite has to remember
// to check `self.zoom.is_some()` and the search/detail flags carry
// stale values from prior zoom sessions. Deferred — touches every
// zoom-related event handler and renderer; safer to land alongside
// snapshot tests for the zoom view.
#[derive(Debug, Clone)]
pub struct UsageViewState {
    pub provider: UsageProvider,
    pub active_tab: UsageTab,
    /// Most-recent parsed usage snapshot. Held as `Arc` so the plugin
    /// can share the same instance across `ui.data` and the cached
    /// filter results without paying a 30K-call clone on every Enter
    /// keypress (drill-down) or render-snapshot copy. Reads deref to
    /// `&UsageData` transparently; writes wrap in `Arc::new` or clone
    /// an existing `Arc` (refcount bump, not a deep copy).
    pub data: Option<std::sync::Arc<UsageData>>,
    /// True for the single render frame immediately after
    /// `cached_filtered` did a real recompute (cache miss). Set by the
    /// plugin's render snapshot — not persisted between frames. The
    /// chip strip renders a brief `↻ updated` badge while this is on,
    /// giving the user a visual confirmation that their pivot landed.
    pub fresh_pivot: bool,
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
    /// Absolute oldest call day observed across the unfiltered call
    /// set, monotonically narrowing across loads. Used to clamp
    /// `step_period_back` independent of the currently-active period
    /// (which restricts `data.daily` to the visible window and would
    /// otherwise prevent stepping into earlier months/quarters).
    ///
    /// Updated on every load via `min(existing, new_min_from_data_calls)`
    /// so a narrow period (eg. `SpecificMonth(May)`) cannot raise the
    /// anchor above an earlier value seen during a wider load.
    pub oldest_call_day: Option<NaiveDate>,
    /// Latest `sessions.scan_progress` tick received from session-reader.
    /// When `data` is `None` and this is `Some`, the render path shows
    /// a skeleton line (`Scanning sessions: N/M · {current_project}` or
    /// `Scanning sessions… N files` when `total = 0`) instead of the
    /// legacy `⏳ Waiting for session-reader plugin…` spinner.
    pub scan_progress: Option<ScanProgressEvent>,
    /// Pre-computed `filter_usage_data(data, filters)` result, supplied
    /// by [`crate::plugin::BurndownPlugin`] from its per-plugin filter
    /// cache. When `Some`, the burndown render path uses this Arc
    /// directly instead of re-running `analyze_turns` + the aggregate
    /// pivot on every paint. `None` means "no cache hit available —
    /// recompute inline" (the default for unit tests and the CLI
    /// snapshot path, both of which set this to `None`).
    pub cached_filtered: Option<std::sync::Arc<UsageData>>,
}

impl Default for UsageViewState {
    fn default() -> Self {
        Self {
            provider: UsageProvider::Claude,
            active_tab: UsageTab::Burndown,
            data: None,
            fresh_pivot: false,
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
            oldest_call_day: None,
            scan_progress: None,
            cached_filtered: None,
        }
    }
}

/// Direction parameter for `step_period`. Internal — wrappers
/// `step_period_back` / `step_period_forward` are the public API.
#[derive(Debug, Clone, Copy)]
enum StepDirection {
    Back,
    Forward,
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
            UsageProviderFilter::Codex => UsageProviderFilter::Cursor,
            UsageProviderFilter::Cursor => UsageProviderFilter::Copilot,
            UsageProviderFilter::Copilot => UsageProviderFilter::Gemini,
            UsageProviderFilter::Gemini => UsageProviderFilter::All,
        };
        self.scroll_offset = 0;
    }

    pub fn set_period(&mut self, period: UsagePeriod) {
        self.period = period;
        self.scroll_offset = 0;
    }

    /// Step the active Month or Quarter picker one unit back. Clamps
    /// at `oldest_call_day` — the absolute oldest day observed across
    /// the unfiltered call set — so the user can step from a narrow
    /// period (eg. `SpecificMonth(May)`) into an earlier month, even
    /// though `data.daily` only holds the May rows in that state.
    /// Returns `true` when the period actually changed.
    ///
    /// No-op when the active period is not a Month/Quarter picker, or
    /// when no data has been loaded yet (no oldest anchor to clamp
    /// against — would let the user wander arbitrarily far back).
    pub fn step_period_back(&mut self) -> bool {
        let Some(oldest) = self.oldest_call_day else {
            return false;
        };
        self.step_period(StepDirection::Back, oldest)
    }

    /// Step forward one unit. Clamps at the current real-world month
    /// or quarter — never lets the user pick a future window.
    pub fn step_period_forward(&mut self) -> bool {
        let today = Local::now().date_naive();
        self.step_period(StepDirection::Forward, today)
    }

    /// Shared body for back/forward stepping. `clamp_anchor` is the
    /// extreme of the allowed range — the oldest data day for `Back`,
    /// today for `Forward`.
    fn step_period(&mut self, direction: StepDirection, clamp_anchor: NaiveDate) -> bool {
        match self.period.clone() {
            UsagePeriod::SpecificMonth(anchor) => {
                let new_anchor = match direction {
                    StepDirection::Back => previous_month_first(anchor),
                    StepDirection::Forward => next_month_first(anchor),
                };
                let clamp = first_of_month(clamp_anchor);
                let out_of_range = match direction {
                    StepDirection::Back => new_anchor < clamp,
                    StepDirection::Forward => new_anchor > clamp,
                };
                if out_of_range {
                    return false;
                }
                self.period = UsagePeriod::SpecificMonth(new_anchor);
                self.scroll_offset = 0;
                true
            }
            UsagePeriod::SpecificQuarter(year, q) => {
                let (new_year, new_q) = match direction {
                    StepDirection::Back => previous_quarter(year, q),
                    StepDirection::Forward => next_quarter(year, q),
                };
                let (cy, cq) = current_quarter(clamp_anchor);
                let out_of_range = match direction {
                    StepDirection::Back => (new_year, new_q) < (cy, cq),
                    StepDirection::Forward => (new_year, new_q) > (cy, cq),
                };
                if out_of_range {
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

    /// Cycle the focus pointer to the next panel. If unfocused, focus
    /// the first panel. Resets `focus_row` to 0.
    pub fn focus_next_panel(&mut self) {
        self.focused_panel = Some(match self.focused_panel {
            Some(panel) => panel.next(),
            None => UsagePanel::ALL[0],
        });
        self.focus_row = 0;
    }

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

    /// Filtered view of the parsed data, applying the full filter
    /// surface — cross-filter chips, period date range, and provider
    /// selector — so callers like `resolve_focused_row` see the same
    /// pivot the dashboard panels render. Cheap when no filters/period/
    /// provider is active (clones the source). Reuses
    /// [`Self::cached_filtered`] when the plugin supplied a
    /// pre-computed Arc, otherwise recomputes inline — callers that
    /// need the cache benefit must populate `cached_filtered` before
    /// invoking this method.
    pub fn filtered_data(&self) -> Option<UsageData> {
        if let Some(arc) = self.cached_filtered.as_ref() {
            return Some((**arc).clone());
        }
        self.data.as_ref().map(|data| {
            filter_usage_data_full(data, &self.filters, &self.period, self.provider_filter)
        })
    }

    /// Resolve the focused panel row to a `(target, value, owner_project)`
    /// triple. Shared by include and exclude commit paths so both
    /// dispatch tables stay in lock-step.
    fn resolve_focused_row(&self) -> Option<(UsageFilterTarget, String, Option<String>)> {
        let panel = self.focused_panel?;
        let target = panel.enter_target()?;
        let filtered = self.filtered_data()?;
        let row_idx = self.focus_row;
        // For Session rows we also need the owning project so we can
        // auto-attach a project chip — session ids can collide across
        // projects/providers because the aggregator key is
        // `provider:project:session_id` but `filters.session` only holds
        // the bare id. Other targets pass None as the second element.
        match (target, panel) {
            (UsageFilterTarget::Project, UsagePanel::Leaderboard | UsagePanel::ByProject) => {
                filtered.projects.get(row_idx).map(|p| (target, p.name.clone(), None))
            }
            (UsageFilterTarget::Activity, _) => filtered
                .activities
                .get(row_idx)
                .map(|a| (target, a.category.label().to_string(), None)),
            (UsageFilterTarget::Model, _) => {
                filtered.models.get(row_idx).map(|m| (target, m.model.clone(), None))
            }
            (UsageFilterTarget::Session, _) => filtered
                .sessions
                .get(row_idx)
                .map(|s| (target, s.session_id.clone(), Some(s.project.clone()))),
            (UsageFilterTarget::Branch, _) => {
                filtered.branches.get(row_idx).map(|b| (target, b.branch.clone(), None))
            }
            _ => None,
        }
    }

    /// Append the focused row of the focused panel as a chip. Returns
    /// `true` if a chip was added (so callers can show feedback).
    /// Requires `data` to be loaded; uses the unfiltered `data` to
    /// resolve the row by index because that's what the user is
    /// looking at when focus is active (we render from filtered_data
    /// at draw time, which is the same source).
    pub fn commit_focused_row(&mut self) -> bool {
        let Some((target, value, owner_project)) = self.resolve_focused_row() else {
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
            UsageFilterTarget::Branch => {
                if !self.filters.branch.contains(&value) {
                    self.filters.branch.push(value);
                }
            }
        }
        true
    }

    /// Append the focused row as an *exclude* chip. Mirror of
    /// `commit_focused_row` that routes into the `exclude_*` filter
    /// lists. Session rows do NOT auto-attach an owner-project exclude
    /// — excluding the project because one of its sessions was excluded
    /// would discard sibling sessions the user did not target.
    pub fn commit_focused_row_exclude(&mut self) -> bool {
        let Some((target, value, _owner_project)) = self.resolve_focused_row() else {
            return false;
        };
        match target {
            UsageFilterTarget::Project => {
                if !self.filters.exclude_project.contains(&value) {
                    self.filters.exclude_project.push(value);
                }
            }
            UsageFilterTarget::Model => {
                if !self.filters.exclude_model.contains(&value) {
                    self.filters.exclude_model.push(value);
                }
            }
            UsageFilterTarget::Activity => {
                if !self.filters.exclude_activity.contains(&value) {
                    self.filters.exclude_activity.push(value);
                }
            }
            UsageFilterTarget::Session => {
                if !self.filters.exclude_session.contains(&value) {
                    self.filters.exclude_session.push(value);
                }
            }
            UsageFilterTarget::Branch => {
                if !self.filters.exclude_branch.contains(&value) {
                    self.filters.exclude_branch.push(value);
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

    /// Begin fuzzy-search input inside the zoomed panel. Preserves the
    /// prior typed query so re-pressing `/` resumes editing where the
    /// last search left off (vim / fzf convention). Esc (`zoom_cancel_search`)
    /// is the path that drops the query entirely.
    pub fn zoom_begin_search(&mut self) {
        if self.zoom.is_some() {
            self.zoom_search_active = true;
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
pub fn render(buf: &mut Buffer, area: Rect, state: &UsageViewState) {
    // Main layout: header + provider selector + tabs + (optional scan banner) +
    // content + help bar. The scan banner is a one-row strip that only appears
    // while session-reader is still walking the per-provider dirs — without it,
    // a mid-scan render shows a populated summary bar but empty panels (data
    // streams in chunks; aggregates fill up before the panels do), which reads
    // as a hang. The banner stays visible until the plugin clears
    // `scan_progress` on the final `is_final` chunk.
    let show_scan_banner = state.scan_progress.is_some()
        && state.data.is_some()
        && !state.loading;
    // Stack-allocated constraints — render is the hot path; avoid the Vec.
    let layout = if show_scan_banner {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // Summary bar
                Constraint::Length(3), // Provider selector
                Constraint::Length(3), // Tab bar
                Constraint::Length(1), // Scan banner (mid-scan only)
                Constraint::Min(0),    // Table content
                Constraint::Length(2), // Help bar
            ])
            .split(area)
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // Summary bar
                Constraint::Length(3), // Provider selector
                Constraint::Length(3), // Tab bar
                Constraint::Min(0),    // Table content
                Constraint::Length(2), // Help bar
            ])
            .split(area)
    };

    render_summary_bar(buf, layout[0], state);
    render_provider_bar(buf, layout[1], state);
    render_tab_bar(buf, layout[2], state);

    let (content_idx, help_idx) = if show_scan_banner {
        render_scan_banner_inline(
            buf,
            layout[3],
            state.scan_progress.as_ref().expect("guarded by show_scan_banner"),
        );
        (4, 5)
    } else {
        (3, 4)
    };

    if state.loading || state.data.is_none() {
        if let Some(progress) = state.scan_progress.as_ref() {
            render_scan_progress(buf, layout[content_idx], progress);
        } else {
            render_loading(buf, layout[content_idx]);
        }
    } else {
        let data = state.data.as_ref().unwrap();
        if data.calls.is_empty() && !state.provider.has_data() {
            render_no_data(buf, layout[content_idx], state);
        } else {
            match state.active_tab {
                UsageTab::Daily => render_daily(buf, layout[content_idx], data, state.scroll_offset),
                UsageTab::Weekly => render_weekly(buf, layout[content_idx], data, state.scroll_offset),
                UsageTab::Projects => render_projects(buf, layout[content_idx], data, state.scroll_offset),
                UsageTab::Burndown => render_burndown(buf, layout[content_idx], data, state),
                UsageTab::Optimize => render_optimize(buf, layout[content_idx], data),
            }
        }
    }

    render_help_bar(buf, layout[help_idx], state);
}

/// Render a slim one-row scan-progress banner above the dashboard. Different
/// from `render_scan_progress` (which paints the full skeleton panel when
/// data is empty) — this is the data-present mid-scan affordance:
///
///   ⏳ Scanning sessions: 12/47 · my-project
///
/// No border, no surrounding panel — just inline text so the row above the
/// dashboard doesn't visually compete with the panel chrome.
fn render_scan_banner_inline(buf: &mut Buffer, area: Rect, progress: &ScanProgressEvent) {
    let mut spans = vec![
        Span::styled(" ⏳ ", Style::default().fg(GOLD)),
        Span::styled(
            scan_progress_headline(progress),
            Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD),
        ),
    ];
    if !progress.current_project.is_empty() {
        spans.push(Span::styled(" · ", Style::default().fg(MUTED_GRAY)));
        spans.push(Span::styled(
            progress.current_project.clone(),
            Style::default().fg(MUTED_GRAY),
        ));
    }
    let paragraph = Paragraph::new(Line::from(spans));
    ratatui::widgets::Widget::render(paragraph, area, buf);
}

fn render_summary_bar(buf: &mut Buffer, area: Rect, state: &UsageViewState) {
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
    ratatui::widgets::Widget::render(paragraph, area, buf);
}

fn render_provider_bar(buf: &mut Buffer, area: Rect, state: &UsageViewState) {
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
    ratatui::widgets::Widget::render(paragraph, area, buf);
}

fn render_no_data(buf: &mut Buffer, area: Rect, state: &UsageViewState) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CORNFLOWER_BLUE))
        .style(Style::default().bg(DARK_BG));

    let inner = block.inner(area);
    ratatui::widgets::Widget::render(block, area, buf);

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
    ratatui::widgets::Widget::render(paragraph, inner, buf);
}

fn render_tab_bar(buf: &mut Buffer, area: Rect, state: &UsageViewState) {
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

    ratatui::widgets::Widget::render(tabs, area, buf);
}

fn render_loading(buf: &mut Buffer, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CORNFLOWER_BLUE))
        .style(Style::default().bg(DARK_BG));

    // Phase 6c: burndown no longer scans session files itself — the
    // session-reader plugin owns the data plane and publishes
    // `sessions.usage_data`. This empty state is what the user sees
    // until the first event arrives (cold-cache scan ~5s) or
    // permanently if session-reader is missing from `dist/plugins/`,
    // which makes the data-flow gap detectable instead of silent.
    let paragraph = Paragraph::new(Line::from(vec![Span::styled(
        "  ⏳ Waiting for session-reader plugin...",
        Style::default().fg(MUTED_GRAY),
    )]))
    .block(block);
    ratatui::widgets::Widget::render(paragraph, area, buf);
}

/// Render the live cold-scan skeleton driven by `sessions.scan_progress`
/// events from session-reader. Two formats depending on whether the
/// scanner has pre-computed a file total:
///
/// * `total > 0` → headline `Scanning sessions: N/M · {current_project}`
///   on row 0 of the inner area, plus a ratatui `Gauge` bar on row 1
///   showing the `N/M` ratio with the inline `XX% (N/M)` label.
/// * `total = 0` → headline `Scanning sessions… N files · {current_project}`
///   only (no bar — without a total there's no ratio to render).
///
/// The text is rendered in the same rounded panel as `render_loading`
/// so the layout doesn't jitter between the two skeleton variants. The
/// gauge area is only allocated when total > 0; otherwise the panel is
/// unchanged from the legacy single-line skeleton.
pub(crate) fn render_scan_progress(
    buf: &mut Buffer,
    area: Rect,
    progress: &ScanProgressEvent,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CORNFLOWER_BLUE))
        .style(Style::default().bg(DARK_BG));
    let inner = block.inner(area);
    ratatui::widgets::Widget::render(block, area, buf);
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let mut spans = vec![
        Span::styled("  ⏳ ", Style::default().fg(GOLD)),
        Span::styled(
            scan_progress_headline(progress),
            Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD),
        ),
    ];
    if !progress.current_project.is_empty() {
        spans.push(Span::styled(" · ", Style::default().fg(MUTED_GRAY)));
        spans.push(Span::styled(
            progress.current_project.clone(),
            Style::default().fg(MUTED_GRAY),
        ));
    }

    // When `total` is known and the panel has room for a second row,
    // split the inner area into a 1-row headline + 1-row gauge. When
    // `total` is 0 (or the panel is too short for 2 rows), fall back to
    // the legacy single-line skeleton so we never render a bar with
    // bogus 0% data.
    let show_gauge = progress.total > 0 && inner.height >= 2;
    if !show_gauge {
        let paragraph = Paragraph::new(Line::from(spans));
        ratatui::widgets::Widget::render(paragraph, inner, buf);
        return;
    }

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
        .split(inner);

    let paragraph = Paragraph::new(Line::from(spans));
    ratatui::widgets::Widget::render(paragraph, layout[0], buf);

    // Cap the ratio at 1.0 in case `scanned` overshoots `total` (can
    // happen briefly if files are added mid-scan — ProgressReporter's
    // counter is monotonic but `total` is the pre-walk snapshot).
    let ratio = (f64::from(progress.scanned) / f64::from(progress.total)).clamp(0.0, 1.0);
    let pct = (ratio * 100.0).round() as u16;
    let gauge = Gauge::default()
        .gauge_style(
            Style::default()
                .fg(SELECTION_GREEN)
                .bg(PANEL_BG)
                .add_modifier(Modifier::BOLD),
        )
        .label(format!(
            "{pct:>3}% ({}/{})",
            progress.scanned, progress.total
        ))
        .ratio(ratio);
    ratatui::widgets::Widget::render(gauge, layout[1], buf);
}

/// Format the scanned/total counters into the headline portion of the
/// skeleton line. Split out so the unit test can assert on the exact
/// string without dragging in the ratatui rendering machinery.
pub(crate) fn scan_progress_headline(progress: &ScanProgressEvent) -> String {
    if progress.total > 0 {
        format!("Scanning sessions: {}/{}", progress.scanned, progress.total)
    } else {
        format!("Scanning sessions… {} files", progress.scanned)
    }
}

fn render_daily(buf: &mut Buffer, area: Rect, data: &UsageData, scroll_offset: usize) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CORNFLOWER_BLUE))
        .style(Style::default().bg(DARK_BG));

    let inner = block.inner(area);
    ratatui::widgets::Widget::render(block, area, buf);

    if data.daily.is_empty() {
        let p = Paragraph::new("  No usage data found.").style(Style::default().fg(MUTED_GRAY));
        ratatui::widgets::Widget::render(p, inner, buf);
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

    ratatui::widgets::Widget::render(table, chunks[0], buf);

    // Bar chart (last N days that fit)
    render_bar_chart(buf, chunks[1], data);
}

fn render_weekly(buf: &mut Buffer, area: Rect, data: &UsageData, scroll_offset: usize) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CORNFLOWER_BLUE))
        .style(Style::default().bg(DARK_BG));

    let inner = block.inner(area);
    ratatui::widgets::Widget::render(block, area, buf);

    if data.weekly.is_empty() {
        let p = Paragraph::new("  No usage data found.").style(Style::default().fg(MUTED_GRAY));
        ratatui::widgets::Widget::render(p, inner, buf);
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

    ratatui::widgets::Widget::render(table, inner, buf);
}

fn render_projects(buf: &mut Buffer, area: Rect, data: &UsageData, scroll_offset: usize) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CORNFLOWER_BLUE))
        .style(Style::default().bg(DARK_BG));

    let inner = block.inner(area);
    ratatui::widgets::Widget::render(block, area, buf);

    if data.projects.is_empty() {
        let p = Paragraph::new("  No usage data found.").style(Style::default().fg(MUTED_GRAY));
        ratatui::widgets::Widget::render(p, inner, buf);
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

    ratatui::widgets::Widget::render(table, inner, buf);
}

fn render_burndown(buf: &mut Buffer, area: Rect, data: &UsageData, state: &UsageViewState) {
    let block = Block::default()
        .title(" [ Burndown ] ")
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(TERMINAL_BORDER))
        .style(Style::default().bg(TERMINAL_BG));

    let inner = block.inner(area);
    ratatui::widgets::Widget::render(block, area, buf);

    if data.calls.is_empty() {
        let p = Paragraph::new("  No usage data found for selected period/provider/filter.")
            .style(Style::default().fg(MUTED_GRAY));
        ratatui::widgets::Widget::render(p, inner, buf);
        return;
    }

    // Apply the full filter surface client-side: cross-filter chips
    // (project/model/activity/session/branch), the period date range
    // (1/2/3/a or specific month/quarter), and the provider selector
    // (Right/Left arrow). All three feed `filter_usage_data_full`,
    // which re-aggregates from the in-memory call set so every panel
    // and the header reflect the active pivot — grafana-style global
    // filters.
    //
    // Fast path: if the plugin pre-populated `cached_filtered`, reuse
    // the Arc without re-running `analyze_turns` + the aggregate
    // pivot. Drops per-render cost from O(N) to O(1) for repeated
    // renders on the same filter state.
    //
    // Slow path (`filtered_owned` populated): no cache hit, so
    // recompute inline. Tests and the CLI snapshot path land here.
    let any_filter_active = state.filters.any()
        || !matches!(state.period, UsagePeriod::All)
        || !matches!(state.provider_filter, UsageProviderFilter::All);
    let filtered_owned: Option<UsageData> = if any_filter_active
        && state.cached_filtered.is_none()
    {
        Some(filter_usage_data_full(
            data,
            &state.filters,
            &state.period,
            state.provider_filter,
        ))
    } else {
        None
    };
    let view_data: &UsageData = if any_filter_active {
        state
            .cached_filtered
            .as_deref()
            .unwrap_or_else(|| filtered_owned.as_ref().expect("filtered_owned set above"))
    } else {
        data
    };

    // Zoom takes the full inner area minus a small breadcrumb and an
    // optional search box. Skip the dashboard grid entirely.
    if let Some(panel) = state.zoom {
        render_burndown_zoomed(buf, inner, view_data, state, panel);
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

    render_burndown_header(buf, vertical[0], view_data, &state.period);
    render_period_row(buf, vertical[1], state);
    render_filter_chip_strip(buf, vertical[2], state);

    if vertical[3].width >= 120 && vertical[3].height >= 22 {
        render_dashboard_grid(buf, vertical[3], view_data, &state.period, state);
    } else if vertical[3].width >= 96 {
        render_dashboard_compact(buf, vertical[3], view_data, state);
    } else {
        render_dashboard_stack(buf, vertical[3], view_data, state);
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
    buf: &mut Buffer,
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

    render_zoom_breadcrumb(buf, vertical[0], panel);
    if search_h > 0 {
        render_zoom_search_bar(buf, vertical[1], state);
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

    render_zoom_panel_body(buf, panel_area, data, state, panel);

    if let Some(detail) = detail_area {
        render_zoom_detail_drawer(buf, detail, data, state, panel);
    }
}

fn render_zoom_breadcrumb(buf: &mut Buffer, area: Rect, panel: UsagePanel) {
    let line = Line::from(vec![
        Span::styled(
            format!(" [ Zoomed: {} ] ", panel.title()),
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ),
        Span::styled("   ", Style::default()),
        Span::styled("◀ BkSp unzoom · Esc home ▶", Style::default().fg(MUTED_GRAY)),
    ]);
    ratatui::widgets::Widget::render(Paragraph::new(line), area, buf);
}

/// Score `haystack` against `query` with nucleo-matcher; `None` means
/// no match. An empty query matches everything (returns `Some(0)`).
///
/// When either side carries non-ASCII codepoints we route through
/// `Utf32String` so multibyte chars match natively. The previous code
/// fed `Utf32Str::Ascii(haystack.as_bytes())` even when the query had
/// non-ASCII content, which silently lost matches.
fn fuzzy_score(matcher: &mut nucleo_matcher::Matcher, query: &str, haystack: &str) -> Option<u32> {
    if query.is_empty() {
        return Some(0);
    }
    let needle = nucleo_matcher::pattern::Pattern::parse(
        query,
        nucleo_matcher::pattern::CaseMatching::Smart,
        nucleo_matcher::pattern::Normalization::Smart,
    );
    if query.is_ascii() && haystack.is_ascii() {
        needle.score(
            nucleo_matcher::Utf32Str::Ascii(haystack.as_bytes()),
            matcher,
        )
    } else {
        let utf32 = nucleo_matcher::Utf32String::from(haystack);
        needle.score(utf32.slice(..), matcher)
    }
}

/// Filter `rows` by a fuzzy-search query, preserving original order.
/// Empty query returns all rows. The `label` closure projects each row
/// to its primary search string (project name, session id, etc.).
///
/// Pattern parsing happens once before the filter loop (was per-call
/// inside the filter via fuzzy_score). Worth doing because zoom-mode
/// re-renders typing-rate (~10/s) over potentially 1k+ row sets.
fn apply_zoom_filter<'a, T, F>(rows: &'a [T], query: &str, label: F) -> Vec<&'a T>
where
    F: Fn(&T) -> String,
{
    if query.is_empty() {
        return rows.iter().collect();
    }
    let needle = nucleo_matcher::pattern::Pattern::parse(
        query,
        nucleo_matcher::pattern::CaseMatching::Smart,
        nucleo_matcher::pattern::Normalization::Smart,
    );
    let query_ascii = query.is_ascii();
    let mut matcher = nucleo_matcher::Matcher::new(nucleo_matcher::Config::DEFAULT);
    rows.iter()
        .filter_map(|row| {
            let label = label(row);
            let score = if query_ascii && label.is_ascii() {
                needle.score(
                    nucleo_matcher::Utf32Str::Ascii(label.as_bytes()),
                    &mut matcher,
                )
            } else {
                let utf32 = nucleo_matcher::Utf32String::from(label.as_str());
                needle.score(utf32.slice(..), &mut matcher)
            };
            score.map(|_| row)
        })
        .collect()
}

fn render_zoom_search_bar(buf: &mut Buffer, area: Rect, state: &UsageViewState) {
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
    ratatui::widgets::Widget::render(Paragraph::new(line), area, buf);
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
    buf: &mut Buffer,
    area: Rect,
    data: &UsageData,
    state: &UsageViewState,
    panel: UsagePanel,
) {
    let q = state.zoom_search_query.as_str();
    match panel {
        UsagePanel::ByProject => render_zoom_by_project(buf, area, data, state, q),
        UsagePanel::ByBranch => render_zoom_by_branch(buf, area, data, state, q),
        UsagePanel::TopSessions => render_zoom_top_sessions(buf, area, data, state, q),
        UsagePanel::Live => render_zoom_top_sessions(buf, area, data, state, q),
        UsagePanel::ByModel => render_zoom_by_model(buf, area, data, state, q),
        UsagePanel::ByActivity => render_zoom_by_activity(buf, area, data, state, q),
        UsagePanel::DailyActivity => render_zoom_daily_activity(buf, area, data, state, q),
        UsagePanel::Leaderboard => render_zoom_by_project(buf, area, data, state, q),
        UsagePanel::CoreTools => {
            render_zoom_named(buf, area, "Core Tools", &data.tools, state, q)
        }
        UsagePanel::ShellCommands => render_zoom_named(
            buf,
            area,
            "Shell Commands",
            &data.shell_commands,
            state,
            q,
        ),
        UsagePanel::McpServers => {
            render_zoom_named(buf, area, "MCP Servers", &data.mcp_servers, state, q)
        }
        UsagePanel::Optimize | UsagePanel::Budget => {
            // These panels are summary cards rather than row lists. We
            // reuse the standard renderers in a fullscreen frame.
            let focus = FocusCtx::for_panel(state, panel);
            if matches!(panel, UsagePanel::Optimize) {
                render_optimize_compact_panel(buf, area, data, focus);
            } else {
                render_budget_panel(buf, area, data, &state.period, focus);
            }
        }
    }
}

// TODO(refactor): the 5 render_zoom_* fns below all build a header,
// truncate to area.height-2 visible rows, build per-row cells, and
// hand a Table back to the frame. Extract a `render_zoom_table` helper
// taking `ZoomTableSpec { headers, widths, rows }` once snapshot tests
// exist to assert visual identity — without them a refactor risks
// silent column-spacing/header-style drift.
fn render_zoom_by_project(
    buf: &mut Buffer,
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
    ratatui::widgets::Widget::render(table, area, buf);
}

/// Zoom view for the By Branch panel. `BranchUsage` carries only the
/// branch label and a `TokenBucket`, so the zoom table is intentionally
/// narrow — `Branch | Tokens | Cost` is everything we have. Search
/// matches on the branch name.
fn render_zoom_by_branch(
    buf: &mut Buffer,
    area: Rect,
    data: &UsageData,
    _state: &UsageViewState,
    query: &str,
) {
    let rows = apply_zoom_filter(&data.branches, query, |b| b.branch.clone());
    let header = Row::new(vec!["#", "Branch", "Tokens", "Cost"])
        .style(Style::default().fg(GOLD).add_modifier(Modifier::BOLD))
        .bottom_margin(1);

    let visible_rows = area.height.saturating_sub(2) as usize;
    let table_rows: Vec<Row> = rows
        .iter()
        .enumerate()
        .take(visible_rows)
        .map(|(idx, row)| {
            let b = &row.bucket;
            Row::new(vec![
                format!("{}", idx + 1),
                row.branch.clone(),
                format_tokens_short(b.total()),
                format_cost(b.cost_usd),
            ])
            .style(Style::default().fg(SOFT_WHITE))
        })
        .collect();

    let widths = [
        Constraint::Length(4),
        Constraint::Min(20),
        Constraint::Length(10),
        Constraint::Length(10),
    ];
    let table = Table::new(table_rows, widths).header(header).column_spacing(1);
    ratatui::widgets::Widget::render(table, area, buf);
}

fn render_zoom_top_sessions(
    buf: &mut Buffer,
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
            let dur = (sess.last_timestamp - sess.first_timestamp).num_seconds().max(0);
            let dur_str = format_duration_min(dur as u64);
            Row::new(vec![
                sess.provider.clone(),
                truncate_string(&sess.project, 24),
                truncate_string(&sess.session_id, 18),
                format_cost(b.cost_usd),
                format_tokens_short(b.total()),
                b.call_count.to_string(),
                dur_str,
                sess.last_timestamp.with_timezone(&chrono::Local).format("%Y-%m-%d").to_string(),
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
    ratatui::widgets::Widget::render(table, area, buf);
}

fn render_zoom_by_model(
    buf: &mut Buffer,
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
    ratatui::widgets::Widget::render(table, area, buf);
}

fn render_zoom_by_activity(
    buf: &mut Buffer,
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
    ratatui::widgets::Widget::render(table, area, buf);
}

fn render_zoom_daily_activity(
    buf: &mut Buffer,
    area: Rect,
    data: &UsageData,
    _state: &UsageViewState,
    query: &str,
) {
    // Daily rows are (date, bucket) tuples — search over the date string.
    let rows: Vec<&(NaiveDate, crate::data::usage::TokenBucket)> = if query.is_empty() {
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
    ratatui::widgets::Widget::render(table, area, buf);
}

fn render_zoom_named(
    buf: &mut Buffer,
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
    ratatui::widgets::Widget::render(table, area, buf);
}

/// Render the detail drawer for the currently-selected row in the
/// zoomed panel. Static info card; no extra fetching for PR-C.
fn render_zoom_detail_drawer(
    buf: &mut Buffer,
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
    ratatui::widgets::Widget::render(block, area, buf);

    let lines = build_detail_lines(data, state, panel);
    let paragraph = Paragraph::new(lines).style(Style::default().fg(SOFT_WHITE));
    ratatui::widgets::Widget::render(paragraph, inner, buf);
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
        UsagePanel::ByBranch => {
            if let Some(b) = data.branches.get(row) {
                lines.push(kv("Branch", b.branch.clone()));
                lines.push(kv("Cost", format_cost(b.bucket.cost_usd)));
                lines.push(kv("Tokens", format_tokens_short(b.bucket.total())));
                lines.push(kv("Calls", b.bucket.call_count.to_string()));
            }
        }
        UsagePanel::TopSessions | UsagePanel::Live => {
            if let Some(s) = data.sessions.get(row) {
                lines.push(kv("Session", s.session_id.clone()));
                lines.push(kv("Project", s.project.clone()));
                lines.push(kv("Provider", s.provider.clone()));
                lines.push(kv(
                    "First seen",
                    s.first_timestamp
                        .with_timezone(&chrono::Local)
                        .format("%Y-%m-%d %H:%M")
                        .to_string(),
                ));
                lines.push(kv(
                    "Last seen",
                    s.last_timestamp
                        .with_timezone(&chrono::Local)
                        .format("%Y-%m-%d %H:%M")
                        .to_string(),
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
        min_ts.with_timezone(&chrono::Local).format("%Y-%m-%d").to_string(),
        max_ts.with_timezone(&chrono::Local).format("%Y-%m-%d").to_string(),
    )
}

/// Top `n` projects by call count for `model_name`, joined with "·".
/// Returns "—" when the model has no calls in `data.calls`.
///
/// Reads from the precomputed `data.model_project_counts` index built
/// during `aggregate_calls`. Render path is O(n) (constant) instead of
/// O(N) over `data.calls` per call.
fn top_projects_for_model(data: &UsageData, model_name: &str, n: usize) -> String {
    let Some(rows) = data.model_project_counts.get(model_name) else {
        return "—".to_string();
    };
    if rows.is_empty() {
        return "—".to_string();
    }
    rows.iter().take(n).map(|(p, _)| p.clone()).collect::<Vec<_>>().join(" · ")
}

/// Render a duration (in seconds) as `1h 04m` / `42m` / `<1m` / `0m`.
///
/// Distinguishes a true zero duration (`first == last` timestamp, e.g. a
/// session with one logged turn) from a positive sub-minute duration
/// (rounded down to 0 minutes). Both used to render as `<1m`, which
/// hid the zero case.
fn format_duration_min(secs_total: u64) -> String {
    if secs_total == 0 {
        return "0m".to_string();
    }
    let min_total = secs_total / 60;
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
    buf: &mut Buffer,
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

    // Row 0 widens to four columns so the headline "where my tokens
    // went" cluster (Daily Activity | By Project | By Branch | Live)
    // reads left-to-right. Rows 1–3 stay 3-column.
    let top = four_columns(rows[0]);
    render_daily_activity_panel(
        buf,
        top[0],
        data,
        FocusCtx::for_panel(state, UsagePanel::DailyActivity),
    );
    render_project_panel(
        buf,
        top[1],
        &data.projects,
        FocusCtx::for_panel(state, UsagePanel::ByProject),
    );
    render_branch_panel(
        buf,
        top[2],
        &data.branches,
        FocusCtx::for_panel(state, UsagePanel::ByBranch),
    );
    render_live_panel(
        buf,
        top[3],
        &data.sessions,
        FocusCtx::for_panel(state, UsagePanel::Live),
    );

    let middle = three_columns(rows[1]);
    render_session_panel(
        buf,
        middle[0],
        &data.sessions,
        FocusCtx::for_panel(state, UsagePanel::TopSessions),
    );
    render_activity_panel(
        buf,
        middle[1],
        &data.activities,
        FocusCtx::for_panel(state, UsagePanel::ByActivity),
    );
    render_model_panel(
        buf,
        middle[2],
        &data.models,
        FocusCtx::for_panel(state, UsagePanel::ByModel),
    );

    let lower = three_columns(rows[2]);
    render_named_panel(
        buf,
        lower[0],
        "Core Tools",
        &data.tools,
        FocusCtx::for_panel(state, UsagePanel::CoreTools),
    );
    render_named_panel(
        buf,
        lower[1],
        "Shell Commands",
        &data.shell_commands,
        FocusCtx::for_panel(state, UsagePanel::ShellCommands),
    );
    render_named_panel(
        buf,
        lower[2],
        "MCP Servers",
        &data.mcp_servers,
        FocusCtx::for_panel(state, UsagePanel::McpServers),
    );

    let bottom = three_columns(rows[3]);
    render_optimize_compact_panel(
        buf,
        bottom[0],
        data,
        FocusCtx::for_panel(state, UsagePanel::Optimize),
    );
    render_leaderboard_panel(
        buf,
        bottom[1],
        data,
        FocusCtx::for_panel(state, UsagePanel::Leaderboard),
    );
    render_budget_panel(
        buf,
        bottom[2],
        data,
        period,
        FocusCtx::for_panel(state, UsagePanel::Budget),
    );
}

fn render_dashboard_compact(
    buf: &mut Buffer,
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
        buf,
        left[0],
        data,
        FocusCtx::for_panel(state, UsagePanel::DailyActivity),
    );
    render_project_panel(
        buf,
        left[1],
        &data.projects,
        FocusCtx::for_panel(state, UsagePanel::ByProject),
    );
    render_session_panel(
        buf,
        left[2],
        &data.sessions,
        FocusCtx::for_panel(state, UsagePanel::TopSessions),
    );
    render_live_panel(
        buf,
        left[3],
        &data.sessions,
        FocusCtx::for_panel(state, UsagePanel::Live),
    );

    render_activity_panel(
        buf,
        right[0],
        &data.activities,
        FocusCtx::for_panel(state, UsagePanel::ByActivity),
    );
    // Compact grid (≥96w, <120w): split row 1 of the right column to
    // host By Model and By Branch side-by-side. Branches typically have
    // few rows so a half-width column reads fine without redistributing
    // the existing four-row vertical budget.
    let row1 = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(right[1]);
    render_model_panel(
        buf,
        row1[0],
        &data.models,
        FocusCtx::for_panel(state, UsagePanel::ByModel),
    );
    render_branch_panel(
        buf,
        row1[1],
        &data.branches,
        FocusCtx::for_panel(state, UsagePanel::ByBranch),
    );
    render_optimize_compact_panel(
        buf,
        right[2],
        data,
        FocusCtx::for_panel(state, UsagePanel::Optimize),
    );
    let tools = three_columns(right[3]);
    render_named_panel(
        buf,
        tools[0],
        "Core Tools",
        &data.tools,
        FocusCtx::for_panel(state, UsagePanel::CoreTools),
    );
    render_named_panel(
        buf,
        tools[1],
        "Shell Commands",
        &data.shell_commands,
        FocusCtx::for_panel(state, UsagePanel::ShellCommands),
    );
    render_named_panel(
        buf,
        tools[2],
        "MCP Servers",
        &data.mcp_servers,
        FocusCtx::for_panel(state, UsagePanel::McpServers),
    );
}

fn render_dashboard_stack(buf: &mut Buffer, area: Rect, data: &UsageData, state: &UsageViewState) {
    // Narrow-width stack (<96w): six equal-ish rows. ByBranch sits at
    // the bottom so the pre-existing top-of-stack reading order
    // (activity → projects → sessions → activity → models) is
    // preserved.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(17),
            Constraint::Percentage(17),
            Constraint::Percentage(17),
            Constraint::Percentage(17),
            Constraint::Percentage(16),
            Constraint::Percentage(16),
        ])
        .split(area);

    render_daily_activity_panel(
        buf,
        chunks[0],
        data,
        FocusCtx::for_panel(state, UsagePanel::DailyActivity),
    );
    render_project_panel(
        buf,
        chunks[1],
        &data.projects,
        FocusCtx::for_panel(state, UsagePanel::ByProject),
    );
    render_session_panel(
        buf,
        chunks[2],
        &data.sessions,
        FocusCtx::for_panel(state, UsagePanel::TopSessions),
    );
    render_activity_panel(
        buf,
        chunks[3],
        &data.activities,
        FocusCtx::for_panel(state, UsagePanel::ByActivity),
    );
    render_model_panel(
        buf,
        chunks[4],
        &data.models,
        FocusCtx::for_panel(state, UsagePanel::ByModel),
    );
    render_branch_panel(
        buf,
        chunks[5],
        &data.branches,
        FocusCtx::for_panel(state, UsagePanel::ByBranch),
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

fn four_columns(area: Rect) -> std::rc::Rc<[Rect]> {
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
        ])
        .split(area)
}

fn render_burndown_header(buf: &mut Buffer, area: Rect, data: &UsageData, period: &UsagePeriod) {
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
    ratatui::widgets::Widget::render(Paragraph::new(lines), area, buf);
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
        ("7d", '2', |p| {
            matches!(p, UsagePeriod::Week | UsagePeriod::LastNDays(7))
        }),
        ("30d", '3', |p| {
            matches!(p, UsagePeriod::ThirtyDays | UsagePeriod::LastNDays(30))
        }),
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
            (today.year(), crate::data::usage::quarter_of(today))
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

fn render_period_row(buf: &mut Buffer, area: Rect, state: &UsageViewState) {
    let lines = build_period_provider_strip(state);
    ratatui::widgets::Widget::render(Paragraph::new(lines), area, buf);
}

/// Build the chip strip line shown directly under the period+provider
/// strip. Active chips render as `[label=value]` in GOLD; with no
/// chips, an instruction hint is shown so users discover the pivot.
///
/// When `state.fresh_pivot` is true (set by the plugin's render path on
/// the single frame after a cache miss), a brief `↻ updated` badge is
/// appended in `SELECTION_GREEN` so the user sees confirmation their
/// chip pivot landed even when the wall-clock compute was too fast to
/// feel like a wait.
pub fn build_filter_chip_line(state: &UsageViewState) -> Line<'static> {
    let mut spans: Vec<Span<'static>> =
        vec![Span::styled("Filters: ", Style::default().fg(MUTED_GRAY))];
    if !state.filters.any() {
        spans.push(Span::styled(
            "(none)",
            Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
        ));
        if state.fresh_pivot {
            push_fresh_pivot_badge(&mut spans);
        }
        return Line::from(spans);
    }
    push_chip_group(&mut spans, "project", &state.filters.project);
    push_chip_group(&mut spans, "model", &state.filters.model);
    push_chip_group(&mut spans, "activity", &state.filters.activity);
    push_chip_group(&mut spans, "session", &state.filters.session);
    push_chip_group(&mut spans, "branch", &state.filters.branch);
    push_exclude_chip_group(&mut spans, "project", &state.filters.exclude_project);
    push_exclude_chip_group(&mut spans, "model", &state.filters.exclude_model);
    push_exclude_chip_group(&mut spans, "activity", &state.filters.exclude_activity);
    push_exclude_chip_group(&mut spans, "session", &state.filters.exclude_session);
    push_exclude_chip_group(&mut spans, "branch", &state.filters.exclude_branch);
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
    if state.fresh_pivot {
        push_fresh_pivot_badge(&mut spans);
    }
    Line::from(spans)
}

/// Append the fresh-pivot badge to the chip strip. Rendered in green
/// to visually distinguish from the gold chip group — the eye reads
/// it as "I just did the thing" rather than another filter state.
fn push_fresh_pivot_badge(spans: &mut Vec<Span<'static>>) {
    spans.push(Span::raw("  "));
    spans.push(Span::styled(
        "↻ updated",
        Style::default()
            .fg(SELECTION_GREEN)
            .add_modifier(Modifier::BOLD),
    ));
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

/// Exclude chips render with a leading `~` and a muted-orange fill so
/// they are immediately distinguishable from include (gold) chips.
/// They also pop first via Esc — see `UsageFilters::pop_last`.
fn push_exclude_chip_group(spans: &mut Vec<Span<'static>>, label: &str, values: &[String]) {
    for value in values {
        let chip_text = format!(" ~{label}={value} ");
        spans.push(Span::styled(
            chip_text,
            Style::default().fg(DARK_BG).bg(BAR_HIGH).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw(" "));
    }
}

fn render_filter_chip_strip(buf: &mut Buffer, area: Rect, state: &UsageViewState) {
    let line = build_filter_chip_line(state);
    ratatui::widgets::Widget::render(Paragraph::new(line), area, buf);
}

fn render_daily_activity_panel(buf: &mut Buffer, area: Rect, data: &UsageData, focus: FocusCtx) {
    let cap = area.height.saturating_sub(2) as usize;
    let inner_w = area.width.saturating_sub(2) as usize;
    if cap == 0 || inner_w < 16 {
        render_panel_lines_with_focus(buf, area, "Daily Activity", vec![], focus);
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
    render_panel_lines_with_focus(buf, area, "Daily Activity", lines, focus);
}

fn render_project_panel(buf: &mut Buffer, area: Rect, rows: &[ProjectUsage], focus: FocusCtx) {
    let cap = area.height.saturating_sub(2) as usize;
    let inner_w = area.width.saturating_sub(2) as usize;
    if cap == 0 || inner_w < 16 {
        render_panel_lines_with_focus(buf, area, "By Project", vec![], focus);
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
    render_panel_lines_with_focus(buf, area, "By Project", lines, focus);
}

/// Per-branch cost bars. Mirrors `render_project_panel` (same gradient
/// layout / column widths) but skips the worktree-aware project label
/// massaging — branch names are short already, so plain truncation is
/// fine. Branchless calls were already dropped during aggregation
/// (`UsageData.branches` only contains `Some(branch)` rows), so this
/// panel never grows a misleading "(no branch)" bucket.
fn render_branch_panel(buf: &mut Buffer, area: Rect, rows: &[BranchUsage], focus: FocusCtx) {
    let cap = area.height.saturating_sub(2) as usize;
    let inner_w = area.width.saturating_sub(2) as usize;
    if cap == 0 || inner_w < 16 {
        render_panel_lines_with_focus(buf, area, "By Branch", vec![], focus);
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
            let label = truncate_string(&row.branch, label_w);
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
    render_panel_lines_with_focus(buf, area, "By Branch", lines, focus);
}

fn render_session_panel(buf: &mut Buffer, area: Rect, rows: &[SessionUsage], focus: FocusCtx) {
    let cap = area.height.saturating_sub(2) as usize;
    let inner_w = area.width.saturating_sub(2) as usize;
    if cap == 0 || inner_w < 16 {
        render_panel_lines_with_focus(buf, area, "Top Sessions", vec![], focus);
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
    render_panel_lines_with_focus(buf, area, "Top Sessions", lines, focus);
}

fn render_live_panel(buf: &mut Buffer, area: Rect, rows: &[SessionUsage], focus: FocusCtx) {
    let cap = area.height.saturating_sub(2) as usize;
    let inner_w = area.width.saturating_sub(2) as usize;
    if cap == 0 || inner_w < 16 {
        render_panel_lines_with_focus(buf, area, "Live Session Ticker", vec![], focus);
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
    render_panel_lines_with_focus(buf, area, "Live Session Ticker", lines, focus);
}

fn render_activity_panel(buf: &mut Buffer, area: Rect, rows: &[ActivityUsage], focus: FocusCtx) {
    let cap = area.height.saturating_sub(2) as usize;
    let inner_w = area.width.saturating_sub(2) as usize;
    if cap == 0 || inner_w < 16 {
        render_panel_lines_with_focus(buf, area, "By Activity", vec![], focus);
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
    render_panel_lines_with_focus(buf, area, "By Activity", lines, focus);
}

fn render_model_panel(buf: &mut Buffer, area: Rect, rows: &[ModelUsage], focus: FocusCtx) {
    let cap = area.height.saturating_sub(2) as usize;
    let inner_w = area.width.saturating_sub(2) as usize;
    if cap == 0 || inner_w < 16 {
        render_panel_lines_with_focus(buf, area, "By Model", vec![], focus);
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
    render_panel_lines_with_focus(buf, area, "By Model", lines, focus);
}

fn render_named_panel(
    buf: &mut Buffer,
    area: Rect,
    title: &str,
    rows: &[NamedUsage],
    focus: FocusCtx,
) {
    let cap = area.height.saturating_sub(2) as usize;
    let inner_w = area.width.saturating_sub(2) as usize;
    if cap == 0 || inner_w < 14 {
        render_panel_lines_with_focus(buf, area, title, vec![], focus);
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
    render_panel_lines_with_focus(buf, area, title, lines, focus);
}

fn render_optimize_compact_panel(buf: &mut Buffer, area: Rect, data: &UsageData, focus: FocusCtx) {
    let cap = area.height.saturating_sub(2) as usize;
    let inner_w = area.width.saturating_sub(2) as usize;
    if cap == 0 {
        render_panel_lines_with_focus(buf, area, "Optimization Recommendations", vec![], focus);
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
    render_panel_lines_with_focus(buf, area, "Optimization Recommendations", lines, focus);
}

fn render_leaderboard_panel(buf: &mut Buffer, area: Rect, data: &UsageData, focus: FocusCtx) {
    let cap = area.height.saturating_sub(2) as usize;
    let inner_w = area.width.saturating_sub(2) as usize;
    if cap == 0 || inner_w < 16 {
        render_panel_lines_with_focus(buf, area, "Agent Leaderboard", vec![], focus);
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
    render_panel_lines_with_focus(buf, area, "Agent Leaderboard", lines, focus);
}

fn render_budget_panel(
    buf: &mut Buffer,
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

    // Live OAuth-window header. Renders above the existing monthly-cap
    // content. Drives the W keybind's CTA when no source is available.
    let live = crate::live_window::current();
    let mut lines: Vec<Line> = budget_live_header_lines(&live, inner_w);

    lines.extend(vec![
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
    ]);
    render_panel_lines_with_focus(buf, area, "Budget · Alerts", lines, focus);
}

/// Build the live-window header injected at the top of the Budget panel.
/// Tier1 → two gradient bars + cost + reset countdown; Tier2 → 5h bar +
/// upgrade hint; None → CTA pointing at the W keybind.
fn budget_live_header_lines(
    live: &crate::live_window::LiveWindow,
    inner_w: usize,
) -> Vec<Line<'static>> {
    use crate::live_window::Source;
    let mut out: Vec<Line<'static>> = Vec::new();
    let bar_w = inner_w.saturating_sub(12).max(8);

    match live.source {
        Source::Tier1Cache => {
            if let Some(pct) = live.five_hour_pct {
                out.push(live_bar_line("5h burn ", pct, bar_w));
            }
            if let Some(pct) = live.seven_day_pct {
                out.push(live_bar_line("7d wnd  ", pct, bar_w));
            }
            let mut footer: Vec<Span<'static>> = Vec::new();
            if let Some(cost) = live.today_cost_usd {
                footer.push(Span::styled(" Today ", Style::default().fg(MUTED_GRAY)));
                footer.push(Span::styled(
                    format!("${cost:.2}"),
                    Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                ));
            }
            if let Some(d) = live.resets_in {
                if !footer.is_empty() {
                    footer.push(Span::styled(" · ", Style::default().fg(MUTED_GRAY)));
                }
                footer.push(Span::styled(" Resets in ", Style::default().fg(MUTED_GRAY)));
                footer.push(Span::styled(
                    format_hms(d),
                    Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD),
                ));
            }
            if !footer.is_empty() {
                out.push(Line::from(footer));
            }
        }
        Source::Tier2Local => {
            if let Some(pct) = live.five_hour_pct {
                out.push(live_bar_line("5h burn ", pct, bar_w));
            }
            out.push(Line::from(Span::styled(
                " Wire statusline (W) for 7d window + cost",
                Style::default().fg(MUTED_GRAY),
            )));
        }
        Source::None => {
            out.push(Line::from(Span::styled(
                " ⓘ Live OAuth window data not available.",
                Style::default().fg(MUTED_GRAY),
            )));
            out.push(Line::from(vec![
                Span::styled("    Press ", Style::default().fg(MUTED_GRAY)),
                Span::styled("[W]", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
                Span::styled(
                    " to wire up Claude Code statusline.",
                    Style::default().fg(MUTED_GRAY),
                ),
            ]));
            out.push(Line::from(Span::styled(
                "    (Provides 5h burn, 7d window, cost, reset times)",
                Style::default().fg(MUTED_GRAY),
            )));
        }
    }
    out
}

fn live_bar_line(label: &'static str, pct: u8, bar_w: usize) -> Line<'static> {
    let mut spans = vec![Span::styled(
        format!(" {label}"),
        Style::default().fg(MUTED_GRAY),
    )];
    spans.extend(ratio_gradient_spans(pct as f64, 100.0, bar_w));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        format!("{pct:>3}%"),
        Style::default()
            .fg(if pct >= 85 {
                BAR_HIGH
            } else if pct >= 60 {
                TERMINAL_ACCENT
            } else {
                TERMINAL_GOOD
            })
            .add_modifier(Modifier::BOLD),
    ));
    Line::from(spans)
}

fn format_hms(d: std::time::Duration) -> String {
    let total = d.as_secs();
    let h = total / 3600;
    let m = (total % 3600) / 60;
    if h > 0 {
        format!("{h}h {m:02}m")
    } else {
        format!("{m}m")
    }
}

fn render_optimize(buf: &mut Buffer, area: Rect, data: &UsageData) {
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
    render_panel(buf, area, "Optimize Findings", lines);
}

fn render_panel(buf: &mut Buffer, area: Rect, title: &str, rows: Vec<String>) {
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
    render_panel_lines(buf, area, title, lines);
}

fn render_panel_lines(buf: &mut Buffer, area: Rect, title: &str, lines: Vec<Line<'_>>) {
    render_panel_lines_with_focus(buf, area, title, lines, FocusCtx::unfocused());
}

/// Render variant that knows about focus: highlights the border in
/// `BAR_HIGH` and replaces the leading-space cell on the focused row
/// with a `▶` indicator. Row clamping is done here too — the state
/// only knows about logical row index, not how many rows the panel is
/// currently displaying.
fn render_panel_lines_with_focus(
    buf: &mut Buffer,
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
    ratatui::widgets::Widget::render(Paragraph::new(final_lines).block(block), area, buf);
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
            // Need room for repo + ':' + at least 2 branch chars. With
            // branch_w == 1 truncate_string returns just `…`, producing
            // `repo:…` which conveys nothing — fall through to the
            // generic shortener instead so we keep the repo name whole.
            if repo_w + 3 <= max_w {
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
            let last = crate::data::usage::last_day_of_month(anchor.year(), anchor.month())?;
            Some((last - first).num_days().max(0) as u64 + 1)
        }
        UsagePeriod::SpecificQuarter(year, q) => {
            let (first, last) = crate::data::usage::quarter_bounds(*year, *q);
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

fn render_bar_chart(buf: &mut Buffer, area: Rect, data: &UsageData) {
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
    ratatui::widgets::Widget::render(paragraph, area, buf);
}

fn render_help_bar(buf: &mut Buffer, area: Rect, state: &UsageViewState) {
    // When zoomed, swap to a focused help string so the user has the
    // zoom-only affordances visible.
    if state.is_zoomed() {
        let spans = vec![
            Span::styled(" /", Style::default().fg(GOLD)),
            Span::styled(" search  ", Style::default().fg(MUTED_GRAY)),
            Span::styled("d", Style::default().fg(GOLD)),
            Span::styled(" detail  ", Style::default().fg(MUTED_GRAY)),
            Span::styled("z/BkSp", Style::default().fg(GOLD)),
            Span::styled(" unzoom  ", Style::default().fg(MUTED_GRAY)),
            Span::styled("Esc", Style::default().fg(GOLD)),
            Span::styled(" home  ", Style::default().fg(MUTED_GRAY)),
            Span::styled("j/k", Style::default().fg(GOLD)),
            Span::styled(" row  ", Style::default().fg(MUTED_GRAY)),
        ];
        let paragraph = Paragraph::new(Line::from(spans)).style(Style::default().bg(DARK_BG));
        ratatui::widgets::Widget::render(paragraph, area, buf);
        return;
    }

    let on_burndown = matches!(state.active_tab, UsageTab::Burndown);
    let mut spans = vec![
        Span::styled(" ◀/▶ p", Style::default().fg(GOLD)),
        Span::styled(" provider  ", Style::default().fg(MUTED_GRAY)),
        Span::styled(
            "1 Today  2 7d  3 30d  4 90d  5 YTD  m Month  q Quarter  a All  D advanced  ",
            Style::default().fg(MUTED_GRAY),
        ),
    ];
    if on_burndown {
        // Burndown view: z zoom; Tab pivots panels; Enter/X commit chips; C clears.
        // `BkSp` = Backspace (pop chip / unzoom); Esc is reserved by the host
        // for navigation back to home, so it's listed in the trailing block.
        spans.extend_from_slice(&[
            Span::styled("z", Style::default().fg(GOLD)),
            Span::styled(" zoom  ", Style::default().fg(MUTED_GRAY)),
            Span::styled("Tab", Style::default().fg(GOLD)),
            Span::styled(" focus  ", Style::default().fg(MUTED_GRAY)),
            Span::styled("Enter", Style::default().fg(GOLD)),
            Span::styled(" add  ", Style::default().fg(MUTED_GRAY)),
            Span::styled("X", Style::default().fg(GOLD)),
            Span::styled(" exclude  ", Style::default().fg(MUTED_GRAY)),
            Span::styled("BkSp", Style::default().fg(GOLD)),
            Span::styled(" pop  ", Style::default().fg(MUTED_GRAY)),
            Span::styled("C", Style::default().fg(GOLD)),
            Span::styled(" clear  ", Style::default().fg(MUTED_GRAY)),
        ]);
    } else {
        spans.extend_from_slice(&[
            Span::styled("Tab", Style::default().fg(GOLD)),
            Span::styled(" view  ", Style::default().fg(MUTED_GRAY)),
        ]);
    }
    spans.extend_from_slice(&[
        Span::styled("j/k", Style::default().fg(GOLD)),
        Span::styled(" scroll  ", Style::default().fg(MUTED_GRAY)),
        Span::styled("r/R", Style::default().fg(GOLD)),
        Span::styled(" refresh  ", Style::default().fg(MUTED_GRAY)),
        Span::styled("F", Style::default().fg(GOLD)),
        Span::styled(" flush cache  ", Style::default().fg(MUTED_GRAY)),
        Span::styled("Esc", Style::default().fg(GOLD)),
        Span::styled(" back", Style::default().fg(MUTED_GRAY)),
    ]);
    let paragraph = Paragraph::new(Line::from(spans)).style(Style::default().bg(DARK_BG));
    ratatui::widgets::Widget::render(paragraph, area, buf);
}

fn truncate_string(s: &str, max_len: usize) -> String {
    crate::ui_helpers::truncate_with_ellipsis(s, max_len).into_owned()
}

fn provider_filter_label(filter: UsageProviderFilter) -> &'static str {
    match filter {
        UsageProviderFilter::All => "All",
        UsageProviderFilter::Claude => "Claude",
        UsageProviderFilter::Codex => "Codex",
        UsageProviderFilter::Cursor => "Cursor",
        UsageProviderFilter::Copilot => "Copilot",
        UsageProviderFilter::Gemini => "Gemini",
    }
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
    use crate::data::usage::{
        ActivityCategory, ActivityUsage, ModelUsage, ProjectUsage, SessionUsage, TokenBucket,
    };
    use chrono::Utc;

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
        let now = Utc::now();
        UsageData {
            daily: vec![],
            weekly: vec![],
            projects: vec![
                ProjectUsage {
                    name: "alpha".into(),
                    path: "/work/alpha".into(),
                    bucket: bucket(3),
                    repo: None,
                },
                ProjectUsage {
                    name: "beta".into(),
                    path: "/work/beta".into(),
                    bucket: bucket(2),
                    repo: None,
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
            branches: vec![],
            model_project_counts: std::collections::HashMap::new(),
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
        state.data = Some(std::sync::Arc::new(fixture()));
        // Fixture uses pre-aggregated projects/sessions with `calls: vec![]`,
        // so re-aggregation via the period filter would zero out the rows.
        // Bypass period filtering — this test is about Enter→chip dispatch.
        state.period = UsagePeriod::All;
        state.focused_panel = Some(UsagePanel::ByProject);
        state.focus_row = 1; // beta
        assert!(state.commit_focused_row());
        assert_eq!(state.filters.project, vec!["beta".to_string()]);
    }

    #[test]
    fn enter_on_top_session_row_sets_session_filter() {
        let mut state = UsageViewState::default();
        state.data = Some(std::sync::Arc::new(fixture()));
        state.period = UsagePeriod::All;
        state.focused_panel = Some(UsagePanel::TopSessions);
        state.focus_row = 0;
        assert!(state.commit_focused_row());
        assert_eq!(state.filters.session, vec!["sess-A".to_string()]);
    }

    #[test]
    fn cross_project_session_id_collision_attaches_owning_project_chip() {
        // Two sessions with the same id "s1" but different owning
        // projects — exactly the case that prompted carrying the
        // session row's project on commit_focused_row. The first
        // commit must attach the alpha project chip; a subsequent
        // pop+commit on a beta-owned row must attach beta.
        let now = Utc::now();
        let session_alpha = SessionUsage {
            provider: "claude".into(),
            project: "alpha".into(),
            session_id: "s1".into(),
            first_timestamp: now,
            last_timestamp: now,
            bucket: bucket(3),
        };
        let session_beta = SessionUsage {
            provider: "claude".into(),
            project: "beta".into(),
            session_id: "s1".into(),
            first_timestamp: now,
            last_timestamp: now,
            bucket: bucket(2),
        };
        let mut data = fixture();
        data.sessions = vec![session_alpha, session_beta];

        let mut state = UsageViewState::default();
        state.data = Some(std::sync::Arc::new(data));
        state.period = UsagePeriod::All;
        state.focused_panel = Some(UsagePanel::TopSessions);
        state.focus_row = 0;
        assert!(state.commit_focused_row());
        assert_eq!(state.filters.session, vec!["s1".to_string()]);
        assert_eq!(state.filters.project, vec!["alpha".to_string()]);

        // Pop both chips and target the beta row.
        state.filters.clear();
        state.focus_row = 1;
        assert!(state.commit_focused_row());
        assert_eq!(state.filters.session, vec!["s1".to_string()]);
        assert_eq!(state.filters.project, vec!["beta".to_string()]);
    }

    #[test]
    fn enter_on_by_model_row_sets_model_filter() {
        let mut state = UsageViewState::default();
        state.data = Some(std::sync::Arc::new(fixture()));
        state.period = UsagePeriod::All;
        state.focused_panel = Some(UsagePanel::ByModel);
        state.focus_row = 0;
        assert!(state.commit_focused_row());
        assert_eq!(state.filters.model, vec!["claude-opus-4".to_string()]);
    }

    #[test]
    fn enter_on_by_activity_row_sets_activity_filter() {
        let mut state = UsageViewState::default();
        state.data = Some(std::sync::Arc::new(fixture()));
        state.period = UsagePeriod::All;
        state.focused_panel = Some(UsagePanel::ByActivity);
        state.focus_row = 1; // Conversation
        assert!(state.commit_focused_row());
        assert_eq!(state.filters.activity, vec!["Conversation".to_string()]);
    }

    #[test]
    fn enter_on_daily_activity_row_is_noop() {
        // Brief: read-only panels — Enter is a no-op.
        let mut state = UsageViewState::default();
        state.data = Some(std::sync::Arc::new(fixture()));
        state.focused_panel = Some(UsagePanel::DailyActivity);
        state.focus_row = 0;
        assert!(!state.commit_focused_row());
        assert!(state.filters.is_empty());
    }

    /// Tab traversal contract: ByBranch sits immediately after ByProject
    /// in the focusable-panel sequence so the row 0 "where my tokens
    /// went" cluster (Daily Activity → By Project → By Branch) reads
    /// left-to-right when users walk the dashboard with Tab.
    #[test]
    fn branch_panel_in_panel_all_after_by_project() {
        let proj_idx = UsagePanel::ALL
            .iter()
            .position(|p| *p == UsagePanel::ByProject)
            .expect("ByProject in ALL");
        assert_eq!(
            UsagePanel::ALL[proj_idx + 1],
            UsagePanel::ByBranch,
            "ByBranch must follow ByProject in the traversal order"
        );
    }

    #[test]
    fn tab_traversal_visits_branch_panel_after_project() {
        let mut state = UsageViewState::default();
        state.focused_panel = Some(UsagePanel::ByProject);
        state.focus_next_panel();
        assert_eq!(state.focused_panel, Some(UsagePanel::ByBranch));
    }

    /// Grafana-style cross-filter: Enter on a By Branch row commits the
    /// branch as a chip and propagates the filter to every other widget.
    #[test]
    fn enter_on_by_branch_row_sets_branch_filter() {
        let mut data = fixture();
        data.branches = vec![
            BranchUsage {
                branch: "main".to_string(),
                bucket: bucket(3),
            },
            BranchUsage {
                branch: "feat/burndown".to_string(),
                bucket: bucket(1),
            },
        ];
        let mut state = UsageViewState::default();
        state.data = Some(std::sync::Arc::new(data));
        state.focused_panel = Some(UsagePanel::ByBranch);
        state.focus_row = 0;
        // Bypass period filtering — this test is about Enter→chip dispatch.
        state.period = UsagePeriod::All;
        assert!(state.commit_focused_row(), "Enter on a branch row must commit a chip");
        assert_eq!(state.filters.branch, vec!["main".to_string()]);
    }

    /// Mirror of the include path: X on a By Branch row commits the
    /// branch into the exclude_branch list.
    #[test]
    fn exclude_on_by_branch_row_sets_exclude_branch_filter() {
        let mut data = fixture();
        data.branches = vec![BranchUsage {
            branch: "main".to_string(),
            bucket: bucket(3),
        }];
        let mut state = UsageViewState::default();
        state.data = Some(std::sync::Arc::new(data));
        state.focused_panel = Some(UsagePanel::ByBranch);
        state.focus_row = 0;
        state.period = UsagePeriod::All;
        assert!(state.commit_focused_row_exclude(), "X on a branch row must commit an exclude chip");
        assert_eq!(state.filters.exclude_branch, vec!["main".to_string()]);
    }

    /// Smoke test: render_branch_panel must not panic on a small frame
    /// with a couple of synthetic BranchUsage rows, and must produce a
    /// non-empty buffer (i.e. it actually drew something).
    #[test]
    fn render_branch_panel_smoke() {
        use ratatui::{Terminal, backend::TestBackend};

        let rows = vec![
            BranchUsage {
                branch: "main".to_string(),
                bucket: bucket(3),
            },
            BranchUsage {
                branch: "feat/burndown-branches-panel".to_string(),
                bucket: bucket(2),
            },
        ];

        let backend = TestBackend::new(60, 8);
        let mut terminal = Terminal::new(backend).expect("test backend");
        terminal
            .draw(|frame| {
                let area = frame.size();
                render_branch_panel(frame.buffer_mut(), area, &rows, FocusCtx::unfocused());
            })
            .expect("render_branch_panel must not panic");

        let buffer = terminal.backend().buffer();
        let flat: String = (0..buffer.area().height)
            .flat_map(|y| (0..buffer.area().width).map(move |x| (x, y)))
            .map(|(x, y)| buffer.get(x, y).symbol().to_string())
            .collect();
        assert!(flat.contains("By Branch"), "panel title missing: {flat}");
        assert!(flat.contains("main"), "branch label missing: {flat}");
    }

    #[test]
    fn enter_on_leaderboard_maps_to_project_filter() {
        let mut state = UsageViewState::default();
        state.data = Some(std::sync::Arc::new(fixture()));
        state.period = UsagePeriod::All;
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
        state.filters.branch.push("main".into());

        // First pop -> branch (rightmost in the strip; pop order is
        // branch → session → activity → model → project).
        let removed = state.pop_filter_chip();
        assert!(matches!(removed, Some(UsageFilterChip::Branch(v)) if v == "main"));
        assert!(state.filters.branch.is_empty());

        // The chip strip must show every surviving chip — including the
        // branch chip while it was set. Render symmetry: the chip the
        // user sees rightmost is the one Esc removes next.
        let line = build_filter_chip_line(&state);
        let flat: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(flat.contains("project=alpha"), "got {flat}");
        assert!(flat.contains("model=opus"), "got {flat}");
        assert!(!flat.contains("branch="), "branch chip lingered: {flat}");

        // Second pop -> model.
        let removed = state.pop_filter_chip();
        assert!(matches!(removed, Some(UsageFilterChip::Model(v)) if v == "opus"));
        let line = build_filter_chip_line(&state);
        let flat: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(flat.contains("project=alpha"), "got {flat}");
        assert!(flat.contains("Esc") && flat.contains("C"), "got {flat}");

        // Third pop -> project. With no chips left we fall back to
        // the discoverability hint.
        assert!(state.pop_filter_chip().is_some());
        assert!(state.filters.is_empty());
        let line = build_filter_chip_line(&state);
        let flat: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(flat.contains("(none)"), "got {flat}");
    }

    /// Tripwire: every `UsageFilterChip` variant must render in
    /// `build_filter_chip_line`. If a new variant is added without a
    /// `push_chip_group` call this test fails — the PR-D regression where
    /// `branch` chips set via `--branch` were invisible but still consumed
    /// `Esc` is exactly what this guards against.
    #[test]
    fn every_filter_chip_variant_renders_in_strip() {
        let mut state = UsageViewState::default();
        state.filters.project.push("p".into());
        state.filters.model.push("m".into());
        state.filters.activity.push("Coding".into());
        state.filters.session.push("s".into());
        state.filters.branch.push("b".into());

        let line = build_filter_chip_line(&state);
        let flat: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        for needle in [
            "project=p",
            "model=m",
            "activity=Coding",
            "session=s",
            "branch=b",
        ] {
            assert!(flat.contains(needle), "chip strip missing {needle}: {flat}");
        }
    }

    /// The `↻ updated` badge renders only when the plugin's render
    /// snapshot has flagged this frame as a fresh-pivot frame.
    #[test]
    fn chip_strip_shows_fresh_pivot_badge_when_flag_set() {
        let mut state = UsageViewState::default();
        state.filters.project.push("p".into());

        state.fresh_pivot = false;
        let line = build_filter_chip_line(&state);
        let flat: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(!flat.contains("↻ updated"), "badge must hide when flag is off: {flat}");

        state.fresh_pivot = true;
        let line = build_filter_chip_line(&state);
        let flat: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(flat.contains("↻ updated"), "badge must show when flag is on: {flat}");
    }

    /// Same indicator visibility applies on the no-chip path so the
    /// user gets confirmation even when they clear chips back to
    /// `(none)` — that's still a pivot that recomputed.
    #[test]
    fn chip_strip_shows_fresh_pivot_badge_with_no_chips() {
        let mut state = UsageViewState::default();
        state.fresh_pivot = true;
        let line = build_filter_chip_line(&state);
        let flat: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(flat.contains("(none)"), "got {flat}");
        assert!(flat.contains("↻ updated"), "got {flat}");
    }

    #[test]
    fn x_on_by_project_row_adds_exclude_chip_and_filter_drops_project() {
        use crate::data::usage::{UsageData, UsageFilters, filter_usage_data};
        use crate::test_support::ProviderCallBuilder;
        use std::collections::HashMap;

        let mut state = UsageViewState::default();
        state.data = Some(std::sync::Arc::new(fixture()));
        state.period = UsagePeriod::All;
        state.focused_panel = Some(UsagePanel::ByProject);
        state.focus_row = 1; // beta
        assert!(state.commit_focused_row_exclude());
        assert_eq!(state.filters.exclude_project, vec!["beta".to_string()]);
        assert!(
            state.filters.project.is_empty(),
            "include side must be untouched"
        );

        // End-to-end: filter_usage_data must drop calls matching the
        // exclude project, leaving only the alpha call.
        let calls = vec![
            ProviderCallBuilder::new().with_project("alpha").build(),
            ProviderCallBuilder::new().with_project("beta").build(),
        ];
        let data = UsageData {
            daily: vec![],
            weekly: vec![],
            projects: vec![],
            grand_total: TokenBucket::default(),
            calls,
            sessions: vec![],
            models: vec![],
            activities: vec![],
            tools: vec![],
            mcp_servers: vec![],
            shell_commands: vec![],
            branches: vec![],
            model_project_counts: HashMap::new(),
        };
        let mut filters = UsageFilters::default();
        filters.exclude_project.push("beta".into());
        let filtered = filter_usage_data(&data, &filters);
        assert_eq!(filtered.calls.len(), 1);
        assert_eq!(filtered.calls[0].project, "alpha");
    }

    #[test]
    fn step_period_back_uses_oldest_call_day_not_data_daily() {
        // The picker must clamp at the absolute oldest call day (which
        // tracks the unfiltered call set across loads), not at
        // `data.daily.first()` — when the active period is
        // `SpecificMonth(May)`, `data.daily` only has May rows so the
        // old clamp-at-data path would refuse to step into April even
        // though April is in range.
        use crate::data::usage::UsagePeriod;
        let mut state = UsageViewState::default();
        state.oldest_call_day = NaiveDate::from_ymd_opt(2026, 4, 1);
        state.period = UsagePeriod::SpecificMonth(NaiveDate::from_ymd_opt(2026, 5, 1).unwrap());

        assert!(
            state.step_period_back(),
            "April is in range; back must succeed"
        );
        assert_eq!(
            state.period,
            UsagePeriod::SpecificMonth(NaiveDate::from_ymd_opt(2026, 4, 1).unwrap()),
        );
    }

    #[test]
    fn step_period_back_clamps_when_oldest_is_inside_target_month() {
        // April 1 < May 15, so stepping back from May lands on April 1,
        // which is BEFORE the oldest known day — must refuse the step.
        use crate::data::usage::UsagePeriod;
        let mut state = UsageViewState::default();
        state.oldest_call_day = NaiveDate::from_ymd_opt(2026, 5, 15);
        state.period = UsagePeriod::SpecificMonth(NaiveDate::from_ymd_opt(2026, 5, 1).unwrap());

        assert!(
            !state.step_period_back(),
            "April predates oldest May 15; back must fail"
        );
        assert_eq!(
            state.period,
            UsagePeriod::SpecificMonth(NaiveDate::from_ymd_opt(2026, 5, 1).unwrap()),
        );
    }

    #[test]
    fn step_period_back_returns_false_when_no_data_loaded() {
        // Without an oldest anchor the user could wander arbitrarily
        // far back; refuse to step until at least one load has populated
        // `oldest_call_day`.
        use crate::data::usage::UsagePeriod;
        let mut state = UsageViewState::default();
        state.oldest_call_day = None;
        state.period = UsagePeriod::SpecificMonth(NaiveDate::from_ymd_opt(2026, 5, 1).unwrap());
        assert!(!state.step_period_back());
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
    use crate::data::usage::{
        ActivityCategory, ActivityUsage, ModelUsage, ProjectUsage, ProviderCall, SessionUsage,
        TokenBucket, UsageData, UsageFilters, filter_usage_data,
    };
    use chrono::Utc;

    fn call(project: &str, model: &str, session: &str) -> ProviderCall {
        crate::test_support::ProviderCallBuilder::new()
            .with_model(model)
            .with_session(session)
            .with_project(project)
            .with_project_path(format!("/work/{project}"))
            .with_timestamp(Utc::now())
            .with_input_tokens(100)
            .with_output_tokens(50)
            .with_cost(1.0)
            .with_tools(&["Edit"])
            .with_user_message("tidy")
            .build()
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

#[cfg(test)]
mod budget_live_header_tests {
    use super::*;
    use crate::live_window::{LiveWindow, Source};
    use std::time::Duration;

    fn flat(lines: &[Line<'_>]) -> String {
        lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref().to_string()))
            .collect::<Vec<_>>()
            .join(" | ")
    }

    #[test]
    fn cta_lines_rendered_when_source_is_none() {
        let live = LiveWindow::empty();
        let lines = budget_live_header_lines(&live, 60);
        let text = flat(&lines);
        assert!(text.contains("[W]"), "CTA must mention W key: {text}");
        assert!(text.contains("statusline"), "CTA copy missing: {text}");
    }

    #[test]
    fn tier1_renders_two_bars_plus_cost_and_reset() {
        let live = LiveWindow {
            five_hour_pct: Some(40),
            seven_day_pct: Some(8),
            today_cost_usd: Some(3.21),
            resets_in: Some(Duration::from_secs(2 * 3600 + 30 * 60)),
            context_pct: None,
            model: None,
            source: Source::Tier1Cache,
        };
        let lines = budget_live_header_lines(&live, 60);
        let text = flat(&lines);
        assert!(text.contains("5h burn"));
        assert!(text.contains("7d wnd"));
        assert!(text.contains("$3.21"));
        assert!(text.contains("2h 30m"));
    }

    #[test]
    fn tier2_renders_5h_bar_and_upgrade_hint_only() {
        let live = LiveWindow {
            five_hour_pct: Some(20),
            seven_day_pct: None,
            today_cost_usd: None,
            resets_in: None,
            context_pct: None,
            model: None,
            source: Source::Tier2Local,
        };
        let lines = budget_live_header_lines(&live, 60);
        let text = flat(&lines);
        assert!(text.contains("5h burn"));
        assert!(!text.contains("7d wnd"));
        assert!(text.contains("Wire statusline"));
    }

    #[test]
    fn format_hms_zero_yields_zero_minutes() {
        assert_eq!(format_hms(Duration::ZERO), "0m");
    }

    #[test]
    fn format_hms_under_one_hour_omits_h_field() {
        assert_eq!(format_hms(Duration::from_secs(45 * 60)), "45m");
    }

    #[test]
    fn format_hms_pads_minutes_when_hours_present() {
        assert_eq!(format_hms(Duration::from_secs(3 * 3600 + 5 * 60)), "3h 05m");
    }
}

#[cfg(test)]
mod scan_progress_tests {
    //! Assert the per-format headline string the skeleton renders, plus
    //! the gating predicate that swaps the legacy ⏳ spinner for the
    //! progress skeleton when `data` is empty and `scan_progress` is
    //! set. Verifies plan §Phase 6 test gate "1/3 → 2/3 → 3/3" at the
    //! formatter level.
    use super::*;
    use ainb_plugin_types_sessions::ScanProgressEvent;

    fn progress(scanned: u32, total: u32, project: &str) -> ScanProgressEvent {
        ScanProgressEvent {
            scanned,
            total,
            current_project: project.into(),
        }
    }

    #[test]
    fn headline_renders_scanned_over_total_when_total_known() {
        assert_eq!(
            scan_progress_headline(&progress(1, 3, "alpha")),
            "Scanning sessions: 1/3"
        );
        assert_eq!(
            scan_progress_headline(&progress(2, 3, "beta")),
            "Scanning sessions: 2/3"
        );
        assert_eq!(
            scan_progress_headline(&progress(3, 3, "gamma")),
            "Scanning sessions: 3/3"
        );
    }

    #[test]
    fn headline_omits_total_when_total_is_zero() {
        // Plan §Phase 6 risks: "Progress totals unknown until directory
        // walk completes — emit total=0 until then; UI renders as
        // 'Scanning sessions… N files'".
        assert_eq!(
            scan_progress_headline(&progress(47, 0, "unknown")),
            "Scanning sessions… 47 files"
        );
    }

    #[test]
    fn render_skeleton_writes_ratatui_buffer() {
        // End-to-end: feed the renderer a ScanProgressEvent and
        // confirm the painted buffer contains the headline + project
        // label. Catches regressions where the layout/style change
        // accidentally drops the text.
        let area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 3,
        };
        let mut buf = Buffer::empty(area);
        render_scan_progress(&mut buf, area, &progress(2, 3, "alpha"));
        let painted: String = (0..area.width)
            .map(|x| buf.get(x, 1).symbol().to_string())
            .collect();
        assert!(
            painted.contains("Scanning sessions: 2/3"),
            "skeleton headline rendered: {painted:?}"
        );
        assert!(
            painted.contains("alpha"),
            "current_project label rendered: {painted:?}"
        );
    }

    #[test]
    fn render_skeleton_omits_dot_when_project_empty() {
        let area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 3,
        };
        let mut buf = Buffer::empty(area);
        render_scan_progress(&mut buf, area, &progress(1, 0, ""));
        let painted: String = (0..area.width)
            .map(|x| buf.get(x, 1).symbol().to_string())
            .collect();
        assert!(painted.contains("1 files"));
        assert!(!painted.contains(" · "), "no separator when project empty: {painted:?}");
    }

    #[test]
    fn render_skeleton_paints_gauge_below_headline_when_total_known() {
        // height=4 leaves an inner area of 2 rows after the rounded
        // block — one for the headline, one for the gauge.
        let area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 4,
        };
        let mut buf = Buffer::empty(area);
        render_scan_progress(&mut buf, area, &progress(50, 100, "alpha"));

        // Row 1 = headline. Row 2 = gauge.
        let headline_row: String = (0..area.width)
            .map(|x| buf.get(x, 1).symbol().to_string())
            .collect();
        let gauge_row: String = (0..area.width)
            .map(|x| buf.get(x, 2).symbol().to_string())
            .collect();
        assert!(
            headline_row.contains("Scanning sessions: 50/100"),
            "headline rendered on row 1: {headline_row:?}"
        );
        assert!(
            gauge_row.contains("50%") || gauge_row.contains("(50/100)"),
            "gauge row contains the percent/ratio label: {gauge_row:?}"
        );
    }

    #[test]
    fn render_skeleton_falls_back_to_single_line_when_total_unknown() {
        // total=0 → never render the gauge, even with a tall area.
        // Without a denominator there's no ratio to draw.
        let area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 6,
        };
        let mut buf = Buffer::empty(area);
        render_scan_progress(&mut buf, area, &progress(17, 0, "alpha"));

        // Row 1 should have the headline. Row 2 should be empty (no
        // gauge), filled with spaces / default cells.
        let row2: String = (0..area.width)
            .map(|x| buf.get(x, 2).symbol().to_string())
            .collect();
        assert!(
            !row2.contains('%'),
            "no gauge row when total=0: row2 = {row2:?}"
        );
    }

    #[test]
    fn render_skeleton_clamps_overshoot_ratio() {
        // If `scanned` somehow exceeds `total` (e.g. files added
        // mid-scan after the pre-walk), the gauge must clamp at 100%
        // rather than over-fill the bar or print "120%".
        let area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 4,
        };
        let mut buf = Buffer::empty(area);
        render_scan_progress(&mut buf, area, &progress(120, 100, "alpha"));
        let gauge_row: String = (0..area.width)
            .map(|x| buf.get(x, 2).symbol().to_string())
            .collect();
        assert!(
            gauge_row.contains("100%"),
            "overshoot clamps to 100%: {gauge_row:?}"
        );
        assert!(
            !gauge_row.contains("120%"),
            "no over-100% label: {gauge_row:?}"
        );
    }
}
