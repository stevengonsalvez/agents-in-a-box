//! Provider `stream-json` transcript classifier: one line taxonomy for every
//! executor, durable and live.
//!
//! A provider run tees its CLI stream-json to disk (`{logs}/claude.jsonl` /
//! `{logs}/codex.jsonl`, see the daemon runner). This module turns those lines
//! into `(MessageKind, body)` pairs in the 5-colour transcript taxonomy
//! ([`MessageKind`]): tool CALLS (name + a compact input), their RESULTS (with
//! a per-tool duration when the log carries timestamps), assistant prose,
//! extended thinking, and a closing LAST REPLY + run-status line.
//!
//! It lives in the proto crate so the daemon (the live `TaskMessage` producer
//! and the durable timeline read) and the plugin (the on-disk timeline render)
//! share exactly one classifier: a live-appended line and its later re-read
//! twin are byte-identical.
//!
//! # Robust to a truncated / mid-write file (the aws lesson: files lag)
//!
//! Classification is line-oriented and **never fails**: a line that is not
//! valid JSON (the common case for the LAST line of a file the daemon is still
//! appending to) is skipped, not fatal. A recognised line with missing fields
//! degrades to what it can show. A shape the classifier does not know (a future
//! line type, a different provider) is skipped rather than mis-rendered. So a
//! partial file classifies its complete prefix and simply stops: never a panic,
//! never a half-decoded line.
//!
//! # Provider shapes
//!
//! Handles the `claude -p --output-format stream-json` line taxonomy
//! (`system` / `assistant` / `user`(tool_result) / `result`) fully, and the codex
//! `exec --json` `{"msg":{"type":…}}` envelope on a best-effort basis (an
//! unrecognised codex line is skipped, never mis-attributed).

use std::collections::HashMap;

use serde_json::Value;

use crate::events::MessageKind;

/// Clip a one-line summary / snippet to this many display chars (char-safe).
const SUMMARY_MAX: usize = 84;

/// Classify a whole provider `stream-json` transcript into `(kind, body)` lines
/// in stream order.
///
/// Line-oriented and total: unparseable / unknown lines are skipped (a
/// mid-write tail never breaks the result), so the output is the transcript's
/// complete decoded prefix. See the module docs for the shape taxonomy +
/// robustness contract. A live producer that sees one line at a time keeps a
/// [`StreamJsonClassifier`] instead, so tool results still resolve their tool
/// name and duration across lines.
#[must_use]
pub fn classify_stream_json(jsonl: &str) -> Vec<(MessageKind, String)> {
    let mut classifier = StreamJsonClassifier::default();
    let mut out = Vec::new();
    for line in jsonl.lines() {
        out.extend(classifier.classify_line(line));
    }
    out
}

/// The running classification state for one transcript.
///
/// Stateful across lines on purpose: a `tool_result` names its tool and carries
/// a per-tool duration only because the earlier `tool_use` line's name and
/// timestamp were remembered. Feed every line of one run through the same
/// instance, in order.
#[derive(Debug, Default)]
pub struct StreamJsonClassifier {
    /// Lines produced by the current [`Self::classify_line`] call.
    out: Vec<(MessageKind, String)>,
    /// `tool_use_id` → the tool name, so a later tool_result can name its tool.
    tool_names: HashMap<String, String>,
    /// `tool_use_id` → the tool_use line's timestamp (epoch ms), so a tool_result
    /// carrying its own timestamp yields a per-tool duration.
    tool_starts: HashMap<String, i64>,
}

