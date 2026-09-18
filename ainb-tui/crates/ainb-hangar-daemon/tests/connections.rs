//! Integration coverage for the daemon's in-memory surface connection registry.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ainb_hangar_daemon::rpc::{self, DaemonHealth};
use ainb_hangar_proto::events::EVENT_METHOD;
use ainb_hangar_proto::{RpcId, RpcRequest, methods};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::attention::{AttentionKind, AttentionRepo, NewAttention};
use ainb_web::data::{CoreFuture, CoreSnapshot, CostFuture, DataSource};
use ainb_web::{ServeError, WebConfig, serve};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::Mutex;

const CONNECTIONS_LIST: &str = "hangar/connections_list";

/// Serializes tests that redirect the process-wide web daemon-home settings.
///
/// `serve` resolves `AINB_HANGAR_HOME` on its background presence task. If
/// these tests update that variable together, one server can authenticate with
/// the other test's daemon, violating both tests' connection assertions.
static WEB_HOME_ENV_LOCK: Mutex<()> = Mutex::const_new(());

struct Client {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
}

impl Client {
    async fn connect(socket_path: &std::path::Path) -> Self {
        let deadline = Instant::now() + Duration::from_secs(5);
        let stream = loop {
            match UnixStream::connect(socket_path).await {
                Ok(stream) => break stream,
                Err(_) if Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(error) => panic!("daemon never accepted a connection: {error}"),
            }
        };
        let (read_half, writer) = stream.into_split();
        Self {
            reader: BufReader::new(read_half),
            writer,
        }
    }

    async fn call(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        let request = RpcRequest {
            jsonrpc: ainb_hangar_proto::jsonrpc_version(),
            id: RpcId::Number(7),
            method: method.to_string(),
            params,
        };
        let body = serde_json::to_vec(&request).expect("request serializes");
        let mut frame = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        frame.extend_from_slice(&body);
        self.writer.write_all(&frame).await.expect("write request");
        self.writer.flush().await.expect("flush request");

        loop {
            let frame = tokio::time::timeout(Duration::from_secs(5), self.read_frame())
                .await
                .expect("response arrives before timeout");
            if frame.get("id").is_some() {
                return frame;
            }
        }
    }

    async fn read_frame(&mut self) -> serde_json::Value {
        use tokio::io::AsyncBufReadExt;

        let mut length = None;
        loop {
            let mut line = String::new();
            let read = self.reader.read_line(&mut line).await.expect("read frame header");
            assert!(read > 0, "connection closed while awaiting a frame");
            let line = line.trim_end_matches("\r\n");
            if line.is_empty() {
                let mut body = vec![0_u8; length.expect("Content-Length header")];
                self.reader.read_exact(&mut body).await.expect("read frame body");
                return serde_json::from_slice(&body).expect("response JSON");
            }
            if let Some((name, value)) = line.split_once(':') {
                if name.trim().eq_ignore_ascii_case("Content-Length") {
                    length = Some(value.trim().parse().expect("numeric content length"));
                }
            }
        }
    }

    async fn hello(&mut self, home: &std::path::Path, surface: Option<&str>) {
        let token = std::fs::read_to_string(ainb_hangar_proto::auth::token_file_in(home))
            .expect("read daemon token");
        let mut params = serde_json::json!({ "token": token.trim() });
        if let Some(kind) = surface {
            params["surface"] = serde_json::json!({ "kind": kind, "pid": 4242 });
        }
        let response = self.call(methods::AUTH_HELLO, params).await;
        assert!(
            response["error"].is_null(),
            "hello must succeed: {response}"
        );
    }

    /// `auth/hello` with an explicit surface pid and `transient` request.
    async fn hello_with(&mut self, home: &std::path::Path, kind: &str, pid: u32, transient: bool) {
        let token = std::fs::read_to_string(ainb_hangar_proto::auth::token_file_in(home))
            .expect("read daemon token");
        let mut params = serde_json::json!({
            "token": token.trim(),
            "surface": { "kind": kind, "pid": pid },
        });
        if transient {
            params["transient"] = serde_json::Value::Bool(true);
        }
        let response = self.call(methods::AUTH_HELLO, params).await;
        assert!(
            response["error"].is_null(),
            "hello must succeed: {response}"
        );
    }

    async fn subscribe_connections(&mut self) {
        let response = self.call(methods::ATTENTION_SUBSCRIBE, serde_json::json!({})).await;
        assert!(
            response["error"].is_null(),
            "subscribe must succeed: {response}"
        );
    }

