//! `ainb skill ...` subcommand — install / remove / update / sync
//! units via tool adapters, with the diff-and-confirm flow from spec
//! §8.5.
//!
//! P3 shipped install + remove. P5 adds:
//!   - update --check [uri]: re-fetch each source, compare locked SHA
//!     vs upstream SHA, report drift. Read-only.
//!   - update [uri] / update --all: re-fetch + re-resolve + diff-
//!     confirm + apply for one or every locked unit. Lockfile records
//!     new SHA + new file hashes.
//!   - sync: reconcile manifest with lockfile. Install units declared
//!     in manifest but missing from lock; uninstall units the lockfile
//!     holds but the manifest no longer mentions (or that `source
//!     remove` flagged `pending_uninstall`).
//!
//! Every mutating flow takes `--yes` (skip prompt) and `--dry-run`
//! (render the plan without mutating). `--targets t1,t2` narrows the
//! adapter set for install / remove.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};

use ainb_adapters_source::pick_adapter;
use ainb_adapters_tool::{
    AcceptDecision, ToolAdapter, adapter_by_name, all_adapters, plan::InstallPlan,
};
use ainb_skill_core::lockfile::{LockedUnit, Lockfile};
use ainb_skill_core::manifest::Manifest;
use ainb_skill_core::paths::{cache_dir_in, lockfile_path_in, manifest_path_in};
use ainb_skill_core::uri::Uri;
use ainb_skill_core::{DeployedRef, UnitKind};

use crate::source::run_fetcher;
use crate::{InstallArgs, RemoveSkillArgs, SkillCommand, SyncArgs, UpdateArgs};

pub fn dispatch(home: &Path, action: SkillCommand, out: &mut dyn io::Write) -> Result<()> {
    match action {
        SkillCommand::Install(args) => install(home, args, out),
        SkillCommand::Remove(args) => remove(home, args, out),
        SkillCommand::Update(args) => update(home, args, out),
        SkillCommand::Sync(args) => sync(home, args, out),
        SkillCommand::Promote(args) => crate::promote::dispatch(home, args, out),
    }
}

