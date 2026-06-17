//! Source-side manifest adapter — reads `manifest.yaml` at the root of
//! a source repo. Distinct from the *user* manifest in `ainb-core`:
//! this one is the source author's explicit unit listing and lets
//! repos that don't fit the raw convention still publish discoverable
//! units.
//!
//! Wire format:
//!
//! ```yaml
//! schema_version: 1
//! units:
//!   - path: skills/foo
//!     kind: skill
//!     name: foo
//!     description: ...
//!     tags: [a, b]
//!     requires: [git]
//! ```

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_yaml_ng::Value as YamlValue;

use crate::SourceAdapter;
use crate::types::{ResolvedUnit, UnitDescriptor};

const MANIFEST_REL: &str = "manifest.yaml";

#[derive(Debug, Deserialize, Serialize)]
struct SourceManifest {
    #[serde(default)]
    units: Vec<UnitEntry>,
}

#[derive(Debug, Deserialize, Serialize)]
struct UnitEntry {
    path: String,
    kind: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    requires: Vec<String>,
}

pub struct ManifestAdapter;

impl ManifestAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ManifestAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl SourceAdapter for ManifestAdapter {
    fn name(&self) -> &'static str {
        "manifest"
    }

    fn detect(&self, fetched_root: &Path) -> bool {
        fetched_root.join(MANIFEST_REL).is_file()
    }

    fn list_units(&self, fetched_root: &Path) -> anyhow::Result<Vec<UnitDescriptor>> {
        let path = fetched_root.join(MANIFEST_REL);
        let body = fs::read_to_string(&path)?;
        let parsed: SourceManifest = serde_yaml_ng::from_str(&body)
            .map_err(|e| anyhow::anyhow!("parsing {}: {e}", path.display()))?;

        Ok(parsed
            .units
            .into_iter()
            .map(|u| {
                let name = u.name.unwrap_or_else(|| basename_of(&u.path));
                UnitDescriptor {
                    name,
                    kind: u.kind,
                    description: u.description,
                    path: u.path,
                    tags: u.tags,
                    requires: u.requires,
                }
            })
            .collect())
    }

    fn resolve_unit(&self, fetched_root: &Path, path: &str) -> anyhow::Result<ResolvedUnit> {
        let units = self.list_units(fetched_root)?;
        let descriptor = units
            .into_iter()
            .find(|u| u.path == path)
            .ok_or_else(|| anyhow::anyhow!("no manifest entry at path `{path}`"))?;
        let file_list = crate::walk::collect_files(fetched_root, path);
        Ok(ResolvedUnit {
            descriptor,
            metadata: YamlValue::Null,
            source_root: fetched_root.to_path_buf(),
            file_list,
        })
    }
}

fn basename_of(path: &str) -> String {
    path.rsplit_once('/').map(|(_, n)| n).unwrap_or(path).to_string()
}