    async fn connections(&mut self) -> serde_json::Value {
        let response = self.call(CONNECTIONS_LIST, serde_json::json!({})).await;
        assert!(
            response["error"].is_null(),
            "connections list must succeed: {response}"
        );
        response["result"].clone()
    }

    async fn next_connections_changed(&mut self) -> serde_json::Value {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .expect("connection event arrives before timeout");
            let frame = tokio::time::timeout(remaining, self.read_frame())
                .await
                .expect("connection event arrives before timeout");
            if frame.get("id").is_none()
                && frame["method"] == EVENT_METHOD
                && frame["params"]["event"] == "connections_changed"
            {
                return frame["params"].clone();
            }
        }
    }
}

async fn start_server(home: &std::path::Path) -> (std::path::PathBuf, Store) {
    let store = Store::open_in(home).await.expect("open store");
    rpc::auth::ensure_socket_token(store.pool(), home)
        .await
        .expect("ensure socket token");
    let socket = rpc::socket_path_in(home);
    let listener = rpc::bind(&socket).expect("bind socket");
    let health = DaemonHealth {
        socket_path: socket.to_string_lossy().into_owned(),
        pid: std::process::id(),
        started_at: Instant::now(),
        version: "test".to_string(),
        stats: std::sync::Arc::new(ainb_hangar_daemon::health_stats::HealthStats::default()),
    };
    tokio::spawn(rpc::serve(
        listener,
        store.pool().clone(),
        health,
        ainb_hangar_daemon::events::EventBroker::new(),
    ));
    (socket, store)
}

#[tokio::test]
async fn registry_lists_surfaces_and_broadcasts_connection_lifecycle() {
    let home = tempfile::tempdir().expect("temporary Hangar home");
    let (socket, _store) = start_server(home.path()).await;

    let mut tui = Client::connect(&socket).await;
    tui.hello(home.path(), Some("tui")).await;
    tui.subscribe_connections().await;

    let mut web = Client::connect(&socket).await;
    web.hello(home.path(), Some("web")).await;
    let changed = tui.next_connections_changed().await;
    assert_eq!(changed["connections"].as_array().map(Vec::len), Some(2));

    let mut legacy = Client::connect(&socket).await;
    legacy.hello(home.path(), None).await;
    let changed = tui.next_connections_changed().await;
    assert_eq!(changed["connections"].as_array().map(Vec::len), Some(3));
    assert!(
        changed["connections"]
            .as_array()
            .expect("connections array")
            .iter()
            .any(|row| row["surface"]["kind"] == "unknown"),
        "clients which omit surface metadata must list as unknown: {changed}"
    );

    let listed = tui.connections().await;
    assert_eq!(listed["connections"].as_array().map(Vec::len), Some(3));
    assert!(
        listed["connections"]
            .as_array()
            .expect("connections array")
            .iter()
            .all(|row| row["host"].is_string() && row["connected_at"].is_string()),
        "daemon stamps host and connected timestamp: {listed}"
    );

    drop(legacy);
    let changed = tui.next_connections_changed().await;
    assert_eq!(changed["connections"].as_array().map(Vec::len), Some(2));

    drop(web);
    let changed = tui.next_connections_changed().await;
    assert_eq!(changed["connections"].as_array().map(Vec::len), Some(1));
    assert_eq!(changed["connections"][0]["surface"]["kind"], "tui");
}

