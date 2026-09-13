// ABOUTME: Refreshed home screen component with premium sidebar and welcome panel
// This is the v2 design for AINB 2.0, featuring:
// - Animated "Boxy" mascot in the header
// - Premium VS Code/Discord-style sidebar navigation with shortcuts
// - Welcome panel with getting started guide and architecture overview

pub use ainb_app::components::home_screen_v2::*;

use crate::app::ui_state::UiState;
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph},
};

use super::mascot::render_mascot;
use super::sidebar::SidebarComponent;
use super::welcome_panel::WelcomePanelComponent;
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
const DISPLAY_VERSION: &str = concat!("v", env!("CARGO_PKG_VERSION"));
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
        state: &HomeScreenV2State,
        workspaces: &[Workspace],
        ui: &mut UiState,
    ) {
        self.render_with_loading(frame, area, state, workspaces, false, ui)
    }

    /// Main render function with loading indicator support
    pub fn render_with_loading(
        &self,
        frame: &mut Frame,
        area: Rect,
        state: &HomeScreenV2State,
        workspaces: &[Workspace],
        is_loading: bool,
        ui: &mut UiState,
    ) {
        let layout_mode = LayoutMode::detect(area);

        // Main container with dark background
        let container = Block::default().style(Style::default().bg(DARK_BG));
        frame.render_widget(container, area);

        match layout_mode {
            LayoutMode::Full | LayoutMode::Standard => {
                self.render_full_layout_with_loading(
                    frame, area, state, workspaces, is_loading, ui,
                );
            }
            LayoutMode::Compact => {
                self.render_compact_layout(frame, area, state, workspaces, ui);
            }
            LayoutMode::Minimal => {
                self.render_minimal_layout(frame, area, state, ui);
            }
        }
    }

    /// Full layout with loading indicator support
    fn render_full_layout_with_loading(
        &self,
        frame: &mut Frame,
        area: Rect,
        state: &HomeScreenV2State,
        workspaces: &[Workspace],
        is_loading: bool,
        ui: &mut UiState,
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
        ui.home_sidebar_rect = Some(content_layout[0]);

        // Render sidebar
        self.sidebar.render_with_edge_highlight(
            frame,
            content_layout[0],
            &state.sidebar,
            state.sidebar_edge_highlighted(),
        );

        // Render welcome panel (needs mutable state for scroll tracking)
        ui.welcome_viewport = self.welcome_panel.render(frame, content_layout[1], &state.welcome);

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
        state: &HomeScreenV2State,
        workspaces: &[Workspace],
        ui: &mut UiState,
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
        ui.home_sidebar_rect = Some(content_layout[0]);

        self.sidebar.render_with_edge_highlight(
            frame,
            content_layout[0],
            &state.sidebar,
            state.sidebar_edge_highlighted(),
        );
        ui.welcome_viewport = self.welcome_panel.render(frame, content_layout[1], &state.welcome);

        self.render_recent_activity(frame, layout[2], workspaces);
        self.render_help_bar(frame, layout[3], state);
    }

    /// Minimal layout for very small terminals
    fn render_minimal_layout(
        &self,
        frame: &mut Frame,
        area: Rect,
        state: &HomeScreenV2State,
        ui: &mut UiState,
    ) {
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
        ui.home_sidebar_rect = Some(content_layout[0]);

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
            Span::styled(DISPLAY_VERSION, Style::default().fg(MUTED_GRAY)),
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
            Span::styled(
                format!("  {DISPLAY_VERSION}"),
                Style::default().fg(MUTED_GRAY),
            ),
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
    use super::super::sidebar::SidebarItem;
    use super::*;
    use std::time::{Duration, Instant};

    fn render_to_string(width: u16, height: u16) -> String {
        let component = HomeScreenV2Component::new();
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let state = HomeScreenV2State::new();
        let mut ui = crate::app::ui_state::UiState::default();

        terminal
            .draw(|frame| component.render(frame, frame.area(), &state, &[], &mut ui))
            .unwrap();

        terminal.backend().buffer().content().iter().map(|cell| cell.symbol()).collect()
    }

    #[test]
    fn headers_render_the_package_version() {
        assert!(render_to_string(120, 40).contains(DISPLAY_VERSION));
        assert!(render_to_string(80, 24).contains(DISPLAY_VERSION));
    }

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
        state.last_sidebar_rect = Some(crate::geometry::Area::new(0, 4, 26, 20));

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
        state.last_sidebar_rect = Some(crate::geometry::Area::new(0, 4, 26, 20));
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
        state.last_sidebar_rect = Some(crate::geometry::Area::new(0, 4, 26, 30));
        let now = Instant::now();

        // first_item_y = rect.y + 3 = 7. Sessions (item 0) is selected and 2 rows
        // tall (rows 7-8), so Setup (item 1) sits at row 9.
        let first = state.click_sidebar_item_at(3, 9, now).unwrap();
        assert_eq!(first.item, SidebarItem::Setup);
        assert!(!first.double_click);
        assert_eq!(state.sidebar.selected_item(), SidebarItem::Setup);

        let second =
            state.click_sidebar_item_at(3, 9, now + sidebar_double_click_window()).unwrap();
        assert_eq!(second.item, SidebarItem::Setup);
        assert!(second.double_click);
    }

    #[test]
    fn slow_second_sidebar_click_is_not_double_click() {
        let mut state = HomeScreenV2State::new();
        state.last_sidebar_rect = Some(crate::geometry::Area::new(0, 4, 26, 30));
        let now = Instant::now();

        assert!(!state.click_sidebar_item_at(3, 10, now).unwrap().double_click);
        assert!(
            !state
                .click_sidebar_item_at(
                    3,
                    10,
                    now + sidebar_double_click_window() + Duration::from_millis(1)
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
        let mut ui = crate::app::ui_state::UiState::default();

        terminal
            .draw(|frame| {
                component.render(frame, Rect::new(0, 0, 120, 40), &state, &[], &mut ui);
            })
            .unwrap();

        assert_eq!(ui.home_sidebar_rect.map(|rect| rect.width), Some(40));
    }
}
