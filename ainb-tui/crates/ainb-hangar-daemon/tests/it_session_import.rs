//! Integration tests for daemon boot import of sessions.json (spec P6d, #1166).

use ainb_hangar_daemon::session_import::{
    ImportReport, ReconcileWatch, import_sessions_from, import_sessions_if_needed,
    reconcile_sessions, reconcile_sessions_bounded,
};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::sessions::{SessionRow, SessionsRepo, reconcile_key};
use std::fs;
use std::path::Path;
use std::time::Duration;

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
    assert!(
        SessionsRepo::import_marker(pool, &marker_key(&sessions_path))
            .await
            .unwrap()
            .is_none()
    );

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
    assert!(
        SessionsRepo::import_marker(pool, &marker_key(&sessions_path))
            .await
            .unwrap()
            .is_none()
    );
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

// ─── P6e: the repeatable reconcile (criterion 3) ───────────────────────────

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

fn ids(rows: &[SessionRow]) -> Vec<&str> {
    let mut ids: Vec<&str> = rows.iter().map(|r| r.session_id.as_str()).collect();
    ids.sort_unstable();
    ids
}

async fn table(pool: &sqlx::SqlitePool) -> Vec<SessionRow> {
    SessionsRepo::list(pool, None, 100).await.unwrap()
}

/// The P6d import ran on an empty file; sessions created while the table was
/// dark exist in the file only. The boot pass inserts every one of them, and
/// only then is the table authoritative.
#[tokio::test]
async fn a_boot_pass_inserts_the_sessions_written_while_dark() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let pool = store.pool();
    let sessions_path = dir.path().join("sessions.json");
    import_sessions_if_needed(pool, &sessions_path).await.unwrap();

    sessions_file(
        &sessions_path,
        &[
            ("00000000-0000-0000-0000-00000000000a", "ainb-dark-a", "ws"),
            ("00000000-0000-0000-0000-00000000000b", "ainb-dark-b", "ws"),
        ],
    );
    let source = marker_key(&sessions_path);
    assert!(!SessionsRepo::import_complete_for(pool, &source).await.unwrap());

    let outcome = ReconcileWatch::new(&sessions_path).tick(pool).await.unwrap().unwrap();
    assert_eq!(outcome.marker.imported, 2);
    assert_eq!(
        ids(&table(pool).await),
        vec![
            "00000000-0000-0000-0000-00000000000a",
            "00000000-0000-0000-0000-00000000000b"
        ]
    );
    assert!(SessionsRepo::import_complete_for(pool, &source).await.unwrap());
}

/// A table row is never overwritten by the file row of the same id: the
/// table is newer than any file row it disagrees with.
#[tokio::test]
async fn a_pass_never_overwrites_a_table_row_with_its_file_row() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let pool = store.pool();
    let sessions_path = dir.path().join("sessions.json");
    let id = "00000000-0000-0000-0000-0000000000c1";
    sessions_file(&sessions_path, &[(id, "ainb-keep", "file-ws")]);
    import_sessions_if_needed(pool, &sessions_path).await.unwrap();

    let mut current = SessionsRepo::get_by_id(pool, id).await.unwrap().unwrap();
    current.workspace_name = "table-ws".to_string();
    current.headroom_enabled = true;
    SessionsRepo::upsert(pool, &current).await.unwrap();

    let outcome = reconcile_sessions(pool, &sessions_path).await.unwrap();
    assert_eq!(outcome.marker.imported, 0);
    assert_eq!(table(pool).await, vec![current]);
}

