//! The status bar's statusline row draws from sections alone.
//!
//! A mirrored host receives sections, not the probe that reads
//! `~/.claude/settings.json` or the watcher that polls the usage cache. These
//! frames are drawn in a scratch HOME where both would say "nothing wired,
//! no data", so the row can only show what the sections say.

#![allow(missing_docs)]

use ainb::app::screens::ids;
use ainb::app::state::AppState;
use ainb::app::ui_state::UiState;
use ainb::cli::statusline_install::StatuslineStatus;
use ainb::components::LayoutComponent;
use ainb::models::live_window::{LiveWindow, Source};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

static HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn frame_text(state: &AppState) -> String {
    let mut layout = LayoutComponent::new();
    let mut ui = UiState::default();
    let mut terminal = Terminal::new(TestBackend::new(160, 40)).expect("test terminal");
    terminal.draw(|frame| layout.render(frame, state, &mut ui)).expect("draw");
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn session_list() -> (tempfile::TempDir, AppState) {
    let home = tempfile::tempdir().expect("scratch home");
    std::env::set_var("HOME", home.path());
    let mut state = AppState::new();
    state.shell.current_screen = ids::SESSION_LIST.to_string();
    (home, state)
}

#[test]
fn the_quota_widget_draws_from_the_live_window_section() {
    let _lock = HOME_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let (_home, mut state) = session_list();
    state.config.statusline_status = Some(StatuslineStatus::Configured);
    state.fleet.live_window = LiveWindow {
        source: Source::Tier1Cache,
        five_hour_pct: Some(81),
        seven_day_pct: Some(24),
        ..LiveWindow::default()
    };

    let text = frame_text(&state);

    assert!(
        text.contains("81%"),
        "the 5h quota from the section:\n{text}"
    );
    assert!(
        !text.contains("press W"),
        "no CTA while data flows:\n{text}"
    );
}

#[test]
fn the_wiring_prompt_follows_the_statusline_section_not_the_probe() {
    let _lock = HOME_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let (_home, mut state) = session_list();

    // The probe would say NotConfigured here; the section says it is wired.
    state.config.statusline_status = Some(StatuslineStatus::Configured);
    assert!(!frame_text(&state).contains("press W"));

    state.config.statusline_status = Some(StatuslineStatus::NotConfigured);
    assert!(frame_text(&state).contains("press W"));
}
