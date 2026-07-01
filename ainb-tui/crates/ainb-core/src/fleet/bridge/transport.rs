// ABOUTME: The bridge's transport to ainb sessions — discover, send, capture reply.
//
// Ported from the Python `ainb_phone_bridge.ainb_client` (the verified spec),
// reusing ainb's EXISTING mechanisms where they exist:
//   1. discover()        — `crate::fleet::discover_from_ainb` -> TargetSessions.
//   2. send_keys()       — tmux send-keys (`-l` literal + `--` terminator +
//                          Enter). The `--` terminator is the verified fix that
//                          stops a payload starting with `-` from being parsed
//                          as a tmux flag; the in-tree `fleet::send::tmux_send`
//                          lacks it, so the bridge owns its own send.
//   3. capture_reply()   — read the session's JSONL transcript: snapshot the
//                          byte offset + wall-clock send time, send, then wait
//                          for the next assistant row with stop_reason ==
//                          "end_turn" that post-dates the send.
//
// RESPONSE-CAPTURE CONTRACT (the three verified bug-fixes preserved verbatim):
//   * COMPLETE-LINE-ONLY: only advance the offset past newline-terminated
//     lines, so a reply landing in a not-yet-flushed partial write is re-read
//     once the line completes instead of being skipped.
//   * ROTATION-FOLLOW: re-resolve the latest transcript every poll; on a
//     resume/compaction rotation to a new *.jsonl, switch to it and reset the
//     offset to 0 so the end-of-turn row in the new file is not missed.
//   * SEND-TIME GUARD: only rows whose JSONL `timestamp` strictly post-dates
//     the wall-clock send instant count as the reply. Without it an offset
//     reset on rotation would surface a rolled-up PRE-send end_turn (carried
//     into the new file by resume/compaction) as the answer. A row with no
//     parseable timestamp is rejected once the guard is active.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use tokio::process::Command;

use super::relay::FleetTransport;
use super::routing::TargetSession;
use crate::fleet::discover::discover_from_ainb;
use crate::fleet::read::{cwd_to_project_slug, is_turn_end_stop_reason};

/// The live transport: discovery via `ainb list`, delivery via tmux send-keys,
/// reply capture via the JSONL transcript tail. This is the production
/// [`FleetTransport`] both channels relay through.
#[derive(Debug, Default, Clone, Copy)]
pub struct AinbTransport;

impl FleetTransport for AinbTransport {
    async fn discover(&self) -> Vec<TargetSession> {
        discover().await
    }

    async fn send_and_capture(
        &self,
        session: &TargetSession,
        text: &str,
        timeout: Duration,
    ) -> Option<String> {
        send_and_capture(session, text, timeout).await
    }
}

/// Discover running sessions via the shared fleet discovery (`ainb list`).
///
/// ROUTING-NAME RECOVERY: `ainb list --format json` does NOT expose the session
/// `ainb run --name` as a distinct field — it only ships `workspace_name`,
/// `tmux_session_name`, `worktree_path`, `session_id`. But the run-name IS
/// recoverable: `ainb run --name X` builds the tmux session as
/// `tmux_{sanitize(X)}` (see `tmux::sanitize_session_name`). Stripping the
/// `tmux_` prefix recovers the sanitized run-name, which is what the user
/// addresses via `default_target` / a `name:` prefix. We make that the PRIMARY
/// routing key and keep `workspace_name` as a fallback alias so addressing by
/// repo folder still works. Sanitisation is lossy (spaces/`.`/`/`/`:` collapse
/// to `_`), so a run-name containing those separators is matched in its
/// sanitised form — an acceptable, documented limitation.
pub async fn discover() -> Vec<TargetSession> {
    let sessions = discover_from_ainb().await.unwrap_or_default();
    sessions
        .into_iter()
        .filter_map(|s| {
            let workspace = s.workspace_name.unwrap_or_default();
            let tmux = s.tmux_session.unwrap_or_default();
            if workspace.is_empty() || tmux.is_empty() {
                // A session with no workspace or no tmux name can't be routed to,
                // so it's dropped — but log it so a misconfigured session that
                // silently vanishes from routing is diagnosable instead of being
                // an invisible blind spot.
                tracing::debug!(
                    session_id = %s.id,
                    workspace_empty = workspace.is_empty(),
                    tmux_empty = tmux.is_empty(),
                    "bridge discover: dropping session with empty workspace/tmux (not routable)"
                );
                return None;
            }
            // Recover the run-name from the tmux session name; fall back to the
            // workspace name if the prefix is absent (e.g. a non-ainb tmux name).
            let run_name = run_name_from_tmux(&tmux, &workspace);
            let cwd = s.worktree_path.unwrap_or(s.cwd);
            Some(TargetSession::with_workspace(
                run_name, workspace, tmux, cwd, s.id,
            ))
        })
        .collect()
}

