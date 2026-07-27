//! Tripwire (multica-gap #10): the `f` faceted-filter panel end-to-end over real
//! wire bytes.
//!
//! Drives the REAL [`HangarPlugin`] behind the REAL SDK [`Server`], playing the
//! host: it relays the plugin's reverse `unix_socket_*` calls to a mock daemon
//! that answers `hangar/issues_list` with the discriminating 5-row matrix (one
//! `todo + P0 + bug` survivor + four decoys), then sends genuine `plugin/handle_key`
//! frames and captures the `plugin/render` buffer. It proves the user-visible
//! acceptance journey the gap asks for — "open the facet panel, select a status
//! AND a priority AND a label, the board shrinks to the matching row, the column
//! counts update, and the panel shows per-value counts":
//!
//!   * [`facet_panel_opens_and_shows_per_value_counts`] — pressing `f` renders the
//!     `⛃ Filters` panel with the EXACT drill-down counts (`bug (3)`, `chore (1)`,
//!     `P0 (3)`). A pre-press negative confirms the panel is not on the board.
//!   * [`three_facet_and_narrows_board_and_counts`] — toggling status=Todo,
//!     priority=P0, label=bug narrows the board to the SOLE `TARGET` card (every
//!     `DECOY` gone), the Todo column shows `(1)` and In Progress `(0)`, and the
//!     closed-panel summary chip reads `status:todo`.
//!
//! Hermetic: no tmux, no staged binary (so no macOS AMFI SIGKILL / first-run
//! wizard flake); every wire byte is a genuine proto envelope. Follows the
//! `tmux-ui-tripwire` skill's EXACT-substring (never substring-OR) discipline.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ainb_hangar_core::ids::IssueId;
use ainb_hangar_proto::events::IssueRow;
use ainb_hangar_proto::snapshots::{AgentsListResult, IssuesListResult};
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

const BUDGET: Duration = Duration::from_secs(30);

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
            let mut framed = header;
            framed.extend_from_slice(&body);
            return Some(framed);
        }
        if let Some((n, v)) = trimmed.split_once(':') {
            if n.trim().eq_ignore_ascii_case("Content-Length") {
                content_length = v.trim().parse().ok();
            }
        }
    }
}

/// One wire row for the matrix. `priority` is the wire scalar (`3` = P0, `1` = P2,
/// `2` = P1); `state` drives the board column; `labels` drive the label facet.
fn row(id: &str, title: &str, state: &str, priority: i64, labels: &[&str]) -> IssueRow {
    IssueRow {
        subscriber_count: 0,
        subscribed: false,
        reactions: Vec::new(),
        properties: Vec::new(),
        metadata: Vec::new(),
        last_dispatch_reason: None,
        last_dispatch_detail: None,
        last_dispatch_at: None,
        origin_type: None,
        origin_id: None,
        id: IssueId::from_str(id).unwrap(),
        display_id: Some(id.to_uppercase()),
        workspace_id: "default".into(),
        title: title.into(),
        description: None,
        state: state.into(),
        assignee: Some("agent:claude".into()),
        creator: "member:user-1".into(),
        created_at: 0,
        priority,
        due_date: None,
        labels: labels.iter().map(|l| (*l).to_string()).collect(),
        pr_url: None,
        branch: None,
        repo_ref: None,
        agent: None,
        source_branch: None,
        target_branch: None,
        external_ref: None,
        run_count: 0,
        last_run_status: None,
        last_run_at: None,
        parent_id: None,
        child_total: 0,
        child_done: 0,
        acceptance_criteria: Vec::new(),
        acceptance: Vec::new(),
        context_refs: Vec::new(),
        dependencies: Vec::new(),
    }
}

/// The discriminating gap-10 matrix: exactly ONE `todo + P0 + bug` survivor.
fn seeded_issues() -> serde_json::Value {
    serde_json::to_value(IssuesListResult {
        issues: vec![
            row("target", "TARGET todo-p0-bug", "todo", 3, &["bug"]),
            row("d_nolbl", "DECOY todo-p0-nolbl", "todo", 3, &[]),
            row("d_p2bug", "DECOY todo-p2-bug", "todo", 1, &["bug"]),
            row("d_progbug", "DECOY prog-p0-bug", "in_progress", 3, &["bug"]),
            row("d_chore", "DECOY done-p1-chore", "done", 2, &["chore"]),
        ],
    })
    .unwrap()
}

/// No named agents — the issue board only needs the issues snapshot.
fn seeded_agents() -> serde_json::Value {
    serde_json::to_value(AgentsListResult { actors: Vec::new() }).unwrap()
}

/// A mock daemon that records `(method, params)` and answers each request with a
/// seed-shaped result built from the real proto types.
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

