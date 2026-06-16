//! GitHub Copilot CLI session parser.
//!
//! Layout: `<sessions_root>/<uuid>/events.jsonl` — one append-only event
//! stream per session (the `~/.copilot/session-state/<uuid>/` tree). One
//! level: the root lists per-session uuid dirs; each holds an
//! `events.jsonl`.
//!
//! ## Token model — read this before touching the mapping
//!
//! Real token usage is written exactly once per session, per model, in
//! the `session.shutdown` event's `data.modelMetrics` block:
//!
//! ```json
//! {"type":"session.shutdown","data":{"modelMetrics":{"gpt-5.4":{
//!   "requests":{"count":31,"cost":3},
//!   "usage":{"inputTokens":..,"outputTokens":..,
//!            "cacheReadTokens":..,"cacheWriteTokens":..}}}},
//!  "timestamp":".."}
//! ```
//!
//! - `assistant.message` carries only `outputTokens`; `assistant.turn_end`
//!   is just `{turnId}`. Input/cache tokens exist ONLY in the shutdown
//!   aggregate — so we mint one [`ProviderCall`] per `(session, model)`,
//!   NOT per turn, and never sum `assistant.message` output on top (that
//!   double-counts).
//! - The `cost` / `requests.cost` field is a **premium-request count**
//!   (it equals `totalPremiumRequests`), NOT US dollars. `cost_usd` is
//!   derived via [`estimate_cost_usd`]; the on-disk `cost` is ignored.
//! - "Real tokens only": a session with no clean `session.shutdown`
//!   (crashed/killed) or that predates the events.jsonl format emits
//!   nothing. No estimated or zeroed rows.

use std::collections::BTreeMap;
use std::path::Path;

use ainb_plugin_types_sessions::{Provider, ProviderCall};
use serde::Deserialize;
use serde_json::Value;

use crate::fnv::provider_call_id;

use super::{estimate_cost_usd, parse_timestamp};

/// One line of `events.jsonl`. The variable payload lives in `data`,
/// dug out per event type — robust to the schema gaining fields.
#[derive(Deserialize)]
struct CopilotEvent {
    #[serde(rename = "type")]
    event_type: String,
    timestamp: Option<String>,
    data: Option<Value>,
}

/// Walk `<sessions_root>/<uuid>/events.jsonl`. Missing roots degrade to
/// an empty result; per-file failures are logged and skipped.
pub fn parse_dir(sessions_root: &Path) -> Vec<ProviderCall> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut cache = None;
        let mut ctx = crate::scanner::ScanCtx::full(&mut cache);
        parse_dir_cached(sessions_root, &mut ctx)
    }
    #[cfg(target_arch = "wasm32")]
    {
        parse_dir_uncached(sessions_root)
    }
}

/// Cache-aware variant of [`parse_dir`]. `cache: None` is equivalent to
/// the no-cache path; a `Some(cache)` short-circuits per-file parses when
/// `(mtime, size)` matches the stored row.
#[cfg(not(target_arch = "wasm32"))]
pub fn parse_dir_cached(
    sessions_root: &Path,
    ctx: &mut crate::scanner::ScanCtx<'_>,
) -> Vec<ProviderCall> {
    let mut reporter = crate::scanner::ProgressReporter::noop();
    parse_dir_cached_with_progress(sessions_root, ctx, &mut reporter)
}

