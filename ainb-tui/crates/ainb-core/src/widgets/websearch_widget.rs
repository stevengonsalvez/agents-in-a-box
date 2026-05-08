// ABOUTME: Widget for rendering WebSearch tool calls showing web search operations
// Displays search queries and domains

use super::{MessageWidget, WidgetOutput, ToolResult, helpers, result_parser};
use crate::agent_parsers::AgentEvent;
use crate::components::live_logs_stream::{LogEntry, LogEntryLevel};
use uuid::Uuid;

pub struct WebSearchWidget;

impl WebSearchWidget {
    pub fn new() -> Self {
        Self
    }
}

impl MessageWidget for WebSearchWidget {
    fn can_handle(&self, event: &AgentEvent) -> bool {
        matches!(event, AgentEvent::ToolCall { name, .. } if name == "WebSearch")
    }

    fn render(&self, event: AgentEvent, container_name: &str, session_id: Uuid) -> WidgetOutput {
        if let AgentEvent::ToolCall { id, name: _, input, description } = event {
            let mut entries = Vec::new();

            // Extract search query
            let query = input.get("query")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            // Header with search query
            let header = format!("🌐 Web Search: \"{}\"", query);
            entries.push(
                helpers::create_log_entry(
                    LogEntryLevel::Info,
                    container_name,
                    header,
                    session_id,
                    "tool_call",
                )
                .with_metadata("tool_id", &id)
                .with_metadata("tool_name", "WebSearch")
                .with_metadata("query", query)
            );

            // Add allowed domains if specified
            if let Some(allowed) = input.get("allowed_domains").and_then(|v| v.as_array()) {
                if !allowed.is_empty() {
                    let domains: Vec<String> = allowed.iter()
                        .filter_map(|d| d.as_str())
                        .map(|s| s.to_string())
                        .collect();

                    entries.push(
                        LogEntry::new(
                            LogEntryLevel::Debug,
                            container_name.to_string(),
                            format!("  ✅ Allowed domains: {}", domains.join(", ")),
                        )
                        .with_session(session_id)
                        .with_metadata("event_type", "search_allowed_domains")
                    );
                }
            }

            // Add blocked domains if specified
            if let Some(blocked) = input.get("blocked_domains").and_then(|v| v.as_array()) {
                if !blocked.is_empty() {
                    let domains: Vec<String> = blocked.iter()
                        .filter_map(|d| d.as_str())
                        .map(|s| s.to_string())
                        .collect();

                    entries.push(
                        LogEntry::new(
                            LogEntryLevel::Debug,
                            container_name.to_string(),
                            format!("  ❌ Blocked domains: {}", domains.join(", ")),
                        )
                        .with_session(session_id)
                        .with_metadata("event_type", "search_blocked_domains")
                    );
                }
            }

            // Add search indicator
            entries.push(
                LogEntry::new(
                    LogEntryLevel::Info,
                    container_name.to_string(),
                    "  🔍 Searching the web...".to_string(),
                )
                .with_session(session_id)
                .with_metadata("event_type", "search_status")
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
                        .with_metadata("event_type", "search_description")
                    );
                }
            }

