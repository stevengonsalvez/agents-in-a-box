// ABOUTME: Renderer-agnostic half of the `live_logs_stream` component: its
// state types and the logic that does not draw. The renderer lives in
// `ainb-core::components::live_logs_stream`, which re-exports this module.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    All,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            LogLevel::All => "ALL",
            LogLevel::Info => "INFO",
            LogLevel::Warn => "WARN",
            LogLevel::Error => "ERROR",
        }
    }

    pub fn next(&self) -> Self {
        match self {
            LogLevel::All => LogLevel::Info,
            LogLevel::Info => LogLevel::Warn,
            LogLevel::Warn => LogLevel::Error,
            LogLevel::Error => LogLevel::All,
        }
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
