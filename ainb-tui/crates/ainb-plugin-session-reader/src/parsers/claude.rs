//! Claude Code JSONL parser.
//!
//! Lifted from `crates/ainb-core/src/models/usage.rs::parse_claude_*`.
//! Differences from the in-tree code:
//!
//! * `std::fs` calls become host-fn wrappers (`host::fs_read_dir`,
//!   `host::fs_read_file`).
//! * `provider_call_id` switches from blake3-truncated to inline
//!   FNV-1a (see `reference_inline_fnv_drop_blake3`).
//! * No append cache: the plugin always full-parses each file. The
//!   in-tree append optimisation lives in `crates/ainb-core/usage_cache`
//!   which depends on rusqlite + filesystem locking — both impractical
//!   inside the wasm sandbox. Caching happens one level up via
//!   `cache_get`/`cache_put` on the whole `UsageData` snapshot.

use ainb_plugin_api::host::LogLevel;
use ainb_plugin_types_sessions::{Provider, ProviderCall};
use serde::Deserialize;
use serde_json::Value;

use crate::fnv::provider_call_id;
use crate::host;

use super::{estimate_cost_usd, parse_timestamp};

#[derive(Deserialize)]
struct ClaudeLine {
    #[serde(rename = "type")]
    msg_type: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    cwd: Option<String>,
    #[serde(rename = "gitBranch")]
    git_branch: Option<String>,
    message: Option<ClaudeMessage>,
}

#[derive(Deserialize)]
struct ClaudeMessage {
    content: Option<Value>,
    model: Option<String>,
    usage: Option<ClaudeUsage>,
}

#[derive(Deserialize)]
struct ClaudeUsage {
    input_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

/// Walk `~/.claude/projects/<project>/<session>.jsonl`. Two-level
/// recursion: outer dir lists per-project subdirs; inner dir lists
/// per-session JSONL files.
pub fn parse_dir(projects_root: &str) -> Vec<ProviderCall> {
    let mut calls = Vec::new();
    let project_dirs = match host::fs_read_dir(projects_root) {
        Ok(d) => d,
        Err(host::HostError::NotPermitted) => {
            host::log(
                LogLevel::Warn,
                "session-reader/claude: not permitted to read projects dir",
            );
            return calls;
        }
        Err(_) => return calls,
    };

    for project_path in project_dirs {
        let project = path_basename(&project_path);
        let session_files = match host::fs_read_dir(&project_path) {
            Ok(f) => f,
            Err(_) => continue,
        };
        for path in session_files {
            if !path.ends_with(".jsonl") {
                continue;
            }
            let bytes = match host::fs_read_file(&path) {
                Ok(b) => b,
                Err(_) => {
                    host::log(
                        LogLevel::Warn,
                        &format!("session-reader/claude: skip unreadable {path}"),
                    );
                    continue;
                }
            };
            let Ok(content) = String::from_utf8(bytes) else {
                host::log(
                    LogLevel::Warn,
                    &format!("session-reader/claude: skip non-utf8 {path}"),
                );
                continue;
            };
            calls.extend(parse_source(&path, &project, &project_path, &content));
        }
    }
    calls
}

/// Parse one JSONL file's worth of bytes into [`ProviderCall`]s.
/// Public so unit tests can drive synthetic fixtures without touching
/// host fns.
pub fn parse_source(
    path: &str,
    project: &str,
    project_path: &str,
    content: &str,
) -> Vec<ProviderCall> {
    let mut calls = Vec::new();
    let mut current_user_message = String::new();
    let mut offset: u64 = 0;
    for chunk in content.split_inclusive('\n') {
        let line_offset = offset;
        offset += chunk.len() as u64;
        let line = chunk.strip_suffix('\n').unwrap_or(chunk);
        if let Some(call) = parse_line(line, path, project, project_path, &mut current_user_message, line_offset) {
            calls.push(call);
        }
    }
    calls
}

fn parse_line(
    line: &str,
    path: &str,
    project: &str,
    project_path: &str,
    current_user_message: &mut String,
    line_offset: u64,
) -> Option<ProviderCall> {
    let parsed: ClaudeLine = serde_json::from_str(line).ok()?;
    match parsed.msg_type.as_deref() {
        Some("user") => {
            if let Some(message) = parsed.message {
                *current_user_message = extract_user_text(message.content.as_ref());
            }
            None
        }
        Some("assistant") => {
            let message = parsed.message?;
            let usage = message.usage?;
            let timestamp = parsed.timestamp.as_deref().and_then(parse_timestamp)?;

            let model = message.model.unwrap_or_else(|| "claude-unknown".to_string());
            let tools = extract_tools(message.content.as_ref());
            let bash_commands = extract_bash_commands(message.content.as_ref());
            let input_tokens = usage.input_tokens.unwrap_or(0);
            let output_tokens = usage.output_tokens.unwrap_or(0);
            let cache_creation_tokens = usage.cache_creation_input_tokens.unwrap_or(0);
            let cache_read_tokens = usage.cache_read_input_tokens.unwrap_or(0);
            let cost_usd = estimate_cost_usd(
                &model,
                input_tokens,
                output_tokens,
                cache_creation_tokens,
                cache_read_tokens,
                0,
            );

            Some(ProviderCall {
                id: provider_call_id(path, line_offset),
                provider: Provider::Claude,
                model,
                session_id: parsed
                    .session_id
                    .unwrap_or_else(|| path_filestem(path).unwrap_or_else(|| "unknown".into())),
                project: project.to_string(),
                project_path: parsed.cwd.unwrap_or_else(|| project_path.to_string()),
                timestamp,
                input_tokens,
                cache_creation_tokens,
                cache_read_tokens,
                output_tokens,
                reasoning_tokens: 0,
                cost_usd,
                tools,
                bash_commands,
                user_message: current_user_message.clone(),
                branch: parsed.git_branch.filter(|b| !b.is_empty()),
            })
        }
        _ => None,
    }
}

fn extract_user_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .filter(|item| item.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

fn extract_tools(content: Option<&Value>) -> Vec<String> {
    let Some(Value::Array(items)) = content else {
        return Vec::new();
    };
    items
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("tool_use"))
        .filter_map(|item| item.get("name").and_then(Value::as_str))
        .map(ToString::to_string)
        .collect()
}

fn extract_bash_commands(content: Option<&Value>) -> Vec<String> {
    let Some(Value::Array(items)) = content else {
        return Vec::new();
    };
    items
        .iter()
        .filter(|item| {
            item.get("type").and_then(Value::as_str) == Some("tool_use")
                && matches!(
                    item.get("name").and_then(Value::as_str),
                    Some("Bash" | "Shell")
                )
        })
        .filter_map(|item| {
            item.get("input")
                .and_then(|input| input.get("command"))
                .and_then(Value::as_str)
        })
        .map(ToString::to_string)
        .collect()
}

fn path_basename(path: &str) -> String {
    path.rsplit('/')
        .find(|s| !s.is_empty())
        .unwrap_or(path)
        .to_string()
}

fn path_filestem(path: &str) -> Option<String> {
    let name = path.rsplit('/').find(|s| !s.is_empty())?;
    Some(name.split('.').next().unwrap_or(name).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assistant_line() -> &'static str {
        r#"{"type":"assistant","timestamp":"2026-05-09T10:00:00Z","sessionId":"sess-1","cwd":"/tmp/proj","gitBranch":"main","message":{"model":"claude-3-5-sonnet","content":[{"type":"text","text":"hi"},{"type":"tool_use","name":"Read"}],"usage":{"input_tokens":100,"output_tokens":200,"cache_read_input_tokens":50}}}"#
    }

    fn user_line() -> &'static str {
        r#"{"type":"user","timestamp":"2026-05-09T09:59:00Z","message":{"content":[{"type":"text","text":"do a thing"}]}}"#
    }

