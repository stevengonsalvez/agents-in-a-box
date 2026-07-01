// ABOUTME: Skills browser screen — shows user-level skills, agents, and the
// associations between them. Mirrors the Usage Analytics layout and async-load
// pattern so provider switches never block the event thread.

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Row, Table, Tabs},
};

use crate::models::skills::{AgentDef, Skill, SkillsData};

// Color palette from TUI style guide (mirrors usage.rs)
const CORNFLOWER_BLUE: Color = Color::Rgb(100, 149, 237);
const GOLD: Color = Color::Rgb(255, 215, 0);
const SELECTION_GREEN: Color = Color::Rgb(100, 200, 100);
const DARK_BG: Color = Color::Rgb(25, 25, 35);
const SOFT_WHITE: Color = Color::Rgb(220, 220, 230);
const MUTED_GRAY: Color = Color::Rgb(120, 120, 140);
const LIST_HIGHLIGHT_BG: Color = Color::Rgb(40, 40, 60);

/// Which agent provider's skills to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SkillsProvider {
    #[default]
    Claude,
    Codex,
    Gemini,
    Copilot,
}

impl SkillsProvider {
    fn all() -> &'static [SkillsProvider] {
        &[
            SkillsProvider::Claude,
            SkillsProvider::Codex,
            SkillsProvider::Gemini,
            SkillsProvider::Copilot,
        ]
    }

    fn label(&self) -> &'static str {
        match self {
            SkillsProvider::Claude => "✻ Claude Code",
            SkillsProvider::Codex => "✦ Codex CLI",
            SkillsProvider::Gemini => "✨ Gemini CLI",
            SkillsProvider::Copilot => "🐙 Copilot",
        }
    }

    pub fn has_data(&self) -> bool {
        matches!(self, SkillsProvider::Claude)
    }

    fn next(&self) -> Self {
        match self {
            SkillsProvider::Claude => SkillsProvider::Codex,
            SkillsProvider::Codex => SkillsProvider::Gemini,
            SkillsProvider::Gemini => SkillsProvider::Copilot,
            SkillsProvider::Copilot => SkillsProvider::Claude,
        }
    }

    fn prev(&self) -> Self {
        match self {
            SkillsProvider::Claude => SkillsProvider::Copilot,
            SkillsProvider::Codex => SkillsProvider::Claude,
            SkillsProvider::Gemini => SkillsProvider::Codex,
            SkillsProvider::Copilot => SkillsProvider::Gemini,
        }
    }
}

/// Which sub-tab is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SkillsTab {
    #[default]
    Skills,
    Agents,
    Associations,
}

impl SkillsTab {
    fn all() -> &'static [SkillsTab] {
        &[
            SkillsTab::Skills,
            SkillsTab::Agents,
            SkillsTab::Associations,
        ]
    }

    fn title(&self) -> &'static str {
        match self {
            SkillsTab::Skills => "Skills",
            SkillsTab::Agents => "Agents",
            SkillsTab::Associations => "Associations",
        }
    }

    fn next(&self) -> Self {
        match self {
            SkillsTab::Skills => SkillsTab::Agents,
            SkillsTab::Agents => SkillsTab::Associations,
            SkillsTab::Associations => SkillsTab::Skills,
        }
    }

    fn prev(&self) -> Self {
        match self {
            SkillsTab::Skills => SkillsTab::Associations,
            SkillsTab::Agents => SkillsTab::Skills,
            SkillsTab::Associations => SkillsTab::Agents,
        }
    }
}

/// View state for the Skills screen.
#[derive(Debug, Clone)]
pub struct SkillsViewState {
    pub provider: SkillsProvider,
    pub active_tab: SkillsTab,
    pub data: Option<SkillsData>,
    pub loading: bool,
    pub selected_index: usize,
    pub search_active: bool,
    pub search_query: String,
}

impl Default for SkillsViewState {
    fn default() -> Self {
        Self {
            provider: SkillsProvider::Claude,
            active_tab: SkillsTab::Skills,
            data: None,
            loading: false,
            selected_index: 0,
            search_active: false,
            search_query: String::new(),
        }
    }
}

impl SkillsViewState {
    // IMPORTANT: do NOT null `data` on provider switch — it stays cached,
    // mirroring the fix in UsageViewState.
    pub fn next_provider(&mut self) {
        self.provider = self.provider.next();
        self.selected_index = 0;
    }

