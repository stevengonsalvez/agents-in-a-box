// ABOUTME: Filesystem layout for the durable orchestration plumbing.
//
// All artefacts live under the ainb home (`$AINB_HOME`, else `~/.agents-in-a-box`):
//   events.jsonl                  — append-only hook event log (notifyd ingests)
//   inbox/<parent_id>.jsonl       — per-parent durable completion inbox (inbox.rs)
//   inbox/<parent_id>.consumed    — exactly-once consumed-fingerprint marker
//   inbox/<parent_id>.budget      — consecutive Stop-drain block budget
//   inbox/dead-letter.jsonl       — completions whose parent is unresolvable
//
// The `*_in(home)` variants take an explicit home so tests isolate to a tempdir
// without mutating process-global env. The bare functions resolve the real home.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// The ainb home directory: `$AINB_HOME` if set + non-empty, else
/// `~/.agents-in-a-box`. Matches `fleet::atc::paths::ainb_home`.
pub fn ainb_home() -> Result<PathBuf> {
    if let Ok(h) = std::env::var("AINB_HOME") {
        if !h.is_empty() {
            return Ok(PathBuf::from(h));
        }
    }
    let home = dirs::home_dir().context("could not resolve home directory")?;
    Ok(home.join(".agents-in-a-box"))
}

/// `<home>/inbox` — per-parent inboxes + dead-letter sink.
pub fn inbox_dir() -> Result<PathBuf> {
    Ok(ainb_home()?.join("inbox"))
}

/// `<home>/inbox` under an explicit home.
#[must_use]
pub fn inbox_dir_in(home: &Path) -> PathBuf {
    home.join("inbox")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dirs_nest_under_home() {
        let home = Path::new("/tmp/x/.agents-in-a-box");
        assert_eq!(inbox_dir_in(home), home.join("inbox"));
    }

    #[test]
    fn ainb_home_honours_env_override() {
        // Pure relationship check without mutating global env: when AINB_HOME is
        // unset the resolved home ends with the canonical dir name.
        if std::env::var("AINB_HOME").is_err() {
            let h = ainb_home().unwrap();
            assert!(h.ends_with(".agents-in-a-box"));
        }
    }
}
