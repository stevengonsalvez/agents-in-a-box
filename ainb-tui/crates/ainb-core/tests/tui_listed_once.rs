//! #1040: a running TUI is ONE `tui` row in `hangar connections list`.
//!
//! A TUI holds its presence lease (`main.rs` `spawn_tui_presence`), and once
//! its hangar screen is open the hangar plugin, a child process of the TUI,
//! holds a daemon connection of its own. Before #1040 the plugin announced its
//! own pid with no `transient` marker, so the registry listed it as a second
//! `tui` row. This test starts both against a real daemon, the plugin dial
//! built by the plugin's own `auth_hello_params`, and asserts one row.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ainb::fleet::bridge::daemon::{DaemonClient, PresenceLease};
use ainb_hangar_daemon::events::EventBroker;
use ainb_hangar_daemon::rpc::{self, DaemonHealth};
use ainb_hangar_proto::connections::{SurfaceInfo, SurfaceKind};
use ainb_hangar_store::Store;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

async fn start_daemon(home: &Path) -> (Store, PathBuf, String) {
    let store = Store::open_in(home).await.expect("open store");
    rpc::auth::ensure_socket_token(store.pool(), home).await.expect("socket token");
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
        EventBroker::new(),
    ));
    let token = std::fs::read_to_string(ainb_hangar_proto::auth::token_file_in(home))
        .expect("token")
        .trim()
        .to_string();
    (store, socket, token)
}

/// Dial the daemon with `params` as the `auth/hello`, the way the plugin runtime
/// relays the hangar plugin's first frame, and hold the connection open.
async fn plugin_dial(socket: &Path, params: serde_json::Value) -> UnixStream {
    let mut stream = UnixStream::connect(socket).await.expect("dial daemon");
    let body = serde_json::to_vec(&serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "auth/hello", "params": params,
    }))
    .unwrap();
    stream
        .write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes())
        .await
        .unwrap();
    stream.write_all(&body).await.unwrap();
    stream.flush().await.unwrap();
    let mut reader = BufReader::new(&mut stream);
    let mut length = 0;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).await.expect("hello reply");
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            length = value.trim().parse().expect("length");
        }
    }
    let mut reply = vec![0; length];
    reader.read_exact(&mut reply).await.expect("hello body");
    let reply: serde_json::Value = serde_json::from_slice(&reply).expect("hello json");
    assert!(reply.get("error").is_none(), "hello refused: {reply}");
    stream
}

async fn tui_rows(client: &DaemonClient) -> Vec<u32> {
    client
        .connections_list()
        .await
        .expect("connections list")
        .connections
        .into_iter()
        .filter(|row| row.surface.kind == SurfaceKind::Tui)
        .map(|row| row.surface.pid)
        .collect()
}

async fn wait_for_rows(client: &DaemonClient, want: usize) -> Vec<u32> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let rows = tui_rows(client).await;
        if rows.len() == want {
            return rows;
        }
        assert!(
            Instant::now() < deadline,
            "tui rows {rows:?}, wanted {want}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tui_with_its_hangar_plugin_dialled_is_one_tui_row() {
    let home = tempfile::tempdir().expect("home");
    let (_store, socket, token) = start_daemon(home.path()).await;
    let tui_pid = std::process::id();

    let lease_socket = socket.clone();
    let lease_token = token.clone();
    let _lease = PresenceLease::spawn_with(
        SurfaceInfo {
            kind: SurfaceKind::Tui,
            pid: tui_pid,
        },
        Box::new(move || {
            Ok(DaemonClient::with_parts(
                lease_socket.clone(),
                lease_token.clone(),
            ))
        }),
    );
    let lister = DaemonClient::with_parts(socket.clone(), token.clone());
    assert_eq!(
        wait_for_rows(&lister, 1).await,
        [tui_pid],
        "the lease is the TUI's row"
    );

    // The hangar screen opens: the plugin dials with its own hello, naming the
    // TUI as its host.
    let _plugin = plugin_dial(
        &socket,
        ainb_plugin_hangar::plugin::auth_hello_params(&token, tui_pid),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        tui_rows(&lister).await,
        [tui_pid],
        "the plugin's connection folds into the TUI's presence"
    );

    // The pre-#1040 hello (the plugin's own pid, no `transient`) is what
    // produced the second row; the registry still lists it, so this test
    // would catch the plugin regressing to it.
    let _old_plugin = plugin_dial(
        &socket,
        serde_json::json!({
            "token": token,
            "surface": { "kind": "tui", "pid": tui_pid.wrapping_add(1) },
        }),
    )
    .await;
    assert_eq!(
        wait_for_rows(&lister, 2).await.len(),
        2,
        "the old hello is listed as a second tui row"
    );
}
