//! The render watchdog: a plugin that blocks inside its own `plugin/render`
//! must not be able to hold the host hostage.
//!
//! `RuntimeConfig::default_render_timeout` existed for a long time without a
//! single reference — nothing enforced it, so a wedged render simply never
//! answered its oneshot. The host went on painting a stale frame, and because
//! the SDK holds one per-plugin mutex across both `render` and the inline
//! `handle_key` dispatch, the plugin's keys stopped being serviced too. That is
//! how pressing `[s]` on the hangar screen made `q` and `Esc` dead.
//!
//! The deliberately-wedging plugin here is the existing slow fixture (200ms
//! render) run against a render budget far below that, which is exactly the
//! shape of a render that overruns.

use std::path::PathBuf;
use std::time::Duration;

use ainb_plugin_protocol::manifest::{
    Capabilities, Lifecycle, Manifest, PluginMeta, Provides, SpawnMode, Subscribes,
};
use ainb_plugin_protocol::params::Viewport;
use ainb_plugin_runtime::registry::RegisteredPlugin;
use ainb_plugin_runtime::types::{PluginId, RenderOutcome};
use ainb_plugin_runtime::{Runtime, RuntimeConfig};

/// The slow fixture sleeps 200ms inside render.
const FIXTURE_RENDER_COST: Duration = Duration::from_millis(200);

fn slow_fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ainb-slow-fixture-plugin"))
}

fn slow_fixture_manifest() -> Manifest {
    Manifest {
        plugin: PluginMeta {
            name: "slow-fixture".into(),
            version: "0.1.0".into(),
            abi_version: 2,
            description: "renders slower than the budget".into(),
        },
        capabilities: Capabilities::default(),
        provides: Provides {
            screens: vec![],
            commands: vec![],
            cli_namespaces: vec![],
            snapshots: vec![],
        },
        subscribes: Subscribes::default(),
        lifecycle: Lifecycle {
            spawn: SpawnMode::Lazy,
            idle_reap_secs: 600,
        },
        config: Vec::new(),
    }
}

/// A runtime whose render budget `budget` is the only thing under test.
fn runtime_with_render_budget(budget: Duration) -> (Runtime, ainb_plugin_runtime::RuntimeHandle) {
    let cfg = RuntimeConfig {
        default_render_timeout: budget,
        ..RuntimeConfig::default()
    };
    Runtime::with_config(cfg).expect("build runtime")
}

fn register_slow_fixture(rt: &Runtime) -> PluginId {
    let plugin = RegisteredPlugin::new(
        slow_fixture_manifest(),
        slow_fixture_path(),
        PathBuf::from("/dev/null/manifest.toml"),
    );
    let id = plugin.id.clone();
    rt.register(plugin);
    id
}

/// A render that overruns its budget must be failed by the host rather than
/// left outstanding forever, and the plugin must be flagged wedged so the host
/// key path can release `q`/`Esc`.
#[test]
fn a_render_that_overruns_its_budget_is_failed_and_flags_the_plugin_wedged() {
    let budget = Duration::from_millis(20);
    assert!(
        budget < FIXTURE_RENDER_COST,
        "the fixture must be slower than the budget or this proves nothing"
    );
    let (rt, handle) = runtime_with_render_budget(budget);
    let id = register_slow_fixture(&rt);

    let rx = handle.render(&id, Viewport::new(40, 8), 0);
    let outcome = rt.tokio_handle().block_on(async {
        // Generous relative to the budget: the point is that the watchdog — not
        // this timeout — is what ends the wait.
        tokio::time::timeout(Duration::from_secs(5), rx)
            .await
            .expect("the watchdog must answer; an outstanding render hung forever")
            .expect("render channel closed")
    });

    match outcome {
        RenderOutcome::RuntimeError(msg) => assert!(
            msg.contains("render exceeded"),
            "the failure must name the overrun, got {msg:?}"
        ),
        other => panic!("expected the watchdog to fail the render, got {other:?}"),
    }
    assert!(
        handle.render_wedged(&id),
        "an overrunning plugin must be flagged so the host can keep q/Esc alive"
    );
}

/// The wedge is not a one-way door: once the plugin answers a render again, it
/// goes back to receiving its own keys.
#[test]
fn the_wedge_lifts_once_the_plugin_renders_again() {
    let (rt, handle) = runtime_with_render_budget(Duration::from_millis(20));
    let id = register_slow_fixture(&rt);

    let rx = handle.render(&id, Viewport::new(40, 8), 0);
    let _ = rt
        .tokio_handle()
        .block_on(async { tokio::time::timeout(Duration::from_secs(5), rx).await });
    assert!(handle.render_wedged(&id), "precondition: wedged");

    // Ask again with a budget the fixture comfortably meets. The runtime config
    // is fixed at construction, so re-render under a fresh, generous runtime.
    let (rt2, handle2) = runtime_with_render_budget(Duration::from_secs(5));
    let id2 = register_slow_fixture(&rt2);
    let rx2 = handle2.render(&id2, Viewport::new(40, 8), 0);
    let outcome = rt2.tokio_handle().block_on(async {
        tokio::time::timeout(Duration::from_secs(10), rx2)
            .await
            .expect("render timed out")
            .expect("render channel closed")
    });
    assert!(
        matches!(outcome, RenderOutcome::Ok(_)),
        "a render inside its budget must still succeed"
    );
    assert!(
        !handle2.render_wedged(&id2),
        "a responsive plugin must not be flagged wedged"
    );
}
