//! Plugin runtime integration for ainb-core (Phase 7 cutover).
//!
//! Replaces the Phase 3 wasmi-based [`ainb_plugin_host::PluginHost`] with the
//! subprocess + JSON-RPC runtime in `ainb-plugin-runtime`. App startup builds a
//! tokio-backed [`ainb_plugin_runtime::Runtime`] and hands the TUI a
//! Send + Clone [`ainb_plugin_runtime::RuntimeHandle`] whose surface methods
//! never `.await` — `try_recv_render`, `snapshot_get`, `invoke_action`.
//!
//! Phase 7b ships the runtime cutover only. The legacy in-tree wasmi plugins
//! (`burndown`, `session-reader`) are not yet repackaged as subprocess Rust
//! binaries — that work lives in Phase 7c. While 7c is in flight the runtime
//! comes up empty (zero registered plugins) and the TUI degrades exactly the
//! same way it did when discovery returned no `dist/plugins/` directory under
//! the old host.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use ainb_plugin_runtime::{Runtime, RuntimeError, RuntimeHandle};
use tracing::{debug, info, warn};

use crate::config::PluginsConfig;

/// Outcome of [`init_plugin_runtime`] — kept for parity with the previous
/// `LoadOutcome` type so callers (CLI, smoke tests) can log discovery results
/// without short-circuiting the whole app on a single bad plugin.
#[derive(Debug, Default)]
pub struct LoadOutcome {
    pub loaded: Vec<String>,
    pub failed: Vec<(String, RuntimeError)>,
}

/// Build the plugin runtime, discover any installed plugins, and return the
/// owning [`Runtime`] alongside a [`RuntimeHandle`] for the TUI thread.
///
/// Returns `(runtime, handle, outcome)`. The caller MUST keep `runtime` alive
/// for the lifetime of the app — dropping it joins every plugin task and tears
/// down the tokio executor.
pub fn init_plugin_runtime() -> Result<(Runtime, RuntimeHandle, LoadOutcome), RuntimeError> {
    let (runtime, handle) = Runtime::new()?;
    let mut outcome = LoadOutcome::default();

    // Operator escape hatch — `AINB_DISABLE_PLUGINS=1` skips discovery
    // entirely so the runtime comes up plugin-free for debugging,
    // bisecting plugin-induced regressions, and the tripwire's
    // "graceful fallback when plugins are disabled" assertion.
    if plugins_disabled() {
        info!("AINB_DISABLE_PLUGINS set — skipping plugin discovery");
        return Ok((runtime, handle, outcome));
    }

    let Some(root) = discover_plugin_root() else {
        debug!("no plugin root discovered — running with no plugins loaded");
        return Ok((runtime, handle, outcome));
    };

    // Load the plugins config once: it drives both the enable/disable filter
    // and the per-plugin `[plugins.<name>]` config injected at init.
    let plugins_cfg = load_plugins_config();

    // Per-plugin enable/disable filter, resolved from env + config.toml
    // with documented precedence. See `resolve_plugin_filter` doc.
    let filter = resolve_plugin_filter_from_env(&plugins_cfg);
    if let Some(reason) = filter.describe() {
        info!(filter = %reason, "applying plugin filter");
    }

    info!(plugin_root = %root.display(), "discovering subprocess plugins");
    let result = handle.discover_filtered_with_config(
        &root,
        |p| filter.allows(p.id.as_str()),
        |p| resolve_plugin_config(&plugins_cfg, p.id.as_str()),
    );
    match result {
        Ok(plugins) => {
            for p in plugins {
                info!(plugin = %p.id, "registered plugin");
                outcome.loaded.push(p.id.to_string());
            }
        }
        Err(e) => {
            warn!(error = %e, root = %root.display(), "plugin discovery failed");
            outcome.failed.push(("<discovery>".into(), e));
        }
    }

    Ok((runtime, handle, outcome))
}

