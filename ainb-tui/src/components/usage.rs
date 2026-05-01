// ABOUTME: Usage analytics screen showing token consumption by day, week, and project.
// Accessible via 'i' key from home screen or Stats sidebar item.

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Row, Table, Tabs},
};

use crate::models::{
    ActivityUsage, ModelUsage, NamedUsage, ProjectUsage, SessionUsage, UsageData, UsagePeriod,
    UsageProviderFilter, UsageQuery, format_tokens_short, optimize_usage,
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

    render_help_bar(frame, layout[4]);
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

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
            Constraint::Length(3),
            Constraint::Min(0),
        ])
        .split(inner);

    render_burndown_header(frame, vertical[0], data);
    render_period_row(frame, vertical[1], state);

    if vertical[2].width >= 120 && vertical[2].height >= 24 {
        render_dashboard_grid(frame, vertical[2], data);
    } else if vertical[2].width >= 96 {
        render_dashboard_compact(frame, vertical[2], data);
    } else {
        render_dashboard_stack(frame, vertical[2], data);
    }
}

fn render_dashboard_grid(frame: &mut Frame, area: Rect, data: &UsageData) {
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
    render_daily_activity_panel(frame, top[0], data);
    render_project_panel(frame, top[1], &data.projects);
    render_live_panel(frame, top[2], &data.sessions);

    let middle = three_columns(rows[1]);
    render_session_panel(frame, middle[0], &data.sessions);
    render_activity_panel(frame, middle[1], &data.activities);
    render_model_panel(frame, middle[2], &data.models);

    let lower = three_columns(rows[2]);
    render_named_panel(frame, lower[0], "Core Tools", &data.tools);
    render_named_panel(frame, lower[1], "Shell Commands", &data.shell_commands);
    render_named_panel(frame, lower[2], "MCP Servers", &data.mcp_servers);

    let bottom = three_columns(rows[3]);
    render_optimize_compact_panel(frame, bottom[0], data);
    render_leaderboard_panel(frame, bottom[1], data);
    render_budget_panel(frame, bottom[2], data);
}

fn render_dashboard_compact(frame: &mut Frame, area: Rect, data: &UsageData) {
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

    render_daily_activity_panel(frame, left[0], data);
    render_project_panel(frame, left[1], &data.projects);
    render_session_panel(frame, left[2], &data.sessions);
    render_live_panel(frame, left[3], &data.sessions);

    render_activity_panel(frame, right[0], &data.activities);
    render_model_panel(frame, right[1], &data.models);
    render_optimize_compact_panel(frame, right[2], data);
    let tools = three_columns(right[3]);
    render_named_panel(frame, tools[0], "Core Tools", &data.tools);
    render_named_panel(frame, tools[1], "Shell Commands", &data.shell_commands);
    render_named_panel(frame, tools[2], "MCP Servers", &data.mcp_servers);
}

