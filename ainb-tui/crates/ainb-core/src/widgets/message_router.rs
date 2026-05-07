// ABOUTME: Central message router for directing JSON events to appropriate widgets
// Similar to Opcode's StreamMessage.tsx, routes different message types to specialized widgets

#![allow(dead_code)]

use crate::agent_parsers::{AgentEvent, types::StructuredPayload};
use crate::components::live_logs_stream::LogEntryLevel;
use serde_json::Value;
use uuid::Uuid;
use tracing::debug;

use super::{
    MessageWidget, WidgetOutput, ToolResult, ToolResultStore,
    BashWidget, EditWidget, TodoWidget, DefaultWidget,
    ReadWidget, WriteWidget, GrepWidget, GlobWidget,
    TaskWidget, WebSearchWidget, WebFetchWidget, ThinkingWidget,
    MultiEditWidget, McpWidget, LsResultWidget, SystemReminderWidget,
    ReminderFilter, helpers,
};


/// Central message router that processes AgentEvents and tool results
pub struct MessageRouter {
    /// Thread-safe store for tool results and pending calls
    tool_result_store: ToolResultStore,

    /// Filter for reducing redundant system reminders
    reminder_filter: ReminderFilter,

    /// Specialized widgets for different message types
    bash_widget: BashWidget,
    edit_widget: EditWidget,
    todo_widget: TodoWidget,
    read_widget: ReadWidget,
    write_widget: WriteWidget,
    grep_widget: GrepWidget,
    glob_widget: GlobWidget,
    task_widget: TaskWidget,
    websearch_widget: WebSearchWidget,
    webfetch_widget: WebFetchWidget,
    thinking_widget: ThinkingWidget,
    default_widget: DefaultWidget,
    multiedit_widget: MultiEditWidget,
    mcp_widget: McpWidget,
    ls_result_widget: LsResultWidget,
    system_reminder_widget: SystemReminderWidget,
}

impl MessageRouter {
    pub fn new() -> Self {
        Self {
            tool_result_store: ToolResultStore::new(),
            reminder_filter: ReminderFilter::new(),
            bash_widget: BashWidget::new(),
            edit_widget: EditWidget::new(),
            todo_widget: TodoWidget::new(),
            read_widget: ReadWidget::new(),
            write_widget: WriteWidget::new(),
            grep_widget: GrepWidget::new(),
            glob_widget: GlobWidget::new(),
            task_widget: TaskWidget::new(),
            websearch_widget: WebSearchWidget::new(),
            webfetch_widget: WebFetchWidget::new(),
            thinking_widget: ThinkingWidget::new(),
            default_widget: DefaultWidget::new(),
            multiedit_widget: MultiEditWidget::new(),
            mcp_widget: McpWidget::new(),
            ls_result_widget: LsResultWidget::new(),
            system_reminder_widget: SystemReminderWidget::new(),
        }
    }

    /// Store a tool result for later matching with tool calls
    pub fn add_tool_result(&mut self, tool_use_id: String, content: String, is_error: bool) {
        debug!("Storing tool result for ID: {}", tool_use_id);

        let content_value = serde_json::from_str(&content).unwrap_or(Value::String(content));

        // Store using the ToolResultStore
        if let Err(e) = self.tool_result_store.store_result(tool_use_id, content_value, is_error) {
            debug!("Failed to store tool result: {}", e);
        }
    }

    /// Get a tool result by its ID
    fn get_tool_result(&self, tool_use_id: &str) -> Option<ToolResult> {
        self.tool_result_store.get_result(tool_use_id)
    }

    /// Register a tool call as pending
    fn register_tool_call(&self, id: String, name: String, input: Value) {
        if let Err(e) = self.tool_result_store.register_tool_call(id, name, input) {
            debug!("Failed to register tool call: {}", e);
        }
    }