    pub fn prev_provider(&mut self) {
        self.provider = self.provider.prev();
        self.selected_index = 0;
    }

    pub fn next_tab(&mut self) {
        self.active_tab = self.active_tab.next();
        self.selected_index = 0;
    }

    pub fn prev_tab(&mut self) {
        self.active_tab = self.active_tab.prev();
        self.selected_index = 0;
    }

    pub fn scroll_up(&mut self) {
        if self.selected_index > 0 {
            self.selected_index -= 1;
        }
    }

    pub fn scroll_down(&mut self, max_rows: usize) {
        if max_rows == 0 {
            return;
        }
        if self.selected_index + 1 < max_rows {
            self.selected_index += 1;
        }
    }

    pub fn scroll_to_top(&mut self) {
        self.selected_index = 0;
    }

    pub fn scroll_to_bottom(&mut self, max_rows: usize) {
        if max_rows == 0 {
            self.selected_index = 0;
            return;
        }
        self.selected_index = max_rows - 1;
    }

    pub fn page_up(&mut self, page_size: usize) {
        self.selected_index = self.selected_index.saturating_sub(page_size);
    }

    pub fn page_down(&mut self, max_rows: usize, page_size: usize) {
        if max_rows == 0 {
            return;
        }
        self.selected_index = (self.selected_index + page_size).min(max_rows - 1);
    }

    pub fn search_push(&mut self, c: char) {
        self.search_query.push(c);
        self.selected_index = 0;
    }

    pub fn search_pop(&mut self) {
        self.search_query.pop();
        self.selected_index = 0;
    }

    /// Clamp `selected_index` to a shrinking row set (e.g. after a filter
    /// narrows results or the active provider/tab swaps).
    pub fn clamp_selection(&mut self, row_count: usize) {
        if row_count == 0 {
            self.selected_index = 0;
        } else if self.selected_index >= row_count {
            self.selected_index = row_count - 1;
        }
    }

    pub fn row_count(&self) -> usize {
        let Some(data) = &self.data else {
            return 0;
        };
        match self.active_tab {
            SkillsTab::Skills => filtered_skills(data, &self.search_query).len(),
            SkillsTab::Agents => filtered_agents(data, &self.search_query).len(),
            SkillsTab::Associations => association_rows(data, &self.search_query).len(),
        }
    }
}

// ------------- filtering helpers -------------

fn matches_query(name: &str, description: &str, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let q = query.to_lowercase();
    name.to_lowercase().contains(&q) || description.to_lowercase().contains(&q)
}

fn filtered_skills<'a>(data: &'a SkillsData, query: &str) -> Vec<&'a Skill> {
    data.skills
        .iter()
        .filter(|s| matches_query(&s.name, &s.description, query))
        .collect()
}

fn filtered_agents<'a>(data: &'a SkillsData, query: &str) -> Vec<&'a AgentDef> {
    data.agents
        .iter()
        .filter(|a| matches_query(&a.name, &a.description, query))
        .collect()
}

/// Returns (skill_name, agent_names) pairs for skills that have at least one
/// associated agent, filtered by search query.
fn association_rows<'a>(data: &'a SkillsData, query: &str) -> Vec<(&'a Skill, Vec<&'a str>)> {
    let mut out: Vec<(&'a Skill, Vec<&'a str>)> = Vec::new();
    for skill in &data.skills {
        let agents = match data.associations.get(&skill.name) {
            Some(v) if !v.is_empty() => v,
            _ => continue,
        };
        let joined = agents.join(" ");
        if !matches_query(&skill.name, &joined, query) {
            continue;
        }
        let refs: Vec<&str> = agents.iter().map(String::as_str).collect();
        out.push((skill, refs));
    }
    out
}

// ------------- top-level render -------------

pub fn render(frame: &mut Frame, area: Rect, state: &SkillsViewState) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Summary bar
            Constraint::Length(3), // Provider selector
            Constraint::Length(3), // Tab bar
            Constraint::Min(0),    // Content area
            Constraint::Length(2), // Help bar
        ])
        .split(area);

    render_summary_bar(frame, layout[0], state);
    render_provider_bar(frame, layout[1], state);
    render_tab_bar(frame, layout[2], state);

    if !state.provider.has_data() {
        render_no_data(frame, layout[3], state);
    } else if state.loading || state.data.is_none() {
        render_loading(frame, layout[3]);
    } else {
        render_content(frame, layout[3], state);
    }

    render_help_bar(frame, layout[4], state);
}

