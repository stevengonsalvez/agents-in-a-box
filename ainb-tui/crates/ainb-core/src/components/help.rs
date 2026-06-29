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

        // Capitalised keys are Shift-chords; spell that out ("Shift+A", not a
        // bare "A") so non-developers aren't left guessing. `head` = section
        // title, `row` = a key/description pair with the key column padded so
        // the descriptions line up regardless of key width.
        let head = |t: &str| {
            ListItem::new(t.to_string())
                .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
        };
        let row = |key: &str, desc: &str| ListItem::new(format!("  {key:<12} {desc}"));

        let help_items = vec![
            head("Navigation:"),
            row("↓", "Next session"),
            row("↑", "Previous session"),
            row("←", "Previous workspace"),
            row("→", "Next workspace"),
            row("Home/End", "Top / bottom"),
            row("Shift+↑/↓", "Scroll the selected session's output"),
            ListItem::new(""),
            head("Session Actions:"),
            row("n", "New session (local or remote)"),
            row("a", "Attach — full-screen takeover (Ctrl+B then D to leave)"),
            row("Shift+A", "Attach in a split pane (keeps the session list visible)"),
            row("1-9", "Quick-attach to numbered session"),
            row("Enter", "Resume (stopped) / attach (running)"),
            row("r", "Resume stopped session (tmux)"),
            row("e", "Recreate Boss/Docker session (fresh container)"),
            row("Space", "Select / deselect session"),
            row("d", "Delete session"),
            row("Shift+D", "Delete selected sessions"),
            row("o", "Open in editor"),
            row("$", "Quick shell"),
            row("F2", "Rename SSH / 'Other tmux' session"),
            row("s / Shift+S", "Star / unstar workspace"),
            row("Shift+F", "Cycle session filter (active/stopped/all)"),
            row("f", "Refresh workspaces"),
            row("x", "Cleanup orphaned containers"),
            row("u", "Re-authenticate credentials"),
            ListItem::new(""),
            head("Git Actions:"),
            row("g", "Show git view"),
            row("p", "Commit & push"),
            ListItem::new(""),
            head("Tools:"),
            row("c", "Toggle Claude chat"),
            ListItem::new(""),
            head("Panels (closing returns here):"),
            row("b", "Inbox (Esc closes)"),
            row("i", "Stats / usage analytics (Esc closes)"),
            row("w", "Witr process browser (quit witr to return)"),
            row("k", "Skills browser (Esc closes)"),
            row("m", "Memory / learnings browser (Esc closes)"),
            row("t", "Abtop agent monitor (quit abtop to return)"),
            ListItem::new(""),
            head("Views:"),
            row("Tab", "Switch focus (list <-> preview)"),
            row("Shift+E", "Expand / collapse all workspaces"),
            row("Shift+B", "Collapse / expand the sidebar"),
            ListItem::new(""),
            head("General:"),
            row("? / Shift+H", "Toggle this help"),
            row("q / Esc", "Quit / home"),
            row("Ctrl+C", "Force quit"),
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
