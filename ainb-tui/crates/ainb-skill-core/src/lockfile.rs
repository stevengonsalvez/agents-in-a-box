//! Lockfile (`~/.agents-in-a-box/lock.yaml`) — spec §6.3.
//!
//! Owned by `ainb`. Records resolved SHAs and on-disk file hashes so
//! reinstall / staleness checks are reproducible. Writes are atomic
//! (`tmp` + rename), missing file loads as empty default.
//!
//! The `DeployedRef::status` tag is explicit so deployed / skipped /
//! pending-uninstall states round-trip cleanly via serde. P1 only
//! exercises the `pending_uninstall` transition (set by `source remove`);
//! `deployed` / `skipped` will be written by P2's install flow.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result};

const CURRENT_SCHEMA_VERSION: u32 = 1;

fn current_schema_version() -> u32 {
    CURRENT_SCHEMA_VERSION
}

/// Locked source — records the SHA we resolved a declared ref to and
/// the on-disk path the source materialized at (under
/// `$AINB_HOME/cache/` for remote sources, the user-supplied path for
/// `local:` sources).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockedSource {
    pub name: String,
    pub uri: String,
    pub declared_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetched_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetched_path: Option<String>,
}

/// Deployment state of one unit on one tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum DeployedRef {
    /// Successfully deployed; file hashes anchor staleness checks.
    Deployed {
        path: String,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        file_hashes: BTreeMap<String, String>,
    },
    /// Adapter declined the unit kind (`accepts()` returned `No`).
    Skipped { reason: String },
    /// Flagged for removal on the next `skill sync` run.
    PendingUninstall,
}

/// Locked unit — full deployment record across all targeted tools.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockedUnit {
    /// Fully-resolved URI (SHA-pinned).
    pub uri: String,
    /// Original URI from manifest (ref name, not SHA).
    pub declared_uri: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub deployed: BTreeMap<String, DeployedRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lockfile {
    #[serde(default = "current_schema_version")]
    pub schema_version: u32,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated_at: Option<String>,

    #[serde(default)]
    pub sources: Vec<LockedSource>,

    #[serde(default)]
    pub units: Vec<LockedUnit>,
}

impl Default for Lockfile {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            generated_at: None,
            sources: Vec::new(),
            units: Vec::new(),
        }
    }
}

impl Lockfile {
    /// Load from the default location ([`crate::paths::default_lockfile_path`]).
    /// Missing file yields an empty default lockfile.
    pub fn load() -> Result<Self> {
        Self::load_from(&crate::paths::default_lockfile_path())
    }

    /// Load from an explicit path. Missing file yields default.
    pub fn load_from(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let bytes = fs::read(path)?;
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(Self::default());
        }
        serde_yaml_ng::from_slice(&bytes).map_err(|e| CoreError::InvalidLockfile {
            path: path.to_path_buf(),
            message: e.to_string(),
        })
    }

    /// Save to the default location.
    pub fn save(&self) -> Result<()> {
        self.save_to(&crate::paths::default_lockfile_path())
    }

    /// Save atomically. Parents are created; write goes to `<path>.tmp`
    /// then rename into place.
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

    /// Flag every unit whose `declared_uri` starts with
    /// `<source_uri_prefix>@` as pending uninstall on every tool it was
    /// deployed to. Returns the count of affected units.
    pub fn mark_units_pending_uninstall_by_source_uri(&mut self, source_uri_prefix: &str) -> usize {
        let needle = format!("{source_uri_prefix}@");
        let mut affected = 0;
        for unit in &mut self.units {
            if unit.declared_uri.starts_with(&needle) {
                for dref in unit.deployed.values_mut() {
                    *dref = DeployedRef::PendingUninstall;
                }
                affected += 1;
            }
        }
        affected
    }
}

fn tmp_path_for(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".tmp");
    PathBuf::from(s)
}