fn render_summary_bar(frame: &mut Frame, area: Rect, state: &SkillsViewState) {
    let mut spans = vec![
        Span::styled("🧠 ", Style::default().fg(GOLD)),
        Span::styled(
            "Skills Catalogue",
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ),
    ];

    if let Some(data) = &state.data {
        let linked = data.associations.values().filter(|v| !v.is_empty()).count();
        spans.extend_from_slice(&[
            Span::styled("  │  ", Style::default().fg(MUTED_GRAY)),
            Span::styled(
                format!("{}", data.skills.len()),
                Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" skills  ", Style::default().fg(MUTED_GRAY)),
            Span::styled(
                format!("{}", data.agents.len()),
                Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" agents  ", Style::default().fg(MUTED_GRAY)),
            Span::styled(
                format!("{}", linked),
                Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" associations", Style::default().fg(MUTED_GRAY)),
        ]);
    } else if state.provider.has_data() {
        spans.push(Span::styled(
            "  │  Loading...",
            Style::default().fg(MUTED_GRAY),
        ));
    }

    if state.search_active || !state.search_query.is_empty() {
        spans.extend_from_slice(&[
            Span::styled("  │  ", Style::default().fg(MUTED_GRAY)),
            Span::styled("/", Style::default().fg(GOLD)),
            Span::styled(
                state.search_query.clone(),
                Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD),
            ),
            if state.search_active {
                Span::styled("▏", Style::default().fg(SELECTION_GREEN))
            } else {
                Span::raw("")
            },
        ]);
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CORNFLOWER_BLUE))
        .style(Style::default().bg(DARK_BG));

    let paragraph = Paragraph::new(Line::from(spans)).block(block);
    frame.render_widget(paragraph, area);
}

fn render_provider_bar(frame: &mut Frame, area: Rect, state: &SkillsViewState) {
    let mut spans: Vec<Span> = vec![Span::styled(
        "  Provider: ",
        Style::default().fg(MUTED_GRAY),
    )];

    for (i, provider) in SkillsProvider::all().iter().enumerate() {
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

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CORNFLOWER_BLUE))
        .style(Style::default().bg(DARK_BG));

    let paragraph = Paragraph::new(Line::from(spans)).block(block);
    frame.render_widget(paragraph, area);
}

fn render_tab_bar(frame: &mut Frame, area: Rect, state: &SkillsViewState) {
    let titles: Vec<Line> = SkillsTab::all()
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

    let idx = SkillsTab::all().iter().position(|t| *t == state.active_tab).unwrap_or(0);
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

fn render_no_data(frame: &mut Frame, area: Rect, state: &SkillsViewState) {
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
                " skills browsing is not yet available.",
                Style::default().fg(MUTED_GRAY),
            ),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "  Skills parsing is currently supported for Claude Code only.",
            Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
        )),
        Line::from(Span::styled(
            "  Other providers will be added as they expose a skills surface.",
            Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
        )),
    ];
    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}

fn render_loading(frame: &mut Frame, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CORNFLOWER_BLUE))
        .style(Style::default().bg(DARK_BG));
    let paragraph = Paragraph::new(Line::from(vec![Span::styled(
        "  ⏳ Scanning ~/.claude/skills and ~/.claude/agents...",
        Style::default().fg(MUTED_GRAY),
    )]))
    .block(block);
    frame.render_widget(paragraph, area);
}

fn render_content(frame: &mut Frame, area: Rect, state: &SkillsViewState) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(area);

    match state.active_tab {
        SkillsTab::Skills => render_skills_tab(frame, chunks[0], chunks[1], state),
        SkillsTab::Agents => render_agents_tab(frame, chunks[0], chunks[1], state),
        SkillsTab::Associations => render_associations_tab(frame, chunks[0], chunks[1], state),
    }
}

fn list_block() -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CORNFLOWER_BLUE))
        .style(Style::default().bg(DARK_BG))
}

fn detail_block(title: &str) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CORNFLOWER_BLUE))
        .style(Style::default().bg(DARK_BG))
        .title(Span::styled(
            format!(" {} ", title),
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ))
}

