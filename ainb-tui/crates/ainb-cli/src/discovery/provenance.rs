//! Provenance matcher (bead `ai-ya9`, skill-manager v1.3).
//!
//! Attributes every discovered unit to its *real* source by joining
//! the cheap on-disk walker outputs (class-A marketplace plugins +
//! class-C orphans) against three parsed manifests:
//!
//! - Claude Code's `~/.claude/plugins/installed_plugins.json`
//!   (git-commit SHA + install scope per marketplace plugin),
//! - the bootstrap-era `external-dependencies.yaml`
//!   (`bundled-skills` → toolkit, `agent-skills` → external git repo),
//! - the ainb `manifest.yaml` (units already adopted).
//!
//! ## Contract
//!
//! - **NO SQLite, no DB.** Inputs are parsed structs; output is a
//!   `Vec<AttributedUnit>`. State stays in YAML / JSON files.
//! - **Pure / total / deterministic.** [`attribute`] never reads the
//!   filesystem, never panics, and returns the same output for the
//!   same input. The (impure) parsing of the three manifests is done
//!   up front by the [`parse_*`] helpers + the caller; `attribute`
//!   only sees already-parsed data.
//! - **Best-effort parsing.** A malformed JSON / YAML manifest yields
//!   an empty parsed view rather than an error — discovery degrades
//!   gracefully (a missing `installed_plugins.json` just means no SHA
//!   is joined, not a hard failure).
//!
//! ## Attribution precedence
//!
//! For each discovered unit, the first matching rule wins:
//!
//! 1. **Marketplace** — class-A units always carry a marketplace
//!    provenance (the SHA + scope are joined from
//!    `installed_plugins.json` when present, `None` otherwise).
//! 2. **AlreadyAdopted** — the unit name matches a `manifest.yaml`
//!    UnitEntry → carry that URI.
//! 3. **Toolkit** — the name matches a `bundled-skills[].name`.
//! 4. **ExternalRepo** — the name matches an `agent-skills[].name`.
//! 5. **Local** — no match; a genuinely hand-authored unit.
//!
//! Toolkit is checked before ExternalRepo because a bundled toolkit
//! skill is "more owned" than an external clone — the `bundled-skills`
//! section is matched before `agent-skills`.

use std::collections::HashMap;
use std::path::PathBuf;

use ainb_skill_core::{UnitEntry, UnitKind, Uri};
use serde_yaml_ng::Value as YamlValue;

use super::class_a::{DiscoveredMarketplaceUnit, DiscoveredUnitKind};
use super::class_c::DiscoveredOrphanUnit;

/// Where a discovered unit really came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Provenance {
    /// A Claude Code marketplace plugin install. `git_commit_sha` and
    /// `scope` are joined from `installed_plugins.json` (both `None` /
    /// absent when the registry is missing).
    Marketplace {
        plugin: String,
        marketplace: String,
        version: String,
        git_commit_sha: Option<String>,
        scope: Option<String>,
    },
    /// An external git repo cloned into the tool home by the legacy
    /// bootstrap (`agent-skills[]` in `external-dependencies.yaml`).
    ExternalRepo {
        repo: String,
        version: Option<String>,
        applies_to: Vec<String>,
    },
    /// A skill bundled in the toolkit (`bundled-skills[]`).
    Toolkit { path: PathBuf },
    /// Already declared in `manifest.yaml`.
    AlreadyAdopted { uri: Uri },
    /// Genuinely hand-authored — no upstream provenance.
    Local,
}

impl Provenance {
    /// Short, CLI-filter-friendly kind label
    /// (`marketplace` / `external` / `toolkit` / `adopted` / `local`).
    /// Matches the `--provenance <kind>` filter values accepted by
    /// `ainb skill scan`.
    pub fn kind_label(&self) -> &'static str {
        match self {
            Provenance::Marketplace { .. } => "marketplace",
            Provenance::ExternalRepo { .. } => "external",
            Provenance::Toolkit { .. } => "toolkit",
            Provenance::AlreadyAdopted { .. } => "adopted",
            Provenance::Local => "local",
        }
    }
}

