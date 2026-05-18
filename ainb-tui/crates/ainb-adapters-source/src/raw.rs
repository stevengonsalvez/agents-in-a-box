//! Raw-convention source adapter.
//!
//! For repos that just dump units in well-known directories without
//! a manifest. Scans:
//!
//! | Path                          | Kind        |
//! |-------------------------------|-------------|
//! | `skills/<name>/SKILL.md`      | skill       |
//! | `plugins/<name>/plugin.json`  | plugin      |
//! | `agents/<name>.md`            | agent       |
//! | `commands/<name>.md`          | command     |
//! | `hooks/<name>/hook.yaml`      | hook        |
//! | `mcp-servers.yaml` (entries)  | mcp-server  |
//! | `statuslines.yaml` (entries)  | statusline  |
//!
//! This adapter is the "last resort" detect — it returns true for any
//! directory, so detection callers should try `marketplace` and
//! `manifest` first and fall back to `raw` only when neither matches.

use std::fs;
use std::path::Path;

use serde_yaml_ng::Value as YamlValue;

use crate::frontmatter;
use crate::types::UnitDescriptor;
use crate::SourceAdapter;

pub struct RawAdapter;

impl RawAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for RawAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl SourceAdapter for RawAdapter {
    fn name(&self) -> &'static str {
        "raw"
    }

    fn detect(&self, fetched_root: &Path) -> bool {
        if !fetched_root.is_dir() {
            return false;
        }
        for d in ["skills", "plugins", "agents", "commands", "hooks"] {
            if fetched_root.join(d).is_dir() {
                return true;
            }
        }
        for f in ["mcp-servers.yaml", "statuslines.yaml"] {
            if fetched_root.join(f).is_file() {
                return true;
            }
        }
        false
    }

    fn list_units(&self, fetched_root: &Path) -> anyhow::Result<Vec<UnitDescriptor>> {
        let mut out = Vec::new();
        scan_skills(fetched_root, &mut out)?;
        scan_plugins(fetched_root, &mut out)?;
        scan_flat_md(fetched_root, "agents", "agent", &mut out)?;
        scan_flat_md(fetched_root, "commands", "command", &mut out)?;
        scan_hooks(fetched_root, &mut out)?;
        scan_yaml_list(fetched_root, "mcp-servers.yaml", "mcp-server", &mut out)?;
        scan_yaml_list(fetched_root, "statuslines.yaml", "statusline", &mut out)?;
        Ok(out)
    }

    fn resolve_unit(
        &self,
        fetched_root: &Path,
        path: &str,
    ) -> anyhow::Result<crate::types::ResolvedUnit> {
        let units = self.list_units(fetched_root)?;
        let descriptor = units
            .into_iter()
            .find(|u| u.path == path)
            .ok_or_else(|| anyhow::anyhow!("no unit at path `{path}` in raw source"))?;
        let file_list = crate::walk::collect_files(fetched_root, path);
        Ok(crate::types::ResolvedUnit {
            descriptor,
            metadata: YamlValue::Null,
            source_root: fetched_root.to_path_buf(),
            file_list,
        })
    }
}

fn scan_skills(root: &Path, out: &mut Vec<UnitDescriptor>) -> anyhow::Result<()> {
    let dir = root.join("skills");
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let p = entry.path();
        if !p.is_dir() {
            continue;
        }
        let skill_md = p.join("SKILL.md");
        if !skill_md.is_file() {
            continue;
        }
        let dir_name = entry.file_name().to_string_lossy().to_string();
        let body = fs::read_to_string(&skill_md).unwrap_or_default();
        let (meta, _) = frontmatter::parse(&body);
        let name = frontmatter::str_field(&meta, "name")
            .map(str::to_string)
            .unwrap_or_else(|| dir_name.clone());
        let description = frontmatter::str_field(&meta, "description").map(str::to_string);
        let tags = frontmatter::str_list_field(&meta, "tags");
        let requires = frontmatter::str_list_field(&meta, "requires");

        out.push(UnitDescriptor {
            name,
            kind: "skill".into(),
            description,
            path: format!("skills/{dir_name}"),
            tags,
            requires,
        });
    }
    Ok(())
}

