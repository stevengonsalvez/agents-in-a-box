//! Integration tests for sessions table and SessionsRepo (migration 0101, spec P6d).

use ainb_hangar_store::Store;
use ainb_hangar_store::repo::sessions::{
    FileSession, ImportMarker, ImportOutcome, NameConflict, SessionRow, SessionsRepo,
    UpsertOutcome, reconcile_key,
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

fn from_file(row: &SessionRow) -> FileSession {
    FileSession {
        row: row.clone(),
        id_minted: false,
    }
}

/// P6e: the P6d import row alone does not make the table authoritative; the
/// import and a reconcile pass for the SAME file do.
#[tokio::test]
async fn import_complete_needs_the_import_and_a_reconcile_of_one_file() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let pool = store.pool();
    let source = "/home/u/.agents-in-a-box/sessions.json";
    let other = "/home/v/.agents-in-a-box/sessions.json";

    assert!(!SessionsRepo::import_complete_for(pool, source).await.unwrap());
    SessionsRepo::complete_import(pool, source, &[], 0, 1).await.unwrap();
    assert!(
        !SessionsRepo::import_complete_for(pool, source).await.unwrap(),
        "the import row alone must not report the table authoritative"
    );

    SessionsRepo::complete_reconcile(pool, other, &[], 0, 2).await.unwrap();
    assert!(
        !SessionsRepo::import_complete_for(pool, source).await.unwrap(),
        "another file's reconcile row must not complete this file"
    );

    SessionsRepo::complete_reconcile(pool, source, &[], 0, 3).await.unwrap();
    assert!(SessionsRepo::import_complete_for(pool, source).await.unwrap());
}

/// A reconcile inserts what the table lacks and never overwrites a row the
/// table already holds under the same id.
#[tokio::test]
async fn reconcile_inserts_missing_rows_and_the_table_wins_on_an_id() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let pool = store.pool();
    let source = "/home/u/.agents-in-a-box/sessions.json";

    let table_row = test_session(
        "00000000-0000-0000-0000-000000000001",
        "ainb-s1",
        "ws",
        1000,
    );
    SessionsRepo::upsert(pool, &table_row).await.unwrap();

    let mut stale = table_row.clone();
    stale.workspace_name = "older-ws".to_string();
    stale.headroom_enabled = false;
    let missing = test_session(
        "00000000-0000-0000-0000-000000000002",
        "ainb-s2",
        "ws",
        2000,
    );

    let out = SessionsRepo::complete_reconcile(
        pool,
        source,
        &[from_file(&stale), from_file(&missing)],
        0,
        7,
    )
    .await
    .unwrap();
    assert_eq!(out.marker.imported, 1);
    assert_eq!(out.marker.skipped, 0);
    assert!(out.conflicts.is_empty());

    let all = SessionsRepo::list(pool, None, 100).await.unwrap();
    assert_eq!(all, vec![missing, table_row]);
}

/// A file session whose tmux name the table binds to another id that the
/// file ALSO has (under a different name there; the table wins on contents)
/// is skipped, counted, and reported; the table row is unchanged.
#[tokio::test]
async fn reconcile_skips_and_counts_a_tmux_name_conflict() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let pool = store.pool();
    let source = "/home/u/.agents-in-a-box/sessions.json";

    let holder = test_session(
        "00000000-0000-0000-0000-000000000001",
        "ainb-s1",
        "ws",
        1000,
    );
    SessionsRepo::upsert(pool, &holder).await.unwrap();
    let rival = test_session(
        "00000000-0000-0000-0000-000000000009",
        "ainb-s1",
        "ws",
        1500,
    );

    // The file still has the holder, renamed there; the table keeps its row.
    let mut holder_in_file = holder.clone();
    holder_in_file.tmux_session_name = "ainb-s1-renamed".to_string();
    let out = SessionsRepo::complete_reconcile(
        pool,
        source,
        &[from_file(&holder_in_file), from_file(&rival)],
        2,
        7,
    )
    .await
    .unwrap();
    assert!(out.deleted.is_empty(), "{:?}", out.deleted);
    assert_eq!(
        out.marker,
        ImportMarker {
            source_path: reconcile_key(source),
            completed_at: 7,
            imported: 0,
            skipped: 1,
            rejected: 2,
        }
    );
    assert_eq!(
        out.conflicts,
        vec![NameConflict {
            session_id: rival.session_id.clone(),
            tmux_session_name: "ainb-s1".to_string(),
            holder: holder.session_id.clone(),
        }]
    );
    assert_eq!(
        SessionsRepo::list(pool, None, 100).await.unwrap(),
        vec![holder]
    );
}

