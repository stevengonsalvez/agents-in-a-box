// ABOUTME: Help overlay component displaying keyboard shortcuts and commands

use ratatui::{
    prelude::*,
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Clear, List, ListItem},
};

pub struct HelpComponent;

impl HelpComponent {
    pub fn new() -> Self {
        Self
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let popup_area = self.centered_rect(60, 80, area);

        frame.render_widget(Clear, popup_area);

        let help_items = vec![
            ListItem::new("Navigation:")
                .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            ListItem::new("  ↓          Next session"),
            ListItem::new("  ↑          Previous session"),
            ListItem::new("  ←          Previous workspace"),
            ListItem::new("  →          Next workspace"),
            ListItem::new("  Home/End   Top / bottom"),
            ListItem::new(""),
            ListItem::new("Session Actions:")
                .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            ListItem::new("  n          New session (local or remote)"),
            ListItem::new("  a          Attach to session"),
            ListItem::new("  1-9        Quick-attach to numbered session"),
            ListItem::new("  Enter      Resume (stopped) / attach (running)"),
            ListItem::new("  r          Resume stopped session (tmux)"),
            ListItem::new("  e          Recreate Boss/Docker session (fresh container)"),
            ListItem::new("  Space      Select / deselect session"),
            ListItem::new("  D          Delete selected sessions"),
            ListItem::new("  d          Delete session"),
            ListItem::new("  o          Open in editor"),
            ListItem::new("  $          Quick shell"),
            ListItem::new("  F2         Rename SSH / 'Other tmux' session"),
            ListItem::new("  s          Star / unstar workspace"),
            ListItem::new("  F          Cycle session filter (active/stopped/all)"),
            ListItem::new("  f          Refresh workspaces"),
            ListItem::new("  x          Cleanup orphaned containers"),
            ListItem::new("  A          Re-authenticate credentials"),
            ListItem::new(""),
            ListItem::new("Git Actions:")
                .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            ListItem::new("  g          Show git view"),
            ListItem::new("  p          Commit & push"),
            ListItem::new(""),
            ListItem::new("Tools:")
                .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            ListItem::new("  c          Toggle Claude chat"),
            ListItem::new(""),
            ListItem::new("Panels (closing returns here):")
                .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            ListItem::new("  b          Inbox (Esc closes)"),
            ListItem::new("  i          Stats / usage analytics (Esc closes)"),
            ListItem::new("  w          Witr process browser (quit witr to return)"),
            ListItem::new("  k          Skills browser (Esc closes)"),
            ListItem::new(""),
            ListItem::new("Views:")
                .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            ListItem::new("  Tab        Switch focus / views"),
            ListItem::new("  E          Expand / collapse all workspaces"),
            ListItem::new(""),
            ListItem::new("General:")
                .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            ListItem::new("  ?/H        Toggle this help"),
            ListItem::new("  q/Esc      Quit / home"),
            ListItem::new("  Ctrl+C     Force quit"),
        ];

        let help_list = List::new(help_items).block(
            Block::default()
                .title("Help - Press ? or Esc to close")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        );

        frame.render_widget(help_list, popup_area);
    }

    fn centered_rect(&self, percent_x: u16, percent_y: u16, r: Rect) -> Rect {
        let popup_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Percentage((100 - percent_y) / 2),
                Constraint::Percentage(percent_y),
                Constraint::Percentage((100 - percent_y) / 2),
            ])
            .split(r);

        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage((100 - percent_x) / 2),
                Constraint::Percentage(percent_x),
                Constraint::Percentage((100 - percent_x) / 2),
            ])
            .split(popup_layout[1])[1]
    }
}

impl Default for HelpComponent {
    fn default() -> Self {
        Self::new()
    }
}
