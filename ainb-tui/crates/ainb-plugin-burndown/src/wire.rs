//! Wire-to-local schema conversion for `sessions.usage_data` snapshots.
//!
//! `ainb-plugin-types-sessions` carries the deterministic on-the-wire
//! shape that session-reader publishes. Burndown's CLI + render
//! pipeline runs against the legacy [`crate::data::usage::UsageData`]
//! shape (richer activity taxonomy, `HashMap` for fast project lookup).
//! This module owns the one-direction translation.
//!
//! Field-for-field identical except:
//! * `Provider` is an enum on the wire, a `String` locally — emit
//!   `provider.as_str().to_string()`.
//! * `model_project_counts` is `Vec<(String, …)>` (deterministic
//!   ordering for byte-stable msgpack) on the wire, `HashMap<String, …>`
//!   locally for cheap render-time lookup.
//! * `ActivityCategory` taxonomy diverges (6 wire buckets vs. 12+
//!   legacy variants) — best-effort map with `Conversation` as the
//!   `Other` fallback. Lossy but the JSON output stays well-formed.

use crate::data::usage::{self as L, UsageData};
use ainb_plugin_types_sessions as W;

fn bucket(b: W::TokenBucket) -> L::TokenBucket {
    L::TokenBucket {
        input_tokens: b.input_tokens,
        cache_creation_tokens: b.cache_creation_tokens,
        cache_read_tokens: b.cache_read_tokens,
        output_tokens: b.output_tokens,
        reasoning_tokens: b.reasoning_tokens,
        session_count: b.session_count,
        project_count: b.project_count,
        call_count: b.call_count,
        cost_usd: b.cost_usd,
    }
}

fn category(c: W::ActivityCategory) -> L::ActivityCategory {
    match c {
        W::ActivityCategory::Code => L::ActivityCategory::Coding,
        W::ActivityCategory::Docs => L::ActivityCategory::Conversation,
        W::ActivityCategory::Debug => L::ActivityCategory::Debugging,
        W::ActivityCategory::Refactor => L::ActivityCategory::Refactoring,
        W::ActivityCategory::Test => L::ActivityCategory::Testing,
        W::ActivityCategory::Other => L::ActivityCategory::Conversation,
    }
}

/// Convert a wire `UsageData` (from `sessions.usage_data`) into
/// burndown's local shape.
///
/// After the field-for-field copy, `rebuild_activity_and_mcp_columns`
/// re-derives the `activities`, `mcp_servers`, and `tools` columns
/// from `calls`: session-reader ships those three intentionally empty
/// (or, for `tools`, with the mcp prefix unsplit) because activity
/// classification + mcp routing live in the consumer's richer
/// taxonomy. See the helper's docstring for the full rationale.
pub fn wire_to_local(w: W::UsageData) -> UsageData {
    let mut local = L::UsageData {
        daily: w.daily.into_iter().map(|(d, b)| (d, bucket(b))).collect(),
        weekly: w.weekly.into_iter().map(|(d, b)| (d, bucket(b))).collect(),
        projects: w
            .projects
            .into_iter()
            .map(|p| L::ProjectUsage {
                name: p.name,
                path: p.path,
                bucket: bucket(p.bucket),
                repo: p.repo,
            })
            .collect(),
        grand_total: bucket(w.grand_total),
        calls: w
            .calls
            .into_iter()
            .map(|c| L::ProviderCall {
                id: c.id,
                provider: c.provider.as_str().to_string(),
                model: c.model,
                session_id: c.session_id,
                project: c.project,
                project_path: c.project_path,
                timestamp: c.timestamp,
                input_tokens: c.input_tokens,
                cache_creation_tokens: c.cache_creation_tokens,
                cache_read_tokens: c.cache_read_tokens,
                output_tokens: c.output_tokens,
                reasoning_tokens: c.reasoning_tokens,
                cost_usd: c.cost_usd,
                tools: c.tools,
                bash_commands: c.bash_commands,
                user_message: c.user_message,
                branch: c.branch,
            })
            .collect(),
        sessions: w
            .sessions
            .into_iter()
            .map(|s| L::SessionUsage {
                provider: s.provider.as_str().to_string(),
                project: s.project,
                project_path: s.project_path,
                session_id: s.session_id,
                first_timestamp: s.first_timestamp,
                last_timestamp: s.last_timestamp,
                bucket: bucket(s.bucket),
            })
            .collect(),
        models: w
            .models
            .into_iter()
            .map(|m| L::ModelUsage {
                model: m.model,
                bucket: bucket(m.bucket),
            })
            .collect(),
        activities: w
            .activities
            .into_iter()
            .map(|a| L::ActivityUsage {
                category: category(a.category),
                bucket: bucket(a.bucket),
                turns: a.turns,
                retries: a.retries,
                edit_turns: a.edit_turns,
                one_shot_turns: a.one_shot_turns,
            })
            .collect(),
        tools: w
            .tools
            .into_iter()
            .map(|n| L::NamedUsage {
                name: n.name,
                calls: n.calls,
            })
            .collect(),
        mcp_servers: w
            .mcp_servers
            .into_iter()
            .map(|n| L::NamedUsage {
                name: n.name,
                calls: n.calls,
            })
            .collect(),
        shell_commands: w
            .shell_commands
            .into_iter()
            .map(|n| L::NamedUsage {
                name: n.name,
                calls: n.calls,
            })
            .collect(),
        branches: w
            .branches
            .into_iter()
            .map(|b| L::BranchUsage {
                branch: b.branch,
                bucket: bucket(b.bucket),
            })
            .collect(),
        model_project_counts: w.model_project_counts.into_iter().collect(),
    };
    L::rebuild_activity_and_mcp_columns(&mut local);
    local
}
