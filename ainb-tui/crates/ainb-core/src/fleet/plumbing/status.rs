// ABOUTME: Per-session lifecycle status files — `~/.agents-in-a-box/status/<id>.json`.
//
// Every managed session writes one status file, rewritten atomically on each
// lifecycle hook event. ATC (and any other consumer) reads them to learn a
// session's coarse state without scanning tmux panes or transcripts. The shape
// is deliberately tiny and stable:
//
//   { "status": "done", "session_id": "...", "event": "Stop",
//     "ts": 1700000000000, "done_summary": "..." }
//
// `status` is the derived lifecycle state; `event` is the raw hook event that
// produced it (kept for debugging / provenance). The mapping from hook event to
// status is the single source of truth in [`SessionStatus::for_event`], shared
// by the Rust side and documented for the shell hook.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::atomic::write_atomic_json;
use super::paths;

/// Coarse lifecycle state of a managed session, derived from its latest hook
/// event. Ordered roughly along a session's life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionStatus {
    /// Session started or finished a turn and is awaiting the next input.
    /// (`SessionStart`, `Stop`, `Notification`.)
    Waiting,
    /// Session is actively working a turn. (`UserPromptSubmit`.)
    Running,
    /// Session produced a completion this turn (a `Stop` carrying a summary).
    /// Distinct from `Waiting` so a consumer can tell "finished with output"
    /// from "idle, nothing new".
    Done,
    /// Session has ended. (`SessionEnd`.)
    Dead,
}

impl SessionStatus {
    /// Map a raw Claude/Codex hook event name to the lifecycle status it
    /// implies. Unknown events default to `Waiting` (the safe, non-actionable
    /// state). Matcher suffixes (e.g. `Notification:idle_prompt`) are tolerated
    /// by matching on the prefix.
    #[must_use]
    pub fn for_event(raw_event: &str) -> Self {
        let base = raw_event.split(':').next().unwrap_or(raw_event);
        match base {
            "UserPromptSubmit" => Self::Running,
            "SessionEnd" => Self::Dead,
            // SessionStart / Stop / Notification all leave the session waiting
            // for the next input. A Stop that carries a done_summary is upgraded
            // to `Done` by the writer (see `StatusFile::from_event`).
            _ => Self::Waiting,
        }
    }

    /// Lowercase wire string (matches the serde representation).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Running => "running",
            Self::Done => "done",
            Self::Dead => "dead",
        }
    }
}

/// The on-disk status record for one session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusFile {
    /// Derived lifecycle status.
    pub status: SessionStatus,
    /// The session this record describes.
    pub session_id: String,
    /// The raw hook event that produced this record.
    pub event: String,
    /// Epoch milliseconds at which the event fired.
    pub ts: i64,
    /// A one-line completion summary, present when a turn finished with output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done_summary: Option<String>,
}

impl StatusFile {
    /// Build a status record from a hook event. A `Stop` carrying a non-empty
    /// summary is recorded as `Done` (finished with output) rather than the
    /// default `Waiting`.
    #[must_use]
    pub fn from_event(
        session_id: impl Into<String>,
        raw_event: &str,
        ts: i64,
        done_summary: Option<String>,
    ) -> Self {
        let session_id = session_id.into();
        let base = raw_event.split(':').next().unwrap_or(raw_event);
        let summary = done_summary.filter(|s| !s.trim().is_empty());
        let status = if base == "Stop" && summary.is_some() {
            SessionStatus::Done
        } else {
            SessionStatus::for_event(raw_event)
        };
        Self {
            status,
            session_id,
            event: raw_event.to_string(),
            ts,
            done_summary: summary,
        }
    }
}

/// Path to the status file for `session_id` under the default home.
pub fn status_path(session_id: &str) -> Result<PathBuf> {
    Ok(paths::status_dir()?.join(format!("{}.json", sanitize(session_id))))
}

/// Path to the status file for `session_id` under an explicit ainb home.
#[must_use]
pub fn status_path_in(home: &Path, session_id: &str) -> PathBuf {
    paths::status_dir_in(home).join(format!("{}.json", sanitize(session_id)))
}

