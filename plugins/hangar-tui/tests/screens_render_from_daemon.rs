//! Integration (P4.10): the real plugin renders **live screen rows** pulled from
//! a daemon over the cap-mediated socket, and routes keys to the active screen.
//!
//! This is the in-crate proof that GAP1 (plugin `render` painted only chrome) is
//! closed. It drives the **real** [`HangarPlugin`] behind the **real** SDK
//! [`Server`], plays the host (relaying the plugin's reverse `unix_socket_*`
//! calls to a mock daemon that answers `workspace/subscribe` + the four
//! `hangar/*` snapshot RPCs with a seeded `Refactor API` issue + `commit` skill),
//! pumps the daemon replies back as socket events, then:
//!
//!   * asserts the issue-list landing screen renders the seeded `Refactor API`
//!     issue (POSITIVE) and is not stuck on a `Loading`/empty state (NEGATIVE);
//!   * sends the `4` tab key and asserts the skill manager renders `commit`,
//!     proving `plugin/handle_key` routes to the active screen reducer.

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

/// Read one Content-Length frame body, decoding the JSON. `None` on EOF.
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

/// A mock daemon answering subscribe + the four `hangar/*` snapshots with a
/// fixed, seed-shaped result so the plugin's screens have real rows to render.
fn spawn_seeded_daemon(listener: UnixListener) {
    spawn_recording_daemon(
        listener,
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
    );
}