/// One discovered unit with its resolved provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttributedUnit {
    pub name: String,
    pub kind: UnitKind,
    /// Tool home the unit lives in (`claude`, `codex`, …) for class-C
    /// orphans; `None` for class-A marketplace plugins (they live in
    /// the shared plugin cache, deployed to claude).
    pub tool: Option<String>,
    pub path: PathBuf,
    pub provenance: Provenance,
}

// ------------------------------------------------------------------
// Parsed views over the three source manifests
// ------------------------------------------------------------------

/// One install record from `installed_plugins.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledRecord {
    pub version: String,
    pub git_commit_sha: Option<String>,
    pub scope: Option<String>,
}

/// Parsed `installed_plugins.json`, keyed by `<plugin>@<marketplace>`.
/// Each key maps to the *first* (user-scope-preferred) install record.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstalledPlugins {
    by_plugin_marketplace: HashMap<String, InstalledRecord>,
}

impl InstalledPlugins {
    /// Look up the install record for a `(plugin, marketplace)` pair.
    pub fn lookup(&self, plugin: &str, marketplace: &str) -> Option<&InstalledRecord> {
        self.by_plugin_marketplace.get(&format!("{plugin}@{marketplace}"))
    }
}

/// One `agent-skills[]` entry from `external-dependencies.yaml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalEntry {
    pub repo: String,
    pub version: Option<String>,
    pub applies_to: Vec<String>,
}

/// One `bundled-skills[]` entry from `external-dependencies.yaml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundledEntry {
    pub path: PathBuf,
}

/// Parsed `external-dependencies.yaml`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExternalDependencies {
    /// `agent-skills[]`, keyed by name → external git repo.
    pub agent_skills: HashMap<String, ExternalEntry>,
    /// `bundled-skills[]`, keyed by name → toolkit path.
    pub bundled_skills: HashMap<String, BundledEntry>,
}

/// The three parsed source manifests handed to [`attribute`]. Build
/// with [`ProvenanceSources::default`] (the "no provenance" case that
/// preserves v1.2 behaviour) or fill the fields from the `parse_*`
/// helpers + the loaded manifest.
#[derive(Debug, Clone, Default)]
pub struct ProvenanceSources {
    pub installed_plugins: InstalledPlugins,
    pub external_deps: ExternalDependencies,
    /// Units already declared in `manifest.yaml`.
    pub manifest_units: Vec<UnitEntry>,
    /// Reserved for an explicit toolkit `bundled-skills` override list
    /// (unused today; bundled skills are read from `external_deps`).
    pub toolkit_bundled: Vec<String>,
}

// ------------------------------------------------------------------
// Parsers (impure boundary — best-effort, never panic)
// ------------------------------------------------------------------

/// Parse Claude Code's `installed_plugins.json` (schema v2). The file
/// shape is:
///
/// ```json
/// { "version": 2,
///   "plugins": {
///     "<plugin>@<marketplace>": [
///       { "scope": "user", "version": "0.1.0", "gitCommitSha": "..." }
///     ] } }
/// ```
///
/// A `<plugin>@<marketplace>` key can have multiple records (one per
/// scope: user / project). We keep the `user`-scope record when one is
/// present, else the first record — so the joined SHA is the
/// user-global install the discovery walker is most likely looking at.
///
/// Malformed / empty input yields an empty [`InstalledPlugins`].
pub fn parse_installed_plugins(json: &str) -> InstalledPlugins {
    let mut out = InstalledPlugins::default();
    let Ok(root) = serde_json::from_str::<serde_json::Value>(json) else {
        return out;
    };
    let Some(plugins) = root.get("plugins").and_then(|p| p.as_object()) else {
        return out;
    };
    for (key, records) in plugins {
        let Some(records) = records.as_array() else {
            continue;
        };
        // Prefer the user-scope record; fall back to the first.
        let chosen = records
            .iter()
            .find(|r| r.get("scope").and_then(|s| s.as_str()) == Some("user"))
            .or_else(|| records.first());
        let Some(rec) = chosen else { continue };
        let version = rec.get("version").and_then(|v| v.as_str()).unwrap_or("unknown").to_string();
        let git_commit_sha = rec.get("gitCommitSha").and_then(|v| v.as_str()).map(str::to_string);
        let scope = rec.get("scope").and_then(|v| v.as_str()).map(str::to_string);
        out.by_plugin_marketplace.insert(
            key.clone(),
            InstalledRecord {
                version,
                git_commit_sha,
                scope,
            },
        );
    }
    out
}

