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
    AcceptDecision, ToolAdapter, adapter_by_name, all_adapters,
    install_root_for, plan::{InstallPlan, PlanOp}, read_root_for,
};
use ainb_skill_core::drift::{
    detect_all as drift_detect_all, DriftBackend, DriftStatus, GitLsRemoteBackend,
};
use ainb_skill_core::lockfile::{LockedUnit, Lockfile};
use ainb_skill_core::manifest::{Manifest, SourceEntry};
use ainb_skill_core::mapping::resolve_pair;
use ainb_skill_core::paths::{cache_dir_in, lockfile_path_in, manifest_path_in};
use ainb_skill_core::sync::{
    apply_to_home, apply_to_repo, ApplyToRepoOpts, ContentFetcher, FetchError as SyncFetchError,
    SyncAction, SyncDirection,
};
use ainb_skill_core::uri::Uri;
use ainb_skill_core::{DeployedRef, SourceType, UnitKind};
use ainb_usage::{InvocationRecord, detect_invocations};

use crate::source::run_fetcher;
use crate::{
    CheckArgs, InstallArgs, RemoveSkillArgs, SkillCommand, SyncArgs, UpdateArgs, UsageArgs,
};

pub fn dispatch(home: &Path, action: SkillCommand, out: &mut dyn io::Write) -> Result<()> {
    match action {
        SkillCommand::Install(args) => install(home, args, out),
        SkillCommand::Remove(args) => remove(home, args, out),
        SkillCommand::Update(args) => update(home, args, out),
        SkillCommand::Sync(args) => sync(home, args, out),
        SkillCommand::Promote(args) => crate::promote::dispatch(home, args, out),
        SkillCommand::Usage(args) => usage(home, args, out),
        SkillCommand::Check(args) => {
            // Production backend: real `git ls-remote`. Tests bypass
            // this top-level dispatch and call `run_check` directly
            // with a `MockBackend`.
            let backend = GitLsRemoteBackend::new();
            run_check(home, args, &backend, out)
        }
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
                // Remap every dst via the source's target_layout (or
                // the bootstrap defaults when it's empty). This is what
                // makes `target_layout` actually move files — the
                // adapter's `plan_install` still emits convention-layout
                // paths; we rewrite them here using `resolve_pair`. See
                // bead v12.C.4.
                let plan = remap_plan_via_target_layout(
                    source,
                    tool.name(),
                    &install_root_for(tool.name()),
                    plan,
                );
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
        usage: Default::default(),
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

/// `ainb skill sync`: bidirectional reconciliation in two passes.
///
/// 1. **Content sync** (Phase D, bidirectional): for every locked unit
///    sourced from a git-backed remote, compare home content vs repo
///    content and apply the planner's direction. `--to-home` /
///    `--to-repo` narrow which directions execute; default is both.
///    `local:` sources skip — there is no remote to push to.
///
/// 2. **Manifest reconciliation** (existing): install any manifest unit
///    missing from the lockfile; uninstall any lockfile unit whose
///    `declared_uri` is no longer in the manifest, or whose per-tool
///    entry is flagged `pending_uninstall`.
///
/// `--source-or-unit` scopes pass 1 to a single source name or unit
/// URI. Pass 2 always considers everything (its semantics depend on
/// the full manifest ↔ lockfile diff).
fn sync(home: &Path, args: SyncArgs, out: &mut dyn io::Write) -> Result<()> {
    let manifest = Manifest::load_from(&manifest_path_in(home))?;
    let lockfile = Lockfile::load_from(&lockfile_path_in(home))?;

    bidirectional_content_sync(home, &manifest, &lockfile, &args, out)?;

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

/// Phase-D bidirectional content sync (D.4) — for each locked unit
/// whose source is git-backed, plan the home↔repo direction and apply
/// via [`apply_to_home`] / [`apply_to_repo`].
///
/// Skips:
///   * units whose source is `local:` (no remote to push to);
///   * units whose source is not in the manifest (orphan — handled in
///     pass 2);
///   * actions whose direction the caller's `--to-home` / `--to-repo`
///     flags disable.
///
/// On `--dry-run`, prints the plan and returns without executing.
/// `args.source_or_unit`, when set, scopes the iteration to one
/// source (by source name) or one unit (by lockfile `declared_uri`).
fn bidirectional_content_sync(
    home: &Path,
    manifest: &Manifest,
    lockfile: &Lockfile,
    args: &SyncArgs,
    out: &mut dyn io::Write,
) -> Result<()> {
    // Direction allow-set: explicit flags narrow; neither means both.
    let allow_to_home = args.to_home || !args.to_repo; // default true
    let allow_to_repo = args.to_repo || !args.to_home; // default true

    let mut planned: Vec<(String, String, SyncDirection, String)> = Vec::new(); // (unit_uri, file_rel, dir, reason)
    let mut applied = 0usize;
    let mut skipped_local = 0usize;

    for unit in &lockfile.units {
        if let Some(filter) = args.source_or_unit.as_deref() {
            // Match by declared_uri or by source-name.
            if filter != unit.declared_uri {
                let Ok(uri) = Uri::parse(&unit.declared_uri) else { continue };
                let source_uri = format!("{}:{}", uri.source_type, uri.locator);
                let matches_source = manifest
                    .sources
                    .iter()
                    .find(|s| s.uri == source_uri)
                    .map(|s| s.name == filter)
                    .unwrap_or(false);
                if !matches_source {
                    continue;
                }
            }
        }

        let parsed = match Uri::parse(&unit.declared_uri) {
            Ok(u) => u,
            Err(_) => continue,
        };

        // Local sources: skip (no remote to commit to).
        if parsed.source_type == SourceType::Local {
            skipped_local += 1;
            continue;
        }

        // Source must still be in the manifest to know its ref / layout.
        let source_uri = format!("{}:{}", parsed.source_type, parsed.locator);
        let source = match manifest.sources.iter().find(|s| s.uri == source_uri) {
            Some(s) => s.clone(),
            None => continue,
        };

        // For each deployed tool entry, sync every file in its
        // file_hashes catalog (those are the install-root-relative
        // paths the install flow stored).
        for (tool_name, dref) in &unit.deployed {
            let DeployedRef::Deployed { file_hashes, .. } = dref else {
                continue;
            };
            let install_root = install_root_for(tool_name);

            for rel_str in file_hashes.keys() {
                let unit_path = PathBuf::from(rel_str);
                let home_file = install_root.join(&unit_path);
                let short_name =
                    unit_short_name(&unit.declared_uri).unwrap_or_else(|| unit.declared_uri.clone());
                let action = match plan_file_action_via_clone(
                    home,
                    &source,
                    &home_file,
                    &unit_path,
                    &short_name,
                ) {
                    Ok(Some(a)) => a,
                    Ok(None) => continue,
                    Err(e) => {
                        writeln!(out, "# skip {}/{rel_str}: {e}", unit.declared_uri)?;
                        continue;
                    }
                };

                // Direction gate.
                let allowed = match action.direction {
                    SyncDirection::ToHome => allow_to_home,
                    SyncDirection::ToRepo => allow_to_repo,
                    SyncDirection::NoOp => true,
                };
                if !allowed {
                    continue;
                }
                planned.push((
                    unit.declared_uri.clone(),
                    rel_str.clone(),
                    action.direction,
                    action.reason.clone(),
                ));

                if args.dry_run {
                    continue;
                }

                match action.direction {
                    SyncDirection::NoOp => {}
                    SyncDirection::ToHome => {
                        let fetcher = GitClonedFetcher {
                            home: home.to_path_buf(),
                            source: source.clone(),
                        };
                        apply_to_home(&action, &install_root, &source, &unit_path, &fetcher)
                            .with_context(|| {
                                format!("apply_to_home {}/{rel_str}", unit.declared_uri)
                            })?;
                        applied += 1;
                    }
                    SyncDirection::ToRepo => {
                        let cache_dir = ensure_repo_clone(home, &source)?;
                        let opts = ApplyToRepoOpts {
                            repo_cache_dir: cache_dir,
                        };
                        apply_to_repo(&action, &install_root, &source, &unit_path, &opts)
                            .with_context(|| {
                                format!("apply_to_repo {}/{rel_str}", unit.declared_uri)
                            })?;
                        applied += 1;
                    }
                }
            }
        }
    }

    // Render plan to the user — keeps dry-run + applied flows
    // observable. Empty plan stays silent so the existing manifest
    // reconciliation drives the "already in sync" message when
    // nothing else needs doing.
    if !planned.is_empty() {
        writeln!(out, "# content sync plan ({} action(s))", planned.len())?;
        for (uri, file, dir, reason) in &planned {
            let arrow = match dir {
                SyncDirection::ToHome => "<-",
                SyncDirection::ToRepo => "->",
                SyncDirection::NoOp => "==",
            };
            writeln!(out, "    {arrow} {uri}/{file}  ({reason})")?;
        }
        if args.dry_run {
            writeln!(out, "# dry-run: not applying")?;
        } else {
            writeln!(out, "# content sync applied {applied} action(s)")?;
        }
    }
    if skipped_local > 0 {
        writeln!(
            out,
            "# {skipped_local} unit(s) skipped (local: source — no remote)"
        )?;
    }
    Ok(())
}

/// Plan a per-file [`SyncAction`] by clone-or-pulling the source and
/// comparing the home file's bytes (+ mtime) against the upstream
/// file's bytes (+ mtime).
///
/// Returns:
///   * `Ok(None)` when neither side has the file (nothing to do).
///   * `Ok(Some(NoOp))` when both sides have byte-identical content.
///   * `Ok(Some(ToHome))` when only repo has the file, or the repo
///     copy is newer than the home copy.
///   * `Ok(Some(ToRepo))` when only home has the file, or the home
///     copy is newer than the repo copy.
///
/// Uses the file-level direct comparison rather than feeding into
/// [`plan_sync`] because the planner's per-unit `deployed_sha` model
/// does not carry per-file SHA precision; for content sync we want
/// "whichever side is byte-different and newer wins".
fn plan_file_action_via_clone(
    home: &Path,
    source: &SourceEntry,
    home_file: &Path,
    repo_rel: &Path,
    unit_uri: &str,
) -> Result<Option<SyncAction>> {
    let clone = ensure_repo_clone(home, source)?;
    let repo_file = clone.join(repo_rel);

    let home_bytes = std::fs::read(home_file).ok();
    let repo_bytes = std::fs::read(&repo_file).ok();

    let act = |dir: SyncDirection, reason: &str| {
        Some(SyncAction {
            unit_name: unit_uri.to_string(),
            direction: dir,
            reason: reason.to_string(),
        })
    };

    Ok(match (home_bytes, repo_bytes) {
        (None, None) => None,
        (Some(_), None) => act(SyncDirection::ToRepo, "home has file, repo does not"),
        (None, Some(_)) => act(SyncDirection::ToHome, "repo has file, home does not"),
        (Some(h), Some(r)) if h == r => act(SyncDirection::NoOp, "home and repo bytes match"),
        (Some(_), Some(_)) => {
            let home_mtime = file_mtime_unix(home_file);
            let repo_mtime = file_mtime_unix(&repo_file);
            match (home_mtime, repo_mtime) {
                (Some(h), Some(r)) if h >= r => {
                    act(SyncDirection::ToRepo, "home newer than repo by mtime")
                }
                (Some(_), Some(_)) => {
                    act(SyncDirection::ToHome, "repo newer than home by mtime")
                }
                // Indeterminate: prefer publishing local edits over
                // overwriting them. Mirrors `plan_sync`'s conservative
                // home-wins default.
                _ => act(SyncDirection::ToRepo, "mtime indeterminate; defaulting to home->repo"),
            }
        }
    })
}

fn file_mtime_unix(path: &Path) -> Option<i64> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs() as i64)
}

/// `ContentFetcher` that resolves files by ensuring a git clone of
/// the source under `<ainb-home>/promote-cache/<key>/` and reading
/// the file at `<clone>/<repo_path>`.
struct GitClonedFetcher {
    home: PathBuf,
    source: SourceEntry,
}

impl ContentFetcher for GitClonedFetcher {
    fn fetch_content(
        &self,
        _ref_name: &str,
        repo_path: &Path,
    ) -> std::result::Result<Vec<u8>, SyncFetchError> {
        let cache = ensure_repo_clone(&self.home, &self.source)
            .map_err(|e| SyncFetchError::Other(format!("clone-or-pull: {e}")))?;
        let target = cache.join(repo_path);
        std::fs::read(&target).map_err(|e| {
            SyncFetchError::Other(format!("read `{}`: {e}", target.display()))
        })
    }
}

/// Clone-or-pull the source repo into
/// `<ainb-home>/promote-cache/<sanitised-remote>/` and return the
/// checkout path. Sanitised remote key mirrors `promote.rs` so the
/// two flows share the same cache directory.
fn ensure_repo_clone(home: &Path, source: &SourceEntry) -> Result<PathBuf> {
    let remote = git_remote_url(source)?;
    let key = sanitise_cache_key(&remote);
    let cache = home.join("promote-cache").join(&key);

    if cache.join(".git").is_dir() {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&cache)
            .args(["pull", "--ff-only"])
            .output()
            .context("git pull --ff-only")?;
        if !out.status.success() {
            // Pull failure should not be fatal — the cache may be on
            // a branch behind origin/HEAD; future ops will surface
            // the real error. Log + continue.
            // Soft no-op (silent).
        }
    } else {
        if let Some(parent) = cache.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let out = std::process::Command::new("git")
            .args(["clone", &remote])
            .arg(&cache)
            .output()
            .context("git clone")?;
        if !out.status.success() {
            bail!(
                "git clone `{remote}` failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
    }
    Ok(cache)
}

/// Compute the git HTTPS / `file://` clone URL for a source URI.
/// Mirrors `git_remote_url` in `ainb-fetch::git` but works from a
/// `SourceEntry` (which is what the sync flow has on hand).
fn git_remote_url(source: &SourceEntry) -> Result<String> {
    let uri = Uri::parse(&source.uri)
        .with_context(|| format!("parsing source URI `{}`", source.uri))?;
    match uri.source_type {
        SourceType::Gh | SourceType::Marketplace => {
            Ok(format!("https://github.com/{}.git", uri.locator))
        }
        SourceType::Gist => Ok(format!("https://gist.github.com/{}.git", uri.locator)),
        SourceType::Git => Ok(uri.locator.clone()),
        other => bail!("source `{}` is unsupported for content sync", other.as_str()),
    }
}

/// Sanitise an arbitrary clone URL into a stable filesystem-safe
/// directory name. Mirrors `promote.rs::sanitise_cache_key`.
fn sanitise_cache_key(url: &str) -> String {
    let mut out = String::with_capacity(url.len());
    let mut last_underscore = false;
    for c in url.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            last_underscore = false;
        } else if !last_underscore {
            out.push('_');
            last_underscore = true;
        }
    }
    out.trim_matches('_').to_string()
}

