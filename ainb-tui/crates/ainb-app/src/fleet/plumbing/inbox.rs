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

    /// Reset this inbox's block budget to zero under the inbox lock — the
    /// genuine-user-turn path. Holding the lock serialises the reset against a
    /// concurrent Stop-drain's block increment so neither read-modify-write
    /// clobbers the other (see [`Self::drain_with_budget`]).
    pub fn reset_budget(&self) -> Result<()> {
        let _guard = self.lock()?;
        let mut budget = self.read_budget();
        budget.reset();
        self.write_budget(&budget)
    }

    /// The Stop-drain decision, computed under a single hold of the inbox lock so
    /// the budget read-modify-write cannot race a concurrent `UserPromptSubmit`
    /// reset (M2) and so completions are never consumed unless they will actually
    /// be emitted (H1).
    ///
    /// Behaviour, all under one lock:
    ///   * Read the current block budget. If it is already exhausted
    ///     (`consecutive_blocks >= budget_cap`), do **not** drain — leave the
    ///     completions in the inbox so a later genuine user turn (which resets the
    ///     budget) lets them drain and deliver. Returns `None` with the inbox
    ///     untouched.
    ///   * Otherwise drain the inbox exactly-once. If the drain yielded fresh
    ///     completions, increment + persist the budget and return the block
    ///     decision carrying them. An empty drain leaves the budget untouched and
    ///     returns `None`.
    ///
    /// Net: a completion is consumed only on the drain that also emits it, so it
    /// is delivered exactly once and never silently destroyed.
    pub fn drain_with_budget(
        &self,
        budget_cap: u32,
    ) -> Result<(Vec<InboxRecord>, Option<super::drain::StopDecision>)> {
        let _guard = self.lock()?;
        let mut budget = self.read_budget();
        // Budget exhausted → do NOT drain. Leaving the completions in the inbox
        // means a later reset (genuine user turn) lets them deliver, rather than
        // consuming them here only to emit nothing (data loss).
        if budget.consecutive_blocks >= budget_cap.max(1) {
            return Ok((Vec::new(), None));
        }
        let completions = self.drain_locked()?;
        let decision = super::drain::decide(&completions, budget.consecutive_blocks, budget_cap);
        if decision.is_some() {
            budget.record_block();
            self.write_budget(&budget)?;
        }
        Ok((completions, decision))
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
        self.drain_locked()
    }

    /// Drain only the records that match `snapshot` by turn fingerprint.
    ///
    /// Used by ATC heartbeat after it has already rendered a peeked completion
    /// snapshot into the body. A child can commit another completion between
    /// that peek and the confirmed tmux send; this method consumes only the
    /// fingerprints already present in the delivered body and leaves later
    /// records in the JSONL for the next heartbeat.
    pub fn drain_matching(&self, snapshot: &[InboxRecord]) -> Result<Vec<InboxRecord>> {
        if snapshot.is_empty() {
            return Ok(Vec::new());
        }
        let expected: HashSet<String> =
            snapshot.iter().map(|r| r.turn_fingerprint.clone()).collect();
        let _guard = self.lock()?;
        self.drain_matching_locked(&expected)
    }

    /// The exactly-once drain body, assuming the caller already holds the inbox
    /// lock (e.g. [`Self::drain_with_budget`]). Never acquires the lock itself.
    fn drain_locked(&self) -> Result<Vec<InboxRecord>> {
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
        // already names them) rather than double-delivering. Each marker carries
        // the completion's timestamp so eviction is oldest-first by recency, not
        // by an arbitrary hex sort (a re-committed old turn past the cap must not
        // re-deliver).
        let mut entries = self.read_consumed_entries();
        for r in &fresh {
            entries.push((r.ts, r.turn_fingerprint.clone()));
        }
        self.write_consumed_entries(entries)?;
        // Clear delivered records from the JSONL (removes the file when empty).
        self.rewrite(&[])?;
        Ok(fresh)
    }

    /// Snapshot drain body, assuming the caller holds the inbox lock.
    fn drain_matching_locked(&self, expected: &HashSet<String>) -> Result<Vec<InboxRecord>> {
        let consumed = self.read_consumed();
        let all = self.read_records();
        let fresh: Vec<InboxRecord> = all
            .iter()
            .filter(|r| {
                expected.contains(&r.turn_fingerprint) && !consumed.contains(&r.turn_fingerprint)
            })
            .cloned()
            .collect();

        if fresh.is_empty() {
            return Ok(fresh);
        }

        let mut entries = self.read_consumed_entries();
        for r in &fresh {
            entries.push((r.ts, r.turn_fingerprint.clone()));
        }
        self.write_consumed_entries(entries)?;

        let delivered: HashSet<String> = fresh.iter().map(|r| r.turn_fingerprint.clone()).collect();
        let remaining: Vec<InboxRecord> = all
            .into_iter()
            .filter(|r| {
                !delivered.contains(&r.turn_fingerprint) && !consumed.contains(&r.turn_fingerprint)
            })
            .collect();
        self.rewrite(&remaining)?;
        Ok(fresh)
    }

    /// Read the set of already-consumed turn fingerprints for membership testing.
    /// Built from the recency-ordered entries (timestamp is ignored here).
    fn read_consumed(&self) -> HashSet<String> {
        self.read_consumed_entries().into_iter().map(|(_, fp)| fp).collect()
    }

    /// Read the consumed marker as `(ts_ms, fingerprint)` entries. Each line is
    /// `"<ts_ms> <fingerprint>"`; a bare-hex line (legacy format) is tolerated as
    /// `ts = 0` so an upgrade in place never re-delivers old turns.
    fn read_consumed_entries(&self) -> Vec<(i64, String)> {
        let Ok(text) = std::fs::read_to_string(self.consumed_path()) else {
            return Vec::new();
        };
        text.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(|l| match l.split_once(' ') {
                Some((ts, fp)) => (ts.trim().parse::<i64>().unwrap_or(0), fp.trim().to_string()),
                // Legacy bare-hex line: no timestamp → treat as oldest (ts = 0).
                None => (0, l.to_string()),
            })
            .collect()
    }

    /// Atomically persist the consumed entries, evicting OLDEST-first by
    /// timestamp so the marker stays bounded at `CONSUMED_CAP` while always
    /// remembering the most recent turns. De-dupes by fingerprint, keeping the
    /// newest timestamp seen for each.
    fn write_consumed_entries(&self, entries: Vec<(i64, String)>) -> Result<()> {
        const CONSUMED_CAP: usize = 1000;
        // Collapse duplicate fingerprints to their newest timestamp.
        let mut newest: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
        for (ts, fp) in entries {
            let e = newest.entry(fp).or_insert(ts);
            if ts > *e {
                *e = ts;
            }
        }
        let mut deduped: Vec<(i64, String)> = newest.into_iter().map(|(fp, ts)| (ts, fp)).collect();
        // Sort by recency (ts asc, fingerprint as a stable tiebreak), then keep
        // the newest CONSUMED_CAP — evicting the oldest, not an arbitrary hex
        // suffix.
        deduped.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        let start = deduped.len().saturating_sub(CONSUMED_CAP);
        let body = deduped[start..]
            .iter()
            .map(|(ts, fp)| format!("{ts} {fp}"))
            .collect::<Vec<_>>()
            .join("\n");
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
    fn drain_matching_preserves_records_written_after_snapshot() {
        let dir = TempDir::new().unwrap();
        let inbox = Inbox::open_in(dir.path(), "atc");
        inbox.commit(&rec("c1", "atc", "sent in heartbeat", 1)).unwrap();
        let snapshot = inbox.peek();
        assert_eq!(snapshot.len(), 1);

        // This completion arrives after the heartbeat body was built. It must
        // not be consumed by the send-commit for the older snapshot.
        inbox.commit(&rec("c2", "atc", "arrived later", 2)).unwrap();

        let drained = inbox.drain_matching(&snapshot).unwrap();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].child_id, "c1");

        let remaining = inbox.peek();
        assert_eq!(remaining.len(), 1, "late completion must remain pending");
        assert_eq!(remaining[0].child_id, "c2");
        assert_eq!(remaining[0].summary, "arrived later");
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

    // --- H1: budget exhaustion must NOT consume completions ------------------

    /// Regression (H1): when the block budget is exhausted, `drain_with_budget`
    /// must NOT drain — the completions stay in the inbox so a later genuine
    /// user turn (which resets the budget) can deliver them. The pre-fix code
    /// drained unconditionally then emitted nothing → the completions were
    /// consumed but delivered nowhere (permanent data loss).
    #[test]
    fn budget_exhaustion_does_not_consume_completions_then_reset_delivers_once() {
        let dir = TempDir::new().unwrap();
        let inbox = Inbox::open_in(dir.path(), "atc");
        inbox.commit(&rec("c1", "atc", "important completion", 1)).unwrap();

        // Drive the budget to the cap.
        let mut budget = super::super::drain::BlockBudget::default();
        for _ in 0..super::super::drain::DEFAULT_BLOCK_BUDGET {
            budget.record_block();
        }
        inbox.write_budget(&budget).unwrap();

        // Budget exhausted → no emission AND the inbox is left untouched.
        let (drained, decision) =
            inbox.drain_with_budget(super::super::drain::DEFAULT_BLOCK_BUDGET).unwrap();
        assert!(drained.is_empty(), "exhausted budget must not drain");
        assert!(decision.is_none(), "exhausted budget must not emit a block");
        assert!(
            !inbox.is_empty(),
            "completions must remain in the inbox, not be silently consumed"
        );
        assert_eq!(inbox.peek().len(), 1, "the completion is still pending");

        // A genuine user turn resets the budget; now the completion delivers...
        inbox.reset_budget().unwrap();
        let (drained, decision) =
            inbox.drain_with_budget(super::super::drain::DEFAULT_BLOCK_BUDGET).unwrap();
        assert_eq!(
            drained.len(),
            1,
            "reset budget delivers the held completion"
        );
        assert_eq!(drained[0].summary, "important completion");
        assert!(decision.is_some(), "reset budget emits the block");

        // ...exactly once: a follow-up drain delivers nothing.
        let (drained, decision) =
            inbox.drain_with_budget(super::super::drain::DEFAULT_BLOCK_BUDGET).unwrap();
        assert!(
            drained.is_empty(),
            "no re-delivery after the held completion drained"
        );
        assert!(decision.is_none());
    }

    // --- M1: consumed-marker eviction is oldest-first by recency -------------

    /// Regression (M1): the consumed marker must evict the OLDEST fingerprints
    /// when over the cap, not the lexically-smallest hex (which evicts randomly
    /// w.r.t. time and lets a re-committed old turn re-deliver). We write >1000
    /// markers over strictly increasing timestamps and assert the oldest are
    /// gone while the most recent are remembered.
    #[test]
    fn consumed_marker_evicts_oldest_by_recency_not_hex() {
        let dir = TempDir::new().unwrap();
        let inbox = Inbox::open_in(dir.path(), "atc");

        // Build 1100 (ts, fingerprint) entries with increasing ts. Use the real
        // fingerprint helper so the values are realistic hex strings.
        let mut entries: Vec<(i64, String)> = Vec::new();
        for ts in 0..1100i64 {
            let fp = fingerprint("child", "summary", ts);
            entries.push((ts, fp));
        }
        let oldest_fp = entries[0].1.clone(); // ts = 0, must be evicted
        let recent_fp = entries[1099].1.clone(); // newest, must survive
        inbox.write_consumed_entries(entries).unwrap();

        let remembered = inbox.read_consumed();
        assert_eq!(remembered.len(), 1000, "capped to CONSUMED_CAP");
        assert!(
            !remembered.contains(&oldest_fp),
            "the oldest fingerprint must be evicted"
        );
        assert!(
            remembered.contains(&recent_fp),
            "the most recent fingerprint must be remembered"
        );

        // And the boundary: ts=99 should be gone (in the oldest 100), ts=100 the
        // first survivor.
        assert!(!remembered.contains(&fingerprint("child", "summary", 99)));
        assert!(remembered.contains(&fingerprint("child", "summary", 100)));
    }

    /// Legacy bare-hex consumed lines (no timestamp) are tolerated on read and
    /// treated as oldest (ts = 0), so an in-place upgrade never re-delivers an
    /// already-consumed turn.
    #[test]
    fn consumed_marker_tolerates_legacy_bare_hex_lines() {
        let dir = TempDir::new().unwrap();
        let inbox = Inbox::open_in(dir.path(), "atc");
        let legacy = fingerprint("c1", "old-turn", 5);
        // Write a legacy-format marker file (bare hex, no ts prefix).
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(inbox.consumed_path(), format!("{legacy}\n")).unwrap();

        let remembered = inbox.read_consumed();
        assert!(
            remembered.contains(&legacy),
            "legacy bare-hex marker must still count as consumed"
        );

        // Committing + draining the same turn must NOT re-deliver it.
        inbox.commit(&InboxRecord::new("c1", "atc", "old-turn", "Stop", 5)).unwrap();
        assert!(
            inbox.drain().unwrap().is_empty(),
            "legacy-consumed turn re-delivered"
        );
    }

    // --- M2: budget reset + block-increment serialise under the lock ---------

    /// Regression (M2): the block-budget read-modify-write must happen under the
    /// inbox lock, so concurrent block increments cannot lose updates. We run N
    /// threads that each commit a turn then `drain_with_budget` (a block, which
    /// increments the budget) against a budget cap large enough that none are
    /// refused, and assert NO increments were lost (final count == N). The
    /// pre-fix unlocked read-modify-write loses increments under contention →
    /// final count < N.
    #[test]
    fn concurrent_block_increments_lose_no_updates_under_lock() {
        use std::sync::Arc;
        const N: u32 = 64;
        let dir = Arc::new(TempDir::new().unwrap());
        let path = dir.path().to_path_buf();
        // A cap above N so every drain is permitted to block (and thus bump).
        let cap = N + 10;

        let mut handles = Vec::new();
        for i in 0..N {
            let p = path.clone();
            handles.push(std::thread::spawn(move || {
                let inbox = Inbox::open_in(&p, "atc");
                // Each thread commits a DISTINCT child so every drain has a fresh
                // completion to emit (a block), incrementing the budget once.
                let child = format!("c{i}");
                inbox.commit(&rec(&child, "atc", "x", i64::from(i) + 1)).unwrap();
                let (_drained, decision) = inbox.drain_with_budget(cap).unwrap();
                // Each thread that drained a fresh completion blocked exactly once.
                decision.is_some()
            }));
        }
        let blocked: u32 = handles.into_iter().map(|h| u32::from(h.join().unwrap())).sum();

        let budget = Inbox::open_in(&path, "atc").read_budget();
        assert_eq!(
            budget.consecutive_blocks, blocked,
            "lost-update: {blocked} blocks emitted but budget counted {}",
            budget.consecutive_blocks
        );
    }
}
