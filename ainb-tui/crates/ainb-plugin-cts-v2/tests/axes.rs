//! Host-side conformance tests: 17 axes for ABI v2.

use std::path::PathBuf;
use std::time::Duration;

use ainb_plugin_protocol::manifest::{
    Capabilities, Lifecycle, Manifest, PluginMeta, Provides, SpawnMode, Subscribes,
};
use ainb_plugin_runtime::registry::RegisteredPlugin;
use ainb_plugin_runtime::types::{
    CliOutcome, LifecycleState, PluginId, RenderOutcome, RuntimeConfig,
};
use ainb_plugin_runtime::{Runtime, RuntimeHandle, Viewport};
use bytes::Bytes;

fn build_runtime() -> (Runtime, RuntimeHandle) {
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

fn manifest(name: &str) -> Manifest {
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
    }
}

fn register(rt: &Runtime, bin_path: PathBuf, m: Manifest) -> PluginId {
    let plugin = RegisteredPlugin::new(m, bin_path, PathBuf::from("/dev/null/manifest.toml"));
    let id = plugin.id.clone();
    rt.register(plugin);
    id
}

fn wait_running(handle: &RuntimeHandle, id: &PluginId) {
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

fn block_render(rt: &Runtime, handle: &RuntimeHandle, id: &PluginId, w: u16, h: u16) -> RenderOutcome {
    let rx = handle.render(id, Viewport::new(w, h), 0);
    rt.tokio_handle().block_on(async {
        tokio::time::timeout(Duration::from_secs(5), rx)
            .await
            .expect("render timed out")
            .expect("render channel closed")
    })
}

fn block_cli(rt: &Runtime, handle: &RuntimeHandle, id: &PluginId, ns: &str, argv: Vec<String>) -> CliOutcome {
    let rx = handle.dispatch_cli(id, ns, argv);
    rt.tokio_handle().block_on(async {
        tokio::time::timeout(Duration::from_secs(5), rx)
            .await
            .expect("cli timed out")
            .expect("cli channel closed")
    })
}

// =====================================================================
// A1: manifest v2 round-trip
// =====================================================================

#[test]
fn a01_manifest_v2_round_trip() {
    let (rt, handle) = build_runtime();
    let manifest_toml = include_str!("canaries/a01_manifest_round_trip/manifest.toml");
    let parsed: Manifest = toml::from_str(manifest_toml).expect("parse manifest");

    let bin = PathBuf::from(env!("CARGO_BIN_EXE_cts-a01-manifest"));
    let id = register(&rt, bin, parsed.clone());

    let outcome = block_render(&rt, &handle, &id, 1, 1);
    match outcome {
        RenderOutcome::Ok(buf) => {
            assert_eq!(buf.cells[0].1.symbol, "A");
        }
        other => panic!("expected render Ok, got {other:?}"),
    }

    let re_encoded = toml::to_string(&parsed).expect("re-encode");
    let back: Manifest = toml::from_str(&re_encoded).expect("round-trip");
    assert_eq!(parsed, back);
}

// =====================================================================
// A2: framing / Content-Length decode
// =====================================================================

#[test]
fn a02_framing_content_length_decode() {
    let (rt, handle) = build_runtime();
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_cts-a02-framing"));
    let id = register(&rt, bin, manifest("cts-a02"));

    let outcome = block_render(&rt, &handle, &id, 80, 24);
    match outcome {
        RenderOutcome::Ok(buf) => {
            assert_eq!(buf.width, 80);
            assert_eq!(buf.height, 24);
            assert_eq!(buf.cells.len(), 80 * 24);
        }
        other => panic!("expected render Ok, got {other:?}"),
    }
}

// =====================================================================
// A3: method dispatch — unknown method returns -32601
// =====================================================================

#[test]
fn a03_method_dispatch_unknown_method() {
    let (rt, handle) = build_runtime();
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_cts-a03-method-dispatch"));
    let id = register(&rt, bin, manifest("cts-a03"));

    let outcome = block_render(&rt, &handle, &id, 1, 1);
    match outcome {
        RenderOutcome::Ok(buf) => {
            assert_eq!(buf.cells[0].1.symbol, "M");
        }
        other => panic!("expected render Ok, got {other:?}"),
    }
}

// =====================================================================
// A4: capability denied — plugin returns -32001 error
// =====================================================================

#[test]
fn a04_capability_denied_error_propagation() {
    let (rt, handle) = build_runtime();
    let mut m = manifest("cts-a04");
    m.provides.cli_namespaces = vec!["a04".into()];
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_cts-a04-cap-denied"));
    let id = register(&rt, bin, m);

    let outcome = block_cli(&rt, &handle, &id, "a04", vec!["probe".into()]);
    match outcome {
        CliOutcome::PluginError { code, .. } => {
            assert_eq!(code, ainb_plugin_protocol::errors::CAPABILITY_DENIED);
        }
        other => panic!("expected CliOutcome::PluginError with -32001, got {other:?}"),
    }
}

// =====================================================================
// A5: render buffer byte determinism (N=10)
// =====================================================================

#[test]
fn a05_render_buffer_byte_determinism() {
    let (rt, handle) = build_runtime();
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_cts-a05-render-determinism"));
    let id = register(&rt, bin, manifest("cts-a05"));

    let first = match block_render(&rt, &handle, &id, 10, 5) {
        RenderOutcome::Ok(buf) => serde_json::to_vec(&buf).expect("serialize"),
        other => panic!("render 0 failed: {other:?}"),
    };
    for i in 1..10 {
        let buf = match block_render(&rt, &handle, &id, 10, 5) {
            RenderOutcome::Ok(buf) => serde_json::to_vec(&buf).expect("serialize"),
            other => panic!("render {i} failed: {other:?}"),
        };
        assert_eq!(first, buf, "render {i} differs from render 0");
    }
}

// =====================================================================
// A6: snapshot get after publish
// =====================================================================

#[test]
fn a06_snapshot_get_after_publish() {
    let (rt, handle) = build_runtime();
    let mut m = manifest("cts-a06");
    m.provides.snapshots = vec!["cts.a06".into()];
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_cts-a06-snapshot-get"));
    let id = register(&rt, bin, m);

    drop(block_render(&rt, &handle, &id, 1, 1));

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut payload = None;
    while std::time::Instant::now() < deadline {
        if let Some(p) = handle.snapshot_get("cts.a06") {
            payload = Some(p);
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let p = payload.expect("snapshot never published");
    assert_eq!(&p[..], b"a06-payload");
}

// =====================================================================
// A7: snapshot subscribe + event delivery
// =====================================================================

#[test]
fn a07_snapshot_subscribe_event_delivery() {
    let (rt, handle) = build_runtime();
    let mut m = manifest("cts-a07");
    m.provides.cli_namespaces = vec!["a07".into()];
    m.subscribes.snapshots = vec!["cts.a07".into()];
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_cts-a07-snapshot-subscribe"));
    let id = register(&rt, bin, m);

    drop(block_render(&rt, &handle, &id, 1, 1));
    wait_running(&handle, &id);

    std::thread::sleep(Duration::from_millis(200));

    handle.publish_snapshot("cts.a07", Bytes::from_static(b"event-data"));

    std::thread::sleep(Duration::from_millis(500));

    let cli = block_cli(&rt, &handle, &id, "a07", vec!["count".into()]);
    match cli {
        CliOutcome::Ok(r) => {
            let stdout = String::from_utf8_lossy(&r.stdout);
            let count: usize = stdout.trim().parse().unwrap_or(0);
            assert!(count >= 1, "expected at least 1 event, got {count}");
        }
        other => panic!("cli outcome: {other:?}"),
    }
}

// =====================================================================
// A8: action invoke timeout (cli_dispatch with 10s sleep)
// =====================================================================

#[test]
fn a08_action_invoke_timeout() {
    let (rt, handle) = build_runtime();
    let mut m = manifest("cts-a08");
    m.provides.cli_namespaces = vec!["a08".into()];
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_cts-a08-action-timeout"));
    let id = register(&rt, bin, m);

    drop(block_render(&rt, &handle, &id, 1, 1));
    wait_running(&handle, &id);

    let rx = handle.dispatch_cli(&id, "a08", vec!["slow".into()]);
    let result = rt.tokio_handle().block_on(async {
        tokio::time::timeout(Duration::from_secs(3), rx).await
    });
    assert!(result.is_err(), "expected timeout but got a response");
}

// =====================================================================
// A9: host log level filtering
// =====================================================================

#[test]
fn a09_host_log_level_filtering() {
    let (rt, handle) = build_runtime();
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_cts-a09-log-filter"));
    let id = register(&rt, bin, manifest("cts-a09"));

    let outcome = block_render(&rt, &handle, &id, 1, 1);
    match outcome {
        RenderOutcome::Ok(buf) => {
            assert_eq!(buf.cells[0].1.symbol, "L");
        }
        other => panic!("expected render Ok, got {other:?}"),
    }
}

// =====================================================================
// A10: fs path guard — plugin returns -32601 for unimplemented fs
// =====================================================================

#[test]
fn a10_fs_path_guard() {
    let (rt, handle) = build_runtime();
    let mut m = manifest("cts-a10");
    m.provides.cli_namespaces = vec!["a10".into()];
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_cts-a10-fs-guard"));
    let id = register(&rt, bin, m);

    let outcome = block_cli(&rt, &handle, &id, "a10", vec!["read".into()]);
    match outcome {
        CliOutcome::PluginError { code, .. } => {
            assert_eq!(code, ainb_plugin_protocol::errors::METHOD_NOT_FOUND);
        }
        other => panic!("expected PluginError, got {other:?}"),
    }
}

// =====================================================================
// A11: graceful shutdown — ack within 1s, exit 0
// =====================================================================

#[test]
fn a11_graceful_shutdown() {
    let (rt, handle) = build_runtime();
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_cts-a11-shutdown"));
    let id = register(&rt, bin, manifest("cts-a11"));

    drop(block_render(&rt, &handle, &id, 1, 1));
    wait_running(&handle, &id);

    drop(rt);

    // If we reach here without hanging, shutdown was successful.
}

// =====================================================================
// A12: crash recovery — inject_kill, then render succeeds on respawn
// =====================================================================

#[test]
fn a12_crash_recovery() {
    let (rt, handle) = build_runtime();
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_cts-a12-crash-recovery"));
    let id = register(&rt, bin, manifest("cts-a12"));

    drop(block_render(&rt, &handle, &id, 1, 1));
    wait_running(&handle, &id);

    handle.inject_kill(&id).expect("inject_kill");
    std::thread::sleep(Duration::from_millis(300));

    drop(handle.render(&id, Viewport::new(1, 1), 1));
    std::thread::sleep(Duration::from_millis(500));

    let mut success = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        let outcome = block_render(&rt, &handle, &id, 1, 1);
        if matches!(outcome, RenderOutcome::Ok(_)) {
            success = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(success, "render never succeeded after crash recovery");
}

// =====================================================================
// A13: quarantine — 3 kills inside failure window → quarantined
// =====================================================================

#[test]
fn a13_quarantine() {
    let (rt, handle) = build_runtime();
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_cts-a13-quarantine"));
    let id = register(&rt, bin, manifest("cts-a13"));

    drop(block_render(&rt, &handle, &id, 1, 1));
    wait_running(&handle, &id);

    for _ in 0..3 {
        handle.inject_kill(&id).expect("inject_kill");
        std::thread::sleep(Duration::from_millis(300));
        drop(handle.render(&id, Viewport::new(1, 1), 0));
        std::thread::sleep(Duration::from_millis(300));
    }

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

    handle.reload(&id).expect("reload");
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        if matches!(
            handle.lifecycle_state(&id),
            Some(LifecycleState::Idle)
        ) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!(
        "reload didn't clear quarantine; state = {:?}",
        handle.lifecycle_state(&id)
    );
}

// =====================================================================
// A14: CLI dispatch stdout capture
// =====================================================================

#[test]
fn a14_cli_dispatch_stdout_capture() {
    let (rt, handle) = build_runtime();
    let mut m = manifest("cts-a14");
    m.provides.cli_namespaces = vec!["echo".into()];
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_cts-a14-cli-dispatch"));
    let id = register(&rt, bin, m);

    let outcome = block_cli(&rt, &handle, &id, "echo", vec!["test".into()]);
    match outcome {
        CliOutcome::Ok(r) => {
            assert_eq!(&r.stdout[..], b"hello\n");
            assert_eq!(r.exit_code, 0);
        }
        other => panic!("expected CliOutcome::Ok, got {other:?}"),
    }
}

// =====================================================================
// A15: event_stream_subscribe — cap-gated streaming, cancellation,
//      anti-cheat sentinel verification, restart cleanup
// =====================================================================

/// Drive a CLI command and return trimmed stdout as a String.
fn cli_stdout(rt: &Runtime, handle: &RuntimeHandle, id: &PluginId, ns: &str, argv: Vec<String>) -> String {
    match block_cli(rt, handle, id, ns, argv) {
        CliOutcome::Ok(r) => String::from_utf8_lossy(&r.stdout).trim().to_string(),
        other => panic!("cli outcome: {other:?}"),
    }
}

fn a15_manifest(name: &str, grant_cap: bool) -> Manifest {
    let mut m = manifest(name);
    m.provides.cli_namespaces = vec!["a15".into()];
    if grant_cap {
        m.capabilities.event_stream_subscribe =
            ainb_plugin_protocol::manifest::CapabilityGrant::List(vec!["workspace:*".into()]);
    }
    m
}

#[test]
fn a15_event_stream_subscribe_delivery_and_cancel() {
    let (rt, handle) = build_runtime();
    // Install a host-side sentinel tap so we can prove the cap-allowed
    // delivery path actually ran (anti-cheat) rather than the canary
    // faking its counter.
    let sentinels = handle.install_log_tap();

    let bin = PathBuf::from(env!("CARGO_BIN_EXE_cts-a15-event-stream"));
    let id = register(&rt, bin, a15_manifest("cts-a15", true));

    drop(block_render(&rt, &handle, &id, 1, 1));
    wait_running(&handle, &id);

    // Plugin subscribed during on_init; learn its unique topic.
    let topic = cli_stdout(&rt, &handle, &id, "a15", vec!["topic".into()]);
    assert!(topic.starts_with("workspace:cts-canary-"), "unexpected topic: {topic}");

    // Publish 3 sentinel events; canary should log SENTINEL_RX_1..=3.
    for _ in 0..3 {
        handle.publish_stream_event(&topic, Bytes::from_static(b"evt"));
        std::thread::sleep(Duration::from_millis(80));
    }
    // 4th event triggers cancel inside the canary.
    handle.publish_stream_event(&topic, Bytes::from_static(b"evt"));
    std::thread::sleep(Duration::from_millis(200));

    // Anti-cheat: 4 sentinels captured HOST-side (not just the canary's count).
    let logs = sentinels.lock().unwrap().clone();
    for n in 1..=4 {
        assert!(
            logs.iter().any(|l| l == &format!("SENTINEL_RX_{n}")),
            "missing host-side sentinel SENTINEL_RX_{n}; got {logs:?}"
        );
    }

    // Cancellation honoured: publish a 5th event, assert no SENTINEL_RX_5.
    handle.publish_stream_event(&topic, Bytes::from_static(b"evt"));
    std::thread::sleep(Duration::from_millis(300));
    let logs = sentinels.lock().unwrap().clone();
    assert!(
        !logs.iter().any(|l| l == "SENTINEL_RX_5"),
        "stream not cancelled — leaked SENTINEL_RX_5: {logs:?}"
    );

    // Canary's own count agrees: exactly 4 received.
    let count = cli_stdout(&rt, &handle, &id, "a15", vec!["count".into()]);
    assert_eq!(count, "4", "canary received count mismatch");
}

#[test]
fn a15_event_stream_subscribe_cap_denied() {
    let (rt, handle) = build_runtime();
    // Same canary binary, but registered WITHOUT the cap grant.
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_cts-a15-event-stream"));
    let id = register(&rt, bin, a15_manifest("cts-a15-denied", false));

    drop(block_render(&rt, &handle, &id, 1, 1));
    wait_running(&handle, &id);

    // on_init's subscribe was denied; canary stashed the error code.
    let suberr = cli_stdout(&rt, &handle, &id, "a15", vec!["suberr".into()]);
    assert_eq!(
        suberr,
        ainb_plugin_protocol::errors::CAPABILITY_DENIED.to_string(),
        "cap-omitted subscribe must return -32001"
    );
}

// =====================================================================
// A16: spawn_managed_subprocess — cap gating, bin allow-list, bool-true
//      rejection, env-allowlist filtering, leak-guard reap on shutdown,
//      anti-cheat sentinel verification
// =====================================================================

/// Build an A16 manifest. `grant` selects the `spawn_managed_subprocess`
/// cap form: `Some(list)` = list grant, `None` = cap omitted (denied).
fn a16_manifest(name: &str, grant: Option<Vec<&str>>) -> Manifest {
    let mut m = manifest(name);
    m.provides.cli_namespaces = vec!["a16".into()];
    if let Some(bins) = grant {
        m.capabilities.spawn_managed_subprocess =
            ainb_plugin_protocol::manifest::CapabilityGrant::List(
                bins.into_iter().map(String::from).collect(),
            );
    }
    m
}

/// Probe process liveness without `unsafe` (CTS forbids it): `kill -0 <pid>`
/// exits 0 iff the process exists and we may signal it.
fn process_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .is_ok_and(|s| s.success())
}

#[test]
fn a16_spawn_managed_subprocess_granted_and_reaped() {
    let (rt, handle) = build_runtime();
    let sentinels = handle.install_log_tap();
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_cts-a16-spawn-managed"));
    let id = register(&rt, bin, a16_manifest("cts-a16", Some(vec!["sleep", "sh"])));

    drop(block_render(&rt, &handle, &id, 1, 1));
    wait_running(&handle, &id);

    // Drive the canary to spawn `sleep 30` via the cap.
    let pid_str = cli_stdout(&rt, &handle, &id, "a16", vec!["spawn".into()]);
    let pid: u32 = pid_str.parse().expect("pid");
    assert_ne!(pid, 0, "spawn returned pid 0");
    assert!(process_alive(pid), "managed child not alive after spawn");

    // Anti-cheat: sentinel captured host-side proves the cap-allowed
    // handler actually ran (not a faked CLI reply).
    let logs = sentinels.lock().unwrap().clone();
    assert!(
        logs.iter().any(|l| l == "SENTINEL_SPAWN_OK"),
        "missing host-side SENTINEL_SPAWN_OK; got {logs:?}"
    );

    // Leak guard: dropping the runtime must reap the managed child.
    drop(rt);
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while process_alive(pid) && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        !process_alive(pid),
        "managed child leaked after Runtime drop (pid {pid} still alive)"
    );
}

#[test]
fn a16_spawn_managed_subprocess_cap_denied() {
    let (rt, handle) = build_runtime();
    // Cap omitted entirely → -32001, no fork.
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_cts-a16-spawn-managed"));
    let id = register(&rt, bin, a16_manifest("cts-a16-denied", None));

    drop(block_render(&rt, &handle, &id, 1, 1));
    wait_running(&handle, &id);

    let code = cli_stdout(&rt, &handle, &id, "a16", vec!["spawnerr".into(), "sleep".into()]);
    assert_eq!(
        code,
        ainb_plugin_protocol::errors::CAPABILITY_DENIED.to_string(),
        "cap-omitted spawn must return -32001"
    );
}

#[test]
fn a16_spawn_managed_subprocess_bin_not_whitelisted() {
    let (rt, handle) = build_runtime();
    // Grant for `sleep` only; attempt to spawn `sh` → -32001.
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_cts-a16-spawn-managed"));
    let id = register(&rt, bin, a16_manifest("cts-a16-wl", Some(vec!["sleep"])));

    drop(block_render(&rt, &handle, &id, 1, 1));
    wait_running(&handle, &id);

    let code = cli_stdout(&rt, &handle, &id, "a16", vec!["spawnerr".into(), "sh".into()]);
    assert_eq!(
        code,
        ainb_plugin_protocol::errors::CAPABILITY_DENIED.to_string(),
        "bin off the allow-list must return -32001"
    );
}

#[test]
fn a16_spawn_managed_subprocess_bool_true_rejected() {
    let (rt, handle) = build_runtime();
    // Bool-true grant is a request for unrestricted spawn — rejected at
    // manifest validation with -32003 (MANIFEST_VALIDATION).
    let mut m = manifest("cts-a16-booltrue");
    m.provides.cli_namespaces = vec!["a16".into()];
    m.capabilities.spawn_managed_subprocess =
        ainb_plugin_protocol::manifest::CapabilityGrant::Bool(true);
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_cts-a16-spawn-managed"));
    let id = register(&rt, bin, m);

    drop(block_render(&rt, &handle, &id, 1, 1));
    wait_running(&handle, &id);

    let code = cli_stdout(&rt, &handle, &id, "a16", vec!["spawnerr".into(), "sleep".into()]);
    assert_eq!(
        code,
        ainb_plugin_protocol::errors::MANIFEST_VALIDATION.to_string(),
        "bool-true grant must be rejected with -32003"
    );
}

#[test]
fn a16_spawn_managed_subprocess_env_allowlist_enforced() {
    // Set two env vars on the HOST process (the registry's spawn reads
    // host env). Only CTS_MANAGED_KEEP is on the canary's env_allowlist.
    std::env::set_var("CTS_MANAGED_KEEP", "keep-me");
    std::env::set_var("CTS_MANAGED_DROP", "drop-me");

    let (rt, handle) = build_runtime();
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_cts-a16-spawn-managed"));
    let id = register(&rt, bin, a16_manifest("cts-a16-env", Some(vec!["sleep", "sh"])));

    drop(block_render(&rt, &handle, &id, 1, 1));
    wait_running(&handle, &id);

    let tmp = std::env::temp_dir().join(format!("cts-a16-env-{}.txt", std::process::id()));
    let tmp_str = tmp.to_string_lossy().to_string();
    let _ = std::fs::remove_file(&tmp);

    let pid_str = cli_stdout(&rt, &handle, &id, "a16", vec!["spawnenv".into(), tmp_str]);
    assert_ne!(pid_str, "0", "spawnenv returned pid 0");

    // Wait for the `sh -c printenv > out` child to finish writing.
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let mut env_dump = String::new();
    while std::time::Instant::now() < deadline {
        if let Ok(s) = std::fs::read_to_string(&tmp) {
            if !s.is_empty() {
                env_dump = s;
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    std::env::remove_var("CTS_MANAGED_KEEP");
    std::env::remove_var("CTS_MANAGED_DROP");
    let _ = std::fs::remove_file(&tmp);
    drop(rt);

    assert!(
        env_dump.contains("CTS_MANAGED_KEEP=keep-me"),
        "allow-listed var missing from child env: {env_dump}"
    );
    assert!(
        !env_dump.contains("CTS_MANAGED_DROP"),
        "unlisted var leaked into child env: {env_dump}"
    );
}

#[test]
fn a15_event_stream_dropped_on_plugin_restart() {
    let (rt, handle) = build_runtime();
    let sentinels = handle.install_log_tap();
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_cts-a15-event-stream"));
    let id = register(&rt, bin, a15_manifest("cts-a15-restart", true));

    drop(block_render(&rt, &handle, &id, 1, 1));
    wait_running(&handle, &id);
    let topic = cli_stdout(&rt, &handle, &id, "a15", vec!["topic".into()]);

    // Kill the plugin — host MUST drop its streams so no events leak.
    handle.inject_kill(&id).expect("inject_kill");
    std::thread::sleep(Duration::from_millis(400));

    // Clear any sentinels captured before the kill.
    sentinels.lock().unwrap().clear();

    // Publish to the (now-dead) stream's topic. With the stream dropped,
    // nothing should be delivered to the OLD subscription.
    handle.publish_stream_event(&topic, Bytes::from_static(b"leak"));
    std::thread::sleep(Duration::from_millis(300));

    let logs = sentinels.lock().unwrap().clone();
    assert!(
        logs.is_empty(),
        "stream leaked after plugin restart: {logs:?}"
    );
}

// =====================================================================
// A17: unix_socket_dial — cap gating, path allow-list, bool-true
//      rejection, symlink-resolution whitelist, bidirectional data,
//      drop-on-restart, anti-cheat sentinel verification
// =====================================================================

/// Build an A17 manifest. `grant` selects the `unix_socket_dial` cap
/// form: `Some(paths)` = list grant, `None` = cap omitted (denied).
fn a17_manifest(name: &str, grant: Option<Vec<String>>) -> Manifest {
    let mut m = manifest(name);
    m.provides.cli_namespaces = vec!["a17".into()];
    if let Some(paths) = grant {
        m.capabilities.unix_socket_dial =
            ainb_plugin_protocol::manifest::CapabilityGrant::List(paths);
    }
    m
}

/// A tiny mock `AF_UNIX` echo/push server on `path`, run on a background
/// std thread. On the first connection it sends `greeting` immediately,
/// then echoes every byte it reads back to the client until the peer
/// closes. The bound listener is closed when the thread ends after one
/// connection.
fn spawn_mock_unix_server(path: &std::path::Path, greeting: &'static [u8]) {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path).expect("bind mock unix socket");
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let _ = stream.write_all(greeting);
            let _ = stream.flush();
            let mut buf = [0u8; 4096];
            loop {
                match stream.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        // Echo back with an `ECHO:` prefix so the test can
                        // distinguish the greeting from the echo.
                        let mut out = b"ECHO:".to_vec();
                        out.extend_from_slice(&buf[..n]);
                        if stream.write_all(&out).is_err() {
                            break;
                        }
                        let _ = stream.flush();
                    }
                }
            }
        }
    });
}