// Note: mark_process_as_surface sets a process-global AtomicBool that is never cleared, leaking across tests in this binary runner.
#[tokio::test]
async fn web_server_presence_lives_for_server_task() {
    let _env_lock = WEB_HOME_ENV_LOCK.lock().await;
    let home = tempfile::tempdir().expect("temporary Hangar home");
    let (socket, _store) = start_server(home.path()).await;
    let _hangar_home = EnvGuard::set("AINB_HANGAR_HOME", home.path());
    let _ainb_home = EnvGuard::set("AINB_HOME", home.path());

    // This starts the production server entry point. Its presence connection is
    // owned by `serve`, never by this test's observer socket.
    let server = tokio::spawn(serve(
        WebConfig {
            listen: "127.0.0.1:0".parse().expect("loopback address"),
            token: None,
            insecure_bind: false,
            read_only: true,
        },
        Arc::new(WebServerSource),
    ));

    let mut observer = Client::connect(&socket).await;
    observer.hello(home.path(), Some("tui")).await;

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let listed = observer.connections().await;
        let count = listed["connections"].as_array().map_or(0, |rows| {
            rows.iter().filter(|row| row["surface"]["kind"] == "web").count()
        });
        assert!(
            count <= 1,
            "web presence registered more than once in connections_list: {listed}"
        );
        if count == 1 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "web presence never appeared in connections_list as exactly one row: {listed}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    let settled = observer.connections().await;
    let settled_count = settled["connections"].as_array().map_or(0, |rows| {
        rows.iter().filter(|row| row["surface"]["kind"] == "web").count()
    });
    assert_eq!(
        settled_count, 1,
        "web presence remains exactly one row after settle: {settled}"
    );

    // Aborting the web server drops its server-owned guard and actual socket.
    // The daemon must remove that row, not retain a synthetic lease.
    server.abort();
    assert!(server.await.expect_err("server was aborted").is_cancelled());
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let listed = observer.connections().await;
        if listed["connections"]
            .as_array()
            .is_some_and(|rows| rows.iter().all(|row| row["surface"]["kind"] != "web"))
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "web presence lingered after its owner ended: {listed}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn web_listen_failure_never_registers_presence() {
    let _env_lock = WEB_HOME_ENV_LOCK.lock().await;
    let home = tempfile::tempdir().expect("temporary Hangar home");
    let (socket, _store) = start_server(home.path()).await;
    let _hangar_home = EnvGuard::set("AINB_HANGAR_HOME", home.path());
    let _ainb_home = EnvGuard::set("AINB_HOME", home.path());

    let occupied = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("occupy loopback port");
    let addr = occupied.local_addr().expect("read occupied address");

    let mut observer = Client::connect(&socket).await;
    observer.hello(home.path(), Some("tui")).await;
    observer.subscribe_connections().await;

    let error = serve(
        WebConfig {
            listen: addr,
            token: None,
            insecure_bind: false,
            read_only: true,
        },
        Arc::new(WebServerSource),
    )
    .await
    .expect_err("occupied port rejects web server");
    match error {
        ServeError::Listen {
            addr: listen_addr, ..
        } => assert_eq!(listen_addr, addr),
        other => panic!("occupied port returns Listen error, got {other:?}"),
    }

    if let Ok(event) = tokio::time::timeout(
        Duration::from_millis(250),
        observer.next_connections_changed(),
    )
    .await
    {
        panic!("failed web bind changed daemon connections: {event}");
    }

    let deadline = Instant::now() + Duration::from_millis(250);
    loop {
        let listed = observer.connections().await;
        assert!(
            listed["connections"]
                .as_array()
                .is_some_and(|rows| rows.iter().all(|row| row["surface"]["kind"] != "web")),
            "failed web bind registered a web presence: {listed}"
        );
        if Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

struct WebServerSource;

impl DataSource for WebServerSource {
    fn core(&self) -> CoreFuture<'_> {
        Box::pin(async {
            Ok(CoreSnapshot {
                sessions: serde_json::json!([]),
                needs: Vec::new(),
            })
        })
    }

    fn cost(&self) -> CostFuture<'_> {
        Box::pin(async { None })
    }
}

/// Creates isolated discovery and delivery shims for the answer route.
///
/// The fake `ainb` exposes one exact-match session; the fake `tmux` reports a
/// composer-less pane, which exercises the normal write plus single-Enter
/// delivery path without touching a real session.
fn install_answer_route_shims(bin: &Path, session_id: &str, cwd: &Path) {
    use std::os::unix::fs::PermissionsExt as _;

    std::fs::create_dir_all(bin).expect("create shim directory");
    let session = serde_json::json!([{
        "session_id": session_id,
        "tmux_session_name": "provenance-target",
        "workspace_name": "provenance",
        "worktree_path": cwd,
        "created_at": "2026-01-01T00:00:00Z",
        "is_running": true,
        "claude_active": true,
    }]);
    let ainb = bin.join("ainb");
    std::fs::write(&ainb, format!("#!/bin/sh\nprintf '%s\\n' '{}'\n", session))
        .expect("write fake ainb");
    std::fs::set_permissions(&ainb, std::fs::Permissions::from_mode(0o755))
        .expect("make fake ainb executable");

    let tmux = bin.join("tmux");
    std::fs::write(&tmux, "#!/bin/sh\nexit 0\n").expect("write fake tmux");
    std::fs::set_permissions(&tmux, std::fs::Permissions::from_mode(0o755))
        .expect("make fake tmux executable");
}