/// Resolved per-plugin filter. Encodes the three states the plugin
/// admission decision can land in after merging env vars + config.toml.
///
/// **Precedence** (most specific wins):
/// 1. `AINB_DISABLE_PLUGINS=1` → discovery skipped entirely. Handled
///    upstream in [`init_plugin_runtime`] before this enum is built,
///    so it never appears as a `PluginFilter` variant.
/// 2. `AINB_ONLY_PLUGINS=a,b` → [`Allow`](PluginFilter::Allow) — env
///    allowlist overrides config entirely.
/// 3. `AINB_DISABLE_PLUGIN=a,b` → [`Deny`](PluginFilter::Deny) — env
///    denylist when no env allowlist is set.
/// 4. `config.toml [plugins].enabled = [...]` → [`Allow`](PluginFilter::Allow)
///    when env didn't decide.
/// 5. `config.toml [plugins].disabled = [...]` → [`Deny`](PluginFilter::Deny)
///    when env didn't decide and config has no allowlist.
/// 6. Default → [`AllOn`](PluginFilter::AllOn).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PluginFilter {
    /// Load every discovered plugin.
    AllOn,
    /// Load only plugins whose `id` appears in this set.
    Allow {
        allowed: HashSet<String>,
        source: &'static str,
    },
    /// Load every plugin except those whose `id` appears in this set.
    Deny {
        denied: HashSet<String>,
        source: &'static str,
    },
}

impl PluginFilter {
    /// Return `true` iff this filter would load a plugin with the
    /// given id. `id` is a string slice (not a `PluginId`) so this
    /// stays free of a runtime-crate-type dependency and unit-tests
    /// cheaply.
    pub(crate) fn allows(&self, id: &str) -> bool {
        match self {
            Self::AllOn => true,
            Self::Allow { allowed, .. } => allowed.contains(id),
            Self::Deny { denied, .. } => !denied.contains(id),
        }
    }

    /// Human-readable summary for the startup log line. `None` when
    /// the filter is the default no-op so we don't log "filter: none"
    /// noise on every cold start.
    pub(crate) fn describe(&self) -> Option<String> {
        match self {
            Self::AllOn => None,
            Self::Allow { allowed, source } => {
                let mut names: Vec<&str> = allowed.iter().map(String::as_str).collect();
                names.sort_unstable();
                Some(format!("allow={names:?} via {source}"))
            }
            Self::Deny { denied, source } => {
                let mut names: Vec<&str> = denied.iter().map(String::as_str).collect();
                names.sort_unstable();
                Some(format!("deny={names:?} via {source}"))
            }
        }
    }
}

/// Load the `[plugins]` section from the layered `config.toml`.
///
/// Surfaces load failures so a corrupt config doesn't silently disable the
/// user's plugin filter intent or drop their per-plugin config. Falling back to
/// `Default` is still the right behaviour — booting plugin-free isn't what the
/// user wanted — but the warn line gives them a breadcrumb in
/// `~/.agents-in-a-box/logs/`.
fn load_plugins_config() -> PluginsConfig {
    match crate::config::AppConfig::load() {
        Ok(c) => c.plugins,
        Err(e) => {
            warn!(
                error = %e,
                "config.toml failed to load — falling back to default plugin config (all plugins enabled, \
                 no per-plugin config). [plugins].enabled / .disabled / per-plugin values will not apply this session."
            );
            PluginsConfig::default()
        }
    }
}

/// Build a [`PluginFilter`] from env vars + an already-loaded [`PluginsConfig`].
///
/// Wrapper that gathers the env inputs from process state. The pure resolution
/// logic lives in [`resolve_plugin_filter`] so it's testable without poking
/// `std::env`.
fn resolve_plugin_filter_from_env(cfg: &PluginsConfig) -> PluginFilter {
    let env_only = std::env::var("AINB_ONLY_PLUGINS").ok();
    let env_disable = std::env::var("AINB_DISABLE_PLUGIN").ok();
    resolve_plugin_filter(env_only.as_deref(), env_disable.as_deref(), cfg)
}