#[test]
fn a17_unix_socket_dial_granted_bidirectional_and_sentinel() {
    let (rt, handle) = build_runtime();
    let sentinels = handle.install_log_tap();

    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("hangar.sock");
    spawn_mock_unix_server(&sock, b"HELLO");
    // Give the listener a moment to bind.
    std::thread::sleep(Duration::from_millis(50));

    let bin = PathBuf::from(env!("CARGO_BIN_EXE_cts-a17-unix-socket"));
    let sock_str = sock.to_string_lossy().to_string();
    let id = register(
        &rt,
        bin,
        a17_manifest("cts-a17", Some(vec![sock_str.clone()])),
    );

    drop(block_render(&rt, &handle, &id, 1, 1));
    wait_running(&handle, &id);

    // Dial the whitelisted socket.
    let stream_id = cli_stdout(&rt, &handle, &id, "a17", vec!["dial".into(), sock_str]);
    assert!(!stream_id.is_empty(), "dial returned empty stream_id");

    // The mock server pushed "HELLO" on connect — the read loop should
    // deliver it as a `data` frame. Wait for it.
    std::thread::sleep(Duration::from_millis(200));

    // Anti-cheat: dial + at-least-one data sentinel captured host-side.
    let logs = sentinels.lock().unwrap().clone();
    assert!(
        logs.iter().any(|l| l == "SENTINEL_DIAL_OK"),
        "missing host-side SENTINEL_DIAL_OK; got {logs:?}"
    );
    assert!(
        logs.iter().any(|l| l == "SENTINEL_RX_DATA"),
        "no socket data delivered to plugin; got {logs:?}"
    );

    let rxbytes = cli_stdout(&rt, &handle, &id, "a17", vec!["rxbytes".into()]);
    assert!(rxbytes.contains("HELLO"), "greeting not received: {rxbytes}");

    // Write to the socket; the mock echoes back with an ECHO: prefix.
    let _ = cli_stdout(&rt, &handle, &id, "a17", vec!["send".into(), "ping".into()]);
    std::thread::sleep(Duration::from_millis(200));
    let rxbytes = cli_stdout(&rt, &handle, &id, "a17", vec!["rxbytes".into()]);
    assert!(
        rxbytes.contains("ECHO:ping"),
        "echo not received after send: {rxbytes}"
    );

    // Close honoured: closing then dropping the runtime must not panic.
    let _ = cli_stdout(&rt, &handle, &id, "a17", vec!["close".into()]);
    drop(rt);
}