/// Restores one process-global environment variable at test end.
struct EnvGuard {
    name: &'static str,
    prior: Option<std::ffi::OsString>,
}

impl EnvGuard {
    fn set(name: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let prior = std::env::var_os(name);
        std::env::set_var(name, value);
        Self { name, prior }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.prior {
            Some(value) => std::env::set_var(self.name, value),
            None => std::env::remove_var(self.name),
        }
    }
}

#[tokio::test]
async fn answer_provenance_comes_from_authenticated_connection_not_client() {
    let home = tempfile::tempdir().expect("temporary Hangar home");
    let bin = home.path().join("bin");
    let session_id = "provenance-session";
    install_answer_route_shims(&bin, session_id, home.path());
    let _ainb_bin = EnvGuard::set("AINB_BIN", bin.join("ainb"));
    let mut path = vec![bin];
    path.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let _path = EnvGuard::set(
        "PATH",
        std::env::join_paths(path).expect("build shimmed PATH"),
    );

    let (socket, store) = start_server(home.path()).await;
    let mut client = Client::connect(&socket).await;
    client.hello(home.path(), Some("web")).await;
    let connections = client.connections().await;
    let connection = &connections["connections"][0];
    assert_eq!(connection["surface"]["kind"], "web");
    let daemon_host = connection["host"].as_str().expect("registry host").to_string();

    let attention_id = "provenance-attention";
    AttentionRepo::insert(
        store.pool(),
        &NewAttention {
            id: attention_id.to_string(),
            session_id: session_id.to_string(),
            cwd: home.path().to_string_lossy().into_owned(),
            workspace_id: None,
            kind: AttentionKind::AskUserQuestion,
            payload: "{}".to_string(),
            degraded: false,
            created_at: 1,
            raise_transcript: None,
            channels: ainb_hangar_core::channel::ChannelSet::NONE,
        },
    )
    .await
    .expect("seed open attention row");

    let spoofed_by = "fleet@forged-host";
    let response = client
        .call(
            methods::ATTENTION_ANSWER,
            serde_json::json!({
                "attention_id": attention_id,
                "answer": "approved",
                "answered_by": spoofed_by,
            }),
        )
        .await;
    assert!(
        response["error"].is_null(),
        "answer must succeed: {response}"
    );
    assert_eq!(response["result"]["outcome"], "delivered", "{response}");

    let answered = AttentionRepo::get(store.pool(), attention_id)
        .await
        .expect("read answered row")
        .expect("seeded row remains present");
    assert_eq!(answered.state, "answered");
    assert_eq!(answered.answer.as_deref(), Some("approved"));
    assert_eq!(
        answered.answered_by.as_deref(),
        Some(format!("web@{daemon_host}").as_str()),
        "daemon provenance must use the registry surface and host, not client input"
    );
    assert_ne!(answered.answered_by.as_deref(), Some(spoofed_by));
}

/// A daemon on its own runtime and thread, so stopping it drops every task and
/// socket it served, the way a killed daemon process does. Aborting the
/// in-process `serve` task alone would leave its connection tasks running.
struct IsolatedDaemon {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl IsolatedDaemon {
    async fn start(home: &Path) -> Self {
        let home = home.to_path_buf();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let (stop, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("daemon runtime");
            runtime.block_on(async move {
                let (_socket, _store) = start_server(&home).await;
                let _ = ready_tx.send(());
                let _ = stop_rx.await;
            });
            // Dropping the runtime here, off any async context, cancels the
            // accept loop and every connection task, closing their sockets.
            drop(runtime);
        });
        ready_rx.await.expect("daemon thread reached its serve loop");
        Self {
            stop: Some(stop),
            thread: Some(thread),
        }
    }

    fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            thread.join().expect("daemon thread exits cleanly");
        }
    }
}

