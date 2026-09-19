//! P6e criteria 1 and 2, read halves: with a ready daemon that speaks the
//! sessions capability, the TUI's process resolves `Daemon` (the accessor
//! the TUI and the embedded desktop host both read) and its real workspace
//! loader lists a session that is in the daemon's table and NOT in
//! `sessions.json`, without a restart. Only a read of the table can list it.
//!
//! Own binary: the process's session source is decided once, and
//! `AINB_HOME` / `AINB_HANGAR_HOME` are process-wide.

use ainb::app::state::AppState;
use ainb::cli::util::{self, SessionSource};
use ainb::interactive::session_manager::SessionStore;
use ainb_hangar_store::repo::sessions::SessionsRepo;

#[path = "support/fleet_hangar.rs"]
mod fleet_hangar;
#[path = "support/session_readers.rs"]
mod session_readers;
use fleet_hangar::{EnvGuard, FleetHangar};
use session_readers::{listed, notices, real_worktree, row_of, session_at};

#[test]
fn the_tui_lists_a_session_it_finds_only_in_the_table() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path().canonicalize().unwrap();
    let hangar_home = home.join("hangar");
    std::fs::create_dir_all(home.join(".agents-in-a-box")).unwrap();
    std::fs::create_dir_all(&hangar_home).unwrap();
    let _ainb = EnvGuard::set("AINB_HOME", &home);
    let _hangar = EnvGuard::set("AINB_HANGAR_HOME", &hangar_home);
    util::advertise_workspace_sessions_for_tests(true);
    ainb_hangar_daemon::rpc::auth::advertise_workspace_sessions_for_tests(true);

    // The file exists and is empty: the daemon's first import and pass make
    // the table authoritative.
    SessionStore::default().save().unwrap();
    let hangar = FleetHangar::start(&hangar_home);
    let path = SessionStore::storage_path();
    hangar.block_on(async {
        ainb_hangar_daemon::session_import::import_sessions_if_needed(hangar.pool(), &path)
            .await
            .unwrap();
        ainb_hangar_daemon::session_import::reconcile_sessions(hangar.pool(), &path)
            .await
            .unwrap();
    });

    // A session written to the table only, as a surface on the daemon leaves
    // it for a reader that has not reloaded.
    let meta = session_at(&real_worktree(&home, "t4b1e001"), "tmux_ainb-table-only");
    let row = row_of(&meta);
    hangar.block_on(async { SessionsRepo::upsert(hangar.pool(), &row).await.unwrap() });
    assert!(
        !SessionStore::load().sessions.contains_key("tmux_ainb-table-only"),
        "the fixture must keep the session out of the file"
    );

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    // Criterion 1: the accessor the TUI and the embedded desktop host read.
    let source = rt.block_on(util::session_source());
    assert!(matches!(source, SessionSource::Daemon(_)), "{source:?}");

    // Criterion 2, read half: the real loader lists it on its first load.
    let mut state = AppState::new();
    rt.block_on(state.load_real_workspaces());
    assert!(
        listed(&state, meta.session_id),
        "the TUI did not read the table"
    );
    assert!(
        notices(&state).is_empty(),
        "a ready daemon owes no notice: {:?}",
        notices(&state)
    );
    drop(rt);
    drop(hangar);
}
