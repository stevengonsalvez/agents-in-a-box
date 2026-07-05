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

/// Canonicalize `cwd` so a symlinked working directory resolves to the SAME
/// slug Claude derives from its real cwd. On macOS Claude runs under the
/// resolved `/private/tmp/…` (or `/private/var/…`) path, so a caller that only
/// knows the `/tmp/…` (or `/var/…`) symlink would otherwise slug to a DIFFERENT
/// project dir and never find the transcript. `std::fs::canonicalize` resolves
/// the symlinks; when the path does not exist (a synthetic/test cwd) or cannot
/// be canonicalized we fall back to the literal input, so a non-existent cwd
/// keeps its verbatim slug.
#[must_use]
pub fn canonical_cwd(cwd: &str) -> String {
    std::fs::canonicalize(cwd)
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| cwd.to_string())
}

/// Locate the most recently modified `.jsonl` file under
/// `~/.claude/projects/<cwd-slug>/`. Returns `None` if no transcripts exist.
///
/// The cwd is canonicalized first ([`canonical_cwd`]) so a `/tmp`-rooted
/// workdir matches Claude's `/private/tmp`-rooted transcript dir on macOS.
pub fn latest_transcript_for_cwd(cwd: &str) -> Option<PathBuf> {
    let mut home = dirs::home_dir()?;
    home.push(".claude");
    home.push("projects");
    home.push(cwd_to_project_slug(&canonical_cwd(cwd)));

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

/// Is this assistant row's `stop_reason` an end-of-turn signal?
///
/// TURN-END REALITY: Claude (>= 2.1.x) streams the assistant turn as JSONL rows
/// that all carry `stop_reason: null` — the visible reply text lands in a
/// `stop_reason: null` row, and the only `"end_turn"` (if any) rides a later
/// metadata row that carries NO text. Code that gated on
/// `stop_reason == "end_turn"` therefore never fired on 2.1.x: a finished turn
/// was never recognised. We treat an absent/`null` `stop_reason` as a candidate
/// end-of-turn too. Non-terminal reasons (`tool_use`, `max_tokens`, etc.) are
/// still rejected — they mean the turn is mid-flight.
///
/// SAFETY: accepting `null` is only safe when the caller ALSO confirms the row
/// carries visible assistant text (a streaming/tool-only row also has
/// `stop_reason: null`). Every caller pairs this with such a gate:
///   * `scan_for_turn_end` requires a non-empty `text` block on the row.
///   * `transport.rs::scan_new_rows_for_turn_end` only records a reply when
///     `assistant_text_from_row` yielded Some(text).
///   * `needs.rs` IDLE additionally requires `!has_user_follow_up` and an
///     `age_min >= idle_threshold_min` (default 5) window, so a 5-min-old
///     assistant row is realistically finished.
#[must_use]
pub fn is_turn_end_stop_reason(stop_reason: Option<&str>) -> bool {
    matches!(stop_reason, None | Some("end_turn"))
}

/// One JSONL row from Claude's transcript. Only the fields we need.
#[derive(Debug, Deserialize)]
struct TranscriptRow {
    /// "user" | "assistant" | "system" | "tool_use" | …
    #[serde(rename = "type")]
    row_type: Option<String>,
    /// Sub-shape for assistant turns — `{ stop_reason, content }`.
    #[serde(default)]
    message: Option<TranscriptMessage>,
}

#[derive(Debug, Deserialize)]
struct TranscriptMessage {
    #[serde(default)]
    stop_reason: Option<String>,
    /// `message.content` — an array of blocks (`text` / `tool_use` / …) or, on
    /// rare rows, a plain string. Used for the text-presence gate that makes
    /// accepting a `null` `stop_reason` safe (see `is_turn_end_stop_reason`).
    #[serde(default)]
    content: Option<Value>,
}

impl TranscriptMessage {
    /// Does this assistant message carry a non-empty visible `text` block?
    /// A tool-only / streaming row has no such block, so a `null`-stop_reason
    /// row that is merely mid-flight is not mistaken for a finished turn.
    fn has_visible_text(&self) -> bool {
        match &self.content {
            Some(Value::Array(blocks)) => blocks.iter().any(|b| {
                b.get("type").and_then(Value::as_str) == Some("text")
                    && b.get("text").and_then(Value::as_str).is_some_and(|t| !t.trim().is_empty())
            }),
            Some(Value::String(s)) => !s.trim().is_empty(),
            _ => false,
        }
    }
}

/// Block until the watched transcript shows a new `assistant`-role row that
/// ends a turn — a row carrying visible text whose `message.stop_reason` is an
/// end-of-turn signal (`null` or `"end_turn"`; see [`is_turn_end_stop_reason`])
/// — or until `timeout` elapses.
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
            if is_assistant {
                if let Some(msg) = row.message.as_ref() {
                    // TEXT-GATE: a finished turn is an assistant row that BOTH
                    // carries visible text AND has an end-of-turn stop_reason.
                    // On 2.1.x the reply text lands on a `stop_reason: null`
                    // row, so we accept null — but only when the row actually
                    // has a text block, else a mid-stream/tool-only `null` row
                    // would falsely signal turn-end.
                    let ended = is_turn_end_stop_reason(msg.stop_reason.as_deref());
                    if ended && msg.has_visible_text() {
                        found_end = true;
                        break;
                    }
                }
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

/// Scan the transcript for the most recent OPEN AskUserQuestion — a picker that
/// has been raised but NOT yet resolved. Returns None if the session has no
/// OPEN ask within the lookback window (including the case where every ask it
/// raised has since been answered).
///
/// STICKY-ASK FIX: the previous version returned the last AskUserQuestion
/// tool_use *regardless of whether it was answered*, so a single interview
/// pinned the session to `ASK` forever — it could never go IDLE and auto-standup
/// never became eligible. An ask is a live need only while it is UNANSWERED.
///
/// CLOSING SIGNAL (verified against real `~/.claude/projects` transcripts):
/// Claude closes EVERY tool call — AskUserQuestion included — with a
/// `tool_result` block on a later `user`-role row whose `tool_use_id` matches
/// the original `tool_use` block's `id`. This holds identically for a real
/// answer (`"Your questions have been answered: …"`), a 60s AFK timeout
/// (`"No response after 60s …"`), and an interrupt — all three emit the paired
/// `tool_result`. So an ask is OPEN iff no such `tool_result` follows it.
///
/// Walks the same exponential lookback as `last_narrative_snapshot` (20 → 320).
/// A tool_result always trails its tool_use, so any window that contains an ask
/// also contains that ask's result (if it was answered) — the per-window
/// closure check is therefore complete.
pub fn last_ask_user_question(path: &Path) -> Option<AskUserQuestionData> {
    let lines = read_lines(path)?;
    if lines.is_empty() {
        return None;
    }
    for window in [20usize, 40, 80, 160, 320] {
        let start = lines.len().saturating_sub(window);
        if let Some(aq) = find_open_ask_user_question(&lines[start..]) {
            return Some(aq);
        }
        if start == 0 {
            break;
        }
    }
    None
}

/// Collect the set of `tool_use` ids that have a matching `tool_result` in
/// `rows` — i.e. tool calls that have been RESOLVED. Claude emits a
/// `tool_result` block (on a `user`-role row) carrying the original call's
/// `tool_use_id` for every completed tool call, so membership here means that
/// call is closed. Used to tell an OPEN AskUserQuestion from an answered one.
fn resolved_tool_use_ids(rows: &[String]) -> std::collections::HashSet<String> {
    let mut ids = std::collections::HashSet::new();
    for row in rows {
        let Ok(v) = serde_json::from_str::<Value>(row) else {
            continue;
        };
        let Some(content) = v.pointer("/message/content").and_then(Value::as_array) else {
            continue;
        };
        for block in content {
            if block.get("type").and_then(Value::as_str) == Some("tool_result") {
                if let Some(id) = block.get("tool_use_id").and_then(Value::as_str) {
                    ids.insert(id.to_string());
                }
            }
        }
    }
    ids
}

/// The newest UNANSWERED AskUserQuestion in `rows`, or None. Walks assistant
/// rows newest-first and returns the first AskUserQuestion `tool_use` block
/// whose `id` has NO matching `tool_result` (see [`resolved_tool_use_ids`]). An
/// ask block with no `id` (older/edge transcript shapes) is treated as open,
/// preserving the pre-closure behaviour for transcripts that never carried ids.
fn find_open_ask_user_question(rows: &[String]) -> Option<AskUserQuestionData> {
    let resolved = resolved_tool_use_ids(rows);
    for row in rows.iter().rev() {
        let Ok(v) = serde_json::from_str::<Value>(row) else {
            continue;
        };
        if v.get("type").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let Some(content) = v.pointer("/message/content").and_then(Value::as_array) else {
            continue;
        };
        // Walk content backward — last tool_use wins within a row.
        for block in content.iter().rev() {
            let is_tool = block.get("type").and_then(Value::as_str) == Some("tool_use");
            let name = block.get("name").and_then(Value::as_str);
            if !is_tool || name != Some("AskUserQuestion") {
                continue;
            }
            // OPEN-ASK GATE: skip an ask that already has a paired tool_result —
            // it was answered / timed out / interrupted, so it is no longer a
            // live need. Keep walking to an older ask (there normally isn't one:
            // Claude cannot raise a new ask while a prior one is unanswered).
            if let Some(id) = block.get("id").and_then(Value::as_str) {
                if resolved.contains(id) {
                    continue;
                }
            }
            return parse_ask_data(block);
        }
        // No open AskUserQuestion in this assistant row — keep searching older rows.
    }
    None
}

/// Extract [`AskUserQuestionData`] from an AskUserQuestion `tool_use` block's
/// `input`. Returns None if the block is malformed (no `questions` array).
fn parse_ask_data(block: &Value) -> Option<AskUserQuestionData> {
    let input = block.get("input")?;
    let questions = input.get("questions").and_then(Value::as_array)?;
    let first = questions.first()?;
    Some(AskUserQuestionData {
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
    })
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

    // --- turn-end stop_reason helper -------------------------------------

    #[test]
    fn is_turn_end_accepts_null_and_end_turn() {
        // 2.1.x stamps the finished text row with null; "end_turn" still works.
        assert!(is_turn_end_stop_reason(None));
        assert!(is_turn_end_stop_reason(Some("end_turn")));
    }

    #[test]
    fn is_turn_end_rejects_non_terminal_reasons() {
        assert!(!is_turn_end_stop_reason(Some("tool_use")));
        assert!(!is_turn_end_stop_reason(Some("max_tokens")));
        assert!(!is_turn_end_stop_reason(Some("stop_sequence")));
    }

    // --- scan_for_turn_end: text-gated null acceptance -------------------

    /// Write `rows` (one JSONL object per element) to a fresh tmp file and run
    /// `scan_for_turn_end` over the whole file. Returns whether turn-end fired.
    fn scan_detects_turn_end(rows: &[&str]) -> bool {
        use std::io::Write;
        let path = std::env::temp_dir().join(format!(
            "ainb-scan-turnend-{}-{:p}.jsonl",
            std::process::id(),
            rows
        ));
        {
            let mut f = std::fs::File::create(&path).unwrap();
            for r in rows {
                writeln!(f, "{r}").unwrap();
            }
        }
        let mut offset = 0u64;
        let found = scan_for_turn_end(&path, &mut offset).unwrap();
        let _ = std::fs::remove_file(&path);
        found == Some(true)
    }

    #[test]
    fn scan_turn_end_null_stop_reason_with_text_is_turn_end() {
        // 2.1.x reality: the finished reply lands on a `stop_reason: null` row.
        assert!(scan_detects_turn_end(&[
            r#"{"type":"user","message":{"content":"ping"}}"#,
            r#"{"type":"assistant","message":{"stop_reason":null,"content":[{"type":"text","text":"PONG"}]}}"#,
        ]));
    }

    #[test]
    fn scan_turn_end_null_stop_reason_no_text_is_not_turn_end() {
        // TEXT-GATE: a `null`-stop_reason row with only a tool_use block is
        // mid-flight, NOT a finished turn.
        assert!(!scan_detects_turn_end(&[
            r#"{"type":"assistant","message":{"stop_reason":null,"content":[{"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}}"#,
        ]));
        // Empty / whitespace-only text also fails the gate.
        assert!(!scan_detects_turn_end(&[
            r#"{"type":"assistant","message":{"stop_reason":null,"content":[{"type":"text","text":"   "}]}}"#,
        ]));
    }

    #[test]
    fn scan_turn_end_explicit_end_turn_still_detected() {
        assert!(scan_detects_turn_end(&[
            r#"{"type":"assistant","message":{"stop_reason":"end_turn","content":[{"type":"text","text":"done"}]}}"#,
        ]));
    }

    #[test]
    fn scan_turn_end_non_terminal_reason_is_not_turn_end() {
        assert!(!scan_detects_turn_end(&[
            r#"{"type":"assistant","message":{"stop_reason":"tool_use","content":[{"type":"text","text":"calling a tool"}]}}"#,
        ]));
        assert!(!scan_detects_turn_end(&[
            r#"{"type":"assistant","message":{"stop_reason":"max_tokens","content":[{"type":"text","text":"truncated"}]}}"#,
        ]));
    }

    #[test]
    fn scan_turn_end_user_row_is_never_turn_end() {
        assert!(!scan_detects_turn_end(&[
            r#"{"type":"user","message":{"stop_reason":null,"content":[{"type":"text","text":"hi"}]}}"#,
        ]));
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

    // --- open-ask lifecycle (the sticky-ASK-forever fix) -----------------

    /// Write `rows` to a fresh temp transcript and run `last_ask_user_question`.
    fn ask_for_rows(rows: &[&str]) -> Option<AskUserQuestionData> {
        use std::io::Write;
        let path = std::env::temp_dir().join(format!(
            "ainb-openask-{}-{:p}.jsonl",
            std::process::id(),
            rows
        ));
        {
            let mut f = std::fs::File::create(&path).unwrap();
            for r in rows {
                writeln!(f, "{r}").unwrap();
            }
        }
        let out = last_ask_user_question(&path);
        let _ = std::fs::remove_file(&path);
        out
    }

    #[test]
    fn open_ask_with_no_result_is_returned() {
        let aq = ask_for_rows(&[
            r#"{"type":"user","message":{"content":"go"}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_open","name":"AskUserQuestion","input":{"questions":[{"question":"Ship it?","options":[{"label":"yes"},{"label":"no"}]}]}}]}}"#,
        ])
        .expect("an unanswered ask is an open need");
        assert_eq!(aq.question, "Ship it?");
        assert_eq!(aq.options.len(), 2);
    }

    #[test]
    fn answered_ask_is_not_returned() {
        // The ask's tool_use id is closed by a later user `tool_result` → the ask
        // is no longer open. Before the fix this returned ASK forever.
        let aq = ask_for_rows(&[
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_a","name":"AskUserQuestion","input":{"questions":[{"question":"Scope?","options":[{"label":"a"}]}]}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_a","content":"Your questions have been answered: \"Scope?\"=\"a\"."}]}}"#,
        ]);
        assert!(
            aq.is_none(),
            "an answered ask must fall through, not stick as ASK"
        );
    }

    #[test]
    fn afk_timeout_result_closes_ask() {
        // A 60s AFK timeout also emits the paired tool_result → the ask is closed.
        let aq = ask_for_rows(&[
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_t","name":"AskUserQuestion","input":{"questions":[{"question":"Pick?","options":[{"label":"a"}]}]}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_t","content":"No response after 60s — the user may be away from keyboard."}]}}"#,
        ]);
        assert!(
            aq.is_none(),
            "an AFK-timed-out ask is closed, not an open need"
        );
    }

    #[test]
    fn newest_open_ask_wins_over_older_answered() {
        // An older ask was answered; a newer ask is still open → the open one wins.
        let aq = ask_for_rows(&[
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_old","name":"AskUserQuestion","input":{"questions":[{"question":"First?","options":[{"label":"a"}]}]}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_old","content":"answered"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_new","name":"AskUserQuestion","input":{"questions":[{"question":"Second?","options":[{"label":"b"}]}]}}]}}"#,
        ])
        .expect("the newer, unanswered ask is the live need");
        assert_eq!(aq.question, "Second?");
    }

    #[test]
    fn ask_without_id_is_treated_as_open() {
        // Older transcripts carried no tool_use id; without one we cannot prove
        // closure, so the ask stays open (prior behaviour preserved).
        let aq = ask_for_rows(&[
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"AskUserQuestion","input":{"questions":[{"question":"NoId?","options":[{"label":"a"}]}]}}]}}"#,
        ]);
        assert!(aq.is_some(), "an id-less ask is treated as open");
    }

    // --- cwd canonicalization (/tmp vs /private/tmp transcript matching) --

    #[test]
    fn canonical_cwd_resolves_symlinks_to_the_same_slug() {
        // A symlinked cwd must canonicalize to its real dir so it slugs
        // identically — the macOS /tmp vs /private/tmp mismatch that broke
        // transcript matching.
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real-workdir");
        std::fs::create_dir(&real).unwrap();
        let link = tmp.path().join("link-workdir");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let via_link = canonical_cwd(link.to_str().unwrap());
        let via_real = canonical_cwd(real.to_str().unwrap());
        assert_eq!(
            via_link, via_real,
            "a symlinked cwd canonicalizes to its real dir"
        );
        assert_eq!(
            cwd_to_project_slug(&via_link),
            cwd_to_project_slug(&via_real),
            "canonicalized symlink + real dir yield the same project slug"
        );
    }

    #[test]
    fn canonical_cwd_falls_back_for_nonexistent_path() {
        let missing = "/no/such/dir/ainb-canon-xyz-12345";
        assert_eq!(
            canonical_cwd(missing),
            missing,
            "a non-existent cwd keeps its literal path (graceful fallback)"
        );
    }
}
