//! Phase 7b smoke test: ainb-core's plugin bootstrap brings the
//! subprocess runtime up cleanly even when zero plugins are
//! installed. Confirms the wasmi-removal cutover doesn't leave a
//! crash-on-startup hole behind, and that the discovery escape hatch
//! (`AINB_DISABLE_PLUGINS`) still short-circuits.

use ainb::plugins::{init_plugin_runtime, LoadOutcome};
use std::sync::Mutex;

/// Serialises process-env mutations across this file's tests so the
/// parallel cargo-test runner doesn't race one test's `AINB_PLUGIN_ROOT`
/// into another's `init_plugin_runtime` call. Pattern documented in
/// the project memory under `reference_env_lock_for_parallel_tests`.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Fresh runtime with no plugin root in scope: should succeed, return
/// an empty `LoadOutcome`, and have zero registered plugins on the
/// handle.
#[test]
fn empty_root_brings_runtime_up_cleanly() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    // Force discovery to a path that doesn't exist so every fallback
    // (exe-dir, cwd, ~/.agents-in-a-box) gets skipped.
    std::env::set_var("AINB_PLUGIN_ROOT", "/definitely/not/a/real/plugin/root");
    std::env::remove_var("AINB_DISABLE_PLUGINS");
    std::env::remove_var("AINB_DISABLE_PLUGIN");
    std::env::remove_var("AINB_ONLY_PLUGINS");

    let (runtime, handle, outcome): (_, _, LoadOutcome) =
        init_plugin_runtime().expect("runtime startup must succeed");

    assert!(
        outcome.loaded.is_empty(),
        "no plugins were installed; expected empty loaded list, got {:?}",
        outcome.loaded
    );
    assert!(
        outcome.failed.is_empty(),
        "expected no failures when discovery returns empty; got {:?}",
        outcome
            .failed
            .iter()
            .map(|(n, e)| format!("{n}: {e}"))
            .collect::<Vec<_>>()
    );
    assert!(
        handle.registered_plugins().is_empty(),
        "RuntimeHandle should expose zero registrations after empty discovery"
    );

    drop(handle);
    drop(runtime);
    std::env::remove_var("AINB_PLUGIN_ROOT");
}

/// `AINB_DISABLE_PLUGINS=1` short-circuits discovery without touching
/// the filesystem. Runtime still comes up so the rest of the host
/// keeps working when plugins are deliberately disabled.
#[test]
fn disable_env_skips_discovery() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    std::env::set_var("AINB_DISABLE_PLUGINS", "1");
    std::env::remove_var("AINB_DISABLE_PLUGIN");
    std::env::remove_var("AINB_ONLY_PLUGINS");
    // Even with a "real" looking root, the env var must take
    // precedence — set it to a path that *would* error if scanned.
    std::env::set_var("AINB_PLUGIN_ROOT", "/this/should/never/be/read");

    let (runtime, handle, outcome) =
        init_plugin_runtime().expect("runtime startup must succeed under disable flag");

    assert!(outcome.loaded.is_empty(), "disable flag must skip loading");
    assert!(outcome.failed.is_empty(), "disable flag must skip failing");
    assert!(handle.registered_plugins().is_empty());

    drop(handle);
    drop(runtime);
    std::env::remove_var("AINB_DISABLE_PLUGINS");
    std::env::remove_var("AINB_PLUGIN_ROOT");
}

/// `AINB_DISABLE_PLUGIN=burndown` runs discovery normally but skips
/// the named plugin. Verifies the env-var deny path lands end-to-end
/// against the real staged `dist/plugins/` layout — unit tests cover
/// the filter math, this one proves the wiring.
///
/// Skips when `dist/plugins/` isn't staged (fresh checkout, no `just
/// stage-plugins` run yet) so CI runs that don't build plugins don't
/// fail this test.
#[test]
fn disable_plugin_env_skips_named_plugin() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());

    let plugin_root = locate_dist_plugins();
    let Some(root) = plugin_root else {
        eprintln!("SKIP: dist/plugins not staged — run `scripts/build-plugins.sh`");
        return;
    };

    // Pin discovery to the staged layout, deny burndown by name.
    std::env::set_var("AINB_PLUGIN_ROOT", &root);
    std::env::set_var("AINB_DISABLE_PLUGIN", "burndown");
    std::env::remove_var("AINB_DISABLE_PLUGINS");
    std::env::remove_var("AINB_ONLY_PLUGINS");

    let (runtime, handle, outcome) =
        init_plugin_runtime().expect("runtime startup with plugin denylist must succeed");

    assert!(
        !outcome.loaded.iter().any(|id| id == "burndown"),
        "AINB_DISABLE_PLUGIN=burndown must skip burndown; loaded={:?}",
        outcome.loaded
    );
    // session-reader should still load (it's the other in-tree plugin
    // and isn't in the denylist).
    assert!(
        outcome.loaded.iter().any(|id| id == "session-reader"),
        "denying burndown must not also drop session-reader; loaded={:?}",
        outcome.loaded
    );

    drop(handle);
    drop(runtime);
    std::env::remove_var("AINB_DISABLE_PLUGIN");
    std::env::remove_var("AINB_PLUGIN_ROOT");
}

/// `AINB_ONLY_PLUGINS=session-reader` is the inverse — only the named
/// plugin loads; burndown must be skipped. Same skip-on-no-dist gate
/// as the deny test.
#[test]
fn only_plugins_env_creates_exact_allowlist() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());

    let plugin_root = locate_dist_plugins();
    let Some(root) = plugin_root else {
        eprintln!("SKIP: dist/plugins not staged — run `scripts/build-plugins.sh`");
        return;
    };

    std::env::set_var("AINB_PLUGIN_ROOT", &root);
    std::env::set_var("AINB_ONLY_PLUGINS", "session-reader");
    std::env::remove_var("AINB_DISABLE_PLUGINS");
    std::env::remove_var("AINB_DISABLE_PLUGIN");

    let (runtime, handle, outcome) =
        init_plugin_runtime().expect("runtime startup with plugin allowlist must succeed");

    assert_eq!(
        outcome.loaded,
        vec!["session-reader".to_string()],
        "AINB_ONLY_PLUGINS=session-reader must load exactly session-reader; got {:?}",
        outcome.loaded
    );

    drop(handle);
    drop(runtime);
    std::env::remove_var("AINB_ONLY_PLUGINS");
    std::env::remove_var("AINB_PLUGIN_ROOT");
}

/// Walk up from the test binary looking for `dist/plugins/` with both
/// in-tree plugin binaries staged. Returns `None` if not present — the
/// test then skips rather than failing.
fn locate_dist_plugins() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_ainb"));
    let mut dir = bin.parent()?;
    for _ in 0..6 {
        let candidate = dir.join("dist").join("plugins");
        if candidate.join("burndown").join("burndown").exists()
            && candidate
                .join("session-reader")
                .join("session-reader")
                .exists()
        {
            return Some(candidate);
        }
        dir = dir.parent()?;
    }
    None
}
