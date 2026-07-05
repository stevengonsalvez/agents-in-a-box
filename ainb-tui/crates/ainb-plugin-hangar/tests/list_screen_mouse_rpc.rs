//! 63l.6 — prove the list-screen RIGHT-CLICK context menu fires a real daemon RPC.
//!
//! Drives the **real** [`HangarPlugin`] behind the **real** SDK [`Server`], with
//! the test playing the host that relays the plugin's reverse `unix_socket_*`
//! calls to a mock daemon recording each `(method, params)`. After opening the
//! Kanban board (`K`), a synthetic RIGHT-click lands on the queued task's card
//! and a synthetic LEFT-click lands on the `Run now` context-menu row. The plugin
//! must then issue a `hangar/task_transition` whose `to_status` is `running` for
//! the clicked task — the EXISTING kanban RPC seam, fired by a mouse gesture, no
//! new wire method.
//!
//! The mutation guard at the foot proves the click is load-bearing: a left-click
//! that opens the card (instead of the right-click → Run-now menu) must NOT issue
//! the transition.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ainb_hangar_proto::{RpcRequest, RpcResponse, methods as daemon_methods};
use ainb_plugin_hangar::HangarPlugin;
use ainb_plugin_protocol::params::{
    HandleEventParams, KeyCode, KeyEvent, MouseButton, MouseEvent, MouseKind, UnixSocketEvent,
    UnixSocketEventKind, UnixSocketSendParams,
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

/// A mock daemon that records `(method, params)` and seeds one queued task so the
/// board has a card to right-click.
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
        m if m == daemon_methods::HANGAR_ISSUES_LIST => serde_json::json!({ "issues": [] }),
        m if m == daemon_methods::HANGAR_AGENTS_LIST => serde_json::json!({ "actors": [] }),
        m if m == daemon_methods::HANGAR_SKILLS_LIST => serde_json::json!({ "skills": [] }),
        m if m == daemon_methods::HANGAR_AUTOPILOTS_LIST => serde_json::json!({
            "autopilots": [
                {"id":"ap-1","workspace_id":"default","agent_id":"agent-1","name":"daily-triage",
                 "cron_expr":"0 9 * * *","next_tick_at":1_700_000_300_000_i64,"enabled":true,
                 "last_run_status":"completed","last_run_at":1_699_000_000_000_i64}
            ]
        }),
        m if m == daemon_methods::HANGAR_TASKS_LIST => serde_json::json!({
            "tasks": [
                {"id":"01HANGARTASKQUEUED01","workspace_id":"default","agent_id":"agent-1",
                 "issue_id":"issue-1","status":"queued","created_at":1_700_000_000_000_i64}
            ]
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

/// Relay the 6 subscribe-snapshot sends + replies.
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

/// Drive one render so the plugin drains its pending actions; relay any reverse
/// send to the daemon + pump the reply back.
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

/// Boot the plugin, dial the daemon, and pump the subscribe ack + snapshots.
/// Returns the wired host/daemon channels for the test body to drive.
struct Harness {
    host_write: tokio::io::WriteHalf<tokio::io::DuplexStream>,
    host_read: BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
    daemon_reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    daemon_write: tokio::net::unix::OwnedWriteHalf,
    server: tokio::task::JoinHandle<()>,
    stream_id: String,
    seen: Seen,
}

async fn boot(home: &std::path::Path, tag: &str) -> Harness {
    std::env::set_var("AINB_HANGAR_HOME", home);
    let stream_id = format!("sock-{tag}-{}", std::process::id());
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));

    let state = home.join("hangar").join("state.toml");
    std::fs::create_dir_all(state.parent().unwrap()).expect("state dir");
    std::fs::write(&state, "warnings_ack = [\"first_run\"]\n").expect("seed ack");

    let socket_path = home.join("hangar.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind daemon");
    spawn_daemon(listener, seen.clone());

    let (host_side, plugin_side) = tokio::io::duplex(256 * 1024);
    let (plugin_read, plugin_write) = tokio::io::split(plugin_side);
    let server = tokio::spawn(async move {
        let _ = Server::new(HangarPlugin::new()).run(plugin_read, plugin_write).await;
    });

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

    Harness {
        host_write,
        host_read,
        daemon_reader,
        daemon_write,
        server,
        stream_id,
        seen,
    }
}

/// Pump up to `n` render passes, returning once `pred` over the recorded daemon
/// calls holds.
async fn pump_until(
    h: &mut Harness,
    n: usize,
    pred: impl Fn(&[(String, serde_json::Value)]) -> bool,
) -> bool {
    for _ in 0..n {
        relay_one_send_or_render(
            &mut h.host_write,
            &mut h.host_read,
            &mut h.daemon_reader,
            &mut h.daemon_write,
            &h.stream_id,
        )
        .await;
        if pred(&h.seen.lock().unwrap()) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

/// Whether the daemon saw a `hangar/task_transition(to_status=running)` for the
/// seeded queued task.
fn saw_run_now(seen: &[(String, serde_json::Value)]) -> bool {
    seen.iter().any(|(m, p)| {
        m == daemon_methods::HANGAR_TASK_TRANSITION
            && p.get("task_id").and_then(serde_json::Value::as_str) == Some("01HANGARTASKQUEUED01")
            && p.get("to_status").and_then(serde_json::Value::as_str) == Some("running")
    })
}

/// Whether the daemon saw a `hangar/autopilot_fire_now` for the seeded autopilot.
fn saw_fire_now(seen: &[(String, serde_json::Value)]) -> bool {
    seen.iter().any(|(m, p)| {
        m == daemon_methods::HANGAR_AUTOPILOT_FIRE_NOW
            && p.get("autopilot_id").and_then(serde_json::Value::as_str) == Some("ap-1")
    })
}

/// A synthetic RIGHT-click on the queued task's card, then a LEFT-click on the
/// `Run now` context-menu row, must issue `hangar/task_transition(running)`.
#[tokio::test]
async fn kanban_right_click_run_now_issues_task_transition() {
    let body = async {
        let home = tempfile::tempdir().expect("home");
        let mut h = boot(home.path(), "kanban-menu").await;

        // Open the Kanban board, then render once so the board layout (the mouse
        // hit-geometry) is recorded for the queued card.
        send_key(&mut h.host_write, KeyCode::Char { ch: 'K' }).await;
        relay_one_send_or_render(
            &mut h.host_write,
            &mut h.host_read,
            &mut h.daemon_reader,
            &mut h.daemon_write,
            &h.stream_id,
        )
        .await;

        // Right-click inside the queued task's card (column 0, first card body).
        // The board paints from row 1; the first card body sits at rows 4..8 in
        // column 0 (x≈1..30), so (2, 4) lands inside it.
        send_mouse(
            &mut h.host_write,
            MouseKind::Down {
                button: MouseButton::Right,
            },
            2,
            4,
        )
        .await;
        // Render so the right-click intent drains and opens the context menu.
        relay_one_send_or_render(
            &mut h.host_write,
            &mut h.host_read,
            &mut h.daemon_reader,
            &mut h.daemon_write,
            &h.stream_id,
        )
        .await;

        // Left-click the `Run now` menu row. The menu anchors at (2, 4): its box
        // top is row 4, the title row, then Open (row 6), Run now (row 7). The row
        // is clickable across the inner box width (x 3..21), so (5, 7) hits it.
        send_mouse(
            &mut h.host_write,
            MouseKind::Down {
                button: MouseButton::Left,
            },
            5,
            7,
        )
        .await;

        let fired = pump_until(&mut h, 40, saw_run_now).await;
        assert!(
            fired,
            "right-click → Run now must issue hangar/task_transition(to_status=running); saw: {:?}",
            h.seen.lock().unwrap()
        );

        drop(h.host_write);
        h.server.abort();
    };
    tokio::time::timeout(BUDGET, body)
        .await
        .expect("exceeded kanban mouse-menu budget");
}

/// MUTATION GUARD: a plain LEFT-click on the card opens it (no menu, no Run-now),
/// so it must NOT issue the task transition — proving the transition is driven by
/// the right-click → Run-now path, not by any click on the card.
#[tokio::test]
async fn kanban_left_click_card_does_not_transition() {
    let body = async {
        let home = tempfile::tempdir().expect("home");
        let mut h = boot(home.path(), "kanban-open").await;

        send_key(&mut h.host_write, KeyCode::Char { ch: 'K' }).await;
        relay_one_send_or_render(
            &mut h.host_write,
            &mut h.host_read,
            &mut h.daemon_reader,
            &mut h.daemon_write,
            &h.stream_id,
        )
        .await;

        // A left press then release on the card = a click that OPENS the task.
        send_mouse(
            &mut h.host_write,
            MouseKind::Down {
                button: MouseButton::Left,
            },
            2,
            4,
        )
        .await;
        send_mouse(
            &mut h.host_write,
            MouseKind::Up {
                button: MouseButton::Left,
            },
            2,
            4,
        )
        .await;

        // Pump several renders; the transition must NEVER appear.
        let transitioned = pump_until(&mut h, 20, saw_run_now).await;
        assert!(
            !transitioned,
            "a left-click open must not issue a task transition; saw: {:?}",
            h.seen.lock().unwrap()
        );

        drop(h.host_write);
        h.server.abort();
    };
    tokio::time::timeout(BUDGET, body)
        .await
        .expect("exceeded kanban left-click budget");
}

/// A synthetic RIGHT-click on the seeded autopilot's card, then a LEFT-click on
/// the `Run now` context-menu row, must issue `hangar/autopilot_fire_now` for that
/// autopilot — the EXISTING autopilot RPC seam, fired by a mouse gesture.
#[tokio::test]
async fn autopilot_right_click_run_now_fires_autopilot() {
    let body = async {
        let home = tempfile::tempdir().expect("home");
        let mut h = boot(home.path(), "ap-menu").await;

        // Open the Autopilots screen (`4`), then render so the board layout is
        // recorded for the autopilot card.
        send_key(&mut h.host_write, KeyCode::Char { ch: '4' }).await;
        relay_one_send_or_render(
            &mut h.host_write,
            &mut h.host_read,
            &mut h.daemon_reader,
            &mut h.daemon_write,
            &h.stream_id,
        )
        .await;

        // Right-click inside the autopilot card. The board paints from body_top=2;
        // the first card body sits at rows 5..9, so (2, 6) lands inside it.
        send_mouse(
            &mut h.host_write,
            MouseKind::Down {
                button: MouseButton::Right,
            },
            2,
            6,
        )
        .await;
        relay_one_send_or_render(
            &mut h.host_write,
            &mut h.host_read,
            &mut h.daemon_reader,
            &mut h.daemon_write,
            &h.stream_id,
        )
        .await;

        // Left-click the `Run now` row. The menu anchors at (2, 6): box top row 6,
        // title row 6, Open (row 8), Run now (row 9). Click inside that row.
        send_mouse(
            &mut h.host_write,
            MouseKind::Down {
                button: MouseButton::Left,
            },
            5,
            9,
        )
        .await;

        let fired = pump_until(&mut h, 40, saw_fire_now).await;
        assert!(
            fired,
            "right-click → Run now must issue hangar/autopilot_fire_now; saw: {:?}",
            h.seen.lock().unwrap()
        );

        drop(h.host_write);
        h.server.abort();
    };
    tokio::time::timeout(BUDGET, body)
        .await
        .expect("exceeded autopilot mouse-menu budget");
}