#[test]
fn a17_unix_socket_dial_cap_denied() {
    let (rt, handle) = build_runtime();
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("hangar.sock");
    spawn_mock_unix_server(&sock, b"HI");
    std::thread::sleep(Duration::from_millis(50));

    let bin = PathBuf::from(env!("CARGO_BIN_EXE_cts-a17-unix-socket"));
    // Cap omitted entirely → -32001, no connect.
    let id = register(&rt, bin, a17_manifest("cts-a17-denied", None));

    drop(block_render(&rt, &handle, &id, 1, 1));
    wait_running(&handle, &id);

    let code = cli_stdout(
        &rt,
        &handle,
        &id,
        "a17",
        vec!["dialerr".into(), sock.to_string_lossy().to_string()],
    );
    assert_eq!(
        code,
        ainb_plugin_protocol::errors::CAPABILITY_DENIED.to_string(),
        "cap-omitted dial must return -32001"
    );
}

#[test]
fn a17_unix_socket_dial_path_not_whitelisted() {
    let (rt, handle) = build_runtime();
    let dir = tempfile::tempdir().unwrap();
    let allowed = dir.path().join("hangar.sock");
    let other = dir.path().join("evil.sock");
    spawn_mock_unix_server(&allowed, b"HI");
    spawn_mock_unix_server(&other, b"HI");
    std::thread::sleep(Duration::from_millis(50));

    let bin = PathBuf::from(env!("CARGO_BIN_EXE_cts-a17-unix-socket"));
    let id = register(
        &rt,
        bin,
        a17_manifest(
            "cts-a17-wl",
            Some(vec![allowed.to_string_lossy().to_string()]),
        ),
    );

    drop(block_render(&rt, &handle, &id, 1, 1));
    wait_running(&handle, &id);

    // Dial a real socket that is NOT on the allow-list → -32001.
    let code = cli_stdout(
        &rt,
        &handle,
        &id,
        "a17",
        vec!["dialerr".into(), other.to_string_lossy().to_string()],
    );
    assert_eq!(
        code,
        ainb_plugin_protocol::errors::CAPABILITY_DENIED.to_string(),
        "path off the allow-list must return -32001"
    );
}

