//! Google Antigravity session parser.
//!
//! Layout: `<brain_root>/<session_id>/.system_generated/logs/transcript.jsonl`
//! (with fallback to `transcript_full.jsonl`).
//!
//! Message types:
//! - `USER_EXPLICIT` / `USER_INPUT`: user turn content.
//! - `PLANNER_RESPONSE` / `MODEL`: model assistant response with thinking,
//!   tool calls, and content.
//! - `GENERIC`: tool execution results returned to model.

use std::path::Path;

use ainb_plugin_types_sessions::{Provider, ProviderCall};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::fnv::provider_call_id;

use super::{estimate_cost_usd, parse_timestamp};

#[derive(Serialize, Deserialize)]
struct AntigravityLine {
    #[serde(default)]
    source: Option<String>,
    #[serde(rename = "type", default)]
    entry_type: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    thinking: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<AntigravityToolCall>>,
}

#[derive(Serialize, Deserialize)]
struct AntigravityToolCall {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    args: Option<Value>,
}

/// Walk `<brain_root>/<session_id>/.system_generated/logs/transcript.jsonl`.
/// Missing roots degrade to an empty result; per-file failures are logged and skipped.
pub fn parse_dir(brain_root: &Path) -> Vec<ProviderCall> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut cache = None;
        let mut ctx = crate::scanner::ScanCtx::full(&mut cache);
        parse_dir_cached(brain_root, &mut ctx)
    }
    #[cfg(target_arch = "wasm32")]
    {
        parse_dir_uncached(brain_root)
    }
}

/// Cache-aware variant of [`parse_dir`].
#[cfg(not(target_arch = "wasm32"))]
pub fn parse_dir_cached(
    brain_root: &Path,
    ctx: &mut crate::scanner::ScanCtx<'_>,
) -> Vec<ProviderCall> {
    let mut reporter = crate::scanner::ProgressReporter::noop();
    parse_dir_cached_with_progress(brain_root, ctx, &mut reporter)
}

/// Cache + progress-aware walk for Antigravity brain sessions.
#[cfg(not(target_arch = "wasm32"))]
pub fn parse_dir_cached_with_progress(
    brain_root: &Path,
    ctx: &mut crate::scanner::ScanCtx<'_>,
    reporter: &mut crate::scanner::ProgressReporter,
) -> Vec<ProviderCall> {
    let mut calls = Vec::new();
    let session_dirs = match std::fs::read_dir(brain_root) {
        Ok(entries) => entries,
        Err(err) => {
            if err.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    path = %brain_root.display(),
                    error = %err,
                    "session-reader/antigravity: read brain root failed"
                );
            }
            return calls;
        }
    };

    for session_entry in session_dirs.flatten() {
        let session_dir = session_entry.path();
        if !session_dir.is_dir() {
            continue;
        }
        let logs_dir = session_dir.join(".system_generated/logs");
        let transcript_path = if logs_dir.join("transcript.jsonl").is_file() {
            logs_dir.join("transcript.jsonl")
        } else if logs_dir.join("transcript_full.jsonl").is_file() {
            logs_dir.join("transcript_full.jsonl")
        } else {
            continue;
        };

        let session_id = path_basename(&session_dir);
        let path_str = transcript_path.to_string_lossy().into_owned();
        reporter.note_file(&session_id);
        let file_calls = super::read_file_cached(&transcript_path, ctx, |content| {
            parse_source(&path_str, &session_id, content)
        });
        calls.extend(file_calls);
    }
    calls
}

/// Uncached walk used by the wasm32 build.
#[cfg(target_arch = "wasm32")]
fn parse_dir_uncached(brain_root: &Path) -> Vec<ProviderCall> {
    let mut calls = Vec::new();
    let session_dirs = match std::fs::read_dir(brain_root) {
        Ok(entries) => entries,
        Err(_) => return calls,
    };
    for session_entry in session_dirs.flatten() {
        let session_dir = session_entry.path();
        if !session_dir.is_dir() {
            continue;
        }
        let logs_dir = session_dir.join(".system_generated/logs");
        let transcript_path = if logs_dir.join("transcript.jsonl").is_file() {
            logs_dir.join("transcript.jsonl")
        } else if logs_dir.join("transcript_full.jsonl").is_file() {
            logs_dir.join("transcript_full.jsonl")
        } else {
            continue;
        };
        let Ok(content) = std::fs::read_to_string(&transcript_path) else {
            continue;
        };
        let session_id = path_basename(&session_dir);
        let path_str = transcript_path.to_string_lossy().into_owned();
        calls.extend(parse_source(&path_str, &session_id, &content));
    }
    calls
}