fn scan_plugins(root: &Path, out: &mut Vec<UnitDescriptor>) -> anyhow::Result<()> {
    let dir = root.join("plugins");
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let p = entry.path();
        if !p.is_dir() {
            continue;
        }
        let plugin_json = p.join("plugin.json");
        if !plugin_json.is_file() {
            continue;
        }
        let dir_name = entry.file_name().to_string_lossy().to_string();
        let body = fs::read_to_string(&plugin_json).unwrap_or_default();
        let json: serde_json::Value = serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
        let name = json
            .get("name")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| dir_name.clone());
        let description = json
            .get("description")
            .and_then(|v| v.as_str())
            .map(str::to_string);

        out.push(UnitDescriptor {
            name,
            kind: "plugin".into(),
            description,
            path: format!("plugins/{dir_name}"),
            tags: Vec::new(),
            requires: Vec::new(),
        });
    }
    Ok(())
}

fn scan_flat_md(
    root: &Path,
    subdir: &str,
    kind: &str,
    out: &mut Vec<UnitDescriptor>,
) -> anyhow::Result<()> {
    let dir = root.join(subdir);
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let p = entry.path();
        if p.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let stem = p
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        let body = fs::read_to_string(&p).unwrap_or_default();
        let (meta, _) = frontmatter::parse(&body);
        let name = frontmatter::str_field(&meta, "name")
            .map(str::to_string)
            .unwrap_or_else(|| stem.clone());
        let description = frontmatter::str_field(&meta, "description").map(str::to_string);
        let tags = frontmatter::str_list_field(&meta, "tags");
        out.push(UnitDescriptor {
            name,
            kind: kind.to_string(),
            description,
            path: format!("{subdir}/{stem}.md"),
            tags,
            requires: Vec::new(),
        });
    }
    Ok(())
}

fn scan_hooks(root: &Path, out: &mut Vec<UnitDescriptor>) -> anyhow::Result<()> {
    let dir = root.join("hooks");
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let p = entry.path();
        if !p.is_dir() {
            continue;
        }
        let hook_yaml = p.join("hook.yaml");
        if !hook_yaml.is_file() {
            continue;
        }
        let dir_name = entry.file_name().to_string_lossy().to_string();
        let body = fs::read_to_string(&hook_yaml).unwrap_or_default();
        let meta: YamlValue = serde_yaml_ng::from_str(&body).unwrap_or(YamlValue::Null);
        let name = meta
            .as_mapping()
            .and_then(|m| m.get(YamlValue::String("name".into())))
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| dir_name.clone());
        let description = meta
            .as_mapping()
            .and_then(|m| m.get(YamlValue::String("description".into())))
            .and_then(|v| v.as_str())
            .map(str::to_string);
        out.push(UnitDescriptor {
            name,
            kind: "hook".into(),
            description,
            path: format!("hooks/{dir_name}"),
            tags: Vec::new(),
            requires: Vec::new(),
        });
    }
    Ok(())
}

fn scan_yaml_list(
    root: &Path,
    filename: &str,
    kind: &str,
    out: &mut Vec<UnitDescriptor>,
) -> anyhow::Result<()> {
    let f = root.join(filename);
    if !f.is_file() {
        return Ok(());
    }
    let body = fs::read_to_string(&f)?;
    let v: YamlValue = match serde_yaml_ng::from_str(&body) {
        Ok(v) => v,
        Err(_) => return Ok(()),
    };
    let entries = match v.as_sequence() {
        Some(s) => s.clone(),
        None => return Ok(()),
    };
    for entry in entries {
        let map = match entry.as_mapping() {
            Some(m) => m,
            None => continue,
        };
        let name = map
            .get(YamlValue::String("name".into()))
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| filename.to_string());
        let description = map
            .get(YamlValue::String("description".into()))
            .and_then(|v| v.as_str())
            .map(str::to_string);
        out.push(UnitDescriptor {
            name,
            kind: kind.to_string(),
            description,
            path: filename.to_string(),
            tags: Vec::new(),
            requires: Vec::new(),
        });
    }
    Ok(())
}