/// A record whose id was minted on read matches the table by tmux name, so a
/// repeated pass neither duplicates it nor reports it as a conflict.
#[tokio::test]
async fn reconcile_matches_a_minted_id_by_tmux_name() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let pool = store.pool();
    let source = "/home/u/.agents-in-a-box/sessions.json";

    let first = test_session(
        "00000000-0000-0000-0000-000000000001",
        "ainb-s1",
        "ws",
        1000,
    );
    let minted = |row: &SessionRow| FileSession {
        row: row.clone(),
        id_minted: true,
    };
    let out = SessionsRepo::complete_reconcile(pool, source, &[minted(&first)], 0, 1)
        .await
        .unwrap();
    assert_eq!(out.marker.imported, 1);

    let reminted = test_session(
        "00000000-0000-0000-0000-000000000005",
        "ainb-s1",
        "ws",
        1000,
    );
    let out = SessionsRepo::complete_reconcile(pool, source, &[minted(&reminted)], 0, 2)
        .await
        .unwrap();
    assert_eq!(out.marker.imported, 0);
    assert!(out.conflicts.is_empty(), "{:?}", out.conflicts);
    assert_eq!(
        SessionsRepo::list(pool, None, 100).await.unwrap(),
        vec![first]
    );
}

/// Each pass rewrites the reconcile marker with its own counts, and never
/// touches the one-time import row.
#[tokio::test]
async fn reconcile_marker_carries_the_latest_pass() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let pool = store.pool();
    let source = "/home/u/.agents-in-a-box/sessions.json";

    SessionsRepo::complete_import(pool, source, &[], 0, 1).await.unwrap();
    let a = test_session(
        "00000000-0000-0000-0000-000000000001",
        "ainb-s1",
        "ws",
        1000,
    );
    SessionsRepo::complete_reconcile(pool, source, &[from_file(&a)], 0, 5)
        .await
        .unwrap();
    SessionsRepo::complete_reconcile(pool, source, &[from_file(&a)], 1, 9)
        .await
        .unwrap();

    let marker = SessionsRepo::import_marker(pool, &reconcile_key(source))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (marker.completed_at, marker.imported, marker.rejected),
        (9, 0, 1)
    );
    let import = SessionsRepo::import_marker(pool, source).await.unwrap().unwrap();
    assert_eq!(import.completed_at, 1);
}

/// Until the flip the file is the authority on which sessions exist: a table
/// row whose session the file does not have is deleted by the pass, and
/// returned in `deleted`, while a row the file has keeps its table contents.
#[tokio::test]
async fn reconcile_keeps_table_rows_the_file_does_not_have() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let pool = store.pool();
    let source = "/home/u/.agents-in-a-box/sessions.json";

    let kept = test_session(
        "00000000-0000-0000-0000-000000000001",
        "ainb-kept",
        "ws",
        1000,
    );
    let gone = test_session(
        "00000000-0000-0000-0000-000000000002",
        "ainb-gone",
        "ws",
        2000,
    );
    SessionsRepo::upsert(pool, &kept).await.unwrap();
    SessionsRepo::upsert(pool, &gone).await.unwrap();
    let mut stale = kept.clone();
    stale.workspace_name = "file-ws".to_string();

    let out = SessionsRepo::complete_reconcile(pool, source, &[from_file(&stale)], 0, 3)
        .await
        .unwrap();
    assert!(
        out.deleted.is_empty(),
        "a pass deleted on the file's word: {:?}",
        out.deleted
    );
    assert_eq!(out.marker.imported, 0);
    let mut left = SessionsRepo::list(pool, None, 100).await.unwrap();
    left.sort_by(|a, b| a.session_id.cmp(&b.session_id));
    assert_eq!(
        left,
        vec![kept, gone],
        "the row the mirror no longer names was taken from the table"
    );
}

/// A tmux name the table binds to an id the file no longer has stays where it
/// is: the pass deletes nothing, so the file's session is a name conflict and
/// the table's row keeps the name.
#[tokio::test]
async fn reconcile_gives_a_tmux_name_to_the_files_session() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let pool = store.pool();
    let source = "/home/u/.agents-in-a-box/sessions.json";

    let old = test_session(
        "00000000-0000-0000-0000-000000000001",
        "ainb-s1",
        "ws",
        1000,
    );
    SessionsRepo::upsert(pool, &old).await.unwrap();
    let new = test_session(
        "00000000-0000-0000-0000-000000000009",
        "ainb-s1",
        "ws",
        1500,
    );

    let out = SessionsRepo::complete_reconcile(pool, source, &[from_file(&new)], 0, 4)
        .await
        .unwrap();
    assert!(out.deleted.is_empty(), "{:?}", out.deleted);
    assert_eq!(out.marker.imported, 0);
    assert_eq!(
        out.conflicts.len(),
        1,
        "the name is still bound, so the file's session is a conflict"
    );
    assert_eq!(
        SessionsRepo::list(pool, None, 100).await.unwrap(),
        vec![old],
        "the table's row lost its name to the mirror"
    );
}

