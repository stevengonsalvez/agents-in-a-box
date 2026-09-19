//! The `workspace/session_reconcile` RPC against the daemon's own
//! `sessions.json` (P6e).
//!
//! A binary of its own because the daemon resolves that file from
//! `AINB_HOME`, and setting a process-wide variable races every other test
//! thread in a shared binary. Keep it to one test.

use ainb_hangar_store::Store;
use std::fs;
use std::path::Path;

/// Write a `sessions.json` holding `sessions` as `(id, tmux name, workspace)`.
fn sessions_file(path: &Path, sessions: &[(&str, &str, &str)]) {
    let mut map = serde_json::Map::new();
    for (id, tmux, ws) in sessions {
        map.insert(
            (*tmux).to_string(),
            serde_json::json!({
                "session_id": id,
                "tmux_session_name": tmux,
                "worktree_path": format!("/home/user/work/{tmux}"),
                "workspace_name": ws,
                "created_at": 1_757_937_600_000_i64,
                "agent_type": "Claude"
            }),
        );
    }
    let json = serde_json::json!({ "sessions": map });
    fs::write(path, serde_json::to_string_pretty(&json).unwrap()).unwrap();
}

/// `workspace/session_reconcile` runs a pass on the daemon's own file and
/// answers once it has committed; `session_list` then reports the table
/// authoritative.
#[tokio::test]
async fn the_reconcile_rpc_runs_a_pass_on_the_daemons_file() {
    use ainb_hangar_daemon::events::EventBroker;
    use ainb_hangar_daemon::health_stats::HealthStats;
    use ainb_hangar_daemon::rpc::{self, DaemonHealth};
    use ainb_hangar_proto::{RpcId, RpcRequest, methods};

    let home = tempfile::tempdir().unwrap();
    std::env::set_var("AINB_HOME", home.path());
    let sessions_path = ainb_hangar_daemon::session_import::daemon_sessions_path();
    assert!(sessions_path.starts_with(home.path()));
    fs::create_dir_all(sessions_path.parent().unwrap()).unwrap();
    let id = "00000000-0000-0000-0000-0000000000c9";
    sessions_file(&sessions_path, &[(id, "ainb-rpc", "ws")]);

    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let pool = store.pool();
    let health = DaemonHealth {
        socket_path: "/tmp/it-session-reconcile.sock".into(),
        pid: 1,
        started_at: std::time::Instant::now(),
        version: "0.1.0".into(),
        stats: std::sync::Arc::new(HealthStats::default()),
    };
    let sink = EventBroker::new().sink();
    let call = |method: &str| RpcRequest {
        jsonrpc: ainb_hangar_proto::jsonrpc_version(),
        id: RpcId::Number(1),
        method: method.into(),
        params: serde_json::Value::Null,
    };

    let before = rpc::dispatch(pool, &call(methods::WORKSPACE_SESSION_LIST), &health, &sink).await;
    assert_eq!(before.result.unwrap()["import_complete"], false);

    let done = rpc::dispatch(
        pool,
        &call(methods::WORKSPACE_SESSION_RECONCILE),
        &health,
        &sink,
    )
    .await;
    assert!(done.error.is_none(), "{done:?}");
    assert!(done.result.unwrap()["completed_at"].as_i64().unwrap() > 0);

    let after = rpc::dispatch(pool, &call(methods::WORKSPACE_SESSION_LIST), &health, &sink)
        .await
        .result
        .unwrap();
    assert_eq!(after["import_complete"], true);
    assert_eq!(after["sessions"][0]["session_id"], id);
}
