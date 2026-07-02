//! P2 — prove the control center answers an ASK over the socket.
//!
//! Drives the **real** [`HangarPlugin`] behind the **real** SDK [`Server`], with
//! the test playing the host that relays the plugin's reverse `unix_socket_*`
//! calls to a mock daemon. The mock daemon answers the fleet-wide
//! `attention/subscribe` with one open ASK row and records every method it sees.
//!
//! The journey: the board seeds from the attention snapshot, `C` opens the
//! control center, and `1` (the ①-glyph option) answers the selected ASK —
//! which must issue an `attention/answer` carrying that row's id and the first
//! option's label. This is the P2 headline behaviour: an `AskUserQuestion` raised
//! in any session is answerable from the TUI.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ainb_hangar_proto::{RpcRequest, RpcResponse, methods as daemon_methods};
use ainb_plugin_hangar::HangarPlugin;
use ainb_plugin_protocol::params::{
    HandleEventParams, KeyCode, KeyEvent, KeyKind, UnixSocketEvent, UnixSocketEventKind,
    UnixSocketSendParams,
};
use ainb_plugin_protocol::{framing, methods};
use ainb_plugin_sdk::Server;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

const BUDGET: Duration = Duration::from_secs(20);

/// A recorded daemon call: method + params.
type Seen = Arc<Mutex<Vec<(String, serde_json::Value)>>>;

async fn read_frame<R: tokio::io::AsyncBufRead + Unpin>(r: &mut R) -> Option<serde_json::Value> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt};
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).await.ok()? == 0 {
            return None;
        }
        let trimmed = line.trim_end_matches("\r\n");
        if trimmed.is_empty() {
            let len = content_length?;
            let mut body = vec![0u8; len];
            r.read_exact(&mut body).await.ok()?;
            return serde_json::from_slice(&body).ok();
        }
        if let Some((n, v)) = trimmed.split_once(':') {
            if n.trim().eq_ignore_ascii_case("Content-Length") {
                content_length = v.trim().parse().ok();
            }
        }
    }
}

async fn read_one_raw_frame<R: tokio::io::AsyncBufRead + Unpin>(r: &mut R) -> Option<Vec<u8>> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt};
    let mut content_length: Option<usize> = None;
    let mut header = Vec::new();
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).await.ok()? == 0 {
            return None;
        }
        header.extend_from_slice(line.as_bytes());
        let trimmed = line.trim_end_matches("\r\n");
        if trimmed.is_empty() {
            let len = content_length?;
            let mut body = vec![0u8; len];
            r.read_exact(&mut body).await.ok()?;
            let mut out = header;
            out.extend_from_slice(&body);
            return Some(out);
        }
        if let Some((n, v)) = trimmed.split_once(':') {
            if n.trim().eq_ignore_ascii_case("Content-Length") {
                content_length = v.trim().parse().ok();
            }
        }
    }
}

fn host_frame(body: &serde_json::Value) -> Vec<u8> {
    framing::encode(&serde_json::to_vec(body).unwrap())
}