/// Atomically write `record` to `session_id`'s status file under `home`.
pub fn write_status_in(home: &Path, record: &StatusFile) -> Result<()> {
    let path = status_path_in(home, &record.session_id);
    write_atomic_json(&path, record)
}

/// Read the status file for `session_id` under `home`, if present + parseable.
#[must_use]
pub fn read_status_in(home: &Path, session_id: &str) -> Option<StatusFile> {
    let path = status_path_in(home, session_id);
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Neutralise a session id for use as a filename: keep alphanumerics, `-`, `_`;
/// everything else becomes `_`. Session ids are UUIDs/peer-ids in practice so
/// this is normally a no-op, but it hard-guards against a path-traversal id.
fn sanitize(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn event_to_status_mapping_is_exhaustive() {
        assert_eq!(
            SessionStatus::for_event("SessionStart"),
            SessionStatus::Waiting
        );
        assert_eq!(
            SessionStatus::for_event("UserPromptSubmit"),
            SessionStatus::Running
        );
        assert_eq!(SessionStatus::for_event("Stop"), SessionStatus::Waiting);
        assert_eq!(
            SessionStatus::for_event("Notification"),
            SessionStatus::Waiting
        );
        assert_eq!(SessionStatus::for_event("SessionEnd"), SessionStatus::Dead);
        // Matcher suffix tolerated.
        assert_eq!(
            SessionStatus::for_event("Notification:idle_prompt"),
            SessionStatus::Waiting
        );
        // Unknown → Waiting (safe default).
        assert_eq!(SessionStatus::for_event("Weird"), SessionStatus::Waiting);
    }

    #[test]
    fn stop_with_summary_records_done() {
        let s = StatusFile::from_event("sid", "Stop", 100, Some("finished the refactor".into()));
        assert_eq!(s.status, SessionStatus::Done);
        assert_eq!(s.done_summary.as_deref(), Some("finished the refactor"));
    }

    #[test]
    fn stop_without_summary_records_waiting() {
        let s = StatusFile::from_event("sid", "Stop", 100, None);
        assert_eq!(s.status, SessionStatus::Waiting);
        assert!(s.done_summary.is_none());
        // Blank summary is treated as absent.
        let s2 = StatusFile::from_event("sid", "Stop", 100, Some("   ".into()));
        assert_eq!(s2.status, SessionStatus::Waiting);
        assert!(s2.done_summary.is_none());
    }

    #[test]
    fn write_then_read_round_trips() {
        let home = TempDir::new().unwrap();
        let rec = StatusFile::from_event("abc-123", "UserPromptSubmit", 42, None);
        write_status_in(home.path(), &rec).unwrap();
        let back = read_status_in(home.path(), "abc-123").unwrap();
        assert_eq!(back, rec);
        assert_eq!(back.status, SessionStatus::Running);
    }

    #[test]
    fn each_event_overwrites_the_same_file() {
        let home = TempDir::new().unwrap();
        write_status_in(
            home.path(),
            &StatusFile::from_event("s", "SessionStart", 1, None),
        )
        .unwrap();
        write_status_in(
            home.path(),
            &StatusFile::from_event("s", "UserPromptSubmit", 2, None),
        )
        .unwrap();
        let back = read_status_in(home.path(), "s").unwrap();
        assert_eq!(back.status, SessionStatus::Running);
        assert_eq!(back.ts, 2);
    }

    #[test]
    fn sanitize_blocks_path_traversal_in_filename() {
        let home = TempDir::new().unwrap();
        let p = status_path_in(home.path(), "../../etc/passwd");
        // The id is flattened; the file stays inside the status dir.
        assert!(p.starts_with(paths::status_dir_in(home.path())));
        assert!(!p.to_string_lossy().contains(".."));
    }

    #[test]
    fn missing_status_reads_none() {
        let home = TempDir::new().unwrap();
        assert!(read_status_in(home.path(), "nope").is_none());
    }
}