fn install(home: &Path, args: InstallArgs, out: &mut dyn io::Write) -> Result<()> {
    let uri = Uri::parse(&args.uri).with_context(|| format!("parsing `{}`", args.uri))?;
    if !uri.is_unit() {
        bail!(
            "`{}` is not a unit URI — expected `<source>@<ref>/<path>`",
            args.uri
        );
    }

    // Look up the source matching this URI in the user manifest.
    let manifest = Manifest::load_from(&manifest_path_in(home))?;
    let source_uri = format!("{}:{}", uri.source_type, uri.locator);
    let source =
        manifest
            .sources
            .iter()
            .find(|s| s.uri == source_uri && s.enabled)
            .ok_or_else(|| {
                anyhow!(
                    "no enabled source matches `{source_uri}` — run \
                 `ainb source add {source_uri}` first"
                )
            })?;

    // Find the fetched checkout via lockfile.
    let mut lockfile = Lockfile::load_from(&lockfile_path_in(home))?;
    let locked = lockfile
        .sources
        .iter()
        .find(|s| s.name == source.name)
        .ok_or_else(|| anyhow!("source `{}` not yet fetched", source.name))?;
    let fetched_path: PathBuf = locked
        .fetched_path
        .as_deref()
        .ok_or_else(|| anyhow!("source `{}` has no fetched_path", source.name))?
        .into();
    if !fetched_path.exists() {
        bail!(
            "fetched source dir `{}` is missing — re-run `ainb source add` to refresh",
            fetched_path.display()
        );
    }

    // Resolve the unit and pick a source adapter for it.
    let adapter = pick_adapter(&fetched_path)
        .ok_or_else(|| anyhow!("no source adapter matched `{}`", fetched_path.display()))?;
    let unit_rel = uri.path.as_deref().ok_or_else(|| anyhow!("unit URI has no path"))?;
    let resolved = adapter
        .resolve_unit(&fetched_path, unit_rel)
        .with_context(|| format!("resolving unit `{unit_rel}`"))?;

    let kind: UnitKind = resolved
        .descriptor
        .kind
        .parse()
        .with_context(|| format!("unit kind `{}`", resolved.descriptor.kind))?;

    // Pick target tools.
    let targets = parse_targets(args.targets.as_deref())?;

    // Build plans + collect skipped tools.
    let mut plans: Vec<(String, InstallPlan)> = Vec::new();
    let mut skipped: Vec<(String, String)> = Vec::new();
    for tool in targets {
        match tool.accepts(kind) {
            AcceptDecision::Yes => {
                let plan = tool
                    .plan_install(&resolved)
                    .with_context(|| format!("planning {} install", tool.name()))?;
                plans.push((tool.name().to_string(), plan));
            }
            AcceptDecision::No { reason } => {
                skipped.push((tool.name().to_string(), reason));
            }
        }
    }

    // Render diff for every plan.
    let plan_refs: Vec<InstallPlan> = plans.iter().map(|(_, p)| p.clone()).collect();
    let diff = ainb_diff::render_plans(&plan_refs);
    writeln!(out, "{diff}")?;
    for (name, reason) in &skipped {
        writeln!(out, "# skipped {name}: {reason}")?;
    }

    if args.dry_run {
        writeln!(out, "# dry-run: not applying")?;
        return Ok(());
    }
    if !args.yes {
        bail!("interactive confirmation not yet wired in P3 — pass --yes (or --dry-run)");
    }

    // Apply each plan.
    let mut deployed_map: BTreeMap<String, DeployedRef> = BTreeMap::new();
    for (tool_name, plan) in &plans {
        let tool =
            adapter_by_name(tool_name).ok_or_else(|| anyhow!("unknown tool `{tool_name}`"))?;
        let report = tool.apply(plan).with_context(|| format!("applying {tool_name} plan"))?;
        deployed_map.insert(
            tool_name.clone(),
            DeployedRef::Deployed {
                path: report.path.to_string_lossy().to_string(),
                file_hashes: report.file_hashes.clone(),
            },
        );
    }
    for (tool_name, reason) in &skipped {
        deployed_map.insert(
            tool_name.clone(),
            DeployedRef::Skipped {
                reason: reason.clone(),
            },
        );
    }

    // Replace or insert the LockedUnit.
    lockfile.units.retain(|u| u.declared_uri != args.uri);
    lockfile.units.push(LockedUnit {
        uri: args.uri.clone(),
        declared_uri: args.uri.clone(),
        kind: kind.to_string(),
        sha: locked.resolved_sha.clone(),
        deployed: deployed_map,
    });
    lockfile.save_to(&lockfile_path_in(home))?;

    writeln!(
        out,
        "installed {} → {} tool(s), {} skipped",
        args.uri,
        plans.len(),
        skipped.len()
    )?;
    Ok(())
}

