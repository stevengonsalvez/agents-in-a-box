//! `ainb migrate ...` — day-1 helpers for moving from the legacy
//! `toolkit/bootstrap.js` flow to ainb-managed unit lifecycle.
//!
//! Four modes, all mutually exclusive at the CLI surface:
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
//!   - `--discover`: scan the Claude Code plugin cache + every
//!     adapter's install root for already-deployed units the manifest
//!     does not yet know about (skill-manager v1.1 discovery flow).
//!     Honors `--dry-run`, `--legacy-yaml=<path>`, and `--force`.
//!
//! All adapter writes respect the per-tool `AINB_TOOL_HOME_<TOOL>`
//! env overrides established by P3, so tests can fully sandbox
//! migrate without touching the user's real `~/.claude`.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde_yaml_ng::Value as YamlValue;

use ainb_adapters_tool::{all_adapters, install_root_for};
use ainb_skill_core::lockfile::{DeployedRef, Lockfile};
use ainb_skill_core::manifest::{Manifest, SourceEntry, UnitEntry};
use ainb_skill_core::paths::{lockfile_path_in, manifest_path_in};

use crate::discovery::class_a::walk as walk_class_a;
use crate::discovery::class_c::walk_orphans;
use crate::discovery::reconcile::{ManifestPatch, WalkerOutput, reconcile};
use crate::{MigrateArgs, SkillCommand, SyncArgs};

