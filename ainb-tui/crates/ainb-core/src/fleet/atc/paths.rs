// ABOUTME: Filesystem layout for ATC instances.
//
// Every instance lives under `~/.agents-in-a-box/atc/<name>/`:
//   meta.json            — AtcMeta config
//   CLAUDE.md            — rendered policy the ATC session reads
//   state.json           — single-writer ATC-session durable state (retry_counts /
//                          escalated / notes); survives context compaction
//   heartbeat-state.json — single-writer heartbeat-process bookkeeping
//                          (last_heartbeat_ms / last_active_ms / continue_counts)
//   task-log.md          — ATC-maintained running log
//
// state.json and heartbeat-state.json are deliberately SEPARATE files with
// disjoint writers: the ATC Claude session owns state.json, the heartbeat
// process owns heartbeat-state.json. This avoids the lost-update race that
// existed when both writers performed unlocked read-modify-write on one file.
// The home root honours `AINB_HOME` so tests can isolate to a tempdir.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Resolved paths for one ATC instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtcPaths {
    /// `~/.agents-in-a-box/atc/<name>/`
    pub dir: PathBuf,
    pub meta: PathBuf,
    pub claude_md: PathBuf,
    pub state: PathBuf,
    /// Heartbeat-process-owned bookkeeping (separate writer from `state`).
    pub heartbeat_state: PathBuf,
    pub task_log: PathBuf,
}

impl AtcPaths {
    /// Build the layout for `name` under the given ATC root.
    #[must_use]
    pub fn under_root(root: &Path, name: &str) -> Self {
        let dir = root.join(name);
        Self {
            meta: dir.join("meta.json"),
            claude_md: dir.join("CLAUDE.md"),
            state: dir.join("state.json"),
            heartbeat_state: dir.join("heartbeat-state.json"),
            task_log: dir.join("task-log.md"),
            dir,
        }
    }

    /// Build the layout for `name` under the resolved default ATC root.
    pub fn resolve(name: &str) -> Result<Self> {
        Ok(Self::under_root(&atc_root()?, name))
    }
}

/// The ainb home directory: `$AINB_HOME` if set, else `~/.agents-in-a-box`.
pub fn ainb_home() -> Result<PathBuf> {
    if let Ok(h) = std::env::var("AINB_HOME") {
        if !h.is_empty() {
            return Ok(PathBuf::from(h));
        }
    }
    let home = dirs::home_dir().context("could not resolve home directory")?;
    Ok(home.join(".agents-in-a-box"))
}

/// The ATC instances root: `<ainb_home>/atc`.
pub fn atc_root() -> Result<PathBuf> {
    Ok(ainb_home()?.join("atc"))
}

/// Enumerate the names of all provisioned ATC instances (dirs containing a
/// `meta.json`). Missing root → empty list, never an error.
pub fn list_instance_names() -> Result<Vec<String>> {
    Ok(list_instance_names_in(&atc_root()?))
}

/// Pure variant of [`list_instance_names`] over an explicit root — the I/O is a
/// directory scan with no global state, so it is unit-testable against a tempdir.
#[must_use]
pub fn list_instance_names_in(root: &Path) -> Vec<String> {
    let mut names = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return names;
    };
    for entry in entries.flatten() {
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        if entry.path().join("meta.json").is_file() {
            if let Some(name) = entry.file_name().to_str() {
                names.push(name.to_string());
            }
        }
    }
    names.sort();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_nests_files_under_named_dir() {
        let root = Path::new("/tmp/x/atc");
        let p = AtcPaths::under_root(root, "tower");
        assert_eq!(p.dir, root.join("tower"));
        assert_eq!(p.meta, root.join("tower/meta.json"));
        assert_eq!(p.claude_md, root.join("tower/CLAUDE.md"));
        assert_eq!(p.state, root.join("tower/state.json"));
        assert_eq!(p.heartbeat_state, root.join("tower/heartbeat-state.json"));
        assert_eq!(p.task_log, root.join("tower/task-log.md"));
    }

    #[test]
    fn atc_root_is_home_plus_atc() {
        // Pure relationship: atc_root == ainb_home / "atc" regardless of how
        // the home is resolved. No global env mutation.
        let home = ainb_home().unwrap();
        assert_eq!(atc_root().unwrap(), home.join("atc"));
    }

    #[test]
    fn list_names_finds_only_dirs_with_meta() {
        let tmp = std::env::temp_dir().join(format!("atc-list-{}", std::process::id()));
        let atc = tmp.join("atc");
        std::fs::create_dir_all(atc.join("real")).unwrap();
        std::fs::write(atc.join("real/meta.json"), "{}").unwrap();
        // A dir without meta.json must be excluded.
        std::fs::create_dir_all(atc.join("empty")).unwrap();
        // A stray file (not a dir) must be excluded.
        std::fs::write(atc.join("stray.txt"), "x").unwrap();

        let names = list_instance_names_in(&atc);
        let _ = std::fs::remove_dir_all(&tmp);

        assert_eq!(names, vec!["real".to_string()]);
    }

    #[test]
    fn list_names_empty_when_root_missing() {
        let missing = std::env::temp_dir().join("atc-does-not-exist-xyz-12345");
        assert!(list_instance_names_in(&missing).is_empty());
    }
}
