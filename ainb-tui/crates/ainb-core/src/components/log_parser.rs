// ABOUTME: Advanced log parser for beautifying Docker container logs in TUI
// Handles pattern detection, ANSI stripping, and intelligent log categorization

#![allow(dead_code)]

use chrono::{DateTime, Utc};
use lazy_static::lazy_static;
use regex::Regex;
use std::collections::HashMap;

/// Parsed log entry with rich metadata
#[derive(Debug, Clone)]
pub struct ParsedLog {
    pub raw_message: String,
    pub clean_message: String,
    pub category: LogCategory,
    pub level: LogLevel,
    pub timestamp: Option<DateTime<Utc>>,
    pub source: LogSource,
    pub metadata: HashMap<String, String>,
    pub is_continuation: bool,
}

/// Log categories for better organization
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogCategory {
    System,
    Authentication,
    Container,
    Claude,
    Command,
    Configuration,
    Error,
    Network,
    Git,
    Unknown,
}

/// Log sources
#[derive(Debug, Clone)]
pub enum LogSource {
    AgentsBox,
    ClaudeSession(String),
    Docker,
    System,
    Unknown,
}

/// Enhanced log levels
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Success,
    Warning,
    Error,
    Fatal,
}

lazy_static! {
    // ANSI escape sequence remover
    static ref ANSI_REGEX: Regex = Regex::new(r"\x1B\[[0-9;]*[mGKH]").unwrap();

    // Docker timestamp pattern (ISO 8601)
    static ref DOCKER_TIMESTAMP: Regex = Regex::new(
        r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d+Z"
    ).unwrap();

    // Claude session pattern [claude/session-id]
    static ref CLAUDE_SESSION: Regex = Regex::new(
        r"\[claude/([a-f0-9-]+)\]"
    ).unwrap();

    // Agents-box tag pattern
    static ref AGENTS_BOX_TAG: Regex = Regex::new(
        r"\[agents-box\]"
    ).unwrap();

    // Log level patterns
    static ref LOG_LEVEL_PATTERNS: Vec<(Regex, LogLevel)> = vec![
        (Regex::new(r"(?i)\b(error|err|fail|failed|fatal)\b").unwrap(), LogLevel::Error),
        (Regex::new(r"(?i)\b(warn|warning)\b").unwrap(), LogLevel::Warning),
        (Regex::new(r"(?i)\b(success|succeed|complete|ready|ok|✓|✅)\b").unwrap(), LogLevel::Success),
        (Regex::new(r"(?i)\b(info|information)\b").unwrap(), LogLevel::Info),
        (Regex::new(r"(?i)\b(debug|dbg)\b").unwrap(), LogLevel::Debug),
        (Regex::new(r"(?i)\b(trace|tracing)\b").unwrap(), LogLevel::Trace),
    ];

    // Category detection patterns
    static ref CATEGORY_PATTERNS: Vec<(Regex, LogCategory)> = vec![
        (Regex::new(r"(?i)(oauth|auth|token|credential|login|ssh key)").unwrap(), LogCategory::Authentication),
        (Regex::new(r"(?i)(container|docker|mount|volume)").unwrap(), LogCategory::Container),
        (Regex::new(r"(?i)(claude-ask|claude-print|claude-script|claude help|claude-start)").unwrap(), LogCategory::Command),
        (Regex::new(r"(?i)(config|setting|environment|env var)").unwrap(), LogCategory::Configuration),
        (Regex::new(r"(?i)(git|branch|commit|push|pull|worktree)").unwrap(), LogCategory::Git),
        (Regex::new(r"(?i)(network|http|api|connection|connected|disconnected)").unwrap(), LogCategory::Network),
        (Regex::new(r"(?i)(system|startup|shutdown|initialization)").unwrap(), LogCategory::System),
    ];

    // Claude command patterns
    static ref CLAUDE_COMMANDS: Regex = Regex::new(
        r"claude-(ask|print|script|start|interactive|help)"
    ).unwrap();

    // Metadata extraction patterns (moved from extract_metadata() for performance)
    static ref CONTAINER_ID_REGEX: Regex = Regex::new(r"container: ([a-f0-9]{12})").unwrap();
    static ref PORT_REGEX: Regex = Regex::new(r"port[: ]+(\d+)").unwrap();
    static ref FILE_PATH_REGEX: Regex = Regex::new(r"(/[\w/.-]+)").unwrap();

    // Message cleanup patterns (moved from clean_message_for_display() for performance)
    static ref TIMESTAMP_CLEANUP_REGEX: Regex = Regex::new(
        r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d+Z\s*"
    ).unwrap();
    static ref SESSION_CLEANUP_REGEX: Regex = Regex::new(r"\[claude/[a-f0-9-]+\]\s*").unwrap();
}

