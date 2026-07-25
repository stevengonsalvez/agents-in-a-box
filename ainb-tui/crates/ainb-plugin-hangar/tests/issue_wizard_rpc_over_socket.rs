//! Phase 5 — prove the Issues create WIZARD dispatches over the socket.
//!
//! Drives the **real** [`HangarPlugin`] behind the **real** SDK [`Server`], with
//! the test playing the host that relays the plugin's reverse `unix_socket_*`
//! calls to a mock daemon recording each `(method, params)`. Walking the wizard
//! (`c` → title → `@` repo pick → source branch → target branch → agent Enter)
//! must issue, in cause-and-effect order:
//!
//! 1. `hangar/issue_create` with the typed title, then — on its reply's id —
//! 2. `hangar/issue_update` persisting repo / agent / source / target, and
//! 3. `hangar/issue_run` with `mode=headless` + the same repo/agent/branch
//!    overrides.
//!
//! This is the **user-visible proof** for the phase: the wizard cannot produce
//! the old title-only inert card — the create is chained straight into a real
//! dispatch carrying the forced agent assignment.

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
/// a plausible result. `issue_create` answers with a full `IssueRow` whose id is
/// `issue-9` — the id the follow-up `issue_update` / `issue_run` must target.
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
        m if m == daemon_methods::HANGAR_AUTOPILOTS_LIST => serde_json::json!({ "autopilots": [] }),
        m if m == daemon_methods::HANGAR_TASKS_LIST => serde_json::json!({ "tasks": [] }),
        // The wizard's create answers with the new row — its id drives the
        // follow-up update + run.
        m if m == daemon_methods::HANGAR_ISSUE_CREATE => serde_json::json!({
            "id":"issue-9","workspace_id":"default","title":"Wizard task",
            "description":null,"state":"open","assignee":null,
            "creator":"member:me","priority":0,"created_at":1_700_000_000_000_i64,
            "due_date":null,"pr_url":null
        }),
        m if m == daemon_methods::HANGAR_ISSUE_RUN => serde_json::json!({
            "task_id":"t-1","agent_id":"agent-claude","runtime_id":"rt-1","mode":"headless"
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

/// Drive one render so the plugin drains its deferred wizard RPCs; relay any
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
async fn wizard_commit_issues_create_update_and_run() {
    let body = async {
        let home = tempfile::tempdir().expect("home");
        std::env::set_var("AINB_HANGAR_HOME", home.path());
        let stream_id = format!("sock-wizard-{}", std::process::id());
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

        // Walk the single-form wizard: `c` opens it (focus Title); type the
        // title; ↓ moves focus to the Brief row; type a multi-line brief (Enter
        // there inserts a newline, NOT a commit); ↓ moves to the Repo row; `@`
        // opens the dropdown (cursor at scratch); Enter picks scratch (closing the
        // dropdown); Enter with the required fields satisfied (title + repo,
        // branches prefilled `main`, agent defaulted to claude) commits
        // `CreateAndRun` carrying the brief as `description`.
        send_key(&mut host_write, KeyCode::Char { ch: 'c' }).await;
        for ch in "Wizard task".chars() {
            send_key(&mut host_write, KeyCode::Char { ch }).await;
        }
        send_key(&mut host_write, KeyCode::Down).await; // Title → Brief
        // Lead with a `/name` skill line: `claude --print` executes a materialised
        // skill, so the slash + newline must reach `description` verbatim.
        for ch in "/graphify it".chars() {
            send_key(&mut host_write, KeyCode::Char { ch }).await;
        }
        send_key(&mut host_write, KeyCode::Enter).await; // newline in the brief
        for ch in "second line".chars() {
            send_key(&mut host_write, KeyCode::Char { ch }).await;
        }
        send_key(&mut host_write, KeyCode::Down).await; // Brief → Link
        send_key(&mut host_write, KeyCode::Down).await; // Link → Acceptance
        send_key(&mut host_write, KeyCode::Down).await; // Acceptance → Context
        send_key(&mut host_write, KeyCode::Down).await; // Context → Repo
        send_key(&mut host_write, KeyCode::Char { ch: '@' }).await;
        send_key(&mut host_write, KeyCode::Enter).await;
        send_key(&mut host_write, KeyCode::Enter).await;

        // Pump renders until all three legs of the chain hit the daemon: the
        // create (with the typed title), the follow-up update persisting
        // repo/agent/branches on the NEW issue id, and the headless run.
        let mut done = false;
        for _ in 0..60 {
            relay_one_send_or_render(
                &mut host_write,
                &mut host_read,
                &mut daemon_reader,
                &mut daemon_write,
                &stream_id,
            )
            .await;
            let s = |p: &serde_json::Value, k: &str| {
                p.get(k).and_then(serde_json::Value::as_str).map(str::to_string)
            };
            // Snapshot the recorded calls (guard dropped immediately) so no
            // lock lives across the await below.
            let calls: Vec<(String, serde_json::Value)> = seen.lock().unwrap().clone();
            let created = calls.iter().any(|(m, p)| {
                m == daemon_methods::HANGAR_ISSUE_CREATE
                    && s(p, "title").as_deref() == Some("Wizard task")
                    // The Brief lands as `description` on the create call VERBATIM:
                    // the leading `/name` slash and the embedded newline survive
                    // unchanged (no trim / escape / normalise), so the skill runs
                    // as typed at dispatch.
                    && s(p, "description").as_deref() == Some("/graphify it\nsecond line")
            });
            let updated = calls.iter().any(|(m, p)| {
                m == daemon_methods::HANGAR_ISSUE_UPDATE
                    && s(p, "issue_id").as_deref() == Some("issue-9")
                    && s(p, "repo_ref").as_deref() == Some("scratch")
                    && s(p, "agent").as_deref() == Some("claude")
                    && s(p, "source_branch").as_deref() == Some("main")
                    && s(p, "target_branch").as_deref() == Some("main")
            });
            let ran = calls.iter().any(|(m, p)| {
                m == daemon_methods::HANGAR_ISSUE_RUN
                    && s(p, "issue_id").as_deref() == Some("issue-9")
                    && s(p, "mode").as_deref() == Some("headless")
                    && s(p, "repo_ref").as_deref() == Some("scratch")
                    && s(p, "agent").as_deref() == Some("claude")
                    && s(p, "source_branch").as_deref() == Some("main")
            });
            if created && updated && ran {
                done = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            done,
            "wizard commit must chain issue_create → issue_update(repo/agent/branches) \
             → issue_run(headless) for the created issue; saw: {:?}",
            seen.lock().unwrap()
        );

        // Ordering discipline (V3-F3 depends on it): the persisting `issue_update`
        // must reach the daemon BEFORE the `issue_run`, so a run that resolves the
        // issue's persisted assignee (the named-agent target) never races the
        // write. The daemon serves one connection's frames in order, so send order
        // == apply order.
        let calls = seen.lock().unwrap().clone();
        let update_ix = calls
            .iter()
            .position(|(m, _)| m == daemon_methods::HANGAR_ISSUE_UPDATE)
            .expect("issue_update was sent");
        let run_ix = calls
            .iter()
            .position(|(m, _)| m == daemon_methods::HANGAR_ISSUE_RUN)
            .expect("issue_run was sent");
        assert!(
            update_ix < run_ix,
            "issue_update must precede issue_run (update @ {update_ix}, run @ {run_ix})"
        );

        drop(host_write);
        server.abort();
    };
    tokio::time::timeout(BUDGET, body)
        .await
        .expect("exceeded wizard-dispatch budget");
}
