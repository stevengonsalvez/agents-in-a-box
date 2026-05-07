// ABOUTME: Widget for rendering WebFetch tool calls showing web content fetching
// Displays URLs and fetch operations

use super::{MessageWidget, WidgetOutput, ToolResult, helpers, result_parser};
use crate::agent_parsers::AgentEvent;
use crate::components::live_logs_stream::{LogEntry, LogEntryLevel};
use uuid::Uuid;

pub struct WebFetchWidget;

impl WebFetchWidget {
    pub fn new() -> Self {
        Self
    }
}

impl MessageWidget for WebFetchWidget {
    fn can_handle(&self, event: &AgentEvent) -> bool {
        matches!(event, AgentEvent::ToolCall { name, .. } if name == "WebFetch")
    }

    fn render(&self, event: AgentEvent, container_name: &str, session_id: Uuid) -> WidgetOutput {
        if let AgentEvent::ToolCall { id, name: _, input, description } = event {
            let mut entries = Vec::new();

            // Extract URL and prompt
            let url = input.get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            let prompt = input.get("prompt")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            // Header with URL
            let header = format!("🌍 Fetching: {}", url);
            entries.push(
                helpers::create_log_entry(
                    LogEntryLevel::Info,
                    container_name,
                    header,
                    session_id,
                    "tool_call",
                )
                .with_metadata("tool_id", &id)
                .with_metadata("tool_name", "WebFetch")
                .with_metadata("url", url)
            );

            // Add prompt if present
            if !prompt.is_empty() {
                entries.push(
                    LogEntry::new(
                        LogEntryLevel::Debug,
                        container_name.to_string(),
                        "  📋 Processing with prompt:".to_string(),
                    )
                    .with_session(session_id)
                    .with_metadata("event_type", "fetch_prompt")
                );

                // Show first few lines of prompt
                for line in prompt.lines().take(3) {
                    let truncated = if line.len() > 80 {
                        format!("{}...", &line[..80])
                    } else {
                        line.to_string()
                    };
                    entries.push(
                        LogEntry::new(
                            LogEntryLevel::Debug,
                            container_name.to_string(),
                            format!("     {}", truncated),
                        )
                        .with_session(session_id)
                        .with_metadata("event_type", "fetch_prompt_line")
                    );
                }

                if prompt.lines().count() > 3 {
                    entries.push(
                        LogEntry::new(
                            LogEntryLevel::Debug,
                            container_name.to_string(),
                            format!("     ... ({} more lines)", prompt.lines().count() - 3),
                        )
                        .with_session(session_id)
                        .with_metadata("event_type", "fetch_prompt_more")
                    );
                }
            }

            // Add status
            entries.push(
                LogEntry::new(
                    LogEntryLevel::Info,
                    container_name.to_string(),
                    "  ⬇️ Downloading content...".to_string(),
                )
                .with_session(session_id)
                .with_metadata("event_type", "fetch_status")
            );

            // Add description if present
            if let Some(desc) = description {
                if !desc.is_empty() {
                    entries.push(
                        LogEntry::new(
                            LogEntryLevel::Debug,
                            container_name.to_string(),
                            format!("  💭 {}", desc),
                        )
                        .with_session(session_id)
                        .with_metadata("event_type", "fetch_description")
                    );
                }
            }

            WidgetOutput::MultiLine(entries)
        } else {
            WidgetOutput::Simple(
                helpers::create_log_entry(
                    LogEntryLevel::Error,
                    container_name,
                    "Invalid event for WebFetchWidget".to_string(),
                    session_id,
                    "error",
                )
            )
        }
    }

