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

/// A plugin the runtime has but whose render blew its budget is not serviced:
/// a back key sent to it is reported, so Esc still leaves a wedged screen.
#[tokio::test]
async fn a_back_key_sent_to_a_wedged_plugin_is_reported() {
    use ainb_plugin_protocol::manifest::{
        Capabilities, Lifecycle, Manifest, PluginMeta, Provides, SpawnMode, Subscribes,
    };
    use std::time::Duration;

    let home = tempfile::tempdir().expect("scratch home");
    std::env::set_var("HOME", home.path());
    let fixture = std::path::PathBuf::from(env!("CARGO_BIN_EXE_ainb"))
        .parent()
        .expect("target dir")
        .join("ainb-slow-fixture-plugin");
    assert!(
        fixture.exists(),
        "slow fixture not found at {fixture:?}: run `cargo build -p ainb-plugin-runtime --bins`"
    );
    // The slow fixture sleeps 200ms in render; a 20ms budget wedges it.
    let (runtime, handle) =
        ainb_plugin_runtime::Runtime::with_config(ainb_plugin_runtime::RuntimeConfig {
            default_render_timeout: Duration::from_millis(20),
            ..ainb_plugin_runtime::RuntimeConfig::default()
        })
        .expect("runtime");
    let manifest = Manifest {
        plugin: PluginMeta {
            name: "burndown".into(),
            version: "0.1.0".into(),
            abi_version: 2,
            description: "renders slower than the budget".into(),
        },
        capabilities: Capabilities::default(),
        provides: Provides {
            screens: vec![],
            commands: vec![],
            cli_namespaces: vec![],
            snapshots: vec![],
        },
        subscribes: Subscribes::default(),
        lifecycle: Lifecycle {
            spawn: SpawnMode::Lazy,
            idle_reap_secs: 600,
        },
        config: Vec::new(),
    };
    let plugin = ainb_plugin_runtime::registry::RegisteredPlugin::new(
        manifest,
        fixture,
        std::path::PathBuf::from("/dev/null/manifest.toml"),
    );
    let id = plugin.id.clone();
    runtime.register(plugin);
    let rx = handle.render(&id, ainb_plugin_protocol::params::Viewport::new(40, 8), 0);
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
    assert_eq!(
        reports.len(),
        1,
        "a back key to a wedged plugin is reported"
    );

    runtime.shutdown();
}
