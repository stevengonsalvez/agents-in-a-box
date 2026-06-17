//! Manifest (`~/.agents-in-a-box/manifest.yaml`) — spec §6.2.
//!
//! User-edited declarative file. `ainb` reads it, never owns the intent.
//! Atomic save: write to a sibling `.tmp` and rename so partial writes
//! never replace the live file.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::fmt;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result};
use crate::uri::Uri;

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

fn is_false(b: &bool) -> bool {
    !b
}

/// Typed view over the free-form `SourceEntry.kind` string. Use
/// [`SourceEntry::kind_typed`] to access; the underlying field stays
/// `Option<String>` so unknown / forward-compat values round-trip
/// losslessly through [`SourceKind::Other`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceKind {
    Manifest,
    Raw,
    Gh,
    Single,
    Marketplace,
    /// Claude Code marketplace plugin source (read-only — ainb does not
    /// `install` / `update` / `uninstall`).
    ClaudeMarketplace,
    /// Catch-all for unrecognised kinds — preserves the raw string
    /// verbatim so v1 manifests round-trip without data loss.
    Other(String),
}

impl FromStr for SourceKind {
    type Err = Infallible;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Ok(match s {
            "manifest" => Self::Manifest,
            "raw" => Self::Raw,
            "gh" => Self::Gh,
            "single" => Self::Single,
            "marketplace" => Self::Marketplace,
            "claude-marketplace" => Self::ClaudeMarketplace,
            other => Self::Other(other.to_string()),
        })
    }
}

impl fmt::Display for SourceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Manifest => "manifest",
            Self::Raw => "raw",
            Self::Gh => "gh",
            Self::Single => "single",
            Self::Marketplace => "marketplace",
            Self::ClaudeMarketplace => "claude-marketplace",
            Self::Other(s) => s,
        })
    }
}

/// One `glob → (home, repo)` folder mapping within a source. Declares
/// where files matched by `glob` land: `home` is the path under the tool
/// home (e.g. `~/.claude`), `repo` is the path within the source repo.
/// Both are relative; the sync layer joins them onto the respective roots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetMapping {
    /// Glob matched against repo-relative paths (e.g. `skills/**`).
    pub glob: String,

    /// Destination relative to the tool home (e.g. `.claude/skills`).
    pub home: PathBuf,

    /// Source location relative to the repo root
    /// (e.g. `toolkit/packages/skills`).
    pub repo: PathBuf,
}

/// One configured source. The `uri` is the source-level identifier
/// (e.g. `gh:org/repo`) — the `@ref/path` suffix is held separately so
/// users can re-pin without rewriting the URI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceEntry {
    pub name: String,

    /// Source kind hint (manifest | raw | gh | single | marketplace |
    /// claude-marketplace). Optional; adapter auto-detection fills it
    /// in when omitted. Stored as a free-form string so unknown values
    /// round-trip — use [`SourceEntry::kind_typed`] for typed access.
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "type")]
    pub kind: Option<String>,

    /// Source URI with no `@ref/path` (e.g. `gh:org/repo`).
    pub uri: String,

    /// Git ref to track — branch, tag, or SHA. Defaults to `main`.
    #[serde(default = "default_ref")]
    pub r#ref: String,

    #[serde(default = "default_true")]
    pub enabled: bool,

    /// When `true`, `ainb` treats this source as externally managed and
    /// will not run install / update / uninstall against it (e.g. a
    /// Claude Code marketplace owned by `/plugin`). Defaults to `false`;
    /// v1 manifests written without this field load with the default.
    #[serde(default, skip_serializing_if = "is_false")]
    pub read_only: bool,

    /// Optional per-source folder mappings (`glob → home/repo`). Empty by
    /// default; legacy sources written without this field load with an
    /// empty vec and round-trip without emitting `target_layout`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub target_layout: Vec<TargetMapping>,
}

impl SourceEntry {
    /// Typed view of [`SourceEntry::kind`]. Returns `None` when no kind
    /// is set; unknown strings parse to [`SourceKind::Other`] so this
    /// never fails.
    pub fn kind_typed(&self) -> Option<SourceKind> {
        self.kind
            .as_deref()
            .map(|s| s.parse().expect("SourceKind::from_str is infallible"))
    }
}

/// One declared unit, by full URI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnitEntry {
    pub uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub targets: Option<Vec<String>>,

    /// When set, the unit is being shadowed by another unit (typically
    /// a marketplace plugin overriding an orphan with the same name).
    /// `ainb skill list` renders the shadowed row as inactive; `[s]`
    /// in the SkillManager TUI flips which side wins. Defaults to
    /// `None`; v1 manifests load with the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadowed_by: Option<Uri>,
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
