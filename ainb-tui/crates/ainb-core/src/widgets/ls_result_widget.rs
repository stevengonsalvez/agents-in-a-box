// ABOUTME: Widget for rendering directory listing (ls) results
// Displays directory trees and file listings in a structured format

use crate::agent_parsers::AgentEvent;
use crate::components::live_logs_stream::LogEntryLevel;
use uuid::Uuid;

use super::{MessageWidget, WidgetOutput, helpers};

pub struct LsResultWidget;

impl LsResultWidget {
    pub fn new() -> Self {
        Self
    }

    /// Check if content looks like an LS result
    fn is_ls_result(content: &str) -> bool {
        // Check for tree structure patterns
        let lines: Vec<&str> = content.lines().collect();

        // Look for tree-like indentation patterns
        let has_tree_structure = lines.iter().any(|line| {
            line.starts_with("├──") ||
            line.starts_with("└──") ||
            line.starts_with("│   ") ||
            line.trim().starts_with("- ")
        });

        // Look for the note at the end
        let has_note = lines.iter().any(|line|
            line.contains("NOTE: do any of the files")
        );

        has_tree_structure || has_note
    }

    /// Format directory tree for display
    fn format_tree_output(content: &str) -> Vec<String> {
        let mut formatted = Vec::new();

        for line in content.lines() {
            // Preserve tree structure but add slight indentation
            if line.starts_with("├──") || line.starts_with("└──") {
                formatted.push(format!("   {}", line));
            } else if line.starts_with("│   ") {
                formatted.push(format!("   {}", line));
            } else if line.trim().starts_with("- ") {
                // Bullet point style listing
                formatted.push(format!("   {}", line));
            } else if line.trim().is_empty() {
                formatted.push(String::new());
            } else if line.contains("NOTE:") {
                // Skip the NOTE line
                break;
            } else {
                // Regular line
                formatted.push(format!("   {}", line));
            }
        }

        formatted
    }
}

impl MessageWidget for LsResultWidget {
    fn can_handle(&self, event: &AgentEvent) -> bool {
        // This widget handles tool results that look like LS output
        if let AgentEvent::ToolResult { content, .. } = event {
            return Self::is_ls_result(content);
        }
        false
    }

    fn render(&self, event: AgentEvent, container_name: &str, session_id: Uuid) -> WidgetOutput {
        let mut entries = Vec::new();

        // Extract content from the event
        if let AgentEvent::ToolResult { content, .. } = event {
            // Header
            entries.push(helpers::create_log_entry(
                LogEntryLevel::Info,
                container_name,
                "📁 Directory Contents:".to_string(),
                session_id,
                "ls_result",
            ));

            // Format and display the tree
            let formatted_lines = Self::format_tree_output(&content);
            for line in formatted_lines {
                if line.is_empty() {
                    entries.push(helpers::create_separator(container_name, session_id));
                } else {
                    entries.push(helpers::create_log_entry(
                        LogEntryLevel::Debug,
                        container_name,
                        line,
                        session_id,
                        "ls_result",
                    ));
                }
            }
        }

        // Add separator for visual clarity
        entries.push(helpers::create_separator(container_name, session_id));

        WidgetOutput::MultiLine(entries)
    }

    fn name(&self) -> &'static str {
        "LsResultWidget"
    }
}

impl Default for LsResultWidget {
    fn default() -> Self {
        Self::new()
    }
}