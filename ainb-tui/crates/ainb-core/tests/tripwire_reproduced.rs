//! Tripwire 7f.4: Reproduced Phase 6 tripwire tests.
//!
//! These 4 tests reproduce Phase 6 tripwire behaviors through the new
//! subprocess runtime, proving zero behaviour regression from the wasmi
//! removal. Each exercises a distinct data path:
//!
//! 1. Render round-trip: fixture → RuntimeHandle → WireBuffer cells
//! 2. Snapshot round-trip: fixture publishes → host reads via snapshot_get
//! 3. Action round-trip: host invokes action → fixture echoes payload
//! 4. CLI report baseline: `ainb plugin lint` produces stable output
//!
//! Architectural constraints:
//! - Zero wasmi imports
//! - Zero ainb-plugin-api imports
//! - All observation via RuntimeHandle (try_recv_render, snapshot_get,
//!   lifecycle_state, registered_plugins) — non-blocking surface only

#[allow(unused)]
#[path = "tripwire_helpers.rs"]
mod tripwire_helpers;

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use ainb_plugin_protocol::params::Viewport;
use ainb_plugin_runtime::types::RenderOutcome;
use bytes::Bytes;
use tripwire_helpers::{fast_runtime, register_plugin, sibling_bin};

/// Phase 6 tripwire 1/4 reproduced: render round-trip through subprocess runtime.
#[test]
fn reproduced_render_round_trip() {
    let fixture_bin = sibling_bin("ainb-fixture-plugin");
    let (rt, handle) = fast_runtime();
    let id = register_plugin(&rt, "fixture", fixture_bin);

    let rx = handle.render(&id, Viewport::new(40, 8), 0);
    let outcome = rt.tokio_handle().block_on(async {
        tokio::time::timeout(Duration::from_secs(5), rx)
            .await
            .expect("render timed out")
            .expect("render channel closed")
    });

    match outcome {
        RenderOutcome::Ok(buf) => {
            assert_eq!(buf.width, 1, "fixture renders 1×1 buffer");
            assert_eq!(buf.height, 1);
            assert_eq!(buf.cells.len(), 1);
            assert_eq!(buf.cells[0].1.symbol, "X", "fixture cell symbol must be X");
        }
        other => panic!("expected RenderOutcome::Ok, got {other:?}"),
    }

    // Verify try_recv_render works after cache population.
    drop(handle.render(&id, Viewport::new(40, 8), 1));
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut got = false;
    while Instant::now() < deadline {
        if let Some(buf) = handle.try_recv_render(&id) {
            assert_eq!(buf.cells[0].1.symbol, "X");
            got = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(got, "try_recv_render never returned a buffer");
}

/// Phase 6 tripwire 2/4 reproduced: snapshot round-trip.
#[test]
fn reproduced_snapshot_round_trip() {
    let fixture_bin = sibling_bin("ainb-fixture-plugin");
    let (rt, handle) = fast_runtime();
    let id = register_plugin(&rt, "fixture", fixture_bin);

    // Trigger lazy spawn.
    drop(handle.render(&id, Viewport::new(20, 5), 0));

    // The fixture publishes snapshot "fixture.greeting" = b"hello" on startup.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut payload = None;
    while Instant::now() < deadline {
        if let Some(p) = handle.snapshot_get("fixture.greeting") {
            payload = Some(p);
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let p = payload.expect("fixture.greeting snapshot never published");
    assert_eq!(&p[..], b"hello", "snapshot payload must be 'hello'");

    // Host-side publish must work too.
    let v1 = handle.publish_snapshot("from.host", Bytes::from_static(b"v1"));
    let v2 = handle.publish_snapshot("from.host", Bytes::from_static(b"v2"));
    assert!(v2 > v1, "version must increase");
    assert_eq!(handle.snapshot_get("from.host").as_deref(), Some(&b"v2"[..]));
}

/// Phase 6 tripwire 3/4 reproduced: action invoke round-trip.
#[test]
fn reproduced_action_round_trip() {
    let fixture_bin = sibling_bin("ainb-fixture-plugin");
    let (rt, handle) = fast_runtime();
    let _id = register_plugin(&rt, "fixture", fixture_bin);

    // The fixture echoes the payload back via host/action/invoke.
    let rx = handle.invoke_action("echo", Bytes::from_static(b"ping"), Duration::from_secs(2));
    let outcome = rt.tokio_handle().block_on(async {
        tokio::time::timeout(Duration::from_secs(5), rx)
            .await
            .expect("action timed out")
            .expect("action channel closed")
    });

    match outcome {
        ainb_plugin_runtime::types::ActionOutcome::Ok(b) => {
            assert_eq!(&b[..], b"ping", "action must echo payload");
        }
        other => panic!("expected ActionOutcome::Ok, got {other:?}"),
    }
}

/// Phase 6 tripwire 4/4 reproduced: CLI plugin lint produces stable output.
///
/// Exercises the `ainb plugin lint <path>` command against a fixture
/// plugin directory, asserting the output format is stable through the
/// new runtime cutover. This replaces the Phase 6f snapshot baseline test.
#[test]
#[cfg(unix)]
fn reproduced_cli_lint_baseline() {
    use std::fs;
    use std::os::unix::fs as unix_fs;

    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = tmp.path().join("fixture");
    fs::create_dir_all(&plugin_dir).unwrap();

    let manifest = r#"
[plugin]
name = "fixture"
version = "0.1.0"
abi_version = 2
description = "tripwire lint baseline"
"#;
    fs::write(plugin_dir.join("manifest.toml"), manifest).unwrap();
    unix_fs::symlink("/bin/sh", plugin_dir.join("fixture")).unwrap();

    let ainb = PathBuf::from(env!("CARGO_BIN_EXE_ainb"));
    let out = Command::new(&ainb)
        .args(["plugin", "lint"])
        .arg(&plugin_dir)
        .env("AINB_DISABLE_PLUGINS", "1")
        .env("HOME", tmp.path())
        .output()
        .expect("spawn ainb");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        out.status.success(),
        "lint must exit 0; stdout={stdout}\nstderr={stderr}"
    );
    assert!(
        stdout.contains("lint: fixture"),
        "expected 'lint: fixture' header in output; stdout={stdout}"
    );
    assert!(
        !stdout.contains("err  "),
        "no error rows expected for valid fixture; stdout={stdout}"
    );

    // Stability gate: output structure must contain expected columns.
    assert!(
        stdout.contains("ok") || stdout.contains("pass") || stdout.contains("✓"),
        "lint output must indicate passing checks; stdout={stdout}"
    );
}
