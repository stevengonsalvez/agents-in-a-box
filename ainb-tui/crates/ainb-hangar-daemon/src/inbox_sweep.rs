//! y0f: periodic cap of the daemon's OWN parent completion inbox.
//!
//! The lifecycle hook (`ainb fleet atc hook`) routes every daemon-spawned
//! session's Stop / Notification completion to the daemon's parent inbox at
//! `<ainb_home>/inbox/hangar-daemon.jsonl` — the daemon stamps
//! `AINB_PARENT_SESSION=hangar-daemon` onto every run it spawns (see
//! [`crate::run_loop`]). But the daemon detects run completion via the run
//! OUTCOME, not the inbox, so nothing ever drains it: the file is pure exhaust
//! that would otherwise grow without bound as tasks accrue.
//!
//! [`sweep_parent_inbox`] caps that exhaust on the sweeper tick, keeping only
//! the most recent [`KEEP_LAST`] records. It is exactly-once safe and never
//! races the hook:
//!
//! * It serialises with the hook's concurrent commits by taking the SAME `fs2`
//!   advisory lock the inbox writer uses (`inbox/hangar-daemon.lock`), held
//!   across the read → keep → rewrite so a commit lands fully before or after
//!   the cap — never interleaved.
//! * It rewrites the JSONL ATOMICALLY (temp file → fsync → rename), so a
//!   concurrent reader never observes a torn file.
//! * It touches ONLY the live JSONL queue — never the exactly-once consumed
//!   marker — so it can never trigger a re-delivery of a completion to any
//!   other consumer. (The daemon is the sole consumer of this inbox and never
//!   drains it, so dropping old exhaust records loses nothing.)
//!
//! The path resolution deliberately mirrors `ainb-core`'s `paths::inbox_dir`
//! (the hook writer) byte-for-byte — `$AINB_HOME` verbatim when set +
//! non-empty, else `~/.agents-in-a-box`, then `/inbox` — so the daemon caps
//! exactly the file the hook appends to. The daemon cannot depend on
//! `ainb-core` (a crate cycle), so the resolution + the minimal
//! lock/atomic-write protocol are re-expressed here rather than reused.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use fs2::FileExt as _;

/// The daemon's parent-session id — the inbox file stem the hook writes to.
/// Mirrors [`crate::run_loop`]'s `HANGAR_PARENT_SESSION`.
const PARENT_ID: &str = "hangar-daemon";

/// How many of the most-recent records each sweep keeps; older exhaust is
/// evicted. Bounds the file while leaving a useful recent tail for eyeball
/// debugging (the inbox is a plain, human-readable JSONL).
pub(crate) const KEEP_LAST: usize = 200;

/// The outcome of one cap pass (for logging + tests).
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct SweepReport {
    /// Records present in the inbox before the cap.
    pub before: usize,
    /// Records kept after the cap (`<= keep_last`).
    pub kept: usize,
}

impl SweepReport {
    /// How many records the cap evicted.
    pub(crate) const fn evicted(&self) -> usize {
        self.before.saturating_sub(self.kept)
    }
}

/// Resolve the ainb inbox dir: `$AINB_HOME` verbatim (set + non-empty) else
/// `~/.agents-in-a-box`, then `/inbox`. `None` when no home resolves (the cap
/// is then a no-op).
fn inbox_dir() -> Option<PathBuf> {
    let base = match std::env::var("AINB_HOME") {
        Ok(h) if !h.is_empty() => PathBuf::from(h),
        _ => dirs::home_dir()?.join(".agents-in-a-box"),
    };
    Some(base.join("inbox"))
}

/// Cap the daemon's parent inbox to the most recent `keep_last` records.
///
/// Best-effort: a missing home / inbox file is a clean no-op ([`SweepReport`]
/// with `before == 0`). Resolves the real inbox dir; see [`sweep_inbox_in`] for
/// the test seam.
///
/// # Errors
///
/// Propagates an [`std::io::Error`] from the lock / read / atomic-rewrite — the
/// caller ([`crate::run_loop`]) logs and swallows it (inbox hygiene must never
/// down a sweeper).
pub(crate) fn sweep_parent_inbox(keep_last: usize) -> std::io::Result<SweepReport> {
    let Some(dir) = inbox_dir() else {
        return Ok(SweepReport::default());
    };
    sweep_inbox_in(&dir, PARENT_ID, keep_last)
}

