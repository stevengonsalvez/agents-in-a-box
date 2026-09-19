//! The real daemon binary opens its socket at once, but serves no session
//! read from the table until this boot's first reconcile pass has committed
//! (P6e, amended: "socket opens at once, first read waits for the first
//! pass").
//!
//! The home is seeded as a previous boot left it: import and reconcile
//! markers present, and a table that no longer matches `sessions.json`. Then
//! `sessions.json.lock` is held so the boot pass cannot run. A read in that
//! window must not be served from that stale table.

use ainb_hangar_client::{DaemonClient, WorkspaceSessionListParams};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::sessions::{SessionRow, SessionsRepo};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const KEPT: &str = "00000000-0000-0000-0000-00000000f001";
const NEW: &str = "00000000-0000-0000-0000-00000000f002";

/// The daemon child, killed by its own pid when the test ends.
struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn row(id: &str, tmux: &str) -> SessionRow {
    SessionRow {
        session_id: id.to_string(),
        tmux_session_name: tmux.to_string(),
        worktree_path: format!("/home/user/work/{tmux}"),
        workspace_name: "ws".to_string(),
        created_at: 1_757_937_600_000,
        agent_type: "Claude".to_string(),
        headroom_enabled: false,
        rtk_enabled: false,
        skip_permissions: None,
        model: None,
        model_source: "LegacyTyped".to_string(),
        codex_model: None,
        codex_thread_id: None,
    }
}

fn write_sessions(path: &Path) {
    let entry = |id: &str, tmux: &str| {
        serde_json::json!({
            "session_id": id,
            "tmux_session_name": tmux,
            "worktree_path": format!("/home/user/work/{tmux}"),
            "workspace_name": "ws",
            "created_at": 1_757_937_600_000_i64,
            "agent_type": "Claude"
        })
    };
    let json = serde_json::json!({
        "sessions": { "ainb-kept": entry(KEPT, "ainb-kept"), "ainb-new": entry(NEW, "ainb-new") }
    });
    std::fs::write(path, serde_json::to_string_pretty(&json).unwrap()).unwrap();
}

fn ids(sessions: &[ainb_hangar_client::WorkspaceSessionEntry]) -> Vec<&str> {
    let mut ids: Vec<&str> = sessions.iter().map(|s| s.session_id.as_str()).collect();
    ids.sort_unstable();
    ids
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_read_during_a_held_lock_is_not_served_from_the_table() {
    let home = tempfile::tempdir().unwrap();
    let hangar = home.path().join(".agents-in-a-box");
    std::fs::create_dir_all(hangar.join("config")).unwrap();
    let sessions_path = hangar.join("sessions.json");
    let source = sessions_path.to_string_lossy().into_owned();

    // A previous boot's leftovers: both markers, and a table holding only
    // KEPT, while the file now also holds NEW.
    {
        let store = Store::open_in(&hangar).await.unwrap();
        let pool = store.pool();
        SessionsRepo::upsert(pool, &row(KEPT, "ainb-kept")).await.unwrap();
        SessionsRepo::complete_import(pool, &source, &[], 0, 1).await.unwrap();
        SessionsRepo::complete_reconcile(pool, &source, &[], 0, 2).await.unwrap();
        pool.close().await;
    }
    write_sessions(&sessions_path);

    let lock = ainb_fleet_core::session_registry::lock_sessions_store_at(&hangar).unwrap();
    let spawned = Instant::now();
    let _daemon = Daemon(
        Command::new(env!("CARGO_BIN_EXE_ainb-hangar-daemon"))
            .env("HOME", home.path())
            .env_remove("AINB_HANGAR_HOME")
            .env_remove("AINB_HOME")
            .env("HANGAR_TEST_PARENT_PID", std::process::id().to_string())
            .env("HANGAR_DAEMON_DISABLE_CLAIM", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn ainb-hangar-daemon"),
    );

    let socket = ainb_hangar_client::socket_path_in(&hangar);
    let token_file = ainb_hangar_proto::auth::token_file_in(&hangar);
    let deadline = Instant::now() + Duration::from_secs(20);
    while !(socket.exists() && token_file.exists()) {
        assert!(
            Instant::now() < deadline,
            "the daemon never opened its socket"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        spawned.elapsed() < Duration::from_secs(10),
        "the socket waited on the held lock: {:?}",
        spawned.elapsed()
    );
    let token = std::fs::read_to_string(&token_file).unwrap().trim().to_string();
    let client = DaemonClient::with_parts(socket, token);

    // The boot pass is stuck on the lock this test holds. The read waits for
    // it, then says not-ready instead of serving the stale table.
    let asked = Instant::now();
    let early = client
        .workspace_session_list(WorkspaceSessionListParams::default())
        .await
        .expect("session_list answers while the pass is stuck");
    let waited = asked.elapsed();
    assert!(
        !early.import_complete,
        "served as authoritative before the first pass"
    );
    assert!(
        early.sessions.is_empty(),
        "served table rows before the first pass: {early:?}"
    );
    assert!(
        waited < Duration::from_secs(5),
        "the read was not bounded: {waited:?}"
    );

    // With the lock free, a pass commits and reads are served, NEW included.
    drop(lock);
    client
        .workspace_session_reconcile()
        .await
        .expect("reconcile once the lock is free");
    let ready = client
        .workspace_session_list(WorkspaceSessionListParams::default())
        .await
        .unwrap();
    assert!(ready.import_complete);
    assert_eq!(ids(&ready.sessions), vec![KEPT, NEW]);
}