/// Parse one Antigravity transcript JSONL into [`ProviderCall`]s.
pub fn parse_source(path: &str, session_id: &str, content: &str) -> Vec<ProviderCall> {
    let mut current_user_message = String::new();
    let mut current_model = "gemini-3.7-flash".to_string();
    let mut current_project_path: Option<String> = None;
    let mut tool_outputs_len: usize = 0;
    let mut calls = Vec::new();

    let mut offset: u64 = 0;
    for chunk in content.split_inclusive('\n') {
        let line_offset = offset;
        offset += chunk.len() as u64;
        let line = chunk.strip_suffix('\n').unwrap_or(chunk);
        let entry: AntigravityLine = match serde_json::from_str(line) {
            Ok(e) => e,
            Err(_) => continue,
        };

        let source = entry.source.as_deref().unwrap_or("");
        let entry_type = entry.entry_type.as_deref().unwrap_or("");

        if source == "USER_EXPLICIT" || entry_type == "USER_INPUT" || source == "USER" {
            if let Some(text) = entry.content.as_deref() {
                if let Some(model) = detect_model(text) {
                    current_model = model;
                }
                current_user_message = extract_user_request(text);
                tool_outputs_len = 0;
            }
            continue;
        }

        if source == "MODEL" && entry_type == "GENERIC" {
            if let Some(c) = entry.content.as_deref() {
                tool_outputs_len = tool_outputs_len.saturating_add(c.len());
            }
            continue;
        }

        let is_model_response = source == "MODEL"
            && (entry_type == "PLANNER_RESPONSE"
                || entry.tool_calls.is_some()
                || entry.thinking.is_some());

        if !is_model_response {
            continue;
        }

        let Some(timestamp) = entry.created_at.as_deref().and_then(parse_timestamp) else {
            continue;
        };

        let mut tools = Vec::new();
        let mut bash_commands = Vec::new();

        if let Some(tool_calls) = &entry.tool_calls {
            for tc in tool_calls {
                let raw_name = tc.name.as_deref().unwrap_or("");
                if raw_name.is_empty() {
                    continue;
                }
                let norm = normalize_tool(raw_name);
                tools.push(norm.to_string());

                if norm == "Bash" || raw_name == "run_command" {
                    if let Some(args) = &tc.args {
                        if let Some(cmd) = extract_command_arg(args) {
                            bash_commands.push(cmd);
                        }
                    }
                }

                if let Some(args) = &tc.args {
                    if let Some(cwd) = extract_cwd_arg(args) {
                        current_project_path = Some(cwd);
                    }
                }
            }
        }

        let project_path = current_project_path
            .clone()
            .unwrap_or_else(|| format!("brain-{session_id}"));
        let project = sanitize_project(&project_path);

        let reasoning_tokens = entry
            .thinking
            .as_deref()
            .map_or(0, |t| (t.len() / 4) as u64);

        let content_len = entry.content.as_deref().map_or(0, |c| c.len());
        let tool_calls_json_len = entry
            .tool_calls
            .as_ref()
            .map_or(0, |tc| serde_json::to_string(tc).unwrap_or_default().len());

        let input_tokens =
            (((current_user_message.len() + tool_outputs_len) / 4).max(100)) as u64;
        let output_tokens = (((content_len + tool_calls_json_len) / 4).max(50)) as u64;

        let cost_usd = estimate_cost_usd(
            &current_model,
            input_tokens,
            output_tokens,
            0,
            0,
            reasoning_tokens,
        );

        calls.push(ProviderCall {
            id: provider_call_id(path, line_offset),
            provider: Provider::Antigravity,
            model: current_model.clone(),
            session_id: session_id.to_string(),
            project,
            project_path,
            timestamp,
            input_tokens,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            output_tokens,
            reasoning_tokens,
            cost_usd,
            tools,
            bash_commands,
            user_message: current_user_message.clone(),
            branch: None,
        });

        tool_outputs_len = 0;
    }

    calls
}