/// `ainb skill usage [<unit-name>]`: refresh per-unit usage telemetry
/// in the lockfile.
///
/// For each targeted unit, scans every tool the unit is deployed to
/// (`read_root_for(tool)` — honours the `AINB_TOOL_HOME_<TOOL>` sandbox
/// override) and sums [`detect_invocations`] across them, keeping the
/// latest `last_used_at`. The aggregate is written to the unit's
/// lockfile `usage` record. Detection is a pure read of the tool homes;
/// the only mutation is the lockfile write, which is idempotent for
/// unchanged logs.
///
/// Note on scan surface: with no `AINB_TOOL_HOME_<TOOL>` override set,
/// `read_root_for` resolves each deployed tool's *real* home (`~/.claude`,
/// `~/.codex`, …) — so a unit installed to every tool is scanned across
/// every real tool home. This is a read-only telemetry scan; tests pin
/// each home to a tempdir via the env override.
fn usage(home: &Path, args: UsageArgs, out: &mut dyn io::Write) -> Result<()> {
    let mut lockfile = Lockfile::load_from(&lockfile_path_in(home))?;
    if lockfile.units.is_empty() {
        writeln!(out, "# no units in lockfile")?;
        return Ok(());
    }

    let target_indices: Vec<usize> = match args.unit_name.as_deref() {
        Some(name) => {
            let idxs: Vec<usize> = lockfile
                .units
                .iter()
                .enumerate()
                .filter(|(_, u)| matches_unit_name(&u.declared_uri, name))
                .map(|(i, _)| i)
                .collect();
            if idxs.is_empty() {
                bail!("no locked unit named `{name}`");
            }
            idxs
        }
        None => (0..lockfile.units.len()).collect(),
    };

    let mut total: u64 = 0;
    let mut refreshed = 0usize;
    for idx in target_indices {
        let (maybe_name, tools): (Option<String>, Vec<String>) = {
            let unit = &lockfile.units[idx];
            (unit_short_name(&unit.declared_uri), unit.deployed.keys().cloned().collect())
        };
        // A unit URI with no path yields no detectable name — querying
        // logs with a bogus name would silently record 0. Skip it
        // honestly rather than mis-attribute "never used".
        let Some(name) = maybe_name else {
            if args.verbose {
                writeln!(
                    out,
                    "# skip {}: cannot derive a unit name",
                    lockfile.units[idx].declared_uri
                )?;
            }
            continue;
        };

        // Sum detected invocations across every tool this unit deploys
        // to, keeping the most recent timestamp.
        let mut agg = InvocationRecord::default();
        for tool in &tools {
            let root = read_root_for(tool);
            let rec = detect_invocations(&root, &name);
            agg.invocations += rec.invocations;
            agg.last_used_at = later_timestamp(agg.last_used_at.take(), rec.last_used_at);
        }

        total += agg.invocations;
        if args.verbose {
            match &agg.last_used_at {
                Some(ts) => writeln!(out, "{name}: {} invocation(s), last used {ts}", agg.invocations)?,
                None => writeln!(out, "{name}: {} invocation(s)", agg.invocations)?,
            }
        }
        lockfile.units[idx].usage = agg.into();
        refreshed += 1;
    }

    lockfile.save_to(&lockfile_path_in(home))?;
    writeln!(out, "# usage refreshed for {refreshed} unit(s), {total} total invocation(s)")?;
    Ok(())
}

