//! Tripwire (multica gap #11-rest): tick ONE acceptance criterion in the REAL
//! task-detail card, over real wire bytes.
//!
//! Drives the REAL [`HangarPlugin`] behind the REAL SDK [`Server`], playing the
//! host: it relays the plugin's reverse `unix_socket_*` calls to a mock daemon
//! that answers `hangar/issues_list` with ONE issue carrying THREE all-unchecked
//! criteria, then sends genuine `plugin/handle_key` frames and captures the
//! `plugin/render` buffer.
//!
//! The journey: Enter on the board opens the task-detail card (`Acceptance: 0/3`,
//! three `☐`), `a a` walks the acceptance cursor to the SECOND criterion, `t`
//! ticks it. The card must then show `☑` on CRITERION-TWO and `☐` on the other
//! two, with every DECOY (`Acceptance: 0/3`, a `☑` on one or three) asserted
//! ABSENT, and the plugin must have fired a real `hangar/issue_criterion_set`
//! naming the SECOND criterion's stable id.
//!
//! Hermetic: no tmux, no staged binary (so no macOS AMFI SIGKILL / first-run
//! wizard flake); every wire byte is a genuine proto envelope. Follows the
//! `tmux-ui-tripwire` skill's EXACT-substring (never substring-OR) discipline.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ainb_hangar_core::acceptance::AcceptanceCriterion;
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

/// One wire row carrying `criteria` as its structured acceptance list.
fn row(id: &str, title: &str, criteria: Vec<AcceptanceCriterion>) -> IssueRow {
    IssueRow {
        id: IssueId::from_str(id).unwrap(),
        display_id: Some(id.to_uppercase()),
        workspace_id: "default".into(),
        title: title.into(),
        description: None,
        state: "todo".into(),
        assignee: Some("agent:claude".into()),
        creator: "member:user-1".into(),
        created_at: 0,
        priority: 3,
        due_date: None,
        labels: Vec::new(),
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
        acceptance_criteria: criteria.iter().map(|c| c.text.clone()).collect(),
        acceptance: criteria,
        context_refs: Vec::new(),
    }
}

/// A fresh unchecked criterion with a deterministic id.
fn crit(id: &str, text: &str) -> AcceptanceCriterion {
    AcceptanceCriterion::with_id(id, text).expect("criterion")
}

/// ONE issue carrying THREE all-unchecked criteria, so ticking exactly one is
/// discriminating: the other two must stay `☐`.
fn seeded_issues() -> serde_json::Value {
    serde_json::to_value(IssuesListResult {
        issues: vec![row(
            "target",
            "TARGET acceptance issue",
            vec![
                crit("ac-one", "CRITERION-ONE"),
                crit("ac-two", "CRITERION-TWO"),
                crit("ac-three", "CRITERION-THREE"),
            ],
        )],
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

/// The painted buffer as one string PER ROW, so a glyph assertion is pinned to
/// the same line as its criterion rather than anywhere on the screen.
fn render_rows(render_resp: &serde_json::Value) -> Vec<String> {
    let cells = render_resp["result"]["buffer"]["cells"].as_array().cloned().unwrap_or_default();
    let mut by_row: std::collections::BTreeMap<i64, Vec<(i64, String)>> =
        std::collections::BTreeMap::new();
    for c in &cells {
        let x = c[0]["x"].as_i64().unwrap_or(0);
        let y = c[0]["y"].as_i64().unwrap_or(0);
        let sym = c[1]["symbol"].as_str().unwrap_or("").to_string();
        by_row.entry(y).or_default().push((x, sym));
    }
    by_row
        .into_values()
        .map(|mut row| {
            row.sort_by_key(|(x, _)| *x);
            row.into_iter().map(|(_, s)| s).collect::<String>()
        })
        .collect()
}

/// Send one `plugin/render` and return the pane BY ROW (same relay contract as
/// [`render_capture`], which returns the flattened pane).
async fn render_capture_rows<W, R, DR, DW>(
    host_write: &mut W,
    host_read: &mut R,
    daemon_reader: &mut DR,
    daemon_write: &mut DW,
    stream_id: &str,
) -> Vec<String>
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
            return Vec::new();
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
            return render_rows(&frame);
        }
    }
}

