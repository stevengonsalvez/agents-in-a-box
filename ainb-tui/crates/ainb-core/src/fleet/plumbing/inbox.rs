// ABOUTME: Durable per-parent completion inbox — the moat that replaces the
// broker's lossy fire-and-forget path.
//
// When a child session finishes a turn it commits an [`InboxRecord`] to its
// parent's inbox at `~/.agents-in-a-box/inbox/<parent_id>.jsonl`. The parent
// (e.g. ATC) drains that inbox on its synchronous `Stop` hook (see `drain.rs`)
// and turns the completions into its next turn. The design guarantees:
//
//   * Durable & crash-safe: every commit rewrites the whole inbox through
//     `write_atomic` (temp → fsync → rename → fsync-dir), under an advisory
//     `fs2` lock so concurrent children never corrupt or lose each other's
//     writes. Once `commit` returns the record survives a power loss.
//   * Last-wins-per-child: a child that finishes several turns before the
//     parent drains leaves exactly one record (its latest), keyed by child id.
//     The parent is never spammed with stale intermediate completions.
//   * Exactly-once consumer effects: each record carries a `turn_fingerprint`
//     (blake3 of child id + summary + ts). `drain` records consumed
//     fingerprints in a sidecar marker and refuses to re-deliver them, so a
//     completion written while the consumer was offline replays exactly once
//     on the consumer's next drain — never zero times, never twice.
//   * Dead-letter: a completion whose parent cannot be resolved is appended to
//     `inbox/dead-letter.jsonl` instead of being silently dropped.
//
// The inbox is intentionally a plain JSONL file, not SQLite: it must be writable
// from a shell hook with nothing but `jq`/`cat`, and readable by eye.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};

use super::atomic::write_atomic;
use super::paths;

/// One completion record committed by a finishing child to its parent's inbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxRecord {
    /// The session that finished (the child). The last-wins dedupe key.
    pub child_id: String,
    /// The session that should consume this completion (the parent / routing key).
    pub parent_id: String,
    /// Deterministic fingerprint of this exact completion — the exactly-once
    /// key. Two commits of the same finished turn share a fingerprint.
    pub turn_fingerprint: String,
    /// One-line human summary of what the child finished.
    pub summary: String,
    /// The raw hook event that produced this record (provenance).
    pub event: String,
    /// Epoch milliseconds at which the child finished.
    pub ts: i64,
}

impl InboxRecord {
    /// Build a record, computing the exactly-once `turn_fingerprint` from the
    /// fields that identify a unique finished turn (child + summary + ts).
    #[must_use]
    pub fn new(
        child_id: impl Into<String>,
        parent_id: impl Into<String>,
        summary: impl Into<String>,
        event: impl Into<String>,
        ts: i64,
    ) -> Self {
        let child_id = child_id.into();
        let summary = summary.into();
        let turn_fingerprint = fingerprint(&child_id, &summary, ts);
        Self {
            child_id,
            parent_id: parent_id.into(),
            turn_fingerprint,
            summary,
            event: event.into(),
            ts,
        }
    }
}

/// Deterministic fingerprint identifying one finished turn.
fn fingerprint(child_id: &str, summary: &str, ts: i64) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(child_id.as_bytes());
    hasher.update(b"\x00");
    hasher.update(summary.as_bytes());
    hasher.update(b"\x00");
    hasher.update(&ts.to_le_bytes());
    hasher.finalize().to_hex().to_string()
}

/// A handle to one parent's durable inbox.
pub struct Inbox {
    inbox_dir: PathBuf,
    parent_id: String,
}

impl Inbox {
    /// Open the inbox for `parent_id` under the default ainb home.
    pub fn open(parent_id: &str) -> Result<Self> {
        Ok(Self::open_in(&paths::inbox_dir()?, parent_id))
    }

    /// Open the inbox for `parent_id` under an explicit inbox directory.
    #[must_use]
    pub fn open_in(inbox_dir: &Path, parent_id: &str) -> Self {
        Self {
            inbox_dir: inbox_dir.to_path_buf(),
            parent_id: parent_id.to_string(),
        }
    }

    fn jsonl_path(&self) -> PathBuf {
        self.inbox_dir.join(format!("{}.jsonl", sanitize(&self.parent_id)))
    }

    fn consumed_path(&self) -> PathBuf {
        self.inbox_dir.join(format!("{}.consumed", sanitize(&self.parent_id)))
    }

