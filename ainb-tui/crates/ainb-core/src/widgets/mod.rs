// ABOUTME: Widget system for rich message rendering in the TUI
// Provides composable widgets for different tool and message types

#![allow(dead_code)]

use crate::agent_parsers::AgentEvent;
use crate::components::live_logs_stream::{LogEntry, LogEntryLevel};
use serde_json::Value;
use uuid::Uuid;
use crossterm::terminal;

pub mod bash_widget;
pub mod edit_widget;
pub mod todo_widget;
pub mod default_widget;
pub mod read_widget;
pub mod write_widget;
pub mod grep_widget;
pub mod glob_widget;
pub mod task_widget;
pub mod websearch_widget;
pub mod webfetch_widget;
pub mod thinking_widget;
pub mod result_parser;
pub mod syntax_highlighter;
pub mod message_router;
pub mod multiedit_widget;
pub mod mcp_widget;
pub mod ls_result_widget;
pub mod system_reminder_widget;
pub mod tool_result_store;
pub mod unified_message;
pub mod reminder_filter;

pub use bash_widget::BashWidget;
pub use edit_widget::EditWidget;
pub use todo_widget::TodoWidget;
pub use default_widget::DefaultWidget;
pub use read_widget::ReadWidget;
pub use write_widget::WriteWidget;
pub use grep_widget::GrepWidget;
pub use glob_widget::GlobWidget;
pub use task_widget::TaskWidget;
pub use websearch_widget::WebSearchWidget;
pub use webfetch_widget::WebFetchWidget;
pub use thinking_widget::ThinkingWidget;
pub use message_router::MessageRouter;
pub use multiedit_widget::MultiEditWidget;
pub use mcp_widget::McpWidget;
pub use ls_result_widget::LsResultWidget;
pub use system_reminder_widget::SystemReminderWidget;
pub use tool_result_store::ToolResultStore;
#[allow(unused_imports)]
pub use unified_message::{UnifiedMessage, MessageType, ContentBlock};
pub use reminder_filter::ReminderFilter;

/// Truncate a string to at most `max_chars` displayed characters,
/// appending the ellipsis glyph `…` when truncation occurs. Returns
/// the input borrowed when no truncation is needed.
///
/// Char-safe: counts and slices on Unicode scalars, so multi-byte
/// strings (branch names, project paths) cannot land mid-codepoint.
/// `max_chars == 0` returns an empty string. `max_chars == 1` returns
/// just `…`.
///
/// Use this in place of the per-file `truncate_string` / `truncate`
/// copies that previously lived in components/cli modules.
pub fn truncate_with_ellipsis(s: &str, max_chars: usize) -> std::borrow::Cow<'_, str> {
    if max_chars == 0 {
        return std::borrow::Cow::Borrowed("");
    }
    if s.chars().count() <= max_chars {
        return std::borrow::Cow::Borrowed(s);
    }
    let take = max_chars.saturating_sub(1);
    let mut out: String = s.chars().take(take).collect();
    out.push('…');
    std::borrow::Cow::Owned(out)
}

/// Output from widget rendering
#[derive(Debug, Clone)]
pub enum WidgetOutput {
    /// Simple single-line log entry
    Simple(LogEntry),
    /// Multi-line log entries for complex displays
    MultiLine(Vec<LogEntry>),
    /// Hierarchical display with header and nested content
    Hierarchical {
        header: Vec<LogEntry>,
        content: Vec<LogEntry>,
        collapsed: bool,
    },
    /// Interactive component (future feature)
    Interactive(InteractiveComponent),
}

/// Interactive component for future TUI enhancements
#[derive(Debug, Clone)]
pub struct InteractiveComponent {
    pub base_entry: LogEntry,
    pub actions: Vec<InteractiveAction>,
    pub expanded: bool,
}

#[derive(Debug, Clone)]
pub enum InteractiveAction {
    ExpandCollapse,
    CopyToClipboard,
    Rerun,
    ViewDetails,
}

/// Tool result data structure
#[derive(Debug, Clone)]
pub struct ToolResult {
    pub tool_use_id: String,
    pub content: Value,
    pub is_error: bool,
}

/// Trait for message widgets that render AgentEvents
pub trait MessageWidget: Send + Sync {
    /// Check if this widget can handle the given event
    fn can_handle(&self, event: &AgentEvent) -> bool;

    /// Render the event into widget output
    fn render(&self, event: AgentEvent, container_name: &str, session_id: Uuid) -> WidgetOutput;

    /// Render the event with an associated tool result
    fn render_with_result(&self, event: AgentEvent, _result: Option<ToolResult>, container_name: &str, session_id: Uuid) -> WidgetOutput {
        // Default implementation just calls render without result
        self.render(event, container_name, session_id)
    }

    /// Get the widget name for debugging
    fn name(&self) -> &'static str;
}

/// Registry for managing all available widgets
pub struct WidgetRegistry {
    widgets: Vec<Box<dyn MessageWidget>>,
    fallback: Box<dyn MessageWidget>,
}

