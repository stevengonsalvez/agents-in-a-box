#![allow(missing_docs)]

// ABOUTME: A reducer that changes a store queues the write and returns: the
// file is untouched until a host runs the effect, and a write that fails comes
// back as a report the reducer turns into a notice. Every test here writes
// under a home directory it takes from the shared guard, which orders it
// against every other test in this binary that needs one.

use ainb_app::app::NoRenderer;
use ainb_app::app::reports;
use ainb_app::app::state::{NotificationType, SessionFilter};
use ainb_app::app::{Persist, Snapshot};
use ainb_app::config::AppConfig;
use ainb_app::config::ClaudeAuthProvider;
use ainb_app::{AppState, Effect, Keymap, dispatch};

#[path = "support/home.rs"]
mod home;

use home::ScopedHome;

/// The home directory this test writes under, held until the test ends.
fn scratch_home() -> ScopedHome {
    ScopedHome::new()
}

fn config_file(home: &ScopedHome) -> std::path::PathBuf {
    home.path().join(".agents-in-a-box").join("config").join("config.toml")
}

#[test]
fn a_settings_change_is_written_by_the_host_and_a_failed_write_is_reported() {
    let home = scratch_home();
    let config_file = config_file(&home);
    let keymap = Keymap::defaults();
    let mut state = AppState::new();

    state.toggle_session_menu_bar();
    let effects = state.take_effects();
    let [Effect::Persist(store)] = effects.as_slice() else {
        panic!("one persistence effect, got {effects:?}");
    };
    assert_eq!(store.store_id(), "config");
    assert!(
        !config_file.exists(),
        "the reducer step wrote nothing to disk"
    );

    ainb_app::config::persist::write(store).expect("the host's write lands");
    assert!(config_file.is_file());

    // A config path the host cannot write: the write fails, and its report
    // becomes a notice instead of blocking or disappearing.
    std::fs::remove_file(&config_file).expect("remove config");
    std::fs::create_dir_all(&config_file).expect("a directory where the file goes");
    state.toggle_session_menu_bar();
    let effects = state.take_effects();
    let [Effect::Persist(store)] = effects.as_slice() else {
        panic!("one persistence effect, got {effects:?}");
    };
    let error = ainb_app::config::persist::write(store).expect_err("cannot write over a directory");
    let _ = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        reports::persist_failed(store.store_id(), &error),
    );

    assert!(
        state
            .shell
            .notifications
            .iter()
            .any(|note| note.notification_type == NotificationType::Error
                && note.message.contains("Could not save settings")),
        "{:?}",
        state.shell.notifications
    );
}

/// The auth provider is one key: a later write from a copy of the config that
/// still holds the old provider does not put it back.
#[test]
fn a_provider_write_survives_a_later_write_from_a_stale_copy() {
    let home = scratch_home();
    let stale = AppConfig::default();
    assert_ne!(
        stale.authentication.claude_provider,
        ClaudeAuthProvider::ApiKey
    );

    let mut chosen = stale.clone();
    chosen.authentication.claude_provider = ClaudeAuthProvider::ApiKey;
    ainb_app::config::persist::write(&Persist::AppConfig {
        config: Snapshot(chosen),
        keys: vec!["authentication.claude_provider".to_string()],
    })
    .expect("provider written");

    let mut toggled = stale;
    toggled.ui_preferences.show_session_menu_bar = !toggled.ui_preferences.show_session_menu_bar;
    ainb_app::config::persist::write(&Persist::AppConfig {
        config: Snapshot(toggled),
        keys: vec!["ui_preferences.show_session_menu_bar".to_string()],
    })
    .expect("toggle written");

    let saved = std::fs::read_to_string(config_file(&home)).expect("config written");
    let table: toml::Table = saved.parse().expect("config parses");
    assert_eq!(
        table["authentication"]["claude_provider"].as_str(),
        Some("api_key"),
        "{saved}"
    );
}

/// Choosing a sign-in method in onboarding shows on the same step, from the
/// reducer's own config, before any host has written it.
#[test]
fn the_onboarding_status_shows_the_method_chosen_in_the_same_step() {
    use ainb_app::app::events::{AppEvent, EventHandler};
    use ainb_app::components::onboarding::{AuthAgent, AuthMethodKind, AuthPane, OnboardingStep};

    let _home = scratch_home();
    let mut state = AppState::new();
    state.config.app_config.authentication.claude_provider = ClaudeAuthProvider::ApiKey;
    state.start_onboarding(false, Some(OnboardingStep::Authentication));
    let claude = |state: &AppState| {
        state
            .onboarding
            .onboarding_state
            .as_ref()
            .expect("onboarding")
            .auth_statuses
            .iter()
            .find(|status| status.agent == AuthAgent::Claude)
            .expect("claude row")
            .method
    };
    assert_eq!(claude(&state), AuthMethodKind::ApiKey);

    state.onboarding.onboarding_state.as_mut().expect("onboarding").auth_pane =
        AuthPane::MethodPicker {
            agent: AuthAgent::Claude,
            cursor: 0,
        };
    EventHandler::process_event(AppEvent::OnboardingAuthSelect, &mut state);

    assert_eq!(claude(&state), AuthMethodKind::Login);
    assert!(
        state.take_effects().iter().any(|effect| matches!(
            effect,
            Effect::Persist(Persist::AppConfig { keys, .. })
                if keys == &["authentication.claude_provider".to_string()]
        )),
        "the provider key is queued for the host"
    );
}