pub struct LogParser {
    multiline_buffer: String,
    last_category: LogCategory,
}

impl LogParser {
    pub fn new() -> Self {
        Self {
            multiline_buffer: String::new(),
            last_category: LogCategory::Unknown,
        }
    }

    /// Parse a raw log line into a structured format
    pub fn parse_log(&mut self, raw_line: &str) -> ParsedLog {
        // Strip ANSI codes first
        let clean_line = self.strip_ansi(raw_line);

        // Extract timestamp if present
        let (timestamp, message) = self.extract_timestamp(&clean_line);

        // Detect source
        let (source, message) = self.detect_source(&message);

        // Detect category
        let category = self.detect_category(&message);

        // Detect log level
        let level = self.detect_level(&message);

        // Check if this is a continuation of previous log
        let is_continuation = self.is_continuation_line(&message);

        // Extract metadata
        let metadata = self.extract_metadata(&message);

        // Clean up the message for display
        let display_message = self.clean_message_for_display(&message);

        // Update state
        self.last_category = category;

        ParsedLog {
            raw_message: raw_line.to_string(),
            clean_message: display_message,
            category,
            level,
            timestamp,
            source,
            metadata,
            is_continuation,
        }
    }

    /// Strip ANSI escape sequences from text
    fn strip_ansi(&self, text: &str) -> String {
        ANSI_REGEX.replace_all(text, "").to_string()
    }

    /// Extract Docker timestamp from log line
    fn extract_timestamp(&self, line: &str) -> (Option<DateTime<Utc>>, String) {
        if let Some(cap) = DOCKER_TIMESTAMP.find(line) {
            let timestamp_str = cap.as_str();
            let remaining = line[cap.end()..].trim().to_string();

            if let Ok(dt) = DateTime::parse_from_rfc3339(timestamp_str) {
                return (Some(dt.with_timezone(&Utc)), remaining);
            }
        }
        (None, line.to_string())
    }

    /// Detect log source (claude-box, claude session, etc.)
    fn detect_source(&self, message: &str) -> (LogSource, String) {
        // Check for Claude session pattern
        if let Some(caps) = CLAUDE_SESSION.captures(message) {
            let session_id = caps.get(1).unwrap().as_str().to_string();
            let cleaned = CLAUDE_SESSION.replace(message, "").trim().to_string();
            return (LogSource::ClaudeSession(session_id), cleaned);
        }

        // Check for agents-box tag
        if AGENTS_BOX_TAG.is_match(message) {
            let cleaned = AGENTS_BOX_TAG.replace(message, "").trim().to_string();
            return (LogSource::AgentsBox, cleaned);
        }

        // Check for docker prefix
        if message.starts_with("[docker]") || message.starts_with("docker:") {
            let cleaned = message
                .trim_start_matches("[docker]")
                .trim_start_matches("docker:")
                .trim()
                .to_string();
            return (LogSource::Docker, cleaned);
        }

        (LogSource::Unknown, message.to_string())
    }

    /// Detect log category based on content
    fn detect_category(&self, message: &str) -> LogCategory {
        // Check for Claude-specific patterns first
        if CLAUDE_COMMANDS.is_match(message) {
            return LogCategory::Claude;
        }

        // Check other category patterns
        for (pattern, category) in CATEGORY_PATTERNS.iter() {
            if pattern.is_match(message) {
                return *category;
            }
        }

        LogCategory::Unknown
    }

    /// Detect log level from message content
    fn detect_level(&self, message: &str) -> LogLevel {
        for (pattern, level) in LOG_LEVEL_PATTERNS.iter() {
            if pattern.is_match(message) {
                return *level;
            }
        }
        LogLevel::Info
    }

    /// Check if this line is a continuation of the previous log
    fn is_continuation_line(&self, message: &str) -> bool {
        // Lines starting with spaces, tabs, or certain characters are continuations
        message.starts_with("  ")
            || message.starts_with("\t")
            || message.starts_with("   •")
            || message.starts_with("   -")
            || message.starts_with("   *")
    }

