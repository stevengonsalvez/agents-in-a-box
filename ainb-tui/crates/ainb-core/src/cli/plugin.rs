//! `ainb plugin` subcommand handlers.
//!
//! Phase 4 marketplace + installer surface. Replaces the Phase 2b stub
//! that only reserved the subcommand surface. Wiring:
//!
//! * `marketplace add <git-url|file://...|local-path>` — fetches a
//!   marketplace's `marketplace.json` into
//!   `<plugins-root>/marketplaces/<name>/marketplace.json`.
//! * `marketplace list` / `marketplace remove <name>` — manage the on-disk
//!   set of registered marketplaces.
//! * `search <query>` — substring search across every registered
//!   marketplace.
//! * `install <name>[@<version>]` — resolves through registered
//!   marketplaces, fetches the compiled `.wasm`, validates the manifest,
//!   prompts the user for capability approval, and writes the binary into
//!   `<plugins-root>/cache/<marketplace>/<plugin>/<version>/`. `--yes`
//!   skips the prompt (CI install). Updates the lockfile.
//! * `update <name>` — re-resolves to the latest matching version. If the
//!   new version requests capabilities the user hasn't already approved,
//!   the prompt fires again.
//! * `remove <name>` — drops cache + lockfile entry. Confirms before
//!   deleting `data/<plugin>/` if it exists (unless `--yes`).
//! * `list` — text or JSON listing of installed plugins.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use ainb_plugin_api::Manifest;
use ainb_plugin_host::cache_layout::{
    self, default_release_template, fill_release_url, InstallLock,
};
use ainb_plugin_host::marketplace::{LockEntry, Lockfile, Marketplace, MarketplaceEntry};
use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use serde::Serialize;
use tracing::{info, warn};

use crate::cli::OutputFormat;

/// Top-level dispatcher for `ainb plugin <subcommand>`.
pub async fn execute(matches: &clap::ArgMatches, format: OutputFormat) -> Result<()> {
    match matches.subcommand() {
        Some(("marketplace", sub)) => match sub.subcommand() {
            Some(("add", a)) => {
                let url = a
                    .get_one::<String>("url")
                    .ok_or_else(|| anyhow!("missing <url>"))?;
                marketplace_add(url).await
            }
            Some(("list", _)) => marketplace_list(format),
            Some(("remove", a)) => {
                let name = a
                    .get_one::<String>("name")
                    .ok_or_else(|| anyhow!("missing <name>"))?;
                marketplace_remove(name)
            }
            _ => bail!("unknown plugin marketplace subcommand"),
        },
        Some(("install", sub)) => {
            let arg = sub
                .get_one::<String>("plugin")
                .ok_or_else(|| anyhow!("missing <plugin>"))?;
            let yes = sub.get_flag("yes");
            install(arg, yes).await
        }
        Some(("update", sub)) => {
            let name = sub
                .get_one::<String>("plugin")
                .ok_or_else(|| anyhow!("missing <plugin>"))?;
            let yes = sub.get_flag("yes");
            update(name, yes).await
        }
        Some(("remove", sub)) => {
            let name = sub
                .get_one::<String>("plugin")
                .ok_or_else(|| anyhow!("missing <plugin>"))?;
            let yes = sub.get_flag("yes");
            remove(name, yes)
        }
        Some(("list", _)) => list_installed(format),
        Some(("search", sub)) => {
            let query = sub
                .get_one::<String>("query")
                .ok_or_else(|| anyhow!("missing <query>"))?;
            search(query, format)
        }
        _ => bail!("unknown plugin subcommand"),
    }
}

