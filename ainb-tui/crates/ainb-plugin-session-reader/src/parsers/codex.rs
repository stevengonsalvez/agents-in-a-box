//! Codex session parser.
//!
//! Phase 7c port. Directory layout:
//! `<sessions_root>/<YYYY>/<MM>/<DD>/rollout-*.jsonl`.

use std::path::Path;

use ainb_plugin_types_sessions::{Provider, ProviderCall};
use serde::Deserialize;

use crate::fnv::provider_call_id;

use super::{estimate_cost_usd, parse_timestamp};

#[derive(Deserialize)]
struct CodexEntry {
    #[serde(rename = "type")]
    entry_type: String,
    timestamp: Option<String>,
    payload: Option<CodexPayload>,
}

#[derive(Deserialize)]
struct CodexPayload {
    #[serde(rename = "type")]
    payload_type: Option<String>,
    role: Option<String>,
    cwd: Option<String>,
    originator: Option<String>,
    session_id: Option<String>,
    model: Option<String>,
    name: Option<String>,
    content: Option<Vec<CodexContent>>,
    info: Option<CodexInfo>,
}

#[derive(Deserialize)]
struct CodexContent {
    #[serde(rename = "type")]
    content_type: Option<String>,
    text: Option<String>,
}

#[derive(Deserialize)]
struct CodexInfo {
    model: Option<String>,
    model_name: Option<String>,
    last_token_usage: Option<CodexTokenUsage>,
    total_token_usage: Option<CodexTokenUsage>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
struct CodexTokenUsage {
    input_tokens: Option<u64>,
    cached_input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    reasoning_output_tokens: Option<u64>,
    total_tokens: Option<u64>,
}

/// Walk `<sessions_root>/<YYYY>/<MM>/<DD>/rollout-*.jsonl`.
pub fn parse_dir(sessions_root: &Path) -> Vec<ProviderCall> {
    let mut calls = Vec::new();
    let years = match std::fs::read_dir(sessions_root) {
        Ok(d) => d,
        Err(err) => {
            if err.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    path = %sessions_root.display(),
                    error = %err,
                    "session-reader/codex: read sessions root failed"
                );
            }
            return calls;
        }
    };

    for year_entry in years.flatten() {
        let year_path = year_entry.path();
        if !is_date_component(&year_path, 4) {
            continue;
        }
        let Ok(months) = std::fs::read_dir(&year_path) else {
            continue;
        };
        for month_entry in months.flatten() {
            let month_path = month_entry.path();
            if !is_date_component(&month_path, 2) {
                continue;
            }
            let Ok(days) = std::fs::read_dir(&month_path) else {
                continue;
            };
            for day_entry in days.flatten() {
                let day_path = day_entry.path();
                if !is_date_component(&day_path, 2) {
                    continue;
                }
                let Ok(files) = std::fs::read_dir(&day_path) else {
                    continue;
                };
                for file_entry in files.flatten() {
                    let path = file_entry.path();
                    let basename = path
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or_default();
                    if !basename.starts_with("rollout-") || !basename.ends_with(".jsonl") {
                        continue;
                    }
                    let content = match std::fs::read_to_string(&path) {
                        Ok(c) => c,
                        Err(err) => {
                            tracing::warn!(
                                path = %path.display(),
                                error = %err,
                                "session-reader/codex: skip unreadable file"
                            );
                            continue;
                        }
                    };
                    if !is_valid_codex_session(&content) {
                        continue;
                    }
                    let path_str = path.to_string_lossy().into_owned();
                    calls.extend(parse_source(&path_str, &content));
                }
            }
        }
    }
    calls
}

