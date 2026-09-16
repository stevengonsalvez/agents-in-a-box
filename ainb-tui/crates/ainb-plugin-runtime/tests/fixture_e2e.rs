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
        // The fixture publishes snapshots, which the bus refuses without it.
        capabilities: Capabilities {
            event_bus: ainb_plugin_protocol::manifest::CapabilityGrant::Bool(true),
            ..Capabilities::default()
        },
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
        config: Vec::new(),
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

    // Lazy-spawn the plugin and WAIT for the first render to complete
    // before sending the key. `send_key` reports enqueue success, not
    // delivery: the priority key channel is drained ahead of the main
    // command inbox (`biased;` select), so a key enqueued before the
    // plugin task has processed the spawn-triggering Render command
    // races it, loses, and is dropped-idle (`handle_key_command`'s
    // `child.is_none()` arm). Awaiting the render outcome proves the
    // child is up, which is the precondition this test needs.
    let render_rx = handle.render(&id, Viewport::new(20, 5), 0);
    rt.tokio_handle().block_on(async {
        tokio::time::timeout(Duration::from_secs(5), render_rx)
            .await
            .expect("spawn render timed out")
            .expect("spawn render channel closed")
    });

    // Send a single key. Fixture re-publishes the params as a snapshot.
    let key = ainb_plugin_runtime::KeyEvent {
        code: ainb_plugin_runtime::KeyCode::Char { ch: '1' },
        mods: 0,
        kind: ainb_plugin_runtime::KeyKind::Press,
    };
    assert!(
        handle.send_key(&id, "ainb_analytics", key),
        "send_key enqueue failed"
    );

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
    assert_eq!(
        handle.snapshot_get("from.host").as_deref(),
        Some(&b"v2"[..])
    );
}