/// A mock daemon that, in addition to answering every request, records each
/// received method name into the shared `seen` vec — so a test can assert the
/// plugin issued a specific RPC (P6.5 `hangar/skills_sync`).
fn spawn_recording_daemon(
    listener: UnixListener,
    seen: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
) {
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
            seen.lock().unwrap().push(req.method.clone());
            let result = result_for(&req.method);
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

/// The seeded result for each method.
fn result_for(method: &str) -> serde_json::Value {
    match method {
        m if m == daemon_methods::WORKSPACE_SUBSCRIBE => serde_json::json!({ "snapshot": {} }),
        m if m == daemon_methods::HANGAR_ISSUES_LIST => serde_json::json!({
            "issues": [
                {"id":"issue-1","workspace_id":"default","title":"Refactor API",
                 "description":null,"state":"open","assignee":"agent:agent-1",
                 "creator":"member:user-1","created_at":0},
                {"id":"issue-2","workspace_id":"default","title":"Fix flaky test",
                 "description":null,"state":"open","assignee":null,
                 "creator":"member:user-1","created_at":1},
                {"id":"issue-3","workspace_id":"default","title":"Add metrics",
                 "description":null,"state":"open","assignee":null,
                 "creator":"member:user-1","created_at":2}
            ]
        }),
        m if m == daemon_methods::HANGAR_AGENTS_LIST => serde_json::json!({
            "actors": [
                {"actor_ref":"agent:agent-1","display_name":"claude-agent",
                 "subtitle":"agent","presence":"online","is_agent":true,"recent_rank":null}
            ]
        }),
        m if m == daemon_methods::HANGAR_SKILLS_LIST => serde_json::json!({
            "skills": [
                {"slug":"skill-commit","name":"commit","used":true,"updated_at":0}
            ]
        }),
        m if m == daemon_methods::HANGAR_HEALTH => serde_json::json!({
            "socket_path":"/tmp/h.sock","pid":1,"uptime_secs":1,"version":"0.1.0","connected":true
        }),
        m if m == daemon_methods::HANGAR_SKILLS_SYNC => serde_json::json!({
            "imported": ["commit","find-missing-tests","brainstorm"], "count": 3
        }),
        _ => serde_json::json!({}),
    }
}

fn host_frame(body: &serde_json::Value) -> Vec<u8> {
    framing::encode(&serde_json::to_vec(body).unwrap())
}

/// Flatten the painted buffer cells into a single string for substring asserts.
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

/// Render until the buffer contains `want`, returning the last buffer seen.
async fn poll_render_for<W, R>(host_write: &mut W, host_read: &mut R, want: &str) -> (bool, String)
where
    W: tokio::io::AsyncWrite + Unpin,
    R: tokio::io::AsyncBufRead + Unpin,
{
    let mut last = String::new();
    for _ in 0..60 {
        host_write
            .write_all(&host_frame(&serde_json::json!({
                "jsonrpc": "2.0", "id": 99, "method": methods::PLUGIN_RENDER,
                "params": { "viewport": {"width": 120, "height": 40}, "generation": 0 }
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

/// Drive init + dial, relay subscribe to the daemon, and return the live daemon
/// connection + the host stream id.
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
                // The very first send is the subscribe; return so the caller can
                // pump the ack + arm the snapshot fetch.
                return daemon_conn.expect("dialed");
            }
            _ => {}
        }
    }
}

/// Push one daemon reply frame to the plugin as a socket Data event.
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

/// Drain any pending reverse `unix_socket_send` calls from the plugin (the
/// snapshot fetch), relay them to the daemon, and pump each reply back as a
/// socket event. Runs until no send is pending within a short grace window.
async fn pump_snapshots<W, R>(
    host_write: &mut W,
    host_read: &mut R,
    daemon_reader: &mut (impl tokio::io::AsyncBufRead + Unpin),
    daemon_write: &mut (impl tokio::io::AsyncWrite + Unpin),
    stream_id: &str,
) where
    W: tokio::io::AsyncWrite + Unpin,
    R: tokio::io::AsyncBufRead + Unpin,
{
    // The plugin issues 4 snapshot requests on the subscribe ack. Relay each and
    // pump its reply back. A single persistent `daemon_reader` is threaded
    // through so a `BufReader` that over-reads past one frame doesn't drop the
    // next reply. Only successful relays count toward the 4 — an interleaved
    // `host/log` notification must NOT consume a slot (that would skip a real
    // snapshot send and leave a screen empty).
    let mut relayed = 0;
    while relayed < 4 {
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

/// Read one raw Content-Length frame (header + body) verbatim for relay.
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

/// Send a single key to the plugin.
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

#[tokio::test]
async fn issue_list_renders_seeded_rows_then_tab_to_skills() {
    let body = async {
        let home = tempfile::tempdir().expect("home");
        // P5.6: this test asserts issue-list rendering, not the first-run
        // danger-full-access modal. Point the plugin's `state.toml` resolution at
        // this isolated home (hermetic — no real `~/.ainb`) and pre-seed the
        // `first_run` ack so the modal is deterministically skipped. Single test
        // per binary, so the process-wide env set is race-free here.
        std::env::set_var("AINB_HANGAR_HOME", home.path());
        let state = home.path().join("hangar").join("state.toml");
        std::fs::create_dir_all(state.parent().unwrap()).expect("state dir");
        std::fs::write(&state, "warnings_ack = [\"first_run\"]\n").expect("seed ack");

        let socket_path = home.path().join("hangar.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind daemon");
        spawn_seeded_daemon(listener);

        let (host_side, plugin_side) = tokio::io::duplex(256 * 1024);
        let (plugin_read, plugin_write) = tokio::io::split(plugin_side);
        let server = tokio::spawn(Server::new(HangarPlugin::new()).run(plugin_read, plugin_write));

        let (host_read_half, mut host_write) = tokio::io::split(host_side);
        let mut host_read = BufReader::new(host_read_half);

        let stream_id = format!("sock-{}", std::process::id());
        let daemon = init_and_dial(&mut host_write, &mut host_read, &socket_path, &stream_id).await;
        // One persistent reader over the daemon connection so over-reads don't
        // drop later snapshot replies.
        let (daemon_read, mut daemon_write) = daemon.into_split();
        let mut daemon_reader = BufReader::new(daemon_read);

        // Subscribe ack → arms snapshot fetch.
        let ack = read_one_raw_frame(&mut daemon_reader)
            .await
            .expect("subscribe ack");
        push_data(&mut host_write, &stream_id, &ack).await;

        // Pump the four snapshot requests + replies.
        pump_snapshots(
            &mut host_write,
            &mut host_read,
            &mut daemon_reader,
            &mut daemon_write,
            &stream_id,
        )
        .await;

        // POSITIVE: the seeded issue renders. NEGATIVE: not stuck loading/empty.
        let (rendered, last) =
            poll_render_for(&mut host_write, &mut host_read, "Refactor API").await;
        assert!(rendered, "issue list never rendered seeded rows:\n{last}");
        assert!(!last.contains("Loading"), "stuck on loading:\n{last}");
        assert!(last.contains("Todo"), "Todo column header missing:\n{last}");

        // Route the `3` tab key → skill manager renders `commit` (e38.38: Skills
        // renumbered 4→3 to close the `[3]Agents` gap).
        send_key(&mut host_write, '3').await;
        let (skills, last2) = poll_render_for(&mut host_write, &mut host_read, "commit").await;
        assert!(skills, "tab to skills did not render `commit`:\n{last2}");
        assert!(
            !last2.contains("Refactor API"),
            "issue list bled into skills:\n{last2}"
        );

        drop(host_write);
        server.abort();
    };

    tokio::time::timeout(BUDGET, body)
        .await
        .expect("exceeded e2e budget");
}

/// Relay one reverse `unix_socket_send` (the plugin firing a daemon RPC) to the
/// daemon and pump its reply back — or drive one render. Returns `true` once a
/// send was relayed. Reads exactly one host frame.
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
    // Nudge a render so the plugin drains its pending skill action.
    host_write
        .write_all(&host_frame(&serde_json::json!({
            "jsonrpc": "2.0", "id": 99, "method": methods::PLUGIN_RENDER,
            "params": { "viewport": {"width": 120, "height": 40}, "generation": 0 }
        })))
        .await
        .unwrap();
    // Read frames until we either relay a send or see the render ack.
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
            return false; // render ack, no send this round
        }
    }
}

#[tokio::test]
async fn skill_screen_s_invokes_skills_sync_rpc() {
    let body = async {
        let home = tempfile::tempdir().expect("home");
        std::env::set_var("AINB_HANGAR_HOME", home.path());
        let state = home.path().join("hangar").join("state.toml");
        std::fs::create_dir_all(state.parent().unwrap()).expect("state dir");
        std::fs::write(&state, "warnings_ack = [\"first_run\"]\n").expect("seed ack");

        let socket_path = home.path().join("hangar.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind daemon");
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        spawn_recording_daemon(listener, seen.clone());

        let (host_side, plugin_side) = tokio::io::duplex(256 * 1024);
        let (plugin_read, plugin_write) = tokio::io::split(plugin_side);
        let server = tokio::spawn(Server::new(HangarPlugin::new()).run(plugin_read, plugin_write));

        let (host_read_half, mut host_write) = tokio::io::split(host_side);
        let mut host_read = BufReader::new(host_read_half);

        let stream_id = format!("sock-sync-{}", std::process::id());
        let daemon = init_and_dial(&mut host_write, &mut host_read, &socket_path, &stream_id).await;
        let (daemon_read, mut daemon_write) = daemon.into_split();
        let mut daemon_reader = BufReader::new(daemon_read);

        // Subscribe ack → arms snapshot fetch.
        let ack = read_one_raw_frame(&mut daemon_reader)
            .await
            .expect("subscribe ack");
        push_data(&mut host_write, &stream_id, &ack).await;
        pump_snapshots(
            &mut host_write,
            &mut host_read,
            &mut daemon_reader,
            &mut daemon_write,
            &stream_id,
        )
        .await;

        // Tab to the skill manager (`3`), then press `s` (sync). (e38.38: Skills
        // renumbered 4→3 to close the `[3]Agents` gap.)
        send_key(&mut host_write, '3').await;
        send_key(&mut host_write, 's').await;

        // Pump renders until the plugin's deferred skill action fires the sync
        // RPC and the daemon records it (bounded).
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
                .any(|m| m == daemon_methods::HANGAR_SKILLS_SYNC)
            {
                sent = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            sent,
            "pressing `s` on the skill screen must issue a `hangar/skills_sync` RPC; saw: {:?}",
            seen.lock().unwrap()
        );

        drop(host_write);
        server.abort();
    };

    tokio::time::timeout(BUDGET, body)
        .await
        .expect("exceeded sync-rpc budget");
}