    fn render_with_result(&self, event: AgentEvent, result: Option<ToolResult>, container_name: &str, session_id: Uuid) -> WidgetOutput {
        if let AgentEvent::ToolCall { id, name: _, input, description } = event {
            let mut header_entries = Vec::new();
            let mut content_entries = Vec::new();

            // Extract URL and prompt
            let url = input.get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            let prompt = input.get("prompt")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            // Build the main header message
            let main_msg = format!("🌍 WebFetch: {}", url);

            // Create the header entry
            let header_entry = helpers::create_log_entry(
                LogEntryLevel::Info,
                container_name,
                main_msg,
                session_id,
                "tool_call",
            )
            .with_metadata("tool_id", &id)
            .with_metadata("tool_name", "WebFetch")
            .with_metadata("url", url);

            header_entries.push(header_entry);

            // Add prompt information to header if present
            if !prompt.is_empty() {
                let prompt_entry = LogEntry::new(
                    LogEntryLevel::Info,
                    container_name.to_string(),
                    format!("📋 Processing with prompt: {}",
                        if prompt.len() > 100 {
                            format!("{}...", &prompt[..100])
                        } else {
                            prompt.to_string()
                        }),
                )
                .with_session(session_id)
                .with_metadata("event_type", "fetch_prompt")
                .with_metadata("tool_id", &id);

                header_entries.push(prompt_entry);
            }

            // Add description if present
            if let Some(desc) = description {
                if !desc.is_empty() {
                    let desc_entry = LogEntry::new(
                        LogEntryLevel::Info,
                        container_name.to_string(),
                        format!("💭 {}", desc),
                    )
                    .with_session(session_id)
                    .with_metadata("event_type", "fetch_description")
                    .with_metadata("tool_id", &id);

                    header_entries.push(desc_entry);
                }
            }

            // Process result if available
            if let Some(tool_result) = result {
                // Extract result content
                if let Some(content_str) = result_parser::format_tool_result(&tool_result.content) {
                    // Check if the content looks like markdown
                    let is_markdown = content_str.contains('#') ||
                                     content_str.contains('*') ||
                                     content_str.contains('`') ||
                                     content_str.contains('\n');

                    if is_markdown {
                        // Parse as markdown
                        let parsed_entries = result_parser::parse_markdown_to_logs(
                            &content_str,
                            container_name,
                            session_id,
                            if tool_result.is_error { LogEntryLevel::Error } else { LogEntryLevel::Info },
                        );
                        content_entries.extend(parsed_entries);
                    } else {
                        // Simple text output
                        let level = if tool_result.is_error {
                            LogEntryLevel::Error
                        } else {
                            LogEntryLevel::Info
                        };

                        for line in content_str.lines() {
                            content_entries.push(
                                LogEntry::new(
                                    level,
                                    container_name.to_string(),
                                    line.to_string(),
                                )
                                .with_session(session_id)
                                .with_metadata("webfetch_output", "true")
                            );
                        }
                    }
                } else if tool_result.is_error {
                    // Error with no content
                    content_entries.push(
                        LogEntry::new(
                            LogEntryLevel::Error,
                            container_name.to_string(),
                            "❌ Web fetch failed with no output".to_string(),
                        )
                        .with_session(session_id)
                    );
                }

                // Return hierarchical output
                WidgetOutput::Hierarchical {
                    header: header_entries,
                    content: content_entries,
                    collapsed: false,
                }
            } else {
                // No result yet, just return the header
                WidgetOutput::MultiLine(header_entries)
            }
        } else {
            // Should not happen if can_handle works correctly
            WidgetOutput::Simple(
                helpers::create_log_entry(
                    LogEntryLevel::Error,
                    container_name,
                    "Invalid event for WebFetchWidget".to_string(),
                    session_id,
                    "error",
                )
            )
        }
    }

    fn name(&self) -> &'static str {
        "WebFetchWidget"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_webfetch_widget_can_handle() {
        let widget = WebFetchWidget::new();

        let fetch_event = AgentEvent::ToolCall {
            id: "test".to_string(),
            name: "WebFetch".to_string(),
            input: json!({}),
            description: None,
        };
        assert!(widget.can_handle(&fetch_event));

        let other_event = AgentEvent::ToolCall {
            id: "test".to_string(),
            name: "WebSearch".to_string(),
            input: json!({}),
            description: None,
        };
        assert!(!widget.can_handle(&other_event));
    }