/// A delete through the bare `workspace/session_delete` RPC removes the
/// table row only, so the file row survives and the next reconcile pass
/// brings the session back. This is the handler's contract, not a gap: the
/// handler cannot take the `sessions.json` flock, because the client holds
/// it across this RPC. Since P6e-2 the client removes the file row under
/// that flock before it calls the RPC, so a delete through `SessionSource`
/// stays deleted (`ainb-core/tests/session_resolver.rs`,
/// `a_delete_through_the_daemon_is_not_brought_back_by_the_next_pass`).
/// What this pins is that the pass trusts the file: a caller that deletes a
/// table row and leaves its file row gets it back.
#[tokio::test]
async fn a_table_only_delete_comes_back_until_clients_delete_the_file_row() {
    use ainb_hangar_daemon::events::EventBroker;
    use ainb_hangar_daemon::health_stats::HealthStats;
    use ainb_hangar_daemon::rpc::{self, DaemonHealth};
    use ainb_hangar_proto::{RpcId, RpcRequest, methods};

    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let pool = store.pool();
    let sessions_path = dir.path().join("sessions.json");
    let (gone, kept) = (
        "00000000-0000-0000-0000-0000000000d1",
        "00000000-0000-0000-0000-0000000000d2",
    );
    sessions_file(
        &sessions_path,
        &[(gone, "ainb-gone", "ws"), (kept, "ainb-kept", "ws")],
    );
    import_sessions_if_needed(pool, &sessions_path).await.unwrap();
    reconcile_sessions(pool, &sessions_path).await.unwrap();

    let health = DaemonHealth {
        socket_path: "/tmp/it-session-delete.sock".into(),
        pid: 1,
        started_at: std::time::Instant::now(),
        version: "0.1.0".into(),
        stats: std::sync::Arc::new(HealthStats::default()),
    };
    let delete = RpcRequest {
        jsonrpc: ainb_hangar_proto::jsonrpc_version(),
        id: RpcId::Number(1),
        method: methods::WORKSPACE_SESSION_DELETE.into(),
        params: serde_json::json!({ "session_id": gone }),
    };
    let resp = rpc::dispatch(pool, &delete, &health, &EventBroker::new().sink()).await;
    assert_eq!(resp.result.unwrap()["deleted"], true);
    assert_eq!(ids(&table(pool).await), vec![kept]);
    assert!(
        fs::read_to_string(&sessions_path).unwrap().contains("ainb-gone"),
        "the RPC now removes the file row: update this test and its doc"
    );

    reconcile_sessions(pool, &sessions_path).await.unwrap();
    assert_eq!(
        ids(&table(pool).await),
        vec![gone, kept],
        "the pass no longer trusts the file row: update this test and its doc"
    );
}

/// A previous release appends to the file after a pass. The watcher sees the
/// mtime move and its next tick inserts the row; an unchanged file runs no
/// pass at all.
#[tokio::test]
async fn an_old_binary_append_is_inserted_by_the_next_tick() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let pool = store.pool();
    let sessions_path = dir.path().join("sessions.json");
    let first = "00000000-0000-0000-0000-0000000000e1";
    sessions_file(&sessions_path, &[(first, "ainb-first", "ws")]);
    import_sessions_if_needed(pool, &sessions_path).await.unwrap();

    let mut watch = ReconcileWatch::new(&sessions_path);
    assert!(watch.tick(pool).await.unwrap().is_ok());
    assert!(
        watch.tick(pool).await.is_none(),
        "an unchanged file must not run a pass"
    );

    tokio::time::sleep(Duration::from_millis(20)).await;
    let old = ainb_fleet_core::session_registry::AinbSessionRecord::new(
        "ainb-old-release",
        "/home/user/work/old".into(),
        "ws",
    );
    ainb_fleet_core::session_registry::register_session_at(&sessions_path, &old).unwrap();

    let outcome = watch.tick(pool).await.expect("a changed file runs a pass").unwrap();
    assert_eq!(outcome.marker.imported, 1);
    let old_id = old.session_id.to_string();
    let mut want = vec![first, old_id.as_str()];
    want.sort_unstable();
    assert_eq!(ids(&table(pool).await), want);
}

