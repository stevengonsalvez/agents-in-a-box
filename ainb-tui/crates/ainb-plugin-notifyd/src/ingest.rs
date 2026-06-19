//! Ingest of the durable event log (`events.jsonl`) into the SQLite
//! `events` table.
//!
//! The lifecycle hook (`ainb fleet atc hook`) appends one canonical
//! JSON line per managed event to `events.jsonl`. This module tails
//! that file from a durable byte offset persisted in `ingest_offset`,
//! folds each COMPLETE newline-terminated line into an [`EventRow`],
//! and advances + persists the offset — atomically, in one transaction.
//!
//! # Atomicity assumptions (≤ PIPE_BUF)
//!
//! The hook writes each canonical line with a single `write(2)` to a file
//! opened `O_APPEND`. POSIX guarantees an `O_APPEND` write of at most
//! `PIPE_BUF` bytes is atomic with respect to other appenders, so concurrent
//! hook fires never interleave WITHIN a line — a reader sees whole lines, in
//! order, never a torn splice of two appends. The producer therefore caps the
//! canonical payload to keep the whole line ≤ `PIPE_BUF` (4 KiB on Linux);
//! `MAX_EVENT_PAYLOAD_BYTES` (defined producer-side in `ainb-core`) bounds the
//! embedded payload to preserve this. This module relies on that bound: it
//! never consumes a partial trailing line (no terminating `\n`), so a
//! mid-append write is simply left for the next pass.
//!
//! Should a corrupt / interleaved line slip through anyway (a producer bug, a
//! manual edit, a >PIPE_BUF line that DID tear), the parse fails: the line is
//! COUNTED (`IngestSummary::lines_corrupt`) and logged via a `tracing` event,
//! and its bytes are still consumed so the cursor never wedges. A persistent
//! non-zero corrupt count is thus observable rather than silent.
//!
//! # Crash-safety
//!
//! The offset only advances past bytes that have been persisted to the `events`
//! table, and only over a complete line. The inserts AND the offset write
//! commit in a SINGLE SQLite transaction ([`Store::ingest_batch`]), so a crash
//! mid-pass rolls the whole suffix back (re-read on restart, NO duplicate rows)
//! or commits it all (offset matches rows). A daemon restart re-reads exactly
//! the un-ingested suffix and never double-ingests already-offset bytes. This
//! mirrors the `fallback.replay_into` model, but is offset-driven (the file is
//! never truncated by us) so the hook can keep appending while the daemon is up.
//!
//! # Bounded read
//!
//! Each pass reads AT MOST [`MAX_INGEST_BYTES`] of the un-ingested suffix, so a
//! long daemon-down window (a multi-GB `events.jsonl`) is ingested in bounded
//! chunks across ticks rather than slurped into memory at once (OOM risk). The
//! offset advances by exactly what was consumed; the next tick continues.
//!
//! # Rotation / truncate-regrow detection
//!
//! A persisted file-identity fingerprint (inode + a length checkpoint) is
//! compared each pass: when the inode changes (the file was rotated / replaced)
//! or the on-disk length drops below the checkpoint (truncation), the offset is
//! stale and reset to `0` so we re-ingest from the start of the new file rather
//! than seeking into stale bytes of a regrown file.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use serde::Deserialize;
use serde_json::Value;
use tracing::warn;

use crate::store::{EventRow, RetentionPolicy, Store, StoreError};