/// Resolve the per-plugin config table for `id` into the JSON the host injects
/// at `plugin/init`.
///
/// Looks up `plugins.values[id]` (the serialized `[plugins.<id>]` table) and
/// converts it from TOML to JSON. An absent entry — or a conversion failure for
/// a value TOML allows but JSON can't represent — yields JSON `null`, which the
/// plugin's typed config parser treats as "all defaults". This is the same
/// sentinel `PluginInitParams.config` defaults to for ABI-2 back-compat.
pub(crate) fn resolve_plugin_config(cfg: &PluginsConfig, id: &str) -> serde_json::Value {
    let Some(table) = cfg.values.get(id) else {
        return serde_json::Value::Null;
    };
    match serde_json::to_value(table) {
        Ok(json) => json,
        Err(e) => {
            warn!(plugin = id, error = %e, "failed to convert [plugins.<name>] table to JSON — injecting null config");
            serde_json::Value::Null
        }
    }
}

/// Pure filter resolution. Inputs are the two env-var values (already
/// fetched) and the loaded [`PluginsConfig`]; output is the merged
/// [`PluginFilter`]. Split from the env-poking wrapper so unit tests
/// don't touch process state.
pub(crate) fn resolve_plugin_filter(
    env_only: Option<&str>,
    env_disable: Option<&str>,
    config: &PluginsConfig,
) -> PluginFilter {
    // 1. Env allowlist beats everything else.
    if let Some(raw) = env_only {
        let set = parse_csv(raw);
        if !set.is_empty() {
            return PluginFilter::Allow {
                allowed: set,
                source: "AINB_ONLY_PLUGINS",
            };
        }
    }
    // 2. Env denylist beats config.
    if let Some(raw) = env_disable {
        let set = parse_csv(raw);
        if !set.is_empty() {
            return PluginFilter::Deny {
                denied: set,
                source: "AINB_DISABLE_PLUGIN",
            };
        }
    }
    // 3. Config allowlist beats config denylist.
    if !config.enabled.is_empty() {
        return PluginFilter::Allow {
            allowed: config.enabled.iter().cloned().collect(),
            source: "config.toml [plugins].enabled",
        };
    }
    // 4. Config denylist.
    if !config.disabled.is_empty() {
        return PluginFilter::Deny {
            denied: config.disabled.iter().cloned().collect(),
            source: "config.toml [plugins].disabled",
        };
    }
    // 5. Default — load everything.
    PluginFilter::AllOn
}

/// Comma-separated list parser. Empty entries (e.g. trailing comma) are
/// dropped silently; surrounding whitespace is trimmed. Returns an
/// empty set when the input is just commas/whitespace.
fn parse_csv(raw: &str) -> HashSet<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

/// Operator-controlled kill switch. Recognises `1`, `true`, `yes`, `on`
/// (case-insensitive) — anything else, including unset, leaves plugins
/// enabled so a typo doesn't silently disable the runtime.
fn plugins_disabled() -> bool {
    match std::env::var("AINB_DISABLE_PLUGINS") {
        Ok(v) => matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => false,
    }
}