    fn lock_path(&self) -> PathBuf {
        self.inbox_dir.join(format!("{}.lock", sanitize(&self.parent_id)))
    }

    fn budget_path(&self) -> PathBuf {
        self.inbox_dir.join(format!("{}.budget", sanitize(&self.parent_id)))
    }

    /// Read this parent's consecutive Stop-drain block budget (missing → fresh).
    #[must_use]
    pub fn read_budget(&self) -> super::drain::BlockBudget {
        std::fs::read_to_string(self.budget_path())
            .map(|s| super::drain::BlockBudget::from_str_or_default(&s))
            .unwrap_or_default()
    }

    /// Persist this parent's consecutive Stop-drain block budget atomically.
    pub fn write_budget(&self, budget: &super::drain::BlockBudget) -> Result<()> {
        std::fs::create_dir_all(&self.inbox_dir)
            .with_context(|| format!("creating inbox dir {}", self.inbox_dir.display()))?;
        let json = budget.to_json().context("serializing block budget")?;
        write_atomic(&self.budget_path(), json.as_bytes())
    }

    /// Acquire an exclusive advisory lock guarding this inbox's files. Held for
    /// the duration of a commit or drain so concurrent children / a draining
    /// parent never interleave a read-modify-write.
    fn lock(&self) -> Result<std::fs::File> {
        std::fs::create_dir_all(&self.inbox_dir)
            .with_context(|| format!("creating inbox dir {}", self.inbox_dir.display()))?;
        let f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.lock_path())
            .context("opening inbox lock file")?;
        f.lock_exclusive().context("acquiring inbox lock")?;
        Ok(f)
    }

    /// Read all live records from the JSONL file (one per line; blank/corrupt
    /// lines skipped). Does not lock — callers that mutate hold the lock first.
    fn read_records(&self) -> Vec<InboxRecord> {
        let Ok(text) = std::fs::read_to_string(self.jsonl_path()) else {
            return Vec::new();
        };
        text.lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str::<InboxRecord>(l).ok())
            .collect()
    }

    /// Commit a completion: insert/replace the record for its child (last-wins),
    /// then atomically rewrite the inbox. Crash-safe + lock-guarded.
    pub fn commit(&self, record: &InboxRecord) -> Result<()> {
        let _guard = self.lock()?;
        let mut records = self.read_records();
        // Last-wins-per-child: drop any prior record for this child.
        records.retain(|r| r.child_id != record.child_id);
        records.push(record.clone());
        self.rewrite(&records)
    }

    /// Rewrite the JSONL file with `records`, atomically + durably. An empty set
    /// removes the file entirely so the empty-inbox fast path is a cheap
    /// `exists()` check.
    fn rewrite(&self, records: &[InboxRecord]) -> Result<()> {
        let path = self.jsonl_path();
        if records.is_empty() {
            // Remove the file so a drained inbox costs nothing to detect.
            let _ = std::fs::remove_file(&path);
            return Ok(());
        }
        let mut buf = String::new();
        for r in records {
            buf.push_str(&serde_json::to_string(r).context("serializing inbox record")?);
            buf.push('\n');
        }
        write_atomic(&path, buf.as_bytes())
    }

    /// Whether this inbox currently holds any undrained completions. The cheap
    /// fast path the Stop-drain uses to pay nothing on a leaf session.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        match std::fs::metadata(self.jsonl_path()) {
            Ok(m) => m.len() == 0,
            Err(_) => true,
        }
    }

    /// Peek at the current undrained records WITHOUT consuming them (no marker
    /// write, no clear). Used by read-only inspection (`inbox peek`).
    #[must_use]
    pub fn peek(&self) -> Vec<InboxRecord> {
        let consumed = self.read_consumed();
        self.read_records()
            .into_iter()
            .filter(|r| !consumed.contains(&r.turn_fingerprint))
            .collect()
    }

    /// Drain the inbox: return every not-yet-consumed completion exactly once,
    /// record their fingerprints in the consumed marker, and clear the JSONL.
    ///
    /// Exactly-once: a fingerprint already in the marker is filtered out (it was
    /// delivered on a prior drain — e.g. before a crash that lost the clear), so
    /// re-delivery never happens. A record written while the consumer was
    /// offline is still present in the JSONL and is delivered on this drain.
    pub fn drain(&self) -> Result<Vec<InboxRecord>> {
        let _guard = self.lock()?;
        let consumed = self.read_consumed();
        let all = self.read_records();
        let fresh: Vec<InboxRecord> =
            all.into_iter().filter(|r| !consumed.contains(&r.turn_fingerprint)).collect();

        if fresh.is_empty() {
            // Nothing new — leave the marker + (empty) JSONL untouched.
            return Ok(fresh);
        }

        // Record the freshly-delivered fingerprints BEFORE clearing the JSONL,
        // so a crash between the two writes re-delivers nothing (the marker
        // already names them) rather than double-delivering.
        let mut new_consumed = consumed;
        for r in &fresh {
            new_consumed.insert(r.turn_fingerprint.clone());
        }
        self.write_consumed(&new_consumed)?;
        // Clear delivered records from the JSONL (removes the file when empty).
        self.rewrite(&[])?;
        Ok(fresh)
    }

    /// Read the set of already-consumed turn fingerprints from the sidecar marker.
    fn read_consumed(&self) -> HashSet<String> {
        std::fs::read_to_string(self.consumed_path())
            .map(|t| t.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
            .unwrap_or_default()
    }

    /// Atomically persist the consumed-fingerprint set. Capped to the most
    /// recent `CONSUMED_CAP` fingerprints so the marker cannot grow unbounded.
    fn write_consumed(&self, set: &HashSet<String>) -> Result<()> {
        const CONSUMED_CAP: usize = 1000;
        let mut lines: Vec<&String> = set.iter().collect();
        // Stable order keeps the file diff-friendly; cap from the end.
        lines.sort();
        let start = lines.len().saturating_sub(CONSUMED_CAP);
        let body = lines[start..].iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n");
        write_atomic(&self.consumed_path(), body.as_bytes())
    }
}

