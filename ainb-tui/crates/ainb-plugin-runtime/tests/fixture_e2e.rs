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
fn send_key_forwards_handle_key_notification() {
    let (rt, handle) = build_runtime();
    let id = register_fixture(&rt);

    // Lazy-spawn the plugin so there's a stdin to write to.
    drop(handle.render(&id, Viewport::new(20, 5), 0));

    // Send a single key. Fixture re-publishes the params as a snapshot.
    let key = ainb_plugin_runtime::KeyEvent {
        code: ainb_plugin_runtime::KeyCode::Char { ch: '1' },
        mods: 0,
        kind: ainb_plugin_runtime::KeyKind::Press,
    };
    // Retry briefly in case the spawn hasn't completed yet — `send_key`
    // drops on idle (the plugin task hasn't transitioned to Running),
    // which would race with the lazy-spawn we just kicked off.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if handle.send_key(&id, "ainb_analytics", key.clone()) {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut payload = None;
    while std::time::Instant::now() < deadline {
        if let Some(p) = handle.snapshot_get("fixture.last_key") {
            payload = Some(p);
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let bytes = payload.expect("fixture never re-published last_key snapshot");
    let decoded: serde_json::Value =
        serde_json::from_slice(&bytes).expect("fixture payload is JSON");

    // Wire shape: { screen_id, key: { code: {type:"char", ch:"1"}, mods, kind }, generation }
    assert_eq!(decoded["screen_id"], "ainb_analytics");
    assert_eq!(decoded["key"]["code"]["type"], "char");
    assert_eq!(decoded["key"]["code"]["ch"], "1");
    assert_eq!(decoded["key"]["kind"], "press");
    assert!(
        decoded["generation"].is_u64(),
        "generation should be present and numeric"
    );
}

#[test]
fn render_dirty_flag_is_event_driven() {
    // Verifies the render-dirty gate that drives the host's
    // event-driven render-tick loop:
    //
    //   - Registration seeds the flag to `true` (first paint must fire).
    //   - One `take_render_dirty` consumes that seed; the next call
    //     returns `false` because nothing has happened since.
    //   - `send_key` flips the flag back to `true`.
    //   - `mark_render_dirty` works as an out-of-band signal (e.g.
    //     viewport resize) without needing a key event.
    //
    // Without this gate `tick_plugin_renders` would kick a
    // `plugin/render` every tick (~30/s at the 33 ms cadence)
    // regardless of state changes — the regression we're guarding.
    let (rt, handle) = build_runtime();
    let id = register_fixture(&rt);

    // Registration seeds dirty=true so first paint after spawn fires.
    assert!(
        handle.take_render_dirty(&id),
        "registration must seed dirty=true so first paint fires"
    );
    // Second call drains nothing — nothing has happened since.
    assert!(
        !handle.take_render_dirty(&id),
        "idle take must return false — render storm regression guard"
    );

    // Lazy-spawn so `send_key` has somewhere to send.
    drop(handle.render(&id, Viewport::new(20, 5), 0));
    // The render kick above also DOESN'T set dirty (renders are
    // consumers, not producers). Drain anything the spawn-side may
    // have set so the next assertion is clean.
    let _ = handle.take_render_dirty(&id);

    // send_key sets dirty=true (retry while lazy-spawn races).
    let key = ainb_plugin_runtime::KeyEvent {
        code: ainb_plugin_runtime::KeyCode::Tab,
        mods: 0,
        kind: ainb_plugin_runtime::KeyKind::Press,
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if handle.send_key(&id, "ainb_analytics", key.clone()) {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        handle.take_render_dirty(&id),
        "send_key must set dirty=true so a render kick lands"
    );
    assert!(
        !handle.take_render_dirty(&id),
        "second take after send_key must be false"
    );

    // mark_render_dirty as an out-of-band signal.
    handle.mark_render_dirty(&id);
    assert!(
        handle.take_render_dirty(&id),
        "mark_render_dirty must set dirty=true"
    );

    // Unknown plugins must not panic and must return false.
    let unknown = PluginId::from("definitely-not-a-plugin");
    assert!(!handle.take_render_dirty(&unknown));
    handle.mark_render_dirty(&unknown); // must be a no-op
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

// Eager-respawn regression: an eager plugin that exits (crash, broken
// pipe, etc.) must come back automatically after the backoff window —
// not only at registration time. Without this guarantee, a single
// transient failure wedges the plugin dead for the rest of the TUI
// session. The original bug: session-reader shipped one oversize
// chunk, host framer rejected it, plugin's stdout pipe closed, plugin
// exited; burndown UI stayed stuck at "Scanning sessions…" forever
// because session-reader never respawned.
#[test]
fn eager_plugin_respawns_automatically_after_exit() {
    let (rt, handle) = build_runtime();
    let mut manifest = fixture_manifest();
    manifest.lifecycle.spawn = SpawnMode::Eager;
    manifest.plugin.name = "fixture-eager-respawn".into();
    let plugin = RegisteredPlugin::new(
        manifest,
        fixture_path(),
        PathBuf::from("/dev/null/manifest.toml"),
    );
    let id = plugin.id.clone();
    rt.register(plugin);

    // Wait for the initial eager spawn.
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        if matches!(handle.lifecycle_state(&id), Some(LifecycleState::Running)) {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        handle.lifecycle_state(&id),
        Some(LifecycleState::Running),
        "eager plugin never reached initial Running"
    );

    // Kill the plugin process. The runtime should observe pipe close,
    // log "plugin exited / pipe closed", run through backoff, then
    // respawn because spawn=eager. No host request (render/cli) needed
    // to trigger the respawn — that's the whole point of this test.
    handle.inject_kill(&id).expect("inject_kill");

    // Backoff is 50ms in the test config, plus exec latency. Give it
    // generous headroom — the respawn path includes child spawn,
    // PluginInit RPC, and reading the init response.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut respawned = false;
    while std::time::Instant::now() < deadline {
        if matches!(handle.lifecycle_state(&id), Some(LifecycleState::Running)) {
            respawned = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    assert!(
        respawned,
        "eager plugin did not auto-respawn after exit; state = {:?}",
        handle.lifecycle_state(&id)
    );
}

// Eager-spawn regression: manifest declaring `spawn = "eager"` must
// cause the runtime to launch the plugin process immediately at
// registration time, without waiting for a first request. Without
// this guarantee any pure-publisher plugin (e.g. session-reader)
// never starts and downstream consumers stall on snapshot fetch.
#[test]
fn eager_spawn_starts_process_without_first_request() {
    let (rt, handle) = build_runtime();
    let mut manifest = fixture_manifest();
    manifest.lifecycle.spawn = SpawnMode::Eager;
    manifest.plugin.name = "fixture-eager".into();
    let plugin = RegisteredPlugin::new(
        manifest,
        fixture_path(),
        PathBuf::from("/dev/null/manifest.toml"),
    );
    let id = plugin.id.clone();
    rt.register(plugin);

    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        if matches!(handle.lifecycle_state(&id), Some(LifecycleState::Running)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!(
        "eager plugin never reached Running; state = {:?}",
        handle.lifecycle_state(&id)
    );
}

// Same guarantee via the `RuntimeHandle::discover` codepath — the
// real ainb-core ingress point. The handle clones the discovery for
// each plugin under the root, so this exercises the parallel branch
// of the eager-spawn fix.
#[test]
fn eager_spawn_via_handle_discover_starts_process() {
    use std::fs;
    let (_rt, handle) = build_runtime();
    let tmp = tempfile::tempdir().expect("tempdir");
    let plugin_dir = tmp.path().join("fixture-eager-discover");
    fs::create_dir_all(&plugin_dir).unwrap();

    // Copy the fixture binary into the discoverable layout.
    let bin_dst = plugin_dir.join("fixture-eager-discover");
    fs::copy(fixture_path(), &bin_dst).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut p = fs::metadata(&bin_dst).unwrap().permissions();
        p.set_mode(0o755);
        fs::set_permissions(&bin_dst, p).unwrap();
    }

    // Manifest the registry expects at `<root>/<name>/manifest.toml`.
    let manifest_toml = r#"
[plugin]
name = "fixture-eager-discover"
version = "0.1.0"
abi_version = 2
description = "eager-spawn discover regression"

[capabilities]
[provides]
screens = []
commands = []
cli_namespaces = ["echo"]
snapshots = ["fixture.greeting"]
[subscribes]
[lifecycle]
spawn = "eager"
idle_reap_secs = 600
"#;
    fs::write(plugin_dir.join("manifest.toml"), manifest_toml).unwrap();

    let plugins = handle.discover(tmp.path()).expect("discover");
    assert_eq!(plugins.len(), 1, "expected single plugin discovered");
    let id = plugins[0].id.clone();

    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        if matches!(handle.lifecycle_state(&id), Some(LifecycleState::Running)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!(
        "eager plugin (via discover) never reached Running; state = {:?}",
        handle.lifecycle_state(&id)
    );
}

// Inverse guarantee: lazy plugins must stay Idle until a request arrives.
#[test]
fn lazy_spawn_stays_idle_without_request() {
    let (rt, handle) = build_runtime();
    let mut manifest = fixture_manifest();
    manifest.lifecycle.spawn = SpawnMode::Lazy;
    manifest.plugin.name = "fixture-lazy".into();
    let plugin = RegisteredPlugin::new(
        manifest,
        fixture_path(),
        PathBuf::from("/dev/null/manifest.toml"),
    );
    let id = plugin.id.clone();
    rt.register(plugin);

    // Give the runtime a moment to settle.
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        matches!(handle.lifecycle_state(&id), Some(LifecycleState::Idle)),
        "lazy plugin must remain Idle without a request; state = {:?}",
        handle.lifecycle_state(&id)
    );
}
