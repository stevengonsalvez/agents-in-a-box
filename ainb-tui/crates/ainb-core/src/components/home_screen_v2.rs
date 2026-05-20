// ABOUTME: Refreshed home screen component with premium sidebar and welcome panel
// This is the v2 design for AINB 2.0, featuring:
// - Animated "Boxy" mascot in the header
// - Premium VS Code/Discord-style sidebar navigation with shortcuts
// - Welcome panel with getting started guide and architecture overview

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph},
};
use std::time::{Duration, Instant};

use super::mascot::{MascotAnimation, render_mascot};
use super::sidebar::{SidebarComponent, SidebarItem, SidebarState};
use super::welcome_panel::{WelcomePanelComponent, WelcomePanelState};
use crate::models::Workspace;

// Color palette from TUI style guide
const CORNFLOWER_BLUE: Color = Color::Rgb(100, 149, 237);
const GOLD: Color = Color::Rgb(255, 215, 0);
const SELECTION_GREEN: Color = Color::Rgb(100, 200, 100);
const DARK_BG: Color = Color::Rgb(25, 25, 35);
const PANEL_BG: Color = Color::Rgb(30, 30, 40);
const SOFT_WHITE: Color = Color::Rgb(220, 220, 230);
const MUTED_GRAY: Color = Color::Rgb(120, 120, 140);
const SUBDUED_BORDER: Color = Color::Rgb(60, 60, 80);
const SIDEBAR_EDGE_HIT_SLOP: u16 = 1;
pub const SIDEBAR_DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(300);

/// Focus area on the home screen
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HomeScreenFocus {
    Sidebar,
    ContentPanel,
}

/// State for the refreshed home screen
#[derive(Debug)]
pub struct HomeScreenV2State {
    /// Current focus (always sidebar for now)
    pub focus: HomeScreenFocus,
    /// Sidebar state
    pub sidebar: SidebarState,
    /// Welcome panel state
    pub welcome: WelcomePanelState,
    /// Mascot animation
    pub mascot: MascotAnimation,
    /// Last sidebar area rendered by HomeScreen V2.
    pub last_sidebar_rect: Option<Rect>,
    /// Whether the mouse is currently over the sidebar resize edge.
    pub sidebar_edge_hovered: bool,
    /// Whether a sidebar resize drag is active.
    pub sidebar_resize_active: bool,
    last_sidebar_click: Option<(usize, Instant)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SidebarClickOutcome {
    pub item: SidebarItem,
    pub double_click: bool,
}

impl HomeScreenV2State {
    pub fn new() -> Self {
        let mut state = Self {
            focus: HomeScreenFocus::Sidebar,
            sidebar: SidebarState::new(),
            welcome: WelcomePanelState::new(),
            mascot: MascotAnimation::new(),
            last_sidebar_rect: None,
            sidebar_edge_hovered: false,
            sidebar_resize_active: false,
            last_sidebar_click: None,
        };
        // Sidebar starts focused
        state.sidebar.is_focused = true;
        state.welcome.is_focused = false;
        state
    }

    /// Toggle focus between sidebar and content panel
    pub fn toggle_focus(&mut self) {
        match self.focus {
            HomeScreenFocus::Sidebar => {
                self.focus = HomeScreenFocus::ContentPanel;
                self.sidebar.is_focused = false;
                self.welcome.is_focused = true;
            }
            HomeScreenFocus::ContentPanel => {
                self.focus = HomeScreenFocus::Sidebar;
                self.sidebar.is_focused = true;
                self.welcome.is_focused = false;
            }
        }
    }

    /// Update mascot animation
    pub fn tick_mascot(&mut self) {
        self.mascot.tick();
    }

    /// Update session count badge
    pub fn set_active_sessions(&mut self, count: usize) {
        self.sidebar.active_sessions_count = count;
    }

    pub fn restore_sidebar_width(&mut self, width: Option<u16>) {
        if let Some(width) = width {
            self.sidebar.preferred_width = width.max(super::sidebar::MIN_SIDEBAR_WIDTH);
        }
    }