#[test]
fn action_round_trip() {
    let (rt, handle) = build_runtime();
    let _id = register_fixture(&rt);

    // The fixture echoes the payload back through host/action/invoke.
    let rx = handle.invoke_action("echo", Bytes::from_static(b"ping"), Duration::from_secs(2));
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

/// A binary that can never be exec'd (the post-`brew upgrade` state of a lazy
/// plugin) must trip the same circuit breaker as a process that starts and
/// dies. The spawn-failure branch used to return with the state still
/// `Spawning` — which `ensure_running` treats as spawnable — and the
/// quarantine check lived only on the exit path, so this case retried the
/// exec on every kick forever and never quarantined.
#[test]
fn unspawnable_binary_quarantines_instead_of_retrying_forever() {
    let (rt, handle) = build_runtime();
    let plugin = RegisteredPlugin::new(
        fixture_manifest(),
        PathBuf::from("/nonexistent/plugin-binary-that-cannot-be-spawned"),
        PathBuf::from("/dev/null/manifest.toml"),
    );
    let id = plugin.id.clone();
    rt.register(plugin);

    // threshold = 3 failures inside failure_window.
    for _ in 0..3 {
        drop(handle.render(&id, Viewport::new(10, 1), 0));
        std::thread::sleep(Duration::from_millis(150));
    }

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
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
        "a plugin whose binary cannot be spawned must quarantine; state = {:?}",
        handle.lifecycle_state(&id)
    );

    rt.shutdown();
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
        if matches!(handle.lifecycle_state(&id), Some(LifecycleState::Running)) {
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

// `host` is the reserved publisher id stamped on host-side snapshot
// publishes; a plugin must not be able to register under it on ANY path
// (discovery filters it; `Runtime::register` rejects it too). Build a
// manifest named `host` and register it directly — it must be a no-op,
// so the runtime never knows a `host` plugin (lifecycle_state == None).
#[test]
fn register_rejects_reserved_host_name() {
    let (rt, handle) = build_runtime();
    let mut manifest = fixture_manifest();
    manifest.plugin.name = "host".into();
    let plugin = RegisteredPlugin::new(
        manifest,
        fixture_path(),
        PathBuf::from("/dev/null/manifest.toml"),
    );
    rt.register(plugin);
    assert!(
        handle.lifecycle_state(&PluginId::from("host")).is_none(),
        "a plugin named `host` must be refused by Runtime::register"
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

/// A runtime whose background idle tick never reaps inside a test (a one-hour
/// window): every reap below is driven by `reap_if_idle`, which runs the same
/// decision as if the window had passed, so nothing waits on the 5 s tick.
fn reap_runtime() -> (Runtime, ainb_plugin_runtime::RuntimeHandle) {
    Runtime::with_config(RuntimeConfig {
        idle_reap: Duration::from_secs(3_600),
        ..RuntimeConfig::default()
    })
    .expect("build runtime")
}

fn register_subscriber(rt: &Runtime, name: &str, latest_state: Vec<String>) -> PluginId {
    let mut manifest = fixture_manifest();
    manifest.plugin.name = name.into();
    manifest.lifecycle.idle_reap_secs = 0;
    manifest.subscribes = Subscribes {
        snapshots: vec!["fleet.agent_status".into()],
        latest_state,
    };
    let plugin = RegisteredPlugin::new(
        manifest,
        fixture_path(),
        PathBuf::from("/dev/null/manifest.toml"),
    );
    let id = plugin.id.clone();
    rt.register(plugin);
    id
}

fn register_named(rt: &Runtime, name: &str) -> PluginId {
    let mut manifest = fixture_manifest();
    manifest.plugin.name = name.into();
    manifest.lifecycle.idle_reap_secs = 0;
    let plugin = RegisteredPlugin::new(
        manifest,
        fixture_path(),
        PathBuf::from("/dev/null/manifest.toml"),
    );
    let id = plugin.id.clone();
    rt.register(plugin);
    id
}

/// Start one lazy plugin and wait for it to run. The bound is a hang guard:
/// with a one-hour idle window nothing can take it back to idle meanwhile.
fn start(handle: &ainb_plugin_runtime::RuntimeHandle, id: &PluginId) {
    drop(handle.render(id, Viewport::new(1, 1), 0));
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while handle.lifecycle_state(id) != Some(LifecycleState::Running) {
        assert!(
            std::time::Instant::now() < deadline,
            "{id} never started: {:?}",
            handle.lifecycle_state(id)
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Run the idle-reap decision for `id` now and return whether it reaped.
fn reap_if_idle(rt: &Runtime, handle: &ainb_plugin_runtime::RuntimeHandle, id: &PluginId) -> bool {
    let rx = handle.reap_if_idle(id).expect("registered");
    rt.tokio_handle().block_on(rx).expect("the task answers the check")
}

/// #1040: idle reap is gated on the manifest's `latest_state` marker. A plugin
/// whose only subscription is a latest-state topic is reaped like any other
/// once idle; one subscribed to a stream is kept running.
#[test]
fn a_latest_state_subscriber_is_reaped_when_idle_and_a_stream_subscriber_is_not() {
    let (rt, handle) = reap_runtime();
    let latest = register_subscriber(&rt, "reaped-latest", vec!["fleet.agent_status".into()]);
    let stream = register_subscriber(&rt, "kept-stream", Vec::new());
    start(&handle, &latest);
    start(&handle, &stream);

    assert!(
        reap_if_idle(&rt, &handle, &latest),
        "a latest-state subscriber is reaped"
    );
    assert_eq!(handle.lifecycle_state(&latest), Some(LifecycleState::Idle));
    assert!(
        !reap_if_idle(&rt, &handle, &stream),
        "a stream subscriber is not reaped"
    );
    assert_eq!(
        handle.lifecycle_state(&stream),
        Some(LifecycleState::Running)
    );
}

/// #1053 review item 3: a plugin whose screen the host has on display is not
/// idle-reaped, however long nothing changes on it; one off screen is.
#[test]
fn a_shown_plugin_past_its_idle_window_is_not_reaped_and_a_hidden_one_is() {
    let (rt, handle) = reap_runtime();
    let shown = register_named(&rt, "shown");
    let hidden = register_named(&rt, "hidden");
    start(&handle, &shown);
    start(&handle, &hidden);
    // The host's render tick for the screen on display, and nothing else.
    let _ = handle.take_render_dirty(&shown);

    assert!(
        !reap_if_idle(&rt, &handle, &shown),
        "a plugin on display is in use"
    );
    assert_eq!(
        handle.lifecycle_state(&shown),
        Some(LifecycleState::Running)
    );
    assert!(
        reap_if_idle(&rt, &handle, &hidden),
        "a hidden idle plugin is reaped"
    );
    assert_eq!(handle.lifecycle_state(&hidden), Some(LifecycleState::Idle));
}

/// #1053 review item 4: a reap answers a request still in flight with a
/// runtime error, not a dropped channel.
#[test]
fn a_reap_answers_an_in_flight_request_with_a_runtime_error() {
    let (rt, handle) = reap_runtime();
    let id = register_named(&rt, "hanging");
    // The dispatch spawns the plugin and leaves the request in flight; the
    // reap check queues behind it on the same inbox, so the ledger holds the
    // request when the reap runs.
    let rx = handle.dispatch_cli(&id, "echo", vec!["hang".into()]);
    assert!(
        reap_if_idle(&rt, &handle, &id),
        "a running idle plugin is reaped"
    );
    match rt.tokio_handle().block_on(rx) {
        Ok(CliOutcome::RuntimeError(why)) => assert_eq!(why, "plugin reaped"),
        other => panic!("expected a runtime error from the reap, got {other:?}"),
    }
}

/// #1063 review item 1: deliveries on a latest-state topic are not use. A
/// plugin fed only the host's card-clock ticks for longer than its idle window,
/// with no render, key or action, is still reaped; the same plugin rendering
/// through that window is kept.
#[test]
fn a_plugin_fed_only_latest_state_ticks_is_still_reaped() {
    const CLOCK: &str = "fleet.agent_status.clock";
    const WINDOW: Duration = Duration::from_millis(300);
    let (rt, handle) = Runtime::with_config(RuntimeConfig {
        idle_reap: WINDOW,
        ..RuntimeConfig::default()
    })
    .expect("build runtime");
    let register = |name: &str| {
        let mut manifest = fixture_manifest();
        manifest.plugin.name = name.into();
        manifest.lifecycle.idle_reap_secs = 0;
        manifest.capabilities.event_bus =
            ainb_plugin_protocol::manifest::CapabilityGrant::List(vec![
                CLOCK.into(),
                "fixture.*".into(),
            ]);
        manifest.subscribes = Subscribes {
            snapshots: vec![CLOCK.into()],
            latest_state: vec![CLOCK.into()],
        };
        let plugin = RegisteredPlugin::new(
            manifest,
            fixture_path(),
            PathBuf::from("/dev/null/manifest.toml"),
        );
        let id = plugin.id.clone();
        rt.register(plugin);
        id
    };
    let ticked = register("ticked");
    let rendered = register("rendered");
    for id in [&ticked, &rendered] {
        start(&handle, id);
        let subscribed = handle.dispatch_cli(id, "echo", vec!["subscribe".into(), CLOCK.into()]);
        assert!(matches!(
            rt.tokio_handle().block_on(subscribed),
            Ok(CliOutcome::Ok(_))
        ));
    }

    // Tick through more than the idle window. Only `rendered` is used.
    let started = std::time::Instant::now();
    let mut ticks = 0;
    while started.elapsed() < WINDOW + Duration::from_millis(200) {
        handle.publish_snapshot(
            CLOCK,
            bytes::Bytes::from(format!("{{\"clock_ms\":{ticks}}}")),
        );
        drop(handle.render(&rendered, Viewport::new(1, 1), ticks));
        ticks += 1;
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(
        handle.snapshot_get("fixture.received").is_some(),
        "the ticks reached the subscribers"
    );

    let past_window = |id: &PluginId| -> bool {
        let rx = handle.reap_if_past_window(id).expect("registered");
        rt.tokio_handle().block_on(rx).expect("the task answers the check")
    };
    // `ticked` last used at its subscribe, more than the window ago; the idle
    // tick may have reaped it already, which is the same verdict.
    assert!(
        past_window(&ticked) || handle.lifecycle_state(&ticked) == Some(LifecycleState::Idle),
        "ticks alone kept the plugin alive: {:?}",
        handle.lifecycle_state(&ticked)
    );
    assert_eq!(handle.lifecycle_state(&ticked), Some(LifecycleState::Idle));
    assert!(!past_window(&rendered), "a rendering plugin is in use");
    assert_eq!(
        handle.lifecycle_state(&rendered),
        Some(LifecycleState::Running)
    );
}

/// #1089: `fleet.` topics are host-publish-only. A plugin granted the card
/// clock by name can subscribe to it, but its publish there is dropped, so no
/// subscriber can be fed a spoofed tick; a publish on its other granted topic
/// still lands.
#[test]
fn a_plugin_granted_a_fleet_topic_cannot_publish_on_it() {
    const CLOCK: &str = "fleet.agent_status.clock";
    let (rt, handle) = Runtime::new().expect("build runtime");
    let mut manifest = fixture_manifest();
    manifest.capabilities.event_bus = ainb_plugin_protocol::manifest::CapabilityGrant::List(vec![
        CLOCK.into(),
        "fixture.*".into(),
    ]);
    let plugin = RegisteredPlugin::new(
        manifest,
        fixture_path(),
        PathBuf::from("/dev/null/manifest.toml"),
    );
    let id = plugin.id.clone();
    rt.register(plugin);
    start(&handle, &id);

    // Both publishes go out on the plugin's one wire in this order, so once
    // the second has landed the first has been judged.
    for topic in [CLOCK, "fixture.after_clock"] {
        let rx = handle.dispatch_cli(&id, "echo", vec!["publish".into(), topic.into()]);
        assert!(matches!(
            rt.tokio_handle().block_on(rx),
            Ok(CliOutcome::Ok(_))
        ));
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while handle.snapshot_get("fixture.after_clock").is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "the granted fixture publish never landed"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        handle.snapshot_get(CLOCK).is_none(),
        "a plugin publish reached the host-publish-only card clock"
    );
}

/// A registered fixture that has stopped reading its stdin, and the key the
/// tests flood it with. `write_bound` is the runtime's frame write timeout.
fn wedged_fixture(
    write_bound: Duration,
) -> (Runtime, ainb_plugin_runtime::RuntimeHandle, PluginId) {
    let (rt, handle) = Runtime::with_config(RuntimeConfig {
        frame_write_timeout: write_bound,
        ..RuntimeConfig::default()
    })
    .expect("build runtime");
    let plugin = RegisteredPlugin::new(
        fixture_manifest(),
        fixture_path(),
        PathBuf::from("/dev/null/manifest.toml"),
    );
    let id = plugin.id.clone();
    rt.register(plugin);
    start(&handle, &id);

    // Never answered: the fixture parks instead of reading on. Flood only once
    // it has said so, so the keys really do meet a plugin that is not reading.
    drop(handle.dispatch_cli(&id, "echo", vec!["wedge".into()]));
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while handle.snapshot_get("fixture.wedged").is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "the fixture never wedged"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    (rt, handle, id)
}

fn flood_key() -> ainb_plugin_protocol::params::KeyEvent {
    ainb_plugin_protocol::params::KeyEvent {
        code: ainb_plugin_protocol::params::KeyCode::Char { ch: 'j' },
        mods: 0,
        kind: ainb_plugin_protocol::params::KeyKind::Press,
    }
}

/// #1087: a plugin that stops reading its stdin costs bounded memory however
/// long the user keeps typing at it. Once the pipe is full the task cannot
/// write, so keys pile up in the inbox, which holds at most its capacity and
/// counts every event it pushes out. The write bound is set far past the test
/// so the plugin is still wedged, not yet dropped, when the inbox is read.
#[test]
fn a_wedged_plugin_keeps_a_bounded_key_inbox_and_counts_drops() {
    const KEYS: usize = 20_000;
    let (_rt, handle, id) = wedged_fixture(Duration::from_secs(600));
    for _ in 0..KEYS {
        assert!(
            handle.send_key(&id, "fixture", flood_key()),
            "the task is alive"
        );
    }

    let stats = handle.input_inbox_stats(&id).expect("registered");
    // Full, and no fuller. One short of full is the same verdict: the task
    // may have taken one key off the queue and be blocked writing it.
    let capacity = ainb_plugin_runtime::inbox::INPUT_INBOX_CAPACITY;
    assert!(
        (capacity - 1..=capacity).contains(&stats.keys_queued),
        "a plugin that is not reading leaves the inbox full, and no fuller: {stats:?}"
    );
    assert!(
        stats.keys_dropped > 0,
        "{KEYS} keys into a wedged plugin dropped none: {stats:?}"
    );
}

/// #1118: a plugin that stops reading its stdin cannot trap Esc. A frame write
/// that outlives the bound flags the plugin wedged, which is what makes the
/// host take a back key itself (`effect_host` reports it undelivered, and the
/// reducer runs `PanelBack`), and the plugin is dropped like a closed pipe.
/// Before the bound, the task sat in its write forever and kept both from
/// happening.
#[test]
fn a_plugin_that_stops_reading_stdin_releases_esc_within_the_write_bound() {
    const BOUND: Duration = Duration::from_millis(300);
    let (_rt, handle, id) = wedged_fixture(BOUND);
    assert!(!handle.render_wedged(&id), "precondition: not wedged yet");

    // Type at it until its pipe is full and a write blocks. The inbox drops
    // the oldest keys, so a single burst writes only a few hundred; keeping on
    // typing is what a user facing a frozen screen does anyway.
    let typing = std::time::Instant::now();
    let deadline = typing + BOUND + Duration::from_secs(10);
    let mut pipe_full_at = None;
    while !handle.render_wedged(&id) {
        assert!(
            std::time::Instant::now() < deadline,
            "a plugin not reading its stdin still held Esc {:?} after typing began",
            typing.elapsed()
        );
        for _ in 0..200 {
            handle.send_key(&id, "fixture", flood_key());
        }
        // The inbox stops draining once the task is parked in a write.
        let stats = handle.input_inbox_stats(&id).expect("registered");
        if pipe_full_at.is_none()
            && stats.keys_queued == ainb_plugin_runtime::inbox::INPUT_INBOX_CAPACITY
        {
            pipe_full_at = Some(std::time::Instant::now());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    // The inbox stays at capacity once the task is parked in its write, so
    // the loop above always saw it full before the plugin was flagged.
    let full = pipe_full_at.expect("the key inbox filled before the plugin was flagged");
    assert!(
        full.elapsed() < BOUND + Duration::from_secs(2),
        "Esc was released {:?} after the task stopped draining, past the {BOUND:?} bound",
        full.elapsed()
    );
    // An Esc now: delivered to the runtime, but not serviced, so the host
    // takes it (the `delivered && !render_wedged` rule in `effect_host`).
    let esc = ainb_plugin_protocol::params::KeyEvent {
        code: ainb_plugin_protocol::params::KeyCode::Esc,
        mods: 0,
        kind: ainb_plugin_protocol::params::KeyKind::Press,
    };
    let serviced = handle.send_key(&id, "fixture", esc) && !handle.render_wedged(&id);
    assert!(
        !serviced,
        "the host takes the back key from a wedged plugin"
    );

    // And the plugin is gone, the way a closed pipe drops it. A deadline of its
    // own: the one above may be nearly spent by the typing loop.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while handle.lifecycle_state(&id) == Some(LifecycleState::Running) {
        assert!(
            std::time::Instant::now() < deadline,
            "the wedged plugin was never dropped: {:?}",
            handle.lifecycle_state(&id)
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A fixture plugin on the render loop, for the #1087 Esc watch.
fn esc_plugin(name: &str) -> (Runtime, ainb_plugin_runtime::RuntimeHandle, PluginId) {
    let (rt, handle) = Runtime::new().expect("build runtime");
    let mut manifest = fixture_manifest();
    manifest.plugin.name = name.into();
    let plugin = RegisteredPlugin::new(
        manifest,
        fixture_path(),
        PathBuf::from("/dev/null/manifest.toml"),
    );
    let id = plugin.id.clone();
    rt.register(plugin);
    start(&handle, &id);
    (rt, handle, id)
}

fn esc() -> ainb_plugin_protocol::params::KeyEvent {
    ainb_plugin_protocol::params::KeyEvent {
        code: ainb_plugin_protocol::params::KeyCode::Esc,
        mods: 0,
        kind: ainb_plugin_protocol::params::KeyKind::Press,
    }
}

/// Paint one frame and wait for it, as the host's render tick does after a key.
fn paint(
    rt: &Runtime,
    handle: &ainb_plugin_runtime::RuntimeHandle,
    id: &PluginId,
    generation: u64,
) {
    let rx = handle.render(id, Viewport::new(1, 1), generation);
    assert!(matches!(
        rt.tokio_handle().block_on(rx),
        Ok(RenderOutcome::Ok(_))
    ));
}

/// #1087: a plugin that keeps painting the same frame while ignoring Esc no
/// longer holds its screen. After `ESC_UNANSWERED_LIMIT` unanswered presses the
/// next Esc is refused, which the host reads as a back key it must take.
#[test]
fn a_plugin_ignoring_esc_gives_the_next_one_to_the_host() {
    let limit = ainb_plugin_runtime::plugin_task::ESC_UNANSWERED_LIMIT;
    let (rt, handle, id) = esc_plugin("ignores-esc");
    for n in 1..=u64::from(limit) {
        assert!(
            handle.send_key(&id, "fixture", esc()),
            "esc {n} is delivered"
        );
        paint(&rt, &handle, &id, n);
    }
    assert!(
        !handle.send_key(&id, "fixture", esc()),
        "after {limit} ignored Esc presses the next one returns to the host"
    );
}

/// A plugin that pops one nested level per Esc answers every press with a new
/// frame, so it is never ejected, however deep it goes.
#[test]
fn a_plugin_popping_a_level_per_esc_keeps_every_esc() {
    const LEVELS: u64 = 6;
    let (rt, handle, id) = esc_plugin("pops-levels");
    let set = handle.dispatch_cli(&id, "echo", vec!["levels".into(), LEVELS.to_string()]);
    assert!(matches!(
        rt.tokio_handle().block_on(set),
        Ok(CliOutcome::Ok(_))
    ));
    for n in 1..=LEVELS {
        assert!(
            handle.send_key(&id, "fixture", esc()),
            "esc {n} is delivered"
        );
        paint(&rt, &handle, &id, n);
    }
}

/// Esc presses with no frame painted in between carry no evidence, so a burst
/// faster than the render tick is not ejected on the frame check. It still
/// meets the press ceiling (#1087 review): past `ESC_PRESS_CEILING` presses
/// with no other key, the next Esc goes to the host.
#[test]
fn an_esc_burst_is_delivered_up_to_the_press_ceiling() {
    let ceiling = ainb_plugin_runtime::plugin_task::ESC_PRESS_CEILING;
    let (_rt, handle, id) = esc_plugin("esc-burst");
    for n in 1..=ceiling {
        assert!(
            handle.send_key(&id, "fixture", esc()),
            "esc {n} is delivered"
        );
    }
    assert!(
        !handle.send_key(&id, "fixture", esc()),
        "past {ceiling} Esc presses in a row the next one returns to the host"
    );
}
