//! Tripwire (multica gap #19): `blocked` and `cancelled` issues RENDER
//! DISTINCTLY on the board, and a card can be MOVED into Blocked through the
//! real context menu — end-to-end over real wire bytes.
//!
//! Drives the REAL [`HangarPlugin`] behind the REAL SDK [`Server`], playing the
//! host: it relays the plugin's reverse `unix_socket_*` calls to a mock daemon
//! that answers `hangar/issues_list` with one card in each of Todo / Blocked /
//! Cancelled / Done, then sends genuine `plugin/handle_mouse` +
//! `plugin/handle_key` frames and captures the `plugin/render` buffer.
//!
//! The assertions resolve each column's on-screen X RANGE from its header and
//! check the card ids against those ranges, because a bare
//! `pane.contains("HGR-2")` would pass while the card sat in the wrong column —
//! and the whole point of this gap is which column a state lands in.
//!
//! * [`blocked_and_cancelled_render_in_their_own_columns`] — the two new column
//!   headers paint with live counts, `HGR-2` sits inside Blocked's x-range and
//!   NOT inside Todo's, `HGR-3` inside Cancelled's and NOT inside Done's.
//! * [`context_menu_moves_a_card_into_blocked_over_the_wire`] — right-click the
//!   Todo card, `Move to ▸ Blocked`, Enter: the board repaints `Blocked (2)`
//!   AND the plugin really sends `hangar/issue_update{state:"blocked"}` down the
//!   socket. The plugin moves the card optimistically, so the UI assertion alone
//!   would pass against a plugin that never persisted anything — the recorded
//!   daemon call is the durability half. (The real daemon's own write is pinned
//!   by `ainb-hangar-daemon/tests/rpc_issue_state_blocked_cancelled.rs`.)
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
    HandleEventParams, KeyCode, KeyEvent, MouseButton, MouseEvent, MouseKind, UnixSocketEvent,
    UnixSocketEventKind, UnixSocketSendParams,
};
use ainb_plugin_protocol::{framing, methods};
use ainb_plugin_sdk::Server;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

const BUDGET: Duration = Duration::from_secs(30);

/// Render viewport. 189 = 7 × 27, wide enough that every one of the SEVEN
/// lifecycle columns paints its full header and its card ids.
const VIEW_W: u16 = 189;
/// Render viewport height.
const VIEW_H: u16 = 44;

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

/// One wire row. `state` is what buckets the card into a board column.
fn row(id: &str, title: &str, state: &str) -> IssueRow {
    IssueRow {
        subscriber_count: 0,
        subscribed: false,
        reactions: Vec::new(),
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
        assignee: None,
        creator: "member:user-1".into(),
        created_at: 0,
        priority: 0,
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
        acceptance_criteria: Vec::new(),
        acceptance: Vec::new(),
        context_refs: Vec::new(),
        dependencies: Vec::new(),
    }
}

/// The wire id of the Todo card the move test drives.
const TODO_ID: &str = "hgr-1";

/// One card in each of Todo / Blocked / Cancelled / Done — the four columns the
/// assertions discriminate between.
fn seeded_issues() -> serde_json::Value {
    serde_json::to_value(IssuesListResult {
        issues: vec![
            row(TODO_ID, "Card in todo", "todo"),
            row("hgr-2", "Card in blocked", "blocked"),
            row("hgr-3", "Card in cancelled", "cancelled"),
            row("hgr-4", "Card in done", "done"),
        ],
    })
    .unwrap()
}

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

/// The painted pane as a rectangular grid of chars, coordinates preserved.
/// Unpainted cells are spaces.
#[derive(Clone, Default)]
struct Pane {
    rows: Vec<Vec<char>>,
}

impl Pane {
    fn from_render(render_resp: &serde_json::Value) -> Self {
        let mut rows = vec![vec![' '; VIEW_W as usize]; VIEW_H as usize];
        if let Some(cells) = render_resp["result"]["buffer"]["cells"].as_array() {
            for c in cells {
                let (Some(x), Some(y)) = (c[0]["x"].as_u64(), c[0]["y"].as_u64()) else {
                    continue;
                };
                let Some(ch) = c[1]["symbol"].as_str().and_then(|s| s.chars().next()) else {
                    continue;
                };
                if (y as usize) < rows.len() && (x as usize) < VIEW_W as usize {
                    rows[y as usize][x as usize] = ch;
                }
            }
        }
        Self { rows }
    }