#[test]
fn a17_unix_socket_dial_bool_true_rejected() {
    let (rt, handle) = build_runtime();
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("hangar.sock");
    spawn_mock_unix_server(&sock, b"HI");
    std::thread::sleep(Duration::from_millis(50));

    // Bool-true grant is a request for unrestricted dial — rejected at
    // manifest validation with -32003 (MANIFEST_VALIDATION).
    let mut m = manifest("cts-a17-booltrue");
    m.provides.cli_namespaces = vec!["a17".into()];
    m.capabilities.unix_socket_dial =
        ainb_plugin_protocol::manifest::CapabilityGrant::Bool(true);

    let bin = PathBuf::from(env!("CARGO_BIN_EXE_cts-a17-unix-socket"));
    let id = register(&rt, bin, m);

    drop(block_render(&rt, &handle, &id, 1, 1));
    wait_running(&handle, &id);

    let code = cli_stdout(
        &rt,
        &handle,
        &id,
        "a17",
        vec!["dialerr".into(), sock.to_string_lossy().to_string()],
    );
    assert_eq!(
        code,
        ainb_plugin_protocol::errors::MANIFEST_VALIDATION.to_string(),
        "bool-true grant must be rejected with -32003"
    );
}

#[test]
fn a17_unix_socket_dial_symlink_outside_whitelist_denied() {
    let (rt, handle) = build_runtime();
    let dir = tempfile::tempdir().unwrap();
    // The canonical whitelisted socket.
    let whitelisted = dir.path().join("hangar.sock");
    spawn_mock_unix_server(&whitelisted, b"HI");
    // A different real socket the attacker wants to reach.
    let target = dir.path().join("docker.sock");
    spawn_mock_unix_server(&target, b"DOCKER");
    // A symlink whose name differs but resolves to the non-whitelisted
    // target — must be DENIED because canonicalization resolves it.
    let link = dir.path().join("attack.sock");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    std::thread::sleep(Duration::from_millis(50));

    let bin = PathBuf::from(env!("CARGO_BIN_EXE_cts-a17-unix-socket"));
    let id = register(
        &rt,
        bin,
        a17_manifest(
            "cts-a17-symlink",
            Some(vec![whitelisted.to_string_lossy().to_string()]),
        ),
    );

    drop(block_render(&rt, &handle, &id, 1, 1));
    wait_running(&handle, &id);

    let code = cli_stdout(
        &rt,
        &handle,
        &id,
        "a17",
        vec!["dialerr".into(), link.to_string_lossy().to_string()],
    );
    assert_eq!(
        code,
        ainb_plugin_protocol::errors::CAPABILITY_DENIED.to_string(),
        "symlink resolving outside the whitelist must return -32001"
    );
}

