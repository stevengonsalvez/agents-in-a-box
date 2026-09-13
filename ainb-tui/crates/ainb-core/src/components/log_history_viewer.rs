// ABOUTME: Log history viewer component for browsing historical application logs
// Displays log files in a list and shows color-coded log entries with filtering
// Supports both JSONL (new) and plain text (legacy) log formats

pub use ainb_app::components::log_history_viewer::*;

use crate::app::ui_state::UiState;
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, List, ListItem, Paragraph},
};

use super::live_logs_stream::{LogEntry, LogEntryLevel};

// Color palette from TUI style guide
const CORNFLOWER_BLUE: Color = Color::Rgb(100, 149, 237);
const GOLD: Color = Color::Rgb(255, 215, 0);
const SELECTION_GREEN: Color = Color::Rgb(100, 200, 100);
const DARK_BG: Color = Color::Rgb(25, 25, 35);
const PANEL_BG: Color = Color::Rgb(30, 30, 40);
const LIST_HIGHLIGHT_BG: Color = Color::Rgb(40, 40, 60);
const SOFT_WHITE: Color = Color::Rgb(220, 220, 230);
const MUTED_GRAY: Color = Color::Rgb(120, 120, 140);
const SUBDUED_BORDER: Color = Color::Rgb(60, 60, 80);
const SELECTION_BG: Color = Color::Rgb(70, 130, 180); // Steel blue for text selection

// Log level colors
const ERROR_RED: Color = Color::Rgb(255, 100, 100);
const WARN_YELLOW: Color = Color::Rgb(255, 200, 100);

const INFO_BLUE: Color = Color::Rgb(100, 149, 237);
const DEBUG_GRAY: Color = Color::Rgb(120, 120, 140);

/// Log history viewer component
pub struct LogHistoryViewerComponent;

impl LogHistoryViewerComponent {
    pub fn new() -> Self {
        Self
    }

    /// Render the log history viewer
    pub fn render(
        &self,
        frame: &mut Frame,
        area: Rect,
        state: &LogHistoryViewerState,
        ui: &mut UiState,
    ) {
        // Main container
        let container = Block::default()
            .title(Line::from(vec![
                Span::styled("📋 ", Style::default().fg(GOLD)),
                Span::styled(
                    "Log History",
                    Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                ),
            ]))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(CORNFLOWER_BLUE))
            .style(Style::default().bg(DARK_BG));

        let inner = container.inner(area);
        frame.render_widget(container, area);

        // Check for error message
        if let Some(error) = &state.error_message {
            let error_text = Paragraph::new(error.as_str())
                .style(Style::default().fg(ERROR_RED))
                .alignment(Alignment::Center);
            frame.render_widget(error_text, inner);
            return;
        }