impl WidgetRegistry {
    /// Create a new widget registry with all available widgets
    pub fn new() -> Self {
        Self {
            widgets: vec![
                // Prioritize more specific widgets first
                Box::new(ThinkingWidget::new()),
                Box::new(TodoWidget::new()),
                Box::new(BashWidget::new()),
                Box::new(EditWidget::new()),
                Box::new(ReadWidget::new()),
                Box::new(WriteWidget::new()),
                Box::new(GrepWidget::new()),
                Box::new(GlobWidget::new()),
                Box::new(TaskWidget::new()),
                Box::new(WebSearchWidget::new()),
                Box::new(WebFetchWidget::new()),
            ],
            fallback: Box::new(DefaultWidget::new()),
        }
    }

    /// Register a new widget
    pub fn register(&mut self, widget: Box<dyn MessageWidget>) {
        self.widgets.push(widget);
    }

    /// Render an event using the appropriate widget
    pub fn render(&self, event: AgentEvent, container_name: &str, session_id: Uuid) -> WidgetOutput {
        // Find the first widget that can handle this event
        for widget in &self.widgets {
            if widget.can_handle(&event) {
                return widget.render(event, container_name, session_id);
            }
        }

        // Fall back to default widget
        self.fallback.render(event, container_name, session_id)
    }
}

/// Helper functions for widgets
pub mod helpers {
    use super::*;

    /// Create a log entry with common fields
    pub fn create_log_entry(
        level: LogEntryLevel,
        container_name: &str,
        message: String,
        session_id: Uuid,
        event_type: &str,
    ) -> LogEntry {
        LogEntry::new(level, container_name.to_string(), message)
            .with_session(session_id)
            .with_metadata("event_type", event_type)
    }

    /// Truncate text safely on char boundaries
    pub fn truncate_text(s: &str, max_chars: usize) -> String {
        let mut result = String::with_capacity(s.len().min(max_chars));
        for (i, ch) in s.chars().enumerate() {
            if i >= max_chars {
                break;
            }
            result.push(ch);
        }
        if s.chars().count() > max_chars {
            result.push_str("... (truncated)");
        }
        result
    }

    /// Format a command with proper escaping for display
    pub fn format_command(cmd: &str) -> String {
        // Add syntax highlighting hints in the future
        cmd.to_string()
    }

    /// Add a blank line separator for visual spacing
    pub fn create_separator(container_name: &str, session_id: Uuid) -> LogEntry {
        LogEntry::new(
            LogEntryLevel::Debug,
            container_name.to_string(),
            String::new(),  // Empty line for spacing
        )
        .with_session(session_id)
        .with_metadata("event_type", "separator")
    }

    /// Generate a dynamic separator line based on terminal width
    pub fn create_dynamic_separator(label: &str, min_width: usize) -> String {
        // Try to get terminal width, fallback to 80 if unable
        let terminal_width = terminal::size()
            .map(|(width, _)| width as usize)
            .unwrap_or(80);

        // Ensure we have a minimum width to work with
        let effective_width = terminal_width.max(min_width);

        // Calculate available space for dashes
        // "╰─ " (3 chars) + label + " " (1 char) + remaining dashes
        let prefix = "╰─ ";
        let suffix = " ";
        let fixed_chars = prefix.len() + label.len() + suffix.len();

        if effective_width <= fixed_chars {
            // If terminal is too narrow, just return basic separator
            format!("{}{}", prefix, label)
        } else {
            let dash_count = effective_width - fixed_chars;
            format!("{}{}{}{}", prefix, label, suffix, "─".repeat(dash_count))
        }
    }
}

impl Default for WidgetRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl WidgetOutput {
    /// Convert widget output to log entries for display
    pub fn to_log_entries(self) -> Vec<LogEntry> {
        match self {
            WidgetOutput::Simple(entry) => vec![entry],
            WidgetOutput::MultiLine(entries) => entries,
            WidgetOutput::Hierarchical { header, content, collapsed } => {
                let mut entries = header;
                if !collapsed && !content.is_empty() {
                    // Add visual separator and content box with dynamic width
                    let separator_line = helpers::create_dynamic_separator("Result", 50);
                    entries.push(LogEntry::new(
                        LogEntryLevel::Debug,
                        String::new(),
                        separator_line,
                    ));

                    // Indent content entries with visual box guides
                    for (i, entry) in content.iter().enumerate() {
                        let mut indented = entry.clone();

                        // Use consistent indentation for content
                        if i == 0 {
                            indented.message = format!("   ╭─ {}", entry.message);
                        } else if i == content.len() - 1 {
                            indented.message = format!("   ╰─ {}", entry.message);
                        } else {
                            indented.message = format!("   │  {}", entry.message);
                        }
                        entries.push(indented);
                    }
                }
                entries
            }
            WidgetOutput::Interactive(component) => {
                // For now, just return the base entry
                vec![component.base_entry]
            }
        }
    }
}
