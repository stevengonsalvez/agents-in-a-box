//! Integration: the hangar plugin dials a mock daemon through the host
//! `unix_socket_dial` cap and reaches `Connected`.
//!
//! This drives the **real** [`HangarPlugin`] behind the **real**
//! `ainb-plugin-sdk-rust` [`Server`], wired over a `tokio::io::duplex`
//! pair. The test plays the *host*: it answers the plugin's reverse
//! `host/unix_socket_dial` / `host/unix_socket_send` calls by relaying to
//! a mock daemon listening on a tempdir `UnixListener`, then pushes the
//! daemon's reply frames back to the plugin as a `socket:<id>`
//! `plugin/handle_event` notification.
//!
//! Acceptance (P3.7): the plugin reaches `Connected` within 1s; P4.1 re-points
//! the render assertion at the shared chrome, whose presence dot reads
//! `online` once the daemon acks the subscribe.

use std::time::Duration;

use ainb_plugin_hangar::HangarPlugin;
use ainb_plugin_protocol::params::{
    HandleEventParams, UnixSocketEvent, UnixSocketEventKind, UnixSocketSendParams,
};
use ainb_plugin_protocol::{framing, methods};
use ainb_plugin_sdk::Server;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

/// Read one Content-Length frame body from an async reader, decoding the
/// JSON body. Returns `None` on clean EOF.
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

/// Mock daemon: accept one connection, answer every Content-Length
/// JSON-RPC request with `{"result": {}}` echoing the id.
fn spawn_mock_daemon(listener: UnixListener) {
    tokio::spawn(async move {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        while let Some(req) = read_frame(&mut reader).await {
            let id = req.get("id").cloned().unwrap_or(serde_json::Value::Null);
            let resp = serde_json::json!({"jsonrpc": "2.0", "id": id, "result": {}});
            let body = serde_json::to_vec(&resp).unwrap();
            if write_half.write_all(&framing::encode(&body)).await.is_err() {
                break;
            }
            let _ = write_half.flush().await;
        }
    });
}

/// Encode a host→plugin frame.
fn host_frame(body: &serde_json::Value) -> Vec<u8> {
    framing::encode(&serde_json::to_vec(body).unwrap())
}

/// Flatten every painted cell symbol from a `plugin/render` result frame.
///
/// P4.1 replaced the P3.7 `"Hangar: Connected"` footer string with the shared
/// chrome: the daemon-link signal now surfaces as the top-bar presence dot
/// (`● online` / `○ offline`). This helper concatenates the whole buffer so
/// the connection test can look for that presence label.
fn render_text(render_resp: &serde_json::Value) -> String {
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

/// Repeatedly `plugin/render` and inspect the chrome until the presence dot
/// reads `online` (the P4.1 connected signal) or the retry budget is
/// exhausted. Renders at the 80×24 chrome floor so the right-hand
/// `<slug> · ● online` cluster has room beside the tab strip.
async fn poll_until_connected<W, R>(host_write: &mut W, host_read: &mut R) -> bool
where
    W: tokio::io::AsyncWrite + Unpin,
    R: tokio::io::AsyncBufRead + Unpin,
{
    for _ in 0..20 {
        host_write
            .write_all(&host_frame(&serde_json::json!({
                "jsonrpc": "2.0", "id": 99, "method": methods::PLUGIN_RENDER,
                "params": { "viewport": {"width": 80, "height": 24}, "generation": 0 }
            })))
            .await
            .unwrap();
        // Drain frames until the render result (id 99) arrives.
        loop {
            let f = read_frame(host_read).await.expect("link alive");
            if f.get("id").and_then(serde_json::Value::as_i64) == Some(99) {
                if render_text(&f).contains("online") {
                    return true;
                }
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

#[tokio::test]
async fn plugin_reaches_connected_against_mock_daemon() {
    let body = async {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket_path = dir.path().join("hangar.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind mock daemon");
        spawn_mock_daemon(listener);

        // Plugin behind the real SDK Server over a duplex link.
        let (host_side, plugin_side) = tokio::io::duplex(64 * 1024);
        let (plugin_read, plugin_write) = tokio::io::split(plugin_side);
        let server = tokio::spawn(Server::new(HangarPlugin::new()).run(plugin_read, plugin_write));

        let (host_read_half, mut host_write) = tokio::io::split(host_side);
        let mut host_read = BufReader::new(host_read_half);

        let stream_id = "sock-test-01";
        let mut daemon_conn: Option<tokio::net::UnixStream> = None;

        // 1. init the plugin — on_init dials + sends subscribe.
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

        // 2. Service the plugin's reverse calls until the subscribe is sent.
        //    The host drives: dial (request) → send (notification) → init result.
        let mut subscribe_sent = false;
        while !subscribe_sent {
            let frame = read_frame(&mut host_read).await.expect("plugin link alive");
            match frame.get("method").and_then(|m| m.as_str()) {
                Some(methods::HOST_UNIX_SOCKET_DIAL) => {
                    let conn = tokio::net::UnixStream::connect(&socket_path)
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
                    let conn = daemon_conn.as_mut().expect("dialed first");
                    conn.write_all(&send.bytes).await.unwrap();
                    conn.flush().await.unwrap();
                    subscribe_sent = true;
                }
                _ => { /* host/log etc. — drain */ }
            }
        }

        // 3. Read the daemon's reply, push it to the plugin as a socket event.
        let conn = daemon_conn.as_mut().unwrap();
        let mut dr = BufReader::new(conn);
        let reply = read_frame(&mut dr).await.expect("daemon reply");
        let reply_frame = framing::encode(&serde_json::to_vec(&reply).unwrap());
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

        // 4. Poll render until the chrome presence dot reads `online`.
        let connected = poll_until_connected(&mut host_write, &mut host_read).await;

        // Tear down: dropping the host write half won't reliably EOF the
        // duplex read half, so abort the server task rather than awaiting it.
        drop(host_write);
        server.abort();
        connected
    };

    let connected = tokio::time::timeout(Duration::from_secs(5), body)
        .await
        .expect("test exceeded 5s budget");
    assert!(connected, "plugin did not reach Connected");
}
