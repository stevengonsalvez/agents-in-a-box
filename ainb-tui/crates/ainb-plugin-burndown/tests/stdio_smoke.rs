//! End-to-end smoke test for the burndown subprocess plugin.
//!
//! Spawns the `ainb-plugin-burndown` binary the same way the runtime
//! would, drives it with Content-Length-framed JSON-RPC requests over
//! stdin/stdout, and asserts:
//!
//! 1. `plugin/init` returns `name=burndown, version=2.0.0` (matches the
//!    `[plugin]` section of `plugin.toml`) — proves the SDK's
//!    manifest-echo path is wired.
//! 2. `plugin/render` with a 40×4 viewport returns a `WireBuffer` of
//!    matching dimensions — proves the dispatcher and the render path
//!    are alive without a `sessions.usage_data` snapshot pre-loaded.
//! 3. `plugin/shutdown` is acknowledged and the process exits within a
//!    short window — proves graceful shutdown.
//!
//! Run with `cargo test -p ainb-plugin-burndown --test stdio_smoke`.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const FRAME_HEADER_PREFIX: &str = "Content-Length: ";

fn binary_path() -> &'static str {
    env!("CARGO_BIN_EXE_ainb-plugin-burndown")
}

fn encode(value: &Value) -> Vec<u8> {
    let body = serde_json::to_vec(value).expect("serialize body");
    let mut out = format!("{FRAME_HEADER_PREFIX}{}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(&body);
    out
}

/// Read one Content-Length-framed JSON body from `r`. Bounded by
/// `deadline` so a hung plugin doesn't wedge the test runner.
fn read_frame<R: Read>(r: &mut R, deadline: Instant) -> Option<Value> {
    // Hand-roll a tiny header reader so we don't pull in tokio/bytes
    // for what's already a 100-line test.
    let mut buf = Vec::new();
    let mut byte = [0_u8; 1];
    let mut content_length: Option<usize> = None;
    loop {
        if Instant::now() > deadline {
            panic!("timed out reading frame header; got so far: {buf:?}");
        }
        match r.read(&mut byte) {
            Ok(0) => return None,
            Ok(_) => buf.push(byte[0]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(e) => panic!("stdout read error: {e}"),
        }
        if buf.ends_with(b"\r\n\r\n") {
            // End of headers.
            let headers = String::from_utf8_lossy(&buf[..buf.len() - 4]);
            for line in headers.split("\r\n") {
                if let Some(rest) = line.strip_prefix(FRAME_HEADER_PREFIX) {
                    content_length = Some(rest.trim().parse().expect("Content-Length numeric"));
                }
            }
            break;
        }
    }
    let len = content_length.expect("frame missing Content-Length header");
    let mut body = vec![0_u8; len];
    let mut read = 0;
    while read < len {
        if Instant::now() > deadline {
            panic!("timed out reading frame body of {len} bytes (got {read})");
        }
        match r.read(&mut body[read..]) {
            Ok(0) => panic!("EOF mid-body at {read}/{len}"),
            Ok(n) => read += n,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(e) => panic!("stdout read error: {e}"),
        }
    }
    Some(serde_json::from_slice(&body).expect("body must decode as JSON"))
}

#[test]
fn init_render_shutdown_round_trip() {
    let mut child = Command::new(binary_path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn burndown plugin");

    let mut stdin = child.stdin.take().expect("stdin handle");
    let mut stdout = child.stdout.take().expect("stdout handle");

    let deadline = Instant::now() + Duration::from_secs(10);

    // 1. plugin/init
    let init = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "plugin/init",
        "params": {
            "manifest_path": "/tmp/burndown/manifest.toml",
            "granted_capabilities": ["read_sessions", "write_plugin_data", "event_bus"],
            "abi_version": 2
        }
    });
    stdin.write_all(&encode(&init)).expect("write init");
    stdin.flush().expect("flush init");

    // Burndown's `on_init` issues three reverse calls in order before
    // returning: (1) `host/snapshot/subscribe` to `sessions.usage_data`
    // (request — expects a reply), (2) a second `host/snapshot/subscribe`
    // to `sessions.scan_progress` (request — Phase 6 progress UX), then
    // (3) `host/snapshot/publish` on the refresh_request topic to kick
    // session-reader (notification — no reply). The SDK awaits
    // `on_init` before replying to `plugin/init`, so all three reverse
    // calls land before the init response.
    let rev1 = read_frame(&mut stdout, deadline).expect("subscribe reverse-call");
    assert_eq!(
        rev1["method"], "host/snapshot/subscribe",
        "reverse #1: {rev1}"
    );
    assert_eq!(
        rev1["params"]["topic"], "sessions.usage_data",
        "reverse #1 topic: {rev1}"
    );
    let rev1_id = rev1["id"].as_i64().expect("reverse request must have id");
    let rev1_reply = json!({
        "jsonrpc": "2.0",
        "id": rev1_id,
        "result": {}
    });
    stdin.write_all(&encode(&rev1_reply)).expect("write subscribe reply");
    stdin.flush().expect("flush subscribe reply");

    let rev2 = read_frame(&mut stdout, deadline).expect("scan-progress subscribe");
    assert_eq!(
        rev2["method"], "host/snapshot/subscribe",
        "reverse #2: {rev2}"
    );
    assert_eq!(
        rev2["params"]["topic"], "sessions.scan_progress",
        "reverse #2 topic: {rev2}"
    );
    let rev2_id = rev2["id"].as_i64().expect("reverse request must have id");
    let rev2_reply = json!({
        "jsonrpc": "2.0",
        "id": rev2_id,
        "result": {}
    });
    stdin
        .write_all(&encode(&rev2_reply))
        .expect("write scan_progress subscribe reply");
    stdin.flush().expect("flush scan_progress subscribe reply");

    let rev3 = read_frame(&mut stdout, deadline).expect("publish notification");
    assert_eq!(
        rev3["method"], "host/snapshot/publish",
        "reverse #3: {rev3}"
    );
    assert_eq!(
        rev3["params"]["topic"], "sessions.refresh_request",
        "reverse #3 topic: {rev3}"
    );
    assert!(
        rev3.get("id").is_none(),
        "refresh_request publish must be a notification (no id): {rev3}"
    );

    // Now the init reply itself.
    let resp = read_frame(&mut stdout, deadline).expect("init response");
    assert_eq!(resp["jsonrpc"], "2.0");
    assert_eq!(resp["id"], 1);
    let result = resp.get("result").expect("init must succeed");
    assert_eq!(result["name"], "burndown", "manifest name echo: {resp}");
    assert_eq!(result["version"], "2.0.0", "manifest version echo: {resp}");

    // 2. plugin/render
    let render = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "plugin/render",
        "params": {
            "viewport": { "width": 40_u16, "height": 4_u16 },
            "generation": 0_u64
        }
    });
    stdin.write_all(&encode(&render)).expect("write render");
    stdin.flush().expect("flush render");

    let resp = read_frame(&mut stdout, deadline).expect("render response");
    assert_eq!(resp["id"], 2);
    let result = resp.get("result").expect("render must succeed");
    let buffer = result.get("buffer").expect("render result has buffer");
    assert_eq!(buffer["width"], 40, "render buffer width: {resp}");
    assert_eq!(buffer["height"], 4, "render buffer height: {resp}");
    assert!(
        buffer["cells"].is_array(),
        "render buffer cells must be array"
    );

    // 3. plugin/shutdown — notification, no response expected.
    let shutdown = json!({
        "jsonrpc": "2.0",
        "method": "plugin/shutdown",
        "params": {}
    });
    stdin.write_all(&encode(&shutdown)).expect("write shutdown");
    stdin.flush().expect("flush shutdown");
    drop(stdin);

    // Wait up to 5s for the process to exit on its own.
    let exit_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait().expect("try_wait") {
            Some(status) => {
                assert!(
                    status.success() || status.code() == Some(0),
                    "plugin exited non-zero: {status:?}"
                );
                return;
            }
            None if Instant::now() > exit_deadline => {
                let _ = child.kill();
                panic!("plugin did not exit within 5s of shutdown notification");
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}
