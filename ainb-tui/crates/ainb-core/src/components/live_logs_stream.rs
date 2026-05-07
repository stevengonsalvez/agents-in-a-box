// ABOUTME: Live Docker log streaming component for real-time container monitoring

#![allow(dead_code)]

use super::log_formatter_simple::{FormatConfig, SimpleLogFormatter};
use crate::app::AppState;
use ratatui::{
    prelude::*,
    style::{Color, Style},
    text::Line,
    widgets::{Block, Borders, Paragraph, Wrap},
};

pub struct LiveLogsStreamComponent {
    auto_scroll: bool,
    scroll_offset: usize,
    max_visible_lines: usize,
    show_timestamps: bool,
    filter_level: LogLevel,
    log_formatter: SimpleLogFormatter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    All,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    fn as_str(&self) -> &'static str {
        match self {
            LogLevel::All => "ALL",
            LogLevel::Info => "INFO",
            LogLevel::Warn => "WARN",
            LogLevel::Error => "ERROR",
        }
    }

    fn next(&self) -> Self {
        match self {
            LogLevel::All => LogLevel::Info,
            LogLevel::Info => LogLevel::Warn,
            LogLevel::Warn => LogLevel::Error,
            LogLevel::Error => LogLevel::All,
        }
    }
}

impl LiveLogsStreamComponent {
    pub fn new() -> Self {
        let format_config = FormatConfig {
            show_timestamps: false,
            use_relative_time: true,
            show_source_badges: true,
            compact_mode: false,
            max_message_length: None,
        };

        Self {
            auto_scroll: true,
            scroll_offset: 0,
            max_visible_lines: 20,
            show_timestamps: false,
            filter_level: LogLevel::All,
            log_formatter: SimpleLogFormatter::new(format_config),
        }
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect, state: &AppState) {
        // Get logs from the selected session
        let session_logs = self.get_session_logs(state);

        // Filter logs based on level
        let filtered_logs = self.filter_logs(&session_logs);

        let title = self.build_title(state, filtered_logs.len(), session_logs.len());

        // Show focus indicator
        use crate::app::state::FocusedPane;
        let (border_color, title_color) = match state.focused_pane {
            FocusedPane::LiveLogs => (Color::Cyan, Color::Yellow), // Focused
            FocusedPane::Sessions => (Color::Gray, Color::Blue),   // Not focused
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .title(title)
            .title_style(Style::default().fg(title_color))
            .border_style(Style::default().fg(border_color));

        if filtered_logs.is_empty() {
            let empty_message = match self.filter_level {
                LogLevel::All => {
                    "No logs available\n\nLogs will appear here when containers are active."
                }
                _ => &format!(
                    "No {} level logs\n\nAdjust filter level with 'f' key.",
                    self.filter_level.as_str().to_lowercase()
                ),
            };

            frame.render_widget(
                Paragraph::new(empty_message)
                    .block(block)
                    .style(Style::default().fg(Color::Gray))
                    .alignment(Alignment::Center),
                area,
            );
            return;
        }

        // Get scroll position before borrowing self mutably
        let scroll_pos = self.get_scroll_position(&filtered_logs);

        // Create formatted log lines using the beautiful formatter
        let log_lines = self.create_formatted_log_lines(&filtered_logs);

        let paragraph = Paragraph::new(log_lines)
            .block(block)
            .wrap(Wrap { trim: false })
            .scroll((scroll_pos as u16, 0));

        frame.render_widget(paragraph, area);

        // Render controls hint
        self.render_controls_hint(frame, area);

        // Update max visible lines based on actual area
        self.max_visible_lines = (area.height.saturating_sub(4)) as usize;
    }

    fn get_session_logs(&self, state: &AppState) -> Vec<LogEntry> {
        // Get logs from currently selected session or all active sessions
        if let Some(session) = state.selected_session() {
            // Get logs for specific session
            state.live_logs.get(&session.id).cloned().unwrap_or_default()
        } else {
            // Aggregate logs from all active sessions
            let mut all_logs = Vec::new();
            for workspace in &state.workspaces {
                for session in &workspace.sessions {
                    if let Some(logs) = state.live_logs.get(&session.id) {
                        all_logs.extend(logs.iter().cloned());
                    }
                }
            }

            // Sort by timestamp
            all_logs.sort_by_key(|log| log.timestamp);
            all_logs
        }
    }

    fn filter_logs<'a>(&self, logs: &'a [LogEntry]) -> Vec<&'a LogEntry> {
        logs.iter().filter(|log| self.should_include_log(log)).collect()
    }

    fn should_include_log(&self, log: &LogEntry) -> bool {
        match self.filter_level {
            LogLevel::All => true,
            LogLevel::Info => matches!(
                log.level,
                LogEntryLevel::Info | LogEntryLevel::Warn | LogEntryLevel::Error
            ),
            LogLevel::Warn => matches!(log.level, LogEntryLevel::Warn | LogEntryLevel::Error),
            LogLevel::Error => matches!(log.level, LogEntryLevel::Error),
        }
    }

    fn build_title(&self, state: &AppState, filtered_count: usize, total_count: usize) -> String {
        let session_info = if let Some(session) = state.selected_session() {
            format!(" {} ", session.branch_name)
        } else {
            " All Sessions ".to_string()
        };

        let filter_info = if self.filter_level != LogLevel::All {
            format!(" [{}] ", self.filter_level.as_str())
        } else {
            String::new()
        };

        let count_info = if filtered_count != total_count {
            format!(" ({}/{}) ", filtered_count, total_count)
        } else {
            format!(" ({}) ", total_count)
        };

        format!("🔴 Live Logs{}{}{}", session_info, filter_info, count_info)
    }

    fn create_formatted_log_lines(&mut self, logs: &[&LogEntry]) -> Vec<Line> {
        let mut all_lines = Vec::new();

        // Process each log entry
        for log in logs {
            if let Some(ref parsed_data) = log.parsed_data {
                // Use beautiful formatter for parsed logs
                all_lines.push(self.log_formatter.format_log(parsed_data));
            } else {
                // Check if this is a structured message with multiple lines
                if log.message.contains('\n') && log.metadata.get("event_type") == Some(&"structured".to_string()) {
                    // Split multi-line messages (like todos) into separate lines
                    for (idx, line_str) in log.message.lines().enumerate() {
                        if idx == 0 {
                            // First line with timestamp and level
                            all_lines.push(self.format_basic_log_line_with_text(log, line_str));
                        } else {
                            // Subsequent lines without timestamp, just indented
                            all_lines.push(Line::from(vec![
                                ratatui::text::Span::raw("         "), // Indent for alignment
                                ratatui::text::Span::raw(line_str.to_string()),
                            ]));
                        }
                    }
                } else {
                    // Single line message
                    all_lines.push(self.format_basic_log_line(log));
                }
            }
        }

        all_lines
    }

    fn format_basic_log_line(&self, log: &LogEntry) -> Line {
        self.format_basic_log_line_with_text(log, &log.message)
    }

    fn format_basic_log_line_with_text(&self, log: &LogEntry, text: &str) -> Line {
        let timestamp_str = if self.show_timestamps {
            format!("[{}] ", log.timestamp.format("%H:%M:%S"))
        } else {
            String::new()
        };

        let (level_icon, level_color) = match log.level {
            LogEntryLevel::Debug => ("🔍", Color::DarkGray),
            LogEntryLevel::Info => ("ℹ️", Color::Blue),
            LogEntryLevel::Warn => ("⚠️", Color::Yellow),
            LogEntryLevel::Error => ("❌", Color::Red),
        };

        Line::from(vec![
            ratatui::text::Span::styled(timestamp_str, Style::default().fg(Color::DarkGray)),
            ratatui::text::Span::styled(level_icon, Style::default().fg(level_color)),
            ratatui::text::Span::raw(" "),
            ratatui::text::Span::raw(text.to_string()),
        ])
    }

    fn create_log_text(&self, logs: &[&LogEntry], available_width: u16) -> String {
        // Legacy method kept for compatibility
        logs.iter()
            .map(|log| self.format_log_entry_wrapped(log, available_width))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn get_scroll_position(&self, logs: &[&LogEntry]) -> usize {
        if self.auto_scroll {
            // Calculate total lines needed for all logs (rough estimate assuming wrapping)
            let total_lines: usize = logs
                .iter()
                .map(|log| {
                    // Estimate lines needed based on message length
                    let msg_len = self.format_log_entry_wrapped(log, 80).len();
                    (msg_len / 80).max(1)
                })
                .sum();

            // Scroll to show the bottom portion of logs
            total_lines.saturating_sub(self.max_visible_lines)
        } else {
            self.scroll_offset
        }
    }

    fn format_log_entry_wrapped(&self, log: &LogEntry, _available_width: u16) -> String {
        let timestamp_str = if self.show_timestamps {
            format!("[{}] ", log.timestamp.format("%H:%M:%S"))
        } else {
            String::new()
        };

        let (level_icon, _level_color) = match log.level {
            LogEntryLevel::Debug => ("🔍", Color::DarkGray),
            LogEntryLevel::Info => ("ℹ️", Color::Blue),
            LogEntryLevel::Warn => ("⚠️", Color::Yellow),
            LogEntryLevel::Error => ("❌", Color::Red),
        };

        let source_str = if !log.source.is_empty() {
            format!("[{}] ", log.source)
        } else {
            String::new()
        };

        // Format the complete log entry without truncation
        format!(
            "{}{} {}{}",
            timestamp_str, level_icon, source_str, log.message
        )
    }

    fn render_controls_hint(&self, frame: &mut Frame, area: Rect) {
        if area.height < 4 {
            return; // Not enough space
        }

        let controls = format!(
            "[f]Filter:{} [t]Time [↑↓]Scroll [Space]AutoScroll:{}",
            self.filter_level.as_str(),
            if self.auto_scroll { "ON" } else { "OFF" }
        );

        let hint_area = Rect {
            x: area.x + 1,
            y: area.y + area.height - 2,
            width: area.width.saturating_sub(2),
            height: 1,
        };

        frame.render_widget(
            Paragraph::new(controls).style(Style::default().fg(Color::DarkGray)),
            hint_area,
        );
    }

    /// Toggle auto-scroll mode
    pub fn toggle_auto_scroll(&mut self) {
        self.auto_scroll = !self.auto_scroll;
    }

    /// Toggle timestamp display
    pub fn toggle_timestamps(&mut self) {
        self.show_timestamps = !self.show_timestamps;
        // Update formatter config
        let mut config = FormatConfig::default();
        config.show_timestamps = self.show_timestamps;
        self.log_formatter = SimpleLogFormatter::new(config);
    }

    /// Cycle through filter levels
    pub fn cycle_filter_level(&mut self) {
        self.filter_level = self.filter_level.next();
    }

    /// Scroll up manually
    pub fn scroll_up(&mut self) {
        self.auto_scroll = false; // Disable auto-scroll when manually scrolling
        if self.scroll_offset > 0 {
            self.scroll_offset -= 1;
        }
    }

    /// Scroll down manually
    pub fn scroll_down(&mut self, _total_logs: usize) {
        self.auto_scroll = false; // Disable auto-scroll when manually scrolling
        // No upper limit check - the Paragraph widget will handle bounds
        self.scroll_offset += 1;
    }

    /// Scroll to bottom
    pub fn scroll_to_bottom(&mut self, total_logs: usize) {
        self.auto_scroll = true; // Re-enable auto-scroll when going to bottom
        self.scroll_offset = if total_logs > self.max_visible_lines {
            total_logs - self.max_visible_lines
        } else {
            0
        };
    }

    /// Scroll to top
    pub fn scroll_to_top(&mut self) {
        self.auto_scroll = false; // Disable auto-scroll when going to top
        self.scroll_offset = 0;
    }

    /// Update max visible lines based on area height
    pub fn update_max_visible(&mut self, area_height: u16) {
        self.max_visible_lines = ((area_height as usize).saturating_sub(4)).max(5);
    }
}

