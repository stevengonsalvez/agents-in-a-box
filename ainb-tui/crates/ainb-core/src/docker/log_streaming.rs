// ABOUTME: Docker log streaming manager for real-time container log collection
// Streams logs from Docker containers to the live logs UI component

#![allow(dead_code)]

use crate::agent_parsers::AgentOutputParser;
use crate::components::live_logs_stream::{LogEntry, LogEntryLevel};
use crate::components::log_parser::LogParser;
use crate::docker::ContainerManager;
use anyhow::{Result, anyhow};
use bollard::container::{LogOutput, LogsOptions};
use futures_util::StreamExt;
use std::collections::HashMap;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

// Maximum size of the in-flight JSON buffer while waiting for a balanced object.
// Prevents unbounded memory growth on malformed or never-terminating streams.
const DEFAULT_JSON_BUF_LIMIT: usize = 256 * 1024; // 256 KB

#[derive(Debug)]
pub struct DockerLogStreamingManager {
    container_manager: ContainerManager,
    streaming_tasks: HashMap<Uuid, StreamingTask>,
    log_sender: mpsc::UnboundedSender<(Uuid, LogEntry)>,
    session_modes: HashMap<Uuid, crate::models::SessionMode>, // Track session modes for proper parsing
}

#[derive(Debug)]
struct StreamingTask {
    container_id: String,
    container_name: String,
    task_handle: JoinHandle<()>,
}

impl DockerLogStreamingManager {
    /// Create a new log streaming manager
    pub fn new(log_sender: mpsc::UnboundedSender<(Uuid, LogEntry)>) -> Result<Self> {
        Ok(Self {
            container_manager: ContainerManager::new_sync()?,
            streaming_tasks: HashMap::new(),
            log_sender,
            session_modes: HashMap::new(),
        })
    }

    /// Start streaming logs for a session's container
    pub async fn start_streaming(
        &mut self,
        session_id: Uuid,
        container_id: String,
        container_name: String,
        session_mode: crate::models::SessionMode,
    ) -> Result<()> {
        // Stop any existing streaming for this session
        self.stop_streaming(session_id).await?;

        info!(
            "Starting log streaming for session {} (container: {}) in {:?} mode",
            session_id, container_id, session_mode
        );

        // Store session mode for parsing
        self.session_modes.insert(session_id, session_mode.clone());

        let log_sender = self.log_sender.clone();
        let container_id_clone = container_id.clone();
        let container_name_clone = container_name.clone();
        let docker = self.container_manager.get_docker_client();

        // Spawn a task to stream logs
        let task_handle = tokio::spawn(async move {
            if let Err(e) = Self::stream_container_logs(
                docker,
                session_id,
                container_id_clone.clone(),
                container_name_clone.clone(),
                log_sender,
                session_mode,
            )
            .await
            {
                error!(
                    "Log streaming error for container {}: {}",
                    container_id_clone, e
                );
            }
        });

        self.streaming_tasks.insert(
            session_id,
            StreamingTask {
                container_id,
                container_name,
                task_handle,
            },
        );

        Ok(())
    }

    /// Stop streaming logs for a session
    pub async fn stop_streaming(&mut self, session_id: Uuid) -> Result<()> {
        if let Some(task) = self.streaming_tasks.remove(&session_id) {
            info!(
                "Stopping log streaming for session {} (container: {})",
                session_id, task.container_id
            );
            task.task_handle.abort();
        }
        // Remove session mode tracking
        self.session_modes.remove(&session_id);
        Ok(())
    }

    /// Stop all log streaming
    pub async fn stop_all_streaming(&mut self) -> Result<()> {
        info!("Stopping all log streaming tasks");
        for (_, task) in self.streaming_tasks.drain() {
            task.task_handle.abort();
        }
        // Clear all session mode tracking
        self.session_modes.clear();
        Ok(())
    }

    /// Get active streaming sessions
    pub fn active_sessions(&self) -> Vec<Uuid> {
        self.streaming_tasks.keys().cloned().collect()
    }