fn remove(home: &Path, args: RemoveSkillArgs, out: &mut dyn io::Write) -> Result<()> {
    let mut lockfile = Lockfile::load_from(&lockfile_path_in(home))?;
    let Some(idx) = lockfile.units.iter().position(|u| u.declared_uri == args.uri) else {
        bail!("`{}` is not in the lockfile — nothing to remove", args.uri);
    };

    // Decide which tool entries to touch.
    let target_filter: Option<Vec<String>> = args
        .targets
        .as_deref()
        .map(|s| s.split(',').map(|n| n.trim().to_string()).collect());

    // Render planned deletions for the user.
    {
        let unit = &lockfile.units[idx];
        writeln!(out, "# uninstall {}", unit.declared_uri)?;
        for (tool, dref) in &unit.deployed {
            if let Some(filter) = &target_filter {
                if !filter.contains(tool) {
                    continue;
                }
            }
            match dref {
                DeployedRef::Deployed { path, file_hashes } => {
                    writeln!(out, "- {tool} @ {path} ({} file(s))", file_hashes.len())?;
                }
                DeployedRef::Skipped { reason } => {
                    writeln!(out, "- {tool}: skipped previously ({reason})")?;
                }
                DeployedRef::PendingUninstall => {
                    writeln!(out, "- {tool}: already flagged pending_uninstall")?;
                }
            }
        }
    }

    if args.dry_run {
        writeln!(out, "# dry-run: not removing")?;
        return Ok(());
    }
    if !args.yes {
        bail!("interactive confirmation not yet wired — pass --yes (or --dry-run)");
    }

    // Apply uninstall for every selected Deployed entry.
    let unit_clone = lockfile.units[idx].clone();
    let mut removed_tools: Vec<String> = Vec::new();
    let mut retained: BTreeMap<String, DeployedRef> = BTreeMap::new();
    for (tool_name, dref) in unit_clone.deployed {
        let in_filter = target_filter.as_ref().map(|f| f.contains(&tool_name)).unwrap_or(true);
        if !in_filter {
            retained.insert(tool_name, dref);
            continue;
        }
        if let DeployedRef::Deployed { .. } = &dref {
            let tool = adapter_by_name(&tool_name)
                .ok_or_else(|| anyhow!("unknown tool `{tool_name}` in lockfile"))?;
            tool.uninstall(&dref)
                .with_context(|| format!("uninstalling from {tool_name}"))?;
            removed_tools.push(tool_name);
        } else {
            // Skipped / PendingUninstall — drop the entry, no fs op.
            removed_tools.push(tool_name);
        }
    }

    if retained.is_empty() {
        lockfile.units.remove(idx);
    } else {
        lockfile.units[idx].deployed = retained;
    }
    lockfile.save_to(&lockfile_path_in(home))?;

    writeln!(
        out,
        "removed {} from {} tool(s)",
        args.uri,
        removed_tools.len()
    )?;
    Ok(())
}

fn parse_targets(spec: Option<&str>) -> Result<Vec<Box<dyn ToolAdapter>>> {
    let Some(spec) = spec else {
        return Ok(all_adapters());
    };
    let mut out: Vec<Box<dyn ToolAdapter>> = Vec::new();
    for name in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let tool =
            adapter_by_name(name).ok_or_else(|| anyhow!("unknown tool `{name}` in --targets"))?;
        out.push(tool);
    }
    if out.is_empty() {
        bail!("--targets resolved to no tools");
    }
    Ok(out)
}

/// `ainb skill update [...]` dispatcher.
fn update(home: &Path, args: UpdateArgs, out: &mut dyn io::Write) -> Result<()> {
    if args.check {
        return update_check(home, args.uri.as_deref(), out);
    }
    if !args.all && args.uri.is_none() {
        bail!("specify a unit URI or pass --all");
    }
    update_apply(home, args, out)
}

