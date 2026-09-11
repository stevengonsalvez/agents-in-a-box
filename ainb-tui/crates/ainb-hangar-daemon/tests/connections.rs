//! Integration coverage for the daemon's in-memory surface connection registry.

use std::path::Path;
use std::time::{Duration, Instant};

use ainb_hangar_daemon::rpc::{self, DaemonHealth};
use ainb_hangar_proto::events::EVENT_METHOD;
use ainb_hangar_proto::{RpcId, RpcRequest, methods};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::attention::{AttentionKind, AttentionRepo, NewAttention};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

const CONNECTIONS_LIST: &str = "hangar/connections_list";

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
