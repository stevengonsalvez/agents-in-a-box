// ABOUTME: Generic popup for config screen settings (choice selection and text input)
// Follows the same pattern as auth_provider_popup.rs

pub use ainb_app::components::config_popup::*;

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};

// Color palette from TUI style guide
const CORNFLOWER_BLUE: Color = Color::Rgb(100, 149, 237);
const GOLD: Color = Color::Rgb(255, 215, 0);
const SELECTION_GREEN: Color = Color::Rgb(100, 200, 100);
const PANEL_BG: Color = Color::Rgb(30, 30, 40);
const LIST_HIGHLIGHT_BG: Color = Color::Rgb(40, 40, 60);
const SOFT_WHITE: Color = Color::Rgb(220, 220, 230);
const MUTED_GRAY: Color = Color::Rgb(120, 120, 140);

/// Config popup component
pub struct ConfigPopupComponent;

impl ConfigPopupComponent {
    pub fn new() -> Self {
        Self
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, state: &ConfigPopupState) {
        if !state.show_popup {
            return;
        }

        // Calculate popup size based on type
        let (popup_width, popup_height) = match &state.popup_type {
            ConfigPopupType::Choice { options, .. } => {
                let height = (options.len() * 2 + 6).min(20) as u16;
                let width = 50u16.min(area.width - 4);
                (width, height)
            }
            ConfigPopupType::Boolean { .. } => (40, 10),
            ConfigPopupType::TextInput { .. } | ConfigPopupType::NumberInput { .. } => (50, 10),
        };

        let popup_x = area.x + (area.width.saturating_sub(popup_width)) / 2;
        let popup_y = area.y + (area.height.saturating_sub(popup_height)) / 2;
        let popup_area = Rect::new(popup_x, popup_y, popup_width, popup_height);

        // Clear background
        frame.render_widget(Clear, popup_area);

        // Main block
        let block = Block::default()
            .title(Span::styled(
                format!(" {} ", state.title),
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(CORNFLOWER_BLUE))
            .style(Style::default().bg(PANEL_BG));

        let inner = block.inner(popup_area);
        frame.render_widget(block, popup_area);

        // Layout
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2), // Description
                Constraint::Min(3),    // Content
                Constraint::Length(2), // Help bar
            ])
            .split(inner);

        // Description, plus a greyed paste hint on the second line for
        // text-entry popups so the Ctrl+V clipboard paste is discoverable
        // in the popup itself — not only in the bottom help bar.
        let mut desc_lines = vec![Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled(&state.description, Style::default().fg(MUTED_GRAY)),
        ])];
        // `is_text_entry()` already encodes "text or number popup" (show_popup
        // is guaranteed true here, past the early return above).
        if state.is_text_entry() {
            desc_lines.push(Line::from(vec![Span::styled(
                "  Ctrl+V to paste",
                Style::default().fg(MUTED_GRAY),
            )]));
        }
        let desc = Paragraph::new(desc_lines).style(Style::default().bg(PANEL_BG));
        frame.render_widget(desc, layout[0]);

        // Content based on type
        match &state.popup_type {
            ConfigPopupType::Choice {
                options,
                selected_index,
            } => {
                self.render_choice(frame, layout[1], options, *selected_index);
            }
            ConfigPopupType::Boolean { value } => {
                self.render_boolean(frame, layout[1], *value);
            }
            ConfigPopupType::TextInput {
                value,
                cursor_position,
            } => {
                self.render_text_input(frame, layout[1], value, *cursor_position);
            }
            ConfigPopupType::NumberInput { input_buffer, .. } => {
                self.render_number_input(frame, layout[1], input_buffer);
            }
        }

        // Help bar
        self.render_help_bar(frame, layout[2], state);
    }

    fn render_choice(&self, frame: &mut Frame, area: Rect, options: &[String], selected: usize) {
        let mut lines = Vec::new();

        for (i, option) in options.iter().enumerate() {
            let is_selected = i == selected;
            let indicator = if is_selected { "▶ " } else { "  " };

            let style = if is_selected {
                Style::default()
                    .fg(SELECTION_GREEN)
                    .bg(LIST_HIGHLIGHT_BG)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(SOFT_WHITE)
            };

            lines.push(Line::from(vec![
                Span::styled(indicator, Style::default().fg(SELECTION_GREEN)),
                Span::styled(option, style),
            ]));
        }

        let paragraph = Paragraph::new(lines).style(Style::default().bg(PANEL_BG));
        frame.render_widget(paragraph, area);
    }

    fn render_boolean(&self, frame: &mut Frame, area: Rect, value: bool) {
        let lines = vec![
            Line::from(vec![
                Span::styled(
                    if value { "▶ " } else { "  " },
                    Style::default().fg(SELECTION_GREEN),
                ),
                Span::styled(
                    "✓ Enabled",
                    if value {
                        Style::default()
                            .fg(SELECTION_GREEN)
                            .bg(LIST_HIGHLIGHT_BG)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(SOFT_WHITE)
                    },
                ),
            ]),
            Line::from(vec![
                Span::styled(
                    if !value { "▶ " } else { "  " },
                    Style::default().fg(SELECTION_GREEN),
                ),
                Span::styled(
                    "✗ Disabled",
                    if !value {
                        Style::default()
                            .fg(SELECTION_GREEN)
                            .bg(LIST_HIGHLIGHT_BG)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(SOFT_WHITE)
                    },
                ),
            ]),
        ];

        let paragraph = Paragraph::new(lines).style(Style::default().bg(PANEL_BG));
        frame.render_widget(paragraph, area);
    }

    fn render_text_input(&self, frame: &mut Frame, area: Rect, value: &str, cursor_pos: usize) {
        let input_block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(GOLD))
            .style(Style::default().bg(LIST_HIGHLIGHT_BG));

        let inner = input_block.inner(area);
        frame.render_widget(input_block, area);

        // Show value with cursor
        let display = if cursor_pos >= value.len() {
            format!("{}|", value)
        } else {
            let (before, after) = value.split_at(cursor_pos);
            format!("{}|{}", before, after)
        };

        let text = Paragraph::new(Line::from(vec![Span::styled(
            &display,
            Style::default().fg(SOFT_WHITE),
        )]))
        .style(Style::default().bg(LIST_HIGHLIGHT_BG));

        frame.render_widget(text, inner);
    }

    fn render_number_input(&self, frame: &mut Frame, area: Rect, value: &str) {
        let input_block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(GOLD))
            .style(Style::default().bg(LIST_HIGHLIGHT_BG));

        let inner = input_block.inner(area);
        frame.render_widget(input_block, area);

        let text = Paragraph::new(Line::from(vec![Span::styled(
            format!("{}|", value),
            Style::default().fg(SOFT_WHITE),
        )]))
        .style(Style::default().bg(LIST_HIGHLIGHT_BG));

        frame.render_widget(text, inner);
    }

    fn render_help_bar(&self, frame: &mut Frame, area: Rect, state: &ConfigPopupState) {
        let help_items = match &state.popup_type {
            ConfigPopupType::Choice { .. } | ConfigPopupType::Boolean { .. } => {
                vec![("↑↓", "select"), ("Enter", "confirm"), ("Esc", "cancel")]
            }
            ConfigPopupType::TextInput { .. } => {
                vec![
                    ("←→", "move"),
                    ("^V", "paste"),
                    ("Enter", "save"),
                    ("Esc", "cancel"),
                ]
            }
            ConfigPopupType::NumberInput { .. } => {
                vec![("Enter", "save"), ("Esc", "cancel")]
            }
        };

        let mut spans = Vec::new();
        spans.push(Span::styled("  ", Style::default()));

        for (i, (key, desc)) in help_items.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(" | ", Style::default().fg(MUTED_GRAY)));
            }
            spans.push(Span::styled(
                *key,
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(" ", Style::default()));
            spans.push(Span::styled(*desc, Style::default().fg(MUTED_GRAY)));
        }

        let help = Paragraph::new(Line::from(spans)).style(Style::default().bg(PANEL_BG));
        frame.render_widget(help, area);
    }
}

