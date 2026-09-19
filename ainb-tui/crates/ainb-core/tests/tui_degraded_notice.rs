//! P6e criterion 8, read side: a TUI whose process starts with no daemon is
//! degraded, says so, and lists the sessions in `sessions.json`. Once a daemon
//! is up and the process leaves the degraded state, the TUI says that too and
//! reads the table: a session only the table has is listed on the next load.
//!
//! Own binary: the process's session source is decided once, and
//! `AINB_HOME` / `AINB_HANGAR_HOME` are process-wide.

use ainb::app::state::AppState;
use ainb::cli::util::{self, SessionSource, SessionSourceNotice};
use ainb::interactive::session_manager::SessionStore;
use ainb_hangar_store::repo::sessions::SessionsRepo;

#[path = "support/fleet_hangar.rs"]
mod fleet_hangar;
#[path = "support/session_readers.rs"]
mod session_readers;
use fleet_hangar::{EnvGuard, FleetHangar};
use session_readers::{listed, notices, real_worktree, row_of, session_at};

#[test]
fn a_tui_started_without_a_daemon_says_so_then_reads_the_table() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path().canonicalize().unwrap();
    let hangar_home = home.join("hangar");
    std::fs::create_dir_all(home.join(".agents-in-a-box")).unwrap();
    std::fs::create_dir_all(&hangar_home).unwrap();
    let _ainb = EnvGuard::set("AINB_HOME", &home);
    let _hangar = EnvGuard::set("AINB_HANGAR_HOME", &hangar_home);
    util::advertise_workspace_sessions_for_tests(true);
    ainb_hangar_daemon::rpc::auth::advertise_workspace_sessions_for_tests(true);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();

    // No daemon: the file's session is listed, with the degraded notice.
    let in_file = session_at(&real_worktree(&home, "f11e0001"), "tmux_ainb-in-file");
    let mut store = SessionStore::default();
    store.upsert(in_file.clone());
    store.save().unwrap();

    let mut state = AppState::new();
    rt.block_on(state.load_real_workspaces());
    assert!(rt.block_on(util::session_source()).is_degraded());
    assert!(
        listed(&state, in_file.session_id),
        "the degraded TUI lost the file's session"
    );
    assert!(
        notices(&state).iter().any(|n| n == SessionSourceNotice::Degraded.message()),
        "no degraded notice: {:?}",
        notices(&state)
    );

    // The daemon comes up; the process leaves the degraded state.
    let hangar = FleetHangar::start(&hangar_home);
    assert!(
        rt.block_on(util::leave_degraded()),
        "the daemon is up and reconciled"
    );
    assert!(matches!(
        rt.block_on(util::session_source()),
        SessionSource::Daemon(_)
    ));

    // A session only the table has is listed on the next load, and the TUI
    // says it is back on the daemon.
    let table_only = session_at(&real_worktree(&home, "t4b1e002"), "tmux_ainb-table-only");
    let row = row_of(&table_only);
    hangar.block_on(async { SessionsRepo::upsert(hangar.pool(), &row).await.unwrap() });

    let mut state = AppState::new();
    rt.block_on(state.load_real_workspaces());
    assert!(
        listed(&state, table_only.session_id),
        "the TUI did not move to the table"
    );
    assert!(
        listed(&state, in_file.session_id),
        "the degraded-time session is missing"
    );
    assert!(
        notices(&state).iter().any(|n| n == SessionSourceNotice::Recovered.message()),
        "no recovered notice: {:?}",
        notices(&state)
    );
    drop(rt);
    drop(hangar);
}