fn result_for(method: &str) -> serde_json::Value {
    match method {
        m if m == daemon_methods::HANGAR_ISSUES_LIST => seeded_issues(),
        m if m == daemon_methods::HANGAR_AGENTS_LIST => seeded_agents(),
        m if m == daemon_methods::HANGAR_HEALTH => serde_json::json!({
            "socket_path":"/tmp/h.sock","pid":1,"uptime_secs":1,"version":"0.1.0","connected":true
        }),
        m if m == daemon_methods::WORKSPACE_SUBSCRIBE
            || m == daemon_methods::ATTENTION_SUBSCRIBE =>
        {
            serde_json::json!({ "snapshot": {} })
        }
        _ => serde_json::json!({}),
    }
}

fn host_frame(body: &serde_json::Value) -> Vec<u8> {
    framing::encode(&serde_json::to_vec(body).unwrap())
}

/// Flatten every painted cell symbol from a `plugin/render` result frame.
fn render_text(render_resp: &serde_json::Value) -> String {
    render_resp["result"]["buffer"]["cells"]
        .as_array()
        .map(|cells| {
            cells.iter().map(|c| c[1]["symbol"].as_str().unwrap_or("")).collect::<String>()
        })
        .unwrap_or_default()
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

/// Send one `plugin/render`, relaying any reverse send to the daemon (and pushing
/// its reply), and return the rendered pane text once the id-99 result arrives.
async fn render_capture<W, R, DR, DW>(
    host_write: &mut W,
    host_read: &mut R,
    daemon_reader: &mut DR,
    daemon_write: &mut DW,
    stream_id: &str,
) -> String
where
    W: tokio::io::AsyncWrite + Unpin,
    R: tokio::io::AsyncBufRead + Unpin,
    DR: tokio::io::AsyncBufRead + Unpin,
    DW: tokio::io::AsyncWrite + Unpin,
{
    host_write
        .write_all(&host_frame(&serde_json::json!({
            "jsonrpc": "2.0", "id": 99, "method": methods::PLUGIN_RENDER,
            "params": { "viewport": {"width": 180, "height": 44}, "generation": 0 }
        })))
        .await
        .unwrap();
    loop {
        let Some(frame) = read_frame(host_read).await else {
            return String::new();
        };
        if frame.get("method").and_then(|m| m.as_str()) == Some(methods::HOST_UNIX_SOCKET_SEND) {
            let send: UnixSocketSendParams =
                serde_json::from_value(frame["params"].clone()).unwrap();
            daemon_write.write_all(&send.bytes).await.unwrap();
            daemon_write.flush().await.unwrap();
            if let Some(reply) = read_one_raw_frame(daemon_reader).await {
                push_data(host_write, stream_id, &reply).await;
            }
            continue;
        }
        if frame.get("id").and_then(serde_json::Value::as_i64) == Some(99) {
            return render_text(&frame);
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

async fn send_char<W: tokio::io::AsyncWrite + Unpin>(host_write: &mut W, ch: char) {
    send_key(host_write, KeyCode::Char { ch }).await;
}

/// Boot the plugin + mock daemon, relay the connect handshake, and pump renders
/// until the issues snapshot has been fetched + folded (the issue board is the
/// landing screen). Returns the live handles + recorder.
#[allow(clippy::type_complexity)]
async fn boot(
    home: &std::path::Path,
    stream_id: &str,
    seen: Seen,
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
    spawn_daemon(listener, seen.clone());

    let (host_side, plugin_side) = tokio::io::duplex(256 * 1024);
    let (plugin_read, plugin_write) = tokio::io::split(plugin_side);
    let server = tokio::spawn(Server::new(HangarPlugin::new()).run(plugin_read, plugin_write));

    let (host_read_half, mut host_write) = tokio::io::split(host_side);
    let mut host_read = BufReader::new(host_read_half);

    let daemon = init_and_dial(&mut host_write, &mut host_read, &socket_path, stream_id).await;
    let (daemon_read, mut daemon_write) = daemon.into_split();
    let mut daemon_reader = BufReader::new(daemon_read);

    // The first handshake reply (auth ack) lands as a socket event.
    if let Some(ack) = read_one_raw_frame(&mut daemon_reader).await {
        push_data(&mut host_write, stream_id, &ack).await;
    }

    // Pump renders until the issues snapshot has been fetched + folded.
    for _ in 0..60 {
        let _ = render_capture(
            &mut host_write,
            &mut host_read,
            &mut daemon_reader,
            &mut daemon_write,
            stream_id,
        )
        .await;
        let have_issues = seen
            .lock()
            .unwrap()
            .iter()
            .any(|(m, _)| m == daemon_methods::HANGAR_ISSUES_LIST);
        if have_issues {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    (host_write, host_read, daemon_reader, daemon_write, server)
}

/// Pump renders until `ok(pane)` holds or the budget of iterations elapses,
/// returning the last captured pane either way.
async fn render_until<W, R, DR, DW, F>(
    hw: &mut W,
    hr: &mut R,
    dr: &mut DR,
    dw: &mut DW,
    stream_id: &str,
    mut ok: F,
) -> String
where
    W: tokio::io::AsyncWrite + Unpin,
    R: tokio::io::AsyncBufRead + Unpin,
    DR: tokio::io::AsyncBufRead + Unpin,
    DW: tokio::io::AsyncWrite + Unpin,
    F: FnMut(&str) -> bool,
{
    let mut pane = String::new();
    for _ in 0..40 {
        pane = render_capture(hw, hr, dr, dw, stream_id).await;
        if ok(&pane) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    pane
}

/// Pressing `f` opens the `⛃ Filters` panel with the EXACT per-value drill-down
/// counts across facets (all facets empty = all-in-scope).
#[tokio::test]
async fn facet_panel_opens_and_shows_per_value_counts() {
    let body = async {
        let home = tempfile::tempdir().expect("home");
        std::env::set_var("AINB_HANGAR_HOME", home.path());
        let stream_id = format!("sock-facets-{}", std::process::id());
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let (mut hw, mut hr, mut dr, mut dw, server) =
            boot(home.path(), &stream_id, seen.clone()).await;

        // PRE-PRESS NEGATIVE: the board (not the panel) is on the landing screen.
        let pre = render_until(&mut hw, &mut hr, &mut dr, &mut dw, &stream_id, |p| {
            p.contains("TARGET")
        })
        .await;
        assert!(pre.contains("TARGET"), "seeded board missing:\n{pre}");
        assert!(
            !pre.contains("Filters"),
            "panel must be closed initially:\n{pre}"
        );

        // Open the panel and pump until it renders.
        send_char(&mut hw, 'f').await;
        let panel = render_until(&mut hw, &mut hr, &mut dr, &mut dw, &stream_id, |p| {
            p.contains("Filters") && p.contains("bug (3)")
        })
        .await;

        // EXACT per-value drill-down counts (never substring-OR).
        assert!(panel.contains("Filters"), "panel title missing:\n{panel}");
        assert!(panel.contains("bug (3)"), "label bug (3) missing:\n{panel}");
        assert!(
            panel.contains("chore (1)"),
            "label chore (1) missing:\n{panel}"
        );
        assert!(
            panel.contains("P0 (3)"),
            "priority P0 (3) missing:\n{panel}"
        );

        drop(hw);
        server.abort();
    };
    tokio::time::timeout(BUDGET, body).await.expect("exceeded facet-panel budget");
}

/// Toggling status=Todo AND priority=P0 AND label=bug narrows the board to the
/// SOLE `TARGET` card, the Todo column shows `(1)` / In Progress `(0)`, and the
/// closed-panel summary chip reads `status:todo`.
#[tokio::test]
async fn three_facet_and_narrows_board_and_counts() {
    let body = async {
        let home = tempfile::tempdir().expect("home");
        std::env::set_var("AINB_HANGAR_HOME", home.path());
        let stream_id = format!("sock-facand-{}", std::process::id());
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let (mut hw, mut hr, mut dr, mut dw, server) =
            boot(home.path(), &stream_id, seen.clone()).await;

        // Open the panel.
        send_char(&mut hw, 'f').await;
        let _ = render_until(&mut hw, &mut hr, &mut dr, &mut dw, &stream_id, |p| {
            p.contains("Filters")
        })
        .await;

        // Status section: toggle Todo (first present value under the cursor).
        send_char(&mut hw, ' ').await;
        // → Priority section, toggle P0 (first, most-urgent).
        send_key(&mut hw, KeyCode::Right).await;
        send_char(&mut hw, ' ').await;
        // → Label section, toggle bug (the only in-scope label).
        send_key(&mut hw, KeyCode::Right).await;
        send_char(&mut hw, ' ').await;
        // Close the panel — the applied facets REMAIN applied.
        send_key(&mut hw, KeyCode::Esc).await;

        let filtered = render_until(&mut hw, &mut hr, &mut dr, &mut dw, &stream_id, |p| {
            p.contains("TARGET") && !p.contains("DECOY")
        })
        .await;

        // Row set: only the survivor remains; every decoy is gone.
        assert!(
            filtered.contains("TARGET"),
            "TARGET must survive:\n{filtered}"
        );
        assert!(
            !filtered.contains("DECOY"),
            "every DECOY must be filtered out:\n{filtered}"
        );
        // Column-header counts reflect the filtered set.
        assert!(
            filtered.contains("Todo (1)"),
            "Todo (1) missing:\n{filtered}"
        );
        assert!(
            filtered.contains("In Progress (0)"),
            "In Progress (0) missing:\n{filtered}"
        );
        // Fail-visible: the closed-panel summary chip shows the applied status.
        assert!(
            filtered.contains("status:todo"),
            "active-facet summary chip missing:\n{filtered}"
        );

        drop(hw);
        server.abort();
    };
    tokio::time::timeout(BUDGET, body).await.expect("exceeded facet-and budget");
}