fn render_skills_tab(
    frame: &mut Frame,
    list_area: Rect,
    detail_area: Rect,
    state: &SkillsViewState,
) {
    let data = state.data.as_ref().unwrap();
    let skills = filtered_skills(data, &state.search_query);

    let list_inner_block = list_block();
    let list_inner = list_inner_block.inner(list_area);
    frame.render_widget(list_inner_block, list_area);

    if skills.is_empty() {
        let p = Paragraph::new("  No skills match.").style(Style::default().fg(MUTED_GRAY));
        frame.render_widget(p, list_inner);
        render_empty_detail(frame, detail_area, "Skill");
        return;
    }

    let visible_rows = list_inner.height.saturating_sub(2) as usize;
    let sel = state.selected_index.min(skills.len().saturating_sub(1));
    let offset = compute_offset(sel, visible_rows, skills.len());

    let header = Row::new(vec!["", "Name", "Description"])
        .style(Style::default().fg(GOLD).add_modifier(Modifier::BOLD))
        .bottom_margin(1);

    let rows: Vec<Row> = skills
        .iter()
        .enumerate()
        .skip(offset)
        .take(visible_rows)
        .map(|(i, s)| {
            let indicator = if i == sel { "▶" } else { " " };
            let style = if i == sel {
                Style::default()
                    .fg(SOFT_WHITE)
                    .bg(LIST_HIGHLIGHT_BG)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(SOFT_WHITE)
            };
            Row::new(vec![
                Span::styled(indicator, Style::default().fg(SELECTION_GREEN)).to_string(),
                truncate_string(&s.name, 24),
                truncate_string(&one_line(&s.description), 80),
            ])
            .style(style)
        })
        .collect();

    let widths = [
        Constraint::Length(1),
        Constraint::Length(24),
        Constraint::Min(20),
    ];
    let table = Table::new(rows, widths).header(header).column_spacing(1);
    frame.render_widget(table, list_inner);

    // Detail pane
    let current = skills.get(sel).copied();
    render_skill_detail(frame, detail_area, current, data);
}

fn render_skill_detail(frame: &mut Frame, area: Rect, skill: Option<&Skill>, data: &SkillsData) {
    let block = detail_block("Skill");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(skill) = skill else {
        render_empty_detail_inner(frame, inner);
        return;
    };

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled("Name: ", Style::default().fg(MUTED_GRAY)),
        Span::styled(
            skill.name.clone(),
            Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD),
        ),
    ]));
    if let Some(inv) = skill.user_invocable {
        lines.push(Line::from(vec![
            Span::styled("User-invocable: ", Style::default().fg(MUTED_GRAY)),
            Span::styled(
                if inv { "yes" } else { "no" },
                Style::default().fg(SOFT_WHITE),
            ),
        ]));
    }
    lines.push(Line::from(vec![
        Span::styled("Source: ", Style::default().fg(MUTED_GRAY)),
        Span::styled(
            display_path(&skill.source_path.display().to_string()),
            Style::default().fg(SOFT_WHITE),
        ),
    ]));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Description",
        Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
    )));
    for l in wrap_text(&skill.description, inner.width.saturating_sub(2) as usize) {
        lines.push(Line::from(Span::styled(l, Style::default().fg(SOFT_WHITE))));
    }
    lines.push(Line::from(""));

    let agents_for_skill: &[String] =
        data.associations.get(&skill.name).map(Vec::as_slice).unwrap_or(&[]);
    lines.push(Line::from(Span::styled(
        format!("Used by {} agent(s)", agents_for_skill.len()),
        Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
    )));
    if agents_for_skill.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (no references found)",
            Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
        )));
    } else {
        for name in agents_for_skill {
            lines.push(Line::from(vec![
                Span::styled("  • ", Style::default().fg(SELECTION_GREEN)),
                Span::styled(name.clone(), Style::default().fg(SOFT_WHITE)),
            ]));
        }
    }

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}

