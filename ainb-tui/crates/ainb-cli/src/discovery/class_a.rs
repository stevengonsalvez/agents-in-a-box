//! Class-A walker: enumerates Claude Code marketplace plugins from the
//! local plugin cache.
//!
//! Layout walked (claude_home = `~/.claude` by default):
//! ```text
//! <claude_home>/
//! ├── plugins/
//! │   ├── known_marketplaces.json           # registry (optional)
//! │   └── cache/
//! │       └── <marketplace>/
//! │           └── <plugin>/
//! │               └── <version>/
//! │                   ├── .claude-plugin/plugin.json   # required
//! │                   ├── skills/<name>/SKILL.md       # 0..n
//! │                   └── agents/<name>.md             # 0..n
//! ```
//!
//! Contract:
//! - **Pure function.** No filesystem writes anywhere in this module.
//! - **Best-effort.** A missing or malformed file at any level is
//!   silently skipped; the walker never panics and never aborts a run
//!   because one plugin is broken.
//! - **O(n) file reads.** Each plugin's `.claude-plugin/plugin.json`
//!   is opened at most once; `skills/` and `agents/` are listed via a
//!   single `read_dir` each.
//! - **Marketplace `unknown` when registry missing.** When
//!   `known_marketplaces.json` is absent the walker still discovers
//!   plugins under `cache/`, but every entry's `marketplace` field is
//!   the literal string `"unknown"`. When the registry exists, the
//!   directory name is used as the marketplace identifier.
//!
//! Output shape is the input to the P3 reconciler, which assembles
//! `marketplace:<plugin>@<marketplace>` URIs and emits manifest
//! patches.

use std::fs;
use std::path::{Path, PathBuf};

/// One marketplace plugin found in the cache, with every skill/agent
/// it bundles enumerated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredMarketplaceUnit {
    /// Plugin slug — taken from the directory name under the
    /// marketplace. Mirrors the `<plugin>` token in
    /// `/plugin install <plugin>@<marketplace>`.
    pub plugin: String,
    /// Marketplace slug, or the literal `"unknown"` when
    /// `known_marketplaces.json` is missing.
    pub marketplace: String,
    /// Version directory name. Per Claude Code's layout the cache is
    /// keyed by version, so the directory name is the canonical
    /// version string for this walker's purposes.
    pub version: String,
    /// Skills + agents shipped by this plugin. Order is filesystem-
    /// dependent; callers should not rely on a particular ordering.
    pub units: Vec<DiscoveredUnit>,
}

/// A single skill or agent shipped by a marketplace plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredUnit {
    pub kind: DiscoveredUnitKind,
    /// Display name — directory name for skills, file stem for agents.
    /// Frontmatter parsing is deferred to the P1 class-C walker; here
    /// the on-disk name is authoritative.
    pub name: String,
    /// Absolute path to the unit's primary file:
    /// `<plugin>/<ver>/skills/<name>/SKILL.md` for skills,
    /// `<plugin>/<ver>/agents/<name>.md` for agents.
    pub path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveredUnitKind {
    Skill,
    Agent,
}

/// Walk `<claude_home>/plugins/cache/` and return every marketplace
/// plugin found. `claude_home` is normally `~/.claude`; tests pass a
/// tempdir.
///
/// See module docs for the read contract. Never writes, never panics.
pub fn walk(claude_home: &Path) -> Vec<DiscoveredMarketplaceUnit> {
    let plugins_root = claude_home.join("plugins");
    let cache_root = plugins_root.join("cache");
    if !cache_root.is_dir() {
        return Vec::new();
    }

    let registry_present = plugins_root.join("known_marketplaces.json").is_file();

    let mut out = Vec::new();
    let Ok(mp_iter) = fs::read_dir(&cache_root) else {
        return out;
    };
    for mp_entry in mp_iter.flatten() {
        let mp_path = mp_entry.path();
        if !mp_path.is_dir() {
            continue;
        }
        let mp_name = match mp_path.file_name().and_then(|s| s.to_str()) {
            Some(s) => s.to_owned(),
            None => continue,
        };
        let marketplace_label = if registry_present {
            mp_name.clone()
        } else {
            "unknown".to_owned()
        };

        let Ok(plugin_iter) = fs::read_dir(&mp_path) else {
            continue;
        };
        for plugin_entry in plugin_iter.flatten() {
            let plugin_path = plugin_entry.path();
            if !plugin_path.is_dir() {
                continue;
            }
            let plugin_name = match plugin_path.file_name().and_then(|s| s.to_str()) {
                Some(s) => s.to_owned(),
                None => continue,
            };

            let Ok(version_iter) = fs::read_dir(&plugin_path) else {
                continue;
            };
            for version_entry in version_iter.flatten() {
                let version_path = version_entry.path();
                if !version_path.is_dir() {
                    continue;
                }
                let version = match version_path.file_name().and_then(|s| s.to_str()) {
                    Some(s) => s.to_owned(),
                    None => continue,
                };

                // Skip dirs without a plugin.json — they are not a
                // Claude Code plugin install (could be a stale temp
                // or unrelated cache content).
                let manifest_path = version_path.join(".claude-plugin").join("plugin.json");
                if !manifest_path.is_file() {
                    continue;
                }

                let mut units = Vec::new();
                collect_skills(&version_path.join("skills"), &mut units);
                collect_agents(&version_path.join("agents"), &mut units);

                out.push(DiscoveredMarketplaceUnit {
                    plugin: plugin_name.clone(),
                    marketplace: marketplace_label.clone(),
                    version,
                    units,
                });
            }
        }
    }
    out
}

/// Enumerate `<skills_dir>/<name>/SKILL.md`. A missing dir or any
/// individual broken entry is silently skipped (best-effort).
fn collect_skills(skills_dir: &Path, out: &mut Vec<DiscoveredUnit>) {
    let Ok(iter) = fs::read_dir(skills_dir) else {
        return;
    };
    for entry in iter.flatten() {
        let p = entry.path();
        if !p.is_dir() {
            continue;
        }
        let name = match p.file_name().and_then(|s| s.to_str()) {
            Some(s) => s.to_owned(),
            None => continue,
        };
        let skill_md = p.join("SKILL.md");
        if !skill_md.is_file() {
            continue;
        }
        out.push(DiscoveredUnit {
            kind: DiscoveredUnitKind::Skill,
            name,
            path: skill_md,
        });
    }
}

/// Enumerate `<agents_dir>/<name>.md`. Claude Code agents live as a
/// single Markdown file per agent (no subdirectory). Skips
/// non-`.md` and any IO errors silently.
fn collect_agents(agents_dir: &Path, out: &mut Vec<DiscoveredUnit>) {
    let Ok(iter) = fs::read_dir(agents_dir) else {
        return;
    };
    for entry in iter.flatten() {
        let p = entry.path();
        if !p.is_file() {
            continue;
        }
        // Agent files are `.md`; skip anything else (e.g. README.txt).
        let ext_ok = p.extension().and_then(|s| s.to_str()) == Some("md");
        if !ext_ok {
            continue;
        }
        let name = match p.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s.to_owned(),
            None => continue,
        };
        out.push(DiscoveredUnit {
            kind: DiscoveredUnitKind::Agent,
            name,
            path: p,
        });
    }
}
