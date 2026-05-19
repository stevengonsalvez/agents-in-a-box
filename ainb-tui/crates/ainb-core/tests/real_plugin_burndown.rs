//! P4 acceptance: `/test-ainb --plugin burndown` exit 0.
//!
//! Spawns the real `ainb-plugin-burndown` binary under the runtime,
//! sends a `plugin/render(viewport)` for a 40×12 tile, and asserts the
//! returned `WireBuffer` is non-empty.
//!
//! Marked `#[ignore]` so it only runs under `--ignored` (the contract
//! the /test-ainb skill's L4 layer uses). Skips gracefully when the
//! binary isn't built — the runtime cannot proceed without it.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ainb_plugin_protocol::manifest::{
    Capabilities, Lifecycle, Manifest, PluginMeta, Provides, SpawnMode, Subscribes,
};
use ainb_plugin_protocol::params::Viewport;
use ainb_plugin_runtime::registry::RegisteredPlugin;
use ainb_plugin_runtime::types::RenderOutcome;
use ainb_plugin_runtime::{Runtime, RuntimeConfig};

/// Walk up from the current executable directory to find the workspace
/// `target/debug` (or `target/release`) directory, then resolve
/// `ainb-plugin-burndown`. Returns `None` when the binary isn't built.
fn burndown_binary_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    // tests run from `target/debug/deps/<test_name>-<hash>`. Walk up two
    // dirs to land on `target/debug`.
    let target_dir = exe.parent()?.parent()?;
    let candidate = target_dir.join("ainb-plugin-burndown");
    if candidate.exists() {
        return Some(candidate);
    }
    // also try sibling target/release
    let release = target_dir
        .parent()
        .map(|p| p.join("release").join("ainb-plugin-burndown"));
    if let Some(p) = release {
        if p.exists() {
            return Some(p);
        }
    }
    None
}

fn burndown_manifest() -> Manifest {
    Manifest {
        plugin: PluginMeta {
            name: "burndown".into(),
            version: "2.0.0".into(),
            abi_version: 2,
            description: "Real-plugin spawn acceptance (P4)".into(),
        },
        capabilities: Capabilities::default(),
        provides: Provides {
            screens: vec!["analytics".into()],
            commands: vec!["/usage".into()],
            cli_namespaces: vec!["usage".into()],
            snapshots: vec![],
            preferred_min_size: Some([40, 12]),
        },
        subscribes: Subscribes::default(),
        lifecycle: Lifecycle {
            spawn: SpawnMode::Lazy,
            idle_reap_secs: 600,
        },
    }
}

#[test]
#[ignore = "real-plugin spawn — runs under /test-ainb --plugin burndown"]
fn burndown_plugin_renders_a_non_empty_wirebuffer() {
    let Some(bin) = burndown_binary_path() else {
        eprintln!(
            "[skip] real_plugin_burndown: ainb-plugin-burndown binary not found in target/debug or target/release; build via `cargo build -p ainb-plugin-burndown` and retry"
        );
        return;
    };
    eprintln!("[real_plugin_burndown] spawning {}", bin.display());

    let cfg = RuntimeConfig {
        respawn_backoff: [
            Duration::from_millis(50),
            Duration::from_millis(100),
            Duration::from_millis(150),
        ],
        failure_window: Duration::from_secs(60),
        ..RuntimeConfig::default()
    };
    let (rt, handle) = Runtime::with_config(cfg).expect("runtime startup");
    let plugin = RegisteredPlugin::new(
        burndown_manifest(),
        bin,
        Path::new("/dev/null/manifest.toml").to_path_buf(),
    );
    let id = plugin.id.clone();
    rt.register(plugin);

    let render_rx = handle.render(&id, Viewport::new(40, 12), 0);

    let outcome = rt.tokio_handle().block_on(async {
        tokio::time::timeout(Duration::from_secs(30), render_rx)
            .await
            .expect("render timed out after 30s")
            .expect("render channel closed")
    });

    match outcome {
        RenderOutcome::Ok(buf) => {
            assert_eq!(buf.width, 40, "viewport width must propagate to plugin");
            assert_eq!(buf.height, 12, "viewport height must propagate to plugin");
            // A real burndown render produces SOME cells. Empty cells
            // would mean the plugin compiled but never wrote anything,
            // which is the failure mode this test exists to catch.
            assert!(
                !buf.cells.is_empty(),
                "burndown produced an empty WireBuffer — render path is broken"
            );
            eprintln!(
                "[real_plugin_burndown] OK · cells.len()={}",
                buf.cells.len()
            );
        }
        other => panic!("burndown render outcome: {other:?}"),
    }

    // Avoid an `unused_variable` lint and document the keep-alive intent.
    let _keep_alive: Arc<()> = Arc::new(());
    drop(handle);
    drop(rt);
}