fn render_agents_tab(
    frame: &mut Frame,
    list_area: Rect,
    detail_area: Rect,
    state: &SkillsViewState,
) {
    let data = state.data.as_ref().unwrap();
    let agents = filtered_agents(data, &state.search_query);

    let list_inner_block = list_block();
    let list_inner = list_inner_block.inner(list_area);
    frame.render_widget(list_inner_block, list_area);

    if agents.is_empty() {
        let p = Paragraph::new("  No agents match.").style(Style::default().fg(MUTED_GRAY));
        frame.render_widget(p, list_inner);
        render_empty_detail(frame, detail_area, "Agent");
        return;
    }

    let visible_rows = list_inner.height.saturating_sub(2) as usize;
    let sel = state.selected_index.min(agents.len().saturating_sub(1));
    let offset = compute_offset(sel, visible_rows, agents.len());

    let header = Row::new(vec!["", "Name", "Description"])
        .style(Style::default().fg(GOLD).add_modifier(Modifier::BOLD))
        .bottom_margin(1);

    let rows: Vec<Row> = agents
        .iter()
        .enumerate()
        .skip(offset)
        .take(visible_rows)
        .map(|(i, a)| {
            let indicator = if i == sel { "▶" } else { " " };
            let style = if i == sel {
                Style::default()
                    .fg(SOFT_WHITE)
                    .bg(LIST_HIGHLIGHT_BG)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(SOFT_WHITE)
            };
            Row::new(vec![
                Span::styled(indicator, Style::default().fg(SELECTION_GREEN)).to_string(),
                truncate_string(&a.name, 26),
                truncate_string(&one_line(&a.description), 80),
            ])
            .style(style)
        })
        .collect();

    let widths = [
        Constraint::Length(1),
        Constraint::Length(26),
        Constraint::Min(20),
    ];
    let table = Table::new(rows, widths).header(header).column_spacing(1);
    frame.render_widget(table, list_inner);

    let current = agents.get(sel).copied();
    render_agent_detail(frame, detail_area, current);
}

fn render_agent_detail(frame: &mut Frame, area: Rect, agent: Option<&AgentDef>) {
    let block = detail_block("Agent");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(agent) = agent else {
        render_empty_detail_inner(frame, inner);
        return;
    };

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled("Name: ", Style::default().fg(MUTED_GRAY)),
        Span::styled(
            agent.name.clone(),
            Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Source: ", Style::default().fg(MUTED_GRAY)),
        Span::styled(
            display_path(&agent.source_path.display().to_string()),
            Style::default().fg(SOFT_WHITE),
        ),
    ]));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Description",
        Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
    )));
    for l in wrap_text(&agent.description, inner.width.saturating_sub(2) as usize) {
        lines.push(Line::from(Span::styled(l, Style::default().fg(SOFT_WHITE))));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!("Tools ({})", agent.tools.len()),
        Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
    )));
    if agent.tools.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (none listed)",
            Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
        )));
    } else {
        for t in &agent.tools {
            lines.push(Line::from(vec![
                Span::styled("  • ", Style::default().fg(SELECTION_GREEN)),
                Span::styled(t.clone(), Style::default().fg(SOFT_WHITE)),
            ]));
        }
    }

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}

fn render_associations_tab(
    frame: &mut Frame,
    list_area: Rect,
    detail_area: Rect,
    state: &SkillsViewState,
) {
    let data = state.data.as_ref().unwrap();
    let rows = association_rows(data, &state.search_query);

    let list_inner_block = list_block();
    let list_inner = list_inner_block.inner(list_area);
    frame.render_widget(list_inner_block, list_area);

    if rows.is_empty() {
        let p = Paragraph::new("  No associations found.").style(Style::default().fg(MUTED_GRAY));
        frame.render_widget(p, list_inner);
        render_empty_detail(frame, detail_area, "Association");
        return;
    }

    let visible_rows = list_inner.height.saturating_sub(2) as usize;
    let sel = state.selected_index.min(rows.len().saturating_sub(1));
    let offset = compute_offset(sel, visible_rows, rows.len());

    let header = Row::new(vec!["", "Skill", "Agents"])
        .style(Style::default().fg(GOLD).add_modifier(Modifier::BOLD))
        .bottom_margin(1);

    let table_rows: Vec<Row> = rows
        .iter()
        .enumerate()
        .skip(offset)
        .take(visible_rows)
        .map(|(i, (skill, agents))| {
            let indicator = if i == sel { "▶" } else { " " };
            let style = if i == sel {
                Style::default()
                    .fg(SOFT_WHITE)
                    .bg(LIST_HIGHLIGHT_BG)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(SOFT_WHITE)
            };
            Row::new(vec![
                Span::styled(indicator, Style::default().fg(SELECTION_GREEN)).to_string(),
                truncate_string(&skill.name, 24),
                format!("{} agent(s)", agents.len()),
            ])
            .style(style)
        })
        .collect();

    let widths = [
        Constraint::Length(1),
        Constraint::Length(24),
        Constraint::Min(10),
    ];
    let table = Table::new(table_rows, widths).header(header).column_spacing(1);
    frame.render_widget(table, list_inner);

    // Detail pane: skill + the list of agents.
    let block = detail_block("Association");
    let inner = block.inner(detail_area);
    frame.render_widget(block, detail_area);

    let Some((skill, agents)) = rows.get(sel) else {
        render_empty_detail_inner(frame, inner);
        return;
    };

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled("Skill: ", Style::default().fg(MUTED_GRAY)),
        Span::styled(
            skill.name.clone(),
            Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Source: ", Style::default().fg(MUTED_GRAY)),
        Span::styled(
            display_path(&skill.source_path.display().to_string()),
            Style::default().fg(SOFT_WHITE),
        ),
    ]));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Description",
        Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
    )));
    for l in wrap_text(&skill.description, inner.width.saturating_sub(2) as usize) {
        lines.push(Line::from(Span::styled(l, Style::default().fg(SOFT_WHITE))));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!("Referenced by {} agent(s)", agents.len()),
        Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
    )));
    for name in agents {
        lines.push(Line::from(vec![
            Span::styled("  • ", Style::default().fg(SELECTION_GREEN)),
            Span::styled((*name).to_string(), Style::default().fg(SOFT_WHITE)),
        ]));
    }

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}