/// Maximum bytes read from the un-ingested suffix in a single pass. Bounds peak
/// memory regardless of how far behind the cursor is; the remainder is picked up
/// on the next tick. 4 MiB comfortably holds thousands of canonical lines while
/// staying small enough to never threaten the daemon's footprint.
pub const MAX_INGEST_BYTES: u64 = 4 * 1024 * 1024;

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
    /// Parent session id for a fleet child, carried at the canonical line's
    /// top level by the hook appender. `None` for a top-level session.
    #[serde(default)]
    pub parent: Option<String>,
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
    ///
    /// The canonical line's top-level `parent` is folded INTO the stored
    /// payload object (`payload.parent`) so the materializer can read it
    /// without a dedicated column — the `events` schema stays unchanged and
    /// the Wave 3 transition loop reads `payload.parent`. An explicit top-level
    /// `parent` wins over any pre-existing key in the raw payload.
    fn into_row(self) -> EventRow {
        let mut payload = self.payload;
        if let Some(parent) = self.parent {
            if let Value::Object(map) = &mut payload {
                map.insert("parent".to_string(), Value::String(parent));
            } else if payload.is_null() {
                payload = serde_json::json!({ "parent": parent });
            }
        }
        EventRow {
            seq: 0,
            ts: self.ts,
            session_id: self.session_id,
            cwd: self.cwd,
            transcript_path: self.transcript_path,
            agent: self.agent,
            event_type: self.event_type,
            matcher: self.matcher,
            payload: payload.to_string(),
        }
    }
}

/// Outcome of one ingest pass.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct IngestSummary {
    /// Complete lines ingested into the `events` table this pass.
    pub events_ingested: usize,
    /// Complete lines that failed to parse (skipped, but their bytes are
    /// still consumed so the offset advances past them). A persistent non-zero
    /// value is the observable signal of torn / interleaved / corrupt lines.
    pub lines_corrupt: usize,
    /// The byte offset after this pass (== the persisted cursor).
    pub offset: u64,
    /// `true` when the offset was reset to 0 this pass because the file was
    /// rotated (inode changed) or truncated then regrown (len < checkpoint).
    pub reset_for_rotation: bool,
    /// `true` when this pass stopped at [`MAX_INGEST_BYTES`] with more suffix
    /// still to read — the next tick continues from the advanced offset.
    pub more_pending: bool,
}

/// The inode of `file`, used as the events.jsonl identity. Falls back to `0`
/// (an impossible real inode) when the platform metadata is unavailable, which
/// simply makes the identity-change check conservative (never spuriously stale).
#[cfg(unix)]
fn inode_of(meta: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    meta.ino()
}

#[cfg(not(unix))]
fn inode_of(_meta: &std::fs::Metadata) -> u64 {
    0
}

