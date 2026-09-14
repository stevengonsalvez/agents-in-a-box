#![allow(missing_docs)]

// ABOUTME: The terminal host runs a store write through its effect executor
// and hands a failure back as the report the reducer turns into a notice, the
// whole round trip a host owes for `Effect::Persist`.

use ainb::app::state::NotificationType;
use ainb::app::ui_state::UiState;
use ainb::app::{NoRenderer, Persist, Snapshot};
use ainb::terminal_clients::TerminalClients;
use ainb::{AppState, Effect, Keymap, dispatch};
use ratatui::layout::Rect;
use ratatui::{Terminal, TerminalOptions, Viewport};

#[tokio::test]
async fn a_failed_store_write_comes_back_through_dispatch_as_a_notice() {
    let home = tempfile::tempdir().expect("scratch home");
    std::env::set_var("HOME", home.path());
    let config_file = home.path().join(".agents-in-a-box").join("config").join("config.toml");
    std::fs::create_dir_all(&config_file).expect("a directory where the file goes");

    // A fixed viewport never asks the real terminal for its size; the store
    // write does not draw.
    let mut terminal = Terminal::with_options(
        ratatui::backend::CrosstermBackend::new(std::io::stdout()),
        TerminalOptions {
            viewport: Viewport::Fixed(Rect::new(0, 0, 80, 24)),
        },
    )
    .expect("terminal");
    let mut state = AppState::new();
    let effect = Effect::Persist(Persist::AppConfig {
        config: Snapshot(state.config.app_config.clone()),
        keys: vec!["ui_preferences.show_session_menu_bar".to_string()],
    });

    let reports = ainb::effect_host::execute(
        effect,
        &mut terminal,
        &UiState::default(),
        &mut TerminalClients::default(),
        None,
    )
    .await
    .expect("the executor ran");

    assert_eq!(reports.len(), 1, "one report for the failed write");
    let keymap = Keymap::defaults();
    for report in reports {
        let follow_up = dispatch(&mut state, &keymap, &mut NoRenderer, report);
        assert!(follow_up.is_empty(), "a failed write queues nothing more");
    }
    assert!(
        state
            .shell
            .notifications
            .iter()
            .any(|note| note.notification_type == NotificationType::Error
                && note.message.starts_with("Could not save settings:")),
        "{:?}",
        state.shell.notifications
    );
}
