//! Single-file source adapter.
//!
//! Used for gists (typically one file in a cloned dir) and direct
//! https downloads (one file under the cache). Detects when the
//! fetched root is *itself* a file, or when it's a dir containing
//! exactly one non-hidden regular file.
//!
//! Kind detection:
//!   - Markdown with frontmatter `kind:` field — use that.
//!   - Markdown with frontmatter but no `kind` — default to `skill`.
//!   - `plugin.json` — kind=plugin.
//!   - Anything else — kind defaults to `skill` for `.md` files,
//!     else falls back to `skill` so the entry remains visible.

use std::fs;
use std::path::{Path, PathBuf};

use serde_yaml_ng::Value as YamlValue;

use crate::SourceAdapter;
use crate::frontmatter;
use crate::types::{ResolvedUnit, UnitDescriptor};

pub struct SingleAdapter;

impl SingleAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SingleAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl SourceAdapter for SingleAdapter {
    fn name(&self) -> &'static str {
        "single"
    }

    fn detect(&self, fetched_root: &Path) -> bool {
        sole_file(fetched_root).is_some()
    }

    fn list_units(&self, fetched_root: &Path) -> anyhow::Result<Vec<UnitDescriptor>> {
        let Some(file) = sole_file(fetched_root) else {
            return Ok(Vec::new());
        };
        Ok(vec![descriptor_for_file(fetched_root, &file)?])
    }

    fn resolve_unit(&self, fetched_root: &Path, path: &str) -> anyhow::Result<ResolvedUnit> {
        let units = self.list_units(fetched_root)?;
        let descriptor = units
            .into_iter()
            .find(|u| u.path == path)
            .ok_or_else(|| anyhow::anyhow!("no single-file unit at path `{path}`"))?;
        let file_list = vec![PathBuf::from(&descriptor.path)];
        Ok(ResolvedUnit {
            descriptor,
            metadata: YamlValue::Null,
            source_root: fetched_root.to_path_buf(),
            file_list,
        })
    }
}

/// Return the absolute path of the single fetched file, if the root
/// resolves to "exactly one file" (either directly, or as the only
/// non-hidden entry in a dir).
fn sole_file(root: &Path) -> Option<PathBuf> {
    if root.is_file() {
        return Some(root.to_path_buf());
    }
    if !root.is_dir() {
        return None;
    }
    let mut visible: Vec<PathBuf> = Vec::new();
    for entry in fs::read_dir(root).ok()? {
        let entry = entry.ok()?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        // Skip hidden / VCS metadata so a cloned gist with a `.git`
        // dir still counts as "one file".
        if name_str.starts_with('.') {
            continue;
        }
        let path = entry.path();
        if path.is_file() {
            visible.push(path);
        } else if path.is_dir() {
            return None;
        }
    }
    if visible.len() == 1 {
        Some(visible.into_iter().next().unwrap())
    } else {
        None
    }
}

fn descriptor_for_file(root: &Path, file: &Path) -> anyhow::Result<UnitDescriptor> {
    let rel = file
        .strip_prefix(root)
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|_| file.to_path_buf());
    let rel_str = rel.to_string_lossy().to_string();
    let stem = file.file_stem().and_then(|s| s.to_str()).unwrap_or("unit").to_string();
    let ext = file.extension().and_then(|e| e.to_str()).unwrap_or("");

    match ext {
        "md" | "markdown" => {
            let body = fs::read_to_string(file).unwrap_or_default();
            let (meta, _) = frontmatter::parse(&body);
            let kind = frontmatter::str_field(&meta, "kind")
                .map(str::to_string)
                .unwrap_or_else(|| "skill".to_string());
            let name = frontmatter::str_field(&meta, "name")
                .map(str::to_string)
                .unwrap_or(stem.clone());
            let description = frontmatter::str_field(&meta, "description").map(str::to_string);
            let tags = frontmatter::str_list_field(&meta, "tags");
            let requires = frontmatter::str_list_field(&meta, "requires");
            Ok(UnitDescriptor {
                name,
                kind,
                description,
                path: rel_str,
                tags,
                requires,
            })
        }
        "json" => {
            let body = fs::read_to_string(file).unwrap_or_default();
            let v: serde_json::Value =
                serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
            let name = v.get("name").and_then(|x| x.as_str()).map(str::to_string).unwrap_or(stem);
            let description = v.get("description").and_then(|x| x.as_str()).map(str::to_string);
            Ok(UnitDescriptor {
                name,
                kind: "plugin".into(),
                description,
                path: rel_str,
                tags: Vec::new(),
                requires: Vec::new(),
            })
        }
        _ => Ok(UnitDescriptor {
            name: stem,
            kind: "skill".into(),
            description: None,
            path: rel_str,
            tags: Vec::new(),
            requires: Vec::new(),
        }),
    }
}
