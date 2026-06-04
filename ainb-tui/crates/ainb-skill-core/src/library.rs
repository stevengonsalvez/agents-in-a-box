//! Own-skill library (`~/.agents-in-a-box/library.yaml`) — bead ai-lgk.
//!
//! A first-class home for skills the user authored locally. State lives
//! in YAML (sibling to `manifest.yaml` / `lock.yaml`) — **no SQLite, no
//! embedded store**. Pure load/save mirror the [`crate::manifest`] /
//! [`crate::lockfile`] shape: missing file loads as an empty default,
//! writes are atomic (`tmp` + rename) so a partial write never replaces
//! the live file.
//!
//! ```text
//! ~/.agents-in-a-box/library.yaml
//! ─────────────────────────────────────
//! schema_version: 1
//! owned:
//!   - name: my-skill
//!     kind: skill
//!     path: .claude/skills/my-skill        # tool-home-relative
//!     created: "2026-06-02T..."
//!     promoted_uri: null                    # set after `promote`
//! ```

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result};
use crate::kind::UnitKind;

/// Current on-disk library schema version.
pub const LIBRARY_SCHEMA_VERSION: u32 = 1;

fn current_schema_version() -> u32 {
    LIBRARY_SCHEMA_VERSION
}

/// One user-authored ("owned") unit tracked in `library.yaml`.
///
/// `path` is **tool-home-relative** (e.g. `.claude/skills/my-skill`) so
/// the entry stays portable across machines — the TUI / CLI join it onto
/// the resolved tool home when it needs an absolute path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnedUnit {
    /// Canonical unit name (the trailing folder / file stem).
    pub name: String,

    /// Unit kind — `skill`, `command`, `agent`, … Stored as the wire
    /// string via [`UnitKind`]'s serde, so unknown kinds are rejected at
    /// load (same contract as the manifest).
    pub kind: UnitKind,

    /// Tool-home-relative on-disk path (e.g. `.claude/skills/my-skill`).
    pub path: String,

    /// RFC 3339 creation timestamp. Stored as a string to match the
    /// lockfile's timestamp convention (`fetched_at`, `generated_at`).
    pub created: String,

    /// `gh:` / `git:` URI the unit was promoted to, or `None` until the
    /// user runs `ainb skill promote`. Round-trips losslessly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub promoted_uri: Option<String>,
}

/// Top-level `library.yaml` type — the registry of owned units.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Library {
    #[serde(default = "current_schema_version")]
    pub schema_version: u32,

    #[serde(default)]
    pub owned: Vec<OwnedUnit>,
}

impl Default for Library {
    fn default() -> Self {
        Self {
            schema_version: LIBRARY_SCHEMA_VERSION,
            owned: Vec::new(),
        }
    }
}

impl Library {
    /// Load from an explicit path. Missing / whitespace-only file yields
    /// a default (empty) library so callers can `load → register → save`
    /// without a pre-existing file.
    pub fn load_from(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let bytes = fs::read(path)?;
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(Self::default());
        }
        serde_yaml_ng::from_slice(&bytes).map_err(|e| CoreError::InvalidLibrary {
            path: path.to_path_buf(),
            message: e.to_string(),
        })
    }

    /// Save atomically to an explicit path. Parents are created as
    /// needed; the write goes to a sibling `<path>.tmp` and is renamed
    /// into place so a crash mid-write never leaves a truncated file.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        let yaml = serde_yaml_ng::to_string(self)?;
        let tmp = tmp_path_for(path);
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(yaml.as_bytes())?;
            f.sync_all()?;
        }
        fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Register an owned unit, dedup-ing by name. When the name is new,
    /// the entry is appended and `true` is returned. When the name
    /// already exists, the existing row is overwritten in place
    /// (latest-wins, keeps repeat `add` idempotent) and `false` is
    /// returned so callers can distinguish insert from update.
    pub fn register(&mut self, unit: OwnedUnit) -> bool {
        if let Some(existing) = self.owned.iter_mut().find(|u| u.name == unit.name) {
            *existing = unit;
            false
        } else {
            self.owned.push(unit);
            true
        }
    }

    /// Look up an owned unit by name.
    pub fn get(&self, name: &str) -> Option<&OwnedUnit> {
        self.owned.iter().find(|u| u.name == name)
    }
}

/// `library.yaml` path for an explicit ainb home directory — sibling to
/// `manifest.yaml` / `lock.yaml`.
pub fn library_path_in(home: &Path) -> PathBuf {
    home.join("library.yaml")
}

/// Default `library.yaml` path (uses [`crate::paths::default_ainb_home`]).
pub fn default_library_path() -> PathBuf {
    library_path_in(&crate::paths::default_ainb_home())
}

fn tmp_path_for(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".tmp");
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_path_is_sibling_of_manifest() {
        let home = Path::new("/tmp/fake-ainb-home");
        assert_eq!(
            library_path_in(home),
            PathBuf::from("/tmp/fake-ainb-home/library.yaml")
        );
    }

    #[test]
    fn register_then_get_round_trips() {
        let mut lib = Library::default();
        assert!(lib.register(OwnedUnit {
            name: "x".into(),
            kind: UnitKind::Skill,
            path: ".claude/skills/x".into(),
            created: "2026-06-02T00:00:00+00:00".into(),
            promoted_uri: None,
        }));
        assert_eq!(lib.get("x").map(|u| u.name.as_str()), Some("x"));
    }
}
