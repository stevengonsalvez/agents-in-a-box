// ABOUTME: Beautiful log formatter for TUI display with rich visual styling
// Creates organized, hierarchical log displays with smart grouping and icons

use super::log_parser::{ParsedLog, LogCategory, LogLevel, LogSource};
use chrono::{DateTime, Utc, Duration};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::collections::VecDeque;

/// Configuration for log formatting
#[derive(Debug, Clone)]
pub struct FormatConfig {
    pub show_timestamps: bool,
    pub use_relative_time: bool,
    pub show_source_badges: bool,
    pub compact_mode: bool,
    pub group_related_logs: bool,
    pub max_message_length: Option<usize>,
}

impl Default for FormatConfig {
    fn default() -> Self {
        Self {
            show_timestamps: false,
            use_relative_time: true,
            show_source_badges: true,
            compact_mode: false,
            group_related_logs: true,
            max_message_length: None,
        }
    }
}

/// Log group for collapsing related messages
#[derive(Debug, Clone)]
pub struct LogGroup {
    pub title: String,
    pub icon: &'static str,
    pub logs: Vec<ParsedLog>,
    pub collapsed: bool,
    pub timestamp: DateTime<Utc>,
}

pub struct LogFormatter {
    config: FormatConfig,
    recent_groups: VecDeque<LogGroup>,
    current_group: Option<LogGroup>,
}

impl LogFormatter {
    pub fn new(config: FormatConfig) -> Self {
        Self {
            config,
            recent_groups: VecDeque::with_capacity(100),
            current_group: None,
        }
    }
    
    /// Format a parsed log into beautiful TUI lines
    pub fn format_log(&mut self, log: &ParsedLog) -> Vec<Line> {
        let mut lines = Vec::new();
        
        // Try to group related logs
        if self.config.group_related_logs {
            if let Some(grouped) = self.try_group_log(log) {
                return grouped;
            }
        }
        
        // Format individual log
        lines.push(self.format_single_log(log));
        
        // Add continuation lines if any
        if log.is_continuation {
            lines.push(self.format_continuation(log));
        }
        
        lines
    }
    