    /// Extract metadata from log message
    fn extract_metadata(&self, message: &str) -> HashMap<String, String> {
        let mut metadata = HashMap::new();

        // Extract container ID if present (uses static regex for performance)
        if let Some(caps) = CONTAINER_ID_REGEX.captures(message) {
            metadata.insert("container_id".to_string(), caps[1].to_string());
        }

        // Extract port numbers (uses static regex for performance)
        if let Some(caps) = PORT_REGEX.captures(message) {
            metadata.insert("port".to_string(), caps[1].to_string());
        }

        // Extract file paths (uses static regex for performance)
        if let Some(caps) = FILE_PATH_REGEX.captures(message) {
            metadata.insert("path".to_string(), caps[1].to_string());
        }

        metadata
    }

    /// Clean message for display (remove redundant tags and timestamps)
    fn clean_message_for_display(&self, message: &str) -> String {
        // Remove redundant timestamps that might be embedded (uses static regex for performance)
        let clean = TIMESTAMP_CLEANUP_REGEX.replace_all(message, "");

        // Remove duplicate container/session references (uses static regex for performance)
        let clean = SESSION_CLEANUP_REGEX.replace_all(&clean, "");

        // Remove excessive whitespace
        clean.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// Process multiline logs (like stack traces)
    pub fn handle_multiline(&mut self, line: &str) -> Option<ParsedLog> {
        if self.is_continuation_line(line) {
            self.multiline_buffer.push('\n');
            self.multiline_buffer.push_str(line);
            None
        } else if !self.multiline_buffer.is_empty() {
            let buffer = std::mem::take(&mut self.multiline_buffer);
            let complete_log = self.parse_log(&buffer);
            Some(complete_log)
        } else {
            Some(self.parse_log(line))
        }
    }
}

impl LogCategory {
    pub fn icon(&self) -> &'static str {
        match self {
            LogCategory::System => "⚙️",
            LogCategory::Authentication => "🔐",
            LogCategory::Container => "📦",
            LogCategory::Claude => "🤖",
            LogCategory::Command => "⌨️",
            LogCategory::Configuration => "🔧",
            LogCategory::Error => "❌",
            LogCategory::Network => "🌐",
            LogCategory::Git => "🔀",
            LogCategory::Unknown => "📝",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            LogCategory::System => "System",
            LogCategory::Authentication => "Auth",
            LogCategory::Container => "Container",
            LogCategory::Claude => "Claude",
            LogCategory::Command => "Command",
            LogCategory::Configuration => "Config",
            LogCategory::Error => "Error",
            LogCategory::Network => "Network",
            LogCategory::Git => "Git",
            LogCategory::Unknown => "Log",
        }
    }
}

impl LogLevel {
    pub fn icon(&self) -> &'static str {
        match self {
            LogLevel::Trace => "🔍",
            LogLevel::Debug => "🐛",
            LogLevel::Info => "ℹ️",
            LogLevel::Success => "✅",
            LogLevel::Warning => "⚠️",
            LogLevel::Error => "❌",
            LogLevel::Fatal => "💀",
        }
    }

    pub fn color(&self) -> ratatui::style::Color {
        use ratatui::style::Color;
        match self {
            LogLevel::Trace => Color::DarkGray,
            LogLevel::Debug => Color::Gray,
            LogLevel::Info => Color::Blue,
            LogLevel::Success => Color::Green,
            LogLevel::Warning => Color::Yellow,
            LogLevel::Error => Color::Red,
            LogLevel::Fatal => Color::Magenta,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ansi_stripping() {
        let parser = LogParser::new();
        let raw = "\x1B[32mSuccess!\x1B[0m Container started";
        let clean = parser.strip_ansi(raw);
        assert_eq!(clean, "Success! Container started");
    }

    #[test]
    fn test_source_detection() {
        let parser = LogParser::new();

        let (source, _) = parser.detect_source("[agents-box] Starting");
        assert!(matches!(source, LogSource::AgentsBox));

        let (source, _) = parser.detect_source("[claude/abc123] Ready");
        assert!(matches!(source, LogSource::ClaudeSession(_)));
    }

    #[test]
    fn test_category_detection() {
        let parser = LogParser::new();

        assert_eq!(
            parser.detect_category("OAuth authentication successful"),
            LogCategory::Authentication
        );

        assert_eq!(
            parser.detect_category("claude-ask 'What is 2+2?'"),
            LogCategory::Claude
        );

        assert_eq!(
            parser.detect_category("Container environment ready"),
            LogCategory::Container
        );
    }
}
