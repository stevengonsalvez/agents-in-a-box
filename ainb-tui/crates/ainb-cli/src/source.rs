//! `ainb source ...` subcommand implementations.
//!
//! P2 makes `add` fetch the source via the right Fetcher and run the
//! detected SourceAdapter so the confirmation message reports a real
//! unit count. Lockfile records the resolved SHA and fetched path so
//! `search` (next module) can find each source's checkout without
//! re-fetching.

use std::io;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};

use ainb_adapters_source::{
    ManifestAdapter, MarketplaceAdapter, RawAdapter, SingleAdapter, SourceAdapter, pick_adapter,
};
use ainb_fetch::{FetchedRef, Fetcher, GitFetcher, HttpFetcher, LocalFetcher};
use ainb_skill_core::SourceType;
use ainb_skill_core::lockfile::{LockedSource, Lockfile};
use ainb_skill_core::manifest::{Manifest, SourceEntry};
use ainb_skill_core::paths::{cache_dir_in, lockfile_path_in, manifest_path_in};
use ainb_skill_core::uri::Uri;

use crate::{AddArgs, NameArg, RemoveArgs, SourceCommand};

/// Accepted `--type` hints. Free-form for P1 but constrained at the
/// CLI surface so users get a clean error early.
const ALLOWED_KINDS: &[&str] = &["marketplace", "manifest", "raw", "single"];

pub fn parse_kind_hint(s: &str) -> std::result::Result<String, String> {
    if ALLOWED_KINDS.contains(&s) {
        Ok(s.to_string())
    } else {
        Err(format!("must be one of {}", ALLOWED_KINDS.join(", ")))
    }
}

pub fn dispatch(home: &Path, action: SourceCommand, out: &mut dyn io::Write) -> Result<()> {
    let manifest_path = manifest_path_in(home);
    let lockfile_path = lockfile_path_in(home);
    match action {
        SourceCommand::Add(args) => add(home, args, &manifest_path, &lockfile_path, out),
        SourceCommand::List => list(&manifest_path, out),
        SourceCommand::Remove(args) => remove(args, &manifest_path, &lockfile_path, out),
        SourceCommand::Enable(arg) => set_enabled(arg, &manifest_path, true, out),
        SourceCommand::Disable(arg) => set_enabled(arg, &manifest_path, false, out),
    }
}

fn add(
    home: &Path,
    args: AddArgs,
    manifest_path: &Path,
    lockfile_path: &Path,
    out: &mut dyn io::Write,
) -> Result<()> {
    let uri =
        Uri::parse(&args.uri).with_context(|| format!("parsing source URI `{}`", args.uri))?;

    if uri.path.is_some() {
        bail!(
            "`{}` looks like a unit URI (has a path). Use `ainb skill install` for units; \
             pass a source URI (no `/path`) here.",
            args.uri
        );
    }

    let stored_uri = format!("{}:{}", uri.source_type, uri.locator);
    let ref_ = uri.ref_.clone().unwrap_or_else(|| "main".to_string());
    let name = args
        .name
        .clone()
        .unwrap_or_else(|| derive_name_slug(&uri.source_type, &uri.locator));

    if name.is_empty() {
        return Err(anyhow!(
            "could not derive a non-empty name from `{}` — pass --name explicitly",
            args.uri
        ));
    }

    // Reject duplicate before doing any expensive fetch work.
    let mut manifest = Manifest::load_from(manifest_path)?;
    if manifest.source(&name).is_some() {
        return Err(ainb_skill_core::CoreError::SourceAlreadyExists(name).into());
    }

    // Fetch the source via the type-appropriate fetcher.
    let cache_root = cache_dir_in(home);
    let fetched = run_fetcher(&uri, &name, &cache_root)
        .with_context(|| format!("fetching source `{name}`"))?;

    // Adapter selection: explicit --type wins, otherwise auto-detect.
    let (adapter, detected_kind) = pick_adapter_for(&fetched.path, args.kind.as_deref())?;
    let units = adapter
        .list_units(&fetched.path)
        .with_context(|| format!("listing units in `{}`", fetched.path.display()))?;
    let unit_count = units.len();

    // Persist the discovered kind in the manifest.
    manifest
        .add_source(SourceEntry {
            name: name.clone(),
            kind: Some(detected_kind.clone()),
            uri: stored_uri,
            r#ref: ref_.clone(),
            enabled: true,
            read_only: false,
            target_layout: Vec::new(),
        })
        .map_err(anyhow::Error::from)?;
    manifest.save_to(manifest_path)?;

    // Lockfile records where the fetched checkout lives so `search`
    // can find it without re-running the fetcher.
    let mut lockfile = Lockfile::load_from(lockfile_path)?;
    lockfile.sources.push(LockedSource {
        name: name.clone(),
        uri: format!("{}:{}", uri.source_type, uri.locator),
        declared_ref: ref_,
        resolved_sha: Some(fetched.resolved_sha),
        fetched_at: Some(fetched.fetched_at),
        fetched_path: Some(fetched.path.to_string_lossy().to_string()),
    });
    lockfile.save_to(lockfile_path)?;

    writeln!(
        out,
        "added source {name} ({detected_kind}) → {unit_count} unit(s)"
    )?;
    Ok(())
}

/// Fetch a source URI into `cache_root` using the type-appropriate
/// fetcher. Exposed at crate scope so sibling subcommands (skill
/// update / sync, migrate) can re-fetch without duplicating the
/// fetcher dispatch table.
pub(crate) fn run_fetcher(uri: &Uri, name: &str, cache_root: &Path) -> Result<FetchedRef> {
    let fetcher: Box<dyn Fetcher> = match uri.source_type {
        SourceType::Gh | SourceType::Git | SourceType::Gist | SourceType::Marketplace => {
            Box::new(GitFetcher::new())
        }
        SourceType::Https => Box::new(HttpFetcher::new()),
        SourceType::Local => Box::new(LocalFetcher::new()),
        SourceType::Npm => bail!("npm: source type not supported in v1"),
    };
    Ok(fetcher.fetch(uri, name, cache_root)?)
}