    pub fn rendered_sidebar_width(&self) -> Option<u16> {
        self.last_sidebar_rect.map(|rect| rect.width)
    }

    fn remember_sidebar_rect(&mut self, rect: Rect) {
        self.last_sidebar_rect = Some(rect);
    }

    pub fn sidebar_edge_highlighted(&self) -> bool {
        self.sidebar_edge_hovered || self.sidebar_resize_active
    }

    pub fn update_sidebar_edge_hover(&mut self, x: u16, y: u16) {
        self.sidebar_edge_hovered = self.is_on_sidebar_edge(x, y);
    }

    pub fn is_on_sidebar_edge(&self, x: u16, y: u16) -> bool {
        let Some(rect) = self.last_sidebar_rect else {
            return false;
        };
        if y < rect.y || y >= rect.y.saturating_add(rect.height) || rect.width == 0 {
            return false;
        }

        let edge_x = rect.x.saturating_add(rect.width.saturating_sub(1));
        x.abs_diff(edge_x) <= SIDEBAR_EDGE_HIT_SLOP
    }

    pub fn begin_sidebar_resize(&mut self, x: u16, y: u16) -> bool {
        let on_edge = self.is_on_sidebar_edge(x, y);
        self.sidebar_resize_active = on_edge;
        self.sidebar_edge_hovered = on_edge;
        on_edge
    }

    pub fn drag_sidebar_resize(&mut self, x: u16, terminal_width: u16) -> bool {
        if !self.sidebar_resize_active {
            return false;
        }
        let Some(rect) = self.last_sidebar_rect else {
            return false;
        };

        let requested_width = x.saturating_sub(rect.x).saturating_add(1);
        self.sidebar.set_preferred_width(requested_width, terminal_width);
        true
    }

    pub fn finish_sidebar_resize(&mut self) -> bool {
        let was_active = self.sidebar_resize_active;
        self.sidebar_resize_active = false;
        was_active
    }

    pub fn click_sidebar_item_at(
        &mut self,
        x: u16,
        y: u16,
        now: Instant,
    ) -> Option<SidebarClickOutcome> {
        let rect = self.last_sidebar_rect?;
        if !rect_contains(rect, x, y) || self.is_on_sidebar_edge(x, y) {
            return None;
        }

        let item_index = SidebarComponent::item_index_at(rect, y)?;
        self.sidebar.select_index(item_index);
        self.focus = HomeScreenFocus::Sidebar;
        self.sidebar.is_focused = true;
        self.welcome.is_focused = false;

        let double_click = self
            .last_sidebar_click
            .map(|(last_index, last_at)| {
                last_index == item_index
                    && now.saturating_duration_since(last_at) <= SIDEBAR_DOUBLE_CLICK_WINDOW
            })
            .unwrap_or(false);
        self.last_sidebar_click = Some((item_index, now));

        Some(SidebarClickOutcome {
            item: self.sidebar.selected_item(),
            double_click,
        })
    }
}

impl Default for HomeScreenV2State {
    fn default() -> Self {
        Self::new()
    }
}

fn rect_contains(rect: Rect, x: u16, y: u16) -> bool {
    x >= rect.x
        && x < rect.x.saturating_add(rect.width)
        && y >= rect.y
        && y < rect.y.saturating_add(rect.height)
}

/// Layout mode based on terminal size
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutMode {
    Full,     // 120+ cols, 35+ rows
    Standard, // 100+ cols, 30+ rows
    Compact,  // 80+ cols, 24+ rows
    Minimal,  // Smaller terminals
}

impl LayoutMode {
    pub fn detect(area: Rect) -> Self {
        match (area.width, area.height) {
            (w, h) if w >= 120 && h >= 35 => Self::Full,
            (w, h) if w >= 100 && h >= 30 => Self::Standard,
            (w, h) if w >= 80 && h >= 24 => Self::Compact,
            _ => Self::Minimal,
        }
    }
}

/// The refreshed home screen component
pub struct HomeScreenV2Component {
    sidebar: SidebarComponent,
    welcome_panel: WelcomePanelComponent,
}

impl HomeScreenV2Component {
    pub fn new() -> Self {
        Self {
            sidebar: SidebarComponent::new(),
            welcome_panel: WelcomePanelComponent::new(),
        }
    }