    /// Format a single log entry
    fn format_single_log(&self, log: &ParsedLog) -> Line {
        let mut spans = Vec::new();
        
        // Add timestamp
        if self.config.show_timestamps {
            spans.push(self.format_timestamp(log.timestamp.as_ref()));
            spans.push(Span::raw(" "));
        }
        
        // Add category icon and badge
        spans.push(self.format_category_badge(&log.category));
        spans.push(Span::raw(" "));
        
        // Add source badge if enabled
        if self.config.show_source_badges {
            if let Some(badge) = self.format_source_badge(&log.source) {
                spans.push(badge);
                spans.push(Span::raw(" "));
            }
        }
        
        // Add level icon for important messages
        if !matches!(log.level, LogLevel::Info | LogLevel::Debug) {
            spans.push(Span::styled(
                log.level.icon(),
                Style::default().fg(log.level.color())
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
            Style::default().fg(Color::DarkGray)
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
            Style::default()
                .bg(bg_color)
                .fg(fg_color)
                .add_modifier(Modifier::BOLD)
        )
    }
    
    /// Format source badge
    fn format_source_badge(&self, source: &LogSource) -> Option<Span> {
        match source {
            LogSource::AgentsBox => Some(Span::styled(
                "[📦]",
                Style::default().fg(Color::Cyan)
            )),
            LogSource::ClaudeSession(id) => Some(Span::styled(
                format!("[🤖 {}]", &id[..8]),
                Style::default().fg(Color::Green)
            )),
            LogSource::Docker => Some(Span::styled(
                "[🐳]",
                Style::default().fg(Color::Blue)
            )),
            LogSource::System => Some(Span::styled(
                "[⚙️]",
                Style::default().fg(Color::Gray)
            )),
            LogSource::Unknown => None,
        }
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
            LogLevel::Error | LogLevel::Fatal => Style::default()
                .fg(Color::Red)
                .add_modifier(Modifier::BOLD),
            LogLevel::Warning => Style::default().fg(Color::Yellow),
            LogLevel::Success => Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
            LogLevel::Debug | LogLevel::Trace => Style::default().fg(Color::DarkGray),
            _ => Style::default(),
        };
        
        Span::styled(formatted, style)
    }
    
    /// Format continuation lines with proper indentation
    fn format_continuation(&self, log: &ParsedLog) -> Line {
        let indent = if self.config.show_timestamps {
            "         "  // 8 spaces for timestamp alignment
        } else {
            "  "
        };
        
        Line::from(vec![
            Span::raw(indent),
            Span::styled("│ ", Style::default().fg(Color::DarkGray)),
            Span::raw(&log.clean_message),
        ])
    }
    
    /// Try to group related logs together
    fn try_group_log(&mut self, log: &ParsedLog) -> Option<Vec<Line>> {
        // Check if this log should start a new group
        if self.should_start_group(log) {
            self.finish_current_group();
            self.start_new_group(log);
            return None;
        }
        
        // Check if this log belongs to current group
        if let Some(ref mut group) = self.current_group {
            if self.belongs_to_group(log, group) {
                group.logs.push(log.clone());
                return None;
            }
        }
        
        // This log doesn't belong to any group
        self.finish_current_group();
        None
    }
    
    /// Check if a log should start a new group
    fn should_start_group(&self, log: &ParsedLog) -> bool {
        // Group initialization sequences
        log.clean_message.contains("Starting") ||
        log.clean_message.contains("Initializing") ||
        log.clean_message.contains("Loading") ||
        log.clean_message.contains("Setting up")
    }
    
    /// Check if a log belongs to the current group
    fn belongs_to_group(&self, log: &ParsedLog, group: &LogGroup) -> bool {
        // Same category and within 5 seconds
        if let Some(last) = group.logs.last() {
            if last.category == log.category {
                if let (Some(last_ts), Some(log_ts)) = (&last.timestamp, &log.timestamp) {
                    let diff = *log_ts - *last_ts;
                    return diff < Duration::seconds(5);
                }
            }
        }
        false
    }
    
    /// Start a new log group
    fn start_new_group(&mut self, log: &ParsedLog) {
        let group = LogGroup {
            title: self.generate_group_title(log),
            icon: log.category.icon(),
            logs: vec![log.clone()],
            collapsed: false,
            timestamp: log.timestamp.unwrap_or_else(Utc::now),
        };
        self.current_group = Some(group);
    }
    
    /// Generate a title for a log group
    fn generate_group_title(&self, log: &ParsedLog) -> String {
        match log.category {
            LogCategory::Container => "Container Setup".to_string(),
            LogCategory::Authentication => "Authentication".to_string(),
            LogCategory::Configuration => "Configuration".to_string(),
            LogCategory::System => "System Initialization".to_string(),
            _ => log.category.label().to_string(),
        }
    }
    
    /// Finish the current group and add it to recent groups
    fn finish_current_group(&mut self) {
        if let Some(group) = self.current_group.take() {
            if !group.logs.is_empty() {
                self.recent_groups.push_back(group);
                if self.recent_groups.len() > 100 {
                    self.recent_groups.pop_front();
                }
            }
        }
    }
    
    /// Format a complete log group
    pub fn format_group(&self, group: &LogGroup) -> Vec<Line> {
        let mut lines = Vec::new();
        
        // Group header
        let header = if group.collapsed {
            format!("▶ {} {} ({} logs)", group.icon, group.title, group.logs.len())
        } else {
            format!("▼ {} {}", group.icon, group.title)
        };
        
        lines.push(Line::from(vec![
            self.format_timestamp(Some(&group.timestamp)),
            Span::raw(" "),
            Span::styled(
                header,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            ),
        ]));
        
        // Show logs if not collapsed
        if !group.collapsed {
            for (i, log) in group.logs.iter().enumerate() {
                let prefix = if i == group.logs.len() - 1 {
                    "└─"
                } else {
                    "├─"
                };
                
                lines.push(Line::from(vec![
                    Span::raw("           "),  // Indent
                    Span::styled(prefix, Style::default().fg(Color::DarkGray)),
                    Span::raw(" "),
                    self.format_compact_log(log),
                ]));
            }
        }
        
        lines
    }
    
    /// Format a log in compact mode (for groups)
    fn format_compact_log(&self, log: &ParsedLog) -> Span {
        let icon = if matches!(log.level, LogLevel::Success) {
            "✅ "
        } else if matches!(log.level, LogLevel::Error) {
            "❌ "
        } else {
            ""
        };
        
        Span::raw(format!("{}{}", icon, log.clean_message))
    }
    
    /// Get all formatted groups
    pub fn get_formatted_groups(&self) -> Vec<Line> {
        let mut lines = Vec::new();
        
        for group in &self.recent_groups {
            lines.extend(self.format_group(group));
        }
        
        // Add current group if any
        if let Some(ref group) = self.current_group {
            lines.extend(self.format_group(group));
        }
        
        lines
    }
}

/// Create a beautiful summary line for multiple logs
pub fn create_summary_line(logs: &[ParsedLog]) -> String {
    let mut auth_count = 0;
    let mut error_count = 0;
    let mut command_count = 0;
    let mut system_count = 0;
    
    for log in logs {
        match log.category {
            LogCategory::Authentication => auth_count += 1,
            LogCategory::Error => error_count += 1,
            LogCategory::Command | LogCategory::Claude => command_count += 1,
            LogCategory::System | LogCategory::Container => system_count += 1,
            _ => {}
        }
    }
    
    let mut parts = Vec::new();
    
    if system_count > 0 {
        parts.push(format!("⚙️ {} system", system_count));
    }
    if auth_count > 0 {
        parts.push(format!("🔐 {} auth", auth_count));
    }
    if command_count > 0 {
        parts.push(format!("🤖 {} commands", command_count));
    }
    if error_count > 0 {
        parts.push(format!("❌ {} errors", error_count));
    }
    
    if parts.is_empty() {
        format!("{} logs", logs.len())
    } else {
        parts.join(" · ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_relative_time_formatting() {
        let formatter = LogFormatter::new(FormatConfig::default());
        let now = Utc::now();
        
        let recent = now - Duration::seconds(5);
        assert_eq!(formatter.relative_time(&recent), "5s ago");
        
        let minute_old = now - Duration::minutes(2);
        assert_eq!(formatter.relative_time(&minute_old), "2m ago");
    }
}