fn pick_adapter_for(
    fetched_root: &Path,
    forced_kind: Option<&str>,
) -> Result<(Box<dyn SourceAdapter>, String)> {
    if let Some(kind) = forced_kind {
        let adapter: Box<dyn SourceAdapter> = match kind {
            "marketplace" => Box::new(MarketplaceAdapter::new()),
            "manifest" => Box::new(ManifestAdapter::new()),
            "raw" => Box::new(RawAdapter::new()),
            "single" => Box::new(SingleAdapter::new()),
            other => bail!("unknown --type `{other}`"),
        };
        return Ok((adapter, kind.to_string()));
    }
    let picked = pick_adapter(fetched_root)
        .ok_or_else(|| anyhow!("no adapter matched fetched source at {fetched_root:?}"))?;
    let name = picked.name().to_string();
    Ok((picked, name))
}

fn list(manifest_path: &Path, out: &mut dyn io::Write) -> Result<()> {
    let manifest = Manifest::load_from(manifest_path)?;
    if manifest.sources.is_empty() {
        writeln!(out, "no sources configured")?;
        return Ok(());
    }

    let name_w = manifest.sources.iter().map(|s| s.name.len()).max().unwrap_or(4).max(4);
    let type_w = manifest
        .sources
        .iter()
        .map(|s| s.kind.as_deref().unwrap_or("-").len())
        .max()
        .unwrap_or(4)
        .max(4);
    let uri_w = manifest.sources.iter().map(|s| s.uri.len()).max().unwrap_or(3).max(3);
    let ref_w = manifest.sources.iter().map(|s| s.r#ref.len()).max().unwrap_or(3).max(3);

    writeln!(
        out,
        "{:<name_w$}  {:<type_w$}  {:<uri_w$}  {:<ref_w$}  ENABLED",
        "NAME", "TYPE", "URI", "REF",
    )?;
    for s in &manifest.sources {
        writeln!(
            out,
            "{:<name_w$}  {:<type_w$}  {:<uri_w$}  {:<ref_w$}  {}",
            s.name,
            s.kind.as_deref().unwrap_or("-"),
            s.uri,
            s.r#ref,
            if s.enabled { "yes" } else { "no" },
        )?;
    }
    Ok(())
}

fn remove(
    args: RemoveArgs,
    manifest_path: &Path,
    lockfile_path: &Path,
    out: &mut dyn io::Write,
) -> Result<()> {
    if !args.yes {
        bail!(
            "refusing to remove `{}` without --yes (interactive confirmation not available in P1)",
            args.name
        );
    }

    let mut manifest = Manifest::load_from(manifest_path)?;
    let removed = manifest.remove_source(&args.name)?;
    manifest.save_to(manifest_path)?;

    let mut lockfile = Lockfile::load_from(lockfile_path)?;
    let flagged = lockfile.mark_units_pending_uninstall_by_source_uri(&removed.uri);
    if flagged > 0 {
        lockfile.save_to(lockfile_path)?;
    }

    writeln!(
        out,
        "removed source {name} — {flagged} unit(s) flagged for uninstall on next `ainb skill sync`",
        name = removed.name,
    )?;
    Ok(())
}

fn set_enabled(
    arg: NameArg,
    manifest_path: &Path,
    enabled: bool,
    out: &mut dyn io::Write,
) -> Result<()> {
    let mut manifest = Manifest::load_from(manifest_path)?;
    {
        let entry = manifest
            .source_mut(&arg.name)
            .ok_or(ainb_skill_core::CoreError::SourceNotFound(arg.name.clone()))?;
        entry.enabled = enabled;
    }
    manifest.save_to(manifest_path)?;
    writeln!(
        out,
        "{verb}d source {name}",
        verb = if enabled { "enable" } else { "disable" },
        name = arg.name,
    )?;
    Ok(())
}

fn derive_name_slug(source_type: &ainb_skill_core::SourceType, locator: &str) -> String {
    let mut s = locator.to_string();
    // For URLs the `://` becomes `--`; users will usually override
    // with `--name` for those, but the slug is still unique + valid.
    s = s.replace([':', '/'], "-").to_lowercase();
    // Collapse repeated dashes and trim — only cosmetic, but keeps
    // names readable when locators contain `://` etc.
    let mut out = String::with_capacity(s.len());
    let mut prev_dash = false;
    for c in s.chars() {
        if c == '-' {
            if !prev_dash && !out.is_empty() {
                out.push(c);
            }
            prev_dash = true;
        } else {
            out.push(c);
            prev_dash = false;
        }
    }
    let trimmed = out.trim_end_matches('-').to_string();
    if trimmed.is_empty() {
        return source_type.as_str().to_string();
    }
    trimmed
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_skill_core::SourceType;

    #[test]
    fn slug_strips_slashes() {
        assert_eq!(
            derive_name_slug(&SourceType::Gh, "stevengonsalvez/my-skills"),
            "stevengonsalvez-my-skills"
        );
    }

    #[test]
    fn slug_collapses_url_garbage() {
        assert_eq!(
            derive_name_slug(&SourceType::Git, "https://gitlab.example.com/x/y"),
            "https-gitlab.example.com-x-y"
        );
    }

    #[test]
    fn slug_handles_local_path() {
        assert_eq!(
            derive_name_slug(&SourceType::Local, "/Users/me/dev/foo"),
            "users-me-dev-foo"
        );
    }
}