    #[test]
    fn parses_assistant_turn_into_provider_call() {
        let content = format!("{}\n{}\n", user_line(), assistant_line());
        let calls = parse_source("/tmp/sess-1.jsonl", "proj", "/tmp/proj", &content);
        assert_eq!(calls.len(), 1);
        let call = &calls[0];
        assert_eq!(call.provider, Provider::Claude);
        assert_eq!(call.model, "claude-3-5-sonnet");
        assert_eq!(call.input_tokens, 100);
        assert_eq!(call.output_tokens, 200);
        assert_eq!(call.cache_read_tokens, 50);
        assert_eq!(call.user_message, "do a thing");
        assert_eq!(call.tools, vec!["Read".to_string()]);
        assert_eq!(call.branch.as_deref(), Some("main"));
    }

    #[test]
    fn skips_invalid_jsonl_lines() {
        let content = format!("not json\n{}\n", assistant_line());
        let calls = parse_source("/tmp/sess.jsonl", "proj", "/tmp/proj", &content);
        assert_eq!(calls.len(), 1);
    }

    #[test]
    fn ignores_assistant_turns_without_usage() {
        let line = r#"{"type":"assistant","timestamp":"2026-05-09T10:00:00Z","message":{"model":"claude-3-5-sonnet"}}"#;
        let calls = parse_source("/tmp/x.jsonl", "p", "/tmp/p", &format!("{line}\n"));
        assert_eq!(calls.len(), 0);
    }

    #[test]
    fn empty_branch_field_drops_to_none() {
        let line = r#"{"type":"assistant","timestamp":"2026-05-09T10:00:00Z","gitBranch":"","message":{"model":"claude-3-5-sonnet","usage":{"input_tokens":1,"output_tokens":1}}}"#;
        let calls = parse_source("/tmp/x.jsonl", "p", "/tmp/p", &format!("{line}\n"));
        assert_eq!(calls[0].branch, None);
    }

    #[test]
    fn provider_call_ids_are_distinct_per_line() {
        let content = format!("{a}\n{a}\n", a = assistant_line());
        let calls = parse_source("/tmp/x.jsonl", "p", "/tmp/p", &content);
        assert_eq!(calls.len(), 2);
        assert_ne!(calls[0].id, calls[1].id);
    }
}
