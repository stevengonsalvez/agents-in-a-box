//! Tripwire (P3.8): end-to-end plugin↔daemon roundtrip.
//!
//! This is the final P3 gate. It proves the user-visible end state of the
//! whole phase: the **real** [`HangarPlugin`], driven behind the **real**
//! `ainb-plugin-sdk-rust` [`Server`], dials a **real daemon socket**, sends
//! `workspace/subscribe`, and surfaces the connected state in the shared P4.1
//! chrome as the `online` presence dot — within a deterministic budget and
//! against an isolated `$HOME`. (P3.8 originally grepped a `"Hangar: Connected"`
//! footer; P4.1 replaced that string with the chrome presence dot.)
//!
//! ## Why not the literal `ainb tui` + tmux skeleton in `P3.md`?
//!
//! The phase doc sketches spawning `ainb-hangar-daemon` + `ainb tui` (which
//! would load `hangar-tui` via the host) inside tmux and grepping the pane
//! for the connected state. That path has two **unmet cross-phase
//! prerequisites** the doc itself calls out (P3.md "Daemon-side requirement"
//! lines 432-435):
//!
//!   1. `ainb-hangar-daemon` does **not** yet open a `UnixListener` on
//!      `~/.agents-in-a-box/hangar.sock` — its `boot()` runs the claim-loop FSM only
//!      (P1 socket-listener work is still open).
//!   2. `ainb tui` does **not** yet discover + load the `hangar-tui`
//!      subprocess plugin through the runtime, so no footer would ever paint.
//!
//! Forcing those would re-do P1 / P3.6 work outside this bead. Instead this
//! tripwire stands in a *real* daemon: a `UnixListener`-backed server that
//! speaks the genuine `ainb-hangar-proto` `workspace/subscribe` method over
//! the genuine Content-Length JSON-RPC framing. Every wire byte is real; the
//! only thing mocked away is the `ainb tui` host shell relaying the cap
//! stream (this test plays that host, exactly as the production runtime
//! would). When the daemon grows a real listener (tracked as a retroactive
//! `hangar:P1.X`), this file is the contract the listener must satisfy.
//!
//! ## What is asserted (the user-visible deliverable)
//!
//!   * Happy path: chrome presence dot reads `online` within budget.
//!   * Failure injection (P3.8 acceptance): stop the daemon mid-stream and
//!     assert the presence dot flips to `offline` within 2s.
//!
//! ## Determinism guards
//!
//!   * Isolated `$HOME` (tempdir) so parallel tripwires never share a socket
//!     dir, and the daemon socket path is per-test unique.
//!   * A hard 30s outer timeout per `reference_vhs_sleep_budget`.
//!   * No wall-clock dependence: the plugin's `Connection` state machine is
//!     event-driven; the test polls render output, not a timer.

use std::time::Duration;

use ainb_hangar_proto::{methods as daemon_methods, RpcRequest, RpcResponse};
use ainb_plugin_hangar::HangarPlugin;
use ainb_plugin_protocol::params::{
    HandleEventParams, UnixSocketEvent, UnixSocketEventKind, UnixSocketSendParams,
};
use ainb_plugin_protocol::{framing, methods};
use ainb_plugin_sdk::Server;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::oneshot;

/// Outer budget for the whole roundtrip — generous per
/// `reference_vhs_sleep_budget`, but the event-driven plugin reaches
/// `Connected` in milliseconds; the budget only guards against a hang.
const E2E_BUDGET: Duration = Duration::from_secs(30);

/// Per-render poll budget: how many `plugin/render` round-trips to attempt
/// before giving up on a target footer. Each poll sleeps 20ms, so this is a
/// ~1s window — far inside `E2E_BUDGET`.
const RENDER_POLLS: usize = 50;

/// Read one Content-Length frame body, decoding the JSON. `None` on EOF.
async fn read_frame<R: tokio::io::AsyncBufRead + Unpin>(r: &mut R) -> Option<serde_json::Value> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt};
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = r.read_line(&mut line).await.ok()?;
        if n == 0 {
            return None;
        }
        let trimmed = line.trim_end_matches("\r\n");
        if trimmed.is_empty() {
            let len = content_length?;
            let mut body = vec![0u8; len];
            r.read_exact(&mut body).await.ok()?;
            return serde_json::from_slice(&body).ok();
        }
        if let Some((name, value)) = trimmed.split_once(':') {
            if name.trim().eq_ignore_ascii_case("Content-Length") {
                content_length = value.trim().parse().ok();
            }
        }
    }
}

