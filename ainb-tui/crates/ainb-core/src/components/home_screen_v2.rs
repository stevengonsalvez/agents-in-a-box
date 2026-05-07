// ABOUTME: Refreshed home screen component with premium sidebar and welcome panel
// This is the v2 design for AINB 2.0, featuring:
// - Animated "Boxy" mascot in the header
// - Premium VS Code/Discord-style sidebar navigation with shortcuts
// - Welcome panel with getting started guide and architecture overview

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph},
    Frame,
};

use super::mascot::{render_mascot, MascotAnimation};
use super::sidebar::{SidebarComponent, SidebarState};
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
}

impl HomeScreenV2State {
    pub fn new() -> Self {
        let mut state = Self {
            focus: HomeScreenFocus::Sidebar,
            sidebar: SidebarState::new(),
            welcome: WelcomePanelState::new(),
            mascot: MascotAnimation::new(),
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
}

impl Default for HomeScreenV2State {
    fn default() -> Self {
        Self::new()
    }
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
    pub fn render(&self, frame: &mut Frame, area: Rect, state: &mut HomeScreenV2State, workspaces: &[Workspace]) {
        self.render_with_loading(frame, area, state, workspaces, false)
    }

    /// Main render function with loading indicator support
    pub fn render_with_loading(&self, frame: &mut Frame, area: Rect, state: &mut HomeScreenV2State, workspaces: &[Workspace], is_loading: bool) {
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
    fn render_full_layout(&self, frame: &mut Frame, area: Rect, state: &mut HomeScreenV2State, workspaces: &[Workspace]) {
        self.render_full_layout_with_loading(frame, area, state, workspaces, false)
    }

    /// Full layout with loading indicator support
    fn render_full_layout_with_loading(&self, frame: &mut Frame, area: Rect, state: &mut HomeScreenV2State, workspaces: &[Workspace], is_loading: bool) {
        // Vertical layout: header, main content, recent activity, help bar
        let main_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(7),  // Header with mascot
                Constraint::Min(20),    // Main content (sidebar + welcome)
                Constraint::Length(3),  // Recent activity
                Constraint::Length(2),  // Help bar
            ])
            .split(area);

        // Render header with mascot
        self.render_header(frame, main_layout[0], state);

        // Horizontal split: sidebar | welcome panel
        let content_layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(26), // Sidebar (wider for shortcuts)
                Constraint::Min(50),    // Welcome panel
            ])
            .split(main_layout[1]);

        // Render sidebar
        self.sidebar.render(frame, content_layout[0], &state.sidebar);

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
            .unwrap_or(0) % spinner_frames.len() as u128) as usize;
        let spinner = spinner_frames[frame_idx];

        let loading_text = format!("{} Loading sessions...", spinner);
        let loading = Paragraph::new(loading_text)
            .style(Style::default().fg(GOLD))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(CORNFLOWER_BLUE))
                    .style(Style::default().bg(PANEL_BG))
            );

        frame.render_widget(loading, area);
    }

    /// Compact layout for smaller terminals
    fn render_compact_layout(&self, frame: &mut Frame, area: Rect, state: &mut HomeScreenV2State, workspaces: &[Workspace]) {
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(4),  // Compact header
                Constraint::Min(16),    // Content
                Constraint::Length(2),  // Recent activity
                Constraint::Length(2),  // Help bar
            ])
            .split(area);

        self.render_compact_header(frame, layout[0], state);

        // Horizontal split: sidebar | welcome
        let content_layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(24), // Sidebar
                Constraint::Min(40),    // Welcome panel
            ])
            .split(layout[1]);

        self.sidebar.render(frame, content_layout[0], &state.sidebar);
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
                Constraint::Length(2),  // Title
                Constraint::Min(10),    // Sidebar list
                Constraint::Length(2),  // Help
            ])
            .split(area);

        let title = Paragraph::new(Line::from(vec![
            Span::styled(" AINB ", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
            Span::styled("- Agents in a Box", Style::default().fg(SOFT_WHITE)),
        ]))
        .alignment(Alignment::Center)
        .style(Style::default().bg(DARK_BG));

        frame.render_widget(title, layout[0]);

        // Render sidebar directly
        self.sidebar.render(frame, layout[1], &state.sidebar);

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
            Span::styled("A I N B", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
            Span::styled("  -  ", Style::default().fg(SUBDUED_BORDER)),
            Span::styled("Agents in a Box", Style::default().fg(SOFT_WHITE)),
        ]))
        .style(Style::default().bg(PANEL_BG));

        let subtitle = Paragraph::new(Line::from(vec![
            Span::styled("Your AI-Powered Development Hub", Style::default().fg(MUTED_GRAY)),
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
                Constraint::Length(8),  // Mini mascot
                Constraint::Min(30),    // Title
            ])
            .split(inner);

        render_mascot(frame, header_layout[0], &mascot_copy);

        let title = Paragraph::new(Line::from(vec![
            Span::styled("AINB", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
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
                    Span::styled(session.branch_name.clone(), Style::default().fg(CORNFLOWER_BLUE)),
                    Span::styled("  ", Style::default()),
                    Span::styled(status_icon, Style::default().fg(status_color)),
                    Span::styled(
                        if session.status.is_running() { " Running" } else { " Stopped" },
                        Style::default().fg(status_color),
                    ),
                ])
            } else {
                Line::from(vec![
                    Span::styled("   No recent sessions", Style::default().fg(MUTED_GRAY)),
                ])
            }
        } else {
            Line::from(vec![
                Span::styled("   No workspaces configured - press ", Style::default().fg(MUTED_GRAY)),
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
            spans.push(Span::styled(*key, Style::default().fg(GOLD).add_modifier(Modifier::BOLD)));
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
}
