//! `ainb plugin` subcommand handlers.
//!
//! Phase 4 shipped a marketplace + installer surface coupled to the wasmi
//! `.wasm` packaging. Phase 7b deletes the wasmi host; the marketplace
//! flow needs to be re-cut against the subprocess ABI 2.0 manifest
//! (`ainb-plugin-protocol::manifest::Manifest`) and the new on-disk
//! cache layout (`<root>/<name>/manifest.toml` + `<root>/<name>/<name>`
//! binary). That work lives under Phase 7c.
//!
//! While 7c is in flight every install/marketplace handler returns the
//! same operator-actionable error so the CLI surface stays stable for
//! `ainb plugin --help` and existing scripts continue to fail loudly
//! instead of silently shipping a half-ported installer. `ainb plugin
//! list` is the one survivor — it walks the runtime's registered-plugins
//! list, which is correct for both the legacy and the new ABI.
//!
//! When 7c lands, restore the install/update/remove/marketplace handlers
//! against `ainb-plugin-installer` (TBD crate) which will own the new
//! cache_layout + marketplace primitives without any wasmi coupling.
//!
//! Phase 7d-cli grows three live-DevX subcommands on top of the install
//! flow — `lint`, `watch`, and `tail` — each in its own submodule below.

use anyhow::{anyhow, bail, Result};

use crate::cli::OutputFormat;

pub mod lint;
pub mod tail;
pub mod watch;

/// Top-level dispatcher for `ainb plugin <subcommand>`.
pub async fn execute(matches: &clap::ArgMatches, format: OutputFormat) -> Result<()> {
    match matches.subcommand() {
        Some(("marketplace", sub)) => match sub.subcommand() {
            Some(("add", _)) | Some(("list", _)) | Some(("remove", _)) => not_yet_ported(),
            _ => bail!("unknown plugin marketplace subcommand"),
        },
        Some(("install", _))
        | Some(("update", _))
        | Some(("remove", _))
        | Some(("search", _)) => not_yet_ported(),
        Some(("list", _)) => list_installed(format),
        Some(("lint", sub)) => {
            let arg = sub
                .get_one::<String>("plugin")
                .ok_or_else(|| anyhow!("missing <plugin>"))?
                .clone();
            lint::run(&arg, format)
        }
        Some(("watch", sub)) => {
            let id = sub
                .get_one::<String>("plugin")
                .ok_or_else(|| anyhow!("missing <plugin>"))?
                .clone();
            let duration_secs = sub.get_one::<u64>("duration").copied();
            watch::run(&id, duration_secs).await
        }
        Some(("tail", sub)) => {
            let id = sub
                .get_one::<String>("plugin")
                .ok_or_else(|| anyhow!("missing <plugin>"))?
                .clone();
            let level = sub
                .get_one::<String>("level")
                .cloned()
                .unwrap_or_else(|| "debug".to_string());
            let since = sub.get_one::<String>("since").cloned();
            let duration_secs = sub.get_one::<u64>("duration").copied();
            tail::run(&id, &level, since.as_deref(), duration_secs).await
        }
        _ => bail!("unknown plugin subcommand"),
    }
}

/// Uniform error for the install/marketplace handlers stranded by the
/// wasmi removal. Exit code matches the runtime's "missing plugin"
/// contract (handled at the binary boundary).
fn not_yet_ported() -> Result<()> {
    Err(anyhow!(
        "the marketplace + installer flow is being re-cut against the subprocess \
         ABI 2.0 (Phase 7c). Re-run after 7c lands."
    ))
}

/// `ainb plugin list` — enumerate plugins the runtime currently knows
/// about. Survives the wasmi removal because it just walks
/// `RuntimeHandle::registered_plugins`.
fn list_installed(format: OutputFormat) -> Result<()> {
    let (runtime, handle, _outcome) = crate::plugins::init_plugin_runtime()
        .map_err(|e| anyhow!("plugin runtime init failed: {e}"))?;
    let plugins = handle.registered_plugins();
    drop(runtime);

    match format {
        OutputFormat::Json => {
            let entries: Vec<_> = plugins
                .iter()
                .map(|p| {
                    serde_json::json!({
                        "id": p.id.to_string(),
                        "binary": p.binary_path.display().to_string(),
                    })
                })
                .collect();
            let out = serde_json::json!({ "plugins": entries });
            println!("{}", serde_json::to_string_pretty(&out)?);
        }
        OutputFormat::Text | OutputFormat::Csv => {
            if plugins.is_empty() {
                println!("(no plugins registered — install via 'ainb plugin install <name>')");
            } else {
                for p in plugins {
                    println!("{}\t{}", p.id, p.binary_path.display());
                }
            }
        }
    }
    Ok(())
}
