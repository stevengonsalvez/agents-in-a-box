//! Integration tests for CLI session resolution and daemon-backed storage (P6d).

use std::fs;
use std::path::PathBuf;
use chrono::Utc;
use uuid::Uuid;

use ainb::cli::ListArgs;
use ainb::cli::list::list_sessions;
use ainb::cli::util::mutate_session_store;
use ainb::interactive::session_manager::{ModelSource, SessionMetadata, SessionStore};
use ainb::models::session::SessionAgentType;
use ainb_hangar_store::repo::sessions::{NewWorkspaceSession, SessionsRepo};

#[path = "support/fleet_hangar.rs"]
mod fleet_hangar;

use fleet_hangar::{EnvGuard, FleetHangar};

fn make_session(name: &str, ws: &str) -> SessionMetadata {
    SessionMetadata {
        session_id: Uuid::new_v4(),
        tmux_session_name: name.to_string(),
        worktree_path: PathBuf::from(format!("/tmp/work/{ws}")),
        workspace_name: ws.to_string(),
        created_at: Utc::now(),
        agent_type: SessionAgentType::Claude,
        headroom_enabled: false,
        rtk_enabled: false,
        skip_permissions: Some(true),
        model: Some("claude-3-5-sonnet".to_string()),
        model_source: ModelSource::Raw,
        codex_model: None,
        codex_thread_id: None,
    }
}

/// Test that a CLI read works with the daemon stopped.
#[tokio::test]
async fn test_cli_read_works_with_daemon_stopped() {
    let home = tempfile::tempdir().expect("create tempdir");
    let _ainb_home = EnvGuard::set("AINB_HOME", home.path());
    let _hangar_home = EnvGuard::set("AINB_HANGAR_HOME", home.path().join("no-such-hangar"));

    // Write a local sessions.json directly
    let mut store = SessionStore::default();
    let meta = make_session("sess-stopped-1", "stopped-ws");
    let sid = meta.session_id.to_string();
    store.upsert(meta);

    let store_dir = home.path().join(".agents-in-a-box");
    fs::create_dir_all(&store_dir).expect("create store dir");
    fs::write(
        store_dir.join("sessions.json"),
        serde_json::to_string_pretty(&store).expect("serialize store"),
    )
    .expect("write sessions.json");

    let sessions = list_sessions(&ListArgs::default())
        .await
        .expect("list_sessions should succeed with daemon stopped");

    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].session_id, sid);
    assert_eq!(sessions[0].workspace_name, "stopped-ws");
}

/// Test that a session created by the daemon and one created by ainb run both appear once in ainb list.
#[tokio::test]
async fn test_session_created_by_daemon_and_by_run_both_appear_once_in_list() {
    let root = tempfile::tempdir().expect("create root");
    let home = root.path().join("home");
    let hangar_home = root.path().join("hangar");
    fs::create_dir_all(&home).expect("create home");
    fs::create_dir_all(&hangar_home).expect("create hangar");

    let _ainb_home = EnvGuard::set("AINB_HOME", &home);
    let _hangar_home = EnvGuard::set("AINB_HANGAR_HOME", &hangar_home);

    let hangar = FleetHangar::start(&hangar_home);

    // 1. Session created by daemon directly in the repo
    let daemon_sid = Uuid::new_v4().to_string();
    let daemon_sess = NewWorkspaceSession {
        session_id: &daemon_sid,
        tmux_session_name: "sess-daemon-1",
        worktree_path: "/tmp/work/daemon-ws",
        workspace_name: "daemon-ws",
        created_at: Utc::now().timestamp_millis(),
        agent_type: "Claude",
        headroom_enabled: false,
        rtk_enabled: false,
        skip_permissions: Some(false),
        model: Some("claude-3-5-haiku"),
        model_source: "Raw",
        codex_model: None,
        codex_thread_id: None,
    };
    hangar.block_on(async {
        SessionsRepo::upsert(hangar.pool(), daemon_sess)
            .await
            .expect("daemon upsert session");
    });

    // 2. Session created by ainb run (using mutate_session_store)
    let run_meta = make_session("sess-run-1", "run-ws");
    let run_sid = run_meta.session_id.to_string();
    mutate_session_store(|s| s.upsert(run_meta)).expect("mutate session store");

    // Act: list sessions
    let sessions = list_sessions(&ListArgs::default())
        .await
        .expect("list_sessions with daemon running");

    // Assert: both appear once, no duplicates
    assert_eq!(sessions.len(), 2, "must have exactly 2 sessions");

    let daemon_matches: Vec<_> = sessions.iter().filter(|s| s.session_id == daemon_sid).collect();
    assert_eq!(daemon_matches.len(), 1, "daemon session must appear once");
    assert_eq!(daemon_matches[0].workspace_name, "daemon-ws");

    let run_matches: Vec<_> = sessions.iter().filter(|s| s.session_id == run_sid).collect();
    assert_eq!(run_matches.len(), 1, "run session must appear once");
    assert_eq!(run_matches[0].workspace_name, "run-ws");
}