    /// Route an event to the appropriate widget based on its type and content
    pub fn route_event(
        &mut self,
        event: AgentEvent,
        container_name: &str,
        session_id: Uuid,
    ) -> WidgetOutput {
        match event {
            AgentEvent::SessionInfo { model, tools, session_id: sid, mcp_servers } => {
                self.render_session_info(model, tools, sid, mcp_servers, container_name, session_id)
            }

            AgentEvent::Thinking { content } => {
                self.thinking_widget.render(
                    AgentEvent::Thinking { content },
                    container_name,
                    session_id
                )
            }

            AgentEvent::Message { content, id } => {
                self.render_assistant_message(content, id, container_name, session_id)
            }

            AgentEvent::StreamingText { delta, message_id } => {
                self.render_streaming_text(delta, message_id, container_name, session_id)
            }

            AgentEvent::ToolCall { id, name, input, description } => {
                self.route_tool_call(id, name, input, description, container_name, session_id)
            }

            AgentEvent::ToolResult { tool_use_id, content, is_error } => {
                // Store the result using ToolResultStore
                let content_value = Value::String(content.clone());
                if let Err(e) = self.tool_result_store.store_result(tool_use_id.clone(), content_value, is_error) {
                    debug!("Failed to store tool result: {}", e);
                }

                // Check if it's a special result type
                if self.ls_result_widget.can_handle(&AgentEvent::ToolResult {
                    tool_use_id: tool_use_id.clone(),
                    content: content.clone(),
                    is_error
                }) {
                    self.ls_result_widget.render(
                        AgentEvent::ToolResult { tool_use_id, content, is_error },
                        container_name,
                        session_id
                    )
                } else if content.contains("<system-reminder>") {
                    // Filter out all system reminders completely
                    WidgetOutput::MultiLine(vec![])
                } else {
                    self.render_tool_result(tool_use_id, content, is_error, container_name, session_id)
                }
            }

            AgentEvent::Error { message, code } => {
                self.render_error(message, code, container_name, session_id)
            }

            AgentEvent::Usage { .. } => {
                // Filter out usage events - return empty vector
                WidgetOutput::MultiLine(vec![])
            }

            AgentEvent::Custom { event_type, data } => {
                self.route_custom_event(event_type, data, container_name, session_id)
            }

            AgentEvent::Structured(payload) => {
                self.route_structured_payload(payload, container_name, session_id)
            }
        }
    }

    /// Route tool calls to specific widgets
    fn route_tool_call(
        &self,
        id: String,
        name: String,
        input: Value,
        description: Option<String>,
        container_name: &str,
        session_id: Uuid,
    ) -> WidgetOutput {
        // Register the tool call
        self.register_tool_call(id.clone(), name.clone(), input.clone());

        // Check if result already available
        let result = self.get_tool_result(&id);

        // Create the event for rendering
        let event = AgentEvent::ToolCall {
            id: id.clone(),
            name: name.clone(),
            input: input.clone(),
            description,
        };

        // Route based on tool name
        let widget_output = match name.to_lowercase().as_str() {
            "bash" => {
                if let Some(result) = result {
                    self.bash_widget.render_with_result(event, Some(result), container_name, session_id)
                } else {
                    self.bash_widget.render(event, container_name, session_id)
                }
            }
            "edit" => {
                if let Some(result) = result {
                    self.edit_widget.render_with_result(event, Some(result), container_name, session_id)
                } else {
                    self.edit_widget.render(event, container_name, session_id)
                }
            }
            "multiedit" => {
                if let Some(result) = result {
                    self.multiedit_widget.render_with_result(event, Some(result), container_name, session_id)
                } else {
                    self.multiedit_widget.render(event, container_name, session_id)
                }
            }
            "todowrite" => {
                if let Some(result) = result {
                    self.todo_widget.render_with_result(event, Some(result), container_name, session_id)
                } else {
                    self.todo_widget.render(event, container_name, session_id)
                }
            }
            "read" => {
                if let Some(result) = result {
                    self.read_widget.render_with_result(event, Some(result), container_name, session_id)
                } else {
                    self.read_widget.render(event, container_name, session_id)
                }
            }
            "write" => {
                if let Some(result) = result {
                    self.write_widget.render_with_result(event, Some(result), container_name, session_id)
                } else {
                    self.write_widget.render(event, container_name, session_id)
                }
            }
            "grep" => {
                if let Some(result) = result {
                    self.grep_widget.render_with_result(event, Some(result), container_name, session_id)
                } else {
                    self.grep_widget.render(event, container_name, session_id)
                }
            }
            "glob" => {
                if let Some(result) = result {
                    self.glob_widget.render_with_result(event, Some(result), container_name, session_id)
                } else {
                    self.glob_widget.render(event, container_name, session_id)
                }
            }
            "task" => {
                if let Some(result) = result {
                    self.task_widget.render_with_result(event, Some(result), container_name, session_id)
                } else {
                    self.task_widget.render(event, container_name, session_id)
                }
            }
            "websearch" => {
                if let Some(result) = result {
                    self.websearch_widget.render_with_result(event, Some(result), container_name, session_id)
                } else {
                    self.websearch_widget.render(event, container_name, session_id)
                }
            }
            "webfetch" => {
                if let Some(result) = result {
                    self.webfetch_widget.render_with_result(event, Some(result), container_name, session_id)
                } else {
                    self.webfetch_widget.render(event, container_name, session_id)
                }
            }
            name if name.starts_with("mcp__") => {
                if let Some(result) = result {
                    self.mcp_widget.render_with_result(event, Some(result), container_name, session_id)
                } else {
                    self.mcp_widget.render(event, container_name, session_id)
                }
            }
            _ => {
                // Unknown tool, use default
                self.default_widget.render(event, container_name, session_id)
            }
        };

        widget_output
    }