fn extract_user_request(raw: &str) -> String {
    if let Some(start) = raw.find("<USER_REQUEST>") {
        let after_start = &raw[start + "<USER_REQUEST>".len()..];
        if let Some(end) = after_start.find("</USER_REQUEST>") {
            return after_start[..end].trim().to_string();
        }
    }
    raw.trim().to_string()
}

fn detect_model(raw: &str) -> Option<String> {
    let lower = raw.to_ascii_lowercase();
    if lower.contains("gemini-3.7-flash")
        || lower.contains("gemini 3.7 flash")
        || lower.contains("3.7-flash")
    {
        Some("gemini-3.7-flash".to_string())
    } else if lower.contains("gemini-2.5-pro")
        || lower.contains("gemini 2.5 pro")
        || lower.contains("2.5-pro")
    {
        Some("gemini-2.5-pro".to_string())
    } else if lower.contains("gemini-2.5-flash")
        || lower.contains("gemini 2.5 flash")
        || lower.contains("2.5-flash")
    {
        Some("gemini-2.5-flash".to_string())
    } else {
        None
    }
}

fn normalize_tool(raw: &str) -> &str {
    match raw {
        "run_command" => "Bash",
        "view_file" | "read_url_content" | "read_browser_page" => "Read",
        "write_to_file" | "replace_file_content" | "notebook_edit" => "Edit",
        "list_dir" | "find_by_name" | "grep_search" | "search_web" => "Glob",
        "send_message" => "Agent",
        _ => raw,
    }
}

fn extract_command_arg(args: &Value) -> Option<String> {
    for key in ["CommandLine", "command", "cmd", "input"] {
        if let Some(val) = args.get(key) {
            if let Some(cleaned) = clean_arg_str(val) {
                return Some(cleaned);
            }
        }
    }
    None
}

fn extract_cwd_arg(args: &Value) -> Option<String> {
    for key in ["Cwd", "cwd", "directory", "path"] {
        if let Some(val) = args.get(key) {
            if let Some(cleaned) = clean_arg_str(val) {
                return Some(cleaned);
            }
        }
    }
    None
}

fn clean_arg_str(val: &Value) -> Option<String> {
    let s = match val {
        Value::String(s) => s.as_str(),
        _ => return None,
    };
    let trimmed = s.trim();
    let unquoted = if (trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2)
        || (trimmed.starts_with('\'') && trimmed.ends_with('\'') && trimmed.len() >= 2)
    {
        &trimmed[1..trimmed.len() - 1]
    } else {
        trimmed
    };
    Some(unquoted.trim().to_string())
}

fn path_basename(path: &Path) -> String {
    path.file_name()
        .and_then(|s| s.to_str())
        .map_or_else(|| path.to_string_lossy().into_owned(), ToString::to_string)
}