    /// Main render function
    pub fn render(
        &self,
        frame: &mut Frame,
        area: Rect,
        state: &mut HomeScreenV2State,
        workspaces: &[Workspace],
    ) {
        self.render_with_loading(frame, area, state, workspaces, false)
    }

    /// Main render function with loading indicator support
    pub fn render_with_loading(
        &self,
        frame: &mut Frame,
        area: Rect,
        state: &mut HomeScreenV2State,
        workspaces: &[Workspace],
        is_loading: bool,
    ) {
        let layout_mode = LayoutMode::detect(area);

        // Main container with dark background
        let container = Block::default().style(Style::default().bg(DARK_BG));
        frame.render_widget(container, area);

        match layout_mode {
            LayoutMode::Full | LayoutMode::Standard => {
                self.render_full_layout_with_loading(frame, area, state, workspaces, is_loading);
            }
            LayoutMode::Compact => {
                self.render_compact_layout(frame, area, state, workspaces);
            }
            LayoutMode::Minimal => {
                self.render_minimal_layout(frame, area, state);
            }
        }
    }

    /// Full layout with sidebar, mascot header, and welcome panel
    fn render_full_layout(
        &self,
        frame: &mut Frame,
        area: Rect,
        state: &mut HomeScreenV2State,
        workspaces: &[Workspace],
    ) {
        self.render_full_layout_with_loading(frame, area, state, workspaces, false)
    }

    /// Full layout with loading indicator support
    fn render_full_layout_with_loading(
        &self,
        frame: &mut Frame,
        area: Rect,
        state: &mut HomeScreenV2State,
        workspaces: &[Workspace],
        is_loading: bool,
    ) {
        // Vertical layout: header, main content, recent activity, help bar
        let main_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(7), // Header with mascot
                Constraint::Min(20),   // Main content (sidebar + welcome)
                Constraint::Length(3), // Recent activity
                Constraint::Length(2), // Help bar
            ])
            .split(area);

        // Render header with mascot
        self.render_header(frame, main_layout[0], state);