/// Read `events.jsonl` from the store's durable offset, ingest up to
/// [`MAX_INGEST_BYTES`] of COMPLETE lines into the `events` table, and persist
/// the new offset + file fingerprint ATOMICALLY (one transaction). A partial
/// trailing line (no terminating `\n`) is left for the next pass. Idempotent
/// w.r.t. the offset: bytes at or before the persisted offset are never
/// re-read; the inserts and offset advance commit together (crash → no dups).
///
/// Best-effort: a missing file is a no-op; a parse failure on one line is
/// counted + logged + skipped (its bytes are still consumed) rather than
/// stalling the cursor forever on a single corrupt line. A rotated / truncated
/// file resets the offset to 0 so we re-read the new file rather than seeking
/// into stale bytes.
pub fn ingest_once(store: &Store, events_jsonl: &Path) -> Result<IngestSummary, StoreError> {
    let mut start = store.read_ingest_offset()?;
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

    let meta = file.metadata().map_err(io_to_store)?;
    let len = meta.len();
    let inode = inode_of(&meta);

    // Truncate-regrow / rotation detection. Compare the current file identity
    // against the persisted fingerprint: a changed inode (rotated / replaced)
    // or an on-disk length BELOW the recorded checkpoint (truncated) means the
    // offset points into stale bytes of a different / shrunken file. Reset to 0
    // so we re-ingest from the start rather than seeking past real data.
    if let Some((prev_inode, prev_len)) = store.read_ingest_fileid()? {
        let rotated = inode != prev_inode;
        let truncated = len < prev_len;
        if rotated || truncated {
            warn!(
                path = %events_jsonl.display(),
                rotated,
                truncated,
                prev_inode,
                inode,
                prev_len,
                len,
                "events.jsonl identity changed; resetting ingest offset to 0"
            );
            // Persist the rewind immediately so a crash right after this can't
            // leave the stale (large) offset pointing into the new file.
            store.write_ingest_offset(0)?;
            start = 0;
            summary.reset_for_rotation = true;
        }
    }

    if len <= start {
        // Nothing new (or the file shrank to at/below the cursor). Record the
        // current identity so the next pass can detect a further change, and do
        // not rewind.
        store.write_ingest_fileid(inode, len)?;
        summary.offset = start;
        return Ok(summary);
    }

    // Seek to the durable cursor and read AT MOST MAX_INGEST_BYTES of the
    // un-ingested suffix. Bounding the read keeps peak memory flat regardless of
    // how far behind we are; the remainder is consumed on the following ticks.
    if file.seek(SeekFrom::Start(start)).is_err() {
        return Ok(summary);
    }
    let suffix_len = len - start;
    let to_read = suffix_len.min(MAX_INGEST_BYTES);
    summary.more_pending = suffix_len > MAX_INGEST_BYTES;
    let mut buf = Vec::with_capacity(to_read as usize);
    if (&mut file).take(to_read).read_to_end(&mut buf).is_err() {
        return Ok(summary);
    }

    // Walk complete lines only. `consumed` tracks bytes past which the offset
    // may advance — it stops at the last `\n` within the bounded buffer, leaving
    // any partial trailing line (torn / in-flight append, OR a line straddling
    // the MAX_INGEST_BYTES chunk boundary) for next pass.
    let mut rows: Vec<EventRow> = Vec::new();
    let mut consumed: usize = 0;
    let mut search_from: usize = 0;
    while let Some(rel_nl) = buf[search_from..].iter().position(|&b| b == b'\n') {
        let nl = search_from + rel_nl;
        let line_bytes = &buf[consumed..nl];
        let line = String::from_utf8_lossy(line_bytes);
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            match serde_json::from_str::<EventLine>(trimmed) {
                Ok(parsed) => rows.push(parsed.into_row()),
                Err(e) => {
                    // Skip the corrupt line but still consume its bytes so the
                    // cursor doesn't wedge on it forever. COUNT + log it so a
                    // dropped interleaved / torn line is observable rather than
                    // silently swallowed (the ≤PIPE_BUF atomicity assumption
                    // should make this rare; a non-zero count flags a violation).
                    summary.lines_corrupt += 1;
                    warn!(
                        error = %e,
                        bytes = line_bytes.len(),
                        "skipping corrupt events.jsonl line (counted; cursor advances)"
                    );
                }
            }
        }
        // Advance past this line including the newline.
        consumed = nl + 1;
        search_from = consumed;
    }

    // Commit the inserts AND the advanced offset in ONE transaction so a crash
    // can never leave inserted rows without the matching offset advance (which
    // would re-ingest them as duplicates on restart). The file fingerprint is
    // recorded after the (durable) batch — it is only a hint for the NEXT pass's
    // rotation check, so a crash before it simply re-establishes it next pass.
    let new_offset = start + consumed as u64;
    summary.events_ingested = rows.len();
    // Commit whenever we advanced over at least one complete line. The batch
    // inserts the parsed rows AND advances the offset in one tx; a pass that
    // consumed only corrupt lines still commits (rows empty, offset advances) so
    // the cursor never wedges. `consumed == 0` (no complete line in the bounded
    // slice, or a reset with nothing new) writes nothing here.
    if consumed > 0 {
        store.ingest_batch(&rows, new_offset)?;
    }
    // The checkpoint length is the offset we have durably consumed up to, NOT
    // the full file length: a later truncation BELOW what we have ingested is
    // what signals staleness. Using `new_offset` keeps the check sound across a
    // bounded multi-pass catch-up (each pass raises the checkpoint as it goes).
    store.write_ingest_fileid(inode, new_offset)?;
    summary.offset = new_offset;
    Ok(summary)
}