/// One step that changes the config twice queues one write, with every key it
/// changed and the final values.
#[test]
fn a_step_that_changes_a_store_twice_writes_it_once_with_the_final_state() {
    let _home = scratch_home();
    let mut state = AppState::new();
    let shown = state.config.app_config.ui_preferences.show_session_menu_bar;

    state.toggle_session_menu_bar();
    state.cycle_session_filter();
    state.toggle_session_menu_bar();

    let effects = state.take_effects();
    let [Effect::Persist(Persist::AppConfig { config, keys })] = effects.as_slice() else {
        panic!("one config write, got {effects:?}");
    };
    assert_eq!(
        keys,
        &[
            "ui_preferences.show_session_menu_bar".to_string(),
            "ui_preferences.session_filter".to_string()
        ]
    );
    assert_eq!(config.0.ui_preferences.show_session_menu_bar, shown);
    assert_eq!(
        config.0.ui_preferences.session_filter,
        state.config.app_config.ui_preferences.session_filter
    );
}

fn write_session_store(home: &ScopedHome, headroom_enabled: bool) -> std::path::PathBuf {
    let path = home.path().join(".agents-in-a-box").join("sessions.json");
    std::fs::create_dir_all(path.parent().expect("dir")).expect("store dir");
    let store = serde_json::json!({
        "sessions": {
            "ainb-persist": {
                "session_id": "00000000-0000-0000-0000-000000000001",
                "tmux_session_name": "ainb-persist",
                "worktree_path": "/tmp/ainb-persist",
                "workspace_name": "persist",
                "created_at": "2026-09-14T00:00:00Z",
                "headroom_enabled": headroom_enabled
            }
        }
    });
    std::fs::write(&path, serde_json::to_string_pretty(&store).expect("json")).expect("store");
    path
}

fn headroom_on_disk(path: &std::path::Path) -> bool {
    let store: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("store")).expect("json");
    store["sessions"]["ainb-persist"]["headroom_enabled"].as_bool().expect("flag")
}

/// The Headroom switch is set only while it still reads what the step decided
/// from; a switch another process moved is left alone and reported.
#[test]
fn a_headroom_write_does_not_overwrite_a_switch_that_moved() {
    let home = scratch_home();
    let path = write_session_store(&home, false);

    let error = ainb_app::config::persist::write(&Persist::SessionHeadroom {
        tmux_session: "ainb-persist".to_string(),
        expected: true,
        enabled: false,
    })
    .expect_err("the switch was already off");
    assert!(error.contains("changed since"), "{error}");
    assert!(!headroom_on_disk(&path));

    ainb_app::config::persist::write(&Persist::SessionHeadroom {
        tmux_session: "ainb-persist".to_string(),
        expected: false,
        enabled: true,
    })
    .expect("the switch still reads off");
    assert!(headroom_on_disk(&path));
}

/// Writing the session store keeps the file's mode.
#[cfg(unix)]
#[test]
fn the_session_store_keeps_its_mode_through_a_write() {
    use std::os::unix::fs::PermissionsExt;

    let home = scratch_home();
    let path = write_session_store(&home, true);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");

    ainb_app::config::persist::write(&Persist::SessionHeadroom {
        tmux_session: "ainb-persist".to_string(),
        expected: true,
        enabled: false,
    })
    .expect("toggled");

    assert!(!headroom_on_disk(&path));
    let mode = std::fs::metadata(&path).expect("store").permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

/// An onboarding record that does not parse is not replaced by a default one
/// holding only the directories.
#[test]
fn a_git_directories_write_leaves_an_unreadable_onboarding_record_alone() {
    let home = scratch_home();
    let path = home.path().join(".agents-in-a-box").join("config").join("onboarding.toml");
    std::fs::create_dir_all(path.parent().expect("dir")).expect("config dir");
    let broken = "completed = [not toml";
    std::fs::write(&path, broken).expect("broken record");

    let error = ainb_app::config::persist::write(&Persist::OnboardingGitDirectories(vec![
        home.path().to_path_buf(),
    ]))
    .expect_err("the record does not load");

    assert!(!error.is_empty());
    assert_eq!(std::fs::read_to_string(&path).expect("record"), broken);
}

#[test]
fn the_persisted_session_filter_is_applied_at_startup() {
    // Shift+F writes `ui_preferences.session_filter`; the next process must
    // start on it rather than on `All` (#1208). The TUI's rows alone read it:
    // `ainb list --frame` and the web list every session whatever it says
    // (`crates/ainb-core/tests/list_frame_every_session.rs`).
    let home = scratch_home();
    let config_file = config_file(&home);
    std::fs::create_dir_all(config_file.parent().expect("config dir")).expect("config dir");
    std::fs::write(
        &config_file,
        "[ui_preferences]\nsession_filter = \"active_only\"\n",
    )
    .expect("persist the filter");

    let state = AppState::new();

    assert_eq!(
        state.sessions.session_filter,
        SessionFilter::ActiveOnly,
        "the state starts on the filter the file holds"
    );
    assert_eq!(
        state.config.app_config.ui_preferences.session_filter,
        SessionFilter::ActiveOnly,
        "and the config section agrees, so the next cycle persists from it"
    );
}
