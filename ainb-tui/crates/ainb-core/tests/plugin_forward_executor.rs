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

#[path = "tripwire_helpers.rs"]
#[allow(dead_code, unused_variables)]
mod tripwire_helpers;

/// One scratch HOME for the whole binary: the tests run in parallel threads,
/// and a HOME each one set and dropped could pull the other's out from under it.
fn isolated_home() {
    static HOME: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    HOME.get_or_init(|| {
        let home = tempfile::tempdir().expect("scratch home");
        std::env::set_var("HOME", home.path());
        home
    });
}

fn is_input_undelivered(report: &ainb::Intent) -> bool {
    matches!(report, ainb::Intent::Command(id, _)
        if id.as_str() == ainb::app::reports::ids::PLUGIN_INPUT_UNDELIVERED)
}

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
    isolated_home();
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
    assert!(
        matches!(reports.as_slice(), [report] if is_input_undelivered(report)),
        "an undelivered back key is reported: {reports:?}"
    );

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

/// A plugin the runtime has but whose render blew its budget is not serviced:
/// a back key sent to it is reported, so Esc still leaves a wedged screen.
#[tokio::test]
async fn a_back_key_sent_to_a_wedged_plugin_is_reported() {
    use std::time::Duration;

    isolated_home();
    let fixture = tripwire_helpers::sibling_bin("ainb-slow-fixture-plugin");
    // A render 4093 wide is held by the slow fixture (its `HOLD_VIEWPORT_WIDTH`,
    // also written into ainb-plugin-runtime's `tests/render_watchdog.rs`) until
    // it receives `r`, so only the 20ms watchdog can answer it and the Esc sent
    // below does not release it. Racing the fixture's 200ms sleep instead let a
    // loaded runner's late answer clear the wedge first (#1133).
    let (runtime, handle) =
        ainb_plugin_runtime::Runtime::with_config(ainb_plugin_runtime::RuntimeConfig {
            default_render_timeout: Duration::from_millis(20),
            ..ainb_plugin_runtime::RuntimeConfig::default()
        })
        .expect("runtime");
    let id = tripwire_helpers::register_plugin(&runtime, "burndown", fixture);
    let rx = handle.render(&id, ainb_plugin_protocol::params::Viewport::new(4093, 8), 0);
    let _ = tokio::time::timeout(Duration::from_secs(5), rx)
        .await
        .expect("watchdog answers");
    assert!(
        handle.render_wedged(&id),
        "precondition: the plugin is wedged"
    );

    let mut terminal = Terminal::with_options(
        ratatui::backend::CrosstermBackend::new(std::io::stdout()),
        TerminalOptions {
            viewport: Viewport::Fixed(Rect::new(0, 0, 80, 24)),
        },
    )
    .expect("terminal");
    let reports = ainb::effect_host::execute(
        forward(true),
        &mut terminal,
        &UiState::default(),
        &mut TerminalClients::default(),
        Some(&handle),
    )
    .await
    .expect("executor ran");
    assert!(
        matches!(reports.as_slice(), [report] if is_input_undelivered(report)),
        "a back key to a wedged plugin is reported: {reports:?}"
    );

    runtime.shutdown();
}