/// Register a marketplace by URL. Three accepted forms:
/// 1. `https://...` git repository — shallow clone.
/// 2. `file:///path/to/marketplace.json` — direct file copy (test harness).
/// 3. `/abs/path/to/marketplace.json` or `./relative/path` — direct file copy.
pub async fn marketplace_add(url: &str) -> Result<()> {
    let _lock = InstallLock::acquire().context("acquire install lock")?;
    cache_layout::ensure_layout().context("ensure layout")?;

    let marketplace_json_src = if url.starts_with("file://") {
        let raw = url.trim_start_matches("file://");
        load_marketplace_file(Path::new(raw))?
    } else if Path::new(url).exists() {
        load_marketplace_file(Path::new(url))?
    } else if url.starts_with("http://") || url.starts_with("https://") || url.starts_with("git@") {
        clone_marketplace_repo(url)?
    } else {
        bail!("unsupported marketplace URL: {url} (expected https://, git@, file://, or local path)");
    };

    let parsed = Marketplace::from_json(&marketplace_json_src)
        .with_context(|| "marketplace.json failed to parse — refusing to register")?;

    let dest_dir = cache_layout::marketplace_dir(&parsed.name)
        .ok_or_else(|| anyhow!("cannot resolve plugins root"))?;
    fs::create_dir_all(&dest_dir)
        .with_context(|| format!("create_dir_all {}", dest_dir.display()))?;
    let dest = dest_dir.join("marketplace.json");
    fs::write(&dest, &marketplace_json_src)
        .with_context(|| format!("write {}", dest.display()))?;

    info!(name = %parsed.name, plugins = parsed.plugins.len(), "registered marketplace");
    println!(
        "Registered marketplace '{}' with {} plugin(s)",
        parsed.name,
        parsed.plugins.len()
    );
    Ok(())
}

fn load_marketplace_file(path: &Path) -> Result<String> {
    fs::read_to_string(path)
        .with_context(|| format!("read marketplace.json from {}", path.display()))
}

fn clone_marketplace_repo(url: &str) -> Result<String> {
    let tmp = tempfile::tempdir().context("create tempdir for shallow clone")?;
    // git2 supports shallow=1 via FetchOptions only with newer libgit2; fall
    // back to a plain clone — marketplace catalogs are tiny so depth doesn't
    // matter much.
    git2::Repository::clone(url, tmp.path())
        .with_context(|| format!("git clone {url}"))?;
    // Look in the canonical locations the schema mirrors:
    // 1. .ainb-plugin/marketplace.json (preferred)
    // 2. .claude-plugin/marketplace.json (Claude Code compat)
    let candidates = [
        tmp.path().join(".ainb-plugin").join("marketplace.json"),
        tmp.path().join(".claude-plugin").join("marketplace.json"),
    ];
    for path in &candidates {
        if path.exists() {
            return load_marketplace_file(path);
        }
    }
    bail!(
        "no marketplace.json found at .ainb-plugin/ or .claude-plugin/ in {url}"
    );
}

/// List currently-registered marketplaces.
pub fn marketplace_list(format: OutputFormat) -> Result<()> {
    let dir = cache_layout::marketplaces_dir()
        .ok_or_else(|| anyhow!("cannot resolve plugins root"))?;
    if !dir.exists() {
        return print_empty_list(format, "marketplaces");
    }

    let mut names: Vec<String> = fs::read_dir(&dir)
        .with_context(|| format!("read_dir {}", dir.display()))?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let p = e.path();
            if p.join("marketplace.json").exists() {
                p.file_name().and_then(|n| n.to_str()).map(String::from)
            } else {
                None
            }
        })
        .collect();
    names.sort();

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string(&names)?);
        }
        _ => {
            if names.is_empty() {
                println!("No marketplaces registered.");
            } else {
                for n in names {
                    println!("{n}");
                }
            }
        }
    }
    Ok(())
}

/// Drop a registered marketplace.
pub fn marketplace_remove(name: &str) -> Result<()> {
    let _lock = InstallLock::acquire().context("acquire install lock")?;
    let dir = cache_layout::marketplace_dir(name)
        .ok_or_else(|| anyhow!("cannot resolve plugins root"))?;
    if !dir.exists() {
        bail!("marketplace '{name}' not registered");
    }
    fs::remove_dir_all(&dir).with_context(|| format!("remove_dir_all {}", dir.display()))?;
    println!("Removed marketplace '{name}'");
    Ok(())
}

