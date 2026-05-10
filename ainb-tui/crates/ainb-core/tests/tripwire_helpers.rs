//! Shared helpers for tripwire integration tests.
//!
//! Locates fixture plugin binaries in the cargo target directory and
//! provides a minimal RuntimeHandle test harness for non-blocking
//! validation.

use std::path::PathBuf;
use std::time::Duration;

use ainb_plugin_protocol::manifest::{
    Capabilities, Lifecycle, Manifest, PluginMeta, Provides, SpawnMode, Subscribes,
};
use ainb_plugin_protocol::params::Viewport;
use ainb_plugin_runtime::registry::RegisteredPlugin;
use ainb_plugin_runtime::types::{LifecycleState, PluginId, RuntimeConfig};
use ainb_plugin_runtime::{Runtime, RuntimeHandle};

/// Locate a sibling binary in the same target/debug/ directory as the
/// `ainb` binary under test.
pub fn sibling_bin(name: &str) -> PathBuf {
    let ainb = PathBuf::from(env!("CARGO_BIN_EXE_ainb"));
    let dir = ainb.parent().expect("ainb binary has parent dir");
    let path = dir.join(name);
    assert!(
        path.exists(),
        "fixture binary not found at {path:?} — run `cargo build -p ainb-plugin-runtime` first"
    );
    path
}

/// Build a runtime with tight backoff for fast crash-recovery tests.
pub fn fast_runtime() -> (Runtime, RuntimeHandle) {
    let cfg = RuntimeConfig {
        respawn_backoff: [
            Duration::from_millis(50),
            Duration::from_millis(100),
            Duration::from_millis(150),
        ],
        failure_window: Duration::from_secs(60),
        quarantine_failure_threshold: 3,
        ..RuntimeConfig::default()
    };
    Runtime::with_config(cfg).expect("build runtime")
}

/// Register a plugin from a binary path with a given name.
pub fn register_plugin(rt: &Runtime, name: &str, bin_path: PathBuf) -> PluginId {
    let manifest = Manifest {
        plugin: PluginMeta {
            name: name.into(),
            version: "0.1.0".into(),
            abi_version: 2,
            description: format!("tripwire fixture: {name}"),
        },
        capabilities: Capabilities::default(),
        provides: Provides {
            screens: vec![],
            commands: vec![],
            cli_namespaces: vec!["echo".into()],
            snapshots: vec!["fixture.greeting".into()],
        },
        subscribes: Subscribes::default(),
        lifecycle: Lifecycle {
            spawn: SpawnMode::Lazy,
            idle_reap_secs: 600,
        },
    };
    let plugin = RegisteredPlugin::new(manifest, bin_path, PathBuf::from("/dev/null/manifest.toml"));
    let id = plugin.id.clone();
    rt.register(plugin);
    id
}

/// Wait for a plugin to reach a target lifecycle state, with timeout.
pub fn wait_for_state(
    handle: &RuntimeHandle,
    id: &PluginId,
    target: LifecycleState,
    timeout: Duration,
) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if handle.lifecycle_state(id) == Some(target) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

/// Force a plugin to Running by issuing a render request and waiting.
pub fn ensure_running(rt: &Runtime, handle: &RuntimeHandle, id: &PluginId) {
    drop(handle.render(id, Viewport::new(10, 1), 0));
    assert!(
        wait_for_state(handle, id, LifecycleState::Running, Duration::from_secs(5)),
        "plugin {id} never reached Running; state = {:?}",
        handle.lifecycle_state(id)
    );
}
