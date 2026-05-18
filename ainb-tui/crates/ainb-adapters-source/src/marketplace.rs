//! Marketplace adapter — reads `.claude-plugin/marketplace.json`.
//!
//! Claude's marketplace format is either:
//!
//! ```json
//! { "plugins": [ {"name": "...", "description": "...", "source": {...}}, ... ] }
//! ```
//!
//! or just a bare array of the same entries. Both shapes round-trip
//! through this adapter as a `kind=plugin` UnitDescriptor list.

use std::fs;
use std::path::Path;

use serde_yaml_ng::Value as YamlValue;

use crate::types::{ResolvedUnit, UnitDescriptor};
use crate::SourceAdapter;

const MANIFEST_REL: &str = ".claude-plugin/marketplace.json";

pub struct MarketplaceAdapter;

impl MarketplaceAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MarketplaceAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl SourceAdapter for MarketplaceAdapter {
    fn name(&self) -> &'static str {
        "marketplace"
    }

    fn detect(&self, fetched_root: &Path) -> bool {
        fetched_root.join(MANIFEST_REL).is_file()
    }

    fn list_units(&self, fetched_root: &Path) -> anyhow::Result<Vec<UnitDescriptor>> {
        let path = fetched_root.join(MANIFEST_REL);
        let body = fs::read_to_string(&path)?;
        let parsed: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| anyhow::anyhow!("parsing {}: {e}", path.display()))?;

        let entries = match &parsed {
            serde_json::Value::Array(arr) => arr.clone(),
            serde_json::Value::Object(map) => match map.get("plugins") {
                Some(serde_json::Value::Array(arr)) => arr.clone(),
                _ => Vec::new(),
            },
            _ => Vec::new(),
        };

        let mut out = Vec::with_capacity(entries.len());
        for entry in entries {
            let name = entry
                .get("name")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let Some(name) = name else { continue };
            let description = entry
                .get("description")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            // Relative path inside the source — if the entry encodes
            // its own location use that, otherwise default to the
            // plugin name.
            let path = entry
                .get("source")
                .and_then(|s| s.get("path"))
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| format!("plugins/{name}"));
            out.push(UnitDescriptor {
                name,
                kind: "plugin".into(),
                description,
                path,
                tags: Vec::new(),
                requires: Vec::new(),
            });
        }
        Ok(out)
    }

    fn resolve_unit(
        &self,
        fetched_root: &Path,
        path: &str,
    ) -> anyhow::Result<ResolvedUnit> {
        let units = self.list_units(fetched_root)?;
        let descriptor = units
            .into_iter()
            .find(|u| u.path == path)
            .ok_or_else(|| anyhow::anyhow!("no marketplace entry at path `{path}`"))?;
        let file_list = crate::walk::collect_files(fetched_root, path);
        Ok(ResolvedUnit {
            descriptor,
            metadata: YamlValue::Null,
            source_root: fetched_root.to_path_buf(),
            file_list,
        })
    }
}