/// A *real* daemon stand-in: a `UnixListener` that decodes genuine
/// `ainb-hangar-proto` requests and answers `workspace/subscribe` with a
/// spec-compliant `RpcResponse` (empty snapshot), exactly as the future P1
/// daemon listener must.
///
/// Returns a [`oneshot::Sender`] the test fires to forcibly close the
/// accepted connection — the failure-injection lever for the
/// "stop daemon mid-stream" leg.
fn spawn_real_daemon(listener: UnixListener) -> oneshot::Sender<()> {
    let (kill_tx, mut kill_rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        let mut authed = false;
        loop {
            tokio::select! {
                _ = &mut kill_rx => {
                    // Failure injection: drop the connection so the plugin sees EOF.
                    return;
                }
                frame = read_frame(&mut reader) => {
                    let Some(raw) = frame else { return };
                    // Decode as the genuine proto request type — proves the
                    // plugin emits real envelopes in the contract order:
                    // `auth/hello` first (e38.1), then `workspace/subscribe`.
                    let req: RpcRequest = match serde_json::from_value(raw) {
                        Ok(r) => r,
                        Err(_) => return,
                    };
                    let result = if authed {
                        assert_eq!(
                            req.method,
                            daemon_methods::WORKSPACE_SUBSCRIBE,
                            "plugin must send workspace/subscribe after auth"
                        );
                        // Empty snapshot ack, per the daemon contract.
                        serde_json::json!({ "snapshot": {} })
                    } else {
                        assert_eq!(
                            req.method,
                            daemon_methods::AUTH_HELLO,
                            "plugin must authenticate on its FIRST frame"
                        );
                        authed = true;
                        serde_json::json!({})
                    };
                    let resp = RpcResponse {
                        jsonrpc: "2.0".into(),
                        id: req.id,
                        result: Some(result),
                        error: None,
                    };
                    let body = serde_json::to_vec(&resp).expect("serialize resp");
                    if write_half.write_all(&framing::encode(&body)).await.is_err() {
                        return;
                    }
                    let _ = write_half.flush().await;
                }
            }
        }
    });
    kill_tx
}

/// Encode a host→plugin frame.
fn host_frame(body: &serde_json::Value) -> Vec<u8> {
    framing::encode(&serde_json::to_vec(body).unwrap())
}

/// Flatten every painted cell symbol from a `plugin/render` result frame.
///
/// P4.1 replaced the P3.x `"Hangar: Connected"` footer string with the shared
/// chrome, where the daemon-link signal surfaces as the top-bar presence dot
/// (`● online` / `○ offline`). This helper concatenates the whole buffer so
/// the tripwire can look for that presence label.
fn chrome_text(render_resp: &serde_json::Value) -> String {
    render_resp["result"]["buffer"]["cells"]
        .as_array()
        .map(|cells| {
            cells
                .iter()
                .map(|c| c[1]["symbol"].as_str().unwrap_or(""))
                .collect::<String>()
        })
        .unwrap_or_default()
}