/// Parse `external-dependencies.yaml` into name-keyed lookups for the
/// `bundled-skills` (toolkit) and `agent-skills` (external repo)
/// sections.
///
/// Malformed / empty input yields an empty [`ExternalDependencies`].
pub fn parse_external_dependencies(yaml: &str) -> ExternalDependencies {
    let mut out = ExternalDependencies::default();
    if yaml.trim().is_empty() {
        return out;
    }
    let Ok(root) = serde_yaml_ng::from_str::<YamlValue>(yaml) else {
        return out;
    };

    if let Some(list) = root.get("bundled-skills").and_then(|v| v.as_sequence()) {
        for entry in list {
            let Some(name) = entry.get("name").and_then(|v| v.as_str()) else {
                continue;
            };
            let path = entry
                .get("path")
                .and_then(|v| v.as_str())
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(format!("packages/skills/{name}")));
            out.bundled_skills.entry(name.to_string()).or_insert(BundledEntry { path });
        }
    }

    if let Some(list) = root.get("agent-skills").and_then(|v| v.as_sequence()) {
        for entry in list {
            let Some(name) = entry.get("name").and_then(|v| v.as_str()) else {
                continue;
            };
            // `repo` is the canonical field; fall back to `source` for
            // forward-compat with the `external-packages` shape.
            let Some(repo) =
                entry.get("repo").or_else(|| entry.get("source")).and_then(|v| v.as_str())
            else {
                continue;
            };
            let version = entry.get("version").and_then(|v| v.as_str()).map(str::to_string);
            let applies_to = entry
                .get("applies-to")
                .and_then(|v| v.as_sequence())
                .map(|seq| seq.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            out.agent_skills.entry(name.to_string()).or_insert(ExternalEntry {
                repo: repo.to_string(),
                version,
                applies_to,
            });
        }
    }

    out
}

// ------------------------------------------------------------------
// attribute() — pure
// ------------------------------------------------------------------

/// Attribute every discovered unit (class-A + class-C) to its real
/// [`Provenance`]. Pure: no fs, no env, no panics.
///
/// Class-A units always resolve to [`Provenance::Marketplace`]; the
/// SHA + scope are joined from `sources.installed_plugins`.
///
/// Class-C orphans resolve by the precedence documented at the module
/// level: AlreadyAdopted → Toolkit → ExternalRepo → Local.
pub fn attribute(
    class_a: &[DiscoveredMarketplaceUnit],
    class_c: &[DiscoveredOrphanUnit],
    sources: &ProvenanceSources,
) -> Vec<AttributedUnit> {
    let manifest_names = manifest_name_index(&sources.manifest_units);
    let mut out = Vec::new();

    // Class-A: marketplace plugins.
    for plugin in class_a {
        let installed = sources.installed_plugins.lookup(&plugin.plugin, &plugin.marketplace);
        for unit in &plugin.units {
            out.push(AttributedUnit {
                name: unit.name.clone(),
                kind: class_a_kind(unit.kind),
                tool: None,
                path: unit.path.clone(),
                provenance: Provenance::Marketplace {
                    plugin: plugin.plugin.clone(),
                    marketplace: plugin.marketplace.clone(),
                    version: plugin.version.clone(),
                    git_commit_sha: installed.and_then(|r| r.git_commit_sha.clone()),
                    scope: installed.and_then(|r| r.scope.clone()),
                },
            });
        }
    }

    // Class-C: orphans, resolved by precedence.
    for orphan in class_c {
        let provenance = attribute_orphan(orphan, sources, &manifest_names);
        out.push(AttributedUnit {
            name: orphan.name.clone(),
            kind: orphan.kind,
            tool: Some(orphan.tool.clone()),
            path: orphan.path.clone(),
            provenance,
        });
    }

    out
}