impl StreamJsonClassifier {
    /// Classify one raw transcript line into zero or more `(kind, body)` lines.
    ///
    /// Blank, non-JSON and unknown-shape lines yield nothing; a multi-line text
    /// block yields one entry per non-empty line so a renderer painting one
    /// entry per row never overflows.
    #[must_use]
    pub fn classify_line(&mut self, line: &str) -> Vec<(MessageKind, String)> {
        let line = line.trim();
        if line.is_empty() {
            return Vec::new();
        }
        // A line that is not valid JSON (a truncated tail the daemon is still
        // appending to, or a stray log line) is skipped, never fatal.
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            return Vec::new();
        };
        self.fold(&v);
        std::mem::take(&mut self.out)
    }

    /// Fold one decoded JSONL line into the output.
    fn fold(&mut self, v: &Value) {
        // Codex envelope: `{"msg":{"type":…}}`, unwrap to the inner event.
        if let Some(msg) = v.get("msg").filter(|m| m.is_object()) {
            self.fold_codex_msg(msg);
            return;
        }
        match v.get("type").and_then(Value::as_str).unwrap_or("") {
            "system" => self.fold_system(v),
            "assistant" => self.fold_message(v, ts_of(v)),
            "user" => self.fold_message(v, ts_of(v)),
            "result" => self.fold_result(v),
            // An explicit error line (some providers emit one) → the error lane.
            "error" => {
                if let Some(text) = v.get("error").and_then(value_text) {
                    push_lines(&mut self.out, MessageKind::Error, &text);
                }
            }
            _ => {}
        }
    }

    /// A `system` line marks a run boundary: surface it as a slate status line
    /// so the session id / subtype reads without inventing a taxonomy lane.
    fn fold_system(&mut self, v: &Value) {
        let subtype = v.get("subtype").and_then(Value::as_str).unwrap_or("system");
        let session = v.get("session_id").and_then(Value::as_str).map(short_id).unwrap_or_default();
        let body = if session.is_empty() {
            format!("· {subtype}")
        } else {
            format!("· {subtype} · session {session}")
        };
        self.out.push((MessageKind::ToolResult, body));
    }

    /// An `assistant` / `user` line carries a `message.content` block array. Each
    /// block maps to a lane: text → agent prose, thinking → the thinking lane,
    /// tool_use → a tool call (name + compact input), tool_result → a tool result
    /// (named + a per-tool duration when both timestamps are known).
    fn fold_message(&mut self, v: &Value, line_ts: Option<i64>) {
        let content = v.get("message").and_then(|m| m.get("content")).and_then(Value::as_array);
        let Some(blocks) = content else {
            return;
        };
        for block in blocks {
            match block.get("type").and_then(Value::as_str).unwrap_or("") {
                "text" => {
                    if let Some(t) = block.get("text").and_then(Value::as_str) {
                        push_lines(&mut self.out, MessageKind::Agent, t);
                    }
                }
                "thinking" => {
                    if let Some(t) = block.get("thinking").and_then(Value::as_str) {
                        push_lines(&mut self.out, MessageKind::Thinking, t);
                    }
                }
                "tool_use" => self.fold_tool_use(block, line_ts),
                "tool_result" => self.fold_tool_result(block, line_ts),
                _ => {}
            }
        }
    }

    /// A `tool_use` block: emit a blue tool-call line (`<name>  <compact input>`)
    /// and remember the tool's name + start timestamp for its result.
    fn fold_tool_use(&mut self, block: &Value, line_ts: Option<i64>) {
        let name = block.get("name").and_then(Value::as_str).unwrap_or("tool");
        let summary = compact_input(block.get("input"));
        let body = if summary.is_empty() {
            name.to_string()
        } else {
            format!("{name}  {summary}")
        };
        self.out.push((MessageKind::ToolCall, body));
        if let Some(id) = block.get("id").and_then(Value::as_str) {
            self.tool_names.insert(id.to_string(), name.to_string());
            if let Some(ts) = line_ts {
                self.tool_starts.insert(id.to_string(), ts);
            }
        }
    }

    /// A `tool_result` block: emit a slate result line naming its tool (resolved
    /// from the matching tool_use) and, when both carry a timestamp, its duration.
    /// An `is_error` result renders in the red error lane instead.
    fn fold_tool_result(&mut self, block: &Value, line_ts: Option<i64>) {
        let id = block.get("tool_use_id").and_then(Value::as_str).unwrap_or("");
        let name = self.tool_names.get(id).map_or("tool", String::as_str);
        let snippet = block
            .get("content")
            .and_then(value_text)
            .map(|s| truncate_chars(&one_line(&s), SUMMARY_MAX))
            .unwrap_or_default();
        let dur = self.tool_starts.get(id).zip(line_ts).map(|(start, end)| fmt_dur(end - start));
        let mut body = match (snippet.is_empty(), &dur) {
            (true, Some(d)) => format!("{name}  ({d})"),
            (true, None) => name.to_string(),
            (false, Some(d)) => format!("{name}  {snippet}  ({d})"),
            (false, None) => format!("{name}  {snippet}"),
        };
        let is_error = block.get("is_error").and_then(Value::as_bool).unwrap_or(false);
        let lane = if is_error {
            body = format!("{body}  [error]");
            MessageKind::Error
        } else {
            MessageKind::ToolResult
        };
        self.out.push((lane, body));
    }

    /// The terminal `result` line: the LAST REPLY block (the final assistant text)
    /// plus a slate run-status transition (subtype · cost · wall-clock duration).
    fn fold_result(&mut self, v: &Value) {
        let subtype = v.get("subtype").and_then(Value::as_str).unwrap_or("result");
        let mut status = format!("· {subtype}");
        if let Some(cost) = v.get("total_cost_usd").and_then(Value::as_f64) {
            if cost > 0.0 {
                status.push_str(&format!(" · ${cost:.4}"));
            }
        }
        if let Some(dur) = v.get("duration_ms").and_then(Value::as_i64) {
            status.push_str(&format!(" · {}", fmt_dur(dur)));
        }
        let lane = if subtype.contains("error") {
            MessageKind::Error
        } else {
            MessageKind::ToolResult
        };
        self.out.push((lane, status));
        if let Some(reply) = v.get("result").and_then(value_text) {
            if !reply.trim().is_empty() {
                self.out.push((MessageKind::Agent, "LAST REPLY".to_string()));
                push_lines(&mut self.out, MessageKind::Agent, &reply);
            }
        }
    }

    /// Best-effort codex `{"msg":{…}}` handling: an `agent_message` is prose, an
    /// `error` is the error lane; other codex event types are skipped (never
    /// mis-rendered as another provider's shape).
    fn fold_codex_msg(&mut self, msg: &Value) {
        match msg.get("type").and_then(Value::as_str).unwrap_or("") {
            "agent_message" | "agent_message_delta" => {
                if let Some(t) = msg.get("message").and_then(Value::as_str) {
                    push_lines(&mut self.out, MessageKind::Agent, t);
                }
            }
            "error" => {
                if let Some(t) = msg.get("message").and_then(value_text) {
                    push_lines(&mut self.out, MessageKind::Error, &t);
                }
            }
            _ => {}
        }
    }
}

