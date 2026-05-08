// ABOUTME: Scrollable welcome panel with markdown content rendering
// Displays getting started info, can be focused and scrolled

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState},
    Frame,
};

// Color palette from TUI style guide
const CORNFLOWER_BLUE: Color = Color::Rgb(100, 149, 237);
const GOLD: Color = Color::Rgb(255, 215, 0);
const SELECTION_GREEN: Color = Color::Rgb(100, 200, 100);
const PANEL_BG: Color = Color::Rgb(30, 30, 40);
const SOFT_WHITE: Color = Color::Rgb(220, 220, 230);
const MUTED_GRAY: Color = Color::Rgb(120, 120, 140);
const SUBDUED_BORDER: Color = Color::Rgb(60, 60, 80);
const ACCENT_CYAN: Color = Color::Rgb(80, 200, 220);

/// Default markdown content for the welcome panel
pub const DEFAULT_WELCOME_CONTENT: &str = r#"# Welcome to AINB

**AI-powered development environment manager** for Claude Code agents
running in isolated git worktrees.

---

## Quick Start

1. **Select an Agent** `[a]` - Configure your Claude instance
2. **Browse Catalog** `[c]` - Find project templates
3. **Launch Sessions** `[s]` - Start working in isolated worktrees

Press **Enter** to activate selection, **↑↓** to navigate the sidebar.

---

## Architecture

```
┌─────────────┐         ┌──────────────┐
│    AINB     │────────▶│   Worktree   │
│     TUI     │         │   + tmux     │
└─────────────┘         └──────────────┘
       │                       │
       ▼                       ▼
┌─────────────┐         ┌──────────────┐
│   Config    │         │    Claude    │
│   Presets   │         │     Code     │
└─────────────┘         └──────────────┘
```

Each session runs in an **isolated git worktree** with its own:
- Branch for changes
- tmux session for terminal access
- Claude Code instance

---

## Key Features

- **Multi-Agent Support** - Run multiple Claude sessions in parallel
- **Git Worktrees** - Each session gets its own branch
- **tmux Integration** - Full terminal access to sessions
- **Session Management** - Start, stop, attach, and monitor

---

## Tips

💡 **Tip:** Press `?` anytime for context-sensitive help

💡 **Tip:** Use `s` to jump directly to Sessions

💡 **Tip:** Each worktree is completely isolated - experiment freely!

---

*Press Tab to switch focus • ↑↓ to scroll • q to quit*
"#;

/// Welcome panel state with scroll position
#[derive(Debug)]
pub struct WelcomePanelState {
    /// Whether the panel is focused
    pub is_focused: bool,
    /// Current scroll position (line offset)
    pub scroll_offset: u16,
    /// Total content height (set during render)
    pub content_height: u16,
    /// Visible height (set during render)
    pub visible_height: u16,
    /// The markdown content to display
    pub content: String,
}

impl WelcomePanelState {
    pub fn new() -> Self {
        Self {
            is_focused: false,
            scroll_offset: 0,
            content_height: 0,
            visible_height: 0,
            content: DEFAULT_WELCOME_CONTENT.to_string(),
        }
    }

    /// Scroll up by one line
    pub fn scroll_up(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_sub(1);
    }

    /// Scroll down by one line
    pub fn scroll_down(&mut self) {
        let max_scroll = self.content_height.saturating_sub(self.visible_height);
        if self.scroll_offset < max_scroll {
            self.scroll_offset += 1;
        }
    }

    /// Scroll up by a page
    pub fn page_up(&mut self) {
        let page_size = self.visible_height.saturating_sub(2);
        self.scroll_offset = self.scroll_offset.saturating_sub(page_size);
    }

    /// Scroll down by a page
    pub fn page_down(&mut self) {
        let page_size = self.visible_height.saturating_sub(2);
        let max_scroll = self.content_height.saturating_sub(self.visible_height);
        self.scroll_offset = (self.scroll_offset + page_size).min(max_scroll);
    }

    /// Set custom content
    pub fn set_content(&mut self, content: String) {
        self.content = content;
        self.scroll_offset = 0;
    }

    /// Copy the welcome panel content to the system clipboard
    pub fn copy_content_to_clipboard(&self) -> Result<(), String> {
        use arboard::Clipboard;
        let mut clipboard = Clipboard::new().map_err(|e| e.to_string())?;
        clipboard.set_text(&self.content).map_err(|e| e.to_string())?;
        Ok(())
    }
}

impl Default for WelcomePanelState {
    fn default() -> Self {
        Self::new()
    }
}

/// Welcome panel component with markdown rendering
pub struct WelcomePanelComponent;

impl WelcomePanelComponent {
    pub fn new() -> Self {
        Self
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, state: &mut WelcomePanelState) {
        // Border color based on focus
        let border_color = if state.is_focused {
            CORNFLOWER_BLUE
        } else {
            SUBDUED_BORDER
        };

        let title_style = if state.is_focused {
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(MUTED_GRAY)
        };

        // Main container block
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(border_color))
            .style(Style::default().bg(PANEL_BG))
            .title(Line::from(vec![
                Span::styled(" ", Style::default()),
                Span::styled("📖", Style::default()),
                Span::styled(" Getting Started ", title_style),
            ]));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        // Parse and render markdown content
        let lines = self.parse_markdown(&state.content);
        state.content_height = lines.len() as u16;
        state.visible_height = inner.height;

