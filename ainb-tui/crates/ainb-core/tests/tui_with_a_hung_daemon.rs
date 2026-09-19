//! P6e criterion 10, reader side: a daemon that answers the resolve and then
//! never answers a session RPC must not hang the TUI. With two live `tmux_`
//! sessions for the discovery loop to walk, the real workspace loader returns
//! within two `SESSION_RPC_DEADLINE`s plus its own work: one bounded read for
//! the workspace load (shared by discovery and the stopped-session pass) and
//! one for the orphan scan, however many sessions tmux has. A read per tmux
//! session, as before, took four deadlines here. The session list paints its
//! next frame, and nothing is listed from a store that did not answer.
//!
//! Own binary: the process's session source is decided once, and
//! `AINB_HOME` / `AINB_HANGAR_HOME` / `TMUX_TMPDIR` are process-wide.

use std::process::Command;
use std::time::{Duration, Instant};

use ainb::app::state::AppState;
use ainb::cli::util::{self, SESSION_RPC_DEADLINE, SessionSource};
use ainb::components::SessionListComponent;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

#[path = "support/fake_session_daemon.rs"]
mod fake_session_daemon;
#[path = "support/fleet_hangar.rs"]
mod fleet_hangar;
use fake_session_daemon::{fake_daemon, ready_list};
use fleet_hangar::EnvGuard;

#[test]
fn a_daemon_that_stops_answering_does_not_hang_the_tui() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let hangar_home = root.path().join("hangar");
    std::fs::create_dir_all(home.join(".agents-in-a-box")).unwrap();
    std::fs::create_dir_all(hangar_home.join("hangar")).unwrap();
    std::fs::create_dir_all(root.path().join("tmux")).unwrap();
    let _env = [
        EnvGuard::set("AINB_HOME", &home),
        EnvGuard::set("AINB_HANGAR_HOME", &hangar_home),
        // A tmux server of this test's own, so the loader walks exactly the
        // sessions seeded below and nothing of the box's.
        EnvGuard::set("TMUX_TMPDIR", root.path().join("tmux")),
    ];
    let seeded = ["tmux_p6e-hung-a", "tmux_p6e-hung-b"];
    for name in seeded {
        let made = Command::new("tmux")
            .args(["new-session", "-d", "-s", name, "sleep", "600"])
            .status()
            .expect("run tmux");
        assert!(made.success(), "could not seed tmux session {name}");
    }
    util::advertise_workspace_sessions_for_tests(true);

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    // Hello and the resolve probe are answered; nothing after that ever is.
    fake_daemon(&rt, &hangar_home, |_, n| (n == 0).then(ready_list));
    let source = rt.block_on(util::session_source());
    assert!(matches!(source, SessionSource::Daemon(_)), "{source:?}");

    let mut state = AppState::new();
    let started = Instant::now();
    let loaded = rt.block_on(async {
        tokio::time::timeout(SESSION_RPC_DEADLINE * 4, state.load_real_workspaces()).await
    });
    let took = started.elapsed();
    assert!(
        loaded.is_ok(),
        "the loader hung on a daemon that stopped answering"
    );
    assert!(
        took < SESSION_RPC_DEADLINE * 2 + Duration::from_secs(2),
        "the loader read the store more than once per step: {took:?}"
    );
    assert!(
        state.sessions.workspaces.iter().all(|w| w.sessions.is_empty()),
        "sessions were listed from a store that never answered"
    );

    // The next frame paints.
    let mut list = SessionListComponent::new();
    let mut ui = ainb::app::ui_state::UiState::default();
    let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
    term.draw(|f| list.render(f, f.area(), &state, &mut ui))
        .expect("the next frame");
    drop(rt);
    for name in seeded {
        let _ = Command::new("tmux").args(["kill-session", "-t", name]).status();
    }
}