/// Parse one rollout file into [`ProviderCall`]s. Public for tests.
#[allow(clippy::too_many_lines)]
pub fn parse_source(path: &str, content: &str) -> Vec<ProviderCall> {
    let mut calls = Vec::new();
    let mut session_id = path_filestem(path).unwrap_or_else(|| "unknown".to_string());
    let mut session_model: Option<String> = None;
    let mut cwd = "unknown".to_string();
    let mut project = "unknown".to_string();
    let mut previous_cumulative_total = 0_u64;
    let mut previous_input = 0_u64;
    let mut previous_cached = 0_u64;
    let mut previous_output = 0_u64;
    let mut previous_reasoning = 0_u64;
    let mut pending_tools: Vec<String> = Vec::new();
    let mut pending_user_message = String::new();

    let mut offset: u64 = 0;
    for chunk in content.split_inclusive('\n') {
        let line_offset = offset;
        offset += chunk.len() as u64;
        let line = chunk.strip_suffix('\n').unwrap_or(chunk);
        let entry: CodexEntry = match serde_json::from_str(line) {
            Ok(entry) => entry,
            Err(_) => continue,
        };

        let payload = entry.payload.as_ref();

        if entry.entry_type == "session_meta" {
            if let Some(payload) = payload {
                if let Some(id) = &payload.session_id {
                    session_id = id.clone();
                }
                if let Some(model) = &payload.model {
                    session_model = Some(model.clone());
                }
                if let Some(session_cwd) = &payload.cwd {
                    cwd = session_cwd.clone();
                    project = sanitize_project(session_cwd);
                }
            }
            continue;
        }

        if entry.entry_type == "turn_context" {
            if let Some(model) = payload.and_then(|p| p.model.as_ref()) {
                session_model = Some(model.clone());
            }
            continue;
        }

        if entry.entry_type == "response_item"
            && payload.and_then(|p| p.payload_type.as_deref()) == Some("function_call")
        {
            if let Some(raw_name) = payload.and_then(|p| p.name.as_deref()) {
                pending_tools.push(normalize_tool(raw_name).to_string());
            }
            continue;
        }

        if entry.entry_type == "event_msg"
            && payload.and_then(|p| p.payload_type.as_deref()) == Some("patch_apply_end")
        {
            pending_tools.push("Edit".to_string());
            continue;
        }

        if entry.entry_type == "response_item"
            && payload.and_then(|p| p.payload_type.as_deref()) == Some("message")
            && payload.and_then(|p| p.role.as_deref()) == Some("user")
        {
            let texts = payload
                .and_then(|p| p.content.as_ref())
                .map(|content| {
                    content
                        .iter()
                        .filter(|item| item.content_type.as_deref() == Some("input_text"))
                        .filter_map(|item| item.text.as_deref())
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            if !texts.is_empty() {
                pending_user_message = texts;
            }
            continue;
        }

        if entry.entry_type != "event_msg"
            || payload.and_then(|p| p.payload_type.as_deref()) != Some("token_count")
        {
            continue;
        }

        let Some(info) = payload.and_then(|p| p.info.as_ref()) else {
            continue;
        };

        let cumulative_total = info
            .total_token_usage
            .and_then(|usage| usage.total_tokens)
            .unwrap_or(0);
        if cumulative_total > 0 && cumulative_total == previous_cumulative_total {
            continue;
        }
        previous_cumulative_total = cumulative_total;

        let (input_tokens, cached_tokens, output_tokens, reasoning_tokens) =
            if let Some(last) = info.last_token_usage {
                let input = last.input_tokens.unwrap_or(0);
                let cached = last.cached_input_tokens.unwrap_or(0);
                let output = last.output_tokens.unwrap_or(0);
                let reasoning = last.reasoning_output_tokens.unwrap_or(0);

                if let Some(total) = info.total_token_usage {
                    previous_input = total.input_tokens.unwrap_or(previous_input + input);
                    previous_cached =
                        total.cached_input_tokens.unwrap_or(previous_cached + cached);
                    previous_output = total.output_tokens.unwrap_or(previous_output + output);
                    previous_reasoning = total
                        .reasoning_output_tokens
                        .unwrap_or(previous_reasoning + reasoning);
                } else {
                    previous_input += input;
                    previous_cached += cached;
                    previous_output += output;
                    previous_reasoning += reasoning;
                }

                (input, cached, output, reasoning)
            } else if let Some(total) = info.total_token_usage {
                let input = total.input_tokens.unwrap_or(0).saturating_sub(previous_input);
                let cached = total
                    .cached_input_tokens
                    .unwrap_or(0)
                    .saturating_sub(previous_cached);
                let output = total.output_tokens.unwrap_or(0).saturating_sub(previous_output);
                let reasoning = total
                    .reasoning_output_tokens
                    .unwrap_or(0)
                    .saturating_sub(previous_reasoning);

                previous_input = total.input_tokens.unwrap_or(0);
                previous_cached = total.cached_input_tokens.unwrap_or(0);
                previous_output = total.output_tokens.unwrap_or(0);
                previous_reasoning = total.reasoning_output_tokens.unwrap_or(0);

                (input, cached, output, reasoning)
            } else {
                continue;
            };

        if input_tokens + cached_tokens + output_tokens + reasoning_tokens == 0 {
            continue;
        }

        let Some(timestamp) = entry.timestamp.as_deref().and_then(parse_timestamp) else {
            continue;
        };

        let uncached_input_tokens = input_tokens.saturating_sub(cached_tokens);
        let model = resolve_model(payload, session_model.as_deref());
        let cost_usd = estimate_cost_usd(
            &model,
            uncached_input_tokens,
            output_tokens + reasoning_tokens,
            0,
            cached_tokens,
            0,
        );

        calls.push(ProviderCall {
            id: provider_call_id(path, line_offset),
            provider: Provider::Codex,
            model,
            session_id: session_id.clone(),
            project: project.clone(),
            project_path: cwd.clone(),
            timestamp,
            input_tokens: uncached_input_tokens,
            cache_creation_tokens: 0,
            cache_read_tokens: cached_tokens,
            output_tokens,
            reasoning_tokens,
            cost_usd,
            tools: std::mem::take(&mut pending_tools),
            bash_commands: Vec::new(),
            user_message: std::mem::take(&mut pending_user_message),
            branch: None,
        });
    }
    calls
}

fn is_valid_codex_session(content: &str) -> bool {
    let Some(first_line) = content.lines().find(|line| !line.trim().is_empty()) else {
        return false;
    };
    let Ok(entry) = serde_json::from_str::<CodexEntry>(first_line) else {
        return false;
    };
    entry.entry_type == "session_meta"
        && entry
            .payload
            .and_then(|payload| payload.originator)
            .is_some_and(|originator| originator.to_lowercase().starts_with("codex"))
}

fn is_date_component(path: &Path, len: usize) -> bool {
    let Some(basename) = path.file_name().and_then(|s| s.to_str()) else {
        return false;
    };
    basename.len() == len && basename.chars().all(|ch| ch.is_ascii_digit())
}

fn sanitize_project(cwd: &str) -> String {
    cwd.trim_start_matches('/').replace('/', "-")
}

fn normalize_tool(raw: &str) -> &str {
    match raw {
        "exec_command" => "Bash",
        "read_file" => "Read",
        "write_file" | "apply_diff" | "apply_patch" => "Edit",
        "spawn_agent" | "close_agent" | "wait_agent" => "Agent",
        "read_dir" => "Glob",
        _ => raw,
    }
}

fn resolve_model(payload: Option<&CodexPayload>, session_model: Option<&str>) -> String {
    payload
        .and_then(|payload| payload.model.as_deref())
        .or_else(|| {
            payload
                .and_then(|payload| payload.info.as_ref())
                .and_then(|info| info.model.as_deref())
        })
        .or_else(|| {
            payload
                .and_then(|payload| payload.info.as_ref())
                .and_then(|info| info.model_name.as_deref())
        })
        .or(session_model)
        .unwrap_or("gpt-5")
        .to_string()
}

fn path_filestem(path: &str) -> Option<String> {
    let name = Path::new(path).file_name()?.to_str()?;
    Some(name.split('.').next().unwrap_or(name).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> String {
        let lines = [
            r#"{"type":"session_meta","timestamp":"2026-05-09T10:00:00Z","payload":{"session_id":"sess-1","originator":"codex","model":"gpt-5","cwd":"/tmp/codex"}}"#,
            r#"{"type":"event_msg","timestamp":"2026-05-09T10:00:01Z","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":1000,"cached_input_tokens":200,"output_tokens":500,"reasoning_output_tokens":0},"total_token_usage":{"total_tokens":1500}}}}"#,
        ];
        lines.join("\n") + "\n"
    }

    #[test]
    fn parses_session_meta_then_token_count() {
        let calls = parse_source("/tmp/rollout-1.jsonl", &fixture());
        assert_eq!(calls.len(), 1);
        let c = &calls[0];
        assert_eq!(c.provider, Provider::Codex);
        assert_eq!(c.model, "gpt-5");
        assert_eq!(c.session_id, "sess-1");
        assert_eq!(c.project, "tmp-codex");
        assert_eq!(c.input_tokens, 800);
        assert_eq!(c.cache_read_tokens, 200);
        assert_eq!(c.output_tokens, 500);
    }

    #[test]
    fn rejects_non_codex_originator() {
        let line = r#"{"type":"session_meta","payload":{"originator":"someone-else"}}"#;
        assert!(!is_valid_codex_session(&format!("{line}\n")));
    }

    #[test]
    fn skips_unparseable_lines() {
        let content = format!("nope\n{}", fixture());
        let calls = parse_source("/tmp/rollout-1.jsonl", &content);
        assert_eq!(calls.len(), 1);
    }

    #[test]
    fn missing_root_returns_empty_without_panic() {
        let calls = parse_dir(Path::new("/this/does/not/exist"));
        assert!(calls.is_empty());
    }
}
