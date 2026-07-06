//! e38.5 — prove the task-detail compose key issues the right daemon RPC.
//!
//! Drives the **real** [`HangarPlugin`] behind the **real** SDK [`Server`], with
//! the test playing the host that relays the plugin's reverse `unix_socket_*`
//! calls to a mock daemon recording each `(method, params)`. After opening an
//! issue's task detail (Enter on the selected issue-list row), pressing the
//! compose key (`c`), typing a body, and pressing Enter, the plugin must issue a
//! `hangar/comment_add` whose `body` is the typed text and whose `issue_id` is the
//! issue the task detail was opened for.
//!
//! This is the **user-visible proof** for the bead: opening a task detail,
//! pressing the compose key, typing, and submitting must reach `comment_add` over
//! the socket with the typed body — not just close a modal.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ainb_hangar_proto::{RpcRequest, RpcResponse, methods as daemon_methods};
use ainb_plugin_hangar::HangarPlugin;
use ainb_plugin_protocol::params::{
    HandleEventParams, KeyCode, KeyEvent, UnixSocketEvent, UnixSocketEventKind,
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

/// A mock daemon that records `(method, params)` and answers every request with
/// a seed-shaped result so the issue list has a row (`issue-1`) the task detail
/// opens for.
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
                {"id":"issue-1","workspace_id":"default","title":"Refactor API",
                 "description":null,"state":"open","assignee":null,
                 "creator":"member:alice","priority":1,"created_at":1_700_000_000_000_i64,
                 "updated_at":1_700_000_000_000_i64,"due_date":null,"task_id":null,
                 "pr_url":null}
            ]
        }),
        m if m == daemon_methods::HANGAR_AGENTS_LIST => serde_json::json!({ "actors": [] }),
        m if m == daemon_methods::HANGAR_SKILLS_LIST => serde_json::json!({ "skills": [] }),
        m if m == daemon_methods::HANGAR_AUTOPILOTS_LIST => serde_json::json!({ "autopilots": [] }),
        m if m == daemon_methods::HANGAR_TASKS_LIST => serde_json::json!({ "tasks": [] }),
        m if m == daemon_methods::HANGAR_COMMENT_ADD => serde_json::json!({
            "id":"c1","issue_id":"issue-1","author":"member:me",
            "body":"ship it","created_at":1_700_000_000_000_i64
        }),
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

/// Drive one render so the plugin drains its pending comment action; relay any
/// reverse send to the daemon + pump the reply back.
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

async fn send_key<W: tokio::io::AsyncWrite + Unpin>(host_write: &mut W, code: KeyCode) {
    let key = KeyEvent {
        code,
        mods: 0,
        kind: ainb_plugin_protocol::params::KeyKind::Press,
    };
    host_write
        .write_all(&host_frame(&serde_json::json!({
            "jsonrpc": "2.0", "method": methods::PLUGIN_HANDLE_KEY,
            "params": { "screen_id": "hangar", "key": key, "generation": 1 }
        })))
        .await
        .unwrap();
}

#[tokio::test]
async fn compose_key_then_enter_issues_comment_add() {
    let body = async {
        let home = tempfile::tempdir().expect("home");
        std::env::set_var("AINB_HANGAR_HOME", home.path());
        let stream_id = format!("sock-comment-{}", std::process::id());
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

        // Enter on the selected issue opens its task detail; `c` opens the
        // compose modal; type a body; Enter submits the comment.
        send_key(&mut host_write, KeyCode::Enter).await;
        send_key(&mut host_write, KeyCode::Char { ch: 'c' }).await;
        for ch in "ship it".chars() {
            send_key(&mut host_write, KeyCode::Char { ch }).await;
        }
        send_key(&mut host_write, KeyCode::Enter).await;

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
                m == daemon_methods::HANGAR_COMMENT_ADD
                    && p.get("issue_id").and_then(serde_json::Value::as_str) == Some("issue-1")
                    && p.get("body").and_then(serde_json::Value::as_str) == Some("ship it")
            });
            if hit {
                sent = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            sent,
            "compose Enter must issue hangar/comment_add(body=ship it) for issue-1; saw: {:?}",
            seen.lock().unwrap()
        );

        drop(host_write);
        server.abort();
    };
    tokio::time::timeout(BUDGET, body)
        .await
        .expect("exceeded comment-compose budget");
}