impl Default for ConfigPopupComponent {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_value(state: &ConfigPopupState) -> (&str, usize) {
        match &state.popup_type {
            ConfigPopupType::TextInput {
                value,
                cursor_position,
            } => (value.as_str(), *cursor_position),
            other => panic!("expected TextInput, got {other:?}"),
        }
    }

    fn open_path() -> ConfigPopupState {
        let mut s = ConfigPopupState::new();
        s.open_text(
            "Default Workspace",
            "dir",
            "default_workspace",
            "/Users/me/git",
        );
        s
    }

    #[test]
    fn open_text_places_cursor_at_end() {
        let s = open_path();
        let (value, cursor) = text_value(&s);
        assert_eq!(value, "/Users/me/git");
        assert_eq!(cursor, value.len());
    }

    #[test]
    fn paste_inserts_at_cursor() {
        let mut s = open_path();
        // Cursor is at end; paste appends.
        s.insert_str("/extra");
        let (value, cursor) = text_value(&s);
        assert_eq!(value, "/Users/me/git/extra");
        assert_eq!(cursor, value.len());
    }

    #[test]
    fn paste_in_the_middle_after_cursor_move() {
        let mut s = open_path();
        s.cursor_home();
        s.insert_str("~"); // user replaces leading segment manually later
        let (value, cursor) = text_value(&s);
        assert_eq!(value, "~/Users/me/git");
        assert_eq!(cursor, 1);
    }

