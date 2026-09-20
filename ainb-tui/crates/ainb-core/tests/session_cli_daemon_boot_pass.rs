//! A CLI write against a daemon whose boot reconcile pass has not committed
//! (P6e). The pass needs the `sessions.json` lock that `mutate` takes, so
//! `mutate` must let go of it while it waits, or it blocks the very pass it
//! is waiting for.
//!
//! A binary of its own: the daemon's first-pass gate is process-wide and
//! stays open once a pass commits, so this test needs a process where no pass
//! has run yet.

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use chrono::Utc;
use uuid::Uuid;

use ainb::cli::util::SessionSource;
use ainb::interactive::session_manager::{ModelSource, SessionMetadata, SessionStore};
use ainb::models::session::SessionAgentType;
use ainb_hangar_store::repo::sessions::SessionsRepo;

#[path = "support/fleet_hangar.rs"]
mod fleet_hangar;
use fleet_hangar::{EnvGuard, FleetHangar};

fn make_session(name: &str) -> SessionMetadata {
    SessionMetadata {
        session_id: Uuid::new_v4(),
        tmux_session_name: name.to_string(),
        worktree_path: PathBuf::from(format!("/tmp/work/{name}")),
        workspace_name: "ws".to_string(),
        created_at: Utc::now(),
        agent_type: SessionAgentType::Claude,
        headroom_enabled: false,
        rtk_enabled: false,
        skip_permissions: None,
        model: None,
        model_source: ModelSource::LegacyTyped,
        codex_model: None,
        codex_thread_id: None,
    }
}

#[test]
fn a_write_during_the_boot_pass_waits_for_it_without_holding_the_lock() {
    let root = tempfile::tempdir().unwrap();
    let ainb = root.path().join("home");
    let dir = ainb.join(".agents-in-a-box");
    let hangar_home = root.path().join("hangar");
    fs::create_dir_all(&dir).unwrap();
    fs::create_dir_all(&hangar_home).unwrap();
    let _ainb_home = EnvGuard::set("AINB_HOME", &ainb);
    let sessions_path = dir.join("sessions.json");

    let kept = make_session("sess-kept");
    let mut file = SessionStore::default();
    file.upsert(kept.clone());
    fs::write(&sessions_path, serde_json::to_vec_pretty(&file).unwrap()).unwrap();

    // The process resolves the daemon while it is ready (markers present, no
    // gate); then the daemon is as a restart leaves it: the gate armed and no
    // pass run since.
    ainb_hangar_daemon::rpc::auth::advertise_workspace_sessions_for_tests(true);
    let hangar = FleetHangar::start(&hangar_home);
    hangar.block_on(async {
        ainb_hangar_daemon::session_import::import_sessions_if_needed(
            hangar.pool(),
            &sessions_path,
        )
        .await
        .unwrap();
        SessionsRepo::complete_reconcile(
            hangar.pool(),
            &sessions_path.to_string_lossy(),
            &[],
            0,
            1,
            ainb_hangar_store::repo::sessions::Deletes::Nothing,
        )
        .await
        .unwrap();
    });
    let token = fs::read_to_string(ainb_hangar_proto::auth::token_file_in(&hangar_home))
        .unwrap()
        .trim()
        .to_string();
    let source = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(SessionSource::resolve_at(
            ainb_hangar_daemon::rpc::socket_path_in(&hangar_home),
            token,
        ));
    assert!(matches!(source, SessionSource::Daemon(_)), "{source:?}");
    ainb_hangar_daemon::session_import::arm_first_pass_gate();

    // The write starts first and takes the lock.
    let added = make_session("sess-added");
    let writer = {
        let added = added.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            rt.block_on(source.mutate(|s| s.upsert(added)))
        })
    };
    let deadline = Instant::now() + Duration::from_secs(2);
    while ainb_fleet_core::session_registry::try_lock_sessions_store_at(&dir)
        .unwrap()
        .is_some()
    {
        assert!(Instant::now() < deadline, "the write never took the lock");
        std::thread::sleep(Duration::from_millis(5));
    }

    // Only now does the boot pass start. It needs the lock the write holds.
    let (path, pool) = (sessions_path, hangar.pool().clone());
    hangar.block_on(async move {
        tokio::spawn(ainb_hangar_daemon::session_import::ReconcileWatch::new(&path).run(pool));
    });

    let started = Instant::now();
    while !writer.is_finished() {
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "the write never finished: it is holding the lock the pass needs"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    writer
        .join()
        .unwrap()
        .expect("the write lands once the pass it waited for has committed");

    let mut ids: Vec<String> = hangar.block_on(async {
        SessionsRepo::list(hangar.pool(), None, 100)
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.session_id)
            .collect()
    });
    ids.sort_unstable();
    let mut want = vec![kept.session_id.to_string(), added.session_id.to_string()];
    want.sort_unstable();
    assert_eq!(
        ids, want,
        "the pass inserted the kept session and the write added its own"
    );
}
