//! P7.5 — prove the autopilot action keys issue the right daemon RPCs.
//!
//! Drives the **real** [`HangarPlugin`] behind the **real** SDK [`Server`], plays
//! the host relaying the plugin's reverse `unix_socket_*` calls to a mock daemon
//! that records each received method + params. After tabbing to the autopilot
//! screen (`5`):
//!
//!   * pressing `r` issues a `hangar/autopilot_fire_now` for the seeded
//!     autopilot (`key_r_fires_now`);
//!   * pressing `d` issues a `hangar/autopilot_set_enabled` with `enabled:false`
//!     (the seeded autopilot is enabled), then pressing `d` again (after the
//!     daemon flips the row) issues `enabled:true` (`key_d_toggles_enabled`).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ainb_hangar_proto::{methods as daemon_methods, RpcRequest, RpcResponse};
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

/// A mock daemon that records `(method, params)` and answers every request with a
/// seed-shaped result so the plugin's screens have rows to render. The autopilot
/// list reflects a shared `enabled` flag so a `set_enabled` toggle is observable
/// on the next list fetch.
fn spawn_daemon(listener: UnixListener, seen: Seen, enabled: Arc<Mutex<bool>>) {
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
            seen.lock()
                .unwrap()
                .push((req.method.clone(), req.params.clone()));
            // A set_enabled call flips the shared flag so the next list reflects it.
            if req.method == daemon_methods::HANGAR_AUTOPILOT_SET_ENABLED {
                if let Some(e) = req
                    .params
                    .get("enabled")
                    .and_then(serde_json::Value::as_bool)
                {
                    *enabled.lock().unwrap() = e;
                }
            }
            let result = result_for(&req.method, *enabled.lock().unwrap());
            let resp = RpcResponse {
                jsonrpc: "2.0".into(),
                id: req.id,
                result: Some(result),
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

fn result_for(method: &str, enabled: bool) -> serde_json::Value {
    match method {
        m if m == daemon_methods::WORKSPACE_SUBSCRIBE => serde_json::json!({ "snapshot": {} }),
        m if m == daemon_methods::HANGAR_ISSUES_LIST => serde_json::json!({ "issues": [] }),
        m if m == daemon_methods::HANGAR_AGENTS_LIST => serde_json::json!({ "actors": [] }),
        m if m == daemon_methods::HANGAR_SKILLS_LIST => serde_json::json!({ "skills": [] }),
        m if m == daemon_methods::HANGAR_AUTOPILOTS_LIST => serde_json::json!({
            "autopilots": [
                {"id":"ap-1","workspace_id":"default","agent_id":"agent-1",
                 "name":"daily-triage","cron_expr":"0 9 * * 1-5",
                 "next_tick_at":1_700_000_300_000_i64,"enabled":enabled,
                 "last_run_status":"completed","last_run_at":1_699_000_000_000_i64}
            ]
        }),
        m if m == daemon_methods::HANGAR_AUTOPILOT_RUNS => serde_json::json!({ "runs": [] }),
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

/// Relay the 5 subscribe-snapshot sends + replies (issues / agents / skills /
/// autopilots / health).
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
    while relayed < 5 {
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

/// Drive one render so the plugin drains its pending autopilot action; relay any
/// reverse send to the daemon + pump the reply back. Returns `true` if a send was
/// relayed.
async fn relay_one_send_or_render<W, R, DR, DW>(
    host_write: &mut W,
    host_read: &mut R,
    daemon_reader: &mut DR,
    daemon_write: &mut DW,
    stream_id: &str,
) -> bool
where
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
            return false;
        };
        if frame.get("method").and_then(|m| m.as_str()) == Some(methods::HOST_UNIX_SOCKET_SEND) {
            let send: UnixSocketSendParams =
                serde_json::from_value(frame["params"].clone()).unwrap();
            daemon_write.write_all(&send.bytes).await.unwrap();
            daemon_write.flush().await.unwrap();
            if let Some(reply) = read_one_raw_frame(daemon_reader).await {
                push_data(host_write, stream_id, &reply).await;
            }
            return true;
        }
        if frame.get("id").and_then(serde_json::Value::as_i64) == Some(99) {
            return false;
        }
    }
}

async fn send_key<W: tokio::io::AsyncWrite + Unpin>(host_write: &mut W, ch: char) {
    let key = KeyEvent {
        code: KeyCode::Char { ch },
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

/// Boot the plugin + mock daemon, run subscribe + snapshot pump, and return the
/// live host/daemon handles plus the shared `seen` recorder.
async fn boot(
    home: &std::path::Path,
    stream_id: &str,
    seen: Seen,
    enabled: Arc<Mutex<bool>>,
) -> (
    impl tokio::io::AsyncWrite + Unpin,
    impl tokio::io::AsyncBufRead + Unpin,
    BufReader<tokio::net::unix::OwnedReadHalf>,
    tokio::net::unix::OwnedWriteHalf,
    tokio::task::JoinHandle<Result<(), ainb_plugin_sdk::SdkError>>,
) {
    let state = home.join("hangar").join("state.toml");
    std::fs::create_dir_all(state.parent().unwrap()).expect("state dir");
    std::fs::write(&state, "warnings_ack = [\"first_run\"]\n").expect("seed ack");

    let socket_path = home.join("hangar.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind daemon");
    spawn_daemon(listener, seen, enabled);

    let (host_side, plugin_side) = tokio::io::duplex(256 * 1024);
    let (plugin_read, plugin_write) = tokio::io::split(plugin_side);
    let server = tokio::spawn(Server::new(HangarPlugin::new()).run(plugin_read, plugin_write));

    let (host_read_half, mut host_write) = tokio::io::split(host_side);
    let mut host_read = BufReader::new(host_read_half);

    let daemon = init_and_dial(&mut host_write, &mut host_read, &socket_path, stream_id).await;
    let (daemon_read, mut daemon_write) = daemon.into_split();
    let mut daemon_reader = BufReader::new(daemon_read);

    let ack = read_one_raw_frame(&mut daemon_reader)
        .await
        .expect("subscribe ack");
    push_data(&mut host_write, stream_id, &ack).await;
    pump_snapshots(
        &mut host_write,
        &mut host_read,
        &mut daemon_reader,
        &mut daemon_write,
        stream_id,
    )
    .await;

    (host_write, host_read, daemon_reader, daemon_write, server)
}

#[tokio::test]
async fn key_r_fires_now() {
    let body = async {
        let home = tempfile::tempdir().expect("home");
        std::env::set_var("AINB_HANGAR_HOME", home.path());
        let stream_id = format!("sock-fire-{}", std::process::id());
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let enabled = Arc::new(Mutex::new(true));
        let (mut host_write, mut host_read, mut daemon_reader, mut daemon_write, server) =
            boot(home.path(), &stream_id, seen.clone(), enabled).await;

        // Tab to the autopilot screen (`4`), then press `r` (run now). (e38.38:
        // Autopilots renumbered 5→4 to close the `[3]Agents` gap.)
        send_key(&mut host_write, '4').await;
        send_key(&mut host_write, 'r').await;

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
            if seen
                .lock()
                .unwrap()
                .iter()
                .any(|(m, _)| m == daemon_methods::HANGAR_AUTOPILOT_FIRE_NOW)
            {
                sent = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            sent,
            "pressing `r` must issue a `hangar/autopilot_fire_now` RPC; saw: {:?}",
            seen.lock().unwrap()
        );

        drop(host_write);
        server.abort();
    };
    tokio::time::timeout(BUDGET, body)
        .await
        .expect("exceeded fire-now budget");
}

#[tokio::test]
async fn key_d_toggles_enabled() {
    let body = async {
        let home = tempfile::tempdir().expect("home");
        std::env::set_var("AINB_HANGAR_HOME", home.path());
        let stream_id = format!("sock-toggle-{}", std::process::id());
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let enabled = Arc::new(Mutex::new(true));
        let (mut host_write, mut host_read, mut daemon_reader, mut daemon_write, server) =
            boot(home.path(), &stream_id, seen.clone(), enabled).await;

        // Tab to the autopilot screen (`4`), then press `d` (disable; the seeded
        // row is enabled → set_enabled(false)). (e38.38: Autopilots renumbered 5→4
        // to close the `[3]Agents` gap.)
        send_key(&mut host_write, '4').await;
        send_key(&mut host_write, 'd').await;

        // Pump until the daemon records set_enabled(false). The daemon flips its
        // shared `enabled` flag + the post-mutation list re-fetch refreshes the
        // plugin's row to disabled.
        let saw_false = pump_until_set_enabled(
            &mut host_write,
            &mut host_read,
            &mut daemon_reader,
            &mut daemon_write,
            &stream_id,
            &seen,
            Some(false),
        )
        .await;
        assert!(
            saw_false,
            "first `d` must issue set_enabled(false); saw: {:?}",
            seen.lock().unwrap()
        );

        // Let the post-mutation re-fetch (autopilots_list, now enabled:false)
        // land + fold into the plugin's row before the next keypress, so the
        // second `d` reads the row as disabled.
        let mut refetched = false;
        for _ in 0..30 {
            relay_one_send_or_render(
                &mut host_write,
                &mut host_read,
                &mut daemon_reader,
                &mut daemon_write,
                &stream_id,
            )
            .await;
            // The re-fetch is the second autopilots_list AFTER the set_enabled.
            let after_toggle = {
                let snapshot: Vec<String> = seen
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|(m, _)| m.clone())
                    .collect();
                snapshot
                    .iter()
                    .position(|m| m == daemon_methods::HANGAR_AUTOPILOT_SET_ENABLED)
                    .is_some_and(|i| {
                        snapshot[i + 1..]
                            .iter()
                            .any(|m| m == daemon_methods::HANGAR_AUTOPILOTS_LIST)
                    })
            };
            if after_toggle {
                refetched = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            refetched,
            "post-toggle autopilots_list re-fetch never fired"
        );

        // Settle: a few extra render pumps so the re-fetch reply is delivered as a
        // socket event AND folded into the plugin's row before the next keypress.
        for _ in 0..5 {
            relay_one_send_or_render(
                &mut host_write,
                &mut host_read,
                &mut daemon_reader,
                &mut daemon_write,
                &stream_id,
            )
            .await;
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        // Press `d` again: the row is now disabled, so this requests enable(true).
        send_key(&mut host_write, 'd').await;
        let saw_true = pump_until_set_enabled(
            &mut host_write,
            &mut host_read,
            &mut daemon_reader,
            &mut daemon_write,
            &stream_id,
            &seen,
            Some(true),
        )
        .await;
        assert!(
            saw_true,
            "second `d` must issue set_enabled(true); saw: {:?}",
            seen.lock().unwrap()
        );

        drop(host_write);
        server.abort();
    };
    tokio::time::timeout(BUDGET, body)
        .await
        .expect("exceeded toggle budget");
}

/// Pump renders relaying sends until the daemon records a `set_enabled` with the
/// given `enabled` flag.
#[allow(clippy::too_many_arguments)]
async fn pump_until_set_enabled<W, R, DR, DW>(
    host_write: &mut W,
    host_read: &mut R,
    daemon_reader: &mut DR,
    daemon_write: &mut DW,
    stream_id: &str,
    seen: &Seen,
    want_enabled: Option<bool>,
) -> bool
where
    W: tokio::io::AsyncWrite + Unpin,
    R: tokio::io::AsyncBufRead + Unpin,
    DR: tokio::io::AsyncBufRead + Unpin,
    DW: tokio::io::AsyncWrite + Unpin,
{
    for _ in 0..60 {
        relay_one_send_or_render(
            host_write,
            host_read,
            daemon_reader,
            daemon_write,
            stream_id,
        )
        .await;
        let hit = seen.lock().unwrap().iter().any(|(m, p)| {
            m == daemon_methods::HANGAR_AUTOPILOT_SET_ENABLED
                && p.get("enabled").and_then(serde_json::Value::as_bool) == want_enabled
        });
        if hit {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}
