//! Ingest of the durable event log (`events.jsonl`) into the SQLite
//! `events` table.
//!
//! The lifecycle hook (`ainb fleet atc hook`) appends one canonical
//! JSON line per managed event to `events.jsonl`. This module tails
//! that file from a durable byte offset persisted in `ingest_offset`,
//! folds each COMPLETE newline-terminated line into an [`EventRow`],
//! and advances + persists the offset.
//!
//! Crash-safety: the offset only advances past bytes that have been
//! persisted to the `events` table, and only over a complete line (a
//! partial trailing line — a torn / in-flight append — is never
//! consumed). So a daemon restart re-reads exactly the un-ingested
//! suffix and never double-ingests already-offset bytes. This mirrors
//! the `fallback.replay_into` model, but is offset-driven (the file is
//! never truncated) so the hook can keep appending while the daemon is
//! up.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

use crate::store::{EventRow, Store, StoreError};

/// The canonical event line the hook appends. Mirrors the format in the
/// Wave 2 plan: every field the materializer (Wave 3) consumes.
#[derive(Debug, Clone, Deserialize)]
pub struct EventLine {
    /// Epoch milliseconds — when the hook fired.
    pub ts: i64,
    /// Host agent's session id (universal hook field).
    pub session_id: String,
    /// Working directory at hook-fire time (universal hook field).
    #[serde(default)]
    pub cwd: String,
    /// Path to the session transcript (universal hook field).
    #[serde(default)]
    pub transcript_path: String,
    /// Host agent. Defaults to `claude` when the line omits it.
    #[serde(default = "default_agent")]
    pub agent: String,
    /// Raw hook event name.
    pub event_type: String,
    /// Discriminator parsed from the payload (nullable).
    #[serde(default)]
    pub matcher: Option<String>,
    /// The raw (bounded) hook stdin payload.
    #[serde(default)]
    pub payload: Value,
}

fn default_agent() -> String {
    "claude".to_string()
}

impl EventLine {
    /// Convert into the row shape the store persists. `seq = 0` (SQLite
    /// assigns it on insert). The payload is re-serialized to a string.
    fn into_row(self) -> EventRow {
        EventRow {
            seq: 0,
            ts: self.ts,
            session_id: self.session_id,
            cwd: self.cwd,
            transcript_path: self.transcript_path,
            agent: self.agent,
            event_type: self.event_type,
            matcher: self.matcher,
            payload: self.payload.to_string(),
        }
    }
}

/// Outcome of one ingest pass.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct IngestSummary {
    /// Complete lines ingested into the `events` table this pass.
    pub events_ingested: usize,
    /// Complete lines that failed to parse (skipped, but their bytes are
    /// still consumed so the offset advances past them).
    pub lines_corrupt: usize,
    /// The byte offset after this pass (== the persisted cursor).
    pub offset: u64,
}

