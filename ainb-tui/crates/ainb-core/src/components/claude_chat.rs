// ABOUTME: Claude chat panel component for integrated TUI chat experience

#![allow(dead_code)]

use crate::app::AppState;
use crate::claude::types::{ClaudeMessage, ClaudeRole};
use ratatui::{
    layout::{Constraint, Direction, Layout},
    prelude::*,
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
};

pub struct ClaudeChatComponent {
    scroll_offset: usize,
    #[allow(dead_code)]
    input_cursor_pos: usize,
    max_visible_messages: usize,
}

impl ClaudeChatComponent {
    pub fn new() -> Self {
        Self {
            scroll_offset: 0,
            input_cursor_pos: 0,
            max_visible_messages: 10,
        }
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect, state: &AppState) {
        // Clear the popup area with a background
        let popup_block = Block::default()
            .borders(Borders::ALL)
            .title(" Claude Chat - Press [ESC] to close ")
            .title_style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
            .border_style(Style::default().fg(Color::Cyan))
            .style(Style::default().bg(Color::Black));

        frame.render_widget(popup_block, area);

        // Split the chat area into messages and input (with margin for border)
        let inner_area = area.inner(&ratatui::layout::Margin {
            horizontal: 1,
            vertical: 1,
        });
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(3),    // Message area
                Constraint::Length(4), // Input area
            ])
            .split(inner_area);

        // Render messages area
        self.render_messages(frame, chunks[0], state);

        // Render input area
        self.render_input(frame, chunks[1], state);
    }

    fn render_messages(&mut self, frame: &mut Frame, area: Rect, state: &AppState) {
        let block = Block::default()
            .borders(Borders::BOTTOM)
            .title(" Messages ")
            .title_style(Style::default().fg(Color::Green))
            .border_style(Style::default().fg(Color::Gray));

        // Get messages from state
        let messages = if let Some(chat_state) = &state.claude_chat_state {
            &chat_state.messages
        } else {
            // Show welcome message if no chat state
            frame.render_widget(
                Paragraph::new("Welcome to Claude Chat!\n\nNo active chat session. Type a message below to start chatting.")
                    .block(block)
                    .wrap(Wrap { trim: true })
                    .style(Style::default().fg(Color::Gray)),
                area
            );
            return;
        };

        if messages.is_empty() {
            frame.render_widget(
                Paragraph::new("No messages yet. Type your first message below!")
                    .block(block)
                    .wrap(Wrap { trim: true })
                    .style(Style::default().fg(Color::Gray)),
                area,
            );
            return;
        }

        // Create list items for messages
        let message_items: Vec<ListItem> = messages
            .iter()
            .enumerate()
            .skip(self.scroll_offset)
            .take(self.max_visible_messages)
            .map(|(index, message)| self.format_message(message, index))
            .collect();

        // Show streaming indicator if currently streaming
        let mut items = message_items;
        if let Some(chat_state) = &state.claude_chat_state {
            if chat_state.is_streaming {
                let streaming_text = chat_state
                    .current_streaming_response
                    .as_ref()
                    .map(|s| format!("🤖 {}", s))
                    .unwrap_or_else(|| "🤖 Claude is thinking...".to_string());

                items.push(
                    ListItem::new(streaming_text)
                        .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::ITALIC)),
                );
            }
        }

        let list = List::new(items)
            .block(block)
            .highlight_style(Style::default().add_modifier(Modifier::BOLD));

        frame.render_widget(list, area);

        // Render scroll indicator if there are more messages
        if messages.len() > self.max_visible_messages {
            let scroll_info = format!(
                " {}-{}/{} ",
                self.scroll_offset + 1,
                (self.scroll_offset + self.max_visible_messages).min(messages.len()),
                messages.len()
            );

            let scroll_area = Rect {
                x: area.x + area.width - scroll_info.len() as u16 - 1,
                y: area.y,
                width: scroll_info.len() as u16,
                height: 1,
            };

            frame.render_widget(
                Paragraph::new(scroll_info)
                    .style(Style::default().fg(Color::DarkGray).bg(Color::Black)),
                scroll_area,
            );
        }
    }

    fn format_message(&self, message: &ClaudeMessage, _index: usize) -> ListItem {
        let (icon, color) = match message.role {
            ClaudeRole::User => ("👤", Color::Green),
            ClaudeRole::Assistant => ("🤖", Color::Cyan),
        };

        // Format timestamp if available
        let timestamp = message
            .timestamp
            .map(|ts| format!("[{}] ", ts.format("%H:%M:%S")))
            .unwrap_or_default();

        // Wrap long messages
        let content = if message.content.len() > 100 {
            format!("{}...", &message.content[..97])
        } else {
            message.content.clone()
        };

        let formatted = format!("{}{} {}", timestamp, icon, content);

        ListItem::new(formatted).style(Style::default().fg(color))
    }

    fn render_input(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        let input_text = if let Some(chat_state) = &state.claude_chat_state {
            &chat_state.input_buffer
        } else {
            ""
        };

        let is_streaming =
            state.claude_chat_state.as_ref().map(|s| s.is_streaming).unwrap_or(false);

        let (title, border_color) = if is_streaming {
            (" Input (Claude is responding...) ", Color::Yellow)
        } else {
            (" Type your message (Enter to send) ", Color::Gray)
        };

        let input_block = Block::default()
            .borders(Borders::TOP)
            .title(title)
            .title_style(Style::default().fg(Color::White))
            .border_style(Style::default().fg(border_color));

        // Show cursor position
        let cursor_indicator = if is_streaming { "" } else { "█" };

        let display_text = if input_text.is_empty() && !is_streaming {
            format!("{}Type your message here...", cursor_indicator)
        } else {
            format!("{}{}", input_text, cursor_indicator)
        };

        let input_paragraph = Paragraph::new(display_text)
            .block(input_block)
            .wrap(Wrap { trim: true })
            .style(if is_streaming {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default().fg(Color::White)
            });

        frame.render_widget(input_paragraph, area);

        // Render send button hint
        if !is_streaming && !input_text.is_empty() {
            let hint_area = Rect {
                x: area.x + area.width - 20,
                y: area.y + area.height - 1,
                width: 18,
                height: 1,
            };

            frame.render_widget(
                Paragraph::new(" [Enter] Send ")
                    .style(Style::default().fg(Color::Green).bg(Color::DarkGray)),
                hint_area,
            );
        }
    }

    /// Handle character input
    pub fn handle_char_input(&mut self, _ch: char) {
        // Input handling is managed by AppState
        // This component just renders the state
    }

    /// Handle backspace
    pub fn handle_backspace(&mut self) {
        // Input handling is managed by AppState
        // This component just renders the state
    }

    /// Scroll messages up
    pub fn scroll_up(&mut self) {
        if self.scroll_offset > 0 {
            self.scroll_offset -= 1;
        }
    }

    /// Scroll messages down
    pub fn scroll_down(&mut self, total_messages: usize) {
        if self.scroll_offset + self.max_visible_messages < total_messages {
            self.scroll_offset += 1;
        }
    }

    /// Scroll to bottom (latest messages)
    pub fn scroll_to_bottom(&mut self, total_messages: usize) {
        self.scroll_offset = if total_messages > self.max_visible_messages {
            total_messages - self.max_visible_messages
        } else {
            0
        };
    }

    /// Auto-scroll when new message arrives
    pub fn auto_scroll(&mut self, total_messages: usize) {
        // Only auto-scroll if we're already at or near the bottom
        let near_bottom = self.scroll_offset + self.max_visible_messages + 2 >= total_messages;
        if near_bottom {
            self.scroll_to_bottom(total_messages);
        }
    }

    /// Update maximum visible messages based on area height
    pub fn update_max_visible(&mut self, area_height: u16) {
        // Account for borders and input area
        self.max_visible_messages = ((area_height as usize).saturating_sub(6)).max(3);
    }
}

