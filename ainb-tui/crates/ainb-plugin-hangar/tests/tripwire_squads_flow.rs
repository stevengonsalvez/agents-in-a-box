//! Tripwire (P7 / D17): the Squads screen end-to-end over real wire bytes.
//!
//! Drives the **real** [`HangarPlugin`] behind the **real** SDK [`Server`], playing
//! the host relaying the plugin's reverse `unix_socket_*` calls to a mock daemon
//! that records each `(method, params)` and answers with seed-shaped results built
//! from the genuine `ainb-hangar-proto` types. It proves the user-visible P7
//! journey the phase asks for — "open squads screen, create squad, assign issue,
//! see member rows":
//!
//!   * [`squads_screen_shows_leader_and_member_rows`] — pressing `S` renders the
//!     Squads screen with the seeded squad's name, its leader, and its MEMBER rows
//!     (resolved from the agents snapshot for live presence). A pre-press negative
//!     assertion confirms the squad is not on the landing screen.
//!   * [`create_and_assign_fire_the_squad_rpcs`] — pressing `c`, typing a name, and
//!     Enter issues a `hangar/squad_create` (leader resolved to the first cached
//!     agent); pressing `x` issues a `hangar/squad_fanout` for the selected squad
//!     with the resolved issue — leader-routing dispatch with member fan-out.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ainb_hangar_core::ids::IssueId;
use ainb_hangar_proto::events::{ActorRow, IssueRow, PresenceState};
use ainb_hangar_proto::snapshots::{
    AgentsListResult, IssuesListResult, SquadAssignResult, SquadFanoutResult,
    SquadMemberDispatchRow, SquadWireRow, SquadsListResult,
};
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

/// The seeded squad the daemon lists: `shippers`, led by `agent:agent-lead`, with
/// an agent member and a human member.
fn seeded_squads() -> serde_json::Value {
    serde_json::to_value(SquadsListResult {
        squads: vec![SquadWireRow {
            id: "s1".into(),
            name: "shippers".into(),
            leader: "agent:agent-lead".into(),
            members: vec!["agent:agent-1".into(), "member:user-1".into()],
        }],
    })
    .unwrap()
}

/// The seeded actors backing the squad's leader/member presence resolution.
fn seeded_agents() -> serde_json::Value {
    let actor = |actor_ref: &str, name: &str, presence: PresenceState, is_agent: bool| ActorRow {
        actor_ref: actor_ref.into(),
        display_name: name.into(),
        subtitle: String::new(),
        presence,
        workload: ainb_hangar_proto::events::Workload::Idle,
        is_agent,
        recent_rank: None,
    };
    serde_json::to_value(AgentsListResult {
        actors: vec![
            actor("agent:agent-lead", "lead-bot", PresenceState::Online, true),
            actor("agent:agent-1", "worker-bot", PresenceState::Unstable, true),
            actor("member:user-1", "alice", PresenceState::Online, false),
        ],
    })
    .unwrap()
}

/// One seeded issue so the assign (`x`) resolves an issue to fan out.
fn seeded_issues() -> serde_json::Value {
    serde_json::to_value(IssuesListResult {
        issues: vec![IssueRow {
            id: IssueId::from_str("issue-1").unwrap(),
            display_id: None,
            workspace_id: "default".into(),
            title: "Refactor API".into(),
            description: None,
            state: "open".into(),
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
        }],
    })
    .unwrap()
}

