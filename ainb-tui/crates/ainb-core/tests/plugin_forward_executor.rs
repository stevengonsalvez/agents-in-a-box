#![allow(missing_docs)]

// ABOUTME: Plugin input is an effect the host runs with the runtime it owns. A
// back key the host cannot deliver comes back as a report, and the reducer
// leaves the plugin screen itself; other undelivered input is dropped.

use ainb::app::ui_state::UiState;
use ainb::app::{NoRenderer, PluginInput};
use ainb::terminal_clients::TerminalClients;
use ainb::{AppState, Effect, Keymap, dispatch};
use ratatui::layout::Rect;
use ratatui::{Terminal, TerminalOptions, Viewport};

fn forward(back: bool) -> Effect {
    Effect::ForwardToPlugin {
        plugin: "burndown".to_string(),
        screen: ainb::app::screens::ids::ANALYTICS.to_string(),
        input: PluginInput::Key(ainb_plugin_runtime::KeyEvent {
            code: ainb_plugin_runtime::KeyCode::Esc,
            mods: 0,
            kind: ainb_plugin_runtime::KeyKind::default(),
        }),
        back,
    }
}

#[tokio::test]
async fn an_undelivered_back_key_leaves_the_plugin_screen_and_other_input_is_dropped() {
    let home = tempfile::tempdir().expect("scratch home");
    std::env::set_var("HOME", home.path());
    let mut terminal = Terminal::with_options(
        ratatui::backend::CrosstermBackend::new(std::io::stdout()),
        TerminalOptions {
            viewport: Viewport::Fixed(Rect::new(0, 0, 80, 24)),
        },
    )
    .expect("terminal");
    // A runtime with no plugins registered: nothing takes the key.
    let (runtime, handle) = ainb_plugin_runtime::Runtime::new().expect("runtime");
    let ui = UiState::default();
    let mut clients = TerminalClients::default();

    let dropped = ainb::effect_host::execute(
        forward(false),
        &mut terminal,
        &ui,
        &mut clients,
        Some(&handle),
    )
    .await
    .expect("executor ran");
    assert!(
        dropped.is_empty(),
        "undelivered input that does not leave says nothing"
    );

    let reports = ainb::effect_host::execute(
        forward(true),
        &mut terminal,
        &ui,
        &mut clients,
        Some(&handle),
    )
    .await
    .expect("executor ran");
    assert_eq!(reports.len(), 1, "an undelivered back key is reported");

    let mut state = AppState::new();
    state.shell.previous_screen = Some(ainb::app::screens::ids::SESSION_LIST.to_string());
    state.shell.current_screen = ainb::app::screens::ids::ANALYTICS.to_string();
    for report in reports {
        let _ = dispatch(&mut state, &Keymap::defaults(), &mut NoRenderer, report);
    }
    assert_ne!(
        state.shell.current_screen,
        ainb::app::screens::ids::ANALYTICS,
        "the reducer left the screen the key was for"
    );

    runtime.shutdown();
}