impl Drop for IsolatedDaemon {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// The lease's dialer, reading the token fresh each attempt as `from_env` does,
/// so a daemon that (re)writes it after the surface started is still reached.
fn lease_dialer(home: &Path) -> ainb_hangar_client::Dialer {
    let home = home.to_path_buf();
    Box::new(move || {
        let token = std::fs::read_to_string(ainb_hangar_proto::auth::token_file_in(&home))
            .map_err(|error| ainb_hangar_client::DaemonError::Token(error.to_string()))?;
        Ok(ainb_hangar_client::DaemonClient::with_parts(
            rpc::socket_path_in(&home),
            token.trim().to_string(),
        ))
    })
}

fn tui_surface() -> ainb_hangar_proto::connections::SurfaceInfo {
    ainb_hangar_proto::connections::SurfaceInfo {
        kind: ainb_hangar_proto::connections::SurfaceKind::Tui,
        pid: std::process::id(),
    }
}

async fn wait_for_state(
    lease: &ainb_hangar_client::PresenceLease,
    want: &ainb_hangar_client::PresenceState,
    within: Duration,
) {
    let mut state = lease.state();
    tokio::time::timeout(within, state.wait_for(|current| current == want))
        .await
        .unwrap_or_else(|_| {
            panic!(
                "lease never reached {want:?}; last: {:?}",
                *lease.state().borrow()
            )
        })
        .expect("lease task alive");
}

/// Kinds and pids of every listed row, from a fresh raw observer that is not a
/// `DaemonClient` and so never transient.
async fn listed_surfaces(home: &Path) -> Vec<(String, u64)> {
    let mut observer = Client::connect(&rpc::socket_path_in(home)).await;
    observer.hello(home, Some("cli")).await;
    observer.connections().await["connections"]
        .as_array()
        .expect("connections array")
        .iter()
        .filter(|row| row["surface"]["pid"] != 4242)
        .map(|row| {
            (
                row["surface"]["kind"].as_str().unwrap_or_default().to_string(),
                row["surface"]["pid"].as_u64().unwrap_or_default(),
            )
        })
        .collect()
}

/// #963: a surface holding a presence lease is exactly one row, however many
/// call connections the same process opens, and whatever kind those calls
/// declare. The row comes back after a daemon restart and goes on close.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn presence_lease_is_one_row_across_calls_and_daemon_restart() {
    let home = tempfile::tempdir().expect("temporary Hangar home");
    let daemon = IsolatedDaemon::start(home.path()).await;
    let pid = u64::from(std::process::id());

    let lease =
        ainb_hangar_client::PresenceLease::spawn_with(tui_surface(), lease_dialer(home.path()));
    wait_for_state(
        &lease,
        &ainb_hangar_client::PresenceState::Connected,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(
        listed_surfaces(home.path()).await,
        vec![("tui".to_string(), pid)]
    );

    // A connection-change subscriber must see nothing from this process's calls.
    let mut watcher = Client::connect(&rpc::socket_path_in(home.path())).await;
    watcher.hello(home.path(), Some("web")).await;
    watcher.subscribe_connections().await;

    // The same process dialing as the TUI's poll sites (`tui`) and as the
    // untouched `from_env` sites (`cli`). Each listing is taken WHILE that call
    // connection is open, so it would contain itself if it were listed.
    let token = std::fs::read_to_string(ainb_hangar_proto::auth::token_file_in(home.path()))
        .expect("daemon token");
    let cli_call = ainb_hangar_client::DaemonClient::with_parts(
        rpc::socket_path_in(home.path()),
        token.trim().to_string(),
    );
    let mut tui_call = cli_call.clone();
    tui_call.set_surface(tui_surface());
    for call in [&cli_call, &tui_call] {
        let seen = call.connections_list().await.expect("transient call is served");
        let ours: Vec<_> = seen
            .connections
            .iter()
            .filter(|row| u64::from(row.surface.pid) == pid)
            .map(|row| row.surface.kind.as_str())
            .collect();
        assert_eq!(ours, vec!["tui"], "one row for this process: {seen:?}");
    }
    if let Ok(event) = tokio::time::timeout(
        Duration::from_millis(300),
        watcher.next_connections_changed(),
    )
    .await
    {
        panic!("a transient call changed the listed registry: {event}");
    }
    drop(watcher);

    // Kill the daemon: the row cannot outlive it, and the lease notices.
    daemon.stop();
    let lost = ainb_hangar_client::PresenceState::Connected;
    let mut state = lease.state();
    tokio::time::timeout(
        Duration::from_secs(5),
        state.wait_for(|current| *current != lost),
    )
    .await
    .expect("lease notices the daemon went away")
    .expect("lease task alive");

    let restarted = Instant::now();
    let daemon = IsolatedDaemon::start(home.path()).await;
    wait_for_state(
        &lease,
        &ainb_hangar_client::PresenceState::Connected,
        Duration::from_secs(5),
    )
    .await;
    assert!(restarted.elapsed() < Duration::from_secs(5));
    assert_eq!(
        listed_surfaces(home.path()).await,
        vec![("tui".to_string(), pid)]
    );

    lease.close().await;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let listed = listed_surfaces(home.path()).await;
        if listed.iter().all(|(kind, _)| kind != "tui") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "tui row outlived the lease: {listed:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    daemon.stop();
}

/// #963: a surface started before its daemon registers once the daemon is up.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn presence_lease_registers_once_a_late_daemon_comes_up() {
    let home = tempfile::tempdir().expect("temporary Hangar home");
    let lease =
        ainb_hangar_client::PresenceLease::spawn_with(tui_surface(), lease_dialer(home.path()));