fn render_empty_detail(frame: &mut Frame, area: Rect, label: &str) {
    let block = detail_block(label);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    render_empty_detail_inner(frame, inner);
}

fn render_empty_detail_inner(frame: &mut Frame, area: Rect) {
    let p = Paragraph::new(Line::from(Span::styled(
        "  Nothing selected.",
        Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
    )));
    frame.render_widget(p, area);
}

fn render_help_bar(frame: &mut Frame, area: Rect, state: &SkillsViewState) {
    let spans = if state.search_active {
        vec![
            Span::styled(" /", Style::default().fg(GOLD)),
            Span::styled(" type to filter  ", Style::default().fg(MUTED_GRAY)),
            Span::styled("Enter", Style::default().fg(GOLD)),
            Span::styled(" apply  ", Style::default().fg(MUTED_GRAY)),
            Span::styled("Esc", Style::default().fg(GOLD)),
            Span::styled(" cancel", Style::default().fg(MUTED_GRAY)),
        ]
    } else {
        vec![
            Span::styled(" ◀/▶", Style::default().fg(GOLD)),
            Span::styled(" provider  ", Style::default().fg(MUTED_GRAY)),
            Span::styled("Tab", Style::default().fg(GOLD)),
            Span::styled(" view  ", Style::default().fg(MUTED_GRAY)),
            Span::styled("j/k", Style::default().fg(GOLD)),
            Span::styled(" scroll  ", Style::default().fg(MUTED_GRAY)),
            Span::styled("/", Style::default().fg(GOLD)),
            Span::styled(" search  ", Style::default().fg(MUTED_GRAY)),
            Span::styled("r", Style::default().fg(GOLD)),
            Span::styled(" refresh  ", Style::default().fg(MUTED_GRAY)),
            Span::styled("Esc", Style::default().fg(GOLD)),
            Span::styled(" back", Style::default().fg(MUTED_GRAY)),
        ]
    };
    let paragraph = Paragraph::new(Line::from(spans)).style(Style::default().bg(DARK_BG));
    frame.render_widget(paragraph, area);
}

// ------------- small helpers -------------

fn compute_offset(selected: usize, visible: usize, total: usize) -> usize {
    if visible == 0 || total == 0 {
        return 0;
    }
    if selected < visible {
        0
    } else {
        let max_offset = total.saturating_sub(visible);
        (selected + 1).saturating_sub(visible).min(max_offset)
    }
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_string(s: &str, max_len: usize) -> String {
    crate::widgets::truncate_with_ellipsis(s, max_len).into_owned()
}

fn display_path(p: &str) -> String {
    if let Some(home) = dirs::home_dir() {
        let home_str = home.display().to_string();
        if let Some(stripped) = p.strip_prefix(&home_str) {
            return format!("~{}", stripped);
        }
    }
    p.to_string()
}

fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if current.is_empty() {
            current.push_str(word);
        } else if current.len() + 1 + word.len() <= width {
            current.push(' ');
            current.push_str(word);
        } else {
            out.push(std::mem::take(&mut current));
            current.push_str(word);
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}