pub fn dispatch(home: &Path, args: MigrateArgs, out: &mut dyn io::Write) -> Result<()> {
    match (args.check, args.clean, args.from_bootstrap, args.discover) {
        (true, _, _, _) => migrate_check(home, out),
        (_, true, _, _) => migrate_clean(home, args, out),
        (_, _, true, _) => migrate_from_bootstrap(home, args, out),
        (_, _, _, true) => migrate_discover(home, args, out),
        _ => bail!(
            "specify one of --check, --clean [--backup], --from-bootstrap, or --discover"
        ),
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
                    shadowed_by: None,
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
            read_only: false,
            target_layout: Vec::new(),
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

/// `ainb migrate --discover`: walk the Claude Code plugin cache +
/// every adapter's install root, classify discovered units, and
/// merge a manifest patch.
///
/// Orchestration order:
///
///   1. Empty-manifest guard — bail unless `--force` is set or the
///      manifest has no units. This prevents accidental re-scans
///      blowing existing curated state.
///   2. Walkers — class-A reads the Claude Code plugin cache rooted
///      at `install_root_for("claude")` (so `AINB_TOOL_HOME_CLAUDE`
///      sandboxes tests); class-C walks every adapter install root
///      via [`walk_orphans`]. Both pure, side-effect-free.
///   3. Reconciler — pure fn over the walker outputs that builds a
///      [`ManifestPatch`] with new sources + units (URIs synthesized
///      per spec §URI scheme additions; shadow relationships
///      computed per §User Flow 3).
///   4. Optional class-B legacy-yaml match — when `--legacy-yaml`
///      is supplied, name-match orphan units against the YAML and
///      rewrite their URI from `local:` to `gh:` so a hand-edited
///      orphan adopted from the bootstrap era is tracked as a
///      git-backed source instead of a local one.
///   5. Persist — unless `--dry-run`, merge the patch into the
///      manifest at `$AINB_HOME/manifest.yaml` (dedup by URI) and
///      save. Lockfile rows are left for `ainb skill sync` to
///      populate (it computes file_hashes from on-disk bytes).
fn migrate_discover(home: &Path, args: MigrateArgs, out: &mut dyn io::Write) -> Result<()> {
    let manifest_path = manifest_path_in(home);
    let mut manifest = Manifest::load_from(&manifest_path)?;

    if !manifest.units.is_empty() && !args.force {
        bail!(
            "manifest at `{}` already declares {} unit(s) — pass `--force` to \
             re-run discovery against a populated manifest, or edit it manually.",
            manifest_path.display(),
            manifest.units.len()
        );
    }

    // Class-A: marketplace plugins under `~/.claude/plugins/cache/`.
    // We resolve `claude_home` through `install_root_for("claude")` so
    // the same `AINB_TOOL_HOME_CLAUDE` override that sandboxes the
    // adapter writes also sandboxes the walker.
    let claude_home = install_root_for("claude");
    let class_a = walk_class_a(&claude_home);

    // Class-C: orphan SKILL.md units across every adapter's home.
    // `walk_orphans` internally calls `install_root_for(tool)` for
    // all 9 tools, so the same env overrides apply.
    let class_c = walk_orphans();

    let walker_out = WalkerOutput { class_a, class_c };
    let mut patch = reconcile(&walker_out);

    // Optional class-B: legacy-yaml name match.
    let mut legacy_yaml_path_used: Option<PathBuf> = None;
    let mut legacy_yaml_matches = 0;
    if let Some(yaml_path) = args.legacy_yaml.as_ref() {
        legacy_yaml_matches = apply_legacy_yaml_match(&mut patch, yaml_path)
            .with_context(|| format!("processing --legacy-yaml={}", yaml_path.display()))?;
        legacy_yaml_path_used = Some(yaml_path.clone());
    }

    // Merge into existing manifest (dedup by name for sources, by URI
    // for units — same convention as --from-bootstrap).
    let existing_source_names: std::collections::HashSet<String> =
        manifest.sources.iter().map(|s| s.name.clone()).collect();
    let existing_unit_uris: std::collections::HashSet<String> =
        manifest.units.iter().map(|u| u.uri.clone()).collect();
    let mut added_sources = 0;
    let mut added_units = 0;
    let mut new_sources_for_print: Vec<&SourceEntry> = Vec::new();
    let mut new_units_for_print: Vec<&UnitEntry> = Vec::new();
    for src in &patch.new_sources {
        if !existing_source_names.contains(&src.name) {
            new_sources_for_print.push(src);
            added_sources += 1;
        }
    }
    for unit in &patch.new_units {
        if !existing_unit_uris.contains(&unit.uri) {
            new_units_for_print.push(unit);
            added_units += 1;
        }
    }

    writeln!(out, "# migrate --discover")?;
    writeln!(
        out,
        "  marketplace plugins discovered: {}",
        walker_out.class_a.len()
    )?;
    let mut per_tool_counts: HashMap<&str, usize> = HashMap::new();
    for orphan in &walker_out.class_c {
        *per_tool_counts.entry(orphan.tool.as_str()).or_insert(0) += 1;
    }
    writeln!(
        out,
        "  orphan units discovered: {}",
        walker_out.class_c.len()
    )?;
    let mut tools_sorted: Vec<&&str> = per_tool_counts.keys().collect();
    tools_sorted.sort();
    for tool in tools_sorted {
        writeln!(out, "    {}: {}", tool, per_tool_counts[*tool])?;
    }
    if let Some(path) = &legacy_yaml_path_used {
        writeln!(
            out,
            "  legacy-yaml matches: {} (from {})",
            legacy_yaml_matches,
            path.display()
        )?;
    }
    writeln!(out, "  new sources to add: {added_sources}")?;
    writeln!(out, "  new units to add: {added_units}")?;
    for src in &new_sources_for_print {
        writeln!(out, "    + source {} ({})", src.name, src.uri)?;
    }
    for unit in &new_units_for_print {
        writeln!(out, "    + unit {}", unit.uri)?;
    }

    if args.dry_run {
        writeln!(out, "# dry-run: not writing manifest")?;
        return Ok(());
    }

    for src in patch.new_sources {
        if !existing_source_names.contains(&src.name) {
            manifest.sources.push(src);
        }
    }
    for unit in patch.new_units {
        if !existing_unit_uris.contains(&unit.uri) {
            manifest.units.push(unit);
        }
    }
    manifest.save_to(&manifest_path)?;

    // Touch the lockfile so it exists alongside the manifest. Locked
    // unit rows (with file_hashes) are populated by `ainb skill sync`
    // — discovery only records inventory, not deployed bytes.
    let lock_path = lockfile_path_in(home);
    let lockfile = Lockfile::load_from(&lock_path).unwrap_or_default();
    lockfile.save_to(&lock_path)?;

    writeln!(out, "# manifest written to {}", manifest_path.display())?;
    writeln!(
        out,
        "# next: run `ainb skill sync` to register deployed file hashes in the lockfile"
    )?;
    Ok(())
}

/// Parse `external-dependencies.yaml` at `path` and rewrite any
/// orphan `local:` unit URI in `patch` whose unit-name matches a
/// `bundled-skills.name` (or `agent-skills.name`) entry to use the
/// matching `gh:<repo>@<ref>` URI instead.
///
/// Returns the number of unit URIs rewritten so the caller can
/// surface the count in the migration report.
///
/// Behavior is documented in the spec §Decisions Made #5
/// ("Legacy bootstrap.js YAML match is opt-in") and §User Flow 1
/// edge case ("must pass `--legacy-yaml=...` explicitly").
fn apply_legacy_yaml_match(patch: &mut ManifestPatch, path: &Path) -> Result<usize> {
    if !path.is_file() {
        bail!(
            "legacy yaml file `{}` does not exist or is not a regular file",
            path.display()
        );
    }
    let body = fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    let parsed: YamlValue = serde_yaml_ng::from_str(&body)
        .with_context(|| format!("parsing {}", path.display()))?;

    // name → (repo, ref, path_in_repo) — first hit wins per section,
    // sections checked in declaration order so a name in
    // `bundled-skills` masks the same name in `agent-skills`.
    let mut name_to_gh: HashMap<String, (String, String, String)> = HashMap::new();
    for section in ["bundled-skills", "agent-skills"] {
        let Some(list) = parsed.get(section).and_then(|v| v.as_sequence()) else {
            continue;
        };
        for entry in list {
            let Some(name) = entry.get("name").and_then(|v| v.as_str()) else {
                continue;
            };
            let Some(repo) = entry.get("repo").and_then(|v| v.as_str()) else {
                continue;
            };
            let r#ref = entry
                .get("ref")
                .and_then(|v| v.as_str())
                .unwrap_or("main")
                .to_string();
            let path_in_repo = entry
                .get("path")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("skills/{name}"));
            name_to_gh
                .entry(name.to_string())
                .or_insert((repo.to_string(), r#ref, path_in_repo));
        }
    }

    if name_to_gh.is_empty() {
        return Ok(0);
    }

    // Compute the new gh: source entries first (one per unique repo+ref
    // pair). We only emit a gh: source for repos that actually rewrite
    // at least one unit.
    let mut gh_sources_seen: HashMap<(String, String), String> = HashMap::new();
    let mut new_gh_sources: Vec<SourceEntry> = Vec::new();
    let mut rewrites = 0;

    let mut rewritten_units: Vec<UnitEntry> = Vec::with_capacity(patch.new_units.len());
    let mut local_source_uris_drained: std::collections::HashSet<String> =
        std::collections::HashSet::new();

    for unit in patch.new_units.drain(..) {
        // Only rewrite local: orphans (not marketplace: A units).
        if !unit.uri.starts_with("local:") {
            rewritten_units.push(unit);
            continue;
        }
        // Extract the unit name — last `/`-segment of the URI.
        let Some(name) = unit.uri.rsplit('/').next() else {
            rewritten_units.push(unit);
            continue;
        };
        let Some((repo, r#ref, path_in_repo)) = name_to_gh.get(name) else {
            rewritten_units.push(unit);
            continue;
        };
        // Track which local: source URI this unit pointed at so we
        // can drop it from new_sources if it's now orphaned.
        if let Some((local_src_uri, _)) = unit.uri.split_once('@') {
            local_source_uris_drained.insert(local_src_uri.to_string());
        }
        let new_uri = format!("gh:{repo}@{ref}/{path_in_repo}");
        let key = (repo.clone(), r#ref.clone());
        gh_sources_seen.entry(key).or_insert_with(|| {
            let source_name = format!("gh-{}", repo.replace('/', "-"));
            new_gh_sources.push(SourceEntry {
                name: source_name.clone(),
                kind: Some("gh".into()),
                uri: format!("gh:{repo}"),
                r#ref: r#ref.clone(),
                enabled: true,
                read_only: false,
                target_layout: Vec::new(),
            });
            source_name
        });
        rewritten_units.push(UnitEntry {
            uri: new_uri,
            targets: unit.targets,
            shadowed_by: unit.shadowed_by,
        });
        rewrites += 1;
    }
    patch.new_units = rewritten_units;

    // If a local: source no longer has any unit referencing it, drop
    // the source entry — it would otherwise dangle.
    if !local_source_uris_drained.is_empty() {
        let still_used: std::collections::HashSet<String> = patch
            .new_units
            .iter()
            .filter_map(|u| u.uri.split_once('@').map(|(src, _)| src.to_string()))
            .collect();
        patch
            .new_sources
            .retain(|s| !local_source_uris_drained.contains(&s.uri) || still_used.contains(&s.uri));
    }

    patch.new_sources.extend(new_gh_sources);
    Ok(rewrites)
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