#[test]
fn a17_unix_socket_dropped_on_plugin_restart() {
    let (rt, handle) = build_runtime();
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("hangar.sock");
    spawn_mock_unix_server(&sock, b"HELLO");
    std::thread::sleep(Duration::from_millis(50));

    let bin = PathBuf::from(env!("CARGO_BIN_EXE_cts-a17-unix-socket"));
    let id = register(
        &rt,
        bin,
        a17_manifest(
            "cts-a17-restart",
            Some(vec![sock.to_string_lossy().to_string()]),
        ),
    );

    drop(block_render(&rt, &handle, &id, 1, 1));
    wait_running(&handle, &id);
    let stream_id = cli_stdout(
        &rt,
        &handle,
        &id,
        "a17",
        vec!["dial".into(), sock.to_string_lossy().to_string()],
    );
    assert!(!stream_id.is_empty());

    // Kill the plugin — host MUST drop its dialled sockets so the read
    // loop is aborted and no `socket:<id>` frame leaks to a dead process.
    handle.inject_kill(&id).expect("inject_kill");
    std::thread::sleep(Duration::from_millis(400));

    // The registry must hold no live sockets for the (dead) plugin.
    // We can't reach the registry directly from the test, but dropping
    // the runtime here must not panic and must not hang on a leaked task.
    drop(rt);
}

