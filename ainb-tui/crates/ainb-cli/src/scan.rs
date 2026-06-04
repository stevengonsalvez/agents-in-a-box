//! `ainb skill scan` — print a provenance tree over every discovered
//! unit (bead `ai-ya9`, skill-manager v1.3).
//!
//! Read-only. Walks the class-A plugin cache and class-C orphans,
//! then joins them against `installed_plugins.json`,
//! `external-dependencies.yaml`, and `manifest.yaml` via the pure
//! [`provenance::attribute`] matcher, and renders a tree grouped by
//! tool. The `--provenance` and `--tool` flags filter the rows;
//! `--json` emits a machine-readable array.
//!
//! NO SQLite, no DB: every input is a parsed file, the matcher is a
//! pure function, and nothing is written back.

use std::io;
use std::path::{Path, PathBuf};

use anyhow::Result;

use ainb_adapters_tool::install_root_for;
use ainb_skill_core::manifest::Manifest;
use ainb_skill_core::paths::manifest_path_in;

use crate::discovery::class_a;
use crate::discovery::class_c::walk_orphans;
use crate::discovery::provenance::{
    attribute, parse_external_dependencies, parse_installed_plugins, AttributedUnit, Provenance,
    ProvenanceSources,
};
use crate::ScanArgs;

pub fn dispatch(home: &Path, args: ScanArgs, out: &mut dyn io::Write) -> Result<()> {
    let claude_home = install_root_for("claude");
    let class_a = class_a::walk(&claude_home);
    let class_c = walk_orphans();

    let sources = load_sources(home, &claude_home, args.ext_deps.as_deref());
    let mut attributed = attribute(&class_a, &class_c, &sources);

    // Filters.
    if let Some(kind) = &args.provenance {
        let kind = kind.to_lowercase();
        attributed.retain(|u| u.provenance.kind_label() == kind);
    }
    if let Some(tool) = &args.tool {
        attributed.retain(|u| u.tool.as_deref() == Some(tool.as_str()));
    }

    if args.json {
        render_json(&attributed, out)
    } else {
        render_tree(&attributed, out)
    }
}

/// Assemble the three parsed source manifests the matcher needs.
/// Best-effort: a missing / malformed file degrades to an empty view.
fn load_sources(
    home: &Path,
    claude_home: &Path,
    ext_deps_override: Option<&Path>,
) -> ProvenanceSources {
    let installed_plugins_json = std::fs::read_to_string(
        claude_home.join("plugins").join("installed_plugins.json"),
    )
    .unwrap_or_default();

    let ext_yaml = ext_deps_override
        .map(PathBuf::from)
        .filter(|p| p.is_file())
        .or_else(default_ext_deps_path)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();

    let manifest = Manifest::load_from(&manifest_path_in(home)).unwrap_or_default();

    ProvenanceSources {
        installed_plugins: parse_installed_plugins(&installed_plugins_json),
        external_deps: parse_external_dependencies(&ext_yaml),
        manifest_units: manifest.units,
        toolkit_bundled: Vec::new(),
    }
}

/// Default `external-dependencies.yaml` lookup: `$HOME/...` then the
/// current working directory. Returns the first that exists.
fn default_ext_deps_path() -> Option<PathBuf> {
    let candidates = [
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("external-dependencies.yaml")),
        std::env::current_dir()
            .ok()
            .map(|c| c.join("external-dependencies.yaml")),
    ];
    candidates.into_iter().flatten().find(|p| p.is_file())
}

/// Human-readable label for a provenance, used in the tree.
fn provenance_label(p: &Provenance) -> String {
    match p {
        Provenance::Marketplace {
            plugin,
            marketplace,
            version,
            git_commit_sha,
            ..
        } => {
            let sha = git_commit_sha
                .as_deref()
                .map(|s| format!(" @{}", &s[..s.len().min(7)]))
                .unwrap_or_default();
            format!("marketplace: {plugin}@{marketplace} {version}{sha}")
        }
        Provenance::ExternalRepo { repo, version, .. } => {
            let v = version
                .as_deref()
                .map(|v| format!(" {v}"))
                .unwrap_or_default();
            format!("external (gh:): {repo}{v}")
        }
        Provenance::Toolkit { path } => format!("toolkit: {}", path.display()),
        Provenance::AlreadyAdopted { uri } => format!("adopted: {}", uri.display()),
        Provenance::Local => "local".to_string(),
    }
}

/// Group label for the tree's top-level node.
fn group_for(u: &AttributedUnit) -> String {
    match &u.tool {
        Some(tool) => format!("tool: {tool}"),
        None => "marketplace (claude plugin cache)".to_string(),
    }
}

fn render_tree(units: &[AttributedUnit], out: &mut dyn io::Write) -> Result<()> {
    if units.is_empty() {
        writeln!(out, "no units discovered")?;
        return Ok(());
    }

    // Stable group order: sort group keys, then units by name within.
    let mut groups: std::collections::BTreeMap<String, Vec<&AttributedUnit>> =
        std::collections::BTreeMap::new();
    for u in units {
        groups.entry(group_for(u)).or_default().push(u);
    }

    for (group, mut members) in groups {
        members.sort_by(|a, b| a.name.cmp(&b.name));
        writeln!(out, "{group}")?;
        for u in members {
            writeln!(
                out,
                "  ├─ {} [{}]  {}",
                u.name,
                u.kind,
                provenance_label(&u.provenance)
            )?;
        }
    }
    Ok(())
}

fn render_json(units: &[AttributedUnit], out: &mut dyn io::Write) -> Result<()> {
    let rows: Vec<serde_json::Value> = units
        .iter()
        .map(|u| {
            serde_json::json!({
                "name": u.name,
                "kind": u.kind.to_string(),
                "tool": u.tool,
                "path": u.path.display().to_string(),
                "provenance": u.provenance.kind_label(),
                "detail": provenance_detail_json(&u.provenance),
            })
        })
        .collect();
    let body = serde_json::to_string_pretty(&rows)?;
    writeln!(out, "{body}")?;
    Ok(())
}

/// Structured detail object per provenance variant for the `--json`
/// output, so scripts can read the SHA / repo / URI without re-parsing
/// the human label.
fn provenance_detail_json(p: &Provenance) -> serde_json::Value {
    match p {
        Provenance::Marketplace {
            plugin,
            marketplace,
            version,
            git_commit_sha,
            scope,
        } => serde_json::json!({
            "plugin": plugin,
            "marketplace": marketplace,
            "version": version,
            "git_commit_sha": git_commit_sha,
            "scope": scope,
        }),
        Provenance::ExternalRepo {
            repo,
            version,
            applies_to,
        } => serde_json::json!({
            "repo": repo,
            "version": version,
            "applies_to": applies_to,
        }),
        Provenance::Toolkit { path } => serde_json::json!({ "path": path.display().to_string() }),
        Provenance::AlreadyAdopted { uri } => serde_json::json!({ "uri": uri.display() }),
        Provenance::Local => serde_json::json!({}),
    }
}
