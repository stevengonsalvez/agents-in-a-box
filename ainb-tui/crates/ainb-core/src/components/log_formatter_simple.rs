// ABOUTME: Simplified log formatter for TUI display with beautiful visual styling
// Stateless formatting to avoid complex borrow checker issues

#![allow(dead_code)]

use super::log_parser::{LogCategory, LogLevel, ParsedLog};
use chrono::{DateTime, Duration, Utc};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Configuration for log formatting
#[derive(Debug, Clone)]
pub struct FormatConfig {
    pub show_timestamps: bool,
    pub use_relative_time: bool,
    pub show_source_badges: bool,
    pub compact_mode: bool,
    pub max_message_length: Option<usize>,
}

impl Default for FormatConfig {
    fn default() -> Self {
        Self {
            show_timestamps: false,
            use_relative_time: true,
            show_source_badges: true,
            compact_mode: false,
            max_message_length: None,
        }
    }
}

/// Stateless log formatter
pub struct SimpleLogFormatter {
    config: FormatConfig,
}

impl SimpleLogFormatter {
    pub fn new(config: FormatConfig) -> Self {
        Self { config }
    }

    /// Format a parsed log into a beautiful TUI line (stateless)
    pub fn format_log(&self, log: &ParsedLog) -> Line {
        let mut spans = Vec::new();

        // Add timestamp
        if self.config.show_timestamps {
            spans.push(self.format_timestamp(log.timestamp.as_ref()));
            spans.push(Span::raw(" "));
        }

        // Add category badge
        spans.push(self.format_category_badge(&log.category));
        spans.push(Span::raw(" "));

        // Add level icon for important messages
        if !matches!(log.level, LogLevel::Info | LogLevel::Debug) {
            spans.push(Span::styled(
                log.level.icon(),
                Style::default().fg(log.level.color()),
            ));
            spans.push(Span::raw(" "));
        }

        // Add the message
        spans.push(self.format_message(&log.clean_message, log.level));

        Line::from(spans)
    }

    /// Format timestamp (absolute or relative)
    fn format_timestamp(&self, timestamp: Option<&DateTime<Utc>>) -> Span {
        let time_str = if let Some(ts) = timestamp {
            if self.config.use_relative_time {
                self.relative_time(ts)
            } else {
                ts.format("%H:%M:%S").to_string()
            }
        } else {
            "        ".to_string()
        };

        Span::styled(
            format!("{:>8}", time_str),
            Style::default().fg(Color::DarkGray),
        )
    }

    /// Convert timestamp to relative time
    fn relative_time(&self, timestamp: &DateTime<Utc>) -> String {
        let now = Utc::now();
        let diff = now - *timestamp;

        if diff < Duration::seconds(1) {
            "now".to_string()
        } else if diff < Duration::minutes(1) {
            format!("{}s ago", diff.num_seconds())
        } else if diff < Duration::hours(1) {
            format!("{}m ago", diff.num_minutes())
        } else if diff < Duration::days(1) {
            format!("{}h ago", diff.num_hours())
        } else {
            timestamp.format("%H:%M").to_string()
        }
    }

    /// Format category badge with background color
    fn format_category_badge(&self, category: &LogCategory) -> Span {
        let (bg_color, fg_color) = match category {
            LogCategory::System => (Color::Blue, Color::White),
            LogCategory::Authentication => (Color::Green, Color::Black),
            LogCategory::Container => (Color::Cyan, Color::Black),
            LogCategory::Claude => (Color::Yellow, Color::Black),
            LogCategory::Command => (Color::Magenta, Color::White),
            LogCategory::Configuration => (Color::Gray, Color::White),
            LogCategory::Error => (Color::Red, Color::White),
            LogCategory::Network => (Color::Blue, Color::White),
            LogCategory::Git => (Color::Green, Color::White),
            LogCategory::Unknown => (Color::DarkGray, Color::White),
        };

        Span::styled(
            format!(" {} {} ", category.icon(), category.label()),
            Style::default().bg(bg_color).fg(fg_color).add_modifier(Modifier::BOLD),
        )
    }

    /// Format the main message content
    fn format_message(&self, message: &str, level: LogLevel) -> Span {
        let formatted = if let Some(max_len) = self.config.max_message_length {
            if message.len() > max_len {
                format!("{}...", &message[..max_len])
            } else {
                message.to_string()
            }
        } else {
            message.to_string()
        };

        let style = match level {
            LogLevel::Error | LogLevel::Fatal => {
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
            }
            LogLevel::Warning => Style::default().fg(Color::Yellow),
            LogLevel::Success => Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
            LogLevel::Debug | LogLevel::Trace => Style::default().fg(Color::DarkGray),
            _ => Style::default(),
        };

        Span::styled(formatted, style)
    }
}
