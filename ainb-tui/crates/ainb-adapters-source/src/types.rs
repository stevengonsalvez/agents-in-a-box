//! Shared types for source-type adapters.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Lightweight per-unit record returned by `list_units`. Carries only
/// the fields the CLI needs to display search results + decide whether
/// to install.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnitDescriptor {
    pub name: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Path relative to the fetched source root.
    pub path: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<String>,
}

/// Full resolution of a unit — descriptor plus its metadata blob, the
/// absolute path of the fetched source root, and the list of files
/// (relative to that root) comprising the unit. `source_root` plus a
/// relative path from `file_list` gives the absolute path to read,
/// and `install_root.join(relative)` gives the destination the tool
/// adapter should write to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolvedUnit {
    pub descriptor: UnitDescriptor,
    #[serde(default)]
    pub metadata: serde_yaml_ng::Value,
    /// Absolute path of the fetched source root.
    #[serde(default)]
    pub source_root: PathBuf,
    /// Files comprising this unit, relative to [`Self::source_root`].
    #[serde(default)]
    pub file_list: Vec<PathBuf>,
}