impl Default for ClaudeChatComponent {
    fn default() -> Self {
        Self::new()
    }
}

// Helper component for displaying connection status
pub struct ClaudeConnectionStatus {
    last_test_time: Option<std::time::Instant>,
    connection_status: ConnectionStatus,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConnectionStatus {
    Unknown,
    Connected,
    Disconnected(String),
    Testing,
}

impl ClaudeConnectionStatus {
    pub fn new() -> Self {
        Self {
            last_test_time: None,
            connection_status: ConnectionStatus::Unknown,
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let status_widget = match &self.connection_status {
            ConnectionStatus::Unknown => Paragraph::new("Claude: Unknown")
                .style(Style::default().fg(Color::Gray))
                .alignment(ratatui::layout::Alignment::Right),
            ConnectionStatus::Connected => Paragraph::new("Claude: Connected ✓")
                .style(Style::default().fg(Color::Green))
                .alignment(ratatui::layout::Alignment::Right),
            ConnectionStatus::Disconnected(reason) => {
                let text = if reason.len() > 30 {
                    format!("Claude: Error ({}...)", &reason[..27])
                } else {
                    format!("Claude: Error ({})", reason)
                };
                Paragraph::new(text)
                    .style(Style::default().fg(Color::Red))
                    .alignment(ratatui::layout::Alignment::Right)
            }
            ConnectionStatus::Testing => Paragraph::new("Claude: Testing...")
                .style(Style::default().fg(Color::Yellow))
                .alignment(ratatui::layout::Alignment::Right),
        };

        frame.render_widget(status_widget, area);
    }

    pub fn update_status(&mut self, status: ConnectionStatus) {
        self.connection_status = status;
        if matches!(
            self.connection_status,
            ConnectionStatus::Connected | ConnectionStatus::Disconnected(_)
        ) {
            self.last_test_time = Some(std::time::Instant::now());
        }
    }

    pub fn is_connected(&self) -> bool {
        matches!(self.connection_status, ConnectionStatus::Connected)
    }

    pub fn should_retest(&self) -> bool {
        // Re-test connection every 5 minutes
        self.last_test_time.map(|last| last.elapsed().as_secs() > 300).unwrap_or(true)
    }
}

impl Default for ClaudeConnectionStatus {
    fn default() -> Self {
        Self::new()
    }
}
