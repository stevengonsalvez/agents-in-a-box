//! Integration tests for sessions table and SessionsRepo (migration 0101, spec P6d).

use ainb_hangar_store::Store;
use ainb_hangar_store::repo::sessions::{
    ImportMarker, ImportOutcome, SessionRow, SessionsRepo, UpsertOutcome,
};

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

    let all = SessionsRepo::list(pool, None, 100).await.unwrap();
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
    let all = SessionsRepo::list(pool, None, 100).await.unwrap();
    assert_eq!(all.len(), 3);
    assert_eq!(all[0].session_id, s2.session_id);
    assert_eq!(all[1].session_id, s3.session_id);
    assert_eq!(all[2].session_id, s1.session_id);

    // List ws-1: s3 then s1
    let ws1 = SessionsRepo::list(pool, Some("ws-1"), 100).await.unwrap();
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

/// An upsert naming a tmux session already bound to a DIFFERENT session id
/// is refused, and the holder's row is left exactly as it was. The old
/// `DELETE ... OR tmux_session_name = ?` evicted the holder silently.
#[tokio::test]
async fn upsert_refuses_a_tmux_name_bound_to_another_session() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let pool = store.pool();

    let holder = test_session(
        "00000000-0000-0000-0000-000000000001",
        "ainb-shared",
        "ws-1",
        1000,
    );
    assert_eq!(
        SessionsRepo::upsert(pool, &holder).await.unwrap(),
        UpsertOutcome::Written
    );

    let intruder = test_session(
        "00000000-0000-0000-0000-000000000002",
        "ainb-shared",
        "ws-2",
        2000,
    );
    assert_eq!(
        SessionsRepo::upsert(pool, &intruder).await.unwrap(),
        UpsertOutcome::TmuxNameTaken {
            holder: holder.session_id.clone()
        }
    );

    let all = SessionsRepo::list(pool, None, 100).await.unwrap();
    assert_eq!(all, vec![holder]);
}

/// The same session id may move to a free tmux name: that is an update of
/// one identity, not an identity swap.
#[tokio::test]
async fn upsert_renames_a_session_to_a_free_tmux_name() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let pool = store.pool();

    let mut s1 = test_session(
        "00000000-0000-0000-0000-000000000001",
        "ainb-old",
        "ws-1",
        1000,
    );
    SessionsRepo::upsert(pool, &s1).await.unwrap();
    s1.tmux_session_name = "ainb-new".to_string();
    assert_eq!(
        SessionsRepo::upsert(pool, &s1).await.unwrap(),
        UpsertOutcome::Written
    );

    assert_eq!(
        SessionsRepo::get_by_tmux_name(pool, "ainb-old").await.unwrap(),
        None
    );
    assert_eq!(
        SessionsRepo::get_by_tmux_name(pool, "ainb-new").await.unwrap(),
        Some(s1)
    );
}

/// `list` returns at most `limit` rows, newest first.
#[tokio::test]
async fn list_stops_at_the_limit() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let pool = store.pool();

    for i in 1..=5 {
        let s = test_session(
            &format!("00000000-0000-0000-0000-00000000000{i}"),
            &format!("ainb-s{i}"),
            "ws",
            i64::from(i) * 1000,
        );
        SessionsRepo::upsert(pool, &s).await.unwrap();
    }

    let two = SessionsRepo::list(pool, None, 2).await.unwrap();
    assert_eq!(two.len(), 2);
    assert_eq!(two[0].tmux_session_name, "ainb-s5");
    assert_eq!(two[1].tmux_session_name, "ainb-s4");
}

/// A fresh home has no import marker.
#[tokio::test]
async fn a_fresh_home_has_no_import_marker() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();

    let marker =
        SessionsRepo::import_marker(store.pool(), "/home/u/.agents-in-a-box/sessions.json")
            .await
            .unwrap();
    assert_eq!(marker, None);
}

/// `complete_import` writes the rows and the marker in one transaction, skips
/// rows whose id or tmux name is already present, and refuses to run twice
/// for one source.
#[tokio::test]
async fn complete_import_writes_rows_and_marker_once() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let pool = store.pool();
    let source = "/home/u/.agents-in-a-box/sessions.json";

    let existing = test_session(
        "00000000-0000-0000-0000-000000000001",
        "ainb-s1",
        "ws",
        1000,
    );
    SessionsRepo::upsert(pool, &existing).await.unwrap();

    let same_name = test_session(
        "00000000-0000-0000-0000-000000000009",
        "ainb-s1",
        "ws",
        1500,
    );
    let fresh = test_session(
        "00000000-0000-0000-0000-000000000002",
        "ainb-s2",
        "ws",
        2000,
    );

    let outcome = SessionsRepo::complete_import(pool, source, &[same_name, fresh.clone()], 3, 42)
        .await
        .unwrap();
    assert_eq!(
        outcome,
        ImportOutcome::Completed(ImportMarker {
            source_path: source.to_string(),
            completed_at: 42,
            imported: 1,
            skipped: 1,
            rejected: 3,
        })
    );

    let all = SessionsRepo::list(pool, None, 100).await.unwrap();
    assert_eq!(all, vec![fresh.clone(), existing]);

    let again = SessionsRepo::complete_import(pool, source, &[fresh], 0, 99).await.unwrap();
    assert_eq!(again, ImportOutcome::AlreadyCompleted);
    let marker = SessionsRepo::import_marker(pool, source).await.unwrap().unwrap();
    assert_eq!(marker.completed_at, 42);
}