/// Append `record` to the dead-letter sink — used when a completion's parent
/// cannot be resolved (no live parent inbox / unknown routing key). Appends
/// rather than rewrites: dead letters are an audit trail, not a live queue.
pub fn dead_letter_in(inbox_dir: &Path, record: &InboxRecord) -> Result<()> {
    use std::io::Write;
    std::fs::create_dir_all(inbox_dir)
        .with_context(|| format!("creating inbox dir {}", inbox_dir.display()))?;
    let path = inbox_dir.join("dead-letter.jsonl");
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("opening dead-letter sink {}", path.display()))?;
    let line = serde_json::to_string(record).context("serializing dead letter")?;
    writeln!(f, "{line}").context("appending dead letter")?;
    f.sync_all().context("fsync dead-letter sink")?;
    Ok(())
}

/// Path to the dead-letter sink under an explicit inbox dir.
#[must_use]
pub fn dead_letter_path_in(inbox_dir: &Path) -> PathBuf {
    inbox_dir.join("dead-letter.jsonl")
}

/// Neutralise a parent id for use as a filename (path-traversal guard).
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

    fn rec(child: &str, parent: &str, summary: &str, ts: i64) -> InboxRecord {
        InboxRecord::new(child, parent, summary, "Stop", ts)
    }

    #[test]
    fn commit_then_drain_delivers_the_record() {
        let dir = TempDir::new().unwrap();
        let inbox = Inbox::open_in(dir.path(), "atc");
        inbox.commit(&rec("c1", "atc", "did the thing", 1)).unwrap();
        let drained = inbox.drain().unwrap();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].child_id, "c1");
        assert_eq!(drained[0].summary, "did the thing");
    }

    #[test]
    fn empty_inbox_drains_to_nothing_and_costs_nothing() {
        let dir = TempDir::new().unwrap();
        let inbox = Inbox::open_in(dir.path(), "atc");
        assert!(inbox.is_empty());
        assert!(inbox.drain().unwrap().is_empty());
        // No JSONL file should be created by an empty drain.
        assert!(!dir.path().join("atc.jsonl").exists());
    }

    #[test]
    fn last_wins_per_child_collapses_multiple_turns() {
        let dir = TempDir::new().unwrap();
        let inbox = Inbox::open_in(dir.path(), "atc");
        // Same child finishes 3 turns before the parent drains.
        inbox.commit(&rec("c1", "atc", "turn one", 1)).unwrap();
        inbox.commit(&rec("c1", "atc", "turn two", 2)).unwrap();
        inbox.commit(&rec("c1", "atc", "turn three", 3)).unwrap();
        let drained = inbox.drain().unwrap();
        assert_eq!(drained.len(), 1, "only the latest turn should survive");
        assert_eq!(drained[0].summary, "turn three");
    }

    #[test]
    fn multiple_children_each_get_one_record() {
        let dir = TempDir::new().unwrap();
        let inbox = Inbox::open_in(dir.path(), "atc");
        inbox.commit(&rec("c1", "atc", "a", 1)).unwrap();
        inbox.commit(&rec("c2", "atc", "b", 2)).unwrap();
        inbox.commit(&rec("c1", "atc", "a2", 3)).unwrap(); // c1 updates
        let mut drained = inbox.drain().unwrap();
        drained.sort_by(|a, b| a.child_id.cmp(&b.child_id));
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].child_id, "c1");
        assert_eq!(drained[0].summary, "a2");
        assert_eq!(drained[1].child_id, "c2");
    }

    #[test]
    fn drain_is_exactly_once_no_redelivery() {
        let dir = TempDir::new().unwrap();
        let inbox = Inbox::open_in(dir.path(), "atc");
        inbox.commit(&rec("c1", "atc", "x", 1)).unwrap();
        let first = inbox.drain().unwrap();
        assert_eq!(first.len(), 1);
        // Second drain on the now-empty inbox delivers nothing.
        let second = inbox.drain().unwrap();
        assert!(second.is_empty(), "record re-delivered: {second:?}");
    }

    #[test]
    fn offline_record_replays_exactly_once_on_next_drain() {
        // Simulate: the child commits while the consumer is offline (never
        // drains), then the consumer comes back and drains. The record must be
        // delivered exactly once.
        let dir = TempDir::new().unwrap();
        let writer = Inbox::open_in(dir.path(), "atc");
        writer.commit(&rec("c1", "atc", "while-offline", 1)).unwrap();

        // Consumer comes online (fresh handle, same files) and drains.
        let consumer = Inbox::open_in(dir.path(), "atc");
        let got = consumer.drain().unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].summary, "while-offline");
        // And never again.
        assert!(consumer.drain().unwrap().is_empty());
    }

    #[test]
    fn duplicate_fingerprint_is_not_redelivered_even_if_recommitted() {
        // A child re-commits the IDENTICAL finished turn (same id+summary+ts) →
        // same fingerprint. After it has been drained once, a re-commit of the
        // same turn must not re-deliver it.
        let dir = TempDir::new().unwrap();
        let inbox = Inbox::open_in(dir.path(), "atc");
        let r = rec("c1", "atc", "same-turn", 1);
        inbox.commit(&r).unwrap();
        assert_eq!(inbox.drain().unwrap().len(), 1);
        // Re-commit the very same turn.
        inbox.commit(&r).unwrap();
        assert!(
            inbox.drain().unwrap().is_empty(),
            "already-consumed fingerprint re-delivered"
        );
    }

    #[test]
    fn peek_does_not_consume() {
        let dir = TempDir::new().unwrap();
        let inbox = Inbox::open_in(dir.path(), "atc");
        inbox.commit(&rec("c1", "atc", "x", 1)).unwrap();
        assert_eq!(inbox.peek().len(), 1);
        // Peeking again still shows it (not consumed).
        assert_eq!(inbox.peek().len(), 1);
        // A real drain then consumes it.
        assert_eq!(inbox.drain().unwrap().len(), 1);
        assert!(inbox.peek().is_empty());
    }

    #[test]
    fn dead_letter_appends_unresolvable_completions() {
        let dir = TempDir::new().unwrap();
        dead_letter_in(dir.path(), &rec("c1", "ghost-parent", "orphaned", 1)).unwrap();
        dead_letter_in(dir.path(), &rec("c2", "ghost-parent", "orphaned2", 2)).unwrap();
        let text = std::fs::read_to_string(dead_letter_path_in(dir.path())).unwrap();
        let lines: Vec<_> = text.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 2, "both dead letters appended");
        assert!(text.contains("orphaned"));
        assert!(text.contains("orphaned2"));
    }

    #[test]
    fn fingerprint_is_deterministic_and_distinguishes_turns() {
        let a = InboxRecord::new("c1", "p", "summary", "Stop", 1);
        let b = InboxRecord::new("c1", "p", "summary", "Stop", 1);
        let c = InboxRecord::new("c1", "p", "different", "Stop", 1);
        assert_eq!(a.turn_fingerprint, b.turn_fingerprint);
        assert_ne!(a.turn_fingerprint, c.turn_fingerprint);
    }
}
