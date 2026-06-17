//! Shared deploy primitives for tool adapters that follow the
//! "convention layout" (skills/foo/SKILL.md, plugins/foo/plugin.json,
//! agents/foo.md, …) — i.e. every v1 tool. Each adapter still chooses
//! which subdirs to scan in list_installed and which kinds to accept,
//! but the plan/apply/uninstall mechanics are identical.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use ainb_adapters_source::ResolvedUnit;
use ainb_skill_core::DeployedRef;

use crate::plan::{InstallPlan, PlanOp, hex_sha256};

/// Compute an InstallPlan by reading each file in `unit.file_list`
/// from `unit.source_root` and mapping it to the same relative path
/// under `install_root`. Files that already match the rendered
/// content byte-for-byte are skipped (idempotent re-installs).
///
/// Pass an empty `substitutions` map to opt out of template
/// rewriting; otherwise every text file's content has its `{{KEY}}`
/// placeholders replaced with the corresponding values before
/// hashing / writing. Mirrors the `bootstrap.js` template flow:
/// `{{TOOL_DIR}}` → `.claude` / `.codex` / `.gemini`, etc.
pub fn plan_install_prefix_swap(
    tool: &str,
    unit: &ResolvedUnit,
    install_root: &Path,
    substitutions: &std::collections::HashMap<&'static str, String>,
) -> anyhow::Result<InstallPlan> {
    let kind = unit
        .descriptor
        .kind
        .parse()
        .map_err(|e| anyhow::anyhow!("parse kind `{}`: {e}", unit.descriptor.kind))?;
    let unit_install_path = install_root.join(&unit.descriptor.path);

    let mut ops = Vec::with_capacity(unit.file_list.len());
    for rel in &unit.file_list {
        let src = unit.source_root.join(rel);
        let dst = install_root.join(rel);
        let raw = fs::read(&src).map_err(|e| anyhow::anyhow!("read `{}`: {e}", src.display()))?;
        let contents = apply_substitutions(&raw, substitutions);
        if dst.exists() {
            let previous = fs::read(&dst).unwrap_or_default();
            if previous == contents {
                continue;
            }
            ops.push(PlanOp::Update {
                dst,
                previous,
                contents,
            });
        } else {
            ops.push(PlanOp::Create { dst, contents });
        }
    }

    Ok(InstallPlan {
        tool: tool.to_string(),
        unit_uri: format!(
            "{}/{}",
            unit.source_root.to_string_lossy(),
            unit.descriptor.path
        ),
        kind,
        unit_install_path,
        ops,
    })
}

/// Apply `{{KEY}}` → `value` substitution to UTF-8 text content.
/// Binary inputs (non-UTF-8) pass through untouched so adapters
/// can ship `.png` / `.bin` payloads alongside markdown.
pub fn apply_substitutions(
    bytes: &[u8],
    substitutions: &std::collections::HashMap<&'static str, String>,
) -> Vec<u8> {
    if substitutions.is_empty() {
        return bytes.to_vec();
    }
    let Ok(text) = std::str::from_utf8(bytes) else {
        return bytes.to_vec();
    };
    let mut out = text.to_string();
    for (key, value) in substitutions {
        let needle = format!("{{{{{key}}}}}");
        if out.contains(&needle) {
            out = out.replace(&needle, value);
        }
    }
    out.into_bytes()
}

/// Uninstall everything in `deployed.file_hashes`, deepest path
/// first, and prune now-empty parent dirs upward until reaching
/// `install_root`.
pub fn uninstall(deployed: &DeployedRef, install_root: &Path) -> anyhow::Result<()> {
    let DeployedRef::Deployed { file_hashes, .. } = deployed else {
        return Ok(());
    };
    let mut paths: Vec<PathBuf> = file_hashes.keys().map(|rel| install_root.join(rel)).collect();
    paths.sort_by_key(|p| std::cmp::Reverse(p.components().count()));

    for abs in &paths {
        if abs.exists() {
            fs::remove_file(abs)?;
        }
        if let Some(parent) = abs.parent() {
            prune_empty_dirs_upward(parent, install_root);
        }
    }
    Ok(())
}

/// Walk `<root>/<subdir>/<unit>/...` listing each unit directory as
/// one deployed ref with full per-file hash catalog.
pub fn collect_subdir_entries(root: &Path, subdir: &str, out: &mut Vec<DeployedRef>) {
    let dir = root.join(subdir);
    let Ok(entries) = fs::read_dir(&dir) else {
        return;
    };
    for e in entries.flatten() {
        let unit_path = e.path();
        if !unit_path.is_dir() {
            continue;
        }
        let mut hashes = BTreeMap::new();
        for f in walkdir::WalkDir::new(&unit_path).sort_by_file_name() {
            let Ok(f) = f else { continue };
            if !f.file_type().is_file() {
                continue;
            }
            let rel = match f.path().strip_prefix(root) {
                Ok(r) => r,
                Err(_) => continue,
            };
            let body = fs::read(f.path()).unwrap_or_default();
            hashes.insert(
                rel.to_string_lossy().to_string(),
                format!("sha256:{}", hex_sha256(&body)),
            );
        }
        out.push(DeployedRef::Deployed {
            path: unit_path.to_string_lossy().to_string(),
            file_hashes: hashes,
        });
    }
}

/// Walk `<root>/<subdir>/*.md` listing each markdown file as one
/// deployed ref.
pub fn collect_flat_md(root: &Path, subdir: &str, out: &mut Vec<DeployedRef>) {
    let dir = root.join(subdir);
    let Ok(entries) = fs::read_dir(&dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("md") {
            continue;
        }
        let rel = match p.strip_prefix(root) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let body = fs::read(&p).unwrap_or_default();
        let mut hashes = BTreeMap::new();
        hashes.insert(
            rel.to_string_lossy().to_string(),
            format!("sha256:{}", hex_sha256(&body)),
        );
        out.push(DeployedRef::Deployed {
            path: p.to_string_lossy().to_string(),
            file_hashes: hashes,
        });
    }
}

fn prune_empty_dirs_upward(dir: &Path, stop_at: &Path) {
    let mut cur = Some(dir.to_path_buf());
    while let Some(p) = cur {
        if p == stop_at || !p.starts_with(stop_at) {
            return;
        }
        let entries = match fs::read_dir(&p) {
            Ok(e) => e.count(),
            Err(_) => return,
        };
        if entries > 0 {
            return;
        }
        if fs::remove_dir(&p).is_err() {
            return;
        }
        cur = p.parent().map(|x| x.to_path_buf());
    }
}
