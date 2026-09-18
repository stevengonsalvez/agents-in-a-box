//! Integration tests for sessions table and SessionsRepo (migration 0101, spec P6d).

use ainb_hangar_store::Store;
use ainb_hangar_store::repo::sessions::{SessionRow, SessionsRepo};

fn test_session(id: &str, tmux: &str, ws: &str, created_at: i64) -> SessionRow {
    SessionRow {
        session_id: id.to_string(),
        tmux_session_name: tmux.to_string(),
        worktree_path: format!("/tmp/worktrees/{tmux}"),
        workspace_name: ws.to_string(),
        created_at,
        agent_type: "Claude".to_string(),
        headroom_enabled: true,
        rtk_enabled: false,
        skip_permissions: Some(true),
        model: Some("claude-sonnet-4".to_string()),
        model_source: "Raw".to_string(),
        codex_model: None,
        codex_thread_id: Some("thread-xyz".to_string()),
    }
}

#[tokio::test]
async fn sessions_round_trip_all_thirteen_fields() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let pool = store.pool();

    let s1 = test_session(
        "00000000-0000-0000-0000-000000000001",
        "ainb-alpha",
        "workspace-a",
        1000,
    );
    SessionsRepo::upsert(pool, &s1).await.unwrap();

    let fetched = SessionsRepo::get_by_id(pool, &s1.session_id).await.unwrap();
    assert_eq!(fetched, Some(s1.clone()));

    let by_tmux = SessionsRepo::get_by_tmux_name(pool, "ainb-alpha").await.unwrap();
    assert_eq!(by_tmux, Some(s1.clone()));

    let all = SessionsRepo::list(pool, None).await.unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0], s1);
}

#[tokio::test]
async fn sessions_list_filtering_and_ordering() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let pool = store.pool();

    let s1 = test_session(
        "00000000-0000-0000-0000-000000000001",
        "ainb-s1",
        "ws-1",
        1000,
    );
    let s2 = test_session(
        "00000000-0000-0000-0000-000000000002",
        "ainb-s2",
        "ws-2",
        3000,
    );
    let s3 = test_session(
        "00000000-0000-0000-0000-000000000003",
        "ainb-s3",
        "ws-1",
        2000,
    );

    SessionsRepo::upsert(pool, &s1).await.unwrap();
    SessionsRepo::upsert(pool, &s2).await.unwrap();
    SessionsRepo::upsert(pool, &s3).await.unwrap();

    // List all: newest first (s2 at 3000, s3 at 2000, s1 at 1000)
    let all = SessionsRepo::list(pool, None).await.unwrap();
    assert_eq!(all.len(), 3);
    assert_eq!(all[0].session_id, s2.session_id);
    assert_eq!(all[1].session_id, s3.session_id);
    assert_eq!(all[2].session_id, s1.session_id);

    // List ws-1: s3 then s1
    let ws1 = SessionsRepo::list(pool, Some("ws-1")).await.unwrap();
    assert_eq!(ws1.len(), 2);
    assert_eq!(ws1[0].session_id, s3.session_id);
    assert_eq!(ws1[1].session_id, s1.session_id);
}

#[tokio::test]
async fn sessions_upsert_and_deletion() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let pool = store.pool();

    let mut s1 = test_session(
        "00000000-0000-0000-0000-000000000001",
        "ainb-s1",
        "ws-1",
        1000,
    );
    SessionsRepo::upsert(pool, &s1).await.unwrap();

    // Update fields
    s1.workspace_name = "ws-updated".to_string();
    s1.headroom_enabled = false;
    SessionsRepo::upsert(pool, &s1).await.unwrap();

    let updated = SessionsRepo::get_by_id(pool, &s1.session_id).await.unwrap();
    assert_eq!(updated, Some(s1.clone()));

    // Delete by tmux name
    assert!(SessionsRepo::delete_by_tmux_name(pool, "ainb-s1").await.unwrap());
    assert!(!SessionsRepo::delete_by_tmux_name(pool, "ainb-s1").await.unwrap());
    assert_eq!(
        SessionsRepo::get_by_id(pool, &s1.session_id).await.unwrap(),
        None
    );

    // Re-insert and delete by ID
    SessionsRepo::upsert(pool, &s1).await.unwrap();
    assert!(SessionsRepo::delete_by_id(pool, &s1.session_id).await.unwrap());
    assert_eq!(
        SessionsRepo::get_by_tmux_name(pool, "ainb-s1").await.unwrap(),
        None
    );
}