/// A mock daemon that records `(method, params)` and answers each request with a
/// seed-shaped result built from the real proto types (so every wire byte is a
/// genuine envelope). Squad mutations answer with the refreshed squads list; the
/// fan-out answers with a leader + one member dispatch.
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
        m if m == daemon_methods::HANGAR_SQUADS_LIST
            || m == daemon_methods::HANGAR_SQUAD_CREATE
            || m == daemon_methods::HANGAR_SQUAD_MEMBER_ADD
            || m == daemon_methods::HANGAR_SQUAD_MEMBER_REMOVE =>
        {
            seeded_squads()
        }
        m if m == daemon_methods::HANGAR_SQUAD_FANOUT => serde_json::to_value(SquadFanoutResult {
            leader: SquadAssignResult {
                task_id: "task-lead".into(),
                leader_agent_id: "agent-lead".into(),
                runtime_id: "runtime-lead".into(),
            },
            members: vec![SquadMemberDispatchRow {
                task_id: "task-m1".into(),
                agent_id: "agent-1".into(),
                runtime_id: "runtime-1".into(),
            }],
        })
        .unwrap(),
        m if m == daemon_methods::HANGAR_AGENTS_LIST => seeded_agents(),
        m if m == daemon_methods::HANGAR_ISSUES_LIST => seeded_issues(),
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
            "params": { "viewport": {"width": 160, "height": 40}, "generation": 0 }
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
/// until BOTH the agents and squads snapshots have been fetched + folded (so the
/// Squads screen has resolved rows). Returns the live handles + recorder.
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

    // Pump renders (relaying each pending snapshot send + pushing its reply) until
    // both the agents and squads snapshots have been fetched — the render loop
    // flushes the whole fetch_snapshots batch, in send order.
    for _ in 0..60 {
        let _ = render_capture(
            &mut host_write,
            &mut host_read,
            &mut daemon_reader,
            &mut daemon_write,
            stream_id,
        )
        .await;
        let have_both = {
            let s = seen.lock().unwrap();
            s.iter().any(|(m, _)| m == daemon_methods::HANGAR_SQUADS_LIST)
                && s.iter().any(|(m, _)| m == daemon_methods::HANGAR_AGENTS_LIST)
        };
        if have_both {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    (host_write, host_read, daemon_reader, daemon_write, server)
}

/// Pressing `S` renders the Squads screen with the seeded squad name, its leader,
/// and its MEMBER rows (resolved from the agents snapshot).
#[tokio::test]
async fn squads_screen_shows_leader_and_member_rows() {
    let body = async {
        let home = tempfile::tempdir().expect("home");
        std::env::set_var("AINB_HANGAR_HOME", home.path());
        let stream_id = format!("sock-squads-{}", std::process::id());
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let (mut hw, mut hr, mut dr, mut dw, server) =
            boot(home.path(), &stream_id, seen.clone()).await;

        // PRE-PRESS NEGATIVE: the landing screen is not the Squads screen.
        let pre = render_capture(&mut hw, &mut hr, &mut dr, &mut dw, &stream_id).await;
        assert!(
            !pre.contains("shippers"),
            "the squad must not be on the landing screen:\n{pre}"
        );

        // Press `S` to open the Squads screen, then render.
        send_char(&mut hw, 'S').await;
        let mut post = String::new();
        for _ in 0..30 {
            post = render_capture(&mut hw, &mut hr, &mut dr, &mut dw, &stream_id).await;
            if post.contains("shippers") && post.contains("worker-bot") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        // POSITIVE: the squad, its leader, and its MEMBER rows all render.
        assert!(post.contains("shippers"), "squad name missing:\n{post}");
        assert!(post.contains("lead-bot"), "leader missing:\n{post}");
        assert!(
            post.contains("worker-bot"),
            "agent member row missing:\n{post}"
        );
        assert!(post.contains("alice"), "human member row missing:\n{post}");
        assert!(post.contains("leader:"), "leader tag missing:\n{post}");

        drop(hw);
        server.abort();
    };
    tokio::time::timeout(BUDGET, body).await.expect("exceeded squads-render budget");
}

/// Pressing `c` + typing + Enter issues a `hangar/squad_create` (leader = first
/// cached agent); pressing `x` issues a `hangar/squad_fanout` for the squad.
#[tokio::test]
async fn create_and_assign_fire_the_squad_rpcs() {
    let body = async {
        let home = tempfile::tempdir().expect("home");
        std::env::set_var("AINB_HANGAR_HOME", home.path());
        let stream_id = format!("sock-squadrpc-{}", std::process::id());
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let (mut hw, mut hr, mut dr, mut dw, server) =
            boot(home.path(), &stream_id, seen.clone()).await;

        // Open the Squads screen.
        send_char(&mut hw, 'S').await;
        let _ = render_capture(&mut hw, &mut hr, &mut dr, &mut dw, &stream_id).await;

        // Create a squad: `c`, type "qa", Enter.
        send_char(&mut hw, 'c').await;
        send_char(&mut hw, 'q').await;
        send_char(&mut hw, 'a').await;
        send_key(&mut hw, KeyCode::Enter).await;

        // Pump until the daemon records the create with the resolved leader + name.
        let mut created = false;
        for _ in 0..60 {
            let _ = render_capture(&mut hw, &mut hr, &mut dr, &mut dw, &stream_id).await;
            created = seen.lock().unwrap().iter().any(|(m, p)| {
                m == daemon_methods::HANGAR_SQUAD_CREATE
                    && p.get("name").and_then(serde_json::Value::as_str) == Some("qa")
                    && p.get("leader").and_then(serde_json::Value::as_str)
                        == Some("agent:agent-lead")
            });
            if created {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            created,
            "pressing c + name + Enter must issue hangar/squad_create with the resolved leader; saw: {:?}",
            seen.lock().unwrap()
        );

        // Assign the current issue to the squad: `x` → hangar/squad_fanout.
        send_char(&mut hw, 'x').await;
        let mut fanned = false;
        for _ in 0..60 {
            let _ = render_capture(&mut hw, &mut hr, &mut dr, &mut dw, &stream_id).await;
            fanned = seen.lock().unwrap().iter().any(|(m, p)| {
                m == daemon_methods::HANGAR_SQUAD_FANOUT
                    && p.get("squad_id").and_then(serde_json::Value::as_str) == Some("s1")
                    && p.get("issue_id").and_then(serde_json::Value::as_str) == Some("issue-1")
            });
            if fanned {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            fanned,
            "pressing x must issue hangar/squad_fanout for the squad + issue; saw: {:?}",
            seen.lock().unwrap()
        );

        drop(hw);
        server.abort();
    };
    tokio::time::timeout(BUDGET, body).await.expect("exceeded squad-rpc budget");
}