    /// Render session info
    fn render_session_info(
        &self,
        model: String,
        tools: Vec<String>,
        _sid: String,
        _mcp_servers: Option<Vec<crate::agent_parsers::types::McpServerInfo>>,
        container_name: &str,
        session_id: Uuid,
    ) -> WidgetOutput {
        let mut entries = Vec::new();

        entries.push(helpers::create_log_entry(
            LogEntryLevel::Info,
            container_name,
            "🚀 System Initialized".to_string(),
            session_id,
            "system_init",
        ));

        entries.push(helpers::create_log_entry(
            LogEntryLevel::Debug,
            container_name,
            format!("   Model: {}", model),
            session_id,
            "system_init",
        ));

        entries.push(helpers::create_log_entry(
            LogEntryLevel::Debug,
            container_name,
            format!("   Tools: {} available", tools.len()),
            session_id,
            "system_init",
        ));

        WidgetOutput::MultiLine(entries)
    }

    /// Render assistant message
    fn render_assistant_message(
        &self,
        content: String,
        _id: Option<String>,
        container_name: &str,
        session_id: Uuid,
    ) -> WidgetOutput {
        let entry = helpers::create_log_entry(
            LogEntryLevel::Info,
            container_name,
            format!("Claude: {}", content),
            session_id,
            "message",
        );

        WidgetOutput::Simple(entry)
    }

    /// Render streaming text
    fn render_streaming_text(
        &self,
        delta: String,
        _message_id: Option<String>,
        container_name: &str,
        session_id: Uuid,
    ) -> WidgetOutput {
        let entry = helpers::create_log_entry(
            LogEntryLevel::Info,
            container_name,
            delta,
            session_id,
            "streaming",
        );

        WidgetOutput::Simple(entry)
    }

    /// Render tool result
    fn render_tool_result(
        &self,
        tool_use_id: String,
        content: String,
        is_error: bool,
        container_name: &str,
        session_id: Uuid,
    ) -> WidgetOutput {
        // Filter out system-reminder content to prevent them from being displayed
        if content.contains("<system-reminder>") {
            let entry = helpers::create_log_entry(
                LogEntryLevel::Debug,
                container_name,
                "🔇 System reminder content filtered".to_string(),
                session_id,
                "reminder_filtered",
            );
            return WidgetOutput::Simple(entry);
        }

        let level = if is_error {
            LogEntryLevel::Error
        } else {
            LogEntryLevel::Info
        };

        let prefix = if is_error { "❌" } else { "✅" };

        let entry = helpers::create_log_entry(
            level,
            container_name,
            format!("{} Result [{}]: {}", prefix, tool_use_id, content),
            session_id,
            "tool_result",
        );

        WidgetOutput::Simple(entry)
    }

