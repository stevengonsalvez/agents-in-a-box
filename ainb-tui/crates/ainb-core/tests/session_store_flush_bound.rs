#![allow(missing_docs)]

// ABOUTME: P6e, the terminal host's quit. A queued session-store write can be
// sitting on a daemon that answered the resolve and then nothing, and a quit
// must not wait on it: the wait is bounded for the whole queue, and what it
// leaves behind is reported rather than waited for. The order matters as much
// as the bound, so the second test holds the host's teardown to restoring the
// terminal before it waits at all: a wait inside the alternate screen is a
// frozen screen to the operator.
//
// Own binary: the process's session source is decided once, and `AINB_HOME` /
// `AINB_HANGAR_HOME` are process-wide.

use std::time::{Duration, Instant};

use ainb::Effect;
use ainb::app::Persist;
use ainb::app::ui_state::UiState;
use ainb::cli::util::{self, SESSION_STORE_FLUSH_BOUND, SessionSource};
use ainb::interactive::session_manager::{ModelSource, SessionMetadata, SessionStore};
use ainb::models::session::SessionAgentType;
use ainb::terminal_clients::TerminalClients;
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

/// Slack over the bound: the flush waits on a channel, so the only time past
/// the bound is the wake-up itself.
const SLACK: Duration = Duration::from_millis(750);

#[test]
fn a_quit_waits_for_the_queued_writes_only_to_the_bound() {
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

    let session = SessionMetadata {
        session_id: Uuid::new_v4(),
        tmux_session_name: "tmux_p6e-flush-bound".to_string(),
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

    // Hello and the resolve probe are answered; the write never is, so the
    // worker is still waiting when the quit comes.
    fake_daemon(&rt, &hangar_home, |_, n| (n == 0).then(ready_list));
    let source = rt.block_on(util::session_source());
    assert!(matches!(source, SessionSource::Daemon(_)), "{source:?}");

    let mut terminal = Terminal::with_options(
        ratatui::backend::CrosstermBackend::new(std::io::stdout()),
        TerminalOptions {
            viewport: Viewport::Fixed(Rect::new(0, 0, 80, 24)),
        },
    )
    .expect("terminal");
    let effect = Effect::Persist(Persist::SessionHeadroom {
        tmux_session: session.tmux_session_name.clone(),
        expected: true,
        enabled: false,
    });
    let reports = rt
        .block_on(ainb::effect_host::execute(
            effect,
            &mut terminal,
            &UiState::default(),
            &mut TerminalClients::default(),
            None,
        ))
        .expect("the executor ran");
    assert!(reports.is_empty(), "the write reported on the tick");
    // The quit: it waits for the queue, and only to the bound.
    let started = Instant::now();
    let dropped = ainb::effect_host::finish_session_store_writes(SESSION_STORE_FLUSH_BOUND);
    let waited = started.elapsed();
    assert!(
        waited < SESSION_STORE_FLUSH_BOUND + SLACK,
        "the quit waited past the bound: {waited:?}"
    );
    assert_eq!(
        dropped, 1,
        "the write that never landed was not reported as dropped"
    );
    rt.shutdown_background();
}

/// The host restores the terminal first, then waits: a wait inside the
/// alternate screen is a screen the operator watches freeze.
#[test]
fn the_terminal_is_restored_before_the_host_waits_for_the_writes() {
    let path: std::path::PathBuf = [env!("CARGO_MANIFEST_DIR"), "src", "main.rs"].iter().collect();
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let lines: Vec<&str> = source.lines().collect();
    let cleanup = lines
        .iter()
        .position(|line| line.contains("cleanup_terminal_with_instance(&mut terminal)"))
        .expect("the host no longer restores the terminal by that name");
    let flush = lines
        .iter()
        .position(|line| line.contains("finish_session_store_writes("))
        .expect("the host no longer waits for its queued session-store writes");
    assert!(
        flush > cleanup,
        "main.rs:{} waits for the queued writes at main.rs:{}, inside the alternate screen",
        flush + 1,
        cleanup + 1
    );
}