/// A file session whose tmux name the table binds to another id is skipped,
/// counted on the marker, and the table row is unchanged.
#[tokio::test]
async fn a_tmux_name_conflict_is_skipped_counted_and_the_table_wins() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let pool = store.pool();
    let sessions_path = dir.path().join("sessions.json");
    let holder = "00000000-0000-0000-0000-0000000000f1";
    sessions_file(&sessions_path, &[(holder, "ainb-shared", "table-ws")]);
    import_sessions_if_needed(pool, &sessions_path).await.unwrap();
    let before = table(pool).await;

    let rival = "00000000-0000-0000-0000-0000000000f2";
    sessions_file(&sessions_path, &[(rival, "ainb-shared", "file-ws")]);
    let outcome = reconcile_sessions(pool, &sessions_path).await.unwrap();

    assert_eq!((outcome.marker.imported, outcome.marker.skipped), (0, 1));
    assert_eq!(outcome.conflicts.len(), 1);
    assert_eq!(outcome.conflicts[0].session_id, rival);
    assert_eq!(outcome.conflicts[0].holder, holder);
    let marker = SessionsRepo::import_marker(pool, &reconcile_key(&marker_key(&sessions_path)))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(marker.skipped, 1);
    assert_eq!(table(pool).await, before);
}

/// A failed pass writes no reconcile marker, so `import_complete` stays false
/// even though the P6d import row exists; the watcher retries it and the
/// repaired file completes.
#[tokio::test]
async fn a_failed_pass_leaves_the_table_not_authoritative() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let pool = store.pool();
    let sessions_path = dir.path().join("sessions.json");
    import_sessions_if_needed(pool, &sessions_path).await.unwrap();
    let source = marker_key(&sessions_path);
    assert!(SessionsRepo::import_marker(pool, &source).await.unwrap().is_some());

    fs::write(&sessions_path, "{ not json").unwrap();
    let mut watch = ReconcileWatch::new(&sessions_path);
    let err = watch.tick(pool).await.unwrap().unwrap_err();
    assert!(format!("{err:#}").contains("parse"), "{err:#}");
    assert!(
        SessionsRepo::import_marker(pool, &reconcile_key(&source))
            .await
            .unwrap()
            .is_none()
    );
    assert!(!SessionsRepo::import_complete_for(pool, &source).await.unwrap());

    // Over the cap fails the same way.
    let capped = ReconcileWatch::with_cap(&sessions_path, 4).tick(pool).await.unwrap();
    assert!(capped.is_err());

    // A failed pass is retried on the next tick even with the mtime unchanged.
    assert!(watch.tick(pool).await.unwrap().is_err());
    sessions_file(
        &sessions_path,
        &[("00000000-0000-0000-0000-0000000000a9", "ainb-fixed", "ws")],
    );
    assert!(watch.tick(pool).await.unwrap().is_ok());
    assert!(SessionsRepo::import_complete_for(pool, &source).await.unwrap());
}