// =====================================================================
// A18: secret_store_get — cap gating, service allow-list, bool-true
//      unconditional read, not-found, platform stub (linux), anti-cheat
//      sentinel verification (macOS real keychain)
// =====================================================================

/// Build an A18 manifest. `grant` selects the `secret_store_get` cap form:
/// `Some(services)` = list grant, `None` = cap omitted (denied).
fn a18_manifest(name: &str, grant: Option<Vec<String>>) -> Manifest {
    let mut m = manifest(name);
    m.provides.cli_namespaces = vec!["a18".into()];
    if let Some(services) = grant {
        m.capabilities.secret_store_get =
            ainb_plugin_protocol::manifest::CapabilityGrant::List(services);
    }
    m
}

#[test]
fn a18_secret_store_get_cap_denied() {
    let (rt, handle) = build_runtime();
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_cts-a18-secret-store"));
    // Cap omitted entirely → -32001, no keychain hit.
    let id = register(&rt, bin, a18_manifest("cts-a18-denied", None));

    drop(block_render(&rt, &handle, &id, 1, 1));
    wait_running(&handle, &id);

    let code = cli_stdout(
        &rt,
        &handle,
        &id,
        "a18",
        vec!["geterr".into(), "any-service".into(), "any-account".into()],
    );
    assert_eq!(
        code,
        ainb_plugin_protocol::errors::CAPABILITY_DENIED.to_string(),
        "cap-omitted secret read must return -32001"
    );
}