/// Recover the session `ainb run --name` from its `tmux_session` name.
///
/// `ainb run --name X` creates the tmux session as `tmux_{sanitize(X)}`, so
/// stripping the `tmux_` prefix yields the (sanitised) run-name. Falls back to
/// `workspace` when the prefix is absent or the stripped remainder is empty, so
/// a non-ainb tmux name never produces an empty routing key.
#[must_use]
fn run_name_from_tmux(tmux: &str, workspace: &str) -> String {
    match tmux.strip_prefix("tmux_") {
        Some(stripped) if !stripped.is_empty() => stripped.to_string(),
        _ => workspace.to_string(),
    }
}

/// Send `text` to a tmux session via literal send-keys + Enter.
///
/// Uses `-l` (literal) so the payload is never interpreted as key names, and a
/// `--` terminator so a message starting with `-` is treated as literal text,
/// not as a tmux flag. This is the injection-safe transport the Python bridge
/// verified; the Enter is sent as a separate key event afterwards.
pub async fn send_keys(tmux_session: &str, text: &str) -> bool {
    let lit = Command::new("tmux")
        .args(["send-keys", "-t", tmux_session, "-l", "--", text])
        .status()
        .await;
    let lit_ok = matches!(lit, Ok(s) if s.success());
    if !lit_ok {
        return false;
    }
    let enter = Command::new("tmux")
        .args(["send-keys", "-t", tmux_session, "Enter"])
        .status()
        .await;
    matches!(enter, Ok(s) if s.success())
}

