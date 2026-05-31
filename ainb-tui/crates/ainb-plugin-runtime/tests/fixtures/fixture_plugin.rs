//! Tiny fixture plugin used by the runtime's e2e tests.
//!
//! Speaks the same JSON-RPC 2.0 / Content-Length stdio dialect as a
//! real plugin, but the implementation is hand-rolled (no SDK dependency
//! — the SDK lives in a sibling crate that the runtime tests must not
//! depend on). Behaviour:
//!
//! - `plugin/init` → reply with name/version echo.
//! - `plugin/render` → reply with a 1×1 buffer carrying "X" at (0,0).
//! - `plugin/cli_dispatch` → reply with stdout "ok\n", exit_code 0.
//! - `plugin/handle_event` → notification, just record (host-side test
//!   asserts via the followup snapshot publish round-trip).
//! - `host/action/invoke` → reply with payload echo (so the runtime
//!   test's invoke_action() round-trips).
//! - `plugin/shutdown` → notification; exit 0.
//!
//! On startup the fixture publishes a snapshot under topic
//! `fixture.greeting` with the payload `b"hello"` so the runtime
//! integration test can assert host/snapshot/publish + snapshot_get
//! end-to-end.
//!
//! Special env-var behaviours (used by the SIGKILL injection test):
//! - `FIXTURE_HANG_ON_INIT=1` → return successfully but never read more
//!   stdin (so the host's later request never gets answered, and the
//!   test SIGKILLs us).

use std::io::{BufReader, Write};

use ainb_plugin_protocol::framing::{MAX_BODY_BYTES, encode};
use ainb_plugin_protocol::methods;
use ainb_plugin_protocol::params::{
    ActionInvokeParams, ActionInvokeResult, CliDispatchResult, PluginInitResult, RenderResult,
    SnapshotPublishParams,
};
use ainb_plugin_protocol::wire_buffer::{Cell, Coord, WireBuffer};
use serde_json::{Value, json};

fn main() {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut writer = stdout.lock();

    // Publish a starting snapshot so the runtime can assert it shows up.
    publish_snapshot(&mut writer, "fixture.greeting", b"hello");

    loop {
        let body = match read_frame_sync(&mut reader) {
            Ok(Some(b)) => b,
            Ok(None) => break, // clean EOF — host closed stdin
            Err(e) => {
                eprintln!("fixture: read frame failed: {e}");
                break;
            }
        };
        let v: Value = match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("fixture: malformed JSON body: {e}");
                continue;
            }
        };
        let id = v.get("id").and_then(serde_json::Value::as_u64);
        let method = v.get("method").and_then(serde_json::Value::as_str).unwrap_or("");
        let params = v.get("params").cloned().unwrap_or(Value::Null);

        match method {
            methods::PLUGIN_INIT => {
                if std::env::var("FIXTURE_HANG_ON_INIT").is_ok() {
                    // Reply OK then deliberately stop reading stdin.
                    if let Some(id) = id {
                        write_response(&mut writer, id, init_result());
                    }
                    std::thread::park();
                } else if let Some(id) = id {
                    write_response(&mut writer, id, init_result());
                }
            }
            methods::PLUGIN_RENDER => {
                if let Some(id) = id {
                    let mut buf = WireBuffer::new(1, 1);
                    buf.push(Coord::new(0, 0), Cell::new("X"));
                    let result = serde_json::to_value(RenderResult { buffer: buf })
                        .expect("RenderResult serializable");
                    write_response(&mut writer, id, result);
                }
            }
            methods::PLUGIN_CLI_DISPATCH => {
                if let Some(id) = id {
                    let result = serde_json::to_value(CliDispatchResult {
                        stdout: bytes::Bytes::from_static(b"ok\n"),
                        stderr: bytes::Bytes::new(),
                        exit_code: 0,
                    })
                    .expect("CliDispatchResult serializable");
                    write_response(&mut writer, id, result);
                }
            }
            methods::PLUGIN_HANDLE_EVENT => {
                // Notification — log to stderr so the host's stderr drain sees it.
                eprintln!("fixture: handle_event {params}");
            }
            methods::PLUGIN_HANDLE_KEY => {
                // Notification — re-publish the raw params as a
                // snapshot the host integration test can poll. The
                // shape is exactly what came in over the wire so the
                // test can assert byte-for-byte equality without
                // round-tripping any types.
                eprintln!("fixture: handle_key {params}");
                let bytes = serde_json::to_vec(&params).expect("params re-serialise");
                publish_snapshot(&mut writer, "fixture.last_key", &bytes);
            }
            methods::HOST_ACTION_INVOKE => {
                // Echo the payload back as the action result. `payload`
                // is wire-encoded as a base64 string (post-v1 wire), but
                // we also accept the legacy JSON-array-of-bytes form so
                // older runtimes paired with this fixture keep working.
                if let Some(id) = id {
                    let payload = decode_action_invoke_params(params.clone());
                    let result = serde_json::to_value(ActionInvokeResult {
                        payload: bytes::Bytes::from(payload),
                    })
                    .expect("ActionInvokeResult serializable");
                    write_response(&mut writer, id, result);
                }
            }
            methods::PLUGIN_SHUTDOWN => {
                eprintln!("fixture: shutdown notification — exiting cleanly");
                std::process::exit(0);
            }
            other => {
                eprintln!("fixture: unknown method {other}");
                if let Some(id) = id {
                    let body = json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": { "code": -32601, "message": format!("method not found: {other}") }
                    });
                    write_value(&mut writer, &body);
                }
            }
        }
    }
}