    /// Check if streaming is active for a session
    pub fn is_streaming(&self, session_id: Uuid) -> bool {
        self.streaming_tasks.contains_key(&session_id)
    }

    /// Stream logs from a container
    async fn stream_container_logs(
        docker: bollard::Docker,
        session_id: Uuid,
        container_id: String,
        container_name: String,
        log_sender: mpsc::UnboundedSender<(Uuid, LogEntry)>,
        session_mode: crate::models::SessionMode,
    ) -> Result<()> {
        let options = LogsOptions::<String> {
            stdout: true,
            stderr: true,
            follow: true,
            timestamps: false,  // Disable timestamps for cleaner JSON output
            tail: "100".to_string(), // Start with last 100 lines
            ..Default::default()
        };

        debug!(
            "Starting log stream for container {} (session {})",
            container_id, session_id
        );

        let mut log_stream = docker.logs(&container_id, Some(options));
        let mut log_parser = LogParser::new();

        // JSON streaming parser (used for Boss Mode, but safe to try for any session)
        let mut agent_parser: Option<Box<dyn AgentOutputParser>> = None;
        let _is_boss_mode = matches!(session_mode, crate::models::SessionMode::Boss);
        // Buffer for partial JSON objects across frames
        let mut boss_json_buffer = String::new();
        let _parser_debug = std::env::var("AGENTS_BOX_PARSER_DEBUG").is_ok();
        let buf_limit: usize = std::env::var("AGENTS_BOX_JSON_BUF_MAX")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_JSON_BUF_LIMIT);

        // Create MessageRouter once for the entire stream to maintain state across events
        use crate::widgets::MessageRouter;
        let mut message_router = MessageRouter::new();

        // Send initial connection message
        let _ = log_sender.send((
            session_id,
            LogEntry::new(
                LogEntryLevel::Info,
                "system".to_string(),
                format!("📡 Connected to container logs: {}", container_name),
            )
            .with_session(session_id),
        ));

