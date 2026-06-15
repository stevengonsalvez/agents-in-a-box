// ABOUTME: JSONL transcript tail for assistant-turn-end detection.
//
// Watches ~/.claude/projects/<cwd-slug>/<session-id>.jsonl using `notify`.
// Returns when the next assistant turn ends or the timeout fires.

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use notify::{EventKind, RecursiveMode, Watcher};
use serde::Deserialize;
use serde_json::Value;

/// Claude maps cwd → project dir by replacing every non-alphanumeric char
/// (except `-`) with `-`. So `/`, `.`, `_`, etc. all collapse to `-`.
/// Examples:
///   `/Users/stevengonsalvez/d/git/foo`  →  `-Users-stevengonsalvez-d-git-foo`
///   `/Users/stevengonsalvez/.agents-in-a-box/worktrees/foo_bar`
///     →  `-Users-stevengonsalvez--agents-in-a-box-worktrees-foo-bar`
#[must_use]
pub fn cwd_to_project_slug(cwd: &str) -> String {
    cwd.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// Locate the most recently modified `.jsonl` file under
/// `~/.claude/projects/<cwd-slug>/`. Returns `None` if no transcripts exist.
pub fn latest_transcript_for_cwd(cwd: &str) -> Option<PathBuf> {
    let mut home = dirs::home_dir()?;
    home.push(".claude");
    home.push("projects");
    home.push(cwd_to_project_slug(cwd));

    let entries = std::fs::read_dir(&home).ok()?;
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for e in entries.flatten() {
        let path = e.path();
        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(meta) = e.metadata() else { continue };
        let Ok(mtime) = meta.modified() else { continue };
        if newest.as_ref().is_none_or(|(t, _)| mtime > *t) {
            newest = Some((mtime, path));
        }
    }
    newest.map(|(_, p)| p)
}

/// One JSONL row from Claude's transcript. Only the fields we need.
#[derive(Debug, Deserialize)]
struct TranscriptRow {
    /// "user" | "assistant" | "system" | "tool_use" | …
    #[serde(rename = "type")]
    row_type: Option<String>,
    /// Sub-shape for assistant turns — `{ stop_reason: "end_turn" | … }`.
    #[serde(default)]
    message: Option<TranscriptMessage>,
}

#[derive(Debug, Deserialize)]
struct TranscriptMessage {
    #[serde(default)]
    stop_reason: Option<String>,
}

/// Block until the watched transcript shows a new `assistant`-role row whose
/// `message.stop_reason` is `end_turn` (or until `timeout` elapses).
///
/// Returns `true` if we observed a turn-end before timing out.
pub fn wait_for_turn_end(path: &Path, timeout: Duration) -> Result<bool> {
    let start_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let (tx, rx) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    })
    .context("constructing notify watcher")?;
    watcher
        .watch(path, RecursiveMode::NonRecursive)
        .context("starting watch on transcript")?;

    let deadline = Instant::now() + timeout;
    let mut last_offset = start_size;

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(false);
        }
        let event = rx.recv_timeout(remaining);
        match event {
            Ok(Ok(ev)) if matches!(ev.kind, EventKind::Modify(_) | EventKind::Create(_)) => {
                if let Some(found) = scan_for_turn_end(path, &mut last_offset)? {
                    if found {
                        return Ok(true);
                    }
                }
            }
            Ok(Ok(_)) => {}
            Ok(Err(_)) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Ok(false);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                return Ok(false);
            }
        }
    }
}

/// Read from `last_offset` to EOF, scan each complete line for assistant
/// turn-end. Updates `last_offset` to the position after the last complete
/// line we parsed. Tolerates partial trailing writes.
fn scan_for_turn_end(path: &Path, last_offset: &mut u64) -> Result<Option<bool>> {
    use std::io::{BufRead, BufReader, Seek, SeekFrom};

    let mut file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    file.seek(SeekFrom::Start(*last_offset))?;
    let mut reader = BufReader::new(file);
    let mut buf = String::new();
    let mut bytes_consumed = 0u64;
    let mut found_end = false;

    loop {
        buf.clear();
        let n = reader.read_line(&mut buf)?;
        if n == 0 {
            break;
        }
        // Only count fully-terminated lines.
        if !buf.ends_with('\n') {
            break;
        }
        bytes_consumed += n as u64;

        if let Ok(row) = serde_json::from_str::<TranscriptRow>(buf.trim()) {
            let is_assistant = row.row_type.as_deref() == Some("assistant");
            let ended =
                row.message.as_ref().and_then(|m| m.stop_reason.as_deref()) == Some("end_turn");
            if is_assistant && ended {
                found_end = true;
                break;
            }
        }
    }

    *last_offset += bytes_consumed;
    Ok(if found_end { Some(true) } else { None })
}