/// Decode a `host/action/invoke` params block. Defers binary decoding
/// to the protocol's `ActionInvokeParams`, which already accepts both
/// the base64-string and legacy-byte-array forms for `payload`.
fn decode_action_invoke_params(value: Value) -> Vec<u8> {
    serde_json::from_value::<ActionInvokeParams>(value)
        .map(|p| p.payload.to_vec())
        .unwrap_or_default()
}

fn init_result() -> Value {
    serde_json::to_value(PluginInitResult {
        name: "fixture".into(),
        version: "0.1.0".into(),
    })
    .expect("PluginInitResult serializable")
}

fn write_response<W: Write>(w: &mut W, id: u64, result: Value) {
    let body = json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    });
    write_value(w, &body);
}

fn write_value<W: Write>(w: &mut W, v: &Value) {
    let bytes = serde_json::to_vec(v).expect("JSON serialize");
    let frame = encode(&bytes);
    if let Err(e) = w.write_all(&frame) {
        eprintln!("fixture: write_all failed: {e}");
    }
    if let Err(e) = w.flush() {
        eprintln!("fixture: flush failed: {e}");
    }
}

fn publish_snapshot<W: Write>(w: &mut W, topic: &str, payload: &[u8]) {
    let params = serde_json::to_value(SnapshotPublishParams {
        topic: topic.into(),
        payload: bytes::Bytes::copy_from_slice(payload),
    })
    .expect("SnapshotPublishParams serializable");
    let body = json!({
        "jsonrpc": "2.0",
        "method": methods::HOST_SNAPSHOT_PUBLISH,
        "params": params,
    });
    write_value(w, &body);
}

/// Sync read of a single Content-Length frame. Mirrors the
/// `read_frame` in the runtime's framing module but uses the std
/// blocking I/O.
fn read_frame_sync<R: std::io::BufRead>(r: &mut R) -> std::io::Result<Option<Vec<u8>>> {
    let mut content_length: Option<usize> = None;
    let mut any_byte_read = false;
    loop {
        let mut line = String::new();
        let n = r.read_line(&mut line)?;
        if n == 0 {
            if any_byte_read {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "EOF in headers",
                ));
            }
            return Ok(None);
        }
        any_byte_read = true;
        if !line.ends_with("\r\n") {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "header not CRLF terminated",
            ));
        }
        let trimmed = line.trim_end_matches("\r\n");
        if trimmed.is_empty() {
            let len = content_length.ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "missing Content-Length")
            })?;
            if len > MAX_BODY_BYTES {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "body too big",
                ));
            }
            let mut body = vec![0u8; len];
            r.read_exact(&mut body)?;
            return Ok(Some(body));
        }
        let Some((name, value)) = trimmed.split_once(':') else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "malformed header",
            ));
        };
        if name.trim().eq_ignore_ascii_case("Content-Length") {
            let parsed: usize = value.trim().parse().map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "non-numeric Content-Length",
                )
            })?;
            content_length = Some(parsed);
        } else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("unsupported header: {name}"),
            ));
        }
    }
}
