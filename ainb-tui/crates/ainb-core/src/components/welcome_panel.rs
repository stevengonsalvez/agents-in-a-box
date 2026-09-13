// ABOUTME: Scrollable welcome panel with markdown content rendering
// Displays getting started info, can be focused and scrolled

pub use ainb_app::components::welcome_panel::*;

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
    },
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

/// Welcome panel component with markdown rendering
pub struct WelcomePanelComponent;

impl WelcomePanelComponent {
    pub fn new() -> Self {
        Self
    }

    /// Paint the panel and report the `(content_height, visible_height)` it
    /// measured. Only a paint can know either — the first is the wrapped line
    /// count, the second the block interior — and the scroll clamp needs both,
    /// so they are returned rather than written back through `state`.
    pub fn render(&self, frame: &mut Frame, area: Rect, state: &WelcomePanelState) -> (u16, u16) {
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
        let content_height = lines.len() as u16;
        let visible_height = inner.height;

        // Create paragraph with scroll
        let paragraph = Paragraph::new(lines)
            .style(Style::default().bg(PANEL_BG))
            .scroll((state.scroll_offset, 0));

        frame.render_widget(paragraph, inner);

        // Render scrollbar if content overflows
        if content_height > visible_height {
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(Some("▲"))
                .end_symbol(Some("▼"))
                .track_symbol(Some("│"))
                .thumb_symbol("█");

            let mut scrollbar_state = ScrollbarState::new(content_height as usize)
                .position(state.scroll_offset as usize)
                .viewport_content_length(visible_height as usize);

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
            let indicator = Paragraph::new(Line::from(vec![Span::styled(
                " ↑↓ scroll ",
                Style::default().fg(ACCENT_CYAN),
            )]))
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

        (content_height, visible_height)
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
            return Line::from(vec![Span::styled(
                "  ─────────────────────────────────────────",
                Style::default().fg(SUBDUED_BORDER),
            )]);
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
        if trimmed.len() > 2 && trimmed.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false)
        {
            if let Some(dot_pos) = trimmed.find(". ") {
                let number = &trimmed[..dot_pos + 1];
                let rest = &trimmed[dot_pos + 2..];
                return Line::from(vec![
                    Span::styled("  ", Style::default()),
                    Span::styled(
                        number.to_string(),
                        Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                    ),
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
