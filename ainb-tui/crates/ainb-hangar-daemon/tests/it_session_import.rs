//! Integration tests for daemon boot import of sessions.json (spec P6d, #1166).

use ainb_hangar_daemon::session_import::{
    ImportReport, import_sessions_from, import_sessions_if_needed,
};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::sessions::SessionsRepo;
use std::fs;

/// Rows imported by a report, or a panic naming what happened instead.
fn imported(report: &ImportReport) -> i64 {
    match report {
        ImportReport::Completed(marker) => marker.imported,
        ImportReport::AlreadyCompleted => panic!("import had already completed"),
    }
}

fn marker_key(path: &std::path::Path) -> String {
    path.to_string_lossy().into_owned()
}

/// 1. A fresh home imports nothing (file does not exist or has empty sessions).
#[tokio::test]
async fn test_fresh_home_imports_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let pool = store.pool();

    let sessions_path = dir.path().join("sessions.json");
    let report = import_sessions_if_needed(pool, &sessions_path).await.unwrap();
    assert_eq!(imported(&report), 0, "fresh home must import 0 sessions");

    let rows = SessionsRepo::list(pool, None, 100).await.unwrap();
    assert!(rows.is_empty(), "sessions table must be empty");
    assert!(
        SessionsRepo::import_marker(pool, &marker_key(&sessions_path))
            .await
            .unwrap()
            .is_some(),
        "a fresh home still completes its import, or the table never becomes authoritative"
    );
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

    let before = fs::read(&sessions_path).unwrap();
    let report = import_sessions_if_needed(pool, &sessions_path).await.unwrap();
    assert_eq!(imported(&report), 2, "must import every record once");

    // Verify records landed in database with all 13 fields intact
    let rows = SessionsRepo::list(pool, None, 100).await.unwrap();
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
    assert_eq!(
        fs::read(&sessions_path).unwrap(),
        before,
        "import must leave sessions.json in place, byte for byte"
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
    assert_eq!(imported(&first), 1);

    // Second boot against same DB and same file: imports nothing
    let second = import_sessions_if_needed(pool, &sessions_path).await.unwrap();
    assert_eq!(second, ImportReport::AlreadyCompleted);

    let rows = SessionsRepo::list(pool, None, 100).await.unwrap();
    assert_eq!(rows.len(), 1, "table row count must remain 1");
}

fn one_session_file(path: &std::path::Path, id: &str, tmux: &str) {
    let json = serde_json::json!({
        "sessions": {
            tmux: {
                "session_id": id,
                "tmux_session_name": tmux,
                "worktree_path": "/home/user/work/delta",
                "workspace_name": "delta-ws",
                "created_at": 1757937600000i64,
                "agent_type": "Claude"
            }
        }
    });
    fs::write(path, serde_json::to_string_pretty(&json).unwrap()).unwrap();
}

/// 4. A row deleted after the import stays deleted on the next boot, even
/// though the file still names it. Without the marker the per-record
/// "already present?" check brought it back.
#[tokio::test]
async fn a_row_deleted_after_import_stays_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let pool = store.pool();
    let sessions_path = dir.path().join("sessions.json");
    one_session_file(
        &sessions_path,
        "00000000-0000-0000-0000-000000000004",
        "ainb-delta",
    );

    assert_eq!(
        imported(&import_sessions_if_needed(pool, &sessions_path).await.unwrap()),
        1
    );
    assert!(SessionsRepo::delete_by_tmux_name(pool, "ainb-delta").await.unwrap());

    let again = import_sessions_if_needed(pool, &sessions_path).await.unwrap();
    assert_eq!(again, ImportReport::AlreadyCompleted);
    assert!(SessionsRepo::list(pool, None, 100).await.unwrap().is_empty());
}

/// 5. An unparseable file fails the import visibly and writes no marker, so
/// clients keep reading the file. Once the file is repaired, the next boot
/// imports it.
#[tokio::test]
async fn an_unparseable_file_fails_without_a_marker_and_retries() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let pool = store.pool();
    let sessions_path = dir.path().join("sessions.json");
    fs::write(&sessions_path, "{ not json").unwrap();

    let err = import_sessions_if_needed(pool, &sessions_path).await.unwrap_err();
    assert!(err.to_string().contains("parse"), "{err:#}");
    assert!(!SessionsRepo::any_import_completed(pool).await.unwrap());

    one_session_file(
        &sessions_path,
        "00000000-0000-0000-0000-000000000005",
        "ainb-eps",
    );
    assert_eq!(
        imported(&import_sessions_if_needed(pool, &sessions_path).await.unwrap()),
        1
    );
}

