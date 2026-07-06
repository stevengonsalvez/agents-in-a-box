//! 63l.4 — prove a cross-column card DRAG on the Issues board issues the right
//! daemon RPC (the headline of the board redesign).
//!
//! Drives the **real** [`HangarPlugin`] behind the **real** SDK [`Server`], with
//! the test playing the host that relays the plugin's reverse `unix_socket_*`
//! calls to a mock daemon recording each `(method, params)`. After a render
//! builds the board hit-map, the test synthesises a left-button DOWN on the
//! backlog card, a DRAG into the In Progress column's drop zone, and an UP there.
//! A follow-up render must drain the resulting `MoveCard` intent and issue a
//! `hangar/issue_update` whose `state` is `in_progress` and whose `issue_id` is
//! the dragged card — the SAME `issue_update` seam the keyboard / agent-picker
//! path uses.
//!
//! This is the user-visible proof: a drag across columns MOVES the issue (reaches
//! `issue_update{state}` over the socket), not just lights up a local highlight.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ainb_hangar_proto::{RpcRequest, RpcResponse, methods as daemon_methods};
use ainb_plugin_hangar::HangarPlugin;
use ainb_plugin_protocol::params::{
    HandleEventParams, MouseButton, MouseEvent, MouseKind, UnixSocketEvent, UnixSocketEventKind,
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

/// A mock daemon that records `(method, params)` and answers every request with a
/// seed-shaped result so the issue list has a backlog card (`issue-1`).
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

fn result_for(method: &str) -> serde_json::Value {
    match method {
        m if m == daemon_methods::WORKSPACE_SUBSCRIBE => serde_json::json!({ "snapshot": {} }),
        m if m == daemon_methods::HANGAR_ISSUES_LIST => serde_json::json!({
            "issues": [
                {"id":"issue-1","display_id":"HGR-1","workspace_id":"default",
                 "title":"Refactor API","description":null,"state":"backlog",
                 "assignee":null,"creator":"member:alice","priority":1,
                 "created_at":1_700_000_000_000_i64,"updated_at":1_700_000_000_000_i64,
                 "due_date":null,"task_id":null,"pr_url":null}
            ]
        }),
        m if m == daemon_methods::HANGAR_AGENTS_LIST => serde_json::json!({ "actors": [] }),
        m if m == daemon_methods::HANGAR_SKILLS_LIST => serde_json::json!({ "skills": [] }),
        m if m == daemon_methods::HANGAR_AUTOPILOTS_LIST => serde_json::json!({ "autopilots": [] }),
        m if m == daemon_methods::HANGAR_TASKS_LIST => serde_json::json!({ "tasks": [] }),
        m if m == daemon_methods::HANGAR_HEALTH => serde_json::json!({
            "socket_path":"/tmp/h.sock","pid":1,"uptime_secs":1,"version":"0.1.0","connected":true
        }),
        _ => serde_json::json!({}),
    }
}

fn host_frame(body: &serde_json::Value) -> Vec<u8> {
    framing::encode(&serde_json::to_vec(body).unwrap())
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

/// Relay the 6 subscribe-snapshot sends + replies (issues / agents / skills /
/// autopilots / tasks / health).
async fn pump_snapshots<W, R, DR, DW>(
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
    let mut relayed = 0;
    while relayed < 6 {
        let frame = tokio::time::timeout(Duration::from_secs(2), read_frame(host_read))
            .await
            .ok()
            .flatten()
            .expect("snapshot send");
        if frame.get("method").and_then(|m| m.as_str()) == Some(methods::HOST_UNIX_SOCKET_SEND) {
            let send: UnixSocketSendParams =
                serde_json::from_value(frame["params"].clone()).unwrap();
            daemon_write.write_all(&send.bytes).await.unwrap();
            daemon_write.flush().await.unwrap();
            if let Some(reply) = read_one_raw_frame(daemon_reader).await {
                push_data(host_write, stream_id, &reply).await;
            }
            relayed += 1;
        }
    }
}

/// Drive one render so the plugin drains its pending mouse intents + the armed
/// issue-state RPC; relay any reverse send to the daemon + pump the reply back.
async fn relay_one_send_or_render<W, R, DR, DW>(
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
            "params": { "viewport": {"width": 120, "height": 40}, "generation": 0 }
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

/// Forward one synthetic mouse event into the plugin (`plugin/handle_mouse`).
async fn send_mouse<W: tokio::io::AsyncWrite + Unpin>(
    host_write: &mut W,
    kind: MouseKind,
    col: u16,
    row: u16,
) {
    let mouse = MouseEvent {
        kind,
        col,
        row,
        mods: 0,
    };
    host_write
        .write_all(&host_frame(&serde_json::json!({
            "jsonrpc": "2.0", "method": methods::PLUGIN_HANDLE_MOUSE,
            "params": { "screen_id": "hangar", "mouse": mouse, "generation": 1 }
        })))
        .await
        .unwrap();
}

#[tokio::test]
async fn drag_card_across_columns_issues_issue_update_state() {
    let body = async {
        let home = tempfile::tempdir().expect("home");
        std::env::set_var("AINB_HANGAR_HOME", home.path());
        let stream_id = format!("sock-drag-{}", std::process::id());
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));

        let state = home.path().join("hangar").join("state.toml");
        std::fs::create_dir_all(state.parent().unwrap()).expect("state dir");
        std::fs::write(&state, "warnings_ack = [\"first_run\"]\n").expect("seed ack");

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

        let ack = read_one_raw_frame(&mut daemon_reader).await.expect("subscribe ack");
        push_data(&mut host_write, &stream_id, &ack).await;
        pump_snapshots(
            &mut host_write,
            &mut host_read,
            &mut daemon_reader,
            &mut daemon_write,
            &stream_id,
        )
        .await;

        // A render builds the board hit-map for the 120×40 viewport. The board
        // body runs from row 2; each of the five columns is 24 cells wide. The
        // single backlog card (`issue-1`) sits in column 0 (x in [0, 24)); its
        // body card spans rows 4..10. The In Progress column is column 2 (x in
        // [48, 72)); its drop zone is the column body below the header.
        relay_one_send_or_render(
            &mut host_write,
            &mut host_read,
            &mut daemon_reader,
            &mut daemon_write,
            &stream_id,
        )
        .await;

        // Press the backlog card (column 0, well inside its body card), drag into
        // the In Progress column's drop zone (column 2), and release there.
        send_mouse(
            &mut host_write,
            MouseKind::Down {
                button: MouseButton::Left,
            },
            3,
            5,
        )
        .await;
        send_mouse(
            &mut host_write,
            MouseKind::Drag {
                button: MouseButton::Left,
            },
            55,
            15,
        )
        .await;
        send_mouse(
            &mut host_write,
            MouseKind::Up {
                button: MouseButton::Left,
            },
            55,
            15,
        )
        .await;

        // A render drains the MoveCard intent (optimistic local move) and fires the
        // armed `hangar/issue_update{state:in_progress}` over the socket.
        let mut sent = false;
        for _ in 0..40 {
            relay_one_send_or_render(
                &mut host_write,
                &mut host_read,
                &mut daemon_reader,
                &mut daemon_write,
                &stream_id,
            )
            .await;
            let hit = seen.lock().unwrap().iter().any(|(m, p)| {
                m == daemon_methods::HANGAR_ISSUE_UPDATE
                    && p.get("issue_id").and_then(serde_json::Value::as_str) == Some("issue-1")
                    && p.get("state").and_then(serde_json::Value::as_str) == Some("in_progress")
            });
            if hit {
                sent = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            sent,
            "a cross-column drag must issue hangar/issue_update(state=in_progress) for issue-1; \
             saw: {:?}",
            seen.lock().unwrap()
        );

        drop(host_write);
        server.abort();
    };
    tokio::time::timeout(BUDGET, body)
        .await
        .expect("exceeded issue-board drag budget");
}
