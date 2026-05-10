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

use std::path::{Path, PathBuf};

use ainb_plugin_runtime::{Runtime, RuntimeError, RuntimeHandle};
use tracing::{debug, info, warn};

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

    info!(plugin_root = %root.display(), "discovering subprocess plugins");
    match handle.discover(&root) {
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
fn discover_plugin_root() -> Option<PathBuf> {
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
