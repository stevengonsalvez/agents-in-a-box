//! `ainb migrate ...` — day-1 helpers for moving from the legacy
//! `toolkit/bootstrap.js` flow to ainb-managed unit lifecycle.
//!
//! Three modes, all mutually exclusive at the CLI surface:
//!
//!   - `--check`: read-only scan of every adapter's install root.
//!     Reports tool / path / file count so the user knows what
//!     `--clean` would wipe.
//!
//!   - `--clean [--backup]`: snapshot each adapter's install root
//!     under `$AINB_HOME/backups/<ts>/<tool>/` when --backup is set,
//!     wipe the install roots, then run `ainb skill sync` to
//!     reinstall from the manifest's desired state. Honors --yes /
//!     --dry-run.
//!
//!   - `--from-bootstrap`: parse `toolkit/external-dependencies.yaml`
//!     and write equivalent source + unit entries into the manifest.
//!     A single `toolkit` `local:` source is added pointing at the
//!     toolkit root; one UnitEntry is recorded per bundled-skill /
//!     agent-skill path so a subsequent `ainb skill sync` picks them
//!     up cleanly.
//!
//! All adapter writes respect the per-tool `AINB_TOOL_HOME_<TOOL>`
//! env overrides established by P3, so tests can fully sandbox
//! migrate without touching the user's real `~/.claude`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde_yaml_ng::Value as YamlValue;

use ainb_adapters_tool::all_adapters;
use ainb_skill_core::lockfile::{DeployedRef, Lockfile};
use ainb_skill_core::manifest::{Manifest, SourceEntry, UnitEntry};
use ainb_skill_core::paths::{lockfile_path_in, manifest_path_in};

use crate::{MigrateArgs, SkillCommand, SyncArgs};

pub fn dispatch(home: &Path, args: MigrateArgs, out: &mut dyn io::Write) -> Result<()> {
    match (args.check, args.clean, args.from_bootstrap) {
        (true, _, _) => migrate_check(home, out),
        (_, true, _) => migrate_clean(home, args, out),
        (_, _, true) => migrate_from_bootstrap(home, args, out),
        _ => bail!("specify one of --check, --clean [--backup], or --from-bootstrap"),
    }
}

/// `ainb migrate --check`: list every adapter's install_root content
/// without modifying anything.
fn migrate_check(home: &Path, out: &mut dyn io::Write) -> Result<()> {
    let _ = home;
    let mut total_files = 0;
    let mut total_units = 0;
    writeln!(out, "# migrate --check")?;
    for adapter in all_adapters() {
        let root = adapter.install_root();
        if !root.exists() {
            writeln!(out, "- {}: (empty) {}", adapter.name(), root.display())?;
            continue;
        }
        let installed = adapter
            .list_installed()
            .with_context(|| format!("listing installed for {}", adapter.name()))?;
        let unit_count = installed.len();
        let file_count: usize = installed
            .iter()
            .map(|d| match d {
                DeployedRef::Deployed { file_hashes, .. } => file_hashes.len(),
                _ => 0,
            })
            .sum();
        total_units += unit_count;
        total_files += file_count;
        writeln!(
            out,
            "- {}: {} unit(s), {} file(s) at {}",
            adapter.name(),
            unit_count,
            file_count,
            root.display()
        )?;
    }
    writeln!(out, "# total: {total_units} unit(s), {total_files} file(s)")?;
    Ok(())
}

/// `ainb migrate --clean`: wipe every adapter's install root (after
/// optional backup) and reinstall via `skill sync`.
fn migrate_clean(home: &Path, args: MigrateArgs, out: &mut dyn io::Write) -> Result<()> {
    let manifest = Manifest::load_from(&manifest_path_in(home))?;
    if manifest.units.is_empty() {
        bail!(
            "manifest declares no units — migrate --clean would wipe \
             everything and leave nothing. Run `ainb migrate --from-bootstrap` \
             first, or edit `manifest.yaml` to declare desired units."
        );
    }

    let backup_dir = if args.backup {
        Some(make_backup_dir(home)?)
    } else {
        None
    };
    writeln!(out, "# migrate --clean")?;
    if let Some(d) = &backup_dir {
        writeln!(out, "# backup → {}", d.display())?;
    }

    let mut to_wipe: Vec<(String, PathBuf)> = Vec::new();
    for adapter in all_adapters() {
        let root = adapter.install_root();
        if root.exists() {
            to_wipe.push((adapter.name().to_string(), root));
        }
    }
    if to_wipe.is_empty() {
        writeln!(out, "# nothing to wipe")?;
    }
    for (tool, root) in &to_wipe {
        writeln!(out, "- {tool}: would wipe {}", root.display())?;
    }

    if args.dry_run {
        writeln!(out, "# dry-run: not applying")?;
        return Ok(());
    }
    if !args.yes {
        bail!("interactive confirmation not yet wired — pass --yes (or --dry-run)");
    }

    // Clear the lockfile's per-unit deployment record BEFORE
    // wiping disk. Order matters: if the process is interrupted
    // mid-wipe, an old lockfile claiming files are deployed would
    // mislead `doctor` and `sync` into thinking everything is fine.
    // Saving the cleared lockfile first ensures any partial state
    // is recoverable by re-running `migrate --clean` (or just
    // `skill sync` against the manifest).
    let lockfile_path = lockfile_path_in(home);
    let mut lockfile = Lockfile::load_from(&lockfile_path)?;
    let cleared = lockfile.units.len();
    lockfile.units.clear();
    lockfile.save_to(&lockfile_path)?;
    writeln!(out, "# cleared {cleared} lockfile unit record(s)")?;

    // Snapshot + wipe each adapter root.
    for (tool, root) in &to_wipe {
        if let Some(d) = &backup_dir {
            let target = d.join(tool);
            copy_dir_recursive(root, &target)
                .with_context(|| format!("backing up {tool} to {}", target.display()))?;
        }
        fs::remove_dir_all(root).with_context(|| format!("wiping {}", root.display()))?;
    }
    writeln!(out, "# wiped {} tool root(s)", to_wipe.len())?;

    // Now run skill sync via the public dispatcher so the diff-and-
    // confirm path is identical to user-typed sync. With the unit
    // records cleared, sync sees every manifest-declared unit as
    // missing and reinstalls it.
    let mut sub = Vec::new();
    crate::skill::dispatch(
        home,
        SkillCommand::Sync(SyncArgs {
            yes: true,
            dry_run: false,
        }),
        &mut sub,
    )?;
    out.write_all(&sub)?;
    Ok(())
}