/// Resolve a single class-C orphan's provenance by precedence.
fn attribute_orphan(
    orphan: &DiscoveredOrphanUnit,
    sources: &ProvenanceSources,
    manifest_names: &HashMap<String, Uri>,
) -> Provenance {
    // 1. AlreadyAdopted — name matches a manifest UnitEntry.
    if let Some(uri) = manifest_names.get(&orphan.name) {
        return Provenance::AlreadyAdopted { uri: uri.clone() };
    }
    // 2. Toolkit — name matches a bundled-skills entry.
    if let Some(bundled) = sources.external_deps.bundled_skills.get(&orphan.name) {
        return Provenance::Toolkit {
            path: bundled.path.clone(),
        };
    }
    // 3. ExternalRepo — name matches an agent-skills entry.
    if let Some(ext) = sources.external_deps.agent_skills.get(&orphan.name) {
        return Provenance::ExternalRepo {
            repo: ext.repo.clone(),
            version: ext.version.clone(),
            applies_to: ext.applies_to.clone(),
        };
    }
    // 4. Local fallback.
    Provenance::Local
}

/// Index manifest units by their trailing path segment (unit name) so
/// orphan names can be matched against already-adopted entries. The
/// last `/`-segment of a unit URI is the unit name.
fn manifest_name_index(units: &[UnitEntry]) -> HashMap<String, Uri> {
    let mut idx = HashMap::new();
    for unit in units {
        let Ok(uri) = Uri::parse(&unit.uri) else {
            continue;
        };
        if let Some(name) = unit.uri.rsplit('/').next() {
            idx.entry(name.to_string()).or_insert(uri);
        }
    }
    idx
}

fn class_a_kind(k: DiscoveredUnitKind) -> UnitKind {
    match k {
        DiscoveredUnitKind::Skill => UnitKind::Skill,
        DiscoveredUnitKind::Agent => UnitKind::Agent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_installed_plugins_prefers_user_scope() {
        let json = r#"{
          "version": 2,
          "plugins": {
            "p@m": [
              { "scope": "project", "version": "0.3.1", "gitCommitSha": "deadbeef" },
              { "scope": "user", "version": "0.1.0", "gitCommitSha": "cafef00d" }
            ]
          }
        }"#;
        let parsed = parse_installed_plugins(json);
        let rec = parsed.lookup("p", "m").expect("p@m present");
        assert_eq!(rec.version, "0.1.0", "user scope wins");
        assert_eq!(rec.git_commit_sha.as_deref(), Some("cafef00d"));
        assert_eq!(rec.scope.as_deref(), Some("user"));
    }

    #[test]
    fn parse_installed_plugins_tolerates_missing_sha() {
        let json = r#"{"plugins":{"p@m":[{"scope":"user","version":"unknown"}]}}"#;
        let rec = parse_installed_plugins(json).lookup("p", "m").cloned();
        let rec = rec.expect("present");
        assert_eq!(rec.git_commit_sha, None);
    }

    #[test]
    fn parse_installed_plugins_garbage_is_empty() {
        assert_eq!(
            parse_installed_plugins("not json"),
            InstalledPlugins::default()
        );
        assert_eq!(parse_installed_plugins(""), InstalledPlugins::default());
    }

    #[test]
    fn parse_external_deps_empty_is_empty() {
        assert_eq!(
            parse_external_dependencies(""),
            ExternalDependencies::default()
        );
        assert_eq!(
            parse_external_dependencies("not: [valid"),
            ExternalDependencies::default()
        );
    }
}