/// A writer that blocks on the flock while a pass holds it lands its row in
/// the file after the pass, and the next pass inserts it: never lost.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_writer_blocked_during_a_pass_is_inserted_by_the_next_pass() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let pool = store.pool().clone();
    let sessions_path = dir.path().join("sessions.json");
    sessions_file(
        &sessions_path,
        &[("00000000-0000-0000-0000-0000000000b1", "ainb-before", "ws")],
    );
    import_sessions_if_needed(&pool, &sessions_path).await.unwrap();

    // Hold the SQLite write lock so the pass stops inside its store write,
    // with the flock taken.
    let mut blocker = pool.acquire().await.unwrap();
    sqlx::query("BEGIN IMMEDIATE").execute(&mut *blocker).await.unwrap();
    let pass = {
        let (pool, path) = (pool.clone(), sessions_path.clone());
        tokio::spawn(async move { reconcile_sessions(&pool, &path).await })
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while ainb_fleet_core::session_registry::try_lock_sessions_store_at(dir.path())
        .unwrap()
        .is_some()
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the pass never took the flock"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    let late = ainb_fleet_core::session_registry::AinbSessionRecord::new(
        "ainb-late",
        "/home/user/work/late".into(),
        "ws",
    );
    let writer = {
        let (path, late) = (sessions_path.clone(), late.clone());
        std::thread::spawn(move || {
            ainb_fleet_core::session_registry::register_session_at(&path, &late).unwrap();
        })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !fs::read_to_string(&sessions_path).unwrap().contains("ainb-late"),
        "the writer got past the flock during the pass"
    );

    sqlx::query("ROLLBACK").execute(&mut *blocker).await.unwrap();
    drop(blocker);
    let first = pass.await.unwrap().unwrap();
    assert_eq!(
        first.marker.imported, 0,
        "the pass read the file before the writer"
    );
    writer.join().unwrap();
    assert!(fs::read_to_string(&sessions_path).unwrap().contains("ainb-late"));

    let second = reconcile_sessions(&pool, &sessions_path).await.unwrap();
    assert_eq!(second.marker.imported, 1);
    assert!(
        SessionsRepo::get_by_id(&pool, &late.session_id.to_string())
            .await
            .unwrap()
            .is_some()
    );
}

/// A pass whose store write is stuck behind another writer gives up at its
/// bound instead of holding the flock: the flock is free again at once, and
/// no marker is written. Every CLI writer blocks on that flock meanwhile.
#[tokio::test]
async fn a_stuck_store_write_releases_the_flock_at_its_bound() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let pool = store.pool();
    let sessions_path = dir.path().join("sessions.json");
    sessions_file(
        &sessions_path,
        &[("00000000-0000-0000-0000-0000000000b9", "ainb-stuck", "ws")],
    );

    let mut blocker = pool.acquire().await.unwrap();
    sqlx::query("BEGIN IMMEDIATE").execute(&mut *blocker).await.unwrap();
    let started = std::time::Instant::now();
    let err = reconcile_sessions_bounded(
        pool,
        &sessions_path,
        1024 * 1024,
        Duration::from_millis(200),
    )
    .await
    .unwrap_err();
    assert!(format!("{err:#}").contains("did not finish"), "{err:#}");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    assert!(
        ainb_fleet_core::session_registry::try_lock_sessions_store_at(dir.path())
            .unwrap()
            .is_some(),
        "the flock outlived the pass"
    );
    sqlx::query("ROLLBACK").execute(&mut *blocker).await.unwrap();
    drop(blocker);

    let source = marker_key(&sessions_path);
    assert!(
        SessionsRepo::import_marker(pool, &reconcile_key(&source))
            .await
            .unwrap()
            .is_none()
    );
    assert!(table(pool).await.is_empty());
}

/// A rewrite that keeps the file's mtime (`cp -p`, a restore) is still a
/// change: the watcher also compares length and inode.
#[tokio::test]
async fn a_rewrite_that_keeps_the_mtime_still_runs_a_pass() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let pool = store.pool();
    let sessions_path = dir.path().join("sessions.json");
    sessions_file(
        &sessions_path,
        &[("00000000-0000-0000-0000-0000000000e7", "ainb-one", "ws")],
    );
    let mtime = fs::metadata(&sessions_path).unwrap().modified().unwrap();

    let mut watch = ReconcileWatch::new(&sessions_path);
    assert!(watch.tick(pool).await.unwrap().is_ok());

    sessions_file(
        &sessions_path,
        &[
            ("00000000-0000-0000-0000-0000000000e7", "ainb-one", "ws"),
            ("00000000-0000-0000-0000-0000000000e8", "ainb-two", "ws"),
        ],
    );
    fs::File::options()
        .write(true)
        .open(&sessions_path)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    assert_eq!(
        fs::metadata(&sessions_path).unwrap().modified().unwrap(),
        mtime
    );

    let outcome = watch.tick(pool).await.expect("a same-mtime rewrite runs a pass").unwrap();
    assert_eq!(outcome.marker.imported, 1);
}