            WidgetOutput::MultiLine(entries)
        } else {
            WidgetOutput::Simple(
                helpers::create_log_entry(
                    LogEntryLevel::Error,
                    container_name,
                    "Invalid event for WebSearchWidget".to_string(),
                    session_id,
                    "error",
                )
            )
        }
    }

    fn render_with_result(&self, event: AgentEvent, result: Option<ToolResult>, container_name: &str, session_id: Uuid) -> WidgetOutput {
        if let AgentEvent::ToolCall { id, name: _, input, description: _ } = event {
            let mut header_entries = Vec::new();
            let mut content_entries = Vec::new();

            // Extract search query
            let query = input.get("query")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            // Header with search query
            let header = format!("🌐 Web Search: \"{}\"", query);
            let header_entry = helpers::create_log_entry(
                LogEntryLevel::Info,
                container_name,
                header,
                session_id,
                "tool_call",
            )
            .with_metadata("tool_id", &id)
            .with_metadata("tool_name", "WebSearch")
            .with_metadata("query", query);

            header_entries.push(header_entry);

            // Add allowed domains if specified
            if let Some(allowed) = input.get("allowed_domains").and_then(|v| v.as_array()) {
                if !allowed.is_empty() {
                    let domains: Vec<String> = allowed.iter()
                        .filter_map(|d| d.as_str())
                        .map(|s| s.to_string())
                        .collect();

                    header_entries.push(
                        LogEntry::new(
                            LogEntryLevel::Debug,
                            container_name.to_string(),
                            format!("✅ Allowed domains: {}", domains.join(", ")),
                        )
                        .with_session(session_id)
                        .with_metadata("event_type", "search_allowed_domains")
                        .with_metadata("tool_id", &id)
                    );
                }
            }

            // Add blocked domains if specified
            if let Some(blocked) = input.get("blocked_domains").and_then(|v| v.as_array()) {
                if !blocked.is_empty() {
                    let domains: Vec<String> = blocked.iter()
                        .filter_map(|d| d.as_str())
                        .map(|s| s.to_string())
                        .collect();

                    header_entries.push(
                        LogEntry::new(
                            LogEntryLevel::Debug,
                            container_name.to_string(),
                            format!("❌ Blocked domains: {}", domains.join(", ")),
                        )
                        .with_session(session_id)
                        .with_metadata("event_type", "search_blocked_domains")
                        .with_metadata("tool_id", &id)
                    );
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
                                .with_metadata("websearch_output", "true")
                            );
                        }
                    }
                } else if tool_result.is_error {
                    // Error with no content
                    content_entries.push(
                        LogEntry::new(
                            LogEntryLevel::Error,
                            container_name.to_string(),
                            "❌ Search failed with no output".to_string(),
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
                    "Invalid event for WebSearchWidget".to_string(),
                    session_id,
                    "error",
                )
            )
        }
    }

    fn name(&self) -> &'static str {
        "WebSearchWidget"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_websearch_widget_can_handle() {
        let widget = WebSearchWidget::new();

        let search_event = AgentEvent::ToolCall {
            id: "test".to_string(),
            name: "WebSearch".to_string(),
            input: json!({}),
            description: None,
        };
        assert!(widget.can_handle(&search_event));

        let other_event = AgentEvent::ToolCall {
            id: "test".to_string(),
            name: "WebFetch".to_string(),
            input: json!({}),
            description: None,
        };
        assert!(!widget.can_handle(&other_event));
    }

    #[test]
    fn test_websearch_widget_render() {
        let widget = WebSearchWidget::new();
        let event = AgentEvent::ToolCall {
            id: "search_123".to_string(),
            name: "WebSearch".to_string(),
            input: json!({
                "query": "Rust async programming",
                "allowed_domains": ["rust-lang.org", "docs.rs"],
                "blocked_domains": ["reddit.com"]
            }),
            description: Some("Research async patterns".to_string()),
        };

        let output = widget.render(event, "test-container", Uuid::nil());

        match output {
            WidgetOutput::MultiLine(entries) => {
                assert!(!entries.is_empty());
                assert!(entries[0].message.contains("🌐 Web Search"));
                assert!(entries[0].message.contains("Rust async programming"));
                assert!(entries.iter().any(|e| e.message.contains("rust-lang.org")));
                assert!(entries.iter().any(|e| e.message.contains("reddit.com")));
            }
            _ => panic!("Expected MultiLine output"),
        }
    }

    #[test]
    fn test_websearch_widget_render_with_result() {
        let widget = WebSearchWidget::new();
        let event = AgentEvent::ToolCall {
            id: "search_123".to_string(),
            name: "WebSearch".to_string(),
            input: json!({
                "query": "Rust async programming"
            }),
            description: Some("Research async patterns".to_string()),
        };

        let result = ToolResult {
            tool_use_id: "search_123".to_string(),
            content: json!({"content": "# Search Results\n\n* **Rust Async Book** - Comprehensive guide\n* **Tokio Documentation** - Runtime for async apps"}),
            is_error: false,
        };

        let output = widget.render_with_result(event, Some(result), "test-container", Uuid::nil());

        match output {
            WidgetOutput::Hierarchical { header, content, collapsed } => {
                assert!(!header.is_empty());
                assert!(header[0].message.contains("🌐 Web Search"));
                assert!(header[0].message.contains("Rust async programming"));
                assert!(!content.is_empty());
                assert!(!collapsed);
            }
            _ => panic!("Expected Hierarchical output"),
        }
    }

    #[test]
    fn test_websearch_widget_render_with_result_no_result() {
        let widget = WebSearchWidget::new();
        let event = AgentEvent::ToolCall {
            id: "search_123".to_string(),
            name: "WebSearch".to_string(),
            input: json!({
                "query": "Rust async programming"
            }),
            description: Some("Research async patterns".to_string()),
        };

        let output = widget.render_with_result(event, None, "test-container", Uuid::nil());

        match output {
            WidgetOutput::MultiLine(entries) => {
                assert!(!entries.is_empty());
                assert!(entries[0].message.contains("🌐 Web Search"));
                assert!(entries[0].message.contains("Rust async programming"));
            }
            _ => panic!("Expected MultiLine output when no result"),
        }
    }
}
