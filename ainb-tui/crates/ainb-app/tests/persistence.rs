#![allow(missing_docs)]

// ABOUTME: A reducer that changes a store queues the write and returns: the
// file is untouched until a host runs the effect, and a write that fails comes
// back as a report the reducer turns into a notice.

use ainb_app::app::NoRenderer;
use ainb_app::app::reports;
use ainb_app::app::state::NotificationType;
use ainb_app::{AppState, Effect, Keymap, dispatch};

#[test]
fn a_settings_change_is_written_by_the_host_and_a_failed_write_is_reported() {
    let home = tempfile::tempdir().expect("scratch home");
    std::env::set_var("HOME", home.path());
    let config_file = home.path().join(".agents-in-a-box").join("config").join("config.toml");
    let keymap = Keymap::defaults();
    let mut state = AppState::new();

    state.toggle_session_menu_bar();
    let effects = state.take_effects();
    let [Effect::Persist(store)] = effects.as_slice() else {
        panic!("one persistence effect, got {effects:?}");
    };
    assert_eq!(store.store(), "settings");
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
        reports::persist_failed(store.store(), &error),
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
