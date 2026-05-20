//! End-to-end smoke test for the witr subprocess plugin.
//!
//! Spawns the `ainb-plugin-witr` binary the same way the runtime would,
//! drives it with Content-Length-framed JSON-RPC requests over
//! stdin/stdout via [`ainb_plugin_protocol::framing`], and asserts:
//!
//! 1. `plugin/init` returns `name=witr, version=0.1.0` (matches the
//!    `[plugin]` section of `manifest.toml`) — proves the SDK's
//!    manifest-echo path is wired.
//! 2. `plugin/render` with a 40×4 viewport returns a `WireBuffer` of
//!    matching dimensions — proves the dispatcher and the render path
//!    are alive.
//! 3. `plugin/shutdown` is acknowledged and the process exits within a
//!    short window — proves graceful shutdown.

use std::io::{BufRead, BufReader, Write};
use std::process::{ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use ainb_plugin_protocol::framing;
use serde_json::{Value, json};

fn binary_path() -> &'static str {
    env!("CARGO_BIN_EXE_ainb-plugin-witr")
}

fn write_frame(w: &mut ChildStdin, body: &Value) {
    let bytes = serde_json::to_vec(body).expect("serialize JSON-RPC body");
    let frame = framing::encode(&bytes);
    w.write_all(&frame).expect("write frame");
    w.flush().expect("flush stdin");
}

fn read_frame<R: BufRead>(r: &mut R) -> Option<Value> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = r.read_line(&mut line).expect("read header line");
        if n == 0 {
            return None;
        }
        if !line.ends_with("\r\n") {
            panic!("header not CRLF terminated: {line:?}");
        }
        let trimmed = line.trim_end_matches("\r\n");
        if trimmed.is_empty() {
            let len = content_length.expect("missing Content-Length header");
            let mut body = vec![0_u8; len];
            r.read_exact(&mut body).expect("read body");
            return Some(serde_json::from_slice(&body).expect("parse JSON body"));
        }
        let (name, value) = trimmed
            .split_once(':')
            .unwrap_or_else(|| panic!("malformed header: {trimmed:?}"));
        if name.trim().eq_ignore_ascii_case("Content-Length") {
            content_length = Some(value.trim().parse().expect("numeric Content-Length"));
        } else {
            panic!("unsupported header: {name}");
        }
    }
}

#[test]
fn init_render_shutdown_round_trip() {
    let mut child = Command::new(binary_path())
        // Mute plugin tracing — keeps the assertion log clean.
        .env("RUST_LOG", "off")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn witr plugin");

    let mut stdin = child.stdin.take().expect("stdin handle");
    let stdout: ChildStdout = child.stdout.take().expect("stdout handle");
    let mut stdout = BufReader::new(stdout);

    // 1. plugin/init — no reverse calls expected at cfx.1 (on_init is
    // the SDK default no-op until cfx.8 adds the snapshot publisher).
    let init = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "plugin/init",
        "params": {
            "manifest_path": "/tmp/witr/manifest.toml",
            "granted_capabilities": ["spawn_subprocess", "event_bus"],
            "abi_version": 2
        }
    });
    write_frame(&mut stdin, &init);

    let resp = read_frame(&mut stdout).expect("init response");
    assert_eq!(resp["jsonrpc"], "2.0");
    assert_eq!(resp["id"], 1);
    let result = resp.get("result").expect("init must succeed");
    assert_eq!(result["name"], "witr", "manifest name echo: {resp}");
    assert_eq!(result["version"], "0.1.0", "manifest version echo: {resp}");

    // 2. plugin/render — empty viewport-shaped WireBuffer at cfx.1.
    let render = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "plugin/render",
        "params": {
            "viewport": { "width": 40_u16, "height": 4_u16 },
            "generation": 0_u64
        }
    });
    write_frame(&mut stdin, &render);

    let resp = read_frame(&mut stdout).expect("render response");
    assert_eq!(resp["id"], 2);
    let result = resp.get("result").expect("render must succeed");
    let buffer = result.get("buffer").expect("render result has buffer");
    assert_eq!(buffer["width"], 40, "render buffer width: {resp}");
    assert_eq!(buffer["height"], 4, "render buffer height: {resp}");
    assert!(
        buffer["cells"].is_array(),
        "render buffer cells must be array"
    );

    // 3. plugin/shutdown — notification.
    let shutdown = json!({
        "jsonrpc": "2.0",
        "method": "plugin/shutdown",
        "params": {}
    });
    write_frame(&mut stdin, &shutdown);
    drop(stdin);

    let exit_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait().expect("try_wait") {
            Some(status) => {
                assert!(status.success(), "plugin exited non-zero: {status:?}");
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
