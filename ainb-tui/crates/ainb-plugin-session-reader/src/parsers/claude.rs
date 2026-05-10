//! Claude Code JSONL parser.
//!
//! Phase 7c port of the Phase 6b parser. Filesystem reads use
//! [`std::fs`] directly — the host gates whether the plugin can spawn
//! at all via the `read_claude_logs` capability, so per-call gating is
//! redundant in the subprocess model.
//!
//! Layout: `<projects_root>/<project>/<session>.jsonl`. Two-level walk:
//! outer dir lists per-project subdirs; inner dir lists session files.

use std::path::{Path, PathBuf};

use ainb_plugin_types_sessions::{Provider, ProviderCall};
use serde::Deserialize;
use serde_json::Value;

use crate::fnv::provider_call_id;

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

/// Walk `<projects_root>/<project>/<session>.jsonl`. Missing roots
/// degrade to an empty result; per-file failures are logged via
/// `tracing::warn` and skipped.
pub fn parse_dir(projects_root: &Path) -> Vec<ProviderCall> {
    let mut calls = Vec::new();
    let project_entries = match std::fs::read_dir(projects_root) {
        Ok(entries) => entries,
        Err(err) => {
            if err.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    path = %projects_root.display(),
                    error = %err,
                    "session-reader/claude: read projects root failed"
                );
            }
            return calls;
        }
    };

    for project_entry in project_entries.flatten() {
        let project_path = project_entry.path();
        if !project_path.is_dir() {
            continue;
        }
        let project = path_basename(&project_path);
        let Ok(session_entries) = std::fs::read_dir(&project_path) else {
            continue;
        };
        for session_entry in session_entries.flatten() {
            let path = session_entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                continue;
            }
            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(err) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %err,
                        "session-reader/claude: skip unreadable file"
                    );
                    continue;
                }
            };
            let path_str = path.to_string_lossy().into_owned();
            let project_path_str = project_path.to_string_lossy().into_owned();
            calls.extend(parse_source(&path_str, &project, &project_path_str, &content));
        }
    }
    calls
}

/// Parse one JSONL file's worth of bytes into [`ProviderCall`]s.
/// Public so unit tests can drive synthetic fixtures without touching
/// the filesystem.
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
        if let Some(call) = parse_line(
            line,
            path,
            project,
            project_path,
            &mut current_user_message,
            line_offset,
        ) {
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

fn path_basename(path: &Path) -> String {
    path.file_name()
        .and_then(|s| s.to_str())
        .map_or_else(|| path.to_string_lossy().into_owned(), ToString::to_string)
}

fn path_filestem(path: &str) -> Option<String> {
    let name = Path::new(path).file_name()?.to_str()?;
    Some(name.split('.').next().unwrap_or(name).to_string())
}

// Silences the unused-import lint if no tests reference PathBuf.
#[allow(dead_code)]
fn _unused() -> Option<PathBuf> {
    None
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

    #[test]
    fn missing_root_returns_empty_without_panic() {
        let calls = parse_dir(Path::new("/this/does/not/exist"));
        assert!(calls.is_empty());
    }

    #[test]
    fn parse_dir_walks_two_levels_and_collects_calls() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let project_dir = tmp.path().join("my-project");
        std::fs::create_dir_all(&project_dir).unwrap();
        let session_path = project_dir.join("abc.jsonl");
        std::fs::write(
            &session_path,
            format!("{}\n{}\n", user_line(), assistant_line()),
        )
        .unwrap();
        // Non-jsonl file alongside should be skipped.
        std::fs::write(project_dir.join("ignore.txt"), b"junk").unwrap();
        let calls = parse_dir(tmp.path());
        assert_eq!(calls.len(), 1, "expected one call from one assistant line");
        assert_eq!(calls[0].project, "my-project");
    }
}
