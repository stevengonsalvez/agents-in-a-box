//! Subprocess smoke test for the session-reader plugin.
//!
//! Spawns the built binary, drives the JSON-RPC handshake by hand
//! (Content-Length framed), and asserts:
//!   1. `plugin/init` returns `{name: "session-reader", version: ...}`.
//!   2. During `on_init` the plugin emits a `host/snapshot/subscribe`
//!      request for `sessions.refresh_request` (we reply OK) and then
//!      returns — it does NOT publish on startup. Publishing is
//!      gated on a `sessions.refresh_request` event so consumers are
//!      guaranteed to be subscribed before chunk 0 hits the wire.
//!   3. A `plugin/handle_event` for `sessions.refresh_request`
//!      triggers a chunked `rmp-serde`-encoded `UsageDataEvent`
//!      publish on `sessions.usage_data`.
//!   4. `plugin/shutdown` returns cleanly and the process exits.
//!
//! The fixture sets `HOME` to an empty tempdir so the scan finds no
//! provider data (parsers degrade to empty `Vec<ProviderCall>` for
//! non-existent dirs). The published `UsageDataEvent` is still valid —
//! `data` is `UsageData::default()` and `version == WIRE_VERSION`.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use ainb_plugin_protocol::{
    framing, methods,
    params::{
        HandleEventParams, PluginInitResult, PluginShutdownResult, SnapshotPublishParams,
        SnapshotSubscribeResult,
    },
};
use ainb_plugin_types_sessions::{UsageDataEvent, WIRE_VERSION};
use serde_json::{Value, json};

fn binary_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_ainb-plugin-session-reader"))
}

fn write_frame<W: Write>(w: &mut W, body: &Value) {
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
            let mut body = vec![0u8; len];
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

/// A frame delivered off the reader thread, or the terminal EOF marker.
enum FrameMsg {
    Frame(Value),
    Eof,
}

/// Spawn a thread that reads Content-Length frames off the child's stdout
/// and forwards them over a channel. This is what makes the read path
/// *bounded*: the blocking `read_frame` (blocking `read_line`/`read_exact`)
/// lives on the thread, and the test polls with `recv_timeout` so a silent
/// plugin surfaces as a timeout instead of wedging the test forever.
fn spawn_frame_reader(stdout: ChildStdout) -> Receiver<FrameMsg> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            match read_frame(&mut reader) {
                Some(frame) => {
                    if tx.send(FrameMsg::Frame(frame)).is_err() {
                        break; // receiver gone — test finished
                    }
                }
                None => {
                    let _ = tx.send(FrameMsg::Eof);
                    break;
                }
            }
        }
    });
    rx
}

/// Wait for the next frame, bounded by `deadline`. `None` means the plugin
/// closed stdout (clean EOF). A timeout or a dead reader thread panics with a
/// clear message rather than hanging.
fn next_frame(rx: &Receiver<FrameMsg>, deadline: Instant) -> Option<Value> {
    let timeout = deadline.saturating_duration_since(Instant::now());
    match rx.recv_timeout(timeout) {
        Ok(FrameMsg::Frame(frame)) => Some(frame),
        Ok(FrameMsg::Eof) => None,
        Err(RecvTimeoutError::Timeout) => {
            panic!(
                "timed out waiting for a plugin frame after {:?} — the plugin is \
                 silent (never emitted host/snapshot/subscribe, the init response, \
                 or a publish). Check the child's stderr for a panic or a stalled \
                 handshake.",
                timeout
            )
        }
        Err(RecvTimeoutError::Disconnected) => {
            panic!(
                "plugin stdout reader thread ended before EOF — a frame failed to \
                 parse (see the panic on the reader thread) or the pipe broke"
            )
        }
    }
}