/// The line's timestamp as epoch ms, from a numeric `timestamp` / `ts` field.
/// Only a numeric millisecond value is honoured: a per-tool duration is a
/// best-effort enrichment, absent (no duration shown) when the log carries no
/// machine timestamp rather than guessed.
fn ts_of(v: &Value) -> Option<i64> {
    v.get("timestamp").or_else(|| v.get("ts")).and_then(Value::as_i64)
}

/// Split `text` into non-empty trimmed lines and push one entry per line in
/// `kind`'s lane, so a multi-line block never overflows a single render row (the
/// renderer paints one entry per row). Empty input pushes nothing.
fn push_lines(out: &mut Vec<(MessageKind, String)>, kind: MessageKind, text: &str) {
    for line in text.lines() {
        let line = line.trim_end();
        if line.trim().is_empty() {
            continue;
        }
        out.push((kind, line.to_string()));
    }
}

/// A compact one-line summary of a tool_use `input` object: the most telling
/// field for the common tools (a shell command, a file path, a query / pattern),
/// else a truncated flat rendering, so a tool call reads at a glance without the
/// full JSON payload.
fn compact_input(input: Option<&Value>) -> String {
    let Some(obj) = input.and_then(Value::as_object) else {
        return input.map(compact_value).unwrap_or_default();
    };
    for key in [
        "command",
        "file_path",
        "path",
        "pattern",
        "query",
        "url",
        "prompt",
    ] {
        if let Some(s) = obj.get(key).and_then(value_text) {
            return truncate_chars(&one_line(&s), SUMMARY_MAX);
        }
    }
    // No telling field: a flat, truncated key=val rendering.
    let flat = obj
        .iter()
        .map(|(k, val)| format!("{k}={}", one_line(&compact_value(val))))
        .collect::<Vec<_>>()
        .join(" ");
    truncate_chars(&flat, SUMMARY_MAX)
}

/// Read a JSON value as display text: a string as-is, else its compact JSON.
/// A tool_result `content` is often an array of `{type:"text", text:…}` blocks;
/// join their text. A plain string content is returned verbatim.
fn value_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Array(items) => {
            let joined = items
                .iter()
                .filter_map(|it| {
                    it.get("text")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .or_else(|| it.as_str().map(str::to_string))
                })
                .collect::<Vec<_>>()
                .join(" ");
            (!joined.is_empty()).then_some(joined)
        }
        Value::Null => None,
        other => Some(other.to_string()),
    }
}

/// A short flat rendering of any JSON value for a compact input summary.
fn compact_value(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Collapse all whitespace runs (incl. newlines) to single spaces, trimmed;
/// keeps a summary on one render row.
fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Format a duration in ms as a compact `Nms` / `N.Ns` / `NmNs` string. A
/// negative delta (clock skew between two lines) reads `0ms` rather than a
/// nonsense value.
fn fmt_dur(ms: i64) -> String {
    let ms = ms.max(0);
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format!("{}m{}s", ms / 60_000, (ms % 60_000) / 1000)
    }
}

/// Truncate to `max` display chars with a trailing ellipsis on overflow
/// (char-safe: never byte-slices, the utf8-truncate trap).
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let prefix: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{prefix}…")
    }
}

/// The last 8 chars of a session id (char-safe), or the whole id when short.
fn short_id(id: &str) -> String {
    let n = id.chars().count();
    if n <= 8 {
        id.to_string()
    } else {
        id.chars().skip(n - 8).collect()
    }
}