        // Horizontal split: sidebar | welcome panel
        let sidebar_width = state.sidebar.effective_width(main_layout[1].width);
        let content_layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(sidebar_width), // Sidebar (mouse-resizable)
                Constraint::Min(1),                // Welcome panel
            ])
            .split(main_layout[1]);
        state.remember_sidebar_rect(content_layout[0]);

        // Render sidebar
        self.sidebar.render_with_edge_highlight(
            frame,
            content_layout[0],
            &state.sidebar,
            state.sidebar_edge_highlighted(),
        );

        // Render welcome panel (needs mutable state for scroll tracking)
        self.welcome_panel.render(frame, content_layout[1], &mut state.welcome);

        // Render recent activity (or loading indicator)
        if is_loading {
            self.render_loading_indicator(frame, main_layout[2]);
        } else {
            self.render_recent_activity(frame, main_layout[2], workspaces);
        }

        // Render help bar
        self.render_help_bar(frame, main_layout[3], state);
    }

    /// Render a loading indicator
    fn render_loading_indicator(&self, frame: &mut Frame, area: Rect) {
        use ratatui::widgets::Paragraph;

        // Animated loading spinner using frame count
        let spinner_frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        let frame_idx = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() / 100)
            .unwrap_or(0)
            % spinner_frames.len() as u128) as usize;
        let spinner = spinner_frames[frame_idx];

        let loading_text = format!("{} Loading sessions...", spinner);
        let loading = Paragraph::new(loading_text).style(Style::default().fg(GOLD)).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(CORNFLOWER_BLUE))
                .style(Style::default().bg(PANEL_BG)),
        );

        frame.render_widget(loading, area);
    }

    /// Compact layout for smaller terminals
    fn render_compact_layout(
        &self,
        frame: &mut Frame,
        area: Rect,
        state: &mut HomeScreenV2State,
        workspaces: &[Workspace],
    ) {
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(4), // Compact header
                Constraint::Min(16),   // Content
                Constraint::Length(2), // Recent activity
                Constraint::Length(2), // Help bar
            ])
            .split(area);

        self.render_compact_header(frame, layout[0], state);

        // Horizontal split: sidebar | welcome
        let sidebar_width = state.sidebar.effective_width(layout[1].width);
        let content_layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(sidebar_width), // Sidebar
                Constraint::Min(1),                // Welcome panel
            ])
            .split(layout[1]);
        state.remember_sidebar_rect(content_layout[0]);

        self.sidebar.render_with_edge_highlight(
            frame,
            content_layout[0],
            &state.sidebar,
            state.sidebar_edge_highlighted(),
        );
        self.welcome_panel.render(frame, content_layout[1], &mut state.welcome);

        self.render_recent_activity(frame, layout[2], workspaces);
        self.render_help_bar(frame, layout[3], state);
    }

    /// Minimal layout for very small terminals
    fn render_minimal_layout(&self, frame: &mut Frame, area: Rect, state: &mut HomeScreenV2State) {
        // Just show sidebar as a simple list
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2), // Title
                Constraint::Min(10),   // Sidebar list
                Constraint::Length(2), // Help
            ])
            .split(area);

        let title = Paragraph::new(Line::from(vec![
            Span::styled(
                " AINB ",
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ),
            Span::styled("- Agents in a Box", Style::default().fg(SOFT_WHITE)),
        ]))
        .alignment(Alignment::Center)
        .style(Style::default().bg(DARK_BG));

        frame.render_widget(title, layout[0]);

        let sidebar_width = state.sidebar.effective_width(layout[1].width);
        let content_layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(sidebar_width), Constraint::Min(1)])
            .split(layout[1]);
        state.remember_sidebar_rect(content_layout[0]);

        self.sidebar.render_with_edge_highlight(
            frame,
            content_layout[0],
            &state.sidebar,
            state.sidebar_edge_highlighted(),
        );

        if content_layout[1].width > 0 {
            let selected = state.sidebar.selected_item();
            let summary = Paragraph::new(Line::from(vec![
                Span::styled(" ", Style::default()),
                Span::styled(selected.label(), Style::default().fg(GOLD)),
            ]))
            .style(Style::default().bg(DARK_BG));
            frame.render_widget(summary, content_layout[1]);
        }

        self.render_help_bar(frame, layout[2], state);
    }

    /// Render header with mascot and title
    fn render_header(&self, frame: &mut Frame, area: Rect, state: &HomeScreenV2State) {
        let block = Block::default()
            .borders(Borders::BOTTOM)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(CORNFLOWER_BLUE))
            .style(Style::default().bg(PANEL_BG));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        // Split header: mascot | title area
        let header_layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(22), // Mascot area
                Constraint::Min(40),    // Title and version
            ])
            .split(inner);

        // Render mascot
        render_mascot(frame, header_layout[0], &state.mascot);

        // Render title section
        let title_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // Top padding
                Constraint::Length(2), // Main title
                Constraint::Length(1), // Subtitle
                Constraint::Min(0),    // Bottom padding
            ])
            .split(header_layout[1]);

        let title = Paragraph::new(Line::from(vec![
            Span::styled(
                "A I N B",
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ),
            Span::styled("  -  ", Style::default().fg(SUBDUED_BORDER)),
            Span::styled("Agents in a Box", Style::default().fg(SOFT_WHITE)),
        ]))
        .style(Style::default().bg(PANEL_BG));

        let subtitle = Paragraph::new(Line::from(vec![
            Span::styled(
                "Your AI-Powered Development Hub",
                Style::default().fg(MUTED_GRAY),
            ),
            Span::styled("                    ", Style::default()),
            Span::styled("v2.0.0", Style::default().fg(MUTED_GRAY)),
        ]))
        .style(Style::default().bg(PANEL_BG));

        frame.render_widget(title, title_layout[1]);
        frame.render_widget(subtitle, title_layout[2]);
    }

    /// Render compact header with mini mascot
    fn render_compact_header(&self, frame: &mut Frame, area: Rect, state: &HomeScreenV2State) {
        let block = Block::default()
            .borders(Borders::BOTTOM)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(CORNFLOWER_BLUE))
            .style(Style::default().bg(PANEL_BG));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        // For compact, use mini mascot inline with title
        let mut mascot_copy = state.mascot.clone();
        mascot_copy.set_mini(true);

        let header_layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(8), // Mini mascot
                Constraint::Min(30),   // Title
            ])
            .split(inner);

        render_mascot(frame, header_layout[0], &mascot_copy);

        let title = Paragraph::new(Line::from(vec![
            Span::styled(
                "AINB",
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" - Agents in a Box", Style::default().fg(SOFT_WHITE)),
            Span::styled("  v2.0.0", Style::default().fg(MUTED_GRAY)),
        ]))
        .style(Style::default().bg(PANEL_BG));

        frame.render_widget(title, header_layout[1]);
    }

    /// Render recent activity bar
    fn render_recent_activity(&self, frame: &mut Frame, area: Rect, workspaces: &[Workspace]) {
        let block = Block::default()
            .borders(Borders::TOP)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(SUBDUED_BORDER))
            .style(Style::default().bg(DARK_BG));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        // Build recent session display
        let recent_line = if let Some(workspace) = workspaces.first() {
            if let Some(session) = workspace.sessions.first() {
                let status_icon = if session.status.is_running() { "" } else { "" };
                let status_color = if session.status.is_running() {
                    SELECTION_GREEN
                } else {
                    MUTED_GRAY
                };

                Line::from(vec![
                    Span::styled("   Recent: ", Style::default().fg(GOLD)),
                    Span::styled(
                        workspace.name.clone(),
                        Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled("/", Style::default().fg(MUTED_GRAY)),
                    Span::styled(
                        session.branch_name.clone(),
                        Style::default().fg(CORNFLOWER_BLUE),
                    ),
                    Span::styled("  ", Style::default()),
                    Span::styled(status_icon, Style::default().fg(status_color)),
                    Span::styled(
                        if session.status.is_running() {
                            " Running"
                        } else {
                            " Stopped"
                        },
                        Style::default().fg(status_color),
                    ),
                ])
            } else {
                Line::from(vec![Span::styled(
                    "   No recent sessions",
                    Style::default().fg(MUTED_GRAY),
                )])
            }
        } else {
            Line::from(vec![
                Span::styled(
                    "   No workspaces configured - press ",
                    Style::default().fg(MUTED_GRAY),
                ),
                Span::styled("s", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
                Span::styled(" to go to Sessions", Style::default().fg(MUTED_GRAY)),
            ])
        };

        let recent = Paragraph::new(recent_line).style(Style::default().bg(DARK_BG));
        frame.render_widget(recent, inner);
    }

    /// Render the bottom help bar
    fn render_help_bar(&self, frame: &mut Frame, area: Rect, state: &HomeScreenV2State) {
        let help_items: Vec<(&str, &str)> = match state.focus {
            HomeScreenFocus::ContentPanel => vec![
                ("Tab", "sidebar"),
                ("↑↓", "scroll"),
                ("PgUp/Dn", "page"),
                ("y", "copy"),
                ("?", "help"),
                ("q", "quit"),
            ],
            HomeScreenFocus::Sidebar => vec![
                ("Enter", "select"),
                ("Tab", "content"),
                ("↑↓", "navigate"),
                ("?", "help"),
                ("q", "quit"),
            ],
        };

        let mut spans = Vec::new();
        spans.push(Span::styled("  ", Style::default()));

        for (i, (key, desc)) in help_items.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(" | ", Style::default().fg(SUBDUED_BORDER)));
            }
            spans.push(Span::styled(
                *key,
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(" ", Style::default()));
            spans.push(Span::styled(*desc, Style::default().fg(MUTED_GRAY)));
        }

        let help_bar = Paragraph::new(Line::from(spans)).style(Style::default().bg(DARK_BG));

        frame.render_widget(help_bar, area);
    }
}