    /// The whole pane as text (for `contains` assertions + failure dumps).
    fn text(&self) -> String {
        self.rows
            .iter()
            .map(|r| r.iter().collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The x COLUMN where `label` starts, searched row by row.
    ///
    /// Matched over CHARS, never bytes: the pane is full of multi-byte box
    /// glyphs, so `str::find`'s byte offset would not be a screen column and
    /// every x-range assertion below would silently compare nonsense.
    fn label_x(&self, label: &str) -> Option<usize> {
        let needle: Vec<char> = label.chars().collect();
        self.rows
            .iter()
            .find_map(|r| r.windows(needle.len()).position(|w| w == needle.as_slice()))
    }

    /// Whether `needle` appears anywhere within the half-open x range
    /// `[from, to)` — i.e. INSIDE one board column.
    fn contains_in_x_range(&self, needle: &str, from: usize, to: usize) -> bool {
        self.rows.iter().any(|r| {
            let slice: String =
                r.iter().skip(from).take(to.saturating_sub(from)).collect::<String>();
            slice.contains(needle)
        })
    }
}

/// Flatten every painted cell symbol (order-preserving), used for the cheap
/// "has it rendered yet" polls.
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

/// Send one `plugin/render`, relaying any reverse send to the daemon (and
/// pushing its reply), and return the painted [`Pane`].
async fn render_capture<W, R, DR, DW>(
    host_write: &mut W,
    host_read: &mut R,
    daemon_reader: &mut DR,
    daemon_write: &mut DW,
    stream_id: &str,
) -> Pane
where
    W: tokio::io::AsyncWrite + Unpin,
    R: tokio::io::AsyncBufRead + Unpin,
    DR: tokio::io::AsyncBufRead + Unpin,
    DW: tokio::io::AsyncWrite + Unpin,
{
    host_write
        .write_all(&host_frame(&serde_json::json!({
            "jsonrpc": "2.0", "id": 99, "method": methods::PLUGIN_RENDER,
            "params": { "viewport": {"width": VIEW_W, "height": VIEW_H}, "generation": 0 }
        })))
        .await
        .unwrap();
    loop {
        let Some(frame) = read_frame(host_read).await else {
            return Pane::default();
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
            let _ = render_text(&frame);
            return Pane::from_render(&frame);
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

/// Boot the plugin + mock daemon, relay the connect handshake, and pump renders
/// until the issues snapshot has been fetched + folded.
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

    if let Some(ack) = read_one_raw_frame(&mut daemon_reader).await {
        push_data(&mut host_write, stream_id, &ack).await;
    }

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

/// Pump renders until `ok(pane)` holds or the iteration budget elapses,
/// returning the last captured pane either way.
async fn render_until<W, R, DR, DW, F>(
    hw: &mut W,
    hr: &mut R,
    dr: &mut DR,
    dw: &mut DW,
    stream_id: &str,
    mut ok: F,
) -> Pane
where
    W: tokio::io::AsyncWrite + Unpin,
    R: tokio::io::AsyncBufRead + Unpin,
    DR: tokio::io::AsyncBufRead + Unpin,
    DW: tokio::io::AsyncWrite + Unpin,
    F: FnMut(&Pane) -> bool,
{
    let mut pane = Pane::default();
    for _ in 0..40 {
        pane = render_capture(hw, hr, dr, dw, stream_id).await;
        if ok(&pane) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    pane
}

/// Cells the status glyph + its trailing space occupy before a column header's
/// text (`⊘ Blocked (1)`). A column's LEFT EDGE is its header label's x minus
/// this, which is what the cards are painted against.
const HEADER_GLYPH_W: usize = 2;

/// The half-open x range `[start, end)` of the column whose header text is
/// `header`, taking the NEXT header to the right as the boundary. Both edges are
/// the columns' real left edges (label x minus the glyph prefix), so a card
/// painted flush against the edge is inside its own column, not the previous
/// one.
fn column_range(pane: &Pane, header: &str, next_header: Option<&str>) -> (usize, usize) {
    let start = pane
        .label_x(header)
        .unwrap_or_else(|| panic!("column header {header:?} not painted:\n{}", pane.text()))
        .saturating_sub(HEADER_GLYPH_W);
    let end = next_header.map_or(VIEW_W as usize, |h| {
        pane.label_x(h)
            .unwrap_or_else(|| panic!("column header {h:?} not painted:\n{}", pane.text()))
            .saturating_sub(HEADER_GLYPH_W)
    });
    assert!(
        end > start,
        "{header:?} must sit LEFT of {next_header:?} (start={start}, end={end})\n{}",
        pane.text()
    );
    (start, end)
}

/// Blocked and Cancelled each get their OWN column, and the cards really land in
/// them — proved against the columns' on-screen x ranges, with the neighbouring
/// column as an explicit decoy.
#[tokio::test]
async fn blocked_and_cancelled_render_in_their_own_columns() {
    let body = async {
        let home = tempfile::tempdir().expect("home");
        std::env::set_var("AINB_HANGAR_HOME", home.path());
        let stream_id = format!("sock-blkcan-{}", std::process::id());
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let (mut hw, mut hr, mut dr, mut dw, server) =
            boot(home.path(), &stream_id, seen.clone()).await;

        let pane = render_until(&mut hw, &mut hr, &mut dr, &mut dw, &stream_id, |p| {
            let t = p.text();
            t.contains("Blocked (1)") && t.contains("Cancelled (1)")
        })
        .await;
        let text = pane.text();

        // The two new columns exist, with live counts (EXACT substrings).
        assert!(
            text.contains("Blocked (1)"),
            "Blocked column header missing:\n{text}"
        );
        assert!(
            text.contains("Cancelled (1)"),
            "Cancelled column header missing:\n{text}"
        );
        // …alongside the pre-existing five, so nothing was displaced.
        for header in [
            "Backlog (0)",
            "Todo (1)",
            "In Progress (0)",
            "In Review (0)",
            "Done (1)",
        ] {
            assert!(text.contains(header), "missing header {header:?}:\n{text}");
        }

        let (todo_x, todo_end) = column_range(&pane, "Todo (", Some("In Progress ("));
        let (done_x, done_end) = column_range(&pane, "Done (", Some("Blocked ("));
        let (blocked_x, blocked_end) = column_range(&pane, "Blocked (", Some("Cancelled ("));
        let (cancelled_x, cancelled_end) = column_range(&pane, "Cancelled (", None);

        // POSITIVE + DECOY: the blocked card sits in Blocked, not in Todo.
        assert!(
            pane.contains_in_x_range("HGR-2", blocked_x, blocked_end),
            "the blocked card must paint inside the Blocked column \
             (x {blocked_x}..{blocked_end}):\n{text}"
        );
        assert!(
            !pane.contains_in_x_range("HGR-2", todo_x, todo_end),
            "the blocked card must NOT paint in the Todo column \
             (x {todo_x}..{todo_end}):\n{text}"
        );

        // POSITIVE + DECOY: the cancelled card sits in Cancelled, not in Done.
        assert!(
            pane.contains_in_x_range("HGR-3", cancelled_x, cancelled_end),
            "the cancelled card must paint inside the Cancelled column \
             (x {cancelled_x}..{cancelled_end}):\n{text}"
        );
        assert!(
            !pane.contains_in_x_range("HGR-3", done_x, done_end),
            "the cancelled card must NOT paint in the Done column \
             (x {done_x}..{done_end}):\n{text}"
        );

        drop(hw);
        server.abort();
    };
    tokio::time::timeout(BUDGET, body)
        .await
        .expect("exceeded blocked/cancelled render budget");
}

/// Moving a card into Blocked through the REAL context menu repaints the board
/// AND sends the durable `hangar/issue_update{state:"blocked"}` down the socket.
#[tokio::test]
async fn context_menu_moves_a_card_into_blocked_over_the_wire() {
    let body = async {
        let home = tempfile::tempdir().expect("home");
        std::env::set_var("AINB_HANGAR_HOME", home.path());
        let stream_id = format!("sock-mvblk-{}", std::process::id());
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let (mut hw, mut hr, mut dr, mut dw, server) =
            boot(home.path(), &stream_id, seen.clone()).await;

        // Render once so the board hit-map exists, and locate the Todo card.
        let pane = render_until(&mut hw, &mut hr, &mut dr, &mut dw, &stream_id, |p| {
            p.text().contains("Blocked (1)")
        })
        .await;
        let (todo_x, _) = column_range(&pane, "Todo (", Some("In Progress ("));
        let card_y = pane
            .rows
            .iter()
            .position(|r| r.iter().skip(todo_x).take(20).collect::<String>().contains("HGR-1"))
            .expect("the Todo card paints its id") as u16;

        // Right-click the Todo card to raise the context menu.
        send_mouse(
            &mut hw,
            MouseKind::Down {
                button: MouseButton::Right,
            },
            u16::try_from(todo_x).unwrap() + 2,
            card_y,
        )
        .await;
        let menu = render_until(&mut hw, &mut hr, &mut dr, &mut dw, &stream_id, |p| {
            p.text().contains("Move to")
        })
        .await;
        assert!(
            menu.text().contains("Move to"),
            "a right-click must raise the context menu:\n{}",
            menu.text()
        );

        // Down to `Move to`, Right to open the submenu (pre-selects the card's
        // CURRENT status, Todo = order 1), then Down ×4 to Blocked (order 5).
        send_key(&mut hw, KeyCode::Down).await;
        send_key(&mut hw, KeyCode::Right).await;
        for _ in 0..4 {
            send_key(&mut hw, KeyCode::Down).await;
        }
        send_key(&mut hw, KeyCode::Enter).await;

        // The board repaints with the card moved (optimistic local move).
        let moved = render_until(&mut hw, &mut hr, &mut dr, &mut dw, &stream_id, |p| {
            p.text().contains("Blocked (2)")
        })
        .await;
        assert!(
            moved.text().contains("Blocked (2)"),
            "the moved card must land in the Blocked column:\n{}",
            moved.text()
        );

        // DURABILITY: the plugin really asked the daemon to persist it. The
        // optimistic move above would render identically against a plugin that
        // never sent anything, so this is the load-bearing half.
        let persisted = seen.lock().unwrap().iter().any(|(m, p)| {
            m == daemon_methods::HANGAR_ISSUE_UPDATE
                && p.get("issue_id").and_then(serde_json::Value::as_str) == Some(TODO_ID)
                && p.get("state").and_then(serde_json::Value::as_str) == Some("blocked")
        });
        assert!(
            persisted,
            "the move must issue hangar/issue_update(state=blocked) for {TODO_ID}; saw: {:?}",
            seen.lock().unwrap()
        );

        drop(hw);
        server.abort();
    };
    tokio::time::timeout(BUDGET, body).await.expect("exceeded blocked-move budget");
}