/// Read `events.jsonl` from the store's durable offset, ingest every
/// COMPLETE line into the `events` table, and persist the new offset.
/// A partial trailing line (no terminating `\n`) is left for the next
/// pass. Idempotent w.r.t. the offset: bytes at or before the persisted
/// offset are never re-read.
///
/// Best-effort: a missing file is a no-op; a parse failure on one line
/// is counted + skipped (its bytes are still consumed) rather than
/// stalling the cursor forever on a single corrupt line.
pub fn ingest_once(store: &Store, events_jsonl: &Path) -> Result<IngestSummary, StoreError> {
    let start = store.read_ingest_offset()?;
    let mut summary = IngestSummary {
        offset: start,
        ..Default::default()
    };

    let mut file = match std::fs::File::open(events_jsonl) {
        Ok(f) => f,
        // No file yet (no hook has fired) — nothing to ingest.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(summary),
        Err(e) => {
            return Err(StoreError::Sqlite(rusqlite::Error::ToSqlConversionFailure(
                Box::new(e),
            )));
        }
    };

    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    if len <= start {
        // Nothing new (or the file shrank — a manual truncation; the
        // offset will be re-validated on the next grow). Do not rewind.
        return Ok(summary);
    }

    // Seek to the durable cursor and read the un-ingested suffix.
    if file.seek(SeekFrom::Start(start)).is_err() {
        return Ok(summary);
    }
    let mut buf = Vec::new();
    if file.read_to_end(&mut buf).is_err() {
        return Ok(summary);
    }

    // Walk complete lines only. `consumed` tracks bytes past which the
    // offset may advance — it stops at the last `\n`, leaving any
    // partial trailing line (torn / in-flight append) for next pass.
    let mut consumed: usize = 0;
    let mut search_from: usize = 0;
    while let Some(rel_nl) = buf[search_from..].iter().position(|&b| b == b'\n') {
        let nl = search_from + rel_nl;
        let line_bytes = &buf[consumed..nl];
        let line = String::from_utf8_lossy(line_bytes);
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            match serde_json::from_str::<EventLine>(trimmed) {
                Ok(parsed) => {
                    store.append_event(&parsed.into_row())?;
                    summary.events_ingested += 1;
                }
                Err(_) => {
                    // Skip the corrupt line but still consume its bytes
                    // so the cursor doesn't wedge on it forever.
                    summary.lines_corrupt += 1;
                }
            }
        }
        // Advance past this line including the newline.
        consumed = nl + 1;
        search_from = consumed;
    }

    // Persist the offset only over the complete-line prefix.
    let new_offset = start + consumed as u64;
    if new_offset != start {
        store.write_ingest_offset(new_offset)?;
    }
    summary.offset = new_offset;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn line(ts: i64, session: &str, event: &str, matcher: Option<&str>) -> String {
        let m = match matcher {
            Some(s) => format!("\"{s}\""),
            None => "null".to_string(),
        };
        format!(
            r#"{{"ts":{ts},"session_id":"{session}","cwd":"/tmp/p","transcript_path":"/t/{session}.jsonl","agent":"claude","event_type":"{event}","matcher":{m},"payload":{{"x":1}}}}"#
        )
    }

    fn append(path: &Path, s: &str) {
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
        f.write_all(s.as_bytes()).unwrap();
    }

    #[test]
    fn missing_file_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("a.db")).unwrap();
        let s = ingest_once(&store, &dir.path().join("nope.jsonl")).unwrap();
        assert_eq!(s.events_ingested, 0);
        assert_eq!(s.offset, 0);
    }

    #[test]
    fn ingests_complete_lines_and_advances_offset() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("a.db")).unwrap();
        let jsonl = dir.path().join("events.jsonl");
        append(
            &jsonl,
            &format!("{}\n", line(100, "s1", "SessionStart", None)),
        );
        append(
            &jsonl,
            &format!(
                "{}\n",
                line(200, "s1", "PreToolUse", Some("AskUserQuestion"))
            ),
        );

        let s = ingest_once(&store, &jsonl).unwrap();
        assert_eq!(s.events_ingested, 2);
        assert_eq!(s.offset, std::fs::metadata(&jsonl).unwrap().len());

        let rows = store.events_since(0).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].matcher.as_deref(), Some("AskUserQuestion"));
        assert_eq!(rows[1].transcript_path, "/t/s1.jsonl");
        assert_eq!(store.read_ingest_offset().unwrap(), s.offset);
    }

    #[test]
    fn does_not_consume_partial_trailing_line() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("a.db")).unwrap();
        let jsonl = dir.path().join("events.jsonl");
        // One complete line + a partial (no trailing newline yet).
        append(&jsonl, &format!("{}\n", line(100, "s1", "Stop", None)));
        let partial = line(200, "s1", "Stop", None);
        append(&jsonl, &partial[..partial.len() / 2]); // truncated, no \n

        let s = ingest_once(&store, &jsonl).unwrap();
        // Only the complete line is ingested.
        assert_eq!(s.events_ingested, 1);
        assert_eq!(store.events_since(0).unwrap().len(), 1);
        // The offset stops at the first newline, NOT end-of-file.
        let full_len = std::fs::metadata(&jsonl).unwrap().len();
        assert!(s.offset < full_len, "partial line must not be consumed");

        // Now complete the partial line (the rest + a newline) and append
        // a third: the next pass picks up from where we stopped, with no
        // double-ingest of the already-offset complete line.
        append(&jsonl, &partial[partial.len() / 2..]);
        append(&jsonl, "\n");
        append(
            &jsonl,
            &format!("{}\n", line(300, "s1", "SessionEnd", None)),
        );
        let s2 = ingest_once(&store, &jsonl).unwrap();
        assert_eq!(s2.events_ingested, 2, "completed line + new line");
        let rows = store.events_since(0).unwrap();
        assert_eq!(rows.len(), 3, "no duplicates of the first line");
        let types: Vec<_> = rows.iter().map(|r| r.event_type.as_str()).collect();
        assert_eq!(types, vec!["Stop", "Stop", "SessionEnd"]);
    }

    #[test]
    fn catches_up_from_a_nonzero_offset_on_restart() {
        // Simulate a daemon restart: a first store ingests two lines and
        // persists the offset; a SECOND store opened on the SAME db reads
        // that durable offset and ingests only the newly-appended suffix.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("a.db");
        let jsonl = dir.path().join("events.jsonl");
        append(
            &jsonl,
            &format!("{}\n", line(100, "s1", "SessionStart", None)),
        );
        append(&jsonl, &format!("{}\n", line(200, "s1", "Stop", None)));
        {
            let store = Store::open(&db).unwrap();
            let s = ingest_once(&store, &jsonl).unwrap();
            assert_eq!(s.events_ingested, 2);
        }
        // Hook keeps appending while the "daemon" is down.
        append(
            &jsonl,
            &format!("{}\n", line(300, "s1", "SessionEnd", None)),
        );
        // Restart: fresh Store handle, durable offset already non-zero.
        let store = Store::open(&db).unwrap();
        assert!(store.read_ingest_offset().unwrap() > 0);
        let s = ingest_once(&store, &jsonl).unwrap();
        assert_eq!(s.events_ingested, 1, "only the new suffix is ingested");
        assert_eq!(store.events_since(0).unwrap().len(), 3, "no re-ingest");
    }

    #[test]
    fn corrupt_line_is_skipped_but_cursor_advances() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("a.db")).unwrap();
        let jsonl = dir.path().join("events.jsonl");
        append(&jsonl, "this is not json\n");
        append(&jsonl, &format!("{}\n", line(200, "s1", "Stop", None)));
        let s = ingest_once(&store, &jsonl).unwrap();
        assert_eq!(s.lines_corrupt, 1);
        assert_eq!(s.events_ingested, 1);
        // Cursor consumed BOTH lines (the corrupt one doesn't wedge it).
        assert_eq!(s.offset, std::fs::metadata(&jsonl).unwrap().len());
    }
}