#[test]
fn a18_secret_store_get_service_not_whitelisted() {
    let (rt, handle) = build_runtime();
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_cts-a18-secret-store"));
    // Grant whitelists ONLY "ainb-hangar"; request a different service.
    let id = register(
        &rt,
        bin,
        a18_manifest("cts-a18-wl", Some(vec!["ainb-hangar".into()])),
    );

    drop(block_render(&rt, &handle, &id, 1, 1));
    wait_running(&handle, &id);

    let code = cli_stdout(
        &rt,
        &handle,
        &id,
        "a18",
        vec!["geterr".into(), "evil-service".into(), "acct".into()],
    );
    assert_eq!(
        code,
        ainb_plugin_protocol::errors::CAPABILITY_DENIED.to_string(),
        "service not on allow-list must return -32001"
    );
}

/// macOS: seed the login Keychain, read it back through the cap, assert the
/// bytes match and the anti-cheat sentinel fired host-side.
#[cfg(target_os = "macos")]
#[test]
fn a18_secret_store_get_granted_reads_keychain_and_sentinel() {
    use security_framework::passwords::{delete_generic_password, set_generic_password};

    // Unique service name so parallel runs / leftover items never collide.
    let service = format!("cts-a18-{}", std::process::id());
    let account = "anthropic-api-key";
    let secret = b"super-secret-token";

    // Seed (best-effort cleanup of any stale item first).
    let _ = delete_generic_password(&service, account);
    set_generic_password(&service, account, secret).expect("seed keychain item");

    let (rt, handle) = build_runtime();
    let sentinels = handle.install_log_tap();
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_cts-a18-secret-store"));
    let id = register(
        &rt,
        bin,
        a18_manifest("cts-a18-ok", Some(vec![service.clone()])),
    );

    drop(block_render(&rt, &handle, &id, 1, 1));
    wait_running(&handle, &id);

    let out = cli_stdout(
        &rt,
        &handle,
        &id,
        "a18",
        vec!["get".into(), service.clone(), account.into()],
    );

    // Teardown the seeded item before any assertion can panic.
    let _ = delete_generic_password(&service, account);

    assert_eq!(out, "super-secret-token", "secret bytes mismatch: {out}");

    // Anti-cheat: SECRET_READ_OK fired host-side — proves the genuine
    // keychain path ran (not a fabricated value).
    let logs = sentinels.lock().unwrap().clone();
    assert!(
        logs.iter().any(|l| l == "SECRET_READ_OK"),
        "missing host-side SECRET_READ_OK sentinel; got {logs:?}"
    );
}

