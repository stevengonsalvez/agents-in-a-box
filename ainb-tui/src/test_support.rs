// ABOUTME: Shared test fixtures for ProviderCall, UsageData, and the
// Claude JSONL turn-line shape. Centralised so a layout change to
// ProviderCall touches one builder rather than four near-identical
// inline literals across unit + integration tests.

//! Shared test fixtures for the usage analytics pipeline.
//!
//! Gated by `cfg(any(test, feature = "test-support"))` so the helpers
//! are available to in-tree unit tests automatically and to integration
//! tests in `tests/` via `--features test-support`.

use chrono::{DateTime, TimeZone, Utc};

use crate::models::{
    ActivityCategory, ActivityUsage, ModelUsage, NamedUsage, ProjectUsage, ProviderCall,
    SessionUsage, TokenBucket, UsageData,
};

/// Fluent builder for `ProviderCall`. Each `with_*` method overrides the
/// corresponding default so a test only mentions the fields it actually
/// cares about. When `ProviderCall` grows a new field, add a default in
/// `new()` and (optionally) a builder method here — every existing test
/// keeps compiling without an edit.
#[derive(Debug, Clone)]
pub struct ProviderCallBuilder {
    call: ProviderCall,
}

impl ProviderCallBuilder {
    /// Construct a builder pre-filled with conservative defaults:
    /// claude-sonnet-4-5 model, session "s1", project "alpha", a fixed
    /// timestamp (2026-04-29T10:00:00 local), and zero tokens.
    pub fn new() -> Self {
        Self {
            call: ProviderCall {
                // Deterministic placeholder id for fixtures. Tests that
                // exercise the analyze_turns precompute (which keys on
                // id) override via `with_id` to differentiate calls.
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

/// Convenience: a fully-defaulted `ProviderCall`. Equivalent to
/// `ProviderCallBuilder::new().build()`.
pub fn provider_call_default() -> ProviderCall {
    ProviderCallBuilder::new().build()
}

/// Deterministic default timestamp shared across fixtures. Pinning this
/// to a specific value (rather than `Utc::now()`) keeps tests
/// reproducible. Stored Utc to match `ProviderCall.timestamp`; render
/// sites convert to local at the boundary.
fn default_timestamp() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 4, 29, 10, 0, 0)
        .single()
        .unwrap_or_else(Utc::now)
}

/// Build a small `UsageData` fixture covering one project, one model,
/// one activity, one session, plus tools/shell — enough surface for the
/// burndown render path and `commit_focused_row` dispatch tests.
///
/// Collapses three near-identical fixtures that previously lived
/// inline in `tests/test_ui_display.rs` and `components/usage::fixture()`.
pub fn sample_usage_data() -> UsageData {
    let bucket = TokenBucket {
        input_tokens: 100,
        output_tokens: 50,
        call_count: 2,
        session_count: 1,
        project_count: 1,
        cost_usd: Some(0.42),
        ..TokenBucket::default()
    };
    let now = default_timestamp();
    UsageData {
        calls: vec![
            ProviderCallBuilder::new()
                .with_project("agents-in-a-box")
                .with_project_path("/tmp/agents-in-a-box")
                .with_input_tokens(100)
                .with_output_tokens(50)
                .with_cost(0.42)
                .with_tools(&["Edit"])
                .with_bash(&["cargo test"])
                .with_user_message("implement burndown")
                .build(),
        ],
        daily: vec![(
            chrono::NaiveDate::from_ymd_opt(2026, 4, 29).unwrap(),
            bucket.clone(),
        )],
        weekly: vec![(
            chrono::NaiveDate::from_ymd_opt(2026, 4, 27).unwrap(),
            bucket.clone(),
        )],
        projects: vec![ProjectUsage {
            name: "agents-in-a-box".to_string(),
            path: "/tmp/agents-in-a-box".to_string(),
            bucket: bucket.clone(),
        }],
        grand_total: bucket.clone(),
        sessions: vec![SessionUsage {
            provider: "claude".to_string(),
            project: "agents-in-a-box".to_string(),
            session_id: "s1".to_string(),
            first_timestamp: now,
            last_timestamp: now,
            bucket: bucket.clone(),
        }],
        models: vec![ModelUsage {
            model: "claude-sonnet-4-5".to_string(),
            bucket: bucket.clone(),
        }],
        activities: vec![ActivityUsage {
            category: ActivityCategory::Feature,
            bucket: bucket.clone(),
            turns: 2,
            retries: 0,
            edit_turns: 1,
            one_shot_turns: 1,
        }],
        tools: vec![NamedUsage {
            name: "Edit".to_string(),
            calls: 1,
        }],
        shell_commands: vec![NamedUsage {
            name: "cargo test".to_string(),
            calls: 1,
        }],
        ..UsageData::default()
    }
}

/// Build a single Claude JSONL "assistant" turn line. Used by branch
/// attribution and parser tests that need realistic on-disk shape but
/// only care about a handful of fields.
///
/// `branch` is emitted as `gitBranch` when `Some`; omitted when `None`
/// so the parser exercises the "branchless turn" path.
pub fn claude_jsonl_turn(
    branch: Option<&str>,
    model: &str,
    in_tok: u64,
    out_tok: u64,
) -> String {
    let branch_field = match branch {
        Some(b) => format!(r#","gitBranch":"{b}""#),
        None => String::new(),
    };
    format!(
        r#"{{"type":"assistant","timestamp":"2026-04-10T09:00:00Z","sessionId":"s1"{branch_field},"message":{{"role":"assistant","model":"{model}","content":[{{"type":"text","text":"x"}}],"usage":{{"input_tokens":{in_tok},"output_tokens":{out_tok}}}}}}}"#
    )
}
