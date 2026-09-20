#![allow(missing_docs)]

// ABOUTME: P6e, the one worker the terminal host queues session-store writes
// on. Two things about it that the queue is worth nothing without: the writes
// land in the order they were queued, and a worker that is gone is replaced
// rather than failing every write after it.
//
// Own binary: the process's session source is decided once, and `AINB_HOME` /
// `AINB_HANGAR_HOME` are process-wide.

use ainb::app::Persist;
use ainb::app::ui_state::UiState;
use ainb::cli::util::SESSION_STORE_FLUSH_BOUND;
use ainb::interactive::session_manager::{ModelSource, SessionMetadata, SessionStore};
use ainb::models::session::SessionAgentType;
use ainb::terminal_clients::TerminalClients;
use ainb::{Effect, effect_host};
use chrono::Utc;
use ratatui::layout::Rect;
use ratatui::{Terminal, TerminalOptions, Viewport};
use uuid::Uuid;

#[path = "support/fleet_hangar.rs"]
mod fleet_hangar;
use fleet_hangar::EnvGuard;

/// The one worker and the one store are process-wide, so the two tests take
/// turns rather than draining each other's queue.
static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// One scratch home for the binary, set before any write.
fn scratch_home() -> &'static std::path::Path {
    static HOME: std::sync::OnceLock<(tempfile::TempDir, Vec<EnvGuard>, std::path::PathBuf)> =
        std::sync::OnceLock::new();
    &HOME
        .get_or_init(|| {
            let root = tempfile::tempdir().expect("scratch home");
            let home = root.path().join("home");
            std::fs::create_dir_all(home.join(".agents-in-a-box")).expect("home");
            let guards = vec![
                EnvGuard::set("HOME", &home),
                EnvGuard::set("AINB_HOME", &home),
                EnvGuard::set("AINB_HANGAR_HOME", &home.join(".agents-in-a-box")),
            ];
            (root, guards, home)
        })
        .2
}

/// A terminal that never asks the real one for its size. A session-store write
/// draws nothing, so the viewport is only there to build the executor.
fn terminal() -> Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>> {
    Terminal::with_options(
        ratatui::backend::CrosstermBackend::new(std::io::stdout()),
        TerminalOptions {
            viewport: Viewport::Fixed(Rect::new(0, 0, 80, 24)),
        },
    )
    .expect("terminal")
}

/// One seeded session with Headroom on.
fn seed(tmux: &str, home: &std::path::Path) {
    let mut store = SessionStore::load();
    store.upsert(SessionMetadata {
        session_id: Uuid::new_v4(),
        tmux_session_name: tmux.to_string(),
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
    });
    store.save().expect("seed sessions.json");
}

/// Queue one Headroom write through the host, as a reducer step would.
fn queue(
    rt: &tokio::runtime::Runtime,
    terminal: &mut Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    tmux: &str,
    expected: bool,
    enabled: bool,
) {
    let reports = rt
        .block_on(effect_host::execute(
            Effect::Persist(Persist::SessionHeadroom {
                tmux_session: tmux.to_string(),
                expected,
                enabled,
            }),
            terminal,
            &UiState::default(),
            &mut TerminalClients::default(),
            None,
        ))
        .expect("the executor ran");
    assert!(reports.is_empty(), "the write reported on the tick");
}

#[test]
fn two_queued_writes_land_in_the_order_they_were_queued() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|p| p.into_inner());
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let home = scratch_home();
    let tmux = "tmux_p6e-fifo";
    seed(tmux, home);
    let mut terminal = terminal();

    // Each write is a compare-and-set against what the one before it left.
    // Run out of order, a write finds a value it did not expect, refuses, and
    // reports: the order is what makes all of them land. Ten pairs rather
    // than one, so a host that ran its writes side by side loses the race
    // somewhere rather than getting away with it.
    for _ in 0..10 {
        queue(&rt, &mut terminal, tmux, true, false);
        queue(&rt, &mut terminal, tmux, false, true);
    }

    let dropped = effect_host::finish_session_store_writes(SESSION_STORE_FLUSH_BOUND);
    assert_eq!(dropped, 0, "a queued write did not land within the bound");
    assert!(
        SessionStore::load().sessions[tmux].headroom_enabled,
        "the last write did not land last"
    );
    let reports = effect_host::take_deferred_reports();
    assert!(
        reports.is_empty(),
        "a write was refused, so they ran out of order: {reports:?}"
    );
}

#[test]
fn a_worker_that_is_gone_is_replaced_rather_than_failing_every_write() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|p| p.into_inner());
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let home = scratch_home();
    let tmux = "tmux_p6e-dead-worker";
    seed(tmux, home);
    let mut terminal = terminal();

    // A first write, so there is a worker to lose. It lands: the slot is left
    // holding a sender nothing reads, as a worker that panicked inside a
    // write leaves it.
    queue(&rt, &mut terminal, tmux, true, false);
    effect_host::break_the_session_store_worker_for_tests();
    assert!(
        !SessionStore::load().sessions[tmux].headroom_enabled,
        "the write before the loss did not land"
    );

    // The next write has to notice the worker is gone and start another.
    queue(&rt, &mut terminal, tmux, false, true);
    let dropped = effect_host::finish_session_store_writes(SESSION_STORE_FLUSH_BOUND);
    assert_eq!(dropped, 0, "the write after the loss did not land");
    assert!(
        SessionStore::load().sessions[tmux].headroom_enabled,
        "the write after the worker was lost never landed"
    );
    let reports = effect_host::take_deferred_reports();
    assert!(
        reports.is_empty(),
        "the write after the loss was reported as failed: {reports:?}"
    );
}