    /// Render error
    fn render_error(
        &self,
        message: String,
        code: Option<String>,
        container_name: &str,
        session_id: Uuid,
    ) -> WidgetOutput {
        let error_text = if let Some(code) = code {
            format!("❌ Error [{}]: {}", code, message)
        } else {
            format!("❌ Error: {}", message)
        };

        let entry = helpers::create_log_entry(
            LogEntryLevel::Error,
            container_name,
            error_text,
            session_id,
            "error",
        );

        WidgetOutput::Simple(entry)
    }

    /// Render usage statistics
    fn render_usage(
        &self,
        input_tokens: u32,
        output_tokens: u32,
        cache_tokens: Option<u32>,
        total_cost: Option<f64>,
        container_name: &str,
        session_id: Uuid,
    ) -> WidgetOutput {
        let mut entries = Vec::new();

        entries.push(helpers::create_log_entry(
            LogEntryLevel::Debug,
            container_name,
            format!("📊 Usage: {} in, {} out", input_tokens, output_tokens),
            session_id,
            "usage",
        ));

        if let Some(cache) = cache_tokens {
            entries.push(helpers::create_log_entry(
                LogEntryLevel::Debug,
                container_name,
                format!("   Cache: {} tokens", cache),
                session_id,
                "usage",
            ));
        }

        if let Some(cost) = total_cost {
            entries.push(helpers::create_log_entry(
                LogEntryLevel::Debug,
                container_name,
                format!("   Cost: ${:.4}", cost),
                session_id,
                "usage",
            ));
        }

        WidgetOutput::MultiLine(entries)
    }

    /// Route custom events
    fn route_custom_event(
        &self,
        event_type: String,
        data: Value,
        container_name: &str,
        session_id: Uuid,
    ) -> WidgetOutput {
        // Filter out system_reminder events to prevent them from being displayed
        if event_type == "system_reminder" {
            return WidgetOutput::MultiLine(vec![]);
        }

        let entry = helpers::create_log_entry(
            LogEntryLevel::Info,
            container_name,
            format!("Custom [{}]: {:?}", event_type, data),
            session_id,
            "custom",
        );

        WidgetOutput::Simple(entry)
    }

    /// Route structured payloads
    fn route_structured_payload(
        &self,
        payload: StructuredPayload,
        container_name: &str,
        session_id: Uuid,
    ) -> WidgetOutput {
        match payload {
            StructuredPayload::TodoList { .. } => {
                self.todo_widget.render(
                    AgentEvent::Structured(payload),
                    container_name,
                    session_id
                )
            }
            StructuredPayload::GlobResults { paths, total } => {
                let mut entries = Vec::new();

                entries.push(helpers::create_log_entry(
                    LogEntryLevel::Info,
                    container_name,
                    format!("📂 Found {} files", total),
                    session_id,
                    "glob",
                ));

                for (i, path) in paths.iter().take(10).enumerate() {
                    entries.push(helpers::create_log_entry(
                        LogEntryLevel::Debug,
                        container_name,
                        format!("   {}", path),
                        session_id,
                        "glob",
                    ));

                    if i == 9 && total > 10 {
                        entries.push(helpers::create_log_entry(
                            LogEntryLevel::Debug,
                            container_name,
                            format!("   ... and {} more", total - 10),
                            session_id,
                            "glob",
                        ));
                    }
                }

                WidgetOutput::MultiLine(entries)
            }
            StructuredPayload::PrettyJson(json) => {
                let entry = helpers::create_log_entry(
                    LogEntryLevel::Info,
                    container_name,
                    json,
                    session_id,
                    "json",
                );

                WidgetOutput::Simple(entry)
            }
        }
    }
}

impl Default for MessageRouter {
    fn default() -> Self {
        Self::new()
    }
}