#[test]
fn subprocess_init_publishes_snapshot_then_responds_ok() {
    // Empty HOME so ProviderRoots::defaults() points at non-existent
    // dirs. Parsers degrade to empty without erroring.
    let home = tempfile::tempdir().expect("tempdir HOME");

    let mut child = Command::new(binary_path())
        .env("HOME", home.path())
        // Mute the plugin's tracing output — keeps the assertion log clean.
        .env("RUST_LOG", "off")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn session-reader binary");

    let mut stdin = child.stdin.take().expect("stdin");
    let frames = spawn_frame_reader(child.stdout.take().expect("stdout"));

    // 1) Send plugin/init.
    write_frame(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": methods::PLUGIN_INIT,
            "params": {
                "manifest_path": "/dev/null/manifest.toml",
                "granted_capabilities": [
                    "read_claude_logs",
                    "read_codex_logs",
                    "event_bus",
                    "write_plugin_data"
                ],
                "abi_version": 2,
            },
        }),
    );

    // 2) During on_init the plugin issues one host/snapshot/subscribe per
    //    topic it wants (currently sessions.refresh_request and
    //    sessions.flush_cache_request) and AWAITS each reply before moving
    //    on, so we must answer every subscribe to let on_init finish and
    //    return the plugin/init response. It must NOT publish during init —
    //    publishing is gated on refresh_request.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut subscribed_topics: Vec<String> = Vec::new();
    let mut init_response: Option<Value> = None;

    while init_response.is_none() && Instant::now() < deadline {
        let frame = match next_frame(&frames, deadline) {
            Some(v) => v,
            None => panic!("plugin closed stdout before init response"),
        };

        if let Some(method) = frame.get("method").and_then(Value::as_str) {
            match method {
                m if m == methods::HOST_SNAPSHOT_SUBSCRIBE => {
                    let id = frame
                        .get("id")
                        .and_then(Value::as_i64)
                        .expect("snapshot/subscribe should be a request with id");
                    let topic = frame
                        .get("params")
                        .and_then(|p| p.get("topic"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    subscribed_topics.push(topic);
                    // Reply so the awaiting on_init can proceed to the next
                    // subscribe (and eventually return the init response).
                    write_frame(
                        &mut stdin,
                        &json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": SnapshotSubscribeResult {},
                        }),
                    );
                }
                m if m == methods::HOST_SNAPSHOT_PUBLISH => {
                    panic!("plugin published during init — gated on refresh_request");
                }
                m if m == methods::HOST_LOG => { /* ignore log notifications */ }
                other => panic!("unexpected outbound method during init: {other}"),
            }
        } else if frame.get("id").and_then(Value::as_i64) == Some(1) {
            init_response = Some(frame);
        } else if frame.get("id").is_some() {
            panic!("unexpected response frame during init: {frame}");
        }
    }

    assert!(
        subscribed_topics.iter().any(|t| t == "sessions.refresh_request"),
        "plugin never subscribed to sessions.refresh_request; saw {subscribed_topics:?}"
    );

    let init_response = init_response.expect("no plugin/init response within deadline");
    assert!(
        init_response.get("error").is_none(),
        "init failed: {init_response}"
    );
    let result_value = init_response.get("result").cloned().expect("init result");
    let init_result: PluginInitResult =
        serde_json::from_value(result_value).expect("decode PluginInitResult");
    assert_eq!(init_result.name, "session-reader");
    assert_eq!(init_result.version, "0.2.0");

    // 5) handle_event for the refresh topic should trigger a publish —
    //    this is the primary trigger for snapshot delivery.
    write_frame(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": methods::PLUGIN_HANDLE_EVENT,
            "params": HandleEventParams {
                topic: "sessions.refresh_request".into(),
                payload: bytes::Bytes::from_static(b""),
            },
        }),
    );

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut got_refresh_publish = false;
    while Instant::now() < deadline {
        let frame = match next_frame(&frames, deadline) {
            Some(v) => v,
            None => break,
        };
        if frame.get("method").and_then(Value::as_str) == Some(methods::HOST_SNAPSHOT_PUBLISH) {
            let params: SnapshotPublishParams =
                serde_json::from_value(frame.get("params").cloned().unwrap_or(Value::Null))
                    .expect("decode publish params");
            assert_eq!(params.topic, "sessions.usage_data");
            let event: UsageDataEvent = rmp_serde::from_slice(&params.payload)
                .expect("snapshot payload is rmp-serde UsageDataEvent");
            assert_eq!(event.version, WIRE_VERSION);
            assert!(!event.partial);
            // Empty HOME → single chunk with is_final = true.
            assert_eq!(event.chunk_index, 0);
            assert!(event.is_final);
            got_refresh_publish = true;
            break;
        }
    }
    assert!(
        got_refresh_publish,
        "plugin did not publish snapshot after handle_event(refresh)"
    );

    // 6) Clean shutdown.
    write_frame(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 99,
            "method": methods::PLUGIN_SHUTDOWN,
            "params": {},
        }),
    );

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut shutdown_ok = false;
    while Instant::now() < deadline {
        let frame = match next_frame(&frames, deadline) {
            Some(v) => v,
            None => break,
        };
        if frame.get("id").and_then(Value::as_i64) == Some(99) {
            assert!(frame.get("error").is_none(), "shutdown failed: {frame}");
            let _: PluginShutdownResult =
                serde_json::from_value(frame.get("result").cloned().unwrap_or(Value::Null))
                    .expect("decode PluginShutdownResult");
            shutdown_ok = true;
            break;
        }
    }
    assert!(shutdown_ok, "no plugin/shutdown response within deadline");

    drop(stdin);

    // Drain stderr so the kill-on-drop doesn't panic on a SIGPIPE write.
    let _ = child.stderr.as_mut().map(|s| {
        let mut sink = Vec::new();
        let _ = s.read_to_end(&mut sink);
    });

    let status = child.wait().expect("wait child");
    assert!(status.success(), "child exited non-zero: {status:?}");
}