/// Search a small list of candidate locations for the plugin staging
/// directory. Returns the first one that exists.
///
/// Search order:
/// 1. `$AINB_PLUGIN_ROOT` (test/CI override).
/// 2. `<exe-dir>/dist/plugins/`         (release tarball layout).
/// 3. `<exe-dir>/../dist/plugins/`      (cargo run from `target/<profile>/`).
/// 4. `<cwd>/dist/plugins/`             (workspace dev layout).
/// 5. `~/.agents-in-a-box/plugins/cache/` (installed plugins).
pub(crate) fn discover_plugin_root() -> Option<PathBuf> {
    if let Ok(env_root) = std::env::var("AINB_PLUGIN_ROOT") {
        let p = PathBuf::from(env_root);
        if p.exists() {
            return Some(p);
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        let exe_dir = exe.parent().map(Path::to_path_buf);
        if let Some(d) = &exe_dir {
            let here = d.join("dist").join("plugins");
            if here.exists() {
                return Some(here);
            }
            let up = d.join("..").join("..").join("dist").join("plugins");
            if up.exists() {
                return Some(up.canonicalize().ok()?);
            }
        }
    }

    if let Ok(cwd) = std::env::current_dir() {
        let here = cwd.join("dist").join("plugins");
        if here.exists() {
            return Some(here);
        }
    }

    if let Some(home) = dirs::home_dir() {
        let installed = home.join(".agents-in-a-box").join("plugins").join("cache");
        if installed.exists() {
            return Some(installed);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_root_returns_no_loaded_plugins() {
        std::env::set_var("AINB_PLUGIN_ROOT", "/definitely/not/a/real/path");
        let (_runtime, handle, outcome) =
            init_plugin_runtime().expect("runtime init must succeed even with no plugin root");
        assert_eq!(outcome.loaded.len(), 0, "no plugins should load");
        assert_eq!(
            outcome.failed.len(),
            0,
            "no plugins should fail (none found)"
        );
        assert!(
            handle.registered_plugins().is_empty(),
            "runtime has zero plugins"
        );
        std::env::remove_var("AINB_PLUGIN_ROOT");
    }

    #[test]
    fn plugins_disabled_recognises_truthy_values() {
        for v in ["1", "true", "TRUE", "yes", "On", "  yes  "] {
            std::env::set_var("AINB_DISABLE_PLUGINS", v);
            assert!(plugins_disabled(), "expected disabled for {v:?}");
        }
        for v in ["0", "false", "no", "off", "", "maybe"] {
            std::env::set_var("AINB_DISABLE_PLUGINS", v);
            assert!(!plugins_disabled(), "expected enabled for {v:?}");
        }
        std::env::remove_var("AINB_DISABLE_PLUGINS");
        assert!(!plugins_disabled(), "unset must leave plugins enabled");
    }
}

#[cfg(test)]
mod plugin_config_tests {
    //! Host-side resolution of the per-plugin `[plugins.<name>]` value
    //! table into the JSON injected at `plugin/init`, plus the grant flow
    //! for the `read_paths` capability (collector arm landed in P0).
    use super::*;
    use ainb_plugin_protocol::manifest::{
        Capabilities, CapabilityGrant, Lifecycle, Manifest, PluginMeta, Provides, SpawnMode,
        Subscribes,
    };
    use ainb_plugin_runtime::RegisteredPlugin;
    use std::path::PathBuf;

    fn values_with(name: &str, pairs: &[(&str, &str)]) -> PluginsConfig {
        let mut table = toml::value::Table::new();
        for (k, v) in pairs {
            table.insert((*k).into(), toml::Value::String((*v).into()));
        }
        let mut values = std::collections::BTreeMap::new();
        values.insert(name.to_string(), toml::Value::Table(table));
        PluginsConfig {
            enabled: Vec::new(),
            disabled: Vec::new(),
            values,
        }
    }

    fn manifest_with_read_paths(name: &str, paths: &[&str]) -> Manifest {
        Manifest {
            plugin: PluginMeta {
                name: name.into(),
                version: "0.1.0".into(),
                abi_version: 2,
                description: String::new(),
            },
            capabilities: Capabilities {
                read_paths: CapabilityGrant::List(paths.iter().map(|s| (*s).into()).collect()),
                ..Capabilities::default()
            },
            provides: Provides::default(),
            subscribes: Subscribes::default(),
            lifecycle: Lifecycle {
                spawn: SpawnMode::Lazy,
                idle_reap_secs: 600,
            },
            config: Vec::new(),
        }
    }

    #[test]
    fn test_resolved_config_injected_at_init() {
        // Host resolves `plugins.values[id]` → JSON and stamps it onto the
        // `RegisteredPlugin.config` that `send_init` forwards verbatim into
        // `PluginInitParams.config`. The JSON carried equals the resolved table.
        let cfg = values_with(
            "learnings",
            &[
                ("learnings_dir", "~/.learnings"),
                ("qmd_collection", "learnings"),
            ],
        );

        let resolved = resolve_plugin_config(&cfg, "learnings");
        // The resolved JSON is exactly the `[plugins.learnings]` table.
        assert_eq!(resolved["learnings_dir"], serde_json::json!("~/.learnings"));
        assert_eq!(resolved["qmd_collection"], serde_json::json!("learnings"));

        // Stamping onto the RegisteredPlugin (what send_init reads) preserves it.
        let plugin = RegisteredPlugin::new(
            manifest_with_read_paths("learnings", &["~/.learnings"]),
            PathBuf::from("/bin/learnings"),
            PathBuf::from("/etc/manifest.toml"),
        )
        .with_config(resolved.clone());
        assert_eq!(plugin.config, resolved);

        // An unconfigured plugin resolves to JSON null (ABI-2 back-compat default).
        let absent = resolve_plugin_config(&cfg, "burndown");
        assert_eq!(absent, serde_json::Value::Null);
    }

    #[test]
    fn test_read_paths_granted_from_manifest() {
        // A manifest declaring `read_paths` carries a granted capability the
        // runtime collector (P0) surfaces as "read_paths"; an absent grant does
        // not. The host stamps config onto the same plugin without disturbing it.
        let granted = manifest_with_read_paths("learnings", &["~/.learnings", "~/.cache/qmd"]);
        assert!(
            granted.capabilities.read_paths.is_granted(),
            "list-form read_paths must be granted"
        );
        assert_eq!(
            granted.capabilities.read_paths.allow_list().unwrap(),
            ["~/.learnings", "~/.cache/qmd"]
        );

        let ungranted = manifest_with_read_paths("plain", &[]);
        assert!(
            !ungranted.capabilities.read_paths.is_granted(),
            "empty list → not granted"
        );

        // Grant + injected config coexist on the registered plugin.
        let plugin = RegisteredPlugin::new(
            granted,
            PathBuf::from("/bin/learnings"),
            PathBuf::from("/etc/manifest.toml"),
        )
        .with_config(serde_json::json!({"learnings_dir": "~/.learnings"}));
        assert!(plugin.manifest.capabilities.read_paths.is_granted());
        assert_eq!(
            plugin.config["learnings_dir"],
            serde_json::json!("~/.learnings")
        );
    }
}

#[cfg(test)]
mod plugin_filter_tests {
    //! Pure unit tests for the per-plugin allow/deny filter. Touch
    //! `resolve_plugin_filter` directly so we don't pollute process
    //! env state — the env-poking wrapper
    //! `resolve_plugin_filter_from_env_and_config` is exercised
    //! end-to-end by the existing tripwires that boot ainb under
    //! various env combinations.
    use super::*;
    use crate::config::PluginsConfig;

    fn cfg(enabled: &[&str], disabled: &[&str]) -> PluginsConfig {
        PluginsConfig {
            enabled: enabled.iter().map(|s| s.to_string()).collect(),
            disabled: disabled.iter().map(|s| s.to_string()).collect(),
            ..PluginsConfig::default()
        }
    }

    #[test]
    fn no_inputs_yields_all_on() {
        let f = resolve_plugin_filter(None, None, &PluginsConfig::default());
        assert_eq!(f, PluginFilter::AllOn);
        assert!(f.allows("burndown"));
        assert!(f.allows("any-other-name"));
        assert!(f.describe().is_none(), "AllOn must not log noise");
    }

    #[test]
    fn env_only_plugins_creates_allowlist() {
        let f = resolve_plugin_filter(Some("burndown"), None, &PluginsConfig::default());
        assert!(f.allows("burndown"));
        assert!(!f.allows("session-reader"));
        assert!(matches!(
            f,
            PluginFilter::Allow { source, .. } if source == "AINB_ONLY_PLUGINS"
        ));
    }

    #[test]
    fn env_disable_plugin_creates_denylist() {
        let f = resolve_plugin_filter(None, Some("burndown"), &PluginsConfig::default());
        assert!(!f.allows("burndown"));
        assert!(f.allows("session-reader"));
        assert!(matches!(
            f,
            PluginFilter::Deny { source, .. } if source == "AINB_DISABLE_PLUGIN"
        ));
    }

    #[test]
    fn env_only_overrides_env_disable() {
        // Both env vars present — allowlist wins per precedence rule.
        let f = resolve_plugin_filter(
            Some("burndown"),
            Some("burndown,session-reader"),
            &PluginsConfig::default(),
        );
        assert!(f.allows("burndown"));
        assert!(!f.allows("session-reader"));
        assert!(matches!(
            f,
            PluginFilter::Allow { source, .. } if source == "AINB_ONLY_PLUGINS"
        ));
    }

    #[test]
    fn env_only_overrides_config_disabled() {
        let f = resolve_plugin_filter(
            Some("burndown"),
            None,
            &cfg(&[], &["burndown", "session-reader"]),
        );
        assert!(f.allows("burndown"));
        assert!(!f.allows("session-reader"));
    }

    #[test]
    fn env_disable_overrides_config_enabled() {
        let f = resolve_plugin_filter(None, Some("burndown"), &cfg(&["burndown"], &[]));
        // Config wanted only burndown; env explicitly disables burndown.
        // Env denylist takes precedence over config allowlist.
        assert!(!f.allows("burndown"));
        assert!(f.allows("session-reader"));
    }

    #[test]
    fn config_enabled_when_no_env() {
        let f = resolve_plugin_filter(None, None, &cfg(&["session-reader"], &[]));
        assert!(!f.allows("burndown"));
        assert!(f.allows("session-reader"));
        assert!(matches!(
            f,
            PluginFilter::Allow { source, .. } if source == "config.toml [plugins].enabled"
        ));
    }

    #[test]
    fn config_disabled_when_no_env_no_enabled() {
        let f = resolve_plugin_filter(None, None, &cfg(&[], &["burndown"]));
        assert!(!f.allows("burndown"));
        assert!(f.allows("session-reader"));
        assert!(matches!(
            f,
            PluginFilter::Deny { source, .. } if source == "config.toml [plugins].disabled"
        ));
    }

    #[test]
    fn config_enabled_beats_config_disabled() {
        // Both lists in config — allowlist wins (don't make the user
        // reason about union/intersection of two lists).
        let f = resolve_plugin_filter(None, None, &cfg(&["burndown"], &["burndown"]));
        assert!(f.allows("burndown"));
        assert!(!f.allows("session-reader"));
    }

    #[test]
    fn empty_env_strings_fall_through_to_config() {
        // Empty/whitespace-only env values must NOT lock the filter
        // into an empty allowlist (which would disable every plugin).
        let f = resolve_plugin_filter(Some(""), Some(",,, , "), &cfg(&[], &["burndown"]));
        // Env contributed nothing → config denylist took effect.
        assert!(!f.allows("burndown"));
        assert!(f.allows("session-reader"));
    }

    #[test]
    fn csv_parser_handles_whitespace_and_empty_entries() {
        let parsed = parse_csv("  burndown , , session-reader,  ");
        assert_eq!(parsed.len(), 2);
        assert!(parsed.contains("burndown"));
        assert!(parsed.contains("session-reader"));
    }

    #[test]
    fn describe_renders_sorted_names_for_logs() {
        // Log line stability — sorting prevents iteration-order churn
        // between processes (HashSet has no order guarantee).
        let f = resolve_plugin_filter(Some("zebra,alpha,middle"), None, &PluginsConfig::default());
        let desc = f.describe().expect("Allow filter must describe");
        assert!(
            desc.contains(r#"["alpha", "middle", "zebra"]"#),
            "got: {desc}"
        );
    }
}