/// The short unit name — the trailing path segment of a unit URI, e.g.
/// `commit` for `gh:org/repo@main/skills/commit`. This is the name that
/// shows up in tool session logs (slash-command tags / `Skill` args),
/// so it is what [`detect_invocations`] matches against.
fn unit_short_name(declared_uri: &str) -> Option<String> {
    let uri = Uri::parse(declared_uri).ok()?;
    let path = uri.path?;
    Some(path.rsplit('/').next().unwrap_or(&path).to_string())
}

/// Whether `declared_uri` refers to the unit `name`, mirroring the
/// trailing-segment match used by `skill promote`.
fn matches_unit_name(declared_uri: &str, name: &str) -> bool {
    Uri::parse(declared_uri)
        .ok()
        .and_then(|u| u.path)
        .map(|p| p == name || p.ends_with(&format!("/{name}")))
        .unwrap_or(false)
}

/// Return the later of two optional RFC 3339 timestamps. Lexicographic
/// compare is correct for the canonical Zulu form these logs emit.
fn later_timestamp(a: Option<String>, b: Option<String>) -> Option<String> {
    match (a, b) {
        (Some(x), Some(y)) => Some(if y > x { y } else { x }),
        (Some(x), None) => Some(x),
        (None, b) => b,
    }
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

/// Rewrite every `dst` in `plan` through the source's `target_layout`
/// (or [`BOOTSTRAP_DEFAULT_MAPPINGS`] when it is empty) — bead v12.C.4.
///
/// The tool adapter's `plan_install` emits convention-layout paths
/// (`<install_root>/<rel>` where `rel` is the unit's repo-relative
/// path). When the source declares a custom `target_layout`, those
/// destinations must move. We re-resolve each rel via [`resolve_pair`]
/// and reroot it under `install_root`, stripping the leading
/// `.<tool>/` segment from the layout's `home` since `install_root_for`
/// already points at the tool home (it embeds `.claude` / `.codex`).
///
/// Paths that don't resolve through the layout (e.g. files outside any
/// declared glob) are left in their original convention location —
/// preserving today's behavior for legacy sources without a layout.
fn remap_plan_via_target_layout(
    source: &SourceEntry,
    tool_name: &str,
    install_root: &Path,
    mut plan: InstallPlan,
) -> InstallPlan {
    // Remap every op's dst.
    for op in plan.ops.iter_mut() {
        let dst_ptr: &mut PathBuf = match op {
            PlanOp::Create { dst, .. }
            | PlanOp::Update { dst, .. }
            | PlanOp::Delete { dst } => dst,
        };
        if let Some(new_dst) = remap_dst(source, tool_name, install_root, dst_ptr) {
            *dst_ptr = new_dst;
        }
    }

    // Remap unit_install_path. It points at the unit directory under
    // the install root (`<install_root>/<descriptor.path>`). We resolve
    // the directory rel through the same layout — most globs are file-
    // scoped (`skills/*/SKILL.md`) and won't match a directory, so we
    // also accept any first-file-mapped path's parent as a fallback.
    if let Some(new_unit) = remap_unit_install_path(source, tool_name, install_root, &plan) {
        plan.unit_install_path = new_unit;
    }

    plan
}

/// Map one absolute `dst` (under `install_root`) through the source's
/// layout. Returns `None` when the rel doesn't resolve — the caller
/// keeps the original dst.
fn remap_dst(
    source: &SourceEntry,
    tool_name: &str,
    install_root: &Path,
    dst: &Path,
) -> Option<PathBuf> {
    let rel = dst.strip_prefix(install_root).ok()?;
    let (home_rel, _repo_rel) = resolve_pair(source, rel)?;
    let stripped = strip_tool_dotdir(tool_name, &home_rel);
    Some(install_root.join(&stripped))
}

/// Strip a leading dot-prefixed segment (`.<X>/`) from `home_rel`
/// since `install_root_for(tool)` already points at the tool home.
///
/// The layout's `home` paths conventionally start with the tool's
/// dotdir (`.claude/...`, `.codex/...`) so a manifest authored with
/// "home is relative to the user `$HOME`" semantics still works. But
/// `install_root_for` already resolves *to* the tool home (it embeds
/// `.claude` / `.codex` for real-home mode, or points at the sandbox
/// for `AINB_TOOL_HOME_<TOOL>`). Joining the layout's `home` onto it
/// would double-count the dotdir.
///
/// Both the source's own explicit `target_layout` AND the bootstrap
/// defaults — which are always rooted at `.claude/...` regardless of
/// the active tool (they're claude-flavored, see
/// `BOOTSTRAP_DEFAULT_MAPPINGS` in `ainb-skill-core::mapping`) — go
/// through this strip. So a `.claude/skills` default applied to a
/// non-claude tool (amazonq, gemini, …) ends up at the tool's
/// `<install_root>/skills/...` rather than the absurd
/// `<install_root>/.claude/skills/...`.
///
/// We strip the **first** segment when it begins with `.`. Examples:
///   `.claude/skills/foo/SKILL.md` → `skills/foo/SKILL.md`
///   `.codex/agents/x.md`          → `agents/x.md`
///   `agents/x.md`                 → `agents/x.md` (no-op)
///
/// The `_tool_name` arg is kept for clarity / future per-tool needs
/// (e.g. `claude-desktop` whose real-home root isn't a dotdir).
fn strip_tool_dotdir(_tool_name: &str, home_rel: &Path) -> PathBuf {
    let mut comps = home_rel.components();
    if let Some(std::path::Component::Normal(os)) = comps.next() {
        if let Some(s) = os.to_str() {
            if s.starts_with('.') && s.len() > 1 {
                return comps.as_path().to_path_buf();
            }
        }
    }
    home_rel.to_path_buf()
}

/// Derive the unit's install-path (the directory the lockfile will
/// store as `deployed.path`) after the layout remap.
///
/// Strategy: resolve any one of the unit's files through the layout
/// and strip back the suffix that lives *inside* the unit. That suffix
/// is the original file's path relative to `descriptor.path` — the
/// same suffix the new dst must end with. What remains is the new
/// unit directory.
fn remap_unit_install_path(
    source: &SourceEntry,
    tool_name: &str,
    install_root: &Path,
    plan: &InstallPlan,
) -> Option<PathBuf> {
    let orig_unit_rel = plan.unit_install_path.strip_prefix(install_root).ok()?;
    // Pick the first op (any file under the unit will do).
    let op = plan.ops.first()?;
    let orig_file_abs = op.destination();
    let orig_file_rel = orig_file_abs.strip_prefix(install_root).ok()?;
    // The file's suffix inside the unit directory.
    let suffix_within_unit = orig_file_rel.strip_prefix(orig_unit_rel).ok()?;

    let (home_rel, _) = resolve_pair(source, orig_file_rel)?;
    let mapped_file_rel = strip_tool_dotdir(tool_name, &home_rel);

    // Drop the in-unit suffix off the mapped path's tail to recover the
    // mapped unit directory. `Path::strip_prefix` only does *leading*
    // stripping, so we do it by component count.
    let n = suffix_within_unit.components().count();
    let mut comps: Vec<_> = mapped_file_rel.components().collect();
    if comps.len() < n {
        return None;
    }
    comps.truncate(comps.len() - n);
    let mapped_unit_rel: PathBuf = comps.iter().collect();

    Some(install_root.join(mapped_unit_rel))
}

/// `ainb skill check [<source>] [--json]`: report drift between each
/// locked unit's pinned SHA and its source's current upstream tip.
/// Pure read; never mutates lockfile or unit files.
///
/// `backend` is the [`DriftBackend`] used to resolve upstream state —
/// the top-level dispatch passes a [`GitLsRemoteBackend`]; tests inject
/// a `MockBackend` so they stay offline.
///
/// Output:
/// - default: a tabular human report (unit URI + drift status column).
/// - `--json`: an array of `{ unit, status, ahead?, behind? }` objects,
///   parseable by scripts.
pub fn run_check(
    home: &Path,
    args: CheckArgs,
    backend: &dyn DriftBackend,
    out: &mut dyn io::Write,
) -> Result<()> {
    let manifest = Manifest::load_from(&manifest_path_in(home))?;
    let lockfile = Lockfile::load_from(&lockfile_path_in(home))?;

    if lockfile.units.is_empty() {
        writeln!(out, "# no units in lockfile")?;
        return Ok(());
    }

    // Per-source scope filter: when `args.source` is set, only units
    // whose declared_uri resolves to that source name are reported on.
    let source_scope: Option<String> = args.source.clone();
    let filtered_units: Vec<&LockedUnit> = lockfile
        .units
        .iter()
        .filter(|unit| {
            let Some(scope) = source_scope.as_deref() else {
                return true;
            };
            unit_source_name(unit, &manifest)
                .map(|n| n == scope)
                .unwrap_or(false)
        })
        .collect();

    if filtered_units.is_empty() {
        writeln!(out, "# no units match the requested source")?;
        return Ok(());
    }

    // Subset manifest/lockfile to scoped units so `drift_detect_all`
    // doesn't query outside the scope.
    let scoped_lockfile = Lockfile {
        schema_version: lockfile.schema_version,
        generated_at: lockfile.generated_at.clone(),
        sources: lockfile.sources.clone(),
        units: filtered_units.iter().map(|u| (*u).clone()).collect(),
    };
    let drift_map = drift_detect_all(&manifest, &scoped_lockfile, backend);

    if args.json {
        let rows: Vec<serde_json::Value> = scoped_lockfile
            .units
            .iter()
            .map(|u| drift_status_to_json(&u.declared_uri, drift_map.get(&u.declared_uri)))
            .collect();
        let payload = serde_json::Value::Array(rows);
        writeln!(out, "{}", serde_json::to_string_pretty(&payload)?)?;
        return Ok(());
    }

    // Tabular default: fixed-width columns for unit + status, no
    // external table dep.
    writeln!(out, "{:<60}  {}", "unit", "status")?;
    writeln!(out, "{:-<60}  {:-<24}", "", "")?;
    for unit in &scoped_lockfile.units {
        let status_str =
            drift_status_to_human(drift_map.get(&unit.declared_uri).copied());
        writeln!(out, "{:<60}  {}", unit.declared_uri, status_str)?;
    }
    writeln!(out, "# {} unit(s) checked", scoped_lockfile.units.len())?;
    Ok(())
}

/// Translate a [`DriftStatus`] into the JSON shape the CLI emits.
fn drift_status_to_json(unit: &str, status: Option<&DriftStatus>) -> serde_json::Value {
    use serde_json::json;
    match status {
        None => json!({ "unit": unit, "status": "unknown" }),
        Some(DriftStatus::InSync) => json!({ "unit": unit, "status": "in-sync" }),
        Some(DriftStatus::Outdated { behind }) => {
            json!({ "unit": unit, "status": "outdated", "behind": behind })
        }
        Some(DriftStatus::Ahead { ahead }) => {
            json!({ "unit": unit, "status": "ahead", "ahead": ahead })
        }
        Some(DriftStatus::Diverged { ahead, behind }) => {
            json!({
                "unit": unit,
                "status": "diverged",
                "ahead": ahead,
                "behind": behind,
            })
        }
    }
}

/// Render a [`DriftStatus`] as a human-readable cell for the tabular
/// output (`"in-sync"`, `"outdated (behind 4)"`, …).
fn drift_status_to_human(status: Option<DriftStatus>) -> String {
    match status {
        None => "unknown".to_string(),
        Some(DriftStatus::InSync) => "in-sync".to_string(),
        Some(DriftStatus::Outdated { behind }) => {
            if behind == 0 {
                "outdated".to_string()
            } else {
                format!("outdated (behind {behind})")
            }
        }
        Some(DriftStatus::Ahead { ahead }) => {
            format!("ahead {ahead}")
        }
        Some(DriftStatus::Diverged { ahead, behind }) => {
            format!("diverged (ahead {ahead}, behind {behind})")
        }
    }
}

/// Look up the source name for a locked unit by re-parsing its URI and
/// matching against `manifest.sources`. Returns `None` when the source
/// is no longer declared (orphan unit) — those are excluded from
/// per-source scopes.
fn unit_source_name(unit: &LockedUnit, manifest: &Manifest) -> Option<String> {
    let uri = Uri::parse(&unit.declared_uri).ok()?;
    let source_uri = format!("{}:{}", uri.source_type, uri.locator);
    manifest.sources.iter().find(|s| s.uri == source_uri).map(|s| s.name.clone())
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