impl Default for HomeScreenV2Component {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_layout_mode_detection() {
        let full = Rect::new(0, 0, 120, 40);
        assert_eq!(LayoutMode::detect(full), LayoutMode::Full);

        let standard = Rect::new(0, 0, 100, 30);
        assert_eq!(LayoutMode::detect(standard), LayoutMode::Standard);

        let compact = Rect::new(0, 0, 80, 24);
        assert_eq!(LayoutMode::detect(compact), LayoutMode::Compact);

        let minimal = Rect::new(0, 0, 60, 20);
        assert_eq!(LayoutMode::detect(minimal), LayoutMode::Minimal);
    }

    #[test]
    fn test_session_badge() {
        let mut state = HomeScreenV2State::new();
        state.set_active_sessions(5);

        assert_eq!(state.sidebar.active_sessions_count, 5);
    }

    #[test]
    fn test_default_focus() {
        let state = HomeScreenV2State::new();
        assert_eq!(state.focus, HomeScreenFocus::Sidebar);
    }

    #[test]
    fn detects_sidebar_drag_start_band() {
        let mut state = HomeScreenV2State::new();
        state.remember_sidebar_rect(Rect::new(0, 4, 26, 20));

        assert!(state.begin_sidebar_resize(24, 8));
        assert!(state.sidebar_resize_active);
        state.finish_sidebar_resize();

        assert!(state.begin_sidebar_resize(25, 8));
        state.finish_sidebar_resize();

        assert!(state.begin_sidebar_resize(26, 8));
        state.finish_sidebar_resize();

        assert!(!state.begin_sidebar_resize(23, 8));
        assert!(!state.sidebar_resize_active);
    }

