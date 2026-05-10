//! Shared test fixtures for the burndown analytics pipeline.
//!
//! Centralised so a layout change to [`ProviderCall`] touches one
//! builder rather than the inline literals scattered across `ui.rs`,
//! `data/usage.rs`, and the integration tests under `tests/`.
//!
//! Compiled only under `#[cfg(test)]` — the helpers stay out of the
//! release binary.

use chrono::{DateTime, TimeZone, Utc};

use crate::data::usage::ProviderCall;

/// Fluent builder for [`ProviderCall`]. Each `with_*` method overrides
/// the corresponding default so a test only mentions the fields it
/// cares about.
#[derive(Debug, Clone)]
pub struct ProviderCallBuilder {
    call: ProviderCall,
}

impl ProviderCallBuilder {
    /// Construct a builder pre-filled with conservative defaults.
    pub fn new() -> Self {
        Self {
            call: ProviderCall {
                id: 0,
                provider: "claude".to_string(),
                model: "claude-sonnet-4-5".to_string(),
                session_id: "s1".to_string(),
                project: "alpha".to_string(),
                project_path: "/work/alpha".to_string(),
                timestamp: default_timestamp(),
                input_tokens: 0,
                cache_creation_tokens: 0,
                cache_read_tokens: 0,
                output_tokens: 0,
                reasoning_tokens: 0,
                cost_usd: None,
                tools: Vec::new(),
                bash_commands: Vec::new(),
                user_message: String::new(),
                branch: None,
            },
        }
    }

    pub fn with_id(mut self, v: u64) -> Self {
        self.call.id = v;
        self
    }
    pub fn with_provider(mut self, v: impl Into<String>) -> Self {
        self.call.provider = v.into();
        self
    }
    pub fn with_model(mut self, v: impl Into<String>) -> Self {
        self.call.model = v.into();
        self
    }
    pub fn with_session(mut self, v: impl Into<String>) -> Self {
        self.call.session_id = v.into();
        self
    }
    pub fn with_project(mut self, v: impl Into<String>) -> Self {
        self.call.project = v.into();
        self
    }
    pub fn with_project_path(mut self, v: impl Into<String>) -> Self {
        self.call.project_path = v.into();
        self
    }
    pub fn with_timestamp(mut self, v: DateTime<Utc>) -> Self {
        self.call.timestamp = v;
        self
    }
    pub fn with_input_tokens(mut self, v: u64) -> Self {
        self.call.input_tokens = v;
        self
    }
    pub fn with_output_tokens(mut self, v: u64) -> Self {
        self.call.output_tokens = v;
        self
    }
    pub fn with_cost(mut self, v: f64) -> Self {
        self.call.cost_usd = Some(v);
        self
    }
    pub fn with_tools(mut self, tools: &[&str]) -> Self {
        self.call.tools = tools.iter().map(|s| (*s).to_string()).collect();
        self
    }
    pub fn with_bash(mut self, cmds: &[&str]) -> Self {
        self.call.bash_commands = cmds.iter().map(|s| (*s).to_string()).collect();
        self
    }
    pub fn with_user_message(mut self, v: impl Into<String>) -> Self {
        self.call.user_message = v.into();
        self
    }
    pub fn with_branch(mut self, v: impl Into<String>) -> Self {
        self.call.branch = Some(v.into());
        self
    }

    pub fn build(self) -> ProviderCall {
        self.call
    }
}

impl Default for ProviderCallBuilder {
    fn default() -> Self {
        Self::new()
    }
}

fn default_timestamp() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 4, 29, 10, 0, 0)
        .single()
        .unwrap_or_else(Utc::now)
}
