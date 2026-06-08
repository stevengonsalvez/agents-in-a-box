//! Host-side conformance tests: 14 axes for ABI v2.

use std::path::PathBuf;
use std::time::Duration;

use ainb_plugin_protocol::manifest::{
    Capabilities, CapabilityGrant, Lifecycle, Manifest, PluginMeta, Provides, SpawnMode, Subscribes,
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
        config: Vec::new(),
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

fn block_render(
    rt: &Runtime,
    handle: &RuntimeHandle,
    id: &PluginId,
    w: u16,
    h: u16,
) -> RenderOutcome {
    let rx = handle.render(id, Viewport::new(w, h), 0);
    rt.tokio_handle().block_on(async {
        tokio::time::timeout(Duration::from_secs(5), rx)
            .await
            .expect("render timed out")
            .expect("render channel closed")
    })
}

fn block_cli(
    rt: &Runtime,
    handle: &RuntimeHandle,
    id: &PluginId,
    ns: &str,
    argv: Vec<String>,
) -> CliOutcome {
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
    let result = rt
        .tokio_handle()
        .block_on(async { tokio::time::timeout(Duration::from_secs(3), rx).await });
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
        if matches!(handle.lifecycle_state(&id), Some(LifecycleState::Idle)) {
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
// read_paths + [config] axis — host/fs read enforcement + config inject
// =====================================================================
//
// The runtime's `host/fs/read_file` guard must allow a read whose target is
// under a granted `read_paths` prefix and DENY one that is not (`-32001`).
// A separate test asserts the host injects the resolved `[plugins.<name>]`
// config table into the plugin at `plugin/init`.
//
// DISK SAFETY: every path here lives under `CARGO_TARGET_TMPDIR`
// (`target/tmp/...`, repo-local). Nothing touches the real home dir.

/// Per-run temp dir under `target/tmp/<pkg>` (repo-local; never `~`).
/// `CARGO_TARGET_TMPDIR` is set by cargo for integration-test binaries.
fn run_tmp(tag: &str) -> PathBuf {
    let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "rpc-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&base).expect("create temp dir");
    base
}

/// Register the read_paths/[config] canary with a custom `read_paths`
/// allow-list, a `[config]` schema, and a stamped resolved config table.
fn register_read_paths_canary(
    rt: &Runtime,
    read_paths: Vec<String>,
    config: serde_json::Value,
) -> PluginId {
    let mut m = manifest("cts-read-paths-config");
    m.capabilities = Capabilities {
        read_paths: CapabilityGrant::List(read_paths),
        ..Capabilities::default()
    };
    m.provides.cli_namespaces = vec!["rpc".into()];

    let bin = PathBuf::from(env!("CARGO_BIN_EXE_cts-read-paths-config"));
    let plugin =
        RegisteredPlugin::new(m, bin, PathBuf::from("/dev/null/manifest.toml")).with_config(config);
    let id = plugin.id.clone();
    rt.register(plugin);
    id
}

#[test]
fn axis_read_paths_grant_honored() {
    let (rt, handle) = build_runtime();

    // Allowed envelope: a per-run temp dir with one file inside it.
    let allowed = run_tmp("allowed");
    let in_envelope = allowed.join("note.md");
    std::fs::write(&in_envelope, b"hello-in-envelope").expect("write in-envelope file");

    let id = register_read_paths_canary(
        &rt,
        vec![allowed.to_string_lossy().into_owned()],
        serde_json::Value::Null,
    );

    let outcome = block_cli(
        &rt,
        &handle,
        &id,
        "rpc",
        vec!["read".into(), in_envelope.to_string_lossy().into_owned()],
    );
    match outcome {
        CliOutcome::Ok(r) => {
            let stdout = String::from_utf8_lossy(&r.stdout);
            assert_eq!(
                stdout.trim(),
                "OK:17",
                "in-envelope read should succeed (17 bytes), got {stdout:?}"
            );
        }
        other => panic!("expected CliOutcome::Ok, got {other:?}"),
    }
}

#[test]
fn axis_read_paths_denied_out_of_envelope() {
    let (rt, handle) = build_runtime();

    // Granted envelope.
    let allowed = run_tmp("allowed");
    std::fs::write(allowed.join("note.md"), b"in").expect("write in-envelope file");

    // A sibling dir NOT under the grant — must be denied.
    let denied = run_tmp("denied");
    let out_of_envelope = denied.join("secret.md");
    std::fs::write(&out_of_envelope, b"secret").expect("write out-of-envelope file");

    let id = register_read_paths_canary(
        &rt,
        vec![allowed.to_string_lossy().into_owned()],
        serde_json::Value::Null,
    );

    let outcome = block_cli(
        &rt,
        &handle,
        &id,
        "rpc",
        vec![
            "read".into(),
            out_of_envelope.to_string_lossy().into_owned(),
        ],
    );
    match outcome {
        CliOutcome::Ok(r) => {
            let stdout = String::from_utf8_lossy(&r.stdout);
            assert_eq!(
                stdout.trim(),
                format!("DENIED:{}", ainb_plugin_protocol::errors::CAPABILITY_DENIED),
                "out-of-envelope read must be denied with -32001, got {stdout:?}"
            );
        }
        other => panic!("expected CliOutcome::Ok, got {other:?}"),
    }
}

/// A non-existent in-envelope read under a *symlinked* grant must NOT be denied
/// as out-of-envelope. On macOS (and many Linux setups) the grant's resolved
/// path differs from its literal path because an ancestor is a symlink
/// (`/tmp -> /private/tmp`, a symlinked `$HOME`, etc.). The guard canonicalizes
/// the granted prefix (it exists) but, for a target that does *not* yet exist,
/// must resolve it against the same on-disk anchor — otherwise a legitimate
/// in-envelope read of a not-yet-existing file (write-then-read-back, probe for
/// an optional file) gets a spurious `-32001`.
///
/// This axis stages a symlinked grant entirely under `CARGO_TARGET_TMPDIR`
/// (repo-local): `grant -> real`, grants `read_paths = [grant]`, and reads a
/// GHOST (non-existent) file under `grant`. The host must NOT return
/// `CAPABILITY_DENIED`; it surfaces the real `ENOENT` read error
/// (`INVALID_PARAMS`) instead.
#[test]
fn axis_read_paths_nonexistent_in_envelope_under_symlink_grant() {
    let (rt, handle) = build_runtime();

    // Stage `grant -> real` under the repo-local temp dir so an ancestor of the
    // granted prefix is a symlink (mirrors macOS `/tmp -> /private/tmp`).
    let base = run_tmp("symlink-grant");
    let real = base.join("real");
    std::fs::create_dir_all(&real).expect("create real grant dir");
    let grant = base.join("grant");
    std::os::unix::fs::symlink(&real, &grant).expect("symlink grant -> real");

    // The target is a not-yet-existing file *inside* the granted (symlinked)
    // prefix — i.e. genuinely in-envelope, just not on disk yet.
    let ghost = grant.join("ghost.md");
    assert!(!ghost.exists(), "ghost must not exist for this axis");

    let id = register_read_paths_canary(
        &rt,
        vec![grant.to_string_lossy().into_owned()],
        serde_json::Value::Null,
    );

    let outcome = block_cli(
        &rt,
        &handle,
        &id,
        "rpc",
        vec!["read".into(), ghost.to_string_lossy().into_owned()],
    );
    match outcome {
        CliOutcome::Ok(r) => {
            let stdout = String::from_utf8_lossy(&r.stdout);
            let trimmed = stdout.trim();
            // Must NOT be denied as out-of-envelope: the read passed the guard
            // and failed at the filesystem with a real ENOENT.
            assert_ne!(
                trimmed,
                format!("DENIED:{}", ainb_plugin_protocol::errors::CAPABILITY_DENIED),
                "in-envelope NON-existent read under a symlinked grant was wrongly \
                 denied -32001 (symlink-prefix asymmetry over-denial), got {stdout:?}"
            );
            assert_eq!(
                trimmed,
                format!("DENIED:{}", ainb_plugin_protocol::errors::INVALID_PARAMS),
                "expected the real ENOENT read error (-32602), got {stdout:?}"
            );
        }
        other => panic!("expected CliOutcome::Ok, got {other:?}"),
    }
}

// =====================================================================
// plugin/handle_mouse axis — host forwards a mouse event to the plugin
// =====================================================================
//
// `RuntimeHandle::send_mouse` must deliver the event over the priority
// mouse channel as a `plugin/handle_mouse` notification, preserving the
// wire shape (kind + button) and the (already viewport-relative)
// coordinates. The canary records the last event and echoes it via cli.

#[test]
fn axis_handle_mouse_forwarded_to_plugin() {
    use ainb_plugin_runtime::{MouseButton, MouseEvent, MouseKind};

    let (rt, handle) = build_runtime();
    let mut m = manifest("cts-mouse-forward");
    m.provides.cli_namespaces = vec!["mouse".into()];
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_cts-mouse-forward"));
    let id = register(&rt, bin, m);

    // Spawn + reach Running before forwarding (a mouse event arriving
    // before the child exists is dropped by design).
    drop(block_render(&rt, &handle, &id, 1, 1));
    wait_running(&handle, &id);

    // No event yet → canary reports `none`.
    match block_cli(&rt, &handle, &id, "mouse", vec!["last".into()]) {
        CliOutcome::Ok(r) => {
            assert_eq!(String::from_utf8_lossy(&r.stdout).trim(), "none");
        }
        other => panic!("expected CliOutcome::Ok, got {other:?}"),
    }

    // Forward a left-button-down at viewport (7, 2) with no modifiers.
    let sent = handle.send_mouse(
        &id,
        "cts-screen",
        MouseEvent {
            kind: MouseKind::Down {
                button: MouseButton::Left,
            },
            col: 7,
            row: 2,
            mods: 0,
        },
    );
    assert!(sent, "send_mouse should enqueue for a running plugin");

    // Poll the read-back until the notification has been observed.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut last = String::new();
    while std::time::Instant::now() < deadline {
        if let CliOutcome::Ok(r) = block_cli(&rt, &handle, &id, "mouse", vec!["last".into()]) {
            last = String::from_utf8_lossy(&r.stdout).trim().to_string();
            if last != "none" {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        last, "down_left:7,2:0",
        "canary must report the forwarded mouse event verbatim"
    );
}

// =====================================================================
// RenderResult.redraw axis — self-animation hint re-marks dirty
// =====================================================================
//
// When a render reply sets `redraw = true`, the runtime must re-mark the
// plugin's render-dirty flag so the host's render loop kicks another
// `plugin/render` without further input. Once the plugin returns
// `redraw = false`, the flag must stay clear. The canary animates for its
// first two renders, then settles.

#[test]
fn axis_render_redraw_hint_remarks_dirty() {
    let (rt, handle) = build_runtime();
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_cts-redraw-hint"));
    let id = register(&rt, bin, manifest("cts-redraw-hint"));

    // Clear the registration dirty-seed so we observe only redraw-driven marks.
    let _ = handle.take_render_dirty(&id);

    // Render #0 → canary wants_redraw == true → runtime re-marks dirty.
    match block_render(&rt, &handle, &id, 1, 1) {
        RenderOutcome::Ok(_) => {}
        other => panic!("render 0 failed: {other:?}"),
    }
    assert!(
        handle.take_render_dirty(&id),
        "redraw=true must re-mark the plugin dirty for another frame"
    );

    // Render #1 → still animating (count 2 < 3) → re-marks dirty again.
    match block_render(&rt, &handle, &id, 1, 1) {
        RenderOutcome::Ok(_) => {}
        other => panic!("render 1 failed: {other:?}"),
    }
    assert!(
        handle.take_render_dirty(&id),
        "redraw=true on frame 1 must re-mark dirty"
    );

    // Render #2 → count now 3, wants_redraw == false → no re-mark.
    match block_render(&rt, &handle, &id, 1, 1) {
        RenderOutcome::Ok(_) => {}
        other => panic!("render 2 failed: {other:?}"),
    }
    assert!(
        !handle.take_render_dirty(&id),
        "redraw=false must NOT re-mark dirty (animation settled)"
    );
}

#[test]
fn axis_config_injected_at_init() {
    let (rt, handle) = build_runtime();

    let allowed = run_tmp("allowed");
    let injected = serde_json::json!({
        "learnings_dir": "/tmp/learnings",
        "qmd_collection": "learnings",
    });

    let id = register_read_paths_canary(
        &rt,
        vec![allowed.to_string_lossy().into_owned()],
        injected.clone(),
    );

    let outcome = block_cli(&rt, &handle, &id, "rpc", vec!["config".into()]);
    match outcome {
        CliOutcome::Ok(r) => {
            let stdout = String::from_utf8_lossy(&r.stdout);
            let reported: serde_json::Value =
                serde_json::from_str(stdout.trim()).expect("canary echoed valid JSON config");
            assert_eq!(
                reported, injected,
                "canary must report the config the host injected"
            );
        }
        other => panic!("expected CliOutcome::Ok, got {other:?}"),
    }
}