async fn push_data<W: tokio::io::AsyncWrite + Unpin>(
    host_write: &mut W,
    stream_id: &str,
    reply: &[u8],
) {
    let event = UnixSocketEvent {
        kind: UnixSocketEventKind::Data,
        bytes: Some(reply.to_vec().into()),
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

async fn send_key<W: tokio::io::AsyncWrite + Unpin>(host_write: &mut W, ch: char) {
    let key = KeyEvent {
        code: KeyCode::Char { ch },
        mods: 0,
        kind: KeyKind::Press,
    };
    host_write
        .write_all(&host_frame(&serde_json::json!({
            "jsonrpc": "2.0", "method": methods::PLUGIN_HANDLE_KEY,
            "params": { "screen_id": "hangar", "key": key, "generation": 1 }
        })))
        .await
        .unwrap();
}

/// The seeded ASK attention row: session `s1`, two options `staging`/`prod`.
fn ask_snapshot() -> serde_json::Value {
    let payload = serde_json::json!({
        "kind": "ASK",
        "context": { "question": "Ship to which env?", "options": [{"label":"staging"},{"label":"prod"}] }
    })
    .to_string();
    serde_json::json!({
        "attention": [{
            "id": "att-1",
            "session_id": "s1",
            "cwd": "/work/deploy",
            "kind": "ask_user_question",
            "payload": payload,
            "created_at": 1_000_000_i64,
        }]
    })
}

/// The mock daemon reply for a method: the ASK snapshot for `attention/subscribe`,
/// a `delivered` outcome for `attention/answer`, and empty-but-valid shapes for
/// the rest (the plugin decodes them leniently).
fn result_for(method: &str) -> serde_json::Value {
    match method {
        m if m == daemon_methods::ATTENTION_SUBSCRIBE => ask_snapshot(),
        m if m == daemon_methods::ATTENTION_ANSWER => {
            serde_json::json!({ "outcome": "delivered", "via": "tmux (s1)" })
        }
        m if m == daemon_methods::WORKSPACE_SUBSCRIBE => serde_json::json!({ "snapshot": {} }),
        _ => serde_json::json!({}),
    }
}

fn spawn_daemon(listener: UnixListener, seen: Seen) {
    tokio::spawn(async move {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        while let Some(raw) = read_frame(&mut reader).await {
            let Ok(req) = serde_json::from_value::<RpcRequest>(raw) else {
                return;
            };
            seen.lock().unwrap().push((req.method.clone(), req.params.clone()));
            let resp = RpcResponse {
                jsonrpc: "2.0".into(),
                id: req.id,
                result: Some(result_for(&req.method)),
                error: None,
            };
            let body = serde_json::to_vec(&resp).unwrap();
            if write_half.write_all(&framing::encode(&body)).await.is_err() {
                return;
            }
            let _ = write_half.flush().await;
        }
    });
}

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
    loop {
        let frame = read_frame(host_read).await.expect("plugin link alive");
        match frame.get("method").and_then(|m| m.as_str()) {
            Some(methods::HOST_UNIX_SOCKET_DIAL) => {
                let conn = UnixStream::connect(socket_path).await.expect("dial daemon");
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
                return daemon_conn.expect("dialed");
            }
            _ => {}
        }
    }
}

/// Send one `plugin/render` and, if the plugin emits an outbound daemon request,
/// relay it to the daemon and push the reply back. Returns after the render
/// response or one relayed send, whichever comes first.
async fn relay_once<W, R, DR, DW>(
    host_write: &mut W,
    host_read: &mut R,
    daemon_reader: &mut DR,
    daemon_write: &mut DW,
    stream_id: &str,
) where
    W: tokio::io::AsyncWrite + Unpin,
    R: tokio::io::AsyncBufRead + Unpin,
    DR: tokio::io::AsyncBufRead + Unpin,
    DW: tokio::io::AsyncWrite + Unpin,
{
    host_write
        .write_all(&host_frame(&serde_json::json!({
            "jsonrpc": "2.0", "id": 99, "method": methods::PLUGIN_RENDER,
            "params": { "viewport": {"width": 160, "height": 40}, "generation": 0 }
        })))
        .await
        .unwrap();
    loop {
        let Some(frame) = read_frame(host_read).await else {
            return;
        };
        if frame.get("method").and_then(|m| m.as_str()) == Some(methods::HOST_UNIX_SOCKET_SEND) {
            let send: UnixSocketSendParams =
                serde_json::from_value(frame["params"].clone()).unwrap();
            daemon_write.write_all(&send.bytes).await.unwrap();
            daemon_write.flush().await.unwrap();
            if let Some(reply) = read_one_raw_frame(daemon_reader).await {
                push_data(host_write, stream_id, &reply).await;
            }
            return;
        }
        if frame.get("id").and_then(serde_json::Value::as_i64) == Some(99) {
            return;
        }
    }
}

#[tokio::test]
async fn pressing_one_answers_the_selected_ask() {
    let body = async {
        let home = tempfile::tempdir().expect("home");
        std::env::set_var("AINB_HANGAR_HOME", home.path());
        // Seed the first-run ack so the danger modal never intercepts keys.
        let state = home.path().join("hangar").join("state.toml");
        std::fs::create_dir_all(state.parent().unwrap()).unwrap();
        std::fs::write(&state, "warnings_ack = [\"first_run\"]\n").unwrap();

        let stream_id = format!("sock-ans-{}", std::process::id());
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let socket_path = home.path().join("hangar.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind daemon");
        spawn_daemon(listener, seen.clone());

        let (host_side, plugin_side) = tokio::io::duplex(256 * 1024);
        let (plugin_read, plugin_write) = tokio::io::split(plugin_side);
        let server = tokio::spawn(Server::new(HangarPlugin::new()).run(plugin_read, plugin_write));
        let (host_read_half, mut host_write) = tokio::io::split(host_side);
        let mut host_read = BufReader::new(host_read_half);

        let daemon = init_and_dial(&mut host_write, &mut host_read, &socket_path, &stream_id).await;
        let (daemon_read, mut daemon_write) = daemon.into_split();
        let mut daemon_reader = BufReader::new(daemon_read);

        // Push the auth ack so the plugin then relays workspace/subscribe.
        let auth = read_one_raw_frame(&mut daemon_reader).await.expect("auth ack");
        push_data(&mut host_write, &stream_id, &auth).await;

        // Drive renders until the fleet-wide attention/subscribe has round-tripped
        // (the board is seeded from its ASK snapshot).
        let mut seeded = false;
        for _ in 0..40 {
            relay_once(
                &mut host_write,
                &mut host_read,
                &mut daemon_reader,
                &mut daemon_write,
                &stream_id,
            )
            .await;
            if seen
                .lock()
                .unwrap()
                .iter()
                .any(|(m, _)| m == daemon_methods::ATTENTION_SUBSCRIBE)
            {
                seeded = true;
                break;
            }
        }
        assert!(
            seeded,
            "attention/subscribe never issued; saw: {:?}",
            seen.lock().unwrap()
        );
        // A couple more renders so the pushed snapshot is applied to the board.
        for _ in 0..3 {
            relay_once(
                &mut host_write,
                &mut host_read,
                &mut daemon_reader,
                &mut daemon_write,
                &stream_id,
            )
            .await;
        }

        // Open the control center and answer option ① (staging).
        send_key(&mut host_write, 'C').await;
        send_key(&mut host_write, '1').await;

        let mut answered = None;
        for _ in 0..40 {
            relay_once(
                &mut host_write,
                &mut host_read,
                &mut daemon_reader,
                &mut daemon_write,
                &stream_id,
            )
            .await;
            if let Some((_, params)) =
                seen.lock().unwrap().iter().find(|(m, _)| m == daemon_methods::ATTENTION_ANSWER)
            {
                answered = Some(params.clone());
                break;
            }
        }
        let params = answered.unwrap_or_else(|| {
            panic!(
                "pressing `1` must issue an attention/answer; saw: {:?}",
                seen.lock().unwrap()
            )
        });
        // The answer targets the seeded row and carries the first option's label.
        assert_eq!(
            params.get("attention_id").and_then(|v| v.as_str()),
            Some("att-1")
        );
        assert_eq!(
            params.get("answer").and_then(|v| v.as_str()),
            Some("staging")
        );
        assert_eq!(
            params.get("answered_by").and_then(|v| v.as_str()),
            Some("tui")
        );

        drop(host_write);
        server.abort();
    };
    tokio::time::timeout(BUDGET, body).await.expect("exceeded answer budget");
}
