//! Fallback JSONL file handling.
//!
//! When the daemon is unreachable at hook-fire time, the hook script
//! appends the envelope to `~/.agents-in-a-box/notify.fallback.jsonl`
//! (one JSON envelope per line). On its next startup the daemon
//! reads every line, persists each valid envelope, and atomically
//! removes the fallback file.
//!
//! Malformed lines (truncated writes, partial flushes, schema drift)
//! are split off to `notify.fallback.corrupt.jsonl` so they don't get
//! silently lost — the user can grep them for context if needed.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::warn;

use crate::envelope::{Envelope, EnvelopeError};
use crate::store::{RetentionPolicy, Store};

/// Result of a single fallback-file replay run.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReplaySummary {
    /// How many lines were read in total.
    pub lines_read: usize,
    /// How many envelopes were successfully persisted.
    pub envelopes_persisted: usize,
    /// How many lines were malformed and routed to the corrupt file.
    pub lines_corrupt: usize,
}

/// Owns the path to the fallback file. Dropping the value does NOT
/// remove the file — replay does that explicitly only on success.
#[derive(Debug, Clone)]
pub struct FallbackFile {
    path: PathBuf,
    corrupt_path: PathBuf,
}

impl FallbackFile {
    /// Construct from a base path. `notify.fallback.corrupt.jsonl`
    /// is derived from `notify.fallback.jsonl` by appending the
    /// `.corrupt.jsonl` extension before the original suffix.
    pub fn new(path: impl AsRef<Path>) -> Self {
        let path = path.as_ref().to_path_buf();
        // Derive the corrupt path: same parent, name with .corrupt
        // before the trailing .jsonl. For our default of
        // `notify.fallback.jsonl` that yields
        // `notify.fallback.corrupt.jsonl`.
        let corrupt_path = path
            .file_name()
            .and_then(|s| s.to_str())
            .map(|s| {
                let stem = s.strip_suffix(".jsonl").unwrap_or(s);
                format!("{stem}.corrupt.jsonl")
            })
            .map(|name| path.with_file_name(name))
            .unwrap_or_else(|| path.with_extension("corrupt.jsonl"));
        Self { path, corrupt_path }
    }

    /// Path to the active fallback file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Path to the corrupt-line dump (only written to when malformed
    /// lines appear).
    pub fn corrupt_path(&self) -> &Path {
        &self.corrupt_path
    }

    /// Append a single envelope as a JSONL line. Used by tests and
    /// (defensively) by the daemon when an in-flight envelope cannot
    /// be parsed but might be recoverable later.
    pub fn append(&self, env: &Envelope) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let line = serde_json::to_string(env)?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("opening fallback file {}", self.path.display()))?;
        writeln!(file, "{line}")?;
        Ok(())
    }

    /// Replay the fallback file into the store, then remove the file.
    /// If the file does not exist, this is a no-op returning a zero
    /// summary.
    pub fn replay_into(&self, store: &Store, policy: &RetentionPolicy) -> Result<ReplaySummary> {
        if !self.path.exists() {
            return Ok(ReplaySummary::default());
        }
        let file = std::fs::File::open(&self.path)
            .with_context(|| format!("opening fallback file {}", self.path.display()))?;
        let reader = BufReader::new(file);
        let mut summary = ReplaySummary::default();
        let mut corrupt_lines: Vec<String> = Vec::new();
        for line in reader.lines() {
            let line =
                line.with_context(|| format!("reading line from {}", self.path.display()))?;
            summary.lines_read += 1;
            if line.trim().is_empty() {
                continue;
            }
            match Envelope::from_bytes(line.as_bytes()) {
                Ok(env) => {
                    if let Err(e) = store.insert_and_prune(&env, policy) {
                        warn!(error = ?e, "failed to persist fallback envelope; routing to corrupt");
                        corrupt_lines.push(line);
                        summary.lines_corrupt += 1;
                    } else {
                        summary.envelopes_persisted += 1;
                    }
                }
                Err(EnvelopeError::Json(_))
                | Err(EnvelopeError::MissingField(_))
                | Err(EnvelopeError::UnsupportedVersion { .. }) => {
                    corrupt_lines.push(line);
                    summary.lines_corrupt += 1;
                }
            }
        }
        if !corrupt_lines.is_empty() {
            self.append_corrupt(&corrupt_lines)?;
        }
        // Remove the active fallback file last so a crash mid-replay
        // leaves a recoverable file behind.
        std::fs::remove_file(&self.path)
            .with_context(|| format!("removing replayed fallback {}", self.path.display()))?;
        Ok(summary)
    }

    fn append_corrupt(&self, lines: &[String]) -> Result<()> {
        if let Some(parent) = self.corrupt_path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.corrupt_path)
            .with_context(|| format!("opening corrupt file {}", self.corrupt_path.display()))?;
        for line in lines {
            writeln!(file, "{line}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn env(ts: i64) -> Envelope {
        Envelope {
            protocol_version: 1,
            agent: "claude".into(),
            raw_event: "Stop".into(),
            session_id: format!("s-{ts}"),
            cwd: "/tmp/x".into(),
            project: "x".into(),
            ts,
            payload: json!({"k": "v"}),
        }
    }

    fn no_retention() -> RetentionPolicy {
        RetentionPolicy {
            retention_days: 0,
            max_rows: 0,
        }
    }

    #[test]
    fn empty_path_replay_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let fb = FallbackFile::new(dir.path().join("notify.fallback.jsonl"));
        let store = Store::open(dir.path().join("notifications.db")).unwrap();
        let summary = fb.replay_into(&store, &no_retention()).unwrap();
        assert_eq!(summary, ReplaySummary::default());
    }

    #[test]
    fn append_then_replay_persists_all_envelopes_and_removes_file() {
        let dir = tempfile::tempdir().unwrap();
        let fb = FallbackFile::new(dir.path().join("notify.fallback.jsonl"));
        fb.append(&env(1)).unwrap();
        fb.append(&env(2)).unwrap();
        fb.append(&env(3)).unwrap();
        assert!(fb.path().exists());
        let store = Store::open(dir.path().join("notifications.db")).unwrap();
        let summary = fb.replay_into(&store, &no_retention()).unwrap();
        assert_eq!(summary.lines_read, 3);
        assert_eq!(summary.envelopes_persisted, 3);
        assert_eq!(summary.lines_corrupt, 0);
        assert_eq!(store.count().unwrap(), 3);
        assert!(!fb.path().exists());
    }

    #[test]
    fn corrupt_lines_routed_to_corrupt_file_and_active_file_cleared() {
        let dir = tempfile::tempdir().unwrap();
        let fb = FallbackFile::new(dir.path().join("notify.fallback.jsonl"));
        fb.append(&env(1)).unwrap();
        // Append a deliberately malformed line.
        {
            let mut f = std::fs::OpenOptions::new().append(true).open(fb.path()).unwrap();
            writeln!(f, "not a json line").unwrap();
        }
        fb.append(&env(2)).unwrap();
        let store = Store::open(dir.path().join("notifications.db")).unwrap();
        let summary = fb.replay_into(&store, &no_retention()).unwrap();
        assert_eq!(summary.lines_read, 3);
        assert_eq!(summary.envelopes_persisted, 2);
        assert_eq!(summary.lines_corrupt, 1);
        assert_eq!(store.count().unwrap(), 2);
        assert!(!fb.path().exists());
        // The corrupt file should now contain exactly the bad line.
        let corrupt = std::fs::read_to_string(fb.corrupt_path()).unwrap();
        assert_eq!(corrupt.trim(), "not a json line");
    }
}
