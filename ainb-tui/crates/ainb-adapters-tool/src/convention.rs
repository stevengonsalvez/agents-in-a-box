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
    // Defense-in-depth: a unit's file list must only ever name files ainb
    // deployed (skills/agents/commands/...). If a corrupt or hand-edited
    // lockfile points uninstall at protected user state — session history,
    // agent memory, task state, or config like settings.json / CLAUDE.md —
    // refuse loudly rather than silently delete the user's data.
    for rel in file_hashes.keys() {
        if is_protected(Path::new(rel)) {
            anyhow::bail!(
                "refusing to uninstall: `{rel}` resolves into protected user state \
                 under {} — the lockfile looks corrupt",
                install_root.display()
            );
        }
    }
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

/// True when `rel` (a path relative to a tool install root, e.g. `~/.claude`)
/// names data ainb never deploys and must never delete. Used by [`uninstall`]
/// as a hard guard against a corrupt lockfile: ainb only ever writes
/// skill/agent/command unit files, so a `file_hashes` entry pointing at
/// session history, agent memory, task state, or top-level config means the
/// lockfile is bad — refuse rather than wipe the user's state.
///
/// Protected: anything under `projects/`, `memory/`, `todos/`, `statsig/`,
/// `shell-snapshots/`, `image-cache/`; the top-level files `settings.json`,
/// `settings.local.json`, `CLAUDE.md`, `history.jsonl`; and any path that
/// tries to escape the install root via `..` or an absolute component.
fn is_protected(rel: &Path) -> bool {
    use std::path::Component;
    // Reject anything that escapes the install root.
    if rel
        .components()
        .any(|c| matches!(c, Component::ParentDir | Component::RootDir | Component::Prefix(_)))
    {
        return true;
    }
    let segments: Vec<&str> = rel
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => s.to_str(),
            _ => None,
        })
        .collect();
    let Some(first) = segments.first() else {
        return false;
    };
    if matches!(
        *first,
        "projects" | "memory" | "todos" | "statsig" | "shell-snapshots" | "image-cache"
    ) {
        return true;
    }
    // Top-level config files only (a unit may legitimately ship a nested
    // file that happens to share one of these names).
    segments.len() == 1
        && matches!(
            *first,
            "settings.json" | "settings.local.json" | "CLAUDE.md" | "history.jsonl"
        )
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

#[cfg(test)]
mod tests {
    use super::*;

    fn deployed(files: &[&str]) -> DeployedRef {
        let mut hashes = BTreeMap::new();
        for f in files {
            hashes.insert((*f).to_string(), "sha256:deadbeef".to_string());
        }
        DeployedRef::Deployed {
            path: "unit".to_string(),
            file_hashes: hashes,
        }
    }

    #[test]
    fn is_protected_flags_state_dirs_and_config_but_not_units() {
        // Protected state.
        for p in [
            "projects/abc.jsonl",
            "memory/MEMORY.md",
            "todos/x.json",
            "statsig/y",
            "shell-snapshots/z",
            "image-cache/i.png",
            "settings.json",
            "settings.local.json",
            "CLAUDE.md",
            "history.jsonl",
            "../escape.txt",
        ] {
            assert!(is_protected(Path::new(p)), "expected protected: {p}");
        }
        // Legitimate unit files (incl. nested files sharing a protected name).
        for p in [
            "skills/foo/SKILL.md",
            "agents/bar.md",
            "commands/baz.md",
            "skills/foo/CLAUDE.md",
            "skills/foo/settings.json",
        ] {
            assert!(!is_protected(Path::new(p)), "expected allowed: {p}");
        }
    }

    #[test]
    fn uninstall_refuses_and_preserves_protected_state() {
        let root = tempfile::tempdir().unwrap();
        // Seed user state that must survive.
        fs::create_dir_all(root.path().join("projects")).unwrap();
        let session = root.path().join("projects/session.jsonl");
        fs::write(&session, b"history").unwrap();
        let settings = root.path().join("settings.json");
        fs::write(&settings, b"{}").unwrap();

        // A corrupt lockfile claims the unit deployed those state files.
        let bad = deployed(&["projects/session.jsonl", "settings.json"]);
        let err = uninstall(&bad, root.path()).unwrap_err();
        assert!(err.to_string().contains("protected user state"), "got: {err}");
        assert!(session.exists(), "session history must survive a refused uninstall");
        assert!(settings.exists(), "settings.json must survive a refused uninstall");
    }

    #[test]
    fn uninstall_removes_only_unit_files() {
        let root = tempfile::tempdir().unwrap();
        let unit_file = root.path().join("skills/foo/SKILL.md");
        fs::create_dir_all(unit_file.parent().unwrap()).unwrap();
        fs::write(&unit_file, b"x").unwrap();
        // Sibling state in the same root.
        fs::create_dir_all(root.path().join("memory")).unwrap();
        let mem = root.path().join("memory/MEMORY.md");
        fs::write(&mem, b"keep").unwrap();

        let ok = deployed(&["skills/foo/SKILL.md"]);
        uninstall(&ok, root.path()).unwrap();
        assert!(!unit_file.exists(), "unit file should be removed");
        assert!(mem.exists(), "untracked state must be untouched");
    }
}
