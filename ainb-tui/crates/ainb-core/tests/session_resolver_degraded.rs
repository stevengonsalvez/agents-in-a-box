//! A process that resolves with no daemon up is `Degraded`, writes to the
//! file, and moves to the daemon once one is up and has reconciled (P6e,
//! "Daemon down at startup"). The sessions it wrote while degraded are in the
//! table after the move, and a write after it goes to both.
//!
//! A binary of its own: the process's session source is decided once per
//! process, and this test needs to watch that one decision change.

use std::fs;
use std::path::PathBuf;

use chrono::Utc;
use uuid::Uuid;

use ainb::cli::util::{self, SessionSource};
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
fn a_degraded_process_moves_to_the_daemon_once_it_is_up() {
    let root = tempfile::tempdir().unwrap();
    let ainb = root.path().join("home");
    let hangar_home = root.path().join("hangar");
    fs::create_dir_all(ainb.join(".agents-in-a-box")).unwrap();
    fs::create_dir_all(&hangar_home).unwrap();
    let _ainb_home = EnvGuard::set("AINB_HOME", &ainb);
    let _hangar = EnvGuard::set("AINB_HANGAR_HOME", &hangar_home);
    util::advertise_workspace_sessions_for_tests(true);
    ainb_hangar_daemon::rpc::auth::advertise_workspace_sessions_for_tests(true);

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();

    // No daemon: degraded, and the write lands in the file.
    assert!(rt.block_on(util::session_source()).is_degraded());
    let while_down = make_session("sess-while-down");
    rt.block_on(util::mutate_session_store_async(|s| {
        s.upsert(while_down.clone())
    }))
    .expect("a degraded write goes to the file");
    assert!(SessionStore::load().sessions.contains_key("sess-while-down"));
    assert!(
        !rt.block_on(util::leave_degraded()),
        "no daemon yet, so still degraded"
    );

    // The daemon comes up. Leaving the degraded state asks it to reconcile,
    // so the session written while down reaches the table.
    let hangar = FleetHangar::start(&hangar_home);
    assert!(
        rt.block_on(util::leave_degraded()),
        "the daemon is up and reconciled"
    );
    let source = rt.block_on(util::session_source());
    assert!(matches!(source, SessionSource::Daemon(_)), "{source:?}");
    let ids = |hangar: &FleetHangar| -> Vec<String> {
        hangar.block_on(async {
            let mut ids: Vec<String> = SessionsRepo::list(hangar.pool(), None, 100)
                .await
                .unwrap()
                .into_iter()
                .map(|row| row.session_id)
                .collect();
            ids.sort_unstable();
            ids
        })
    };
    assert_eq!(ids(&hangar), vec![while_down.session_id.to_string()]);

    // After the move a write goes to the table and to the file.
    let after = make_session("sess-after");
    rt.block_on(util::mutate_session_store_async(|s| {
        s.upsert(after.clone())
    }))
    .expect("a write on the daemon");
    let mut want = vec![
        while_down.session_id.to_string(),
        after.session_id.to_string(),
    ];
    want.sort_unstable();
    assert_eq!(ids(&hangar), want);
    assert!(SessionStore::load().sessions.contains_key("sess-after"));

    // The move is made once: it reports the daemon, and nothing moves back.
    assert!(rt.block_on(util::leave_degraded()));
}