        while let Some(log_result) = log_stream.next().await {
            match log_result {
                Ok(log_output) => {
                    // Extract raw message
                    let raw_message = match &log_output {
                        LogOutput::StdOut { message }
                        | LogOutput::StdErr { message }
                        | LogOutput::Console { message }
                        | LogOutput::StdIn { message } => {
                            String::from_utf8_lossy(message).to_string()
                        }
                    };

                    // For boss mode, try to extract a JSON slice from the line
                    // Docker logs format: "2025-09-08T19:20:30.123456789Z {"type":"..."}"
                    let mut handled_as_json = false;
                    let parser_debug = std::env::var("AGENTS_BOX_PARSER_DEBUG").is_ok();

                    // Prefer robust streaming JSON handling for any line that looks like JSON
                    if let Some(start) = raw_message.find('{') {
                        let mut candidate = String::new();
                        if !boss_json_buffer.is_empty() {
                            candidate.push_str(&boss_json_buffer);
                        }
                        candidate.push_str(&raw_message[start..]);

                        let (objects, incomplete) = Self::stream_json_objects(&candidate);

                        if parser_debug {
                            debug!(
                                got_objects = objects.len(),
                                incomplete,
                                preview = %candidate.chars().take(120).collect::<String>(),
                                mode = ?session_mode,
                                "JSON candidate evaluated"
                            );
                        }

                        if !objects.is_empty() {
                            if agent_parser.is_none() {
                                agent_parser =
                                    Some(Box::new(crate::agent_parsers::ClaudeJsonParser::new()));
                            }
                            if let Some(ref mut parser) = agent_parser {
                                for obj in objects {
                                    match parser.parse_line(&obj) {
                                        Ok(events) => {
                                            for event in events {
                                                let log_entries = Self::agent_event_to_log_entries(
                                                    event,
                                                    &container_name,
                                                    session_id,
                                                    &mut message_router,
                                                );
                                                for log_entry in log_entries {
                                                    if let Err(e) =
                                                        log_sender.send((session_id, log_entry))
                                                    {
                                                        warn!("Failed to send log entry: {}", e);
                                                        break;
                                                    }
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            debug!("Parser error on JSON object: {}", e);
                                            // Don't show raw JSON to user on parse error
                                            // This prevents JSON from appearing in the TUI
                                        }
                                    }
                                }
                            }
                            handled_as_json = true;
                        }

                        if incomplete {
                            // Enforce a hard cap to avoid unbounded buffering
                            if candidate.len() > buf_limit {
                                let warn_msg = format!(
                                    "⚠️ JSON buffer limit exceeded ({} bytes > {} bytes). Flushing as plain text.",
                                    candidate.len(),
                                    buf_limit
                                );
                                let _ = log_sender.send((
                                    session_id,
                                    LogEntry::new(
                                        LogEntryLevel::Warn,
                                        container_name.clone(),
                                        warn_msg,
                                    )
                                    .with_session(session_id),
                                ));

                                // Emit a trimmed preview of the buffered content to avoid huge messages
                                let preview = if candidate.len() > 4000 {
                                    format!("{}... (truncated)", &candidate[..4000])
                                } else {
                                    candidate.clone()
                                };
                                let _ = log_sender.send((
                                    session_id,
                                    LogEntry::new(
                                        LogEntryLevel::Info,
                                        container_name.clone(),
                                        preview,
                                    )
                                    .with_session(session_id),
                                ));

                                boss_json_buffer.clear();
                                handled_as_json = true;
                            } else {
                                boss_json_buffer = candidate; // keep buffering until complete
                                handled_as_json = true;
                            }
                        } else {
                            boss_json_buffer.clear();
                        }
                    } else if !boss_json_buffer.is_empty() {
                        // Continue buffering if we were mid-object
                        boss_json_buffer.push_str(&raw_message);
                        handled_as_json = true;
                    }

                    // Regular parsing for non-JSON lines or when JSON parsing fails
                    if !handled_as_json {
                        let log_entry = Self::parse_log_output_with_parser(
                            log_output,
                            &container_name,
                            session_id,
                            &session_mode,
                            &mut log_parser,
                        );

                        if let Err(e) = log_sender.send((session_id, log_entry)) {
                            warn!("Failed to send log entry: {}", e);
                            break;
                        }
                    }
                }
                Err(e) => {
                    error!("Error reading log stream: {}", e);
                    let _ = log_sender.send((
                        session_id,
                        LogEntry::new(
                            LogEntryLevel::Error,
                            "system".to_string(),
                            format!("❌ Log stream error: {}", e),
                        )
                        .with_session(session_id),
                    ));
                    break;
                }
            }
        }

        debug!(
            "Log stream ended for container {} (session {})",
            container_id, session_id
        );

        // Send disconnection message
        let _ = log_sender.send((
            session_id,
            LogEntry::new(
                LogEntryLevel::Info,
                "system".to_string(),
                format!("📡 Disconnected from container logs: {}", container_name),
            )
            .with_session(session_id),
        ));

        Ok(())
    }

    /// Extract a JSON object slice from a Docker log line.
    /// Examples:
    ///  - "2025-09-08T19:20:30.123Z {\"type\":\"assistant\"}"
    ///  - "{\"type\":\"system\"}"
    fn extract_json_from_log(line: &str) -> Option<&str> {
        // Find first '{' and return slice from there
        let start = line.find('{')?;
        let slice = &line[start..];
        // Cheap sanity: must contain a closing brace sometime later
        if slice.contains('}') {
            Some(slice)
        } else {
            None
        }
    }

    /// Consume one or more JSON values by brace-balance scanning; robust to whitespace and preserves exact slices.
    /// Returns (objects_as_string, incomplete_last)
    fn stream_json_objects(input: &str) -> (Vec<String>, bool) {
        let mut out = Vec::new();
        let mut i = 0usize;
        let bytes = input.as_bytes();
        let mut incomplete = false;

        while i < bytes.len() {
            // Skip whitespace
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i >= bytes.len() {
                break;
            }
            let start = i;
            let opener = bytes[i];
            if opener != b'{' && opener != b'[' {
                break;
            }

            // Balance braces/brackets, handling strings and escapes
            let mut depth = 0i32;
            let mut in_string = false;
            let mut esc = false;
            while i < bytes.len() {
                let c = bytes[i] as char;
                if in_string {
                    if esc {
                        esc = false;
                    } else if c == '\\' {
                        esc = true;
                    } else if c == '"' {
                        in_string = false;
                    }
                } else {
                    match c {
                        '"' => in_string = true,
                        '{' | '[' => depth += 1,
                        '}' | ']' => {
                            depth -= 1;
                            if depth == 0 {
                                i += 1;
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                i += 1;
            }

            if depth != 0 {
                incomplete = true;
                break;
            }
            let end = i; // slice is [start, end)
            let slice = &input[start..end];
            // Trust the brace-balanced scanner - validation happens later in the parser
            out.push(slice.to_string());
            // Continue; next iteration will skip whitespace
        }

        (out, incomplete)
    }

    /// Parse Docker log output with the new parser
    fn parse_log_output_with_parser(
        log_output: LogOutput,
        container_name: &str,
        session_id: Uuid,
        _session_mode: &crate::models::SessionMode,
        parser: &mut LogParser,
    ) -> LogEntry {
        let (raw_message, _is_stderr) = match log_output {
            LogOutput::StdOut { message } => (String::from_utf8_lossy(&message).to_string(), false),
            LogOutput::StdErr { message } => (String::from_utf8_lossy(&message).to_string(), true),
            LogOutput::Console { message } => {
                (String::from_utf8_lossy(&message).to_string(), false)
            }
            LogOutput::StdIn { message } => (String::from_utf8_lossy(&message).to_string(), false),
        };

        // Parse the log with our advanced parser
        let parsed_log = parser.parse_log(&raw_message);

        // Convert parsed log to LogEntry
        let level = match parsed_log.level {
            crate::components::log_parser::LogLevel::Error
            | crate::components::log_parser::LogLevel::Fatal => LogEntryLevel::Error,
            crate::components::log_parser::LogLevel::Warning => LogEntryLevel::Warn,
            crate::components::log_parser::LogLevel::Debug
            | crate::components::log_parser::LogLevel::Trace => LogEntryLevel::Debug,
            _ => LogEntryLevel::Info,
        };

        // Use the clean message from parser
        LogEntry::new_with_parsed_data(
            level,
            container_name.to_string(),
            parsed_log.clean_message.clone(),
            session_id,
            Some(parsed_log),
        )
    }

    /// Convert AgentEvent to multiple LogEntries for display using the widget system
    fn agent_event_to_log_entries(
        event: crate::agent_parsers::AgentEvent,
        container_name: &str,
        session_id: Uuid,
        message_router: &mut crate::widgets::MessageRouter,
    ) -> Vec<LogEntry> {
        // Use the message router to render the event

        // Render the event using the appropriate widget
        let output = message_router.route_event(event, container_name, session_id);

        // Convert widget output to LogEntry vector using the proper to_log_entries method
        let entries = output.to_log_entries();

        // Return entries as-is, even if empty (for filtered events)
        entries
    }

    /// Convert AgentEvent to LogEntry for display (backwards compatibility)
    fn agent_event_to_log_entry(
        event: crate::agent_parsers::AgentEvent,
        container_name: &str,
        session_id: Uuid,
    ) -> LogEntry {
        // Create a temporary router for backwards compatibility (used only in tests)
        let mut temp_router = crate::widgets::MessageRouter::new();
        // Use the new function and return the first entry
        let entries = Self::agent_event_to_log_entries(event, container_name, session_id, &mut temp_router);
        entries.into_iter().next().unwrap_or_else(|| {
            LogEntry::new(
                LogEntryLevel::Error,
                container_name.to_string(),
                "No log entry generated".to_string(),
            )
            .with_session(session_id)
        })
    }

    /// Legacy implementation of agent_event_to_log_entry (kept for reference)
    #[allow(dead_code)]
    fn agent_event_to_log_entry_legacy(
        event: crate::agent_parsers::AgentEvent,
        container_name: &str,
        session_id: Uuid,
    ) -> LogEntry {
        use crate::agent_parsers::AgentEvent;

        match event {
            AgentEvent::SessionInfo {
                model,
                tools,
                session_id: sid,
                mcp_servers,
            } => {
                let mut info = format!("📊 Session: {} | {} tools available", model, tools.len());
                if let Some(servers) = mcp_servers {
                    info.push_str(&format!(
                        " | MCP: {}",
                        servers
                            .iter()
                            .map(|s| format!("{} ({})", s.name, s.status))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
                LogEntry::new(LogEntryLevel::Info, container_name.to_string(), info)
                    .with_session(session_id)
                    .with_metadata("event_type", "session_info")
                    .with_metadata("session_id", &sid)
            }

            AgentEvent::Thinking { content } => LogEntry::new(
                LogEntryLevel::Debug,
                container_name.to_string(),
                format!("💭 {}", content),
            )
            .with_session(session_id)
            .with_metadata("event_type", "thinking"),

            AgentEvent::Message { content, id } => LogEntry::new(
                LogEntryLevel::Info,
                container_name.to_string(),
                format!("💬 {}", content),
            )
            .with_session(session_id)
            .with_metadata("event_type", "message")
            .with_metadata("message_id", &id.unwrap_or_default()),

            AgentEvent::StreamingText { delta, message_id } => {
                LogEntry::new(LogEntryLevel::Info, container_name.to_string(), delta)
                    .with_session(session_id)
                    .with_metadata("event_type", "streaming")
                    .with_metadata("message_id", &message_id.unwrap_or_default())
            }

            AgentEvent::ToolCall {
                id,
                name,
                input,
                description,
            } => {
                // Build a REPL-like message: a headline with the tool name/desc
                // and, if present, a separate command line for clarity.
                let mut msg = String::new();
                let desc = description.unwrap_or_default();

                // Special handling for TodoWrite
                if name == "TodoWrite" {
                    msg.push_str("📝 TodoWrite: Updating task list");
                    if let Some(todos_val) = input.get("todos").and_then(|t| t.as_array()) {
                        let mut pending = 0u32;
                        let mut in_progress = 0u32;
                        let mut done = 0u32;

                        for t in todos_val {
                            let status = t.get("status").and_then(|x| x.as_str()).unwrap_or("pending");
                            match status {
                                "completed" | "done" => done += 1,
                                "in_progress" => in_progress += 1,
                                _ => pending += 1,
                            }
                        }

                        msg.push_str(&format!(
                            "\n  Σ {} tasks • {} pending • {} ⏳ • {} ☑",
                            todos_val.len(),
                            pending,
                            in_progress,
                            done
                        ));
                    }
                } else if !desc.is_empty() {
                    msg.push_str(&format!("🔧 {}: {}", name, desc));
                } else {
                    msg.push_str(&format!("🔧 {}", name));
                }

                // Add command/query details for non-TodoWrite tools
                if name != "TodoWrite" {
                    if let Some(cmd) = input.get("command").and_then(|v| v.as_str()) {
                        if !msg.is_empty() {
                            msg.push('\n');
                        }
                        msg.push_str(&format!("💻 Command: {}", cmd));
                    } else if let Some(query) = input.get("query").and_then(|v| v.as_str()) {
                        if !msg.is_empty() {
                            msg.push('\n');
                        }
                        msg.push_str(&format!("🔎 Query: {}", query));
                    } else if desc.is_empty() {
                        // Fall back to showing the raw input JSON if nothing else was available
                        msg.push_str(&format!(": {}", input));
                    }
                }

                LogEntry::new(LogEntryLevel::Info, container_name.to_string(), msg)
                    .with_session(session_id)
                    .with_metadata("event_type", "tool_call")
                    .with_metadata("tool_id", &id)
                    .with_metadata("tool_name", &name)
            }

            AgentEvent::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                let (level, prefix) = if is_error {
                    (LogEntryLevel::Error, "❌")
                } else {
                    (LogEntryLevel::Info, "✅")
                };
                // Truncate very long results safely on char boundaries
                fn truncate_display(s: &str, max_chars: usize) -> String {
                    let mut out = String::with_capacity(s.len().min(max_chars));
                    for (i, ch) in s.chars().enumerate() {
                        if i >= max_chars {
                            break;
                        }
                        out.push(ch);
                    }
                    if s.chars().count() > max_chars {
                        out.push_str("... (truncated)");
                    }
                    out
                }
                let display_content = truncate_display(&content, 500);
                LogEntry::new(
                    level,
                    container_name.to_string(),
                    format!("{} Result: {}", prefix, display_content),
                )
                .with_session(session_id)
                .with_metadata("event_type", "tool_result")
                .with_metadata("tool_use_id", &tool_use_id)
            }

            AgentEvent::Error { message, code } => LogEntry::new(
                LogEntryLevel::Error,
                container_name.to_string(),
                format!("❌ Error: {}", message),
            )
            .with_session(session_id)
            .with_metadata("event_type", "error")
            .with_metadata("error_code", &code.unwrap_or_default()),

            AgentEvent::Usage { .. } => {
                return LogEntry::new(
                    LogEntryLevel::Debug,
                    container_name.to_string(),
                    "".to_string()
                ).with_session(session_id);
            }

            AgentEvent::Custom { event_type, data } => LogEntry::new(
                LogEntryLevel::Info,
                container_name.to_string(),
                format!("📌 {}: {}", event_type, data),
            )
            .with_session(session_id)
            .with_metadata("event_type", "custom")
            .with_metadata("custom_type", &event_type),

            AgentEvent::Structured(payload) => {
                use crate::agent_parsers::types::StructuredPayload;

                let (level, icon, message) = match payload {
                    StructuredPayload::TodoList {
                        title,
                        items,
                        pending,
                        in_progress,
                        done,
                    } => {
                        // Create a proper multi-line todo list display
                        let mut lines = Vec::new();

                        // Title
                        if let Some(t) = title {
                            lines.push(format!("📝 {}", t));
                        } else {
                            lines.push("📝 Todos".to_string());
                        }

                        // Show each todo item on its own line
                        for item in items.iter() {
                            let icon = match item.status.as_str() {
                                "done" | "completed" => "☑",
                                "in_progress" | "active" => "⏳",
                                _ => "◻︎",
                            };
                            lines.push(format!("  {} {}", icon, item.text));
                        }

                        // Add summary line
                        lines.push(format!(
                            "  Σ {} • {} pending • {} ⏳ • {} ☑",
                            items.len(),
                            pending,
                            in_progress,
                            done
                        ));

                        // Join with newlines for multi-line display
                        let msg = lines.join("\n");

                        (LogEntryLevel::Info, "📝", msg)
                    }

                    StructuredPayload::GlobResults { paths, total } => {
                        let mut msg = format!("📂 Found {} files\n", total);

                        // Show first 15 paths
                        for path in paths.iter().take(15) {
                            msg.push_str(&format!("  • {}\n", path));
                        }

                        if paths.len() > 15 {
                            msg.push_str(&format!("  … +{} more", paths.len() - 15));
                        }

                        (LogEntryLevel::Info, "📂", msg)
                    }

                    StructuredPayload::PrettyJson(json_str) => {
                        (LogEntryLevel::Info, "📋", format!("📋 Data:\n{}", json_str))
                    }
                };

                LogEntry::new(level, container_name.to_string(), message)
                    .with_session(session_id)
                    .with_metadata("event_type", "structured")
                    .with_metadata("icon", icon)
            }
        }
    }

    /// Legacy parse method (kept for compatibility)
    fn parse_log_output(
        log_output: LogOutput,
        container_name: &str,
        session_id: Uuid,
        session_mode: &crate::models::SessionMode,
    ) -> LogEntry {
        let (message, is_stderr) = match log_output {
            LogOutput::StdOut { message } => (String::from_utf8_lossy(&message).to_string(), false),
            LogOutput::StdErr { message } => (String::from_utf8_lossy(&message).to_string(), true),
            LogOutput::Console { message } => {
                (String::from_utf8_lossy(&message).to_string(), false)
            }
            LogOutput::StdIn { message } => (String::from_utf8_lossy(&message).to_string(), false),
        };

        // Clean up the message (remove trailing newlines)
        let message = message.trim_end().to_string();

        // Use boss mode parsing if this is a boss mode session
        let is_boss_mode = matches!(session_mode, crate::models::SessionMode::Boss);

        if is_boss_mode {
            LogEntry::from_docker_log_with_mode(container_name, &message, Some(session_id), true)
        } else {
            // Determine log level based on content and stream type for interactive mode
            let level = if is_stderr {
                LogEntryLevel::Error
            } else {
                LogEntry::parse_level_from_message(&message)
            };

            LogEntry::new(level, container_name.to_string(), message).with_session(session_id)
        }
    }

    /// Start streaming logs for all active sessions
    pub async fn start_streaming_for_sessions(
        &mut self,
        sessions: &[(Uuid, String, String, crate::models::SessionMode)], // (session_id, container_id, container_name, session_mode)
    ) -> Result<()> {
        for (session_id, container_id, container_name, session_mode) in sessions {
            if let Err(e) = self
                .start_streaming(
                    *session_id,
                    container_id.clone(),
                    container_name.clone(),
                    session_mode.clone(),
                )
                .await
            {
                warn!(
                    "Failed to start log streaming for session {}: {}",
                    session_id, e
                );
            }
        }
        Ok(())
    }
}

impl Drop for DockerLogStreamingManager {
    fn drop(&mut self) {
        // Abort all streaming tasks when manager is dropped
        for (_, task) in self.streaming_tasks.drain() {
            task.task_handle.abort();
        }
    }
}

/// Log streaming coordinator for the application
#[derive(Debug)]
pub struct LogStreamingCoordinator {
    manager: Option<DockerLogStreamingManager>,
    log_receiver: mpsc::UnboundedReceiver<(Uuid, LogEntry)>,
}

impl LogStreamingCoordinator {
    /// Create a new coordinator with channels for log communication
    pub fn new() -> (Self, mpsc::UnboundedSender<(Uuid, LogEntry)>) {
        let (log_sender, log_receiver) = mpsc::unbounded_channel();

        (
            Self {
                manager: None,
                log_receiver,
            },
            log_sender,
        )
    }

    /// Initialize the streaming manager
    pub fn init_manager(
        &mut self,
        log_sender: mpsc::UnboundedSender<(Uuid, LogEntry)>,
    ) -> Result<()> {
        self.manager = Some(DockerLogStreamingManager::new(log_sender)?);
        Ok(())
    }

    /// Get the next log entry from any container (non-blocking)
    pub fn try_next_log(&mut self) -> Option<(Uuid, LogEntry)> {
        self.log_receiver.try_recv().ok()
    }

    /// Get the next log entry from any container (blocking)
    pub async fn next_log(&mut self) -> Option<(Uuid, LogEntry)> {
        self.log_receiver.recv().await
    }

    /// Start streaming for a session
    pub async fn start_streaming(
        &mut self,
        session_id: Uuid,
        container_id: String,
        container_name: String,
        session_mode: crate::models::SessionMode,
    ) -> Result<()> {
        if let Some(manager) = &mut self.manager {
            manager
                .start_streaming(session_id, container_id, container_name, session_mode)
                .await
        } else {
            Err(anyhow!("Log streaming manager not initialized"))
        }
    }

    /// Stop streaming for a session
    pub async fn stop_streaming(&mut self, session_id: Uuid) -> Result<()> {
        if let Some(manager) = &mut self.manager {
            manager.stop_streaming(session_id).await
        } else {
            Err(anyhow!("Log streaming manager not initialized"))
        }
    }

    /// Stop all streaming
    pub async fn stop_all(&mut self) -> Result<()> {
        if let Some(manager) = &mut self.manager {
            manager.stop_all_streaming().await
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_log_entry_parsing() {
        let container_name = "test-container";
        let session_id = Uuid::new_v4();

        // Test stdout parsing in interactive mode
        let stdout = LogOutput::StdOut {
            message: b"INFO: Test message\n".to_vec().into(),
        };
        let entry = DockerLogStreamingManager::parse_log_output(
            stdout,
            container_name,
            session_id,
            &crate::models::SessionMode::Interactive,
        );
        assert_eq!(entry.level, LogEntryLevel::Info);
        assert_eq!(entry.message, "INFO: Test message");

        // Test stderr parsing in interactive mode
        let stderr = LogOutput::StdErr {
            message: b"Error occurred\n".to_vec().into(),
        };
        let entry = DockerLogStreamingManager::parse_log_output(
            stderr,
            container_name,
            session_id,
            &crate::models::SessionMode::Interactive,
        );
        assert_eq!(entry.level, LogEntryLevel::Error);
        assert_eq!(entry.message, "Error occurred");

        // Test boss mode parsing (JSON fallback still works)
        let boss_stdout = LogOutput::StdOut {
            message: b"{\"type\": \"message\", \"content\": \"Hello from Claude!\"}\n"
                .to_vec()
                .into(),
        };
        let entry = DockerLogStreamingManager::parse_log_output(
            boss_stdout,
            container_name,
            session_id,
            &crate::models::SessionMode::Boss,
        );
        assert_eq!(entry.level, LogEntryLevel::Info);
        assert_eq!(entry.message, "🤖 Claude: Hello from Claude!");
        assert_eq!(entry.source, "claude-boss");
    }

    #[test]
    fn test_todo_write_formatting() {
        // Test that TodoWrite tool calls are formatted nicely
        let tool_call = crate::agent_parsers::AgentEvent::ToolCall {
            id: "test_123".to_string(),
            name: "TodoWrite".to_string(),
            input: serde_json::json!({
                "todos": [
                    {"content": "Write tests", "status": "completed"},
                    {"content": "Fix bugs", "status": "in_progress"},
                    {"content": "Deploy", "status": "pending"},
                    {"content": "Document", "status": "pending"}
                ]
            }),
            description: None,
        };

        let entry = DockerLogStreamingManager::agent_event_to_log_entry(
            tool_call,
            "test-container",
            uuid::Uuid::new_v4(),
        );

        // The new widget system produces a cleaner todo summary format
        assert!(entry.message.contains("📝 Todos"));
        assert!(entry.message.contains("Σ 4 tasks"));
        assert!(entry.message.contains("2 pending"));
        assert!(entry.message.contains("1 ⏳"));
        assert!(entry.message.contains("1 ☑"));
    }

    #[test]
    fn repl_render_for_tool_use() {
        // Build a ToolCall event resembling the user's example
        let event = crate::agent_parsers::AgentEvent::ToolCall {
            id: "toolu_01ABQEDLEQpqdHAYimuPszpk".to_string(),
            name: "Bash".to_string(),
            input: serde_json::json!({
                "command": "cargo test --quiet 2>&1 | tail -10",
                "description": "Run tests to check current status"
            }),
            description: Some("Run tests to check current status".to_string()),
        };

        let entry =
            DockerLogStreamingManager::agent_event_to_log_entry(event, "container", Uuid::nil());

        // The new widget system produces a cleaner format without the emoji
        assert!(entry.message.contains("Bash: Run tests to check current status"));
    }
}
