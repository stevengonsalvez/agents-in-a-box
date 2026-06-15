//! Shared test harness: build a [`Runtime`] with tight backoff and
//! register a canary binary resolved via `CARGO_BIN_EXE_<name>`.

use std::path::PathBuf;
use std::time::Duration;

use ainb_plugin_protocol::manifest::{
    Capabilities, Lifecycle, Manifest, PluginMeta, Provides, SpawnMode, Subscribes,
};
use ainb_plugin_runtime::registry::RegisteredPlugin;
use ainb_plugin_runtime::types::{LifecycleState, PluginId, RuntimeConfig};
use ainb_plugin_runtime::{Runtime, RuntimeHandle};

/// Build a runtime with tight backoff for fast CTS cycles.
pub fn build_runtime() -> (Runtime, RuntimeHandle) {
    let cfg = RuntimeConfig {
        respawn_backoff: [
            Duration::from_millis(50),
            Duration::from_millis(100),
            Duration::from_millis(150),
        ],
        failure_window: Duration::from_secs(60),
        ..RuntimeConfig::default()
    };
    Runtime::with_config(cfg).expect("build runtime")
}

/// Minimal manifest suitable for most canary plugins.
pub fn canary_manifest(name: &str) -> Manifest {
    Manifest {
        plugin: PluginMeta {
            name: name.into(),
            version: "0.0.1".into(),
            abi_version: 2,
            description: format!("CTS canary: {name}"),
        },
        capabilities: Capabilities::default(),
        provides: Provides::default(),
        subscribes: Subscribes::default(),
        lifecycle: Lifecycle {
            spawn: SpawnMode::Lazy,
            idle_reap_secs: 600,
        },
        config: Vec::new(),
    }
}

/// Register a canary plugin with a custom manifest, resolved from the
/// `CARGO_BIN_EXE_<name>` env var that cargo sets for `[[bin]]` targets.
pub fn register_canary(rt: &Runtime, bin_path: PathBuf, manifest: Manifest) -> PluginId {
    let plugin =
        RegisteredPlugin::new(manifest, bin_path, PathBuf::from("/dev/null/manifest.toml"));
    let id = plugin.id.clone();
    rt.register(plugin);
    id
}

/// Poll until the plugin reaches `Running` or panic after timeout.
pub fn wait_running(handle: &RuntimeHandle, id: &PluginId) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if matches!(handle.lifecycle_state(id), Some(LifecycleState::Running)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!(
        "plugin {id} never reached Running; state = {:?}",
        handle.lifecycle_state(id)
    );
}

/// Poll until the plugin reaches `Quarantined` or panic after timeout.
pub fn wait_quarantined(handle: &RuntimeHandle, id: &PluginId, timeout: Duration) {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if matches!(
            handle.lifecycle_state(id),
            Some(LifecycleState::Quarantined)
        ) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!(
        "plugin {id} never quarantined; state = {:?}",
        handle.lifecycle_state(id)
    );
}

/// Block on a tokio oneshot receiver with timeout.
pub fn block_on_with_timeout<T>(
    rt: &Runtime,
    rx: tokio::sync::oneshot::Receiver<T>,
    timeout: Duration,
) -> T {
    rt.tokio_handle().block_on(async {
        tokio::time::timeout(timeout, rx)
            .await
            .expect("timed out waiting for response")
            .expect("channel closed")
    })
}
