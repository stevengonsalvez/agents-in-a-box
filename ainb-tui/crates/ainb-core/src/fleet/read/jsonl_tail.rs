// ABOUTME: JSONL transcript tail for assistant-turn-end detection.
//
// Watches ~/.claude/projects/<cwd-slug>/<session-id>.jsonl using `notify`.
// Returns when the next assistant turn ends or the timeout fires.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use notify::{EventKind, RecursiveMode, Watcher};
use serde::Deserialize;

/// Claude maps cwd → project dir by replacing `/` with `-`.
#[must_use]
pub fn cwd_to_project_slug(cwd: &str) -> String {
    cwd.replace('/', "-")
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
            let ended = row
                .message
                .as_ref()
                .and_then(|m| m.stop_reason.as_deref())
                == Some("end_turn");
            if is_assistant && ended {
                found_end = true;
                break;
            }
        }
    }

    *last_offset += bytes_consumed;
    Ok(if found_end { Some(true) } else { None })
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
}
