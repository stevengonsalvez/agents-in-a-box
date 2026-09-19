//! P6e criterion 2, write halves: with a ready daemon, the TUI's writers go
//! through the process's session source, so each write lands in the daemon's
//! table (and, file first, in `sessions.json`):
//!
//! - a session the TUI's create path writes is listed by the real
//!   `ainb list --format json` from another process;
//! - `persist_codex_thread_id` changes the table row;
//! - `Persist::SessionHeadroom` keeps its compare-and-set on the table: the
//!   switch applies when the expected value holds, and a second write whose
//!   expected value moved is refused and leaves the row as it was.
//!
//! Each fails when its site is put back on `SessionStore::mutate`, which
//! writes the file only. Own binary: the session source is decided once per
//! process, and `AINB_HOME` / `AINB_HANGAR_HOME` are process-wide.

use std::path::PathBuf;

use ainb::app::effect::Persist;
use ainb::cli::util::{self, SessionSource};
use ainb::interactive::session_manager::{ModelSource, SessionMetadata, SessionStore};
use ainb::models::session::SessionAgentType;
use ainb_hangar_store::repo::sessions::{SessionRow, SessionsRepo};
use chrono::Utc;
use uuid::Uuid;

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
        agent_type: SessionAgentType::Codex,
        headroom_enabled: true,
        rtk_enabled: false,
        skip_permissions: None,
        model: None,
        model_source: ModelSource::LegacyTyped,
        codex_model: None,
        codex_thread_id: None,
    }
}

fn row(hangar: &FleetHangar, tmux: &str) -> Option<SessionRow> {
    hangar.block_on(async { SessionsRepo::get_by_tmux_name(hangar.pool(), tmux).await.unwrap() })
}

#[test]
fn the_tuis_writers_land_in_the_daemons_table() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let hangar_home = root.path().join("hangar");
    std::fs::create_dir_all(home.join(".agents-in-a-box")).unwrap();
    std::fs::create_dir_all(&hangar_home).unwrap();
    let _env = [
        EnvGuard::set("AINB_HOME", &home),
        EnvGuard::set("AINB_HANGAR_HOME", &hangar_home),
    ];
    util::advertise_workspace_sessions_for_tests(true);
    ainb_hangar_daemon::rpc::auth::advertise_workspace_sessions_for_tests(true);

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
    assert!(matches!(
        tokio::runtime::Runtime::new().unwrap().block_on(util::session_source()),
        SessionSource::Daemon(_)
    ));

    // The create path's write (`session_manager.rs`, create and recovery).
    let created = make_session("tmux_ainb-tui-created");
    util::mutate_session_store(|s| s.upsert(created.clone())).expect("create write");
    assert!(
        row(&hangar, "tmux_ainb-tui-created").is_some(),
        "the create missed the table"
    );

    // Another process's `ainb list` lists it.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_ainb"))
        .args(["list", "--format", "json"])
        .env("AINB_HOME", &home)
        .env("HOME", &home)
        .env("AINB_HANGAR_HOME", &hangar_home)
        .env("AINB_TEST_WORKSPACE_SESSIONS", "1")
        .output()
        .expect("run ainb list");
    let listed = String::from_utf8_lossy(&out.stdout);
    assert!(
        listed.contains(&created.session_id.to_string()),
        "ainb list did not list the TUI's session:\n{listed}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // `persist_codex_thread_id` changes the table row.
    ainb::interactive::session_manager::persist_codex_thread_id(
        created.session_id,
        "thread-p6e".to_string(),
    )
    .expect("thread write");
    assert_eq!(
        row(&hangar, "tmux_ainb-tui-created").unwrap().codex_thread_id.as_deref(),
        Some("thread-p6e"),
        "persist_codex_thread_id missed the table"
    );

    // The Headroom compare-and-set, through the table.
    let switch = |expected: bool, enabled: bool| {
        ainb::config::persist::write(&Persist::SessionHeadroom {
            tmux_session: "tmux_ainb-tui-created".to_string(),
            expected,
            enabled,
        })
    };
    switch(true, false).expect("the switch applies when the expected value holds");
    assert!(
        !row(&hangar, "tmux_ainb-tui-created").unwrap().headroom_enabled,
        "the Headroom switch missed the table"
    );
    let moved = switch(true, false).expect_err("the expected value moved");
    assert!(moved.contains("changed since it was read"), "{moved}");
    assert!(
        !row(&hangar, "tmux_ainb-tui-created").unwrap().headroom_enabled,
        "a refused compare-and-set changed the row"
    );
    assert!(
        !SessionStore::load().sessions["tmux_ainb-tui-created"].headroom_enabled,
        "the file row did not follow the table"
    );
    drop(hangar);
}