        // Layout: session list | log entries
        let layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(25), // Session list
                Constraint::Min(40),    // Log entries
            ])
            .split(inner);

        // Store log entries area for mouse coordinate mapping
        ui.log_entries_area = Some(layout[1]);

        self.render_session_list(frame, layout[0], state, ui);
        self.render_log_entries(frame, layout[1], state);
    }

    /// Render the session list
    fn render_session_list(
        &self,
        frame: &mut Frame,
        area: Rect,
        state: &LogHistoryViewerState,
        ui: &mut UiState,
    ) {
        let is_focused = state.focus == LogViewerFocus::SessionList;
        let border_color = if is_focused {
            CORNFLOWER_BLUE
        } else {
            SUBDUED_BORDER
        };

        // Build title with directory path (replace $HOME with ~)
        let dir_display = state
            .log_dir
            .as_ref()
            .map(|p| {
                let path_str = p.display().to_string();
                if let Ok(home) = std::env::var("HOME") {
                    path_str.replace(&home, "~")
                } else {
                    path_str
                }
            })
            .unwrap_or_else(|| "Not configured".to_string());

        let block = Block::default()
            .title(Line::from(vec![
                Span::styled("📁 ", Style::default().fg(GOLD)),
                Span::styled(dir_display, Style::default().fg(MUTED_GRAY)),
            ]))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(border_color))
            .style(Style::default().bg(PANEL_BG));

        if state.sessions.is_empty() {
            let empty_msg = Paragraph::new("No log files found")
                .block(block)
                .style(Style::default().fg(MUTED_GRAY))
                .alignment(Alignment::Center);
            frame.render_widget(empty_msg, area);
            return;
        }

        let items: Vec<ListItem> = state
            .sessions
            .iter()
            .map(|session| {
                let status_indicator = if session.error_count > 0 {
                    Span::styled("● ", Style::default().fg(ERROR_RED))
                } else if session.warn_count > 0 {
                    Span::styled("● ", Style::default().fg(WARN_YELLOW))
                } else {
                    Span::styled("● ", Style::default().fg(SELECTION_GREEN))
                };

                let name = Span::styled(&session.display_name, Style::default().fg(SOFT_WHITE));

                let count = Span::styled(
                    format!(" ({})", session.log_count),
                    Style::default().fg(MUTED_GRAY),
                );

                ListItem::new(Line::from(vec![status_indicator, name, count]))
            })
            .collect();

        let list = List::new(items)
            .block(block)
            .highlight_style(
                Style::default()
                    .bg(LIST_HIGHLIGHT_BG)
                    .fg(SOFT_WHITE)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▶ ");

        // Core owns which row is selected; the widget's own scroll offset is
        // the renderer's, so the paint runs against a mirror of the selection.
        ui.log_history_list.select(state.selected_session);
        frame.render_stateful_widget(list, area, &mut ui.log_history_list);
    }

    /// Render the log entries
    fn render_log_entries(&self, frame: &mut Frame, area: Rect, state: &LogHistoryViewerState) {
        let is_focused = state.focus == LogViewerFocus::LogEntries;
        let border_color = if is_focused {
            CORNFLOWER_BLUE
        } else {
            SUBDUED_BORDER
        };

        let filtered_logs = state.filtered_logs();
        let total_count = state.current_logs.len();
        let filtered_count = filtered_logs.len();

        let title = if filtered_count != total_count {
            format!(
                "Logs [{}/{}] [{}]",
                filtered_count,
                total_count,
                state.filter_level.as_str()
            )
        } else {
            format!("Logs [{}] [{}]", total_count, state.filter_level.as_str())
        };

        let block = Block::default()
            .title(Line::from(vec![Span::styled(
                title,
                Style::default().fg(if is_focused { GOLD } else { MUTED_GRAY }),
            )]))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(border_color))
            .style(Style::default().bg(PANEL_BG));

        if state.selected_log_file.is_none() {
            let msg = Paragraph::new("Select a log file to view logs")
                .block(block)
                .style(Style::default().fg(MUTED_GRAY))
                .alignment(Alignment::Center);
            frame.render_widget(msg, area);
            return;
        }

        if filtered_logs.is_empty() {
            let msg = if state.search_query.is_some() {
                "No logs match the search query"
            } else {
                "No logs at this filter level"
            };
            let empty = Paragraph::new(msg)
                .block(block)
                .style(Style::default().fg(MUTED_GRAY))
                .alignment(Alignment::Center);
            frame.render_widget(empty, area);
            return;
        }

        // Create log lines with selection highlighting
        let log_lines: Vec<Line> = filtered_logs
            .iter()
            .enumerate()
            .map(|(idx, log)| self.format_log_line_with_selection(log, idx, state))
            .collect();

        let paragraph = Paragraph::new(log_lines)
            .block(block)
            .scroll((state.scroll_offset as u16, state.horizontal_scroll as u16)); // Enable vertical and horizontal scrolling

        frame.render_widget(paragraph, area);
    }

    /// Format a single log line with color coding and selection highlighting
    fn format_log_line_with_selection<'a>(
        &self,
        entry: &'a LogEntry,
        line_idx: usize,
        state: &LogHistoryViewerState,
    ) -> Line<'a> {
        let (icon, color) = match entry.level {
            LogEntryLevel::Error => ("❌", ERROR_RED),
            LogEntryLevel::Warn => ("⚠️", WARN_YELLOW),
            LogEntryLevel::Info => ("ℹ️", INFO_BLUE),
            LogEntryLevel::Debug => ("🔍", DEBUG_GRAY),
        };

        let timestamp = entry.timestamp.format("%H:%M:%S").to_string();
        let line_text = format!("{} {} {}", timestamp, icon, entry.message);

        // Check if this line has selection
        // Note: sel_start/sel_end are character (grapheme) offsets, need to convert to byte offsets
        let char_count = line_text.chars().count();
        if let Some((sel_start, sel_end)) = state.get_line_selection_range(line_idx, char_count) {
            // Convert character offsets to byte offsets safely
            let byte_start = char_to_byte_index(&line_text, sel_start);
            let byte_end = char_to_byte_index(&line_text, sel_end);

            // Build spans with selection highlighting
            let mut spans = Vec::new();

            // Before selection
            if byte_start > 0 {
                let before = &line_text[..byte_start];
                spans.push(Span::styled(
                    before.to_string(),
                    self.get_base_style_for_segment(before, &timestamp, icon, color),
                ));
            }

            // Selected portion
            if byte_start < byte_end && byte_end <= line_text.len() {
                let selected = &line_text[byte_start..byte_end];
                spans.push(Span::styled(
                    selected.to_string(),
                    Style::default().fg(Color::Black).bg(SELECTION_BG),
                ));
            }

            // After selection
            if byte_end < line_text.len() {
                let after = &line_text[byte_end..];
                spans.push(Span::styled(
                    after.to_string(),
                    self.get_base_style_for_segment(after, &timestamp, icon, color),
                ));
            }

            Line::from(spans)
        } else {
            // No selection - use original formatting
            Line::from(vec![
                Span::styled(format!("{} ", timestamp), Style::default().fg(MUTED_GRAY)),
                Span::styled(format!("{} ", icon), Style::default().fg(color)),
                Span::styled(entry.message.clone(), Style::default().fg(SOFT_WHITE)),
            ])
        }
    }

    /// Get base style for a text segment (simplified - just returns white for now)
    fn get_base_style_for_segment(
        &self,
        _segment: &str,
        _timestamp: &str,
        _icon: &str,
        _color: Color,
    ) -> Style {
        // Simplified: just return soft white for selected portions
        Style::default().fg(SOFT_WHITE)
    }

    /// Render the help bar at the bottom
    pub fn render_help_bar(&self, frame: &mut Frame, area: Rect, state: &LogHistoryViewerState) {
        let help_items = match state.focus {
            LogViewerFocus::SessionList => vec![
                ("↑↓", "navigate"),
                ("Enter", "load"),
                ("Tab", "logs"),
                ("f", "filter"),
                ("r", "refresh"),
                ("C", "clean"),
                ("Esc", "back"),
            ],
            LogViewerFocus::LogEntries => vec![
                ("↑↓", "scroll"),
                ("←→/⇧🖱", "pan"),
                ("drag", "select"),
                ("y/^C", "copy"),
                ("Tab", "files"),
                ("f", "filter"),
                ("C", "clean"),
                ("Esc", "back"),
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

impl Default for LogHistoryViewerComponent {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_filter_level_cycle() {
        let mut level = LogFilterLevel::All;
        level = level.next();
        assert_eq!(level, LogFilterLevel::Info);
        level = level.next();
        assert_eq!(level, LogFilterLevel::Warn);
        level = level.next();
        assert_eq!(level, LogFilterLevel::Error);
        level = level.next();
        assert_eq!(level, LogFilterLevel::All);
    }

    #[test]
    fn test_filter_matches() {
        assert!(LogFilterLevel::All.matches(LogEntryLevel::Debug));
        assert!(LogFilterLevel::All.matches(LogEntryLevel::Error));

        assert!(!LogFilterLevel::Info.matches(LogEntryLevel::Debug));
        assert!(LogFilterLevel::Info.matches(LogEntryLevel::Info));

        assert!(!LogFilterLevel::Warn.matches(LogEntryLevel::Info));
        assert!(LogFilterLevel::Warn.matches(LogEntryLevel::Warn));
        assert!(LogFilterLevel::Warn.matches(LogEntryLevel::Error));

        assert!(!LogFilterLevel::Error.matches(LogEntryLevel::Warn));
        assert!(LogFilterLevel::Error.matches(LogEntryLevel::Error));
    }

    #[test]
    fn test_state_navigation() {
        let mut state = LogHistoryViewerState::new();
        state.sessions = vec![
            SessionLogSummary {
                filename: "agents-in-a-box-20260107-001310.jsonl".to_string(),
                display_name: "2026-01-07 00:13:10".to_string(),
                log_path: PathBuf::from("/tmp/test1.jsonl"),
                log_count: 10,
                error_count: 0,
                warn_count: 0,
                is_jsonl: true,
            },
            SessionLogSummary {
                filename: "agents-in-a-box-20260107-001410.jsonl".to_string(),
                display_name: "2026-01-07 00:14:10".to_string(),
                log_path: PathBuf::from("/tmp/test2.jsonl"),
                log_count: 20,
                error_count: 1,
                warn_count: 2,
                is_jsonl: true,
            },
        ];
        state.selected_session = Some(0);

        state.select_next_session();
        assert_eq!(state.selected_session, Some(1));

        state.select_next_session();
        assert_eq!(state.selected_session, Some(1)); // Should stay at max

        state.select_prev_session();
        assert_eq!(state.selected_session, Some(0));
    }

    #[test]
    fn test_toggle_focus() {
        let mut state = LogHistoryViewerState::new();
        assert_eq!(state.focus, LogViewerFocus::SessionList);

        state.toggle_focus();
        assert_eq!(state.focus, LogViewerFocus::LogEntries);

        state.toggle_focus();
        assert_eq!(state.focus, LogViewerFocus::SessionList);
    }
}