    #[test]
    fn paste_strips_newlines_and_control_chars() {
        let mut s = ConfigPopupState::new();
        s.open_text("Default Workspace", "dir", "default_workspace", "");
        s.insert_str("/Users/me/projects\n");
        s.insert_str("\t/more");
        let (value, _) = text_value(&s);
        assert_eq!(value, "/Users/me/projects/more");
    }

    #[test]
    fn cursor_left_right_home_end() {
        let mut s = open_path();
        let end = "/Users/me/git".len();
        s.cursor_home();
        assert_eq!(text_value(&s).1, 0);
        s.cursor_right();
        assert_eq!(text_value(&s).1, 1);
        s.cursor_end();
        assert_eq!(text_value(&s).1, end);
        s.cursor_left();
        assert_eq!(text_value(&s).1, end - 1);
    }

    #[test]
    fn backspace_and_delete_at_cursor() {
        let mut s = ConfigPopupState::new();
        s.open_text("t", "d", "k", "abc");
        s.cursor_home();
        s.delete_forward(); // removes 'a'
        assert_eq!(text_value(&s), ("bc", 0));
        s.cursor_end();
        s.backspace(); // removes 'c'
        assert_eq!(text_value(&s), ("b", 1));
    }

    #[test]
    fn editing_respects_multibyte_chars() {
        let mut s = ConfigPopupState::new();
        s.open_text("t", "d", "k", "");
        s.input_char('é'); // 2 bytes
        s.input_char('x');
        let (value, cursor) = text_value(&s);
        assert_eq!(value, "éx");
        assert_eq!(cursor, value.len());
        s.cursor_home();
        s.cursor_right(); // should land after 'é', not mid-codepoint
        assert_eq!(text_value(&s).1, "é".len());
        s.backspace(); // removes whole 'é'
        assert_eq!(text_value(&s), ("x", 0));
    }

    #[test]
    fn delete_forward_removes_whole_multibyte_char() {
        // Forward-delete must remove the entire codepoint under the cursor;
        // a byte-only remove would panic on a non-boundary index.
        let mut s = ConfigPopupState::new();
        s.open_text("t", "d", "k", "é/x"); // 'é' is 2 bytes
        s.cursor_home();
        s.delete_forward(); // removes whole 'é'
        assert_eq!(text_value(&s), ("/x", 0));
        s.delete_forward(); // removes '/'
        assert_eq!(text_value(&s), ("x", 0));
    }

    #[test]
    fn number_input_paste_keeps_only_digits() {
        let mut s = ConfigPopupState::new();
        s.open_number("Max", "d", "max_repositories", 500);
        s.insert_str("12ab3");
        match &s.popup_type {
            ConfigPopupType::NumberInput { input_buffer, .. } => {
                assert_eq!(input_buffer, "500123");
            }
            other => panic!("expected NumberInput, got {other:?}"),
        }
    }

    fn render_to_text(state: &ConfigPopupState, w: u16, h: u16) -> String {
        let component = ConfigPopupComponent::new();
        let backend = ratatui::backend::TestBackend::new(w, h);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| component.render(f, Rect::new(0, 0, w, h), state)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer.get(x, y).symbol().chars().next().unwrap_or(' '))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn text_popup_renders_ctrl_v_paste_hint() {
        let mut s = ConfigPopupState::new();
        s.open_text(
            "Default Workspace",
            "Default directory for new sessions",
            "default_workspace",
            "/Users/me/git",
        );
        let text = render_to_text(&s, 60, 14);
        assert!(
            text.contains("Ctrl+V to paste"),
            "text popup must show the greyed paste hint:\n{text}"
        );
    }

    #[test]
    fn number_popup_renders_ctrl_v_paste_hint() {
        let mut s = ConfigPopupState::new();
        s.open_number("Max Repositories", "Maximum repos", "max_repositories", 500);
        let text = render_to_text(&s, 60, 14);
        assert!(
            text.contains("Ctrl+V to paste"),
            "number popup must show the greyed paste hint:\n{text}"
        );
    }

    #[test]
    fn choice_popup_has_no_paste_hint() {
        let mut s = ConfigPopupState::new();
        s.open_choice(
            "Theme",
            "Pick a theme",
            "theme",
            vec!["Dark".to_string(), "Light".to_string()],
            0,
        );
        let text = render_to_text(&s, 60, 14);
        assert!(
            !text.contains("Ctrl+V to paste"),
            "choice popup must NOT show the paste hint:\n{text}"
        );
    }
}
