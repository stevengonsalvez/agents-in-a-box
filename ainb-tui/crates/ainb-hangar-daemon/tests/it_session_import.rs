//! Integration tests for daemon boot import of sessions.json (spec P6d, #1166).

use ainb_hangar_daemon::session_import::import_sessions_if_needed;
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::sessions::SessionsRepo;
use std::fs;

/// 1. A fresh home imports nothing (file does not exist or has empty sessions).
#[tokio::test]
async fn test_fresh_home_imports_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let pool = store.pool();

    let sessions_path = dir.path().join("sessions.json");
    let count = import_sessions_if_needed(pool, &sessions_path).await.unwrap();
    assert_eq!(count, 0, "fresh home must import 0 sessions");

    let rows = SessionsRepo::list(pool, None).await.unwrap();
    assert!(rows.is_empty(), "sessions table must be empty");
}

/// 2. A populated file imports every record once and leaves the file in place.
#[tokio::test]
async fn test_populated_file_imports_every_record_once() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let pool = store.pool();

    let sessions_path = dir.path().join("sessions.json");
    let sample_json = serde_json::json!({
        "sessions": {
            "ainb-sess-alpha": {
                "session_id": "00000000-0000-0000-0000-000000000001",
                "tmux_session_name": "ainb-sess-alpha",
                "worktree_path": "/home/user/work/alpha",
                "workspace_name": "alpha-ws",
                "created_at": "2026-09-15T12:00:00Z",
                "agent_type": "Claude",
                "headroom_enabled": true,
                "rtk_enabled": false,
                "skip_permissions": true,
                "model": "claude-3-5-sonnet",
                "model_source": "LegacyTyped",
                "codex_model": null,
                "codex_thread_id": null
            },
            "ainb-sess-beta": {
                "session_id": "00000000-0000-0000-0000-000000000002",
                "tmux_session_name": "ainb-sess-beta",
                "worktree_path": "/home/user/work/beta",
                "workspace_name": "beta-ws",
                "created_at": 1757937600000i64,
                "agent_type": "Codex",
                "headroom_enabled": false,
                "rtk_enabled": true,
                "skip_permissions": false,
                "model": "o3-mini",
                "model_source": "Raw",
                "codex_model": "codex-standard",
                "codex_thread_id": "thread-xyz"
            }
        }
    });
    fs::write(
        &sessions_path,
        serde_json::to_string_pretty(&sample_json).unwrap(),
    )
    .unwrap();

    let count = import_sessions_if_needed(pool, &sessions_path).await.unwrap();
    assert_eq!(count, 2, "must import every record once");

    // Verify records landed in database with all 13 fields intact
    let rows = SessionsRepo::list(pool, None).await.unwrap();
    assert_eq!(rows.len(), 2);

    let alpha = SessionsRepo::get_by_tmux_name(pool, "ainb-sess-alpha").await.unwrap().unwrap();
    assert_eq!(alpha.session_id, "00000000-0000-0000-0000-000000000001");
    assert_eq!(alpha.workspace_name, "alpha-ws");
    assert_eq!(alpha.agent_type, "Claude");
    assert!(alpha.headroom_enabled);
    assert!(!alpha.rtk_enabled);
    assert_eq!(alpha.skip_permissions, Some(true));
    assert_eq!(alpha.model.as_deref(), Some("claude-3-5-sonnet"));
    assert_eq!(alpha.model_source, "LegacyTyped");

    let beta = SessionsRepo::get_by_tmux_name(pool, "ainb-sess-beta").await.unwrap().unwrap();
    assert_eq!(beta.session_id, "00000000-0000-0000-0000-000000000002");
    assert_eq!(beta.workspace_name, "beta-ws");
    assert_eq!(beta.agent_type, "Codex");
    assert!(!beta.headroom_enabled);
    assert!(beta.rtk_enabled);
    assert_eq!(beta.skip_permissions, Some(false));
    assert_eq!(beta.model.as_deref(), Some("o3-mini"));
    assert_eq!(beta.model_source, "Raw");
    assert_eq!(beta.codex_model.as_deref(), Some("codex-standard"));
    assert_eq!(beta.codex_thread_id.as_deref(), Some("thread-xyz"));

    // Verify file is still in place
    assert!(
        sessions_path.exists(),
        "import must leave sessions.json in place"
    );
}

/// 3. A second boot imports nothing further.
#[tokio::test]
async fn test_second_boot_imports_nothing_further() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let pool = store.pool();

    let sessions_path = dir.path().join("sessions.json");
    let sample_json = serde_json::json!({
        "sessions": {
            "ainb-sess-gamma": {
                "session_id": "00000000-0000-0000-0000-000000000003",
                "tmux_session_name": "ainb-sess-gamma",
                "worktree_path": "/home/user/work/gamma",
                "workspace_name": "gamma-ws",
                "created_at": 1757937600000i64,
                "agent_type": "Claude"
            }
        }
    });
    fs::write(
        &sessions_path,
        serde_json::to_string_pretty(&sample_json).unwrap(),
    )
    .unwrap();

    // First boot: imports 1
    let first = import_sessions_if_needed(pool, &sessions_path).await.unwrap();
    assert_eq!(first, 1);

    // Second boot against same DB and same file: imports 0
    let second = import_sessions_if_needed(pool, &sessions_path).await.unwrap();
    assert_eq!(second, 0, "second boot must import nothing further");

    let rows = SessionsRepo::list(pool, None).await.unwrap();
    assert_eq!(rows.len(), 1, "table row count must remain 1");
}
