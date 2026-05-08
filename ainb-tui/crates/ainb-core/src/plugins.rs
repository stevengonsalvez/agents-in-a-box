//! Plugin host integration for ainb-core.
//!
//! Phase 3 Wave 3 — minimum viable host wiring. On app startup we discover
//! bundled plugins under `dist/plugins/` (next to the running binary or
//! workspace root) and load each one through `ainb_plugin_host::PluginHost`.
//!
//! Today the only bundled plugin is `burndown` (`crates/ainb-plugin-burndown`).
//! It owns its own UsageData ingest and rendering; the host calls `_render`
//! once per frame and paints the resulting `WireBuffer` through `PluginScreen`.
//!
//! Discovery is best-effort: if no plugin directory exists, the host comes up
//! empty (loaded.is_empty()) and ainb runs exactly as it did before.

use std::path::{Path, PathBuf};

use ainb_plugin_host::{LoadOutcome, PluginHost};
use tracing::{debug, info, warn};

/// Build a `PluginHost` and load every bundled plugin we can locate.
///
/// Returns `(host, outcome)` so the caller can log/assert on which plugins
/// loaded vs. failed without short-circuiting the whole app on a single
/// degraded plugin.
#[must_use]
pub fn init_plugin_host() -> (PluginHost, LoadOutcome) {
    let mut host = PluginHost::new();
    let mut outcome = LoadOutcome::default();

    let Some(root) = discover_plugin_root() else {
        debug!("no plugin root discovered — running with no plugins loaded");
        return (host, outcome);
    };

    info!(plugin_root = %root.display(), "loading bundled plugins");
    let entries = match std::fs::read_dir(&root) {
        Ok(it) => it,
        Err(e) => {
            warn!(error = %e, root = %root.display(), "could not read plugin root");
            return (host, outcome);
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("<unknown>")
            .to_string();
        match host.load_dir(&path) {
            Ok(id) => {
                info!(plugin = %id, dir = %path.display(), "loaded plugin");
                outcome.loaded.push(id);
            }
            Err(e) => {
                warn!(plugin = %name, error = %e, "failed to load plugin (continuing)");
                outcome.failed.push((name, e));
            }
        }
    }

    (host, outcome)
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
            // `cargo run` lands in target/<profile>/ainb — bubble up two.
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
        let installed = home
            .join(".agents-in-a-box")
            .join("plugins")
            .join("cache");
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
        // Force a non-existent root so discovery falls through every branch
        // and returns None — the host should still construct cleanly.
        std::env::set_var("AINB_PLUGIN_ROOT", "/definitely/not/a/real/path");
        let (host, outcome) = init_plugin_host();
        assert_eq!(outcome.loaded.len(), 0, "no plugins should load");
        assert_eq!(outcome.failed.len(), 0, "no plugins should fail (none found)");
        assert!(host.plugins().next().is_none(), "host has zero plugins");
        std::env::remove_var("AINB_PLUGIN_ROOT");
    }
}