/// `ainb plugin search <query>` — substring search on entry name + version
/// across every registered marketplace.
pub fn search(query: &str, format: OutputFormat) -> Result<()> {
    let mut hits: Vec<SearchHit> = Vec::new();
    for (mkt_name, mkt) in load_all_marketplaces()? {
        for entry in &mkt.plugins {
            if entry.name.contains(query) {
                hits.push(SearchHit {
                    marketplace: mkt_name.clone(),
                    plugin: entry.name.clone(),
                    version: entry.version.clone(),
                    repo: entry.repo.clone(),
                });
            }
        }
    }

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string(&hits)?);
        }
        _ => {
            if hits.is_empty() {
                println!("No matches for '{query}'.");
            } else {
                for h in hits {
                    println!(
                        "{marketplace}/{plugin}@{version}  ({repo})",
                        marketplace = h.marketplace,
                        plugin = h.plugin,
                        version = h.version,
                        repo = h.repo
                    );
                }
            }
        }
    }
    Ok(())
}

#[derive(Serialize)]
struct SearchHit {
    marketplace: String,
    plugin: String,
    version: String,
    repo: String,
}

fn load_all_marketplaces() -> Result<Vec<(String, Marketplace)>> {
    let dir = match cache_layout::marketplaces_dir() {
        Some(d) => d,
        None => return Ok(Vec::new()),
    };
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(&dir).with_context(|| format!("read_dir {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path().join("marketplace.json");
        if !path.exists() {
            continue;
        }
        let raw = fs::read_to_string(&path)
            .with_context(|| format!("read {}", path.display()))?;
        match Marketplace::from_json(&raw) {
            Ok(m) => {
                let dir_name = entry
                    .path()
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default()
                    .to_string();
                out.push((dir_name, m));
            }
            Err(e) => {
                warn!(path = %path.display(), error = %e, "skipping malformed marketplace.json");
            }
        }
    }
    Ok(out)
}

/// Install a plugin from a registered marketplace.
///
/// `arg` is one of:
///   * `name`            — pick the highest version found in any marketplace.
///   * `name@version`    — pin to an exact version.
///   * `marketplace/name`/`marketplace/name@version` — pin marketplace too.
pub async fn install(arg: &str, assume_yes: bool) -> Result<()> {
    let _lock = InstallLock::acquire().context("acquire install lock")?;
    cache_layout::ensure_layout().context("ensure layout")?;

    let parsed = parse_install_arg(arg);
    let resolved = resolve(&parsed)?;
    perform_install(&resolved, assume_yes, /* is_update */ false).await
}

/// Update an installed plugin to the latest matching version. Re-prompts
/// the user if any new capabilities are requested.
pub async fn update(name: &str, assume_yes: bool) -> Result<()> {
    let _lock = InstallLock::acquire().context("acquire install lock")?;
    cache_layout::ensure_layout().context("ensure layout")?;

    let lockfile = read_lockfile()?;
    let prior = lockfile
        .get(name)
        .ok_or_else(|| anyhow!("plugin '{name}' is not installed"))?
        .clone();
    let parsed = ParsedArg {
        marketplace: Some(prior.marketplace.clone()),
        name: name.to_string(),
        version: None, // pull latest
    };
    let resolved = resolve(&parsed)?;
    perform_install(&resolved, assume_yes, /* is_update */ true).await
}

#[derive(Debug, Clone)]
struct ParsedArg {
    marketplace: Option<String>,
    name: String,
    version: Option<String>,
}

fn parse_install_arg(arg: &str) -> ParsedArg {
    let (marketplace, rest) = match arg.split_once('/') {
        Some((m, r)) => (Some(m.to_string()), r),
        None => (None, arg),
    };
    let (name, version) = match rest.split_once('@') {
        Some((n, v)) => (n.to_string(), Some(v.to_string())),
        None => (rest.to_string(), None),
    };
    ParsedArg {
        marketplace,
        name,
        version,
    }
}

#[derive(Debug)]
struct ResolvedPlugin {
    marketplace_name: String,
    entry: MarketplaceEntry,
}

fn resolve(arg: &ParsedArg) -> Result<ResolvedPlugin> {
    let marketplaces = load_all_marketplaces()?;
    if marketplaces.is_empty() {
        bail!("no marketplaces registered — run `ainb plugin marketplace add <url>` first");
    }
    let mut hits: Vec<(String, &MarketplaceEntry)> = Vec::new();
    for (mkt_dir, mkt) in &marketplaces {
        if let Some(filter) = &arg.marketplace {
            if mkt_dir != filter && mkt.name != *filter {
                continue;
            }
        }
        if let Some(entry) = mkt.lookup(&arg.name, arg.version.as_deref()) {
            hits.push((mkt_dir.clone(), entry));
        }
    }
    match hits.len() {
        0 => Err(anyhow!(
            "no marketplace entry for '{}'{}",
            arg.name,
            arg.version
                .as_ref()
                .map(|v| format!("@{v}"))
                .unwrap_or_default()
        )),
        1 => {
            let (mkt, entry) = hits.into_iter().next().expect("len==1");
            Ok(ResolvedPlugin {
                marketplace_name: mkt,
                entry: entry.clone(),
            })
        }
        _ => Err(anyhow!(
            "plugin '{}' is offered by multiple marketplaces — disambiguate with `<marketplace>/{}`",
            arg.name,
            arg.name
        )),
    }
}

async fn perform_install(
    resolved: &ResolvedPlugin,
    assume_yes: bool,
    is_update: bool,
) -> Result<()> {
    let entry = &resolved.entry;
    let mkt_name = &resolved.marketplace_name;
    println!(
        "Resolving from marketplace '{mkt_name}'…\nFound {name}@{ver}",
        name = entry.name,
        ver = entry.version
    );

    let wasm_bytes = fetch_wasm(entry).await?;
    let manifest = read_manifest_for_entry(entry).await?;

    // Sanity check: manifest plugin name + version must match the entry.
    if manifest.plugin.name != entry.name {
        bail!(
            "manifest name '{}' does not match marketplace entry '{}'",
            manifest.plugin.name,
            entry.name
        );
    }
    if manifest.plugin.version != entry.version {
        bail!(
            "manifest version '{}' does not match marketplace entry '{}'",
            manifest.plugin.version,
            entry.version
        );
    }

    let caps = enumerate_capabilities(&manifest);
    let mut lockfile = read_lockfile()?;
    let prior_caps: Vec<String> = lockfile
        .get(&entry.name)
        .map(|e| e.capabilities_approved.clone())
        .unwrap_or_default();

    if !caps.is_empty() {
        let needs_prompt = if is_update {
            !ainb_plugin_host::marketplace::new_capabilities(&prior_caps, &caps).is_empty()
        } else {
            true
        };
        if needs_prompt {
            print_capability_request(&caps);
            if !assume_yes && !confirm("Allow?")? {
                bail!("install aborted: capability approval declined");
            }
        }
    }

    // Cache layout: cache/<mkt>/<plugin>/<version>/{plugin.toml,plugin.wasm}.
    let cache_target = cache_layout::plugin_cache_dir(mkt_name, &entry.name, &entry.version)
        .ok_or_else(|| anyhow!("cannot resolve plugin cache dir"))?;
    fs::create_dir_all(&cache_target)
        .with_context(|| format!("create_dir_all {}", cache_target.display()))?;
    fs::write(cache_target.join("plugin.wasm"), &wasm_bytes)
        .with_context(|| format!("write plugin.wasm in {}", cache_target.display()))?;
    let manifest_toml = toml::to_string(&manifest).context("re-encode manifest as TOML")?;
    fs::write(cache_target.join("plugin.toml"), manifest_toml)
        .with_context(|| format!("write plugin.toml in {}", cache_target.display()))?;

    // Drop the previous version's directory (if any) when this is an update
    // bumping the version — keeps the cache from accumulating stale builds.
    if is_update {
        if let Some(prior) = lockfile.get(&entry.name) {
            if prior.version != entry.version {
                if let Some(prev_dir) = cache_layout::plugin_cache_dir(
                    &prior.marketplace,
                    &entry.name,
                    &prior.version,
                ) {
                    if prev_dir.exists() {
                        let _ = fs::remove_dir_all(&prev_dir);
                    }
                }
            }
        }
    }

    let entry_lock = LockEntry {
        marketplace: mkt_name.clone(),
        version: entry.version.clone(),
        installed_at: Utc::now().to_rfc3339(),
        capabilities_approved: caps.clone(),
    };
    lockfile.insert(entry.name.clone(), entry_lock);
    write_lockfile(&lockfile)?;

    println!(
        "Installed {name}@{ver} from '{mkt_name}'",
        name = entry.name,
        ver = entry.version,
    );
    Ok(())
}

fn enumerate_capabilities(m: &Manifest) -> Vec<String> {
    let mut caps = Vec::new();
    if m.capabilities.read_sessions {
        caps.push("read_sessions".into());
    }
    if m.capabilities.write_plugin_data {
        caps.push("write_plugin_data".into());
    }
    if m.capabilities.event_bus {
        caps.push("event_bus".into());
    }
    if m.capabilities.read_claude_logs {
        caps.push("read_claude_logs".into());
    }
    if m.capabilities.read_codex_logs {
        caps.push("read_codex_logs".into());
    }
    if m.capabilities.spawn_subprocess {
        caps.push("spawn_subprocess".into());
    }
    for n in &m.capabilities.network {
        caps.push(format!("network:{n}"));
    }
    for f in &m.capabilities.filesystem {
        caps.push(format!("filesystem:{f}"));
    }
    caps
}

fn print_capability_request(caps: &[String]) {
    println!("\nPlugin requests:");
    for c in caps {
        println!("  • {c}");
    }
    println!();
}

fn confirm(prompt: &str) -> Result<bool> {
    let mut stdout = io::stdout().lock();
    write!(stdout, "{prompt} [y/N] ")?;
    stdout.flush()?;
    let mut buf = String::new();
    io::stdin().read_line(&mut buf).context("read stdin")?;
    Ok(matches!(buf.trim().to_ascii_lowercase().as_str(), "y" | "yes"))
}

async fn fetch_wasm(entry: &MarketplaceEntry) -> Result<Vec<u8>> {
    let template = entry
        .release_url
        .clone()
        .unwrap_or_else(|| default_release_template().to_string());
    let url = fill_release_url(&template, &entry.repo, &entry.git_ref, &entry.name);
    fetch_bytes(&url).await
}

async fn fetch_bytes(url: &str) -> Result<Vec<u8>> {
    if let Some(local) = local_path_from_url(url) {
        let mut buf = Vec::new();
        fs::File::open(&local)
            .with_context(|| format!("open {}", local.display()))?
            .read_to_end(&mut buf)
            .with_context(|| format!("read {}", local.display()))?;
        return Ok(buf);
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .context("build reqwest client")?;
    let resp = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    if !resp.status().is_success() {
        bail!("download {url} failed: HTTP {}", resp.status());
    }
    let bytes = resp.bytes().await.context("read response body")?;
    Ok(bytes.to_vec())
}

fn local_path_from_url(url: &str) -> Option<PathBuf> {
    if let Some(rest) = url.strip_prefix("file://") {
        return Some(PathBuf::from(rest));
    }
    let p = PathBuf::from(url);
    if p.is_absolute() && p.exists() {
        return Some(p);
    }
    None
}

async fn read_manifest_for_entry(entry: &MarketplaceEntry) -> Result<Manifest> {
    // Resolve the manifest by templating release_url with `manifest_path`'s
    // basename when the URL contains `{plugin}` — this covers the common
    // pattern where `release_url` is set to `…/{plugin}.toml` for testing.
    let template = entry.release_url.clone();
    let manifest_url = manifest_url_for(entry, template.as_deref());
    let raw = match manifest_url {
        Some(url) => {
            let bytes = fetch_bytes(&url).await?;
            String::from_utf8(bytes).context("manifest is not valid UTF-8")?
        }
        None => {
            // Final fallback: GitHub raw URL constructed from owner/repo.
            let template = "https://raw.githubusercontent.com/{owner}/{repo}/{tag}/{plugin_path}";
            let (owner, repo) = split_owner_repo(&entry.repo);
            let url = template
                .replace("{owner}", owner)
                .replace("{repo}", repo)
                .replace("{tag}", &entry.git_ref)
                .replace("{plugin_path}", &entry.manifest_path);
            let bytes = fetch_bytes(&url).await?;
            String::from_utf8(bytes).context("manifest is not valid UTF-8")?
        }
    };
    Manifest::from_toml(&raw).context("parse plugin.toml")
}

fn manifest_url_for(entry: &MarketplaceEntry, template: Option<&str>) -> Option<String> {
    let template = template?;
    // When the marketplace ships a side-by-side .toml under a parallel
    // path, swap the trailing .wasm for .toml. This is the convention the
    // installer uses for file:// fixtures in tests.
    if template.ends_with(".wasm") {
        let toml_template = template.replace(".wasm", ".toml");
        return Some(fill_release_url(
            &toml_template,
            &entry.repo,
            &entry.git_ref,
            &entry.name,
        ));
    }
    None
}

fn split_owner_repo(repo: &str) -> (&str, &str) {
    let trimmed = repo.strip_suffix(".git").unwrap_or(repo);
    let segments: Vec<&str> = trimmed
        .trim_end_matches('/')
        .rsplit('/')
        .take(2)
        .collect();
    match segments.as_slice() {
        [name, owner] => (owner, name),
        _ => ("", trimmed),
    }
}

/// Remove an installed plugin: drops the cache + the lockfile entry. If
/// `data/<plugin>/` exists it asks before deleting (unless `--yes`).
pub fn remove(name: &str, assume_yes: bool) -> Result<()> {
    let _lock = InstallLock::acquire().context("acquire install lock")?;
    let mut lockfile = read_lockfile()?;
    let entry = lockfile
        .get(name)
        .ok_or_else(|| anyhow!("plugin '{name}' is not installed"))?
        .clone();

    if let Some(cache) =
        cache_layout::plugin_cache_dir(&entry.marketplace, name, &entry.version)
    {
        if cache.exists() {
            fs::remove_dir_all(&cache)
                .with_context(|| format!("remove_dir_all {}", cache.display()))?;
        }
    }

    if let Some(data) = cache_layout::plugin_data_dir(name) {
        if data.exists() {
            let proceed = assume_yes
                || confirm(&format!(
                    "Plugin data directory exists at {}. Delete?",
                    data.display()
                ))?;
            if proceed {
                fs::remove_dir_all(&data)
                    .with_context(|| format!("remove_dir_all {}", data.display()))?;
            }
        }
    }

    lockfile.remove(name);
    write_lockfile(&lockfile)?;
    println!("Removed {name}@{ver}", ver = entry.version);
    Ok(())
}

/// `ainb plugin list` — show installed plugins, text or JSON.
pub fn list_installed(format: OutputFormat) -> Result<()> {
    let lockfile = read_lockfile()?;
    if lockfile.entries.is_empty() {
        return print_empty_list(format, "installed plugins");
    }
    match format {
        OutputFormat::Json => {
            #[derive(Serialize)]
            struct Out {
                name: String,
                marketplace: String,
                version: String,
                installed_at: String,
                capabilities_approved: Vec<String>,
            }
            let listed: Vec<Out> = lockfile
                .entries
                .into_iter()
                .map(|(name, e)| Out {
                    name,
                    marketplace: e.marketplace,
                    version: e.version,
                    installed_at: e.installed_at,
                    capabilities_approved: e.capabilities_approved,
                })
                .collect();
            println!("{}", serde_json::to_string(&listed)?);
        }
        _ => {
            for (name, e) in &lockfile.entries {
                println!(
                    "{name}@{ver}  marketplace={mkt}  installed_at={ts}",
                    ver = e.version,
                    mkt = e.marketplace,
                    ts = e.installed_at,
                );
            }
        }
    }
    Ok(())
}

fn print_empty_list(format: OutputFormat, what: &str) -> Result<()> {
    match format {
        OutputFormat::Json => println!("[]"),
        _ => println!("No {what} found."),
    }
    Ok(())
}

fn read_lockfile() -> Result<Lockfile> {
    let path = cache_layout::lockfile_path()
        .ok_or_else(|| anyhow!("cannot resolve plugins root"))?;
    if !path.exists() {
        return Ok(Lockfile::default());
    }
    let raw = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    Lockfile::from_toml(&raw)
        .with_context(|| format!("parse lockfile {}", path.display()))
}

fn write_lockfile(lf: &Lockfile) -> Result<()> {
    let path = cache_layout::lockfile_path()
        .ok_or_else(|| anyhow!("cannot resolve plugins root"))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create_dir_all {}", parent.display()))?;
    }
    let body = lf.to_toml().context("encode lockfile")?;
    fs::write(&path, body).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_install_arg_handles_marketplace_and_version() {
        let p = parse_install_arg("ainb-plugins/burndown@0.1.0");
        assert_eq!(p.marketplace.as_deref(), Some("ainb-plugins"));
        assert_eq!(p.name, "burndown");
        assert_eq!(p.version.as_deref(), Some("0.1.0"));

        let p = parse_install_arg("burndown");
        assert!(p.marketplace.is_none());
        assert_eq!(p.name, "burndown");
        assert!(p.version.is_none());

        let p = parse_install_arg("burndown@0.2.0");
        assert!(p.marketplace.is_none());
        assert_eq!(p.name, "burndown");
        assert_eq!(p.version.as_deref(), Some("0.2.0"));
    }

    #[test]
    fn enumerate_capabilities_lists_only_truthy_flags() {
        let mut m = Manifest::from_toml(
            r#"
            [plugin]
            name = "x"
            version = "0.1.0"
            ainb_min_version = "1.1.0"

            [capabilities]
            read_sessions = true
            write_plugin_data = false
            network = ["api.example.com:443"]
            filesystem = ["~/.foo/**"]
            "#,
        )
        .expect("parses");
        // sanity: defaults
        m.capabilities.spawn_subprocess = false;
        let caps = enumerate_capabilities(&m);
        assert!(caps.contains(&"read_sessions".to_string()));
        assert!(!caps.contains(&"write_plugin_data".to_string()));
        assert!(caps.contains(&"network:api.example.com:443".to_string()));
        assert!(caps.contains(&"filesystem:~/.foo/**".to_string()));
    }

    #[test]
    fn local_path_from_url_handles_file_scheme_and_absolute_paths() {
        let p = local_path_from_url("file:///tmp/x.wasm").expect("file://");
        assert_eq!(p, PathBuf::from("/tmp/x.wasm"));
        // http URL → None
        assert!(local_path_from_url("https://example.com/x.wasm").is_none());
    }
}
