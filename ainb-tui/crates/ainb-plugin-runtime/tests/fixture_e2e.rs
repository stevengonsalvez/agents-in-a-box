//! End-to-end runtime ↔ fixture-plugin tests.
//!
//! Spawns the `ainb-fixture-plugin` binary (built from
//! `tests/fixtures/fixture_plugin.rs`) under the runtime, exercises
//! every plugin-side wire method, then injects a SIGKILL to assert
//! crash recovery + quarantine semantics.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ainb_plugin_protocol::manifest::{
    Capabilities, Lifecycle, Manifest, PluginMeta, Provides, SpawnMode, Subscribes,
};
use ainb_plugin_protocol::params::Viewport;
use ainb_plugin_runtime::registry::RegisteredPlugin;
use ainb_plugin_runtime::types::{
    ActionOutcome, CliOutcome, LifecycleState, PluginId, RenderOutcome,
};
use ainb_plugin_runtime::{Runtime, RuntimeConfig};
use bytes::Bytes;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ainb-fixture-plugin"))
}

fn fixture_manifest() -> Manifest {
    Manifest {
        plugin: PluginMeta {
            name: "fixture".into(),
            version: "0.1.0".into(),
            abi_version: 2,
            description: "e2e fixture".into(),
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
    }
}

fn build_runtime() -> (Runtime, ainb_plugin_runtime::RuntimeHandle) {
    // Tighten backoff so the SIGKILL test doesn't sleep through three
    // 1 / 4 / 16 second waits.
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

fn register_fixture(rt: &Runtime) -> PluginId {
    let plugin = RegisteredPlugin::new(
        fixture_manifest(),
        fixture_path(),
        PathBuf::from("/dev/null/manifest.toml"),
    );
    let id = plugin.id.clone();
    rt.register(plugin);
    id
}

#[test]
fn render_and_cli_round_trip() {
    let (rt, handle) = build_runtime();
    let id = register_fixture(&rt);

    let render_rx = handle.render(&id, Viewport::new(40, 8), 0);
    let cli_rx = handle.dispatch_cli(&id, "echo", vec!["hi".into()]);

    let render = rt.tokio_handle().block_on(async {
        tokio::time::timeout(Duration::from_secs(5), render_rx)
            .await
            .expect("render timed out")
            .expect("render channel closed")
    });
    match render {
        RenderOutcome::Ok(buf) => {
            assert_eq!(buf.width, 1);
            assert_eq!(buf.height, 1);
            assert_eq!(buf.cells.len(), 1);
            assert_eq!(buf.cells[0].1.symbol, "X");
        }
        other => panic!("render outcome: {other:?}"),
    }

    let cli = rt.tokio_handle().block_on(async {
        tokio::time::timeout(Duration::from_secs(5), cli_rx)
            .await
            .expect("cli timed out")
            .expect("cli channel closed")
    });
    match cli {
        CliOutcome::Ok(r) => {
            assert_eq!(&r.stdout[..], b"ok\n");
            assert_eq!(r.exit_code, 0);
        }
        other => panic!("cli outcome: {other:?}"),
    }

    // try_recv_render must work synchronously after a render completes.
    // The cache may have been drained by the prior render call; issue
    // another and poll until it lands.
    drop(handle.render(&id, Viewport::new(40, 8), 1));
    let mut got = None;
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        if let Some(buf) = handle.try_recv_render(&id) {
            got = Some(buf);
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let buf = got.expect("try_recv_render never returned");
    assert_eq!(buf.cells[0].1.symbol, "X");
}

#[test]
fn snapshot_round_trip() {
    let (rt, handle) = build_runtime();
    let id = register_fixture(&rt);

    // Trigger lazy spawn — render forces the process up.
    drop(handle.render(&id, Viewport::new(20, 5), 0));

    // The fixture publishes `fixture.greeting` = b"hello" on startup.
    // Poll briefly because spawn + first frame is async.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut payload = None;
    while std::time::Instant::now() < deadline {
        if let Some(p) = handle.snapshot_get("fixture.greeting") {
            payload = Some(p);
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let p = payload.expect("snapshot never published");
    assert_eq!(&p[..], b"hello");

    // Host can publish too — must not panic, version must increase.
    let v1 = handle.publish_snapshot("from.host", Bytes::from_static(b"v1"));
    let v2 = handle.publish_snapshot("from.host", Bytes::from_static(b"v2"));
    assert!(v2 > v1);
    assert_eq!(handle.snapshot_get("from.host").as_deref(), Some(&b"v2"[..]));
}

#[test]
fn action_round_trip() {
    let (rt, handle) = build_runtime();
    let _id = register_fixture(&rt);

    // The fixture echoes the payload back through host/action/invoke.
    let rx = handle.invoke_action(
        "echo",
        Bytes::from_static(b"ping"),
        Duration::from_secs(2),
    );
    let outcome = rt.tokio_handle().block_on(async {
        tokio::time::timeout(Duration::from_secs(5), rx)
            .await
            .expect("action timed out")
            .expect("action channel closed")
    });
    match outcome {
        ActionOutcome::Ok(b) => assert_eq!(&b[..], b"ping"),
        other => panic!("action outcome: {other:?}"),
    }
}

#[test]
fn sigkill_triggers_respawn_then_quarantine() {
    let (rt, handle) = build_runtime();
    let id = register_fixture(&rt);

    // Lazy-spawn the plugin so there's a child to kill.
    drop(handle.render(&id, Viewport::new(10, 1), 0));

    // Wait for it to reach Running.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if matches!(
            handle.lifecycle_state(&id),
            Some(LifecycleState::Running)
        ) {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        handle.lifecycle_state(&id),
        Some(LifecycleState::Running),
        "plugin never reached Running"
    );

    // Inject 3 SIGKILLs in close succession to trip quarantine
    // (failure_window = 60s, threshold = 3).
    for _ in 0..3 {
        handle.inject_kill(&id).expect("inject_kill");
        // Give the runtime time to notice exit + record failure.
        std::thread::sleep(Duration::from_millis(300));
        // Force the fsm forward — issue a render so ensure_running()
        // attempts a respawn (which should also crash if we re-kill,
        // but we just want each cycle to count as a failure).
        drop(handle.render(&id, Viewport::new(10, 1), 0));
        std::thread::sleep(Duration::from_millis(300));
    }

    // Wait for quarantine.
    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    let mut quarantined = false;
    while std::time::Instant::now() < deadline {
        if matches!(
            handle.lifecycle_state(&id),
            Some(LifecycleState::Quarantined)
        ) {
            quarantined = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        quarantined,
        "plugin never quarantined; state = {:?}",
        handle.lifecycle_state(&id)
    );

    // Reload should clear quarantine.
    handle.reload(&id).expect("reload");
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        match handle.lifecycle_state(&id) {
            Some(LifecycleState::Idle) => return,
            _ => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    panic!(
        "reload didn't clear quarantine; state = {:?}",
        handle.lifecycle_state(&id)
    );
}

// Sanity: lifecycle_state for unknown plugin must not panic.
#[test]
fn unknown_plugin_lifecycle_returns_none() {
    let (_rt, handle) = build_runtime();
    assert!(handle.lifecycle_state(&PluginId::from("nope")).is_none());
}

// Make sure the runtime handle is Send + Clone — compile-time check.
#[allow(dead_code)]
fn handle_is_send_and_clone() {
    const fn assert_send_clone<T: Send + Clone + 'static>() {}
    assert_send_clone::<ainb_plugin_runtime::RuntimeHandle>();
    let _: Arc<dyn Send + Sync> = Arc::new(()) as Arc<dyn Send + Sync>;
}