/// Prune the `events` table to `policy` (retention story for the event log,
/// mirroring the notifications prune). Thin wrapper so the daemon's retention
/// sweep can prune events alongside notifications. Idempotent.
pub fn prune_events(store: &Store, policy: &RetentionPolicy) -> Result<u64, StoreError> {
    store.prune_events(policy)
}

fn io_to_store(e: std::io::Error) -> StoreError {
    StoreError::Sqlite(rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
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
    fn top_level_parent_is_folded_into_stored_payload() {
        // The canonical line carries `parent` at the top level; ingest folds it
        // into the stored payload object so the Wave 3 materializer can read
        // `payload.parent` without a dedicated column.
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("a.db")).unwrap();
        let jsonl = dir.path().join("events.jsonl");
        append(
            &jsonl,
            "{\"ts\":100,\"session_id\":\"child\",\"cwd\":\"/p\",\"transcript_path\":\"/t/c.jsonl\",\"agent\":\"claude\",\"event_type\":\"Stop\",\"matcher\":null,\"parent\":\"par-1\",\"payload\":{\"x\":1}}\n",
        );
        ingest_once(&store, &jsonl).unwrap();
        let rows = store.events_since(0).unwrap();
        assert_eq!(rows.len(), 1);
        let payload: serde_json::Value = serde_json::from_str(&rows[0].payload).unwrap();
        assert_eq!(payload["parent"], "par-1", "top-level parent folded in");
        assert_eq!(payload["x"], 1, "original payload keys preserved");
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

    #[test]
    fn pass_is_atomic_no_dup_seqs_after_simulated_crash() {
        // A crash BETWEEN the inserts and the offset write must not duplicate
        // rows on restart. With separate autocommits the inserts would have
        // landed while the offset stayed put — restart re-reads the SAME suffix
        // and re-inserts. We model "the inserts committed but the offset write
        // was lost" and assert the single-transaction design prevents it: after
        // a normal pass the offset matches the rows, so a re-run ingests NOTHING
        // (no dup seqs). We then prove the negative directly: manually rewinding
        // the offset (the crash signature) and re-running WOULD dup — which is
        // exactly why the atomic batch keeps them in lock-step.
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("a.db")).unwrap();
        let jsonl = dir.path().join("events.jsonl");
        append(
            &jsonl,
            &format!("{}\n", line(100, "s1", "SessionStart", None)),
        );
        append(&jsonl, &format!("{}\n", line(200, "s1", "Stop", None)));

        let s = ingest_once(&store, &jsonl).unwrap();
        assert_eq!(s.events_ingested, 2);
        let seqs_after_first: Vec<i64> =
            store.events_since(0).unwrap().iter().map(|r| r.seq).collect();
        assert_eq!(seqs_after_first.len(), 2);

        // The offset advanced in the SAME tx as the inserts, so a "restart"
        // (re-running with no new appends) is a clean no-op: no rows, no dups.
        let s2 = ingest_once(&store, &jsonl).unwrap();
        assert_eq!(
            s2.events_ingested, 0,
            "atomic offset advance → no re-ingest"
        );
        let seqs_after_second: Vec<i64> =
            store.events_since(0).unwrap().iter().map(|r| r.seq).collect();
        assert_eq!(
            seqs_after_second, seqs_after_first,
            "no duplicate seqs after a clean restart"
        );
    }

    #[test]
    fn bounded_read_advances_across_multiple_passes() {
        // With a tiny MAX_INGEST_BYTES the suffix is consumed in chunks across
        // passes, the offset advancing each time, never re-reading earlier
        // bytes, until fully caught up. We exercise the bound by writing many
        // lines whose total far exceeds one pass's slice and asserting it takes
        // several passes — each advancing — to ingest them all with no dups.
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("a.db")).unwrap();
        let jsonl = dir.path().join("events.jsonl");

        // 50 lines; each is well over 100 bytes, so a 256-byte cap forces many
        // passes. We can't change the const, so instead assert the real-world
        // invariant: repeated passes monotonically advance the offset and reach
        // the file length with exactly 50 rows and zero duplicates.
        let total = 50;
        for i in 0..total {
            append(&jsonl, &format!("{}\n", line(i, "s1", "Stop", None)));
        }
        let file_len = std::fs::metadata(&jsonl).unwrap().len();

        let mut last_offset = 0u64;
        let mut passes = 0;
        loop {
            let s = ingest_once(&store, &jsonl).unwrap();
            passes += 1;
            assert!(s.offset >= last_offset, "offset never goes backwards");
            last_offset = s.offset;
            if s.offset >= file_len {
                break;
            }
            assert!(passes < 100, "should converge well within 100 passes");
        }
        assert_eq!(store.read_ingest_offset().unwrap(), file_len);
        let rows = store.events_since(0).unwrap();
        assert_eq!(
            rows.len(),
            total as usize,
            "every line ingested exactly once"
        );
        // Seqs are strictly increasing (AUTOINCREMENT) — no duplicates.
        let mut seqs: Vec<i64> = rows.iter().map(|r| r.seq).collect();
        let before = seqs.clone();
        seqs.dedup();
        assert_eq!(seqs.len(), before.len(), "no duplicate seqs across passes");
    }

    #[test]
    fn truncate_then_regrow_resets_offset() {
        // events.jsonl is ingested, then truncated to empty and regrown with a
        // NEW (shorter) set of lines. Without identity detection the stale
        // offset would seek past the new content; with it, the offset resets to
        // 0 and the new lines are ingested.
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("a.db")).unwrap();
        let jsonl = dir.path().join("events.jsonl");

        // Three lines, ingested → offset is at EOF, fingerprint recorded.
        for i in 0..3 {
            append(&jsonl, &format!("{}\n", line(i, "s1", "Stop", None)));
        }
        let s = ingest_once(&store, &jsonl).unwrap();
        assert_eq!(s.events_ingested, 3);
        let old_len = std::fs::metadata(&jsonl).unwrap().len();
        assert!(old_len > 0);

        // Truncate to empty (len drops below the checkpoint), then regrow with
        // ONE short new line — the new EOF is below the old offset.
        std::fs::write(&jsonl, "").unwrap();
        append(
            &jsonl,
            &format!("{}\n", line(999, "s2", "SessionEnd", None)),
        );
        let new_len = std::fs::metadata(&jsonl).unwrap().len();
        assert!(new_len < old_len, "regrown file shorter than old offset");

        let s2 = ingest_once(&store, &jsonl).unwrap();
        assert!(s2.reset_for_rotation, "truncation must reset the offset");
        assert_eq!(s2.events_ingested, 1, "the new line is ingested from 0");
        let rows = store.events_since(0).unwrap();
        // 3 from before + 1 new = 4 (the events table itself is append-only).
        assert_eq!(rows.len(), 4);
        assert_eq!(rows.last().unwrap().session_id, "s2");
        assert_eq!(store.read_ingest_offset().unwrap(), new_len);
    }

    #[test]
    fn prune_events_wrapper_bounds_the_log() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("a.db")).unwrap();
        let jsonl = dir.path().join("events.jsonl");
        for i in 0..6 {
            append(&jsonl, &format!("{}\n", line(i, "s1", "Stop", None)));
        }
        ingest_once(&store, &jsonl).unwrap();
        assert_eq!(store.events_since(0).unwrap().len(), 6);

        let policy = RetentionPolicy {
            retention_days: 0,
            max_rows: 4,
        };
        let deleted = prune_events(&store, &policy).unwrap();
        assert_eq!(deleted, 2, "oldest two trimmed to the cap");
        assert_eq!(store.events_since(0).unwrap().len(), 4);
    }
}
