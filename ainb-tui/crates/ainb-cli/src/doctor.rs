//! `ainb doctor` — single-shot health check (spec §8.7).
//!
//! Aggregates every check before reporting, so the user sees the
//! whole picture instead of bailing on the first issue. Returns
//! `Err` at the end if any check failed; the CLI runner then exits
//! non-zero. Each check writes a `✓` / `✗` line + reason.
//!
//! Checks:
//!   - Manifest schema parses cleanly.
//!   - Lockfile schema parses cleanly.
//!   - Every lockfile `DeployedRef::Deployed` file exists on disk
//!     and its sha256 matches the recorded `sha256:` prefix.
//!   - Every LockedSource has a `fetched_path` that still exists
//!     (cache wasn't cleared).
//!   - (when not --offline) every enabled source re-fetches successfully.
//!   - Drift: each adapter's `list_installed` returns files the
//!     lockfile doesn't recognise (or vice versa).

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::Path;

use anyhow::{Result, bail};

use ainb_adapters_tool::{all_adapters, plan::hex_sha256};
use ainb_skill_core::lockfile::{DeployedRef, Lockfile};
use ainb_skill_core::manifest::Manifest;
use ainb_skill_core::paths::{cache_dir_in, lockfile_path_in, manifest_path_in};
use ainb_skill_core::uri::Uri;

use crate::DoctorArgs;
use crate::source::run_fetcher;