fn sanitize_project(cwd: &str) -> String {
    cwd.trim_start_matches('/').replace('/', "-")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_transcript() -> &'static str {
        concat!(
            "{\"step_index\":0,\"source\":\"USER_EXPLICIT\",\"type\":\"USER_INPUT\",\"status\":\"DONE\",",
            "\"created_at\":\"2026-08-28T16:12:00Z\",\"content\":\"<USER_REQUEST>\\nFix the build\\n</USER_REQUEST>\"}\n",
            "{\"step_index\":1,\"source\":\"MODEL\",\"type\":\"PLANNER_RESPONSE\",\"status\":\"DONE\",",
            "\"created_at\":\"2026-08-28T16:12:04Z\",\"thinking\":\"Analyzing git repo structure\",",
            "\"tool_calls\":[{\"name\":\"run_command\",\"args\":{\"CommandLine\":\"\\\"git status\\\"\",\"Cwd\":\"\\\"/Users/stevengonsalvez/d/git/ai-coder-rules\\\"\"}}]}\n",
            "{\"step_index\":2,\"source\":\"MODEL\",\"type\":\"GENERIC\",\"status\":\"DONE\",",
            "\"created_at\":\"2026-08-28T16:12:07Z\",\"content\":\"On branch main\\nnothing to commit\"}\n",
            "{\"step_index\":3,\"source\":\"MODEL\",\"type\":\"PLANNER_RESPONSE\",\"status\":\"DONE\",",
            "\"created_at\":\"2026-08-28T16:12:10Z\",\"thinking\":\"Reading file\",",
            "\"tool_calls\":[{\"name\":\"view_file\",\"args\":{\"AbsolutePath\":\"/tmp/test.rs\"}}]}\n"
        )
    }

    #[test]
    fn missing_root_returns_empty_without_panic() {
        let calls = parse_dir(Path::new("/this/path/does/not/exist"));
        assert!(calls.is_empty());
    }

    #[test]
    fn parses_planner_response_and_user_input() {
        let calls = parse_source(
            "/home/.gemini/antigravity-cli/brain/uuid-1/.system_generated/logs/transcript.jsonl",
            "uuid-1",
            sample_transcript(),
        );

        assert_eq!(calls.len(), 2);

        let c0 = &calls[0];
        assert_eq!(c0.provider, Provider::Antigravity);
        assert_eq!(c0.model, "gemini-3.7-flash");
        assert_eq!(c0.session_id, "uuid-1");
        assert_eq!(c0.user_message, "Fix the build");
        assert_eq!(c0.tools, vec!["Bash"]);
        assert_eq!(c0.bash_commands, vec!["git status"]);
        assert_eq!(c0.project_path, "/Users/stevengonsalvez/d/git/ai-coder-rules");
        assert_eq!(c0.project, "Users-stevengonsalvez-d-git-ai-coder-rules");
        assert!(c0.reasoning_tokens > 0);
        assert!(c0.cost_usd.is_some());

        let c1 = &calls[1];
        assert_eq!(c1.tools, vec!["Read"]);
        assert!(c1.bash_commands.is_empty());
    }

    #[test]
    fn normalizes_tools_and_extracts_bash_commands() {
        let content = concat!(
            "{\"source\":\"USER_EXPLICIT\",\"type\":\"USER_INPUT\",\"content\":\"test\"}\n",
            "{\"source\":\"MODEL\",\"type\":\"PLANNER_RESPONSE\",\"created_at\":\"2026-08-28T16:00:00Z\",",
            "\"tool_calls\":[",
            "{\"name\":\"run_command\",\"args\":{\"CommandLine\":\"cargo build\"}},",
            "{\"name\":\"view_file\",\"args\":{}},",
            "{\"name\":\"write_to_file\",\"args\":{}},",
            "{\"name\":\"list_dir\",\"args\":{}},",
            "{\"name\":\"send_message\",\"args\":{}}",
            "]}\n"
        );

        let calls = parse_source("/path/transcript.jsonl", "sess-1", content);
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].tools,
            vec!["Bash", "Read", "Edit", "Glob", "Agent"]
        );
        assert_eq!(calls[0].bash_commands, vec!["cargo build"]);
    }

    #[test]
    fn parse_dir_walks_brain_dirs_and_collects_calls() {
        let temp = tempfile::tempdir().unwrap();
        let session_dir = temp.path().join("03128f4e-b5f1-41f5-bc9e-7fc66b8db270");
        let logs_dir = session_dir.join(".system_generated/logs");
        std::fs::create_dir_all(&logs_dir).unwrap();
        std::fs::write(logs_dir.join("transcript.jsonl"), sample_transcript()).unwrap();

        let calls = parse_dir(temp.path());
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls[0].session_id,
            "03128f4e-b5f1-41f5-bc9e-7fc66b8db270"
        );
    }

    #[test]
    fn malformed_jsonl_stress_test() {
        let malformed = concat!(
            "not a json line at all\n",
            "{\"truncated\": true, \"source\": \n",
            "[1, 2, 3, 4, 5]\n",
            "1234567\n",
            "\"just a string\"\n",
            "null\n",
            "{\"source\": null, \"type\": null, \"created_at\": null}\n",
            "{\"source\": \"MODEL\", \"type\": \"PLANNER_RESPONSE\", \"created_at\": \"invalid-date\"}\n",
            "{\"source\": \"MODEL\", \"type\": \"PLANNER_RESPONSE\", \"created_at\": \"2026-08-28T16:00:00Z\", \"content\": 12345}\n",
            "\n\n   \t\r\n\n",
            "{\"source\":\"USER_EXPLICIT\",\"type\":\"USER_INPUT\",\"content\":\"Valid after junk\"}\n",
            // This line has invalid tool_calls array element (null), so serde skips the line
            "{\"source\":\"MODEL\",\"type\":\"PLANNER_RESPONSE\",\"created_at\":\"2026-08-28T16:00:01Z\",",
            "\"tool_calls\":[null]}\n",
            // This line is valid and should be parsed
            "{\"source\":\"MODEL\",\"type\":\"PLANNER_RESPONSE\",\"created_at\":\"2026-08-28T16:00:02Z\",",
            "\"tool_calls\":[{\"name\":\"\"}, {\"name\":\"run_command\",\"args\":null}, {\"name\":\"run_command\",\"args\":{\"CommandLine\":12345}}, {\"name\":\"run_command\",\"args\":{\"CommandLine\":\"cargo check\"}}]}\n"
        );

        let calls = parse_source("/path/test.jsonl", "stress-sess", malformed);
        // Valid MODEL entry has valid created_at and tool_calls, should parse safely without panicking.
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].session_id, "stress-sess");
        assert_eq!(calls[0].user_message, "Valid after junk");
        assert_eq!(calls[0].tools, vec!["Bash", "Bash", "Bash"]);
        // Non-string CommandLine should not crash and should not be extracted, only string "cargo check"
        assert_eq!(calls[0].bash_commands, vec!["cargo check"]);
    }


    #[test]
    fn empty_logs_and_whitespace_only() {
        assert!(parse_source("/path/test.jsonl", "empty-1", "").is_empty());
        assert!(parse_source("/path/test.jsonl", "empty-2", "\n\n\n").is_empty());
        assert!(parse_source("/path/test.jsonl", "empty-3", "   \t\r\n  \n").is_empty());
    }

    #[test]
    fn model_detection_and_fallback_stress_test() {
        // Test detect_model directly
        assert_eq!(detect_model("Use gemini-3.7-flash please"), Some("gemini-3.7-flash".to_string()));
        assert_eq!(detect_model("Model: Gemini 3.7 Flash"), Some("gemini-3.7-flash".to_string()));
        assert_eq!(detect_model("Switch to 3.7-flash"), Some("gemini-3.7-flash".to_string()));
        assert_eq!(detect_model("Use gemini-2.5-pro for reasoning"), Some("gemini-2.5-pro".to_string()));
        assert_eq!(detect_model("Switch to 2.5-pro"), Some("gemini-2.5-pro".to_string()));
        assert_eq!(detect_model("Use gemini-2.5-flash for speed"), Some("gemini-2.5-flash".to_string()));
        assert_eq!(detect_model("Switch to 2.5-flash"), Some("gemini-2.5-flash".to_string()));
        assert_eq!(detect_model("Unknown model gemini-1.5-pro"), None);

        // Test transcript flow with model detection
        let content = concat!(
            "{\"source\":\"USER_EXPLICIT\",\"type\":\"USER_INPUT\",\"content\":\"<USER_REQUEST>\\nPlease use gemini-2.5-pro\\n</USER_REQUEST>\"}\n",
            "{\"source\":\"MODEL\",\"type\":\"PLANNER_RESPONSE\",\"created_at\":\"2026-08-28T16:00:00Z\",\"content\":\"Sure\"}\n"
        );
        let calls = parse_source("/path/test.jsonl", "sess-model", content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].model, "gemini-2.5-pro");
        assert_eq!(calls[0].user_message, "Please use gemini-2.5-pro");

        // Unclosed <USER_REQUEST> tag
        let unclosed = concat!(
            "{\"source\":\"USER_EXPLICIT\",\"type\":\"USER_INPUT\",\"content\":\"<USER_REQUEST>Unclosed request\"}\n",
            "{\"source\":\"MODEL\",\"type\":\"PLANNER_RESPONSE\",\"created_at\":\"2026-08-28T16:00:00Z\",\"content\":\"OK\"}\n"
        );
        let calls_unclosed = parse_source("/path/test.jsonl", "sess-unclosed", unclosed);
        assert_eq!(calls_unclosed.len(), 1);
        assert_eq!(calls_unclosed[0].user_message, "<USER_REQUEST>Unclosed request");
    }

    #[test]
    fn thinking_tokens_and_cost_calculation_sanity() {
        let content = concat!(
            "{\"source\":\"USER_EXPLICIT\",\"type\":\"USER_INPUT\",\"content\":\"Think carefully\"}\n",
            "{\"source\":\"MODEL\",\"type\":\"PLANNER_RESPONSE\",\"created_at\":\"2026-08-28T16:00:00Z\",",
            "\"thinking\":\"1234567890123456\",", // 16 bytes -> 4 tokens
            "\"content\":\"Done\"}\n"
        );
        let calls = parse_source("/path/test.jsonl", "sess-thinking", content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].reasoning_tokens, 4);
        assert!(calls[0].cost_usd.is_some());
        let cost = calls[0].cost_usd.unwrap();
        assert!(cost > 0.0);
        assert!(!cost.is_nan());
        assert!(!cost.is_infinite());

        // Empty thinking string
        let content_empty_thinking = concat!(
            "{\"source\":\"USER_EXPLICIT\",\"type\":\"USER_INPUT\",\"content\":\"Think\"}\n",
            "{\"source\":\"MODEL\",\"type\":\"PLANNER_RESPONSE\",\"created_at\":\"2026-08-28T16:00:00Z\",",
            "\"thinking\":\"\",",
            "\"content\":\"Done\"}\n"
        );
        let calls_empty = parse_source("/path/test.jsonl", "sess-empty-thinking", content_empty_thinking);
        assert_eq!(calls_empty.len(), 1);
        assert_eq!(calls_empty[0].reasoning_tokens, 0);

        // Unicode multi-byte thinking
        let content_unicode = concat!(
            "{\"source\":\"USER_EXPLICIT\",\"type\":\"USER_INPUT\",\"content\":\"Think\"}\n",
            "{\"source\":\"MODEL\",\"type\":\"PLANNER_RESPONSE\",\"created_at\":\"2026-08-28T16:00:00Z\",",
            "\"thinking\":\"🤔🤔🤔🤔\",", // 4 emojis * 4 bytes = 16 bytes -> 4 tokens
            "\"content\":\"Done\"}\n"
        );
        let calls_unicode = parse_source("/path/test.jsonl", "sess-unicode", content_unicode);
        assert_eq!(calls_unicode.len(), 1);
        assert_eq!(calls_unicode[0].reasoning_tokens, 4);
    }

    #[test]
    fn tool_args_extraction_and_normalization_variations() {
        let content = concat!(
            "{\"source\":\"USER_EXPLICIT\",\"type\":\"USER_INPUT\",\"content\":\"Run tests\"}\n",
            "{\"source\":\"MODEL\",\"type\":\"PLANNER_RESPONSE\",\"created_at\":\"2026-08-28T16:00:00Z\",",
            "\"tool_calls\":[",
            "{\"name\":\"run_command\",\"args\":{\"command\":\"'pytest -v'\",\"directory\":\"'/tmp/work'\"}},",
            "{\"name\":\"run_command\",\"args\":{\"cmd\":\"echo hello\",\"path\":\"/var/tmp\"}},",
            "{\"name\":\"run_command\",\"args\":{\"input\":\"cargo test\"}},",
            "{\"name\":\"custom_extension_tool\",\"args\":{}}",
            "]}\n"
        );
        let calls = parse_source("/path/test.jsonl", "sess-tools", content);
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].tools,
            vec!["Bash", "Bash", "Bash", "custom_extension_tool"]
        );
        assert_eq!(
            calls[0].bash_commands,
            vec!["pytest -v", "echo hello", "cargo test"]
        );
        assert_eq!(calls[0].project_path, "/var/tmp");
    }

    #[test]
    fn fallback_to_transcript_full_and_directory_traversal_edge_cases() {
        let temp = tempfile::tempdir().unwrap();

        // 1. Session with transcript_full.jsonl only
        let sess1 = temp.path().join("session-full-only");
        let logs1 = sess1.join(".system_generated/logs");
        std::fs::create_dir_all(&logs1).unwrap();
        std::fs::write(logs1.join("transcript_full.jsonl"), sample_transcript()).unwrap();

        // 2. Session with both transcript.jsonl and transcript_full.jsonl (prefers transcript.jsonl)
        let sess2 = temp.path().join("session-both");
        let logs2 = sess2.join(".system_generated/logs");
        std::fs::create_dir_all(&logs2).unwrap();
        std::fs::write(logs2.join("transcript.jsonl"), sample_transcript()).unwrap();
        std::fs::write(logs2.join("transcript_full.jsonl"), "malformed full").unwrap();

        // 3. Session dir with no logs dir
        let sess3 = temp.path().join("session-empty");
        std::fs::create_dir_all(&sess3).unwrap();

        // 4. Non-directory file in brain root
        std::fs::write(temp.path().join("stray_file.txt"), "hello").unwrap();

        let calls = parse_dir(temp.path());
        assert_eq!(calls.len(), 4); // 2 from sess1, 2 from sess2
    }
}