fn render_dashboard_stack(frame: &mut Frame, area: Rect, data: &UsageData) {
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

    render_daily_activity_panel(frame, chunks[0], data);
    render_project_panel(frame, chunks[1], &data.projects);
    render_session_panel(frame, chunks[2], &data.sessions);
    render_activity_panel(frame, chunks[3], &data.activities);
    render_model_panel(frame, chunks[4], &data.models);
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

fn render_burndown_header(frame: &mut Frame, area: Rect, data: &UsageData) {
    let cost = format_cost(data.grand_total.cost_usd);
    let cache_hit = cache_hit_percent(data);
    let projected = projected_month_cost(data);
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

fn render_period_row(frame: &mut Frame, area: Rect, state: &UsageViewState) {
    let text = Line::from(vec![
        Span::styled(
            "[1 Today]  [2 7d]  [3 30d]  [4 Month]  [5 All]  [d Custom]  [/] filter  [x] exclude  [r] reload",
            Style::default().fg(MUTED_GRAY),
        ),
        Span::styled("   Active: ", Style::default().fg(MUTED_GRAY)),
        Span::styled(
            period_label(&state.period),
            Style::default().fg(TERMINAL_ACCENT).add_modifier(Modifier::BOLD),
        ),
    ]);
    frame.render_widget(Paragraph::new(text), area);
}

fn render_daily_activity_panel(frame: &mut Frame, area: Rect, data: &UsageData) {
    let max = data
        .daily
        .iter()
        .filter_map(|(_, bucket)| bucket.cost_usd)
        .fold(0.0_f64, f64::max)
        .max(1.0);
    let rows: Vec<_> = data
        .daily
        .iter()
        .rev()
        .take(area.height.saturating_sub(2) as usize)
        .map(|(date, bucket)| {
            let cost = bucket.cost_usd.unwrap_or(0.0);
            format!(
                "{} {} {:>7} {} calls",
                date.format("%m-%d"),
                ratio_bar(cost, max, 18),
                format_cost(bucket.cost_usd),
                bucket.call_count
            )
        })
        .collect();
    render_panel(frame, area, "Daily Activity", rows);
}

fn render_project_panel(frame: &mut Frame, area: Rect, rows: &[ProjectUsage]) {
    let max = rows.iter().map(|row| row.bucket.total()).max().unwrap_or(1);
    let lines = rows
        .iter()
        .take(area.height.saturating_sub(2) as usize)
        .map(|row| {
            format!(
                "{} {} {}",
                truncate_string(&row.name, 22),
                bar(row.bucket.total(), max, 12),
                format_cost(row.bucket.cost_usd)
            )
        })
        .collect();
    render_panel(frame, area, "By Project", lines);
}

fn render_session_panel(frame: &mut Frame, area: Rect, rows: &[SessionUsage]) {
    let max = rows.iter().map(|row| row.bucket.total()).max().unwrap_or(1);
    let lines = rows
        .iter()
        .take(area.height.saturating_sub(2) as usize)
        .map(|row| {
            format!(
                "{} {} {}",
                truncate_string(&row.project, 18),
                bar(row.bucket.total(), max, 12),
                format_tokens_short(row.bucket.total())
            )
        })
        .collect();
    render_panel(frame, area, "Top Sessions", lines);
}

fn render_live_panel(frame: &mut Frame, area: Rect, rows: &[SessionUsage]) {
    let lines = rows
        .iter()
        .take(area.height.saturating_sub(2) as usize)
        .map(|row| {
            format!(
                "● {} · {} · {}",
                truncate_string(&row.project, 18),
                truncate_string(&row.provider, 6),
                format_cost(row.bucket.cost_usd)
            )
        })
        .collect();
    render_panel(frame, area, "Live Session Ticker", lines);
}

fn render_activity_panel(frame: &mut Frame, area: Rect, rows: &[ActivityUsage]) {
    let max = rows.iter().map(|row| row.bucket.total()).max().unwrap_or(1);
    let lines = rows
        .iter()
        .take(area.height.saturating_sub(2) as usize)
        .map(|row| {
            format!(
                "{} {} {}t {}r",
                row.category.label(),
                bar(row.bucket.total(), max, 10),
                row.turns,
                row.retries
            )
        })
        .collect();
    render_panel(frame, area, "By Activity", lines);
}

fn render_model_panel(frame: &mut Frame, area: Rect, rows: &[ModelUsage]) {
    let max = rows.iter().map(|row| row.bucket.total()).max().unwrap_or(1);
    let lines = rows
        .iter()
        .take(area.height.saturating_sub(2) as usize)
        .map(|row| {
            format!(
                "{} {} {}",
                truncate_string(&row.model, 18),
                bar(row.bucket.total(), max, 10),
                format_tokens_short(row.bucket.total())
            )
        })
        .collect();
    render_panel(frame, area, "By Model", lines);
}

fn render_named_panel(frame: &mut Frame, area: Rect, title: &str, rows: &[NamedUsage]) {
    let max = rows.iter().map(|row| row.calls as u64).max().unwrap_or(1);
    let lines = rows
        .iter()
        .take(area.height.saturating_sub(2) as usize)
        .map(|row| {
            format!(
                "{} {} {}",
                truncate_string(&row.name, 14),
                bar(row.calls as u64, max, 8),
                row.calls
            )
        })
        .collect();
    render_panel(frame, area, title, lines);
}

fn render_optimize_compact_panel(frame: &mut Frame, area: Rect, data: &UsageData) {
    let result = optimize_usage(data);
    let mut lines = vec![format!(
        "Health {:?} · score {} · save {} tokens",
        result.grade,
        result.score,
        format_tokens_short(result.potential_tokens_saved)
    )];
    for finding in result.findings.iter().take(area.height.saturating_sub(3) as usize) {
        lines.push(format!(
            "{:?} {}",
            finding.impact,
            truncate_string(&finding.title, area.width.saturating_sub(14) as usize)
        ));
    }
    render_panel(frame, area, "Optimization Recommendations", lines);
}

fn render_leaderboard_panel(frame: &mut Frame, area: Rect, data: &UsageData) {
    let max = data.projects.iter().map(|row| row.bucket.total()).max().unwrap_or(1);
    let lines = data
        .projects
        .iter()
        .enumerate()
        .take(area.height.saturating_sub(2) as usize)
        .map(|(idx, project)| {
            format!(
                "{} {} {} {}",
                idx + 1,
                truncate_string(&project.name, 16),
                bar(project.bucket.total(), max, 8),
                format_cost(project.bucket.cost_usd)
            )
        })
        .collect();
    render_panel(frame, area, "Agent Leaderboard", lines);
}

fn render_budget_panel(frame: &mut Frame, area: Rect, data: &UsageData) {
    let spent = data.grand_total.cost_usd.unwrap_or(0.0);
    let projected = projected_month_cost(data).unwrap_or(spent);
    let cap = projected.max(spent).max(1.0) * 1.25;
    let usage = ((projected / cap) * 100.0).min(100.0);
    let lines = vec![
        format!(
            "Monthly cap {} / {}",
            format_cost(Some(projected)),
            format_cost(Some(cap))
        ),
        format!("{} {:>4.1}% projected", ratio_bar(usage, 100.0, 24), usage),
        format!(
            "Plan utilization {}",
            format_cost(data.grand_total.cost_usd)
        ),
        format!(
            "{} cache hit · {} days sampled",
            format!("{:.1}%", cache_hit_percent(data)),
            data.daily.len()
        ),
    ];
    render_panel(frame, area, "Budget · Alerts", lines);
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
    let block = Block::default()
        .title(format!(" [ {title} ] "))
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(TERMINAL_BORDER))
        .style(Style::default().bg(TERMINAL_PANEL));
    let lines = if rows.is_empty() {
        vec![Line::from(Span::styled(
            "No data",
            Style::default().fg(MUTED_GRAY),
        ))]
    } else {
        rows.into_iter()
            .map(|row| Line::from(Span::styled(row, Style::default().fg(SOFT_WHITE))))
            .collect()
    };
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn bar(value: u64, max: u64, width: usize) -> String {
    let filled = if max == 0 {
        0
    } else {
        ((value as f64 / max as f64) * width as f64).round() as usize
    }
    .min(width);
    format!(
        "{}{}",
        "█".repeat(filled),
        "░".repeat(width.saturating_sub(filled))
    )
}

fn ratio_bar(value: f64, max: f64, width: usize) -> String {
    let filled = if max <= 0.0 {
        0
    } else {
        ((value / max) * width as f64).round() as usize
    }
    .min(width);
    format!(
        "{}{}",
        "▓".repeat(filled),
        "░".repeat(width.saturating_sub(filled))
    )
}

fn cache_hit_percent(data: &UsageData) -> f64 {
    let cached = data.grand_total.cache_creation_tokens + data.grand_total.cache_read_tokens;
    let denominator = data.grand_total.input_tokens + cached;
    if denominator == 0 {
        0.0
    } else {
        cached as f64 * 100.0 / denominator as f64
    }
}

fn projected_month_cost(data: &UsageData) -> Option<f64> {
    let spent = data.grand_total.cost_usd?;
    if data.daily.is_empty() {
        return Some(spent);
    }
    Some(spent / data.daily.len() as f64 * 30.0)
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

fn render_help_bar(frame: &mut Frame, area: Rect) {
    let spans = vec![
        Span::styled(" ◀/▶", Style::default().fg(GOLD)),
        Span::styled(" provider  ", Style::default().fg(MUTED_GRAY)),
        Span::styled("p", Style::default().fg(GOLD)),
        Span::styled(" filter  ", Style::default().fg(MUTED_GRAY)),
        Span::styled("1-5", Style::default().fg(GOLD)),
        Span::styled(" period  ", Style::default().fg(MUTED_GRAY)),
        Span::styled("/ x d c", Style::default().fg(GOLD)),
        Span::styled(" filters  ", Style::default().fg(MUTED_GRAY)),
        Span::styled("Tab", Style::default().fg(GOLD)),
        Span::styled(" view  ", Style::default().fg(MUTED_GRAY)),
        Span::styled("j/k", Style::default().fg(GOLD)),
        Span::styled(" scroll  ", Style::default().fg(MUTED_GRAY)),
        Span::styled("g/G", Style::default().fg(GOLD)),
        Span::styled(" top/bottom  ", Style::default().fg(MUTED_GRAY)),
        Span::styled("r", Style::default().fg(GOLD)),
        Span::styled(" refresh  ", Style::default().fg(MUTED_GRAY)),
        Span::styled("Esc", Style::default().fg(GOLD)),
        Span::styled(" back", Style::default().fg(MUTED_GRAY)),
    ];
    let paragraph = Paragraph::new(Line::from(spans)).style(Style::default().bg(DARK_BG));
    frame.render_widget(paragraph, area);
}

fn truncate_string(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}…", &s[..max_len - 1])
    }
}

fn provider_filter_label(filter: UsageProviderFilter) -> &'static str {
    match filter {
        UsageProviderFilter::All => "All",
        UsageProviderFilter::Claude => "Claude",
        UsageProviderFilter::Codex => "Codex",
    }
}

fn period_label(period: &UsagePeriod) -> String {
    match period {
        UsagePeriod::Today => "Today".to_string(),
        UsagePeriod::Week => "Week".to_string(),
        UsagePeriod::ThirtyDays => "30 days".to_string(),
        UsagePeriod::Month => "Month".to_string(),
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
