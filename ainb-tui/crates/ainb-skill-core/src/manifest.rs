//! Manifest (`~/.agents-in-a-box/manifest.yaml`) — spec §6.2.
//!
//! User-edited declarative file. `ainb` reads it, never owns the intent.
//! Atomic save: write to a sibling `.tmp` and rename so partial writes
//! never replace the live file.

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

fn default_true() -> bool {
    true
}

fn default_ref() -> String {
    "main".to_string()
}

/// One configured source. The `uri` is the source-level identifier
/// (e.g. `gh:org/repo`) — the `@ref/path` suffix is held separately so
/// users can re-pin without rewriting the URI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceEntry {
    pub name: String,

    /// Source kind hint (marketplace | manifest | raw | single). Optional;
    /// adapter auto-detection in P2 will fill it in when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "type")]
    pub kind: Option<String>,

    /// Source URI with no `@ref/path` (e.g. `gh:org/repo`).
    pub uri: String,

    /// Git ref to track — branch, tag, or SHA. Defaults to `main`.
    #[serde(default = "default_ref")]
    pub r#ref: String,

    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// One declared unit, by full URI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnitEntry {
    pub uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub targets: Option<Vec<String>>,
}

/// Manifest-level defaults applied when a unit omits a field.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Defaults {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub targets: Option<Vec<String>>,
}

/// Free-form options block; serialised as a YAML mapping.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Options {
    #[serde(flatten)]
    pub values: BTreeMap<String, serde_yaml_ng::Value>,
}

/// Top-level manifest type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    #[serde(default = "current_schema_version")]
    pub schema_version: u32,

    #[serde(default)]
    pub sources: Vec<SourceEntry>,

    #[serde(default)]
    pub units: Vec<UnitEntry>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defaults: Option<Defaults>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Options>,
}

impl Default for Manifest {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            sources: Vec::new(),
            units: Vec::new(),
            defaults: None,
            options: None,
        }
    }
}

impl Manifest {
    /// Load from the default location ([`crate::paths::default_manifest_path`]).
    /// Missing file yields an empty default manifest.
    pub fn load() -> Result<Self> {
        Self::load_from(&crate::paths::default_manifest_path())
    }

    /// Load from an explicit path. Missing file yields a default manifest.
    pub fn load_from(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let bytes = fs::read(path)?;
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(Self::default());
        }
        serde_yaml_ng::from_slice(&bytes).map_err(|e| CoreError::InvalidManifest {
            path: path.to_path_buf(),
            message: e.to_string(),
        })
    }

    /// Save to the default location.
    pub fn save(&self) -> Result<()> {
        self.save_to(&crate::paths::default_manifest_path())
    }

    /// Save atomically to an explicit path. Parents are created as needed;
    /// the write goes to a sibling `<path>.tmp` and is renamed into place.
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

    /// Look up a source by name.
    pub fn source(&self, name: &str) -> Option<&SourceEntry> {
        self.sources.iter().find(|s| s.name == name)
    }

    /// Mutable look-up.
    pub fn source_mut(&mut self, name: &str) -> Option<&mut SourceEntry> {
        self.sources.iter_mut().find(|s| s.name == name)
    }

    /// Append a source; returns [`CoreError::SourceAlreadyExists`] if the
    /// name is taken.
    pub fn add_source(&mut self, entry: SourceEntry) -> Result<()> {
        if self.source(&entry.name).is_some() {
            return Err(CoreError::SourceAlreadyExists(entry.name));
        }
        self.sources.push(entry);
        Ok(())
    }

    /// Remove a source by name; returns it on success.
    pub fn remove_source(&mut self, name: &str) -> Result<SourceEntry> {
        let pos = self
            .sources
            .iter()
            .position(|s| s.name == name)
            .ok_or_else(|| CoreError::SourceNotFound(name.to_string()))?;
        Ok(self.sources.remove(pos))
    }
}

fn tmp_path_for(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".tmp");
    PathBuf::from(s)
}
