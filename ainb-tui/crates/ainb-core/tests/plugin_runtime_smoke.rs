//! Phase 7b smoke test: ainb-core's plugin bootstrap brings the
//! subprocess runtime up cleanly even when zero plugins are
//! installed. Confirms the wasmi-removal cutover doesn't leave a
//! crash-on-startup hole behind, and that the discovery escape hatch
//! (`AINB_DISABLE_PLUGINS`) still short-circuits.

use ainb::plugins::{init_plugin_runtime, LoadOutcome};

/// Fresh runtime with no plugin root in scope: should succeed, return
/// an empty `LoadOutcome`, and have zero registered plugins on the
/// handle.
#[test]
fn empty_root_brings_runtime_up_cleanly() {
    // Force discovery to a path that doesn't exist so every fallback
    // (exe-dir, cwd, ~/.agents-in-a-box) gets skipped.
    std::env::set_var("AINB_PLUGIN_ROOT", "/definitely/not/a/real/plugin/root");
    std::env::remove_var("AINB_DISABLE_PLUGINS");

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
    std::env::set_var("AINB_DISABLE_PLUGINS", "1");
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