/// Structured AskUserQuestion data extracted from a transcript tool_use block.
/// Matches the shape of the AskUserQuestion tool's `input` parameter.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AskUserQuestionData {
    pub question: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    pub options: Vec<AskOption>,
    #[serde(default)]
    pub multi_select: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AskOption {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Last assistant turn's timing + text + stop reason — for IDLE detection
/// and for the IDLE card's "last said" snippet.
#[derive(Debug, Clone)]
pub struct LastAssistantInfo {
    /// Unix ms of the timestamp on the assistant row.
    pub ts_ms: i64,
    /// stop_reason field (e.g. "end_turn", "tool_use", "max_tokens").
    pub stop_reason: Option<String>,
    /// First text-block content, truncated and whitespace-collapsed.
    pub text_snippet: Option<String>,
    /// Whether there's a subsequent user-role row in the file.
    pub has_user_follow_up: bool,
}

/// Scan the transcript for the most recent AskUserQuestion tool_use call.
/// Returns None if the session has no such call within the lookback window.
///
/// Walks the same exponential lookback as `last_narrative_snapshot` (20 → 320).
pub fn last_ask_user_question(path: &Path) -> Option<AskUserQuestionData> {
    let lines = read_lines(path)?;
    if lines.is_empty() {
        return None;
    }
    for window in [20usize, 40, 80, 160, 320] {
        let start = lines.len().saturating_sub(window);
        if let Some(aq) = find_ask_user_question(&lines[start..]) {
            return Some(aq);
        }
        if start == 0 {
            break;
        }
    }
    None
}

fn find_ask_user_question(rows: &[String]) -> Option<AskUserQuestionData> {
    for row in rows.iter().rev() {
        let Ok(v) = serde_json::from_str::<Value>(row) else {
            continue;
        };
        if v.get("type").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let content = v.pointer("/message/content").and_then(Value::as_array)?;
        // Walk content backward — last tool_use wins.
        for block in content.iter().rev() {
            let is_tool = block.get("type").and_then(Value::as_str) == Some("tool_use");
            let name = block.get("name").and_then(Value::as_str);
            if !is_tool || name != Some("AskUserQuestion") {
                continue;
            }
            let input = block.get("input")?;
            let questions = input.get("questions").and_then(Value::as_array)?;
            let first = questions.first()?;
            return Some(AskUserQuestionData {
                question: first
                    .get("question")
                    .and_then(Value::as_str)
                    .unwrap_or("(no question text)")
                    .to_string(),
                header: first.get("header").and_then(Value::as_str).map(str::to_string),
                options: first
                    .get("options")
                    .and_then(Value::as_array)
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|o| {
                                Some(AskOption {
                                    label: o.get("label").and_then(Value::as_str)?.to_string(),
                                    description: o
                                        .get("description")
                                        .and_then(Value::as_str)
                                        .map(str::to_string),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                multi_select: first.get("multiSelect").and_then(Value::as_bool).unwrap_or(false),
            });
        }
        // No tool_use in this assistant row — keep searching older rows.
    }
    None
}

/// Probe the transcript for the last assistant turn's timing + stop reason.
/// Used by the IDLE classifier.
pub fn last_assistant_info(path: &Path) -> Option<LastAssistantInfo> {
    let lines = read_lines(path)?;
    if lines.is_empty() {
        return None;
    }
    // Walk backward; first assistant row wins. Track whether we see a user
    // row AFTER it (i.e. earlier in our reverse walk).
    let mut saw_user_after = false;
    for row in lines.iter().rev() {
        let Ok(v) = serde_json::from_str::<Value>(row) else {
            continue;
        };
        let row_type = v.get("type").and_then(Value::as_str).unwrap_or("");
        if row_type == "user" {
            saw_user_after = true;
            continue;
        }
        if row_type != "assistant" {
            continue;
        }
        let ts_ms = parse_ts_ms(v.get("timestamp").and_then(Value::as_str).unwrap_or(""));
        let stop_reason =
            v.pointer("/message/stop_reason").and_then(Value::as_str).map(str::to_string);
        let text_snippet =
            v.pointer("/message/content").and_then(Value::as_array).and_then(|arr| {
                let mut buf = String::new();
                for b in arr {
                    if b.get("type").and_then(Value::as_str) == Some("text") {
                        if let Some(t) = b.get("text").and_then(Value::as_str) {
                            buf.push_str(t);
                            buf.push(' ');
                        }
                    }
                }
                let collapsed = collapse_whitespace(buf.trim());
                if collapsed.is_empty() {
                    None
                } else {
                    Some(truncate(&collapsed, 120))
                }
            });
        return Some(LastAssistantInfo {
            ts_ms: ts_ms.unwrap_or(0),
            stop_reason,
            text_snippet,
            has_user_follow_up: saw_user_after,
        });
    }
    None
}

fn read_lines(path: &Path) -> Option<Vec<String>> {
    let file = std::fs::File::open(path).ok()?;
    let reader = std::io::BufReader::new(file);
    Some(reader.lines().map_while(Result::ok).collect())
}

/// Fallback ERR detection from the transcript. Reverse-scans the newest
/// `window` JSONL rows (as raw text) for an API-error signal — used when the
/// tmux pane capture misses the error (scrolled past the 80-line window, or
/// the capture itself failed). Newest match wins. Returns `(pattern, raw)`.
pub fn last_api_error_from_jsonl(
    path: &Path,
    window: usize,
    at_ms: i64,
) -> Option<(String, String)> {
    let lines = read_lines(path)?;
    if lines.is_empty() {
        return None;
    }
    let start = lines.len().saturating_sub(window);
    for row in lines[start..].iter().rev() {
        let sigs = crate::fleet::read::errors::detect_error_signals(row, at_ms);
        if let Some(crate::fleet::types::Signal::ApiError { pattern, raw, .. }) =
            sigs.into_iter().next()
        {
            return Some((pattern, raw));
        }
    }
    None
}

fn parse_ts_ms(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s).ok().map(|dt| dt.timestamp_millis())
}

/// Synthesise a one-line "what is this session doing right now" string from
/// the transcript. Walks recent rows backward looking for the freshest
/// assistant turn with substance.
///
/// Exponential lookback: scans the last 20 rows first, expanding to 40, 80,
/// 160, then 320 if no signal turns up. Stops at the first window that yields
/// content. Cheap for active sessions (signal lives in the last few rows);
/// resilient when the tail is full of tool_result noise.
pub fn last_narrative_snapshot(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let reader = std::io::BufReader::new(file);
    let lines: Vec<String> = reader.lines().map_while(Result::ok).collect();
    if lines.is_empty() {
        return None;
    }
    for window in [20usize, 40, 80, 160, 320] {
        let start = lines.len().saturating_sub(window);
        if let Some(snap) = synthesize_from_rows(&lines[start..]) {
            return Some(snap);
        }
        if start == 0 {
            break;
        }
    }
    None
}

fn synthesize_from_rows(rows: &[String]) -> Option<String> {
    // Walk backwards; first assistant row with text-or-tool wins.
    for row in rows.iter().rev() {
        let Ok(v) = serde_json::from_str::<Value>(row) else {
            continue;
        };
        if v.get("type").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let content = v.pointer("/message/content");
        if let Some(arr) = content.and_then(Value::as_array) {
            // Prefer the LAST tool_use block — that's what the session is doing right now.
            for block in arr.iter().rev() {
                if block.get("type").and_then(Value::as_str) == Some("tool_use") {
                    let name = block.get("name").and_then(Value::as_str).unwrap_or("?");
                    let arg = tool_arg_snippet(block.get("input"));
                    return Some(if arg.is_empty() {
                        format!("{name}")
                    } else {
                        format!("{name} · {arg}")
                    });
                }
            }
            // No tool calls — concatenate text blocks.
            let mut text = String::new();
            for block in arr.iter() {
                if block.get("type").and_then(Value::as_str) == Some("text") {
                    if let Some(t) = block.get("text").and_then(Value::as_str) {
                        text.push_str(t);
                        text.push(' ');
                    }
                }
            }
            let collapsed = collapse_whitespace(text.trim());
            if !collapsed.is_empty() {
                return Some(truncate(&collapsed, 100));
            }
        }
        // Rare: message.content as plain string.
        if let Some(s) = content.and_then(Value::as_str) {
            let collapsed = collapse_whitespace(s.trim());
            if !collapsed.is_empty() {
                return Some(truncate(&collapsed, 100));
            }
        }
    }
    None
}

/// Pull the most descriptive field from a tool_use input object.
fn tool_arg_snippet(input: Option<&Value>) -> String {
    let Some(input) = input else {
        return String::new();
    };
    let pick = input
        .get("command")
        .or_else(|| input.get("file_path"))
        .or_else(|| input.get("path"))
        .or_else(|| input.get("pattern"))
        .or_else(|| input.get("description"))
        .or_else(|| input.get("query"))
        .or_else(|| input.get("subject"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let collapsed = collapse_whitespace(pick);
    truncate(&collapsed, 60)
}

fn collapse_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_a_typical_cwd() {
        assert_eq!(
            cwd_to_project_slug("/Users/stevengonsalvez/d/git/foo"),
            "-Users-stevengonsalvez-d-git-foo"
        );
    }

    #[test]
    fn slugs_dots_and_underscores() {
        // `.` and `_` both collapse to `-`. `/.` → `--`.
        assert_eq!(
            cwd_to_project_slug("/Users/stevengonsalvez/.agents-in-a-box/worktrees/foo_bar_baz"),
            "-Users-stevengonsalvez--agents-in-a-box-worktrees-foo-bar-baz"
        );
    }

    #[test]
    fn synth_extracts_tool_use() {
        let rows = vec![
            r#"{"type":"user","message":{"content":"go"}}"#.to_string(),
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Running tests."},{"type":"tool_use","name":"Bash","input":{"command":"cargo test --lib"}}]}}"#.to_string(),
        ];
        let s = synthesize_from_rows(&rows).expect("synthesised");
        assert!(s.contains("Bash"), "got: {s}");
        assert!(s.contains("cargo test"), "got: {s}");
    }

    #[test]
    fn synth_falls_back_to_text_when_no_tool() {
        let rows = vec![
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Investigating the RLS chain on auth.audit_log inserts."}]}}"#.to_string(),
        ];
        let s = synthesize_from_rows(&rows).expect("synthesised");
        assert!(s.contains("Investigating"), "got: {s}");
        assert!(s.contains("RLS"), "got: {s}");
    }

    #[test]
    fn synth_skips_user_rows() {
        let rows = vec![r#"{"type":"user","message":{"content":"hello"}}"#.to_string()];
        assert!(synthesize_from_rows(&rows).is_none());
    }

    #[test]
    fn jsonl_err_fallback_finds_error_row() {
        use std::io::Write;
        let path =
            std::env::temp_dir().join(format!("ainb-jsonl-err-{}.jsonl", std::process::id()));
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"all good"}}]}}}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"type":"system","content":"API Error: rate_limited please retry"}}"#
        )
        .unwrap();
        drop(f);
        let hit = last_api_error_from_jsonl(&path, 40, 0);
        let _ = std::fs::remove_file(&path);
        let (pattern, _) = hit.expect("should find an error in the JSONL tail");
        assert_eq!(pattern, "rate_limited");
    }

    #[test]
    fn jsonl_err_fallback_clean_returns_none() {
        use std::io::Write;
        let path =
            std::env::temp_dir().join(format!("ainb-jsonl-clean-{}.jsonl", std::process::id()));
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"all good"}}]}}}}"#
        )
        .unwrap();
        drop(f);
        let hit = last_api_error_from_jsonl(&path, 40, 0);
        let _ = std::fs::remove_file(&path);
        assert!(hit.is_none());
    }
}