    #[test]
    fn test_webfetch_widget_render() {
        let widget = WebFetchWidget::new();
        let event = AgentEvent::ToolCall {
            id: "fetch_123".to_string(),
            name: "WebFetch".to_string(),
            input: json!({
                "url": "https://docs.rust-lang.org",
                "prompt": "Extract information about async programming"
            }),
            description: Some("Get Rust async docs".to_string()),
        };

        let output = widget.render(event, "test-container", Uuid::nil());

        match output {
            WidgetOutput::MultiLine(entries) => {
                assert!(!entries.is_empty());
                assert!(entries[0].message.contains("🌍 Fetching"));
                assert!(entries[0].message.contains("docs.rust-lang.org"));
                assert!(entries.iter().any(|e| e.message.contains("async programming")));
                assert!(entries.iter().any(|e| e.message.contains("⬇️ Downloading")));
            }
            _ => panic!("Expected MultiLine output"),
        }
    }

    #[test]
    fn test_webfetch_widget_render_with_result() {
        let widget = WebFetchWidget::new();
        let event = AgentEvent::ToolCall {
            id: "fetch_123".to_string(),
            name: "WebFetch".to_string(),
            input: json!({
                "url": "https://example.com",
                "prompt": "Extract key information"
            }),
            description: Some("Test fetch".to_string()),
        };

        let tool_result = ToolResult {
            tool_use_id: "fetch_123".to_string(),
            content: json!({
                "content": "# Test Content\n\nThis is some **bold** text with `code`."
            }),
            is_error: false,
        };

        let output = widget.render_with_result(event, Some(tool_result), "test-container", Uuid::nil());

        match output {
            WidgetOutput::Hierarchical { header, content, collapsed } => {
                assert!(!header.is_empty());
                assert!(!content.is_empty());
                assert!(!collapsed);
                assert!(header[0].message.contains("🌍 WebFetch: https://example.com"));
                assert!(content.iter().any(|e| e.message.contains("# Test Content")));
            }
            _ => panic!("Expected Hierarchical output with result"),
        }
    }

    #[test]
    fn test_webfetch_widget_render_with_error_result() {
        let widget = WebFetchWidget::new();
        let event = AgentEvent::ToolCall {
            id: "fetch_123".to_string(),
            name: "WebFetch".to_string(),
            input: json!({
                "url": "https://invalid-url.com",
                "prompt": "Extract information"
            }),
            description: None,
        };

        let tool_result = ToolResult {
            tool_use_id: "fetch_123".to_string(),
            content: json!({}),
            is_error: true,
        };

        let output = widget.render_with_result(event, Some(tool_result), "test-container", Uuid::nil());

        match output {
            WidgetOutput::Hierarchical { header, content, collapsed: _ } => {
                assert!(!header.is_empty());
                assert!(!content.is_empty());
                assert!(header[0].message.contains("🌍 WebFetch"));
                assert!(content.iter().any(|e| e.message.contains("❌ Web fetch failed")));
            }
            _ => panic!("Expected Hierarchical output with error"),
        }
    }

    #[test]
    fn test_webfetch_widget_render_with_no_result() {
        let widget = WebFetchWidget::new();
        let event = AgentEvent::ToolCall {
            id: "fetch_123".to_string(),
            name: "WebFetch".to_string(),
            input: json!({
                "url": "https://example.com",
                "prompt": "Extract information"
            }),
            description: None,
        };

        let output = widget.render_with_result(event, None, "test-container", Uuid::nil());

        match output {
            WidgetOutput::MultiLine(entries) => {
                assert!(!entries.is_empty());
                assert!(entries[0].message.contains("🌍 WebFetch"));
                assert!(entries.iter().any(|e| e.message.contains("📋 Processing with prompt")));
            }
            _ => panic!("Expected MultiLine output with no result"),
        }
    }
}