/// [`sweep_parent_inbox`] against an explicit inbox dir + parent id — the test
/// seam (isolates to a tempdir without mutating process env).
///
/// # Errors
///
/// As [`sweep_parent_inbox`].
pub(crate) fn sweep_inbox_in(
    inbox_dir: &Path,
    parent_id: &str,
    keep_last: usize,
) -> std::io::Result<SweepReport> {
    let jsonl = inbox_dir.join(format!("{parent_id}.jsonl"));

    // Cheap unlocked peek: skip the lock + rewrite entirely when the file is
    // absent (never written) or already within the cap. A commit racing this
    // peek only means we re-check under the lock below before touching anything.
    let Ok(text) = std::fs::read_to_string(&jsonl) else {
        return Ok(SweepReport::default());
    };
    let peeked = count_records(&text);
    if peeked <= keep_last {
        return Ok(SweepReport {
            before: peeked,
            kept: peeked,
        });
    }

    // Over the cap: take the inbox lock (the SAME advisory lock the hook writer
    // holds for a commit) and re-read UNDER it, so the keep-set reflects any
    // commit that landed between the peek and here. The lock releases when
    // `_lock` drops at end of scope (including the early returns).
    std::fs::create_dir_all(inbox_dir)?;
    let lock_path = inbox_dir.join(format!("{parent_id}.lock"));
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)?;
    lock.lock_exclusive()?;
    // Bind so the guard lives to the end of the function (drop == unlock).
    let _lock = lock;

    let text = std::fs::read_to_string(&jsonl).unwrap_or_default();
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let before = lines.len();
    if before <= keep_last {
        return Ok(SweepReport {
            before,
            kept: before,
        });
    }

    // Rank the records by recency (`ts`; an unparseable line ranks oldest, ts 0),
    // keep the newest `keep_last`, and re-emit them in their ORIGINAL order so the
    // surviving tail reads chronologically like the hook wrote it.
    let mut indexed: Vec<(usize, i64)> =
        lines.iter().enumerate().map(|(i, l)| (i, parse_ts(l))).collect();
    // Ascending by (ts, original index) — the last `keep_last` are the newest.
    indexed.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    let mut keep_idx: Vec<usize> = indexed[before - keep_last..].iter().map(|(i, _)| *i).collect();
    keep_idx.sort_unstable();

    let mut body = String::new();
    for i in keep_idx {
        body.push_str(lines[i]);
        body.push('\n');
    }

    // Atomic rewrite: write the capped body to a sibling temp, fsync it, then
    // rename over the live file (an atomic replace on unix). A crash mid-write
    // leaves the original intact + a stray `.tmp`, never a torn inbox.
    let tmp = inbox_dir.join(format!("{parent_id}.jsonl.cap.tmp"));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(body.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &jsonl)?;

    Ok(SweepReport {
        before,
        kept: keep_last,
    })
}

/// Count the non-blank records in a raw JSONL body.
fn count_records(text: &str) -> usize {
    text.lines().filter(|l| !l.trim().is_empty()).count()
}