/// Pump row-wise renders until `ok(rows)` holds or the iteration budget elapses.
async fn render_rows_until<W, R, DR, DW, F>(
    hw: &mut W,
    hr: &mut R,
    dr: &mut DR,
    dw: &mut DW,
    stream_id: &str,
    mut ok: F,
) -> Vec<String>
where
    W: tokio::io::AsyncWrite + Unpin,
    R: tokio::io::AsyncBufRead + Unpin,
    DR: tokio::io::AsyncBufRead + Unpin,
    DW: tokio::io::AsyncWrite + Unpin,
    F: FnMut(&[String]) -> bool,
{
    let mut rows = Vec::new();
    for _ in 0..40 {
        rows = render_capture_rows(hw, hr, dr, dw, stream_id).await;
        if ok(&rows) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    rows
}

/// The single pane row containing `needle`.
fn line_with<'a>(rows: &'a [String], needle: &str) -> &'a str {
    rows.iter()
        .find(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("no pane row contains `{needle}`:\n{}", rows.join("\n")))
}

/// `a a t` on the task-detail card ticks the SECOND criterion and ONLY it.
#[tokio::test]
async fn acceptance_tick_marks_only_the_selected_criterion() {
    let body = async {
        let home = tempfile::tempdir().expect("home");
        std::env::set_var("AINB_HANGAR_HOME", home.path());
        let stream_id = format!("sock-accept-{}", std::process::id());
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let (mut hw, mut hr, mut dr, mut dw, server) =
            boot(home.path(), &stream_id, seen.clone()).await;

        // Board first, then Enter opens the task-detail card.
        let _ = render_rows_until(&mut hw, &mut hr, &mut dr, &mut dw, &stream_id, |r| {
            r.iter().any(|l| l.contains("TARGET acceptance issue"))
        })
        .await;
        send_char(&mut hw, '\r').await;

        // PRE-TICK: three criteria, ALL unchecked, header 0/3.
        let pre = render_rows_until(&mut hw, &mut hr, &mut dr, &mut dw, &stream_id, |r| {
            r.iter().any(|l| l.contains("Acceptance: 0/3"))
        })
        .await;
        let joined = pre.join("\n");
        assert!(
            joined.contains("Acceptance: 0/3"),
            "detail card did not open with a 0/3 header:\n{joined}"
        );
        assert!(
            !joined.contains('☑'),
            "nothing is ticked before `t`:\n{joined}"
        );
        for name in ["CRITERION-ONE", "CRITERION-TWO", "CRITERION-THREE"] {
            assert!(line_with(&pre, name).contains('☐'), "{name} must be ☐");
        }

        // `a a` walks the cursor to the SECOND criterion, `t` ticks it.
        send_char(&mut hw, 'a').await;
        send_char(&mut hw, 'a').await;
        send_char(&mut hw, 't').await;

        let post = render_rows_until(&mut hw, &mut hr, &mut dr, &mut dw, &stream_id, |r| {
            r.iter().any(|l| l.contains("Acceptance: 1/3"))
        })
        .await;
        let joined = post.join("\n");

        // The counted header moved, and the DECOY headers are absent.
        assert!(joined.contains("Acceptance: 1/3"), "header:\n{joined}");
        assert!(!joined.contains("Acceptance: 0/3"), "decoy 0/3:\n{joined}");
        assert!(!joined.contains("Acceptance: 3/3"), "decoy 3/3:\n{joined}");

        // ONLY the second criterion is ticked — the decoys stay ☐.
        let two = line_with(&post, "CRITERION-TWO");
        assert!(
            two.contains('☑') && !two.contains('☐'),
            "CRITERION-TWO must be ☑: {two}"
        );
        for decoy in ["CRITERION-ONE", "CRITERION-THREE"] {
            let line = line_with(&post, decoy);
            assert!(
                line.contains('☐') && !line.contains('☑'),
                "{decoy} must stay ☐: {line}"
            );
        }

        // The plugin fired a REAL hangar/issue_criterion_set naming the SECOND
        // criterion's STABLE id — not an ordinal, not the first criterion.
        let calls = seen.lock().unwrap().clone();
        let (_, params) = calls
            .iter()
            .find(|(m, _)| m == daemon_methods::HANGAR_ISSUE_CRITERION_SET)
            .unwrap_or_else(|| {
                panic!(
                    "no hangar/issue_criterion_set was sent; saw: {:?}",
                    calls.iter().map(|(m, _)| m).collect::<Vec<_>>()
                )
            });
        assert_eq!(params["criterion"], "ac-two", "params: {params}");
        assert_eq!(params["issue_id"], "target", "params: {params}");
        assert_eq!(params["checked"], true, "params: {params}");

        drop(hw);
        server.abort();
    };
    tokio::time::timeout(BUDGET, body)
        .await
        .expect("exceeded acceptance-tick budget");
}