/// `ainb migrate --from-bootstrap`: parse the legacy
/// `external-dependencies.yaml` and seed the manifest with a
/// `toolkit` source + one UnitEntry per bundled-skill / agent-skill
/// path.
fn migrate_from_bootstrap(home: &Path, args: MigrateArgs, out: &mut dyn io::Write) -> Result<()> {
    let toolkit_root = args
        .toolkit_root
        .clone()
        .or_else(|| std::env::current_dir().ok().map(|p| p.join("toolkit")))
        .ok_or_else(|| anyhow!("could not determine toolkit root; pass --toolkit-root"))?;
    if !toolkit_root.is_dir() {
        bail!(
            "toolkit root `{}` does not exist or is not a directory",
            toolkit_root.display()
        );
    }
    let ext_yaml = toolkit_root.join("external-dependencies.yaml");
    if !ext_yaml.is_file() {
        bail!(
            "missing `{}` — pass --toolkit-root pointing at a directory \
             containing `external-dependencies.yaml`",
            ext_yaml.display()
        );
    }

    let body =
        fs::read_to_string(&ext_yaml).with_context(|| format!("reading {}", ext_yaml.display()))?;
    let parsed: YamlValue =
        serde_yaml_ng::from_str(&body).with_context(|| "parsing external-dependencies.yaml")?;

    let mut units: Vec<UnitEntry> = Vec::new();
    for section in ["bundled-skills", "agent-skills"] {
        if let Some(list) = parsed.get(section).and_then(|v| v.as_sequence()) {
            for entry in list {
                let Some(path) = entry.get("path").and_then(|v| v.as_str()) else {
                    continue;
                };
                let local_uri = format!("local:{}", toolkit_root.display());
                let unit_uri = format!("{local_uri}@main/{path}");
                units.push(UnitEntry {
                    uri: unit_uri,
                    targets: None,
                });
            }
        }
    }

    let manifest_path = manifest_path_in(home);
    let mut manifest = Manifest::load_from(&manifest_path)?;
    let source_name = "toolkit".to_string();
    let source_uri = format!("local:{}", toolkit_root.display());

    let mut added_source = false;
    if manifest.sources.iter().all(|s| s.name != source_name) {
        manifest.sources.push(SourceEntry {
            name: source_name.clone(),
            kind: Some("raw".into()),
            uri: source_uri.clone(),
            r#ref: "main".into(),
            enabled: true,
        });
        added_source = true;
    }

    // Deduplicate against existing UnitEntry by URI.
    let existing: std::collections::HashSet<String> =
        manifest.units.iter().map(|u| u.uri.clone()).collect();
    let mut added_units = 0;
    for unit in units {
        if existing.contains(&unit.uri) {
            continue;
        }
        manifest.units.push(unit);
        added_units += 1;
    }

    writeln!(out, "# migrate --from-bootstrap")?;
    writeln!(out, "  toolkit root: {}", toolkit_root.display())?;
    writeln!(
        out,
        "  source `{source_name}` ({}): {}",
        source_uri,
        if added_source { "added" } else { "unchanged" }
    )?;
    writeln!(out, "  unit entries added: {added_units}")?;
    if args.dry_run {
        writeln!(out, "# dry-run: not writing manifest")?;
        return Ok(());
    }
    manifest.save_to(&manifest_path)?;
    writeln!(out, "# manifest written to {}", manifest_path.display())?;
    Ok(())
}

fn make_backup_dir(home: &Path) -> Result<PathBuf> {
    let ts = ainb_fetch::fetcher::now_utc_iso8601().replace(':', "-");
    let dir = home.join("backups").join(ts);
    fs::create_dir_all(&dir).with_context(|| format!("creating backup dir {}", dir.display()))?;
    Ok(dir)
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let target = dst.join(entry.file_name());
        let ft = entry.file_type()?;
        if ft.is_dir() {
            copy_dir_recursive(&path, &target)?;
        } else if ft.is_file() {
            fs::copy(&path, &target)?;
        }
    }
    Ok(())
}