/// Extract a record's `ts` (epoch-ms) for recency ranking; a line that is not
/// JSON, or lacks a numeric `ts`, ranks oldest (0) so malformed exhaust is
/// evicted first.
fn parse_ts(line: &str) -> i64 {
    serde_json::from_str::<serde_json::Value>(line)
        .ok()
        .and_then(|v| v.get("ts").and_then(serde_json::Value::as_i64))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write `n` inbox-shaped records (`{"ts": i, ...}`) to
    /// `<dir>/<parent>.jsonl`, ts ascending 0..n, and return the path.
    fn seed_inbox(dir: &Path, parent: &str, n: usize) -> PathBuf {
        let path = dir.join(format!("{parent}.jsonl"));
        let mut body = String::new();
        for i in 0..n {
            body.push_str(&format!(
                r#"{{"child_id":"c{i}","parent_id":"{parent}","turn_fingerprint":"fp{i}","summary":"s{i}","event":"Stop","ts":{i}}}"#
            ));
            body.push('\n');
        }
        std::fs::write(&path, body).unwrap();
        path
    }

    fn read_ts(path: &Path) -> Vec<i64> {
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(parse_ts)
            .collect()
    }

    #[test]
    fn missing_file_is_a_clean_noop() {
        let dir = tempfile::tempdir().unwrap();
        let report = sweep_inbox_in(dir.path(), PARENT_ID, KEEP_LAST).unwrap();
        assert_eq!(report, SweepReport { before: 0, kept: 0 });
        assert!(!dir.path().join(format!("{PARENT_ID}.jsonl")).exists());
    }

    #[test]
    fn under_cap_is_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = seed_inbox(dir.path(), PARENT_ID, 5);
        let before = std::fs::read_to_string(&path).unwrap();
        let report = sweep_inbox_in(dir.path(), PARENT_ID, 10).unwrap();
        assert_eq!(report, SweepReport { before: 5, kept: 5 });
        // Byte-identical: no rewrite happened under the cap.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    }

    #[test]
    fn exactly_at_cap_is_untouched() {
        let dir = tempfile::tempdir().unwrap();
        seed_inbox(dir.path(), PARENT_ID, 10);
        let report = sweep_inbox_in(dir.path(), PARENT_ID, 10).unwrap();
        assert_eq!(report.evicted(), 0);
        assert_eq!(
            report,
            SweepReport {
                before: 10,
                kept: 10,
            }
        );
    }

    #[test]
    fn over_cap_keeps_only_the_newest_n_by_ts() {
        let dir = tempfile::tempdir().unwrap();
        let path = seed_inbox(dir.path(), PARENT_ID, 50);
        let report = sweep_inbox_in(dir.path(), PARENT_ID, 10).unwrap();
        assert_eq!(
            report,
            SweepReport {
                before: 50,
                kept: 10,
            }
        );
        // The survivors are the newest 10 (ts 40..=49), in chronological order.
        assert_eq!(read_ts(&path), (40..50).collect::<Vec<i64>>());
        // A re-sweep is idempotent (already at the cap).
        let again = sweep_inbox_in(dir.path(), PARENT_ID, 10).unwrap();
        assert_eq!(again.evicted(), 0);
        assert_eq!(read_ts(&path), (40..50).collect::<Vec<i64>>());
    }

    #[test]
    fn malformed_lines_rank_oldest_and_are_evicted_first() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(format!("{PARENT_ID}.jsonl"));
        // Two junk lines (rank ts 0) interleaved with three real ones.
        let body = "not json\n\
            {\"child_id\":\"c1\",\"ts\":100}\n\
            also not json\n\
            {\"child_id\":\"c2\",\"ts\":200}\n\
            {\"child_id\":\"c3\",\"ts\":300}\n";
        std::fs::write(&path, body).unwrap();
        let report = sweep_inbox_in(dir.path(), PARENT_ID, 2).unwrap();
        assert_eq!(report, SweepReport { before: 5, kept: 2 });
        // The two newest real records survive; both junk lines are gone.
        assert_eq!(read_ts(&path), vec![200, 300]);
    }

    #[test]
    fn blank_lines_are_ignored_in_the_count() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(format!("{PARENT_ID}.jsonl"));
        // 3 real records with blank padding — the count must see 3, not 6.
        std::fs::write(&path, "\n{\"ts\":1}\n\n{\"ts\":2}\n{\"ts\":3}\n\n").unwrap();
        let report = sweep_inbox_in(dir.path(), PARENT_ID, 5).unwrap();
        assert_eq!(report, SweepReport { before: 3, kept: 3 });
    }
}
