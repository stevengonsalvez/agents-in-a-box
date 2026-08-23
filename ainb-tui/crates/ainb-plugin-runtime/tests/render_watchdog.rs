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

/// The wedge is not a one-way door: once the SAME plugin answers a render, it
/// goes back to receiving its own keys.
///
/// The earlier version of this test registered a second plugin under a second
/// runtime and asserted that one was unwedged — which only proved a fresh
/// registration starts clean, and never exercised the lift path at all.
#[test]
fn the_wedge_lifts_once_the_same_plugin_renders_again() {
    // A budget the fixture blows on the first render but can meet once the
    // process is warm would be racy, so instead: wedge under a tight budget,
    // then prove the very same plugin lifts it by answering.
    let (rt, handle) = runtime_with_render_budget(Duration::from_millis(20));
    let id = register_slow_fixture(&rt);

    let rx = handle.render(&id, Viewport::new(40, 8), 0);
    let outcome = rt.tokio_handle().block_on(async {
        tokio::time::timeout(Duration::from_secs(5), rx)
            .await
            .expect("watchdog must answer")
    });
    assert!(
        matches!(outcome, Ok(RenderOutcome::RuntimeError(_))),
        "precondition: the first render must be failed by the watchdog"
    );
    assert!(handle.render_wedged(&id), "precondition: wedged");

    // The fixture answers ~200ms after each request. Keep asking until one of
    // those late answers lands and clears the flag on THIS plugin — that is the
    // lift path, and nothing else in the suite covers it.
    let lifted = rt.tokio_handle().block_on(async {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while tokio::time::Instant::now() < deadline {
            let rx = handle.render(&id, Viewport::new(40, 8), 0);
            // The watchdog answers this receiver at the 20ms budget; the
            // fixture's own answer lands ~180ms later. It is that later answer
            // that must lift the wedge, so wait past it before checking.
            let _ = tokio::time::timeout(Duration::from_secs(2), rx).await;
            tokio::time::sleep(Duration::from_millis(400)).await;
            if !handle.render_wedged(&id) {
                return true;
            }
        }
        false
    });
    assert!(
        lifted,
        "a plugin that answers a render again must stop being treated as wedged, \
         or q/Esc are permanently diverted away from a healthy screen"
    );
}