/// Newest `*.jsonl` under the cwd's Claude project dir, or `None`.
#[must_use]
pub fn latest_transcript_for_cwd(cwd: &str) -> Option<PathBuf> {
    let mut base = dirs::home_dir()?;
    base.push(".claude");
    base.push("projects");
    base.push(cwd_to_project_slug(cwd));
    let entries = std::fs::read_dir(&base).ok()?;
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

/// Byte length of `path` right now (the send watermark).
#[must_use]
pub fn current_offset(path: Option<&Path>) -> u64 {
    let Some(path) = path else { return 0 };
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// Parse a row's top-level ISO-8601 `timestamp` to epoch milliseconds, or
/// `None`. Claude writes each row with a `timestamp` like
/// `"2025-06-14T12:34:56.789Z"`.
fn row_timestamp_ms(obj: &Value) -> Option<i64> {
    let ts = obj.get("timestamp").and_then(Value::as_str)?;
    if ts.is_empty() {
        return None;
    }
    chrono::DateTime::parse_from_rfc3339(ts).ok().map(|dt| dt.timestamp_millis())
}

/// Return `(stop_reason, text)` for an assistant row, concatenating every
/// `{"type":"text"}` block in `message.content`. Mirrors the Python
/// `_assistant_text_from_row` and the in-tree narrative extraction.
fn assistant_text_from_row(obj: &Value) -> Option<(Option<String>, Option<String>)> {
    if obj.get("type").and_then(Value::as_str) != Some("assistant") {
        return None;
    }
    let message = obj.get("message")?;
    let stop_reason = message.get("stop_reason").and_then(Value::as_str).map(str::to_string);
    let mut parts: Vec<String> = Vec::new();
    match message.get("content") {
        Some(Value::Array(blocks)) => {
            for block in blocks {
                if block.get("type").and_then(Value::as_str) == Some("text") {
                    if let Some(t) = block.get("text").and_then(Value::as_str) {
                        if !t.is_empty() {
                            parts.push(t.to_string());
                        }
                    }
                }
            }
        }
        Some(Value::String(s)) => parts.push(s.clone()),
        _ => {}
    }
    let text = parts.join("\n");
    let text = text.trim();
    Some((stop_reason, (!text.is_empty()).then(|| text.to_string())))
}

/// Scan rows from `start_offset`; return `(new_offset, reply_text|None)`.
///
/// `reply_text` is set only when an assistant row that BOTH carries visible
/// text AND has an end-of-turn `stop_reason` (see [`is_turn_end_stop_reason`])
/// is found in the new region; the LAST such turn wins. When `min_ts_ms` is
/// given, only rows whose JSONL `timestamp` is strictly AFTER it are accepted
/// (the send-time backlog guard). Only COMPLETE (newline-terminated) lines are
/// consumed — a trailing partial write is left for the next poll.
pub fn scan_new_rows_for_turn_end(
    path: &Path,
    start_offset: u64,
    min_ts_ms: Option<i64>,
) -> (u64, Option<String>) {
    use std::io::{Read, Seek, SeekFrom};

    let Ok(mut file) = std::fs::File::open(path) else {
        return (start_offset, None);
    };
    if file.seek(SeekFrom::Start(start_offset)).is_err() {
        return (start_offset, None);
    }
    let mut data = Vec::new();
    if file.read_to_end(&mut data).is_err() {
        return (start_offset, None);
    }

    // Advance only past complete lines: up to and including the last newline.
    let Some(last_nl) = data.iter().rposition(|&b| b == b'\n') else {
        // No complete line yet — nothing to consume, hold the offset.
        return (start_offset, None);
    };
    let complete = &data[..=last_nl];
    let new_offset = start_offset + (last_nl as u64) + 1;

    let mut reply: Option<String> = None;
    for raw_line in complete.split(|&b| b == b'\n') {
        let line = String::from_utf8_lossy(raw_line);
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(obj) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some((stop_reason, text)) = assistant_text_from_row(&obj) else {
            continue;
        };
        if is_turn_end_stop_reason(stop_reason.as_deref()) {
            if let Some(text) = text {
                if let Some(min_ts) = min_ts_ms {
                    match row_timestamp_ms(&obj) {
                        // `>=`, NOT `>`: the JSONL `timestamp` is millisecond
                        // resolution, and the send physically PRECEDES the row
                        // write. A reply whose timestamp truncates to the SAME
                        // millisecond as the send instant is therefore the reply,
                        // not backlog — a strict `>` dropped fast PONGs that
                        // answered within the send millisecond ("no reply within
                        // Ns" despite a live reply). The offset + visible-text
                        // gates already exclude the pre-send rolled-up row in the
                        // common case, so equality here is safe.
                        Some(row_ts) if row_ts >= min_ts => {}
                        // Backlog (or unprovable) row — predates the send, skip.
                        _ => continue,
                    }
                }
                reply = Some(text);
            }
        }
    }
    (new_offset, reply)
}

/// Poll the transcript for the next end-of-turn assistant reply.
///
/// `transcript` may be `None` at send time (no transcript yet); it is
/// re-resolved on each poll so a freshly-created file is picked up. On a
/// rotation to a newer transcript the offset resets to 0. `send_time_ms` is the
/// backlog guard. Returns the reply text or `None` on timeout.
pub async fn wait_for_reply(
    cwd: &str,
    start_offset: u64,
    transcript: Option<PathBuf>,
    timeout: Duration,
    poll_interval: Duration,
    send_time_ms: Option<i64>,
) -> Option<String> {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut offset = start_offset;
    let mut path = transcript;

    while tokio::time::Instant::now() < deadline {
        let cwd = cwd.to_string();
        let latest = tokio::task::spawn_blocking(move || latest_transcript_for_cwd(&cwd))
            .await
            .ok()
            .flatten();
        if let Some(latest) = latest {
            if path.as_deref() != Some(latest.as_path()) {
                // First resolution of a None send-time path, or a rotation to a
                // newer transcript — read the new file from the top.
                path = Some(latest);
                offset = 0;
            }
        }
        if let Some(ref p) = path {
            let p = p.clone();
            let scan = tokio::task::spawn_blocking(move || {
                scan_new_rows_for_turn_end(&p, offset, send_time_ms)
            })
            .await;
            if let Ok((new_offset, reply)) = scan {
                offset = new_offset;
                if reply.is_some() {
                    return reply;
                }
            }
        }
        tokio::time::sleep(poll_interval).await;
    }
    None
}

/// Send `text` to `session` and capture its next end-of-turn reply.
///
/// Returns `None` if the send failed or no reply arrived before `timeout`.
pub async fn send_and_capture(
    session: &TargetSession,
    text: &str,
    timeout: Duration,
) -> Option<String> {
    let cwd = session.cwd.clone();
    let transcript = tokio::task::spawn_blocking(move || latest_transcript_for_cwd(&cwd))
        .await
        .ok()
        .flatten();
    let offset = current_offset(transcript.as_deref());
    // Wall-clock watermark: the reply must be a turn that ends AFTER this instant.
    let send_time_ms = chrono::Utc::now().timestamp_millis();
    if !send_keys(&session.tmux_session, text).await {
        return None;
    }
    wait_for_reply(
        &session.cwd,
        offset,
        transcript,
        timeout,
        Duration::from_millis(500),
        Some(send_time_ms),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp_jsonl(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("ainb-bridge-{}-{}.jsonl", name, std::process::id()))
    }

    #[test]
    fn captures_last_end_turn_text() {
        let path = tmp_jsonl("end-turn");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"{{"type":"assistant","timestamp":"2025-06-14T12:00:01Z","message":{{"stop_reason":"tool_use","content":[{{"type":"text","text":"thinking"}}]}}}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"type":"assistant","timestamp":"2025-06-14T12:00:02Z","message":{{"stop_reason":"end_turn","content":[{{"type":"text","text":"done!"}}]}}}}"#
        )
        .unwrap();
        drop(f);
        let (_, reply) = scan_new_rows_for_turn_end(&path, 0, None);
        let _ = std::fs::remove_file(&path);
        assert_eq!(reply.as_deref(), Some("done!"));
    }

    #[test]
    fn holds_offset_on_partial_trailing_line() {
        let path = tmp_jsonl("partial");
        let mut f = std::fs::File::create(&path).unwrap();
        // A complete line, then a partial (no trailing newline) one.
        write!(
            f,
            "{}\n{}",
            r#"{"type":"user","message":{"content":"go"}}"#,
            r#"{"type":"assistant","message":{"stop_reason":"end_t"#
        )
        .unwrap();
        drop(f);
        let total = std::fs::metadata(&path).unwrap().len();
        let (new_offset, reply) = scan_new_rows_for_turn_end(&path, 0, None);
        let _ = std::fs::remove_file(&path);
        // Offset advanced only past the complete first line; partial held back.
        assert!(
            new_offset < total,
            "offset {new_offset} should be < total {total}"
        );
        assert!(reply.is_none());
    }

    #[test]
    fn send_time_guard_rejects_backlog_rows() {
        let path = tmp_jsonl("backlog");
        let mut f = std::fs::File::create(&path).unwrap();
        // A pre-send end_turn (e.g. rolled in by compaction) — must be ignored.
        writeln!(
            f,
            r#"{{"type":"assistant","timestamp":"2025-06-14T11:59:00Z","message":{{"stop_reason":"end_turn","content":[{{"type":"text","text":"stale answer"}}]}}}}"#
        )
        .unwrap();
        drop(f);
        // Send happened at 12:00:00Z.
        let send_ms = chrono::DateTime::parse_from_rfc3339("2025-06-14T12:00:00Z")
            .unwrap()
            .timestamp_millis();
        let (_, reply) = scan_new_rows_for_turn_end(&path, 0, Some(send_ms));
        let _ = std::fs::remove_file(&path);
        assert!(reply.is_none(), "pre-send backlog must be rejected");
    }

    #[test]
    fn send_time_guard_accepts_post_send_rows() {
        let path = tmp_jsonl("postsend");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"{{"type":"assistant","timestamp":"2025-06-14T12:00:05Z","message":{{"stop_reason":"end_turn","content":[{{"type":"text","text":"fresh answer"}}]}}}}"#
        )
        .unwrap();
        drop(f);
        let send_ms = chrono::DateTime::parse_from_rfc3339("2025-06-14T12:00:00Z")
            .unwrap()
            .timestamp_millis();
        let (_, reply) = scan_new_rows_for_turn_end(&path, 0, Some(send_ms));
        let _ = std::fs::remove_file(&path);
        assert_eq!(reply.as_deref(), Some("fresh answer"));
    }

    #[test]
    fn send_time_guard_accepts_same_millisecond_row() {
        // BOUNDARY (live bug): a fast reply whose JSONL timestamp truncates to the
        // SAME millisecond as the send instant must be ACCEPTED. The send
        // physically precedes the row write, so equality is the reply, not
        // backlog. A strict `>` rejected it and surfaced "no reply within Ns"
        // despite a live PONG.
        let path = tmp_jsonl("same-ms");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"{{"type":"assistant","timestamp":"2026-06-18T22:01:52.500Z","message":{{"stop_reason":"end_turn","content":[{{"type":"text","text":"PONG"}}]}}}}"#
        )
        .unwrap();
        drop(f);
        // Send watermark == the reply row's millisecond exactly.
        let send_ms = chrono::DateTime::parse_from_rfc3339("2026-06-18T22:01:52.500Z")
            .unwrap()
            .timestamp_millis();
        let (_, reply) = scan_new_rows_for_turn_end(&path, 0, Some(send_ms));
        let _ = std::fs::remove_file(&path);
        assert_eq!(
            reply.as_deref(),
            Some("PONG"),
            "a reply written in the same millisecond as the send must be accepted"
        );
    }

    #[test]
    fn guard_rejects_rows_without_timestamp() {
        let path = tmp_jsonl("nots");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"{{"type":"assistant","message":{{"stop_reason":"end_turn","content":[{{"type":"text","text":"no ts"}}]}}}}"#
        )
        .unwrap();
        drop(f);
        let (_, reply) = scan_new_rows_for_turn_end(&path, 0, Some(0));
        let _ = std::fs::remove_file(&path);
        assert!(
            reply.is_none(),
            "unprovable (no-timestamp) row must be rejected under guard"
        );
    }

    #[test]
    fn captures_null_stop_reason_text_reply() {
        // REGRESSION (PR #293): real Claude >= 2.1.x transcripts stamp the
        // visible-text assistant row with `stop_reason: null`, not "end_turn".
        // Rows below are trimmed verbatim from the live `bridgetest` transcript
        // that replied PONG yet produced "no reply within 120s": a thinking row
        // and the PONG text row, BOTH `stop_reason:null`, dated AFTER the send.
        // Before the fix the scan required "end_turn" and returned None here.
        let path = tmp_jsonl("null-stopreason");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"{{"type":"assistant","timestamp":"2026-06-18T22:01:58.109Z","message":{{"model":"claude-sonnet-4-6","stop_reason":null,"content":[{{"type":"thinking","thinking":"Discord ping; respond."}}]}}}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"type":"assistant","timestamp":"2026-06-18T22:01:58.121Z","message":{{"model":"claude-sonnet-4-6","stop_reason":null,"content":[{{"type":"text","text":"PONG"}}]}}}}"#
        )
        .unwrap();
        drop(f);
        // Send watermark just before the reply rows.
        let send_ms = chrono::DateTime::parse_from_rfc3339("2026-06-18T22:01:52Z")
            .unwrap()
            .timestamp_millis();
        let (_, reply) = scan_new_rows_for_turn_end(&path, 0, Some(send_ms));
        let _ = std::fs::remove_file(&path);
        assert_eq!(
            reply.as_deref(),
            Some("PONG"),
            "a null-stop_reason text reply (real transcript shape) must be captured"
        );
    }

    #[test]
    fn tool_use_row_is_not_a_reply() {
        // A mid-turn `tool_use` row is NOT end-of-turn: the turn is still in
        // flight. It must never be surfaced as the reply even though it may
        // carry a leading text block.
        let path = tmp_jsonl("tooluse-midturn");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"{{"type":"assistant","timestamp":"2026-06-18T22:01:58.000Z","message":{{"stop_reason":"tool_use","content":[{{"type":"text","text":"let me check"}},{{"type":"tool_use","name":"Bash","input":{{"command":"ls"}}}}]}}}}"#
        )
        .unwrap();
        drop(f);
        let (_, reply) = scan_new_rows_for_turn_end(&path, 0, None);
        let _ = std::fs::remove_file(&path);
        assert!(
            reply.is_none(),
            "a tool_use (mid-turn) row must not be a reply"
        );
    }

    #[test]
    fn concatenates_multiple_text_blocks() {
        let path = tmp_jsonl("multiblock");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"{{"type":"assistant","message":{{"stop_reason":"end_turn","content":[{{"type":"text","text":"line one"}},{{"type":"tool_use","name":"X"}},{{"type":"text","text":"line two"}}]}}}}"#
        )
        .unwrap();
        drop(f);
        let (_, reply) = scan_new_rows_for_turn_end(&path, 0, None);
        let _ = std::fs::remove_file(&path);
        assert_eq!(reply.as_deref(), Some("line one\nline two"));
    }

    #[test]
    fn current_offset_of_none_is_zero() {
        assert_eq!(current_offset(None), 0);
    }

    #[test]
    fn run_name_recovered_from_tmux_prefix() {
        // `ainb run --name bridgetest` -> tmux `tmux_bridgetest`.
        assert_eq!(
            run_name_from_tmux("tmux_bridgetest", "agents-in-a-box"),
            "bridgetest"
        );
    }

    #[test]
    fn run_name_falls_back_to_workspace_without_prefix() {
        assert_eq!(
            run_name_from_tmux("weird-name", "agents-in-a-box"),
            "agents-in-a-box"
        );
    }

    #[test]
    fn run_name_falls_back_to_workspace_when_prefix_only() {
        // A bare `tmux_` with nothing after it must not become an empty key.
        assert_eq!(
            run_name_from_tmux("tmux_", "agents-in-a-box"),
            "agents-in-a-box"
        );
    }
}