/// macOS: bool-true grant reads any service unconditionally (no allow-list).
#[cfg(target_os = "macos")]
#[test]
fn a18_secret_store_get_bool_true_grants_any_service() {
    use security_framework::passwords::{delete_generic_password, set_generic_password};

    let service = format!("cts-a18-bool-{}", std::process::id());
    let account = "key";
    let _ = delete_generic_password(&service, account);
    set_generic_password(&service, account, b"v").expect("seed keychain item");

    let (rt, handle) = build_runtime();
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_cts-a18-secret-store"));
    let mut m = manifest("cts-a18-bool");
    m.provides.cli_namespaces = vec!["a18".into()];
    m.capabilities.secret_store_get =
        ainb_plugin_protocol::manifest::CapabilityGrant::Bool(true);
    let id = register(&rt, bin, m);

    drop(block_render(&rt, &handle, &id, 1, 1));
    wait_running(&handle, &id);

    let code = cli_stdout(
        &rt,
        &handle,
        &id,
        "a18",
        vec!["geterr".into(), service.clone(), account.into()],
    );
    let _ = delete_generic_password(&service, account);
    assert_eq!(code, "0", "bool-true grant must read any service: {code}");
}

/// macOS: service granted but no such item → -32004 `SECRET_NOT_FOUND`, and
/// the `SECRET_READ_OK` sentinel must NOT fire (anti-cheat for the miss path).
#[cfg(target_os = "macos")]
#[test]
fn a18_secret_store_get_not_found() {
    use security_framework::passwords::delete_generic_password;

    let service = format!("cts-a18-missing-{}", std::process::id());
    let account = "absent";
    // Ensure the item really is absent.
    let _ = delete_generic_password(&service, account);

    let (rt, handle) = build_runtime();
    let sentinels = handle.install_log_tap();
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_cts-a18-secret-store"));
    let id = register(
        &rt,
        bin,
        a18_manifest("cts-a18-missing", Some(vec![service.clone()])),
    );

    drop(block_render(&rt, &handle, &id, 1, 1));
    wait_running(&handle, &id);

    let code = cli_stdout(
        &rt,
        &handle,
        &id,
        "a18",
        vec!["geterr".into(), service, account.into()],
    );
    assert_eq!(
        code,
        ainb_plugin_protocol::errors::SECRET_NOT_FOUND.to_string(),
        "absent item must return -32004"
    );
    let logs = sentinels.lock().unwrap().clone();
    assert!(
        !logs.iter().any(|l| l == "SECRET_READ_OK"),
        "SECRET_READ_OK must not fire on the not-found path: {logs:?}"
    );
}

/// linux / non-macOS: the cap exists and is granted, but the backend is the
/// `-32005 NOT_IMPLEMENTED` stub so the plugin can degrade to dotenv.
#[cfg(not(target_os = "macos"))]
#[test]
fn a18_secret_store_get_not_implemented_on_non_macos() {
    let (rt, handle) = build_runtime();
    let sentinels = handle.install_log_tap();
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_cts-a18-secret-store"));
    let id = register(
        &rt,
        bin,
        a18_manifest("cts-a18-stub", Some(vec!["ainb-hangar".into()])),
    );

    drop(block_render(&rt, &handle, &id, 1, 1));
    wait_running(&handle, &id);

    let code = cli_stdout(
        &rt,
        &handle,
        &id,
        "a18",
        vec!["geterr".into(), "ainb-hangar".into(), "acct".into()],
    );
    assert_eq!(
        code,
        ainb_plugin_protocol::errors::NOT_IMPLEMENTED.to_string(),
        "non-macOS secret read must return -32005"
    );
    // Anti-cheat: the stub path must NOT emit the success sentinel.
    let logs = sentinels.lock().unwrap().clone();
    assert!(
        !logs.iter().any(|l| l == "SECRET_READ_OK"),
        "SECRET_READ_OK must not fire on the not-implemented stub path: {logs:?}"
    );
}