impl Default for LiveLogsStreamComponent {
    fn default() -> Self {
        Self::new()
    }
}

// Log entry types that correspond to app state
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LogEntry {
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub level: LogEntryLevel,
    pub source: String, // Container name or source
    pub message: String,
    pub session_id: Option<uuid::Uuid>,
    #[serde(skip)]
    pub parsed_data: Option<super::log_parser::ParsedLog>, // Rich parsed metadata (not serialized)
    #[serde(default)]
    pub metadata: std::collections::HashMap<String, String>, // Additional metadata for agent events
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum LogEntryLevel {
    Debug,
    Info,
    Warn,
    Error,
}

impl LogEntry {
    pub fn new(level: LogEntryLevel, source: String, message: String) -> Self {
        Self {
            timestamp: chrono::Utc::now(),
            level,
            source,
            message,
            session_id: None,
            parsed_data: None,
            metadata: std::collections::HashMap::new(),
        }
    }

    pub fn new_with_parsed_data(
        level: LogEntryLevel,
        source: String,
        message: String,
        session_id: uuid::Uuid,
        parsed_data: Option<super::log_parser::ParsedLog>,
    ) -> Self {
        Self {
            timestamp: chrono::Utc::now(),
            level,
            source,
            message,
            session_id: Some(session_id),
            parsed_data,
            metadata: std::collections::HashMap::new(),
        }
    }

    pub fn with_session(mut self, session_id: uuid::Uuid) -> Self {
        self.session_id = Some(session_id);
        self
    }

    pub fn with_metadata(mut self, key: &str, value: &str) -> Self {
        self.metadata.insert(key.to_string(), value.to_string());
        self
    }

    /// Parse log level from Docker log line
    pub fn parse_level_from_message(message: &str) -> LogEntryLevel {
        let lower_msg = message.to_lowercase();
        if lower_msg.contains("error") || lower_msg.contains("fatal") {
            LogEntryLevel::Error
        } else if lower_msg.contains("warn") || lower_msg.contains("warning") {
            LogEntryLevel::Warn
        } else if lower_msg.contains("debug") {
            LogEntryLevel::Debug
        } else {
            LogEntryLevel::Info
        }
    }

    /// Create from raw Docker log line
    pub fn from_docker_log(
        container_name: &str,
        log_line: &str,
        session_id: Option<uuid::Uuid>,
    ) -> Self {
        let level = Self::parse_level_from_message(log_line);
        Self {
            timestamp: chrono::Utc::now(),
            level,
            source: container_name.to_string(),
            message: log_line.to_string(),
            session_id,
            parsed_data: None,
            metadata: std::collections::HashMap::new(),
        }
    }

    /// Create from Docker log line with boss mode parsing (text or JSON)
    pub fn from_docker_log_with_mode(
        container_name: &str,
        log_line: &str,
        session_id: Option<uuid::Uuid>,
        is_boss_mode: bool,
    ) -> Self {
        if is_boss_mode {
            Self::parse_boss_mode_json(container_name, log_line, session_id)
        } else {
            Self::from_docker_log(container_name, log_line, session_id)
        }
    }

    /// Parse Claude CLI output for boss mode (text format with JSON fallback)
    fn parse_boss_mode_json(
        container_name: &str,
        log_line: &str,
        session_id: Option<uuid::Uuid>,
    ) -> Self {
        // Try to parse as JSON first
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(log_line) {
            let message = match json.get("type").and_then(|t| t.as_str()) {
                Some("message") => {
                    if let Some(content) = json.get("content").and_then(|c| c.as_str()) {
                        format!("🤖 Claude: {}", content)
                    } else {
                        format!("🤖 Claude message: {}", log_line)
                    }
                }
                Some("tool_use") => {
                    let tool_name =
                        json.get("tool_name").and_then(|t| t.as_str()).unwrap_or("unknown");

                    let parameters = json
                        .get("parameters")
                        .map(|p| {
                            serde_json::to_string_pretty(p).unwrap_or_else(|_| "{}".to_string())
                        })
                        .unwrap_or_else(|| "{}".to_string());

                    format!(
                        "🔧 Tool Use: {} with parameters:\n{}",
                        tool_name, parameters
                    )
                }
                Some("tool_result") => {
                    let content =
                        json.get("content").and_then(|c| c.as_str()).unwrap_or("No content");

                    // Truncate very long tool results for readability
                    let truncated_content = if content.len() > 500 {
                        format!(
                            "{}...\n[Output truncated - {} characters total]",
                            &content[..500],
                            content.len()
                        )
                    } else {
                        content.to_string()
                    };

                    format!("📤 Tool Result:\n{}", truncated_content)
                }
                Some("error") => {
                    let error_msg =
                        json.get("message").and_then(|m| m.as_str()).unwrap_or("Unknown error");
                    format!("❌ Error: {}", error_msg)
                }
                Some("thinking") => {
                    // Claude's thinking process - might want to show or hide these
                    let thinking =
                        json.get("content").and_then(|c| c.as_str()).unwrap_or("Thinking...");
                    format!("💭 Claude thinking: {}", thinking)
                }
                _ => {
                    // Unknown JSON type, show the raw JSON
                    format!("📋 Claude output: {}", log_line)
                }
            };

            // Determine log level based on JSON type
            let level = match json.get("type").and_then(|t| t.as_str()) {
                Some("error") => LogEntryLevel::Error,
                Some("tool_use") | Some("tool_result") => LogEntryLevel::Info,
                Some("message") => LogEntryLevel::Info,
                Some("thinking") => LogEntryLevel::Debug,
                _ => LogEntryLevel::Info,
            };

            Self {
                timestamp: chrono::Utc::now(),
                level,
                source: "claude-boss".to_string(), // Special source for boss mode
                message,
                session_id,
                parsed_data: None,
                metadata: std::collections::HashMap::new(),
            }
        } else {
            // Not valid JSON, treat as regular log line but mark as boss mode
            let level = Self::parse_level_from_message(log_line);
            Self {
                timestamp: chrono::Utc::now(),
                level,
                source: format!("{}-boss", container_name),
                message: format!("📟 {}", log_line), // Add prefix to indicate boss mode
                session_id,
                parsed_data: None,
                metadata: std::collections::HashMap::new(),
            }
        }
    }
}