        // Create paragraph with scroll
        let paragraph = Paragraph::new(lines)
            .style(Style::default().bg(PANEL_BG))
            .scroll((state.scroll_offset, 0));

        frame.render_widget(paragraph, inner);

        // Render scrollbar if content overflows
        if state.content_height > state.visible_height {
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(Some("▲"))
                .end_symbol(Some("▼"))
                .track_symbol(Some("│"))
                .thumb_symbol("█");

            let mut scrollbar_state = ScrollbarState::new(state.content_height as usize)
                .position(state.scroll_offset as usize)
                .viewport_content_length(state.visible_height as usize);

            // Scrollbar area (right edge of inner)
            let scrollbar_area = Rect {
                x: inner.x + inner.width.saturating_sub(1),
                y: inner.y,
                width: 1,
                height: inner.height,
            };

            frame.render_stateful_widget(scrollbar, scrollbar_area, &mut scrollbar_state);
        }

        // Show focus indicator
        if state.is_focused {
            let indicator = Paragraph::new(Line::from(vec![
                Span::styled(" ↑↓ scroll ", Style::default().fg(ACCENT_CYAN)),
            ]))
            .style(Style::default().bg(PANEL_BG));

            // Position at bottom right of the block
            if area.height > 2 {
                let indicator_area = Rect {
                    x: area.x + area.width.saturating_sub(14),
                    y: area.y + area.height - 1,
                    width: 12,
                    height: 1,
                };
                frame.render_widget(indicator, indicator_area);
            }
        }
    }

    /// Parse markdown-like content into styled lines
    fn parse_markdown(&self, content: &str) -> Vec<Line<'static>> {
        let mut lines: Vec<Line<'static>> = Vec::new();

        for line in content.lines() {
            let styled_line = self.parse_line(line);
            lines.push(styled_line);
        }

        lines
    }

    /// Parse a single line of markdown
    fn parse_line(&self, line: &str) -> Line<'static> {
        let trimmed = line.trim_start();

        // Headers
        if trimmed.starts_with("# ") {
            return Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(
                    trimmed[2..].to_string(),
                    Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                ),
            ]);
        }
        if trimmed.starts_with("## ") {
            return Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(
                    trimmed[3..].to_string(),
                    Style::default().fg(ACCENT_CYAN).add_modifier(Modifier::BOLD),
                ),
            ]);
        }
        if trimmed.starts_with("### ") {
            return Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(
                    trimmed[4..].to_string(),
                    Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD),
                ),
            ]);
        }

        // Horizontal rule
        if trimmed == "---" {
            return Line::from(vec![
                Span::styled("  ─────────────────────────────────────────", Style::default().fg(SUBDUED_BORDER)),
            ]);
        }

        // Code blocks (simplified - just style the whole line)
        if trimmed.starts_with("```") {
            return Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(trimmed.to_string(), Style::default().fg(MUTED_GRAY)),
            ]);
        }

        // List items
        if trimmed.starts_with("- ") {
            let rest = &trimmed[2..];
            return Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled("• ", Style::default().fg(SELECTION_GREEN)),
                Span::styled(self.style_inline(rest), Style::default().fg(SOFT_WHITE)),
            ]);
        }

        // Numbered list items
        if trimmed.len() > 2 && trimmed.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) {
            if let Some(dot_pos) = trimmed.find(". ") {
                let number = &trimmed[..dot_pos + 1];
                let rest = &trimmed[dot_pos + 2..];
                return Line::from(vec![
                    Span::styled("  ", Style::default()),
                    Span::styled(number.to_string(), Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
                    Span::styled(" ", Style::default()),
                    Span::styled(self.style_inline(rest), Style::default().fg(SOFT_WHITE)),
                ]);
            }
        }

        // Tip lines
        if trimmed.starts_with("💡") {
            return Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(trimmed.to_string(), Style::default().fg(GOLD)),
            ]);
        }

        // Empty lines
        if trimmed.is_empty() {
            return Line::from("");
        }

        // Regular text with inline styling
        Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled(self.style_inline(line), Style::default().fg(SOFT_WHITE)),
        ])
    }

    /// Handle inline markdown (bold, code, etc.) - simplified version
    fn style_inline(&self, text: &str) -> String {
        // For now, just return the text as-is
        // A full implementation would parse **bold**, `code`, etc.
        text.to_string()
    }
}

impl Default for WelcomePanelComponent {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_welcome_panel_state() {
        let state = WelcomePanelState::new();
        assert!(!state.is_focused);
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn test_scroll_navigation() {
        let mut state = WelcomePanelState::new();
        state.content_height = 100;
        state.visible_height = 20;

        state.scroll_down();
        assert_eq!(state.scroll_offset, 1);

        state.scroll_up();
        assert_eq!(state.scroll_offset, 0);

        // Can't scroll above 0
        state.scroll_up();
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn test_page_navigation() {
        let mut state = WelcomePanelState::new();
        state.content_height = 100;
        state.visible_height = 20;

        state.page_down();
        assert_eq!(state.scroll_offset, 18); // visible_height - 2

        state.page_up();
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn test_custom_content() {
        let mut state = WelcomePanelState::new();
        state.scroll_offset = 10;

        state.set_content("# New Content".to_string());

        assert_eq!(state.content, "# New Content");
        assert_eq!(state.scroll_offset, 0); // Reset on content change
    }
}