    // No daemon, no token: the lease waits quietly rather than failing.
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(
        matches!(
            *lease.state().borrow(),
            ainb_hangar_client::PresenceState::Waiting { .. }
        ),
        "lease must wait for a daemon: {:?}",
        *lease.state().borrow()
    );

    let daemon = IsolatedDaemon::start(home.path()).await;
    wait_for_state(
        &lease,
        &ainb_hangar_client::PresenceState::Connected,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(
        listed_surfaces(home.path()).await,
        vec![("tui".to_string(), u64::from(std::process::id()))]
    );
    lease.close().await;
    daemon.stop();
}

/// #963, web half: the web surface's one-shot reads are transient beside its
/// presence socket, so only that socket is ever listed. Before the flag each
/// read listed a second `web` row for its lifetime, which an exact-count smoke
/// saw as flicker.
#[tokio::test]
async fn web_one_shot_calls_beside_its_presence_never_list_a_second_row() {
    let _env_lock = WEB_HOME_ENV_LOCK.lock().await;
    let home = tempfile::tempdir().expect("temporary Hangar home");
    let (socket, _store) = start_server(home.path()).await;
    let _hangar_home = EnvGuard::set("AINB_HANGAR_HOME", home.path());
    let _ainb_home = EnvGuard::set("AINB_HOME", home.path());

    // The web server's presence socket, at this process's pid, as `serve`
    // holds it.
    let lease = ainb_hangar_client::PresenceLease::spawn_with(
        ainb_hangar_proto::connections::SurfaceInfo {
            kind: ainb_hangar_proto::connections::SurfaceKind::Web,
            pid: std::process::id(),
        },
        lease_dialer(home.path()),
    );
    wait_for_state(
        &lease,
        &ainb_hangar_client::PresenceState::Connected,
        Duration::from_secs(5),
    )
    .await;

    let mut watcher = Client::connect(&socket).await;
    watcher.hello(home.path(), Some("tui")).await;
    watcher.subscribe_connections().await;

    let web = ainb_web::daemon::web_client().expect("web client from env");
    for _ in 0..3 {
        web.attention_list_fleet().await.expect("one-shot web read is served");
    }

    if let Ok(event) = tokio::time::timeout(
        Duration::from_millis(300),
        watcher.next_connections_changed(),
    )
    .await
    {
        panic!("a one-shot web read changed the listed registry: {event}");
    }
    let listed = watcher.connections().await;
    let this_pid = u64::from(std::process::id());
    let this_process_rows = listed["connections"].as_array().map_or(0, |rows| {
        rows.iter()
            .filter(|row| row["surface"]["pid"].as_u64() == Some(this_pid))
            .count()
    });
    assert_eq!(
        this_process_rows, 1,
        "only the presence socket is listed for this process: {listed}"
    );
    lease.close().await;
}

/// Review finding on #998: the daemon, not the client, decides transient. A
/// transient hello with no listed presence at its pid is listed like any other
/// connection and announced to subscribers, so no client can hide by asking.
#[tokio::test]
async fn a_transient_hello_without_a_presence_at_its_pid_is_listed_and_announced() {
    let home = tempfile::tempdir().expect("temporary Hangar home");
    let (socket, _store) = start_server(home.path()).await;

    let mut watcher = Client::connect(&socket).await;
    watcher.hello(home.path(), Some("tui")).await;
    watcher.subscribe_connections().await;

    let mut hiding = Client::connect(&socket).await;
    hiding.hello_with(home.path(), "cli", 31337, true).await;

    let changed = watcher.next_connections_changed().await;
    let announced = changed["connections"]
        .as_array()
        .is_some_and(|rows| rows.iter().any(|row| row["surface"]["pid"] == 31337));
    assert!(
        announced,
        "the refused transient row must be announced: {changed}"
    );

    let listed = watcher.connections().await;
    assert!(
        listed["connections"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["surface"]["pid"] == 31337)),
        "the refused transient row must be listed: {listed}"
    );
    drop(hiding);
}