/// P6e-6: with no file to read, the mirror was lost rather than emptied. The
/// pass deletes nothing and still commits its marker, so readers are served
/// from the table instead of waiting for a file that may never come back.
#[tokio::test]
async fn a_post_flip_pass_with_no_file_keeps_every_row_and_still_commits() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let pool = store.pool();
    let source = "/home/p/.agents-in-a-box/sessions.json";

    let held = test_session(
        "00000000-0000-0000-0000-0000000000cc",
        "ainb-held",
        "ws",
        1_000,
    );
    SessionsRepo::upsert(pool, &held).await.unwrap();
    // The P6d import ran on this home before the flip, as it does on a real
    // one: what this test turns on is the reconcile that follows.
    SessionsRepo::complete_import(pool, source, &[], 0, 1_000).await.unwrap();

    let out = SessionsRepo::complete_reconcile(pool, source, &[], 0, 4_000).await.unwrap();

    assert!(out.deleted.is_empty(), "{:?}", out.deleted);
    assert_eq!(out.marker.imported, 0);
    let left = SessionsRepo::list(pool, None, 10).await.unwrap();
    assert_eq!(left.len(), 1, "the lost mirror emptied the table");
    assert!(
        SessionsRepo::import_complete_for(pool, source).await.unwrap(),
        "the pass did not commit its marker, so readers would never be served"
    );
}

/// P6e-6, migration safety: a home whose `sessions.json` holds sessions the
/// table has never seen keeps every one of them on the first pass after the
/// flip. The rows are inserted, nothing is dropped for being unknown.
#[tokio::test]
async fn the_first_post_flip_pass_keeps_the_files_own_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let pool = store.pool();
    let source = "/home/p/.agents-in-a-box/sessions.json";

    let older = test_session(
        "00000000-0000-0000-0000-0000000000d1",
        "ainb-older",
        "ws",
        1_000,
    );
    let newer = test_session(
        "00000000-0000-0000-0000-0000000000d2",
        "ainb-newer",
        "ws",
        8_000,
    );

    let out = SessionsRepo::complete_reconcile(
        pool,
        source,
        &[from_file(&older), from_file(&newer)],
        0,
        10_000,
    )
    .await
    .unwrap();

    assert_eq!(out.marker.imported, 2);
    assert!(out.deleted.is_empty(), "{:?}", out.deleted);
    let mut left: Vec<String> = SessionsRepo::list(pool, None, 10)
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.tmux_session_name)
        .collect();
    left.sort();
    assert_eq!(left, vec!["ainb-newer", "ainb-older"]);
}

/// P6e-6: a record that failed validation says nothing about the sessions that
/// did not. One malformed entry in the mirror must not cost a live row.
#[tokio::test]
async fn a_rejected_record_takes_no_row_with_it() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let pool = store.pool();
    let source = "/home/p/.agents-in-a-box/sessions.json";

    let live = test_session(
        "00000000-0000-0000-0000-0000000000f1",
        "ainb-live",
        "ws",
        1_000,
    );
    SessionsRepo::upsert(pool, &live).await.unwrap();

    // The whole file parsed to one rejected record and nothing usable.
    let out = SessionsRepo::complete_reconcile(pool, source, &[], 1, 2_000).await.unwrap();

    assert!(out.deleted.is_empty(), "{:?}", out.deleted);
    assert_eq!(out.marker.rejected, 1);
    assert_eq!(
        SessionsRepo::list(pool, None, 10).await.unwrap().len(),
        1,
        "a rejected record emptied the table"
    );
}

/// P6e-6: a mirror restored from an old backup names sessions that were killed
/// since. The pass adds what the table lacks and takes nothing away, so the
/// rows that are still live stay live.
#[tokio::test]
async fn a_stale_restored_mirror_takes_nothing_away() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let pool = store.pool();
    let source = "/home/p/.agents-in-a-box/sessions.json";

    let live = test_session(
        "00000000-0000-0000-0000-0000000000f2",
        "ainb-live",
        "ws",
        5_000,
    );
    SessionsRepo::upsert(pool, &live).await.unwrap();
    // The backup is from before that session existed and names another.
    let old = test_session(
        "00000000-0000-0000-0000-0000000000f3",
        "ainb-old",
        "ws",
        1_000,
    );

    let out = SessionsRepo::complete_reconcile(pool, source, &[from_file(&old)], 0, 6_000)
        .await
        .unwrap();

    assert!(out.deleted.is_empty(), "{:?}", out.deleted);
    let mut names: Vec<String> = SessionsRepo::list(pool, None, 10)
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.tmux_session_name)
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec!["ainb-live", "ainb-old"],
        "the live session did not survive a restored backup"
    );
}