pub fn dispatch(home: &Path, args: DoctorArgs, out: &mut dyn io::Write) -> Result<()> {
    let mut issues: Vec<String> = Vec::new();
    let mut writeline = |line: &str, problem: bool, msg: &str| -> io::Result<()> {
        if problem {
            issues.push(format!("{line}: {msg}"));
            writeln!(out, "✗ {line}: {msg}")
        } else {
            writeln!(out, "✓ {line}: {msg}")
        }
    };

    // 1. Manifest schema.
    let manifest_path = manifest_path_in(home);
    let manifest = match Manifest::load_from(&manifest_path) {
        Ok(m) => {
            writeline("manifest", false, "schema OK")?;
            m
        }
        Err(e) => {
            writeline("manifest", true, &format!("{e}"))?;
            return finalize(out, issues);
        }
    };

    // 2. Lockfile schema.
    let lockfile_path = lockfile_path_in(home);
    let lockfile = match Lockfile::load_from(&lockfile_path) {
        Ok(l) => {
            writeline("lockfile", false, "schema OK")?;
            l
        }
        Err(e) => {
            writeline("lockfile", true, &format!("{e}"))?;
            return finalize(out, issues);
        }
    };

    // 3. Per-unit file-hash consistency.
    //
    // The lockfile records `file_hashes` keyed by paths relative to
    // each adapter's install_root (the same `rel` strings the
    // adapter writes through `convention::plan_install_prefix_swap`).
    // Resolve by joining install_root with `rel`; fall back to
    // joining the recorded `path` only when the tool is unknown
    // (legacy or removed adapter).
    let mut file_problems = 0;
    let mut deployed_paths: BTreeSet<String> = BTreeSet::new();
    for unit in &lockfile.units {
        for (tool, dref) in &unit.deployed {
            let DeployedRef::Deployed { path, file_hashes } = dref else {
                continue;
            };
            let base = Path::new(path);
            for (rel, recorded) in file_hashes {
                let candidate = match ainb_adapters_tool::adapter_by_name(tool) {
                    Some(adapter) => adapter.install_root().join(rel),
                    None => base.join(rel),
                };
                if !candidate.exists() {
                    writeline(
                        "file",
                        true,
                        &format!("{} (tool {tool}) missing on disk", candidate.display()),
                    )?;
                    file_problems += 1;
                    continue;
                }
                let body = fs::read(&candidate).unwrap_or_default();
                let recorded_hex = recorded.trim_start_matches("sha256:");
                let actual_hex = hex_sha256(&body);
                if recorded_hex != actual_hex {
                    writeline(
                        "file",
                        true,
                        &format!(
                            "{} (tool {tool}) hash mismatch: recorded {} != actual {}",
                            candidate.display(),
                            short(recorded_hex),
                            short(&actual_hex)
                        ),
                    )?;
                    file_problems += 1;
                } else {
                    deployed_paths.insert(candidate.to_string_lossy().to_string());
                }
            }
        }
    }
    if file_problems == 0 {
        writeline(
            "deployed files",
            false,
            &format!("{} OK", deployed_paths.len()),
        )?;
    }

    // 4. LockedSource fetched_path exists.
    let mut source_problems = 0;
    let cache_root = cache_dir_in(home);
    for locked in &lockfile.sources {
        let Some(fetched) = locked.fetched_path.as_deref() else {
            writeline(
                "source",
                true,
                &format!("{}: no fetched_path recorded", locked.name),
            )?;
            source_problems += 1;
            continue;
        };
        let p = Path::new(fetched);
        if !p.exists() {
            writeline(
                "source",
                true,
                &format!("{}: cache `{}` is missing", locked.name, fetched),
            )?;
            source_problems += 1;
            continue;
        }
        writeline(
            "source",
            false,
            &format!("{}: cache present at {}", locked.name, fetched),
        )?;
    }
    let _ = cache_root; // reserved for future cache audits.

    // 5. Source reachability (skipped when --offline).
    if !args.offline {
        for entry in &manifest.sources {
            if !entry.enabled {
                writeline(
                    "reachability",
                    false,
                    &format!("{}: disabled, skipped", entry.name),
                )?;
                continue;
            }
            let uri_str = format!("{}@{}", entry.uri, entry.r#ref);
            match Uri::parse(&uri_str) {
                Ok(uri) => match run_fetcher(&uri, &entry.name, &cache_dir_in(home)) {
                    Ok(_) => {
                        writeline("reachability", false, &format!("{}: fetch OK", entry.name))?
                    }
                    Err(e) => {
                        writeline("reachability", true, &format!("{}: {e}", entry.name))?;
                        source_problems += 1;
                    }
                },
                Err(e) => {
                    writeline(
                        "reachability",
                        true,
                        &format!("{}: bad URI `{uri_str}` — {e}", entry.name),
                    )?;
                    source_problems += 1;
                }
            }
        }
    }

    // 6. Drift detection: install roots vs lockfile.
    let mut drift = 0;
    for adapter in all_adapters() {
        let installed = match adapter.list_installed() {
            Ok(v) => v,
            Err(e) => {
                writeline(
                    "drift",
                    true,
                    &format!("{}: list_installed failed — {e}", adapter.name()),
                )?;
                drift += 1;
                continue;
            }
        };
        let disk_paths: BTreeSet<String> = installed
            .into_iter()
            .filter_map(|d| match d {
                DeployedRef::Deployed { path, .. } => Some(path),
                _ => None,
            })
            .collect();
        let lockfile_paths_for_tool: BTreeSet<String> = lockfile
            .units
            .iter()
            .filter_map(|u| u.deployed.get(adapter.name()))
            .filter_map(|d| match d {
                DeployedRef::Deployed { path, .. } => Some(path.clone()),
                _ => None,
            })
            .collect();
        // Disk has extras the lockfile doesn't know about.
        for extra in disk_paths.difference(&lockfile_paths_for_tool) {
            writeline(
                "drift",
                true,
                &format!("{}: untracked deploy {extra}", adapter.name()),
            )?;
            drift += 1;
        }
        // Lockfile claims things the disk doesn't have.
        for missing in lockfile_paths_for_tool.difference(&disk_paths) {
            writeline(
                "drift",
                true,
                &format!(
                    "{}: lockfile entry `{missing}` not present on disk",
                    adapter.name()
                ),
            )?;
            drift += 1;
        }
    }
    if drift == 0 {
        writeline("drift", false, "no untracked deploys")?;
    }

    let _ = file_problems;
    let _ = source_problems;
    finalize(out, issues)
}

fn finalize(out: &mut dyn io::Write, issues: Vec<String>) -> Result<()> {
    // Point at the dependency catalog + installer (lives in the ainb-core setup
    // engine, not this crate — so surface the commands rather than re-render it).
    writeln!(out, "\nDependencies (tools the agents + plugins need):")?;
    writeln!(
        out,
        "  ainb init --check                    # full topic/dependency status"
    )?;
    writeln!(
        out,
        "  ainb init --script --agent <claude|codex|copilot>   # write an installer for what's missing"
    )?;
    writeln!(out)?;

    if issues.is_empty() {
        writeln!(out, "doctor: all checks passed")?;
        return Ok(());
    }
    writeln!(out, "doctor: {} issue(s)", issues.len())?;
    for i in &issues {
        writeln!(out, "  - {i}")?;
    }
    bail!("doctor reported {} issue(s)", issues.len());
}

fn short(hex: &str) -> &str {
    if hex.len() > 12 { &hex[..12] } else { hex }
}