    #[test]
    fn drag_resize_updates_width_with_bounds() {
        let mut state = HomeScreenV2State::new();
        state.remember_sidebar_rect(Rect::new(0, 4, 26, 20));
        assert!(state.begin_sidebar_resize(25, 8));
        assert!(state.drag_sidebar_resize(44, 120));
        assert_eq!(state.sidebar.preferred_width, 45);

        assert!(state.drag_sidebar_resize(2, 120));
        assert_eq!(
            state.sidebar.preferred_width,
            crate::components::sidebar::MIN_SIDEBAR_WIDTH
        );
    }

    #[test]
    fn sidebar_click_selects_then_double_click_navigates() {
        let mut state = HomeScreenV2State::new();
        state.remember_sidebar_rect(Rect::new(0, 4, 26, 30));
        let now = Instant::now();

        let first = state.click_sidebar_item_at(3, 10, now).unwrap();
        assert_eq!(first.item, SidebarItem::Catalog);
        assert!(!first.double_click);
        assert_eq!(state.sidebar.selected_item(), SidebarItem::Catalog);

        let second = state.click_sidebar_item_at(3, 10, now + SIDEBAR_DOUBLE_CLICK_WINDOW).unwrap();
        assert_eq!(second.item, SidebarItem::Catalog);
        assert!(second.double_click);
    }

    #[test]
    fn slow_second_sidebar_click_is_not_double_click() {
        let mut state = HomeScreenV2State::new();
        state.remember_sidebar_rect(Rect::new(0, 4, 26, 30));
        let now = Instant::now();

        assert!(!state.click_sidebar_item_at(3, 10, now).unwrap().double_click);
        assert!(
            !state
                .click_sidebar_item_at(
                    3,
                    10,
                    now + SIDEBAR_DOUBLE_CLICK_WINDOW + Duration::from_millis(1)
                )
                .unwrap()
                .double_click
        );
    }

    #[test]
    fn rendered_width_uses_sidebar_state() {
        let component = HomeScreenV2Component::new();
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let mut state = HomeScreenV2State::new();
        state.sidebar.set_preferred_width(40, 120);

        terminal
            .draw(|frame| {
                component.render(frame, Rect::new(0, 0, 120, 40), &mut state, &[]);
            })
            .unwrap();

        assert_eq!(state.rendered_sidebar_width(), Some(40));
    }
}