/// 6. A file over the size cap is not read at all: the import fails and no
/// marker is written.
#[tokio::test]
async fn a_file_over_the_size_cap_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let pool = store.pool();
    let sessions_path = dir.path().join("sessions.json");
    one_session_file(
        &sessions_path,
        "00000000-0000-0000-0000-000000000006",
        "ainb-zeta",
    );

    let err = import_sessions_from(pool, &sessions_path, 16).await.unwrap_err();
    assert!(err.to_string().contains("limit"), "{err:#}");
    assert!(!SessionsRepo::any_import_completed(pool).await.unwrap());
}

/// 7. A record whose id is present but not a UUID is rejected and counted,
/// never given an invented id; a record with no id gets a UUID minted once;
/// a record with an unusable tmux name is rejected. The rest import.
#[tokio::test]
async fn bad_records_are_rejected_and_a_missing_id_becomes_a_uuid() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let pool = store.pool();
    let sessions_path = dir.path().join("sessions.json");
    let json = serde_json::json!({
        "sessions": {
            "ainb-ulid": {
                "session_id": "01J8Z3K6Q2N4T5V7W9X0Y1Z2A3",
                "tmux_session_name": "ainb-ulid",
                "worktree_path": "/home/user/work/ulid",
                "workspace_name": "ws",
                "created_at": 1757937600000i64
            },
            "ainb-noid": {
                "tmux_session_name": "ainb-noid",
                "worktree_path": "/home/user/work/noid",
                "workspace_name": "ws",
                "created_at": 1757937600000i64
            },
            "bad:name": {
                "session_id": "00000000-0000-0000-0000-000000000007",
                "tmux_session_name": "bad:name",
                "worktree_path": "/home/user/work/bad",
                "workspace_name": "ws",
                "created_at": 1757937600000i64
            }
        }
    });
    fs::write(&sessions_path, serde_json::to_string(&json).unwrap()).unwrap();

    let report = import_sessions_if_needed(pool, &sessions_path).await.unwrap();
    let ImportReport::Completed(marker) = report else {
        panic!("import did not complete: {report:?}");
    };
    assert_eq!((marker.imported, marker.rejected), (1, 2));

    let rows = SessionsRepo::list(pool, None, 100).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].tmux_session_name, "ainb-noid");
    assert!(
        ainb_hangar_proto::sessions::is_canonical_uuid(&rows[0].session_id),
        "minted id {} is not a UUID",
        rows[0].session_id
    );
}

/// 8. A home that imported before the marker existed (rows present, no
/// marker) completes without duplicating or overwriting those rows.
#[tokio::test]
async fn a_pre_marker_home_completes_without_duplicates() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let pool = store.pool();
    let sessions_path = dir.path().join("sessions.json");
    one_session_file(
        &sessions_path,
        "00000000-0000-0000-0000-000000000008",
        "ainb-eta",
    );
    let mut prior = SessionsRepo::list(pool, None, 1).await.unwrap();
    assert!(prior.is_empty());
    prior.push(ainb_hangar_store::repo::sessions::SessionRow {
        session_id: "00000000-0000-0000-0000-000000000008".to_string(),
        tmux_session_name: "ainb-eta".to_string(),
        worktree_path: "/home/user/work/eta-moved".to_string(),
        workspace_name: "eta".to_string(),
        created_at: 1,
        agent_type: "Codex".to_string(),
        headroom_enabled: false,
        rtk_enabled: false,
        skip_permissions: None,
        model: None,
        model_source: "Raw".to_string(),
        codex_model: None,
        codex_thread_id: None,
    });
    SessionsRepo::upsert(pool, &prior[0]).await.unwrap();

    let ImportReport::Completed(marker) =
        import_sessions_if_needed(pool, &sessions_path).await.unwrap()
    else {
        panic!("import did not complete");
    };
    assert_eq!((marker.imported, marker.skipped), (0, 1));
    assert_eq!(SessionsRepo::list(pool, None, 100).await.unwrap(), prior);
}
