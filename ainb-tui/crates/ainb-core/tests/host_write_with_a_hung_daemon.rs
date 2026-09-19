#![allow(missing_docs)]

// ABOUTME: P6e criterion 10, the write side at the host. A session-store write
// can reach the hangar daemon, and a daemon that answers the resolve and then
// nothing holds a write for the client's whole deadline. The terminal host
// must not spend that on its tick: `Effect::Persist` of the session store
// returns at once, leaving the tick free to draw, and the failure arrives
// later as the deferred report the reducer turns into a notice.
//
// Own binary: the process's session source is decided once, and `AINB_HOME` /
// `AINB_HANGAR_HOME` are process-wide.

use std::time::{Duration, Instant};

use ainb::app::state::NotificationType;
use ainb::app::ui_state::UiState;
use ainb::app::{NoRenderer, Persist};
use ainb::cli::util::{self, SESSION_RPC_DEADLINE, SessionSource};
use ainb::interactive::session_manager::{ModelSource, SessionMetadata, SessionStore};
use ainb::models::session::SessionAgentType;
use ainb::terminal_clients::TerminalClients;
use ainb::{AppState, Effect, Keymap, dispatch};
use chrono::Utc;
use ratatui::layout::Rect;
use ratatui::{Terminal, TerminalOptions, Viewport};
use uuid::Uuid;

#[path = "support/fake_session_daemon.rs"]
mod fake_session_daemon;
#[path = "support/fleet_hangar.rs"]
mod fleet_hangar;
use fake_session_daemon::{fake_daemon, ready_list};
use fleet_hangar::EnvGuard;

#[test]
fn a_session_store_write_does_not_hold_the_host_tick() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let hangar_home = root.path().join("hangar");
    std::fs::create_dir_all(home.join(".agents-in-a-box")).unwrap();
    std::fs::create_dir_all(hangar_home.join("hangar")).unwrap();
    let _env = [
        EnvGuard::set("HOME", &home),
        EnvGuard::set("AINB_HOME", &home),
        EnvGuard::set("AINB_HANGAR_HOME", &hangar_home),
    ];
    util::advertise_workspace_sessions_for_tests(true);

    // Sessions are present: the write has a row to change, so nothing short
    // circuits before the daemon is asked.
    let session = SessionMetadata {
        session_id: Uuid::new_v4(),
        tmux_session_name: "tmux_p6e-host-write".to_string(),
        worktree_path: home.join("work"),
        workspace_name: "ws".to_string(),
        created_at: Utc::now(),
        agent_type: SessionAgentType::Claude,
        headroom_enabled: true,
        rtk_enabled: false,
        skip_permissions: None,
        model: None,
        model_source: ModelSource::LegacyTyped,
        codex_model: None,
        codex_thread_id: None,
    };
    let mut store = SessionStore::default();
    store.upsert(session.clone());
    store.save().unwrap();

    // Hello and the resolve probe are answered; nothing after that ever is.
    // The fake runs on this test's runtime, so it keeps answering while the
    // host's worker waits.
    fake_daemon(&rt, &hangar_home, |_, n| (n == 0).then(ready_list));
    let source = rt.block_on(util::session_source());
    assert!(matches!(source, SessionSource::Daemon(_)), "{source:?}");

    // A fixed viewport never asks the real terminal for its size, and the
    // store write draws nothing (as `persist_executor.rs` does it).
    let mut terminal = Terminal::with_options(
        ratatui::backend::CrosstermBackend::new(std::io::stdout()),
        TerminalOptions {
            viewport: Viewport::Fixed(Rect::new(0, 0, 80, 24)),
        },
    )
    .expect("terminal");
    let mut state = AppState::new();
    let effect = Effect::Persist(Persist::SessionHeadroom {
        tmux_session: session.tmux_session_name.clone(),
        expected: true,
        enabled: false,
    });

    let started = Instant::now();
    let reports = rt
        .block_on(ainb::effect_host::execute(
            effect,
            &mut terminal,
            &UiState::default(),
            &mut TerminalClients::default(),
            None,
        ))
        .expect("the executor ran");
    let on_tick = started.elapsed();
    assert!(
        on_tick < Duration::from_millis(250),
        "the session-store write held the tick: {on_tick:?}"
    );
    assert!(
        reports.is_empty(),
        "the write reports later, not on the tick: {reports:?}"
    );

    // The failure comes back as the deferred report the reducer turns into a
    // notice, within the client's deadline plus the host's own slack.
    let keymap = Keymap::defaults();
    let deadline = Instant::now() + SESSION_RPC_DEADLINE * 3;
    let notice = loop {
        for report in ainb::effect_host::take_deferred_reports() {
            dispatch(&mut state, &keymap, &mut NoRenderer, report);
        }
        let notice = state
            .shell
            .notifications
            .iter()
            .find(|note| note.notification_type == NotificationType::Error)
            .map(|note| note.message.clone());
        if let Some(notice) = notice {
            break notice;
        }
        assert!(
            Instant::now() < deadline,
            "the failed write never came back as a notice"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(
        notice.contains("the session store"),
        "the notice does not name the store: {notice}"
    );
    rt.shutdown_background();
}