/// Repeatedly `plugin/render` until the chrome contains `want`, or the poll
/// budget is exhausted. Returns the last chrome text seen so callers can
/// assert a helpful message on miss. Renders at the 80×24 chrome floor so the
/// right-hand `<slug> · ● <presence>` cluster has room beside the tab strip.
async fn poll_chrome_for<W, R>(host_write: &mut W, host_read: &mut R, want: &str) -> (bool, String)
where
    W: tokio::io::AsyncWrite + Unpin,
    R: tokio::io::AsyncBufRead + Unpin,
{
    let mut last = String::new();
    for _ in 0..RENDER_POLLS {
        host_write
            .write_all(&host_frame(&serde_json::json!({
                "jsonrpc": "2.0", "id": 99, "method": methods::PLUGIN_RENDER,
                // 140 cols so the right-cluster presence dot fits alongside the
                // ten-tab strip (e38.35 added the Usage tab, ~108 cols); the tabs
                // win width contention, so at a narrower width the cluster yields
                // to the tabs and `online` would not render.
                "params": { "viewport": {"width": 140, "height": 24}, "generation": 0 }
            })))
            .await
            .unwrap();
        loop {
            let f = read_frame(host_read).await.expect("plugin link alive");
            if f.get("id").and_then(serde_json::Value::as_i64) == Some(99) {
                last = chrome_text(&f);
                if last.contains(want) {
                    return (true, last);
                }
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    (false, last)
}

/// Drive `plugin/init` and service the plugin's reverse cap calls
/// (`unix_socket_dial` → `unix_socket_send`) by relaying to the real daemon
/// connection. Returns the live daemon connection + the host-minted
/// `stream_id` so the caller can pump daemon replies back as socket events.
async fn init_and_dial<W, R>(
    host_write: &mut W,
    host_read: &mut R,
    socket_path: &std::path::Path,
    stream_id: &str,
) -> UnixStream
where
    W: tokio::io::AsyncWrite + Unpin,
    R: tokio::io::AsyncBufRead + Unpin,
{
    host_write
        .write_all(&host_frame(&serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": methods::PLUGIN_INIT,
            "params": {
                "manifest_path": socket_path.to_str().unwrap(),
                "granted_capabilities": ["unix_socket_dial", "event_stream_subscribe"],
                "abi_version": 2,
            }
        })))
        .await
        .unwrap();

    let mut daemon_conn: Option<UnixStream> = None;
    // The plugin sends TWO connect-time frames back-to-back: the e38.1
    // `auth/hello` first, then `workspace/subscribe`. Relay both.
    let mut sends_relayed = 0;
    while sends_relayed < 2 {
        let frame = read_frame(host_read).await.expect("plugin link alive");
        match frame.get("method").and_then(|m| m.as_str()) {
            Some(methods::HOST_UNIX_SOCKET_DIAL) => {
                let conn = UnixStream::connect(socket_path)
                    .await
                    .expect("host dials daemon");
                daemon_conn = Some(conn);
                let id = frame["id"].clone();
                host_write
                    .write_all(&host_frame(&serde_json::json!({
                        "jsonrpc": "2.0", "id": id, "result": { "stream_id": stream_id }
                    })))
                    .await
                    .unwrap();
            }
            Some(methods::HOST_UNIX_SOCKET_SEND) => {
                let send: UnixSocketSendParams =
                    serde_json::from_value(frame["params"].clone()).unwrap();
                let conn = daemon_conn.as_mut().expect("dialed before send");
                conn.write_all(&send.bytes).await.unwrap();
                conn.flush().await.unwrap();
                sends_relayed += 1;
            }
            _ => { /* host/log etc. — drain */ }
        }
    }
    daemon_conn.expect("dialed")
}

/// Push one daemon reply frame to the plugin as a `socket:<stream_id>`
/// `plugin/handle_event` notification (the host's job in production).
async fn push_socket_data<W: tokio::io::AsyncWrite + Unpin>(
    host_write: &mut W,
    stream_id: &str,
    reply: &serde_json::Value,
) {
    let reply_frame = framing::encode(&serde_json::to_vec(reply).unwrap());
    let event = UnixSocketEvent {
        kind: UnixSocketEventKind::Data,
        bytes: Some(reply_frame.into()),
        error: None,
    };
    let evt = HandleEventParams {
        topic: format!("socket:{stream_id}"),
        payload: serde_json::to_vec(&event).unwrap().into(),
    };
    host_write
        .write_all(&host_frame(&serde_json::json!({
            "jsonrpc": "2.0", "method": methods::PLUGIN_HANDLE_EVENT, "params": evt
        })))
        .await
        .unwrap();
}

/// Push an EOF socket event — used by the failure-injection leg to model the
/// host observing the daemon connection closing.
async fn push_socket_eof<W: tokio::io::AsyncWrite + Unpin>(host_write: &mut W, stream_id: &str) {
    let event = UnixSocketEvent {
        kind: UnixSocketEventKind::Eof,
        bytes: None,
        error: None,
    };
    let evt = HandleEventParams {
        topic: format!("socket:{stream_id}"),
        payload: serde_json::to_vec(&event).unwrap().into(),
    };
    host_write
        .write_all(&host_frame(&serde_json::json!({
            "jsonrpc": "2.0", "method": methods::PLUGIN_HANDLE_EVENT, "params": evt
        })))
        .await
        .unwrap();
}

/// Happy path: real plugin dials real daemon socket and the chrome presence
/// dot reaches `online`. This is the P3.8 user-visible deliverable, re-pointed
/// at the P4.1 chrome (the old `"Hangar: Connected"` footer string is gone).
#[tokio::test]
async fn tripwire_hangar_plugin_connects() {
    let body = async {
        // Isolated $HOME-equivalent: a per-test tempdir owns the socket path,
        // so parallel tripwires never collide (determinism guard).
        let home = tempfile::tempdir().expect("isolated home");
        let socket_path = home.path().join("hangar.sock");
        let listener = UnixListener::bind(&socket_path).expect("daemon binds socket");
        let _kill = spawn_real_daemon(listener);

        let (host_side, plugin_side) = tokio::io::duplex(64 * 1024);
        let (plugin_read, plugin_write) = tokio::io::split(plugin_side);
        let server = tokio::spawn(Server::new(HangarPlugin::new()).run(plugin_read, plugin_write));

        let (host_read_half, mut host_write) = tokio::io::split(host_side);
        let mut host_read = BufReader::new(host_read_half);

        let stream_id = format!("sock-trip-{}", std::process::id());
        let mut daemon_conn =
            init_and_dial(&mut host_write, &mut host_read, &socket_path, &stream_id).await;

        // Read the daemon's real acks (auth, then workspace/subscribe) and
        // relay each to the plugin.
        let mut dr = BufReader::new(&mut daemon_conn);
        for which in ["auth ack", "subscribe ack"] {
            let reply = read_frame(&mut dr).await.expect(which);
            push_socket_data(&mut host_write, &stream_id, &reply).await;
        }

        let (connected, last) = poll_chrome_for(&mut host_write, &mut host_read, "online").await;

        drop(host_write);
        server.abort();
        assert!(
            connected,
            "presence never reached online; last chrome = {last:?}"
        );
    };

    tokio::time::timeout(E2E_BUDGET, body)
        .await
        .expect("tripwire exceeded e2e budget");
}

/// Failure injection (P3.8 acceptance): once connected, killing the daemon
/// connection must flip the chrome presence dot to `offline` within 2s.
#[tokio::test]
async fn tripwire_detects_daemon_drop() {
    let body = async {
        let home = tempfile::tempdir().expect("isolated home");
        let socket_path = home.path().join("hangar.sock");
        let listener = UnixListener::bind(&socket_path).expect("daemon binds socket");
        let kill = spawn_real_daemon(listener);

        let (host_side, plugin_side) = tokio::io::duplex(64 * 1024);
        let (plugin_read, plugin_write) = tokio::io::split(plugin_side);
        let server = tokio::spawn(Server::new(HangarPlugin::new()).run(plugin_read, plugin_write));

        let (host_read_half, mut host_write) = tokio::io::split(host_side);
        let mut host_read = BufReader::new(host_read_half);

        let stream_id = format!("sock-drop-{}", std::process::id());
        let mut daemon_conn =
            init_and_dial(&mut host_write, &mut host_read, &socket_path, &stream_id).await;

        let mut dr = BufReader::new(&mut daemon_conn);
        for which in ["auth ack", "subscribe ack"] {
            let reply = read_frame(&mut dr).await.expect(which);
            push_socket_data(&mut host_write, &stream_id, &reply).await;
        }

        let (connected, _) = poll_chrome_for(&mut host_write, &mut host_read, "online").await;
        assert!(connected, "precondition: must reach online before drop");

        // Stop the daemon mid-stream. In production the host's socket reader
        // observes the close and emits an Eof socket event; model that here.
        let _ = kill.send(());
        push_socket_eof(&mut host_write, &stream_id).await;

        let (disconnected, last) =
            poll_chrome_for(&mut host_write, &mut host_read, "offline").await;

        drop(host_write);
        server.abort();
        assert!(
            disconnected,
            "presence did not flip to offline after daemon drop; last = {last:?}"
        );
    };

    // 2s acceptance budget for the failure-detection leg, with headroom.
    tokio::time::timeout(Duration::from_secs(5), body)
        .await
        .expect("daemon-drop detection exceeded budget");
}