/// `ainb skill update --check`: re-fetch each source and report drift
/// between the upstream SHA and the lockfile SHA. Never mutates the
/// lockfile or any on-disk unit. If `name_or_uri` is given, the report
/// is scoped to that one unit's source.
fn update_check(home: &Path, name_or_uri: Option<&str>, out: &mut dyn io::Write) -> Result<()> {
    let manifest = Manifest::load_from(&manifest_path_in(home))?;
    let mut lockfile = Lockfile::load_from(&lockfile_path_in(home))?;

    let source_filter = scope_to_source_names(&manifest, &lockfile, name_or_uri)?;
    let cache_root = cache_dir_in(home);
    let mut rows = 0;
    let mut drift_count = 0;
    for locked in lockfile.sources.iter_mut() {
        if let Some(filter) = &source_filter {
            if !filter.contains(&locked.name) {
                continue;
            }
        }
        let Some(entry) = manifest.sources.iter().find(|e| e.name == locked.name) else {
            continue;
        };
        if !entry.enabled {
            writeln!(out, "- {} (disabled, skipped)", locked.name)?;
            continue;
        }
        let uri = Uri::parse(&format!("{}@{}", entry.uri, entry.r#ref))
            .with_context(|| format!("parsing source URI for `{}`", locked.name))?;
        match run_fetcher(&uri, &locked.name, &cache_root) {
            Ok(fetched) => {
                let locked_sha = locked.resolved_sha.clone().unwrap_or_default();
                let drift = fetched.resolved_sha != locked_sha;
                if drift {
                    drift_count += 1;
                    writeln!(
                        out,
                        "- {}: drift {} → {} (declared ref `{}`)",
                        locked.name,
                        short_sha(&locked_sha),
                        short_sha(&fetched.resolved_sha),
                        entry.r#ref
                    )?;
                } else {
                    writeln!(
                        out,
                        "- {}: up to date ({})",
                        locked.name,
                        short_sha(&locked_sha),
                    )?;
                }
                rows += 1;
            }
            Err(err) => {
                writeln!(out, "- {}: fetch failed — {err}", locked.name)?;
            }
        }
    }
    if rows == 0 {
        writeln!(out, "# no sources checked")?;
    }
    writeln!(out, "# {rows} source(s) inspected, {drift_count} drift")?;
    Ok(())
}

/// `ainb skill update <uri>` / `--all`: re-fetch the source(s), re-
/// resolve the unit(s), and re-run plan/apply for each tool currently
/// holding a Deployed entry in the lockfile. Skips units whose source
/// SHA matches the lockfile's stored SHA unless the on-disk content
/// itself diverges.
fn update_apply(home: &Path, args: UpdateArgs, out: &mut dyn io::Write) -> Result<()> {
    let manifest = Manifest::load_from(&manifest_path_in(home))?;
    let mut lockfile = Lockfile::load_from(&lockfile_path_in(home))?;
    if lockfile.units.is_empty() {
        writeln!(out, "# no units to update")?;
        return Ok(());
    }

    // Resolve which lockfile units to touch.
    let mut target_indices: Vec<usize> = Vec::new();
    if args.all {
        target_indices.extend(0..lockfile.units.len());
    } else if let Some(uri) = args.uri.as_deref() {
        let idx = lockfile
            .units
            .iter()
            .position(|u| u.declared_uri == uri)
            .ok_or_else(|| anyhow!("`{uri}` is not in the lockfile"))?;
        target_indices.push(idx);
    }

    let cache_root = cache_dir_in(home);
    let mut all_plans: Vec<(usize, String, InstallPlan)> = Vec::new();
    let mut deferred_sha_updates: BTreeMap<String, (String, String)> = BTreeMap::new();
    let mut skipped_unchanged: Vec<String> = Vec::new();
    let mut had_errors = false;

    for idx in &target_indices {
        let unit = &lockfile.units[*idx];
        let uri = Uri::parse(&unit.declared_uri)
            .with_context(|| format!("parsing `{}`", unit.declared_uri))?;
        let source_uri = format!("{}:{}", uri.source_type, uri.locator);
        let source = match manifest.sources.iter().find(|s| s.uri == source_uri && s.enabled) {
            Some(s) => s,
            None => {
                writeln!(
                    out,
                    "# skip {}: no enabled source matches `{}`",
                    unit.declared_uri, source_uri
                )?;
                had_errors = true;
                continue;
            }
        };

        // Re-fetch the source.
        let source_uri_full = Uri::parse(&format!("{}@{}", source.uri, source.r#ref))
            .with_context(|| format!("parsing source URI `{}@{}`", source.uri, source.r#ref))?;
        let fetched = match run_fetcher(&source_uri_full, &source.name, &cache_root) {
            Ok(f) => f,
            Err(e) => {
                writeln!(out, "# skip {}: fetch failed — {e}", unit.declared_uri)?;
                had_errors = true;
                continue;
            }
        };
        let previous_sha = unit.sha.clone().unwrap_or_default();
        if fetched.resolved_sha == previous_sha {
            skipped_unchanged.push(unit.declared_uri.clone());
            continue;
        }

        // Resolve the unit at the new SHA.
        let adapter = pick_adapter(&fetched.path)
            .ok_or_else(|| anyhow!("no source adapter for `{}`", fetched.path.display()))?;
        let unit_rel = uri.path.as_deref().ok_or_else(|| anyhow!("unit URI has no path"))?;
        let resolved = adapter
            .resolve_unit(&fetched.path, unit_rel)
            .with_context(|| format!("resolving `{unit_rel}`"))?;

        // Plan against the tools that hold a Deployed entry today.
        let unit = lockfile.units[*idx].clone();
        for (tool_name, dref) in &unit.deployed {
            if !matches!(dref, DeployedRef::Deployed { .. }) {
                continue;
            }
            let tool = adapter_by_name(tool_name)
                .ok_or_else(|| anyhow!("unknown tool `{tool_name}` in lockfile"))?;
            let plan = tool.plan_install(&resolved).with_context(|| {
                format!("planning {tool_name} update for {}", unit.declared_uri)
            })?;
            all_plans.push((*idx, tool_name.clone(), plan));
        }
        deferred_sha_updates.insert(
            unit.declared_uri.clone(),
            (
                fetched.resolved_sha.clone(),
                fetched.path.to_string_lossy().to_string(),
            ),
        );
    }

    if all_plans.is_empty() {
        for uri in &skipped_unchanged {
            writeln!(out, "# {uri}: up to date")?;
        }
        if had_errors {
            bail!("update aborted with errors");
        }
        writeln!(out, "# nothing to apply")?;
        return Ok(());
    }

    // Render every plan.
    let plan_refs: Vec<InstallPlan> = all_plans.iter().map(|(_, _, p)| p.clone()).collect();
    let diff = ainb_diff::render_plans(&plan_refs);
    writeln!(out, "{diff}")?;
    for uri in &skipped_unchanged {
        writeln!(out, "# {uri}: up to date")?;
    }
    if args.dry_run {
        writeln!(out, "# dry-run: not applying")?;
        return Ok(());
    }
    if !args.yes {
        bail!("interactive confirmation not yet wired — pass --yes (or --dry-run)");
    }

    // Apply every plan + record new deployed hashes.
    let mut new_hashes: BTreeMap<(usize, String), DeployedRef> = BTreeMap::new();
    for (idx, tool_name, plan) in &all_plans {
        let tool =
            adapter_by_name(tool_name).ok_or_else(|| anyhow!("unknown tool `{tool_name}`"))?;
        let report = tool.apply(plan).with_context(|| format!("applying {tool_name} update"))?;
        new_hashes.insert(
            (*idx, tool_name.clone()),
            DeployedRef::Deployed {
                path: report.path.to_string_lossy().to_string(),
                file_hashes: report.file_hashes.clone(),
            },
        );
    }

    // Mutate lockfile: overwrite deployed entries + bump SHA per unit.
    for ((idx, tool_name), dref) in new_hashes {
        lockfile.units[idx].deployed.insert(tool_name, dref);
    }
    for (declared_uri, (new_sha, _fetched_path)) in deferred_sha_updates {
        if let Some(unit) = lockfile.units.iter_mut().find(|u| u.declared_uri == declared_uri) {
            unit.sha = Some(new_sha.clone());
        }
        // Also bump LockedSource.resolved_sha for the source this unit
        // belongs to so update --check reports clean immediately after.
        let uri = Uri::parse(&declared_uri)?;
        let source_uri = format!("{}:{}", uri.source_type, uri.locator);
        if let Some(source_entry) = manifest.sources.iter().find(|s| s.uri == source_uri) {
            if let Some(lsource) = lockfile.sources.iter_mut().find(|s| s.name == source_entry.name)
            {
                lsource.resolved_sha = Some(new_sha);
            }
        }
    }
    lockfile.save_to(&lockfile_path_in(home))?;

    let updated = all_plans.iter().map(|(idx, _, _)| *idx).collect::<BTreeSet<_>>();
    writeln!(
        out,
        "updated {} unit(s); {} unchanged",
        updated.len(),
        skipped_unchanged.len()
    )?;
    if had_errors {
        bail!("update completed with errors");
    }
    Ok(())
}

/// `ainb skill sync`: reconcile manifest with lockfile. Install any
/// manifest unit not yet in the lockfile; uninstall any lockfile unit
/// whose `declared_uri` no longer appears in the manifest, or whose
/// per-tool entry is flagged `pending_uninstall`.
fn sync(home: &Path, args: SyncArgs, out: &mut dyn io::Write) -> Result<()> {
    let manifest = Manifest::load_from(&manifest_path_in(home))?;
    let lockfile = Lockfile::load_from(&lockfile_path_in(home))?;

    let declared: BTreeSet<String> = manifest.units.iter().map(|u| u.uri.clone()).collect();
    let locked: BTreeSet<String> = lockfile.units.iter().map(|u| u.declared_uri.clone()).collect();

    let missing: Vec<String> = declared.difference(&locked).cloned().collect();
    let orphans: Vec<String> = locked.difference(&declared).cloned().collect();
    let pending_uninstall: Vec<String> = lockfile
        .units
        .iter()
        .filter(|u| u.deployed.values().any(|d| matches!(d, DeployedRef::PendingUninstall)))
        .map(|u| u.declared_uri.clone())
        .collect();

    writeln!(out, "# sync plan")?;
    writeln!(out, "  install:  {}", missing.len())?;
    for u in &missing {
        writeln!(out, "    + {u}")?;
    }
    writeln!(out, "  uninstall: {}", orphans.len())?;
    for u in &orphans {
        writeln!(out, "    - {u}")?;
    }
    writeln!(out, "  pending_uninstall: {}", pending_uninstall.len())?;
    for u in &pending_uninstall {
        writeln!(out, "    - {u}")?;
    }
    if missing.is_empty() && orphans.is_empty() && pending_uninstall.is_empty() {
        writeln!(out, "# already in sync")?;
        return Ok(());
    }

    if args.dry_run {
        writeln!(out, "# dry-run: not applying")?;
        return Ok(());
    }
    if !args.yes {
        bail!("interactive confirmation not yet wired — pass --yes (or --dry-run)");
    }

    // Install missing units via the same path `skill install` takes,
    // honoring the manifest's per-unit `targets` override.
    let mut install_count = 0;
    for uri in &missing {
        let targets = manifest.units.iter().find(|u| u.uri == *uri).and_then(|u| u.targets.clone());
        let install_args = InstallArgs {
            uri: uri.clone(),
            targets: targets.map(|v| v.join(",")),
            dry_run: false,
            yes: true,
        };
        install(home, install_args, out).with_context(|| format!("installing {uri}"))?;
        install_count += 1;
    }

    // Uninstall orphans + pending_uninstall via the same path
    // `skill remove` takes.
    let mut remove_count = 0;
    for uri in orphans.iter().chain(pending_uninstall.iter()) {
        let remove_args = RemoveSkillArgs {
            uri: uri.clone(),
            targets: None,
            dry_run: false,
            yes: true,
        };
        // remove() bails when the URI is missing; tolerate that for
        // pending_uninstall entries whose owning unit is also in
        // orphans.
        if let Err(e) = remove(home, remove_args, out) {
            writeln!(out, "# remove {uri}: {e}")?;
            continue;
        }
        remove_count += 1;
    }

    writeln!(
        out,
        "# sync done: {install_count} installed, {remove_count} removed"
    )?;
    Ok(())
}

/// Translate a positional `uri` (or short unit name in the future)
/// into the set of source names whose units it scopes. Returns `None`
/// when no filter was specified (everything is in scope).
fn scope_to_source_names(
    manifest: &Manifest,
    lockfile: &Lockfile,
    name_or_uri: Option<&str>,
) -> Result<Option<BTreeSet<String>>> {
    let Some(input) = name_or_uri else {
        return Ok(None);
    };
    // Treat the argument as a unit URI first.
    if let Ok(uri) = Uri::parse(input) {
        if uri.is_unit() {
            let source_uri = format!("{}:{}", uri.source_type, uri.locator);
            let name =
                manifest.sources.iter().find(|s| s.uri == source_uri).map(|s| s.name.clone());
            if let Some(name) = name {
                return Ok(Some([name].into_iter().collect()));
            }
        }
    }
    // Otherwise look up a lockfile unit whose declared_uri matches.
    if let Some(unit) = lockfile.units.iter().find(|u| u.declared_uri == input) {
        let uri = Uri::parse(&unit.declared_uri)?;
        let source_uri = format!("{}:{}", uri.source_type, uri.locator);
        if let Some(source) = manifest.sources.iter().find(|s| s.uri == source_uri) {
            return Ok(Some([source.name.clone()].into_iter().collect()));
        }
    }
    bail!("`{input}` does not resolve to a known unit or source");
}

/// Render a short SHA prefix for human-readable drift reports. Falls
/// back to `"—"` if the SHA is empty.
fn short_sha(s: &str) -> &str {
    if s.is_empty() {
        "—"
    } else if s.len() <= 12 {
        s
    } else {
        &s[..12]
    }
}