/// Cache + progress-aware walk. Drives `reporter.note_file` once per
/// `events.jsonl` file (whether served from cache or freshly parsed).
#[cfg(not(target_arch = "wasm32"))]
pub fn parse_dir_cached_with_progress(
    sessions_root: &Path,
    ctx: &mut crate::scanner::ScanCtx<'_>,
    reporter: &mut crate::scanner::ProgressReporter,
) -> Vec<ProviderCall> {
    let mut calls = Vec::new();
    let session_dirs = match std::fs::read_dir(sessions_root) {
        Ok(entries) => entries,
        Err(err) => {
            if err.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    path = %sessions_root.display(),
                    error = %err,
                    "session-reader/copilot: read sessions root failed"
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
        let path = session_dir.join("events.jsonl");
        if !path.is_file() {
            continue;
        }
        let uuid = path_basename(&session_dir);
        let path_str = path.to_string_lossy().into_owned();
        let uuid_owned = uuid.clone();
        reporter.note_file(&uuid_owned);
        let file_calls = super::read_file_cached(&path, ctx, |content| {
            parse_source(&path_str, &uuid_owned, content)
        });
        calls.extend(file_calls);
    }
    calls
}

/// Uncached walk used by the wasm32 build (where the cache module is
/// stubbed out). Kept private; the public API stays `parse_dir`.
#[cfg(target_arch = "wasm32")]
fn parse_dir_uncached(sessions_root: &Path) -> Vec<ProviderCall> {
    let mut calls = Vec::new();
    let session_dirs = match std::fs::read_dir(sessions_root) {
        Ok(entries) => entries,
        Err(_) => return calls,
    };
    for session_entry in session_dirs.flatten() {
        let session_dir = session_entry.path();
        if !session_dir.is_dir() {
            continue;
        }
        let path = session_dir.join("events.jsonl");
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        let uuid = path_basename(&session_dir);
        let path_str = path.to_string_lossy().into_owned();
        calls.extend(parse_source(&path_str, &uuid, &content));
    }
    calls
}

/// Parse one `events.jsonl` into [`ProviderCall`]s — one call per
/// `(session, model)` recorded in the `session.shutdown` aggregate.
/// Public so unit tests can drive synthetic fixtures without touching
/// the filesystem.
pub fn parse_source(path: &str, session_uuid: &str, content: &str) -> Vec<ProviderCall> {
    let mut cwd = "unknown".to_string();
    let mut project = "unknown".to_string();
    let mut branch: Option<String> = None;
    let mut user_message = String::new();
    let mut tools: Vec<String> = Vec::new();
    let mut bash_commands: Vec<String> = Vec::new();
    let mut calls = Vec::new();

    let mut offset: u64 = 0;
    for chunk in content.split_inclusive('\n') {
        let line_offset = offset;
        offset += chunk.len() as u64;
        let line = chunk.strip_suffix('\n').unwrap_or(chunk);
        let event: CopilotEvent = match serde_json::from_str(line) {
            Ok(event) => event,
            Err(_) => continue,
        };
        let data = event.data.as_ref();

        match event.event_type.as_str() {
            "session.start" => {
                if let Some(ctx) = data.and_then(|d| d.get("context")) {
                    if let Some(c) = ctx.get("cwd").and_then(Value::as_str) {
                        cwd = c.to_string();
                        project = sanitize_project(c);
                    }
                    branch = ctx
                        .get("branch")
                        .and_then(Value::as_str)
                        .filter(|b| !b.is_empty())
                        .map(ToString::to_string);
                }
            }
            "user.message" => {
                if user_message.is_empty() {
                    if let Some(text) = data.and_then(|d| d.get("content")).and_then(Value::as_str)
                    {
                        user_message = text.to_string();
                    }
                }
            }
            "tool.execution_start" => {
                if let Some(name) = data.and_then(|d| d.get("toolName")).and_then(Value::as_str) {
                    tools.push(name.to_string());
                    if is_shell_tool(name) {
                        if let Some(cmd) =
                            data.and_then(|d| d.get("arguments")).and_then(extract_command)
                        {
                            bash_commands.push(cmd);
                        }
                    }
                }
            }
            "session.shutdown" => {
                let Some(timestamp) = event.timestamp.as_deref().and_then(parse_timestamp) else {
                    continue;
                };
                let Some(metrics) =
                    data.and_then(|d| d.get("modelMetrics")).and_then(Value::as_object)
                else {
                    continue;
                };
                // Deterministic model order so re-parses emit calls in a
                // stable sequence (the map is otherwise arbitrary).
                let ordered: BTreeMap<&String, &Value> = metrics.iter().collect();
                let mut first = true;
                for (model, entry) in ordered {
                    let usage = entry.get("usage");
                    let input = usage_u64(usage, "inputTokens");
                    let output = usage_u64(usage, "outputTokens");
                    let cache_read = usage_u64(usage, "cacheReadTokens");
                    let cache_write = usage_u64(usage, "cacheWriteTokens");
                    if input + output + cache_read + cache_write == 0 {
                        continue;
                    }
                    let cost_usd =
                        estimate_cost_usd(model, input, output, cache_write, cache_read, 0);

                    // Session-level fields (tools, user message) belong to
                    // the call as a whole — attribute them to the first
                    // model only so a multi-model session doesn't
                    // double-count tool executions.
                    let (call_tools, call_bash, call_user) = if first {
                        (
                            std::mem::take(&mut tools),
                            std::mem::take(&mut bash_commands),
                            std::mem::take(&mut user_message),
                        )
                    } else {
                        (Vec::new(), Vec::new(), String::new())
                    };
                    first = false;

                    calls.push(ProviderCall {
                        // Fold a multi-model shutdown's calls apart: same
                        // line offset, distinct model → distinct id.
                        id: provider_call_id(&format!("{path}#{model}"), line_offset),
                        provider: Provider::Copilot,
                        model: model.clone(),
                        session_id: session_uuid.to_string(),
                        project: project.clone(),
                        project_path: cwd.clone(),
                        timestamp,
                        input_tokens: input,
                        cache_creation_tokens: cache_write,
                        cache_read_tokens: cache_read,
                        output_tokens: output,
                        reasoning_tokens: 0,
                        cost_usd,
                        tools: call_tools,
                        bash_commands: call_bash,
                        user_message: call_user,
                        branch: branch.clone(),
                    });
                }
            }
            _ => {}
        }
    }
    calls
}

/// `usage.<field>` as `u64`, defaulting to 0 when absent.
fn usage_u64(usage: Option<&Value>, field: &str) -> u64 {
    usage.and_then(|u| u.get(field)).and_then(Value::as_u64).unwrap_or(0)
}

/// A tool whose arguments carry a shell command line worth recording in
/// `bash_commands`.
fn is_shell_tool(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.contains("bash") || n.contains("shell") || n.contains("terminal")
}

/// Pull a command string from a `tool.execution_start` arguments object,
/// trying the field names Copilot tools use.
fn extract_command(args: &Value) -> Option<String> {
    for key in ["command", "commandLine", "script", "cmd"] {
        if let Some(cmd) = args.get(key).and_then(Value::as_str) {
            return Some(cmd.to_string());
        }
    }
    None
}

/// `/Users/x/proj` → `Users-x-proj`. Mirrors the Codex parser so two
/// worktrees of the same upstream stay distinct projects (the aggregator
/// keys on this string as-is).
fn sanitize_project(cwd: &str) -> String {
    cwd.trim_start_matches('/').replace('/', "-")
}

fn path_basename(path: &Path) -> String {
    path.file_name()
        .and_then(|s| s.to_str())
        .map_or_else(|| path.to_string_lossy().into_owned(), ToString::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start_line() -> &'static str {
        r#"{"type":"session.start","timestamp":"2026-03-19T10:00:00Z","data":{"sessionId":"u-1","context":{"cwd":"/tmp/proj","branch":"main","repository":"acme/proj"}}}"#
    }

    fn user_line() -> &'static str {
        r#"{"type":"user.message","timestamp":"2026-03-19T10:00:01Z","data":{"content":"do a thing","source":"user"}}"#
    }

    fn tool_line() -> &'static str {
        r#"{"type":"tool.execution_start","timestamp":"2026-03-19T10:00:02Z","data":{"toolCallId":"c1","toolName":"bash","arguments":{"command":"ls -la"}}}"#
    }

    fn shutdown_line() -> &'static str {
        r#"{"type":"session.shutdown","timestamp":"2026-03-19T10:58:49.686Z","data":{"shutdownType":"routine","totalPremiumRequests":3,"modelMetrics":{"gpt-5.4":{"requests":{"count":31,"cost":3},"usage":{"inputTokens":1278715,"outputTokens":19175,"cacheReadTokens":1038464,"cacheWriteTokens":0}}}}}"#
    }

    fn full_session() -> String {
        [start_line(), user_line(), tool_line(), shutdown_line()].join("\n") + "\n"
    }

    #[test]
    fn shutdown_metrics_become_one_call_per_model() {
        let calls = parse_source("/p/u-1/events.jsonl", "u-1", &full_session());
        assert_eq!(calls.len(), 1);
        let c = &calls[0];
        assert_eq!(c.provider, Provider::Copilot);
        assert_eq!(c.model, "gpt-5.4");
        assert_eq!(c.session_id, "u-1");
        assert_eq!(c.project, "tmp-proj");
        assert_eq!(c.project_path, "/tmp/proj");
        assert_eq!(c.input_tokens, 1_278_715);
        assert_eq!(c.output_tokens, 19_175);
        assert_eq!(c.cache_read_tokens, 1_038_464);
        assert_eq!(c.cache_creation_tokens, 0);
        assert_eq!(c.branch.as_deref(), Some("main"));
        assert_eq!(c.user_message, "do a thing");
        assert_eq!(c.tools, vec!["bash".to_string()]);
        assert_eq!(c.bash_commands, vec!["ls -la".to_string()]);
    }

    #[test]
    fn cost_is_estimated_not_the_premium_request_count() {
        let calls = parse_source("/p/u-1/events.jsonl", "u-1", &full_session());
        // gpt-5* priced in cost.rs → some USD; never the on-disk `cost:3`.
        let cost = calls[0].cost_usd.expect("gpt-5* has pricing");
        assert!(
            cost > 0.0 && (cost - 3.0).abs() > f64::EPSILON,
            "got {cost}"
        );
    }

    #[test]
    fn session_without_shutdown_emits_nothing() {
        let content = [start_line(), user_line(), tool_line()].join("\n") + "\n";
        let calls = parse_source("/p/u-1/events.jsonl", "u-1", &content);
        assert!(calls.is_empty(), "no shutdown → no real tokens → no rows");
    }

    #[test]
    fn shutdown_with_zero_usage_is_skipped() {
        let line = r#"{"type":"session.shutdown","timestamp":"2026-03-19T10:58:49.686Z","data":{"modelMetrics":{"gpt-5.4":{"usage":{"inputTokens":0,"outputTokens":0,"cacheReadTokens":0,"cacheWriteTokens":0}}}}}"#;
        let calls = parse_source("/p/u-1/events.jsonl", "u-1", &format!("{line}\n"));
        assert!(calls.is_empty());
    }

    #[test]
    fn multi_model_shutdown_emits_distinct_calls_without_double_counting_tools() {
        let line = r#"{"type":"session.shutdown","timestamp":"2026-03-19T10:58:49.686Z","data":{"modelMetrics":{"gpt-5.4":{"usage":{"inputTokens":100,"outputTokens":10,"cacheReadTokens":0,"cacheWriteTokens":0}},"claude-sonnet":{"usage":{"inputTokens":200,"outputTokens":20,"cacheReadTokens":0,"cacheWriteTokens":0}}}}}"#;
        let content = [start_line(), tool_line(), line].join("\n") + "\n";
        let calls = parse_source("/p/u-1/events.jsonl", "u-1", &content);
        assert_eq!(calls.len(), 2);
        assert_ne!(calls[0].id, calls[1].id, "distinct id per model");
        // Tools attributed to exactly one of the two model calls.
        let total_tools: usize = calls.iter().map(|c| c.tools.len()).sum();
        assert_eq!(
            total_tools, 1,
            "tool executions counted once, not per model"
        );
    }

    #[test]
    fn skips_unparseable_lines() {
        let content = format!("not json\n{}", full_session());
        let calls = parse_source("/p/u-1/events.jsonl", "u-1", &content);
        assert_eq!(calls.len(), 1);
    }

    #[test]
    fn missing_root_returns_empty_without_panic() {
        let calls = parse_dir(Path::new("/this/does/not/exist"));
        assert!(calls.is_empty());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn parse_dir_walks_uuid_dirs_and_collects_calls() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session_dir = tmp.path().join("08fcfc80-1bf2-4d87-b969-e2e48a9262b6");
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(session_dir.join("events.jsonl"), full_session()).unwrap();
        // A uuid dir with no events.jsonl is skipped, not an error.
        std::fs::create_dir_all(tmp.path().join("empty-session")).unwrap();
        let calls = parse_dir(tmp.path());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].session_id, "08fcfc80-1bf2-4d87-b969-e2e48a9262b6");
    }
}
