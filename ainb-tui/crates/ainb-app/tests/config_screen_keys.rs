// ABOUTME: Regression tests for issue #989: on the Config screen `enter` opens
// the setting editor again, and characters typed into the `/` filter reach the
// query instead of being swallowed by navigation rows and global shortcuts.
//
// Every key goes through the real default keymap and the real reducer, the
// same path the TUI takes, so a missing keymap row fails here rather than in a
// manual checkpoint.

use ainb_app::app::events::AppEvent;
use ainb_app::app::state::{ConfigPane, ConfigScreenState};
use ainb_app::app::{EventHandler, NoRenderer};
use ainb_app::{AppState, Chord, Intent, Key, Keymap, Mods, dispatch};

const CONFIG: &str = "config";

fn config_screen() -> AppState {
    let mut state = AppState::new();
    state.shell.current_screen = CONFIG.to_string();
    state
}

fn press(state: &mut AppState, key: Key) {
    dispatch(
        state,
        &Keymap::defaults(),
        &mut NoRenderer,
        Intent::Key(Chord::new(key, Mods::NONE)),
    );
}

fn type_text(state: &mut AppState, text: &str) {
    for character in text.chars() {
        press(state, Key::Char(character));
    }
}

/// Move the settings cursor onto the first editable row that is not the
/// Claude auth row, which has its own popup.
fn select_plain_setting(state: &mut AppState) {
    let screen = &mut state.config.config_screen_state;
    screen.focused_pane = ConfigPane::Settings;
    let rows = screen.current_settings().len();
    for _ in 0..rows {
        let plain = screen.current_setting().is_some_and(|row| {
            row.key != ConfigScreenState::CLAUDE_PROVIDER_KEY
                && screen.current_read_only_reason().is_none()
        });
        if plain {
            return;
        }
        screen.select_next_setting();
    }
    panic!("the first config category has no plain editable row");
}

#[test]
fn enter_on_settings_pane_opens_the_editor() {
    let mut state = config_screen();
    select_plain_setting(&mut state);
    assert!(!state.config.config_popup_state.show_popup);

    press(&mut state, Key::Enter);

    assert!(
        state.config.config_popup_state.show_popup,
        "enter on a settings row must open the editor popup"
    );
}

#[test]
fn enter_on_categories_pane_toggles_the_tree_node() {
    let mut state = config_screen();
    state.config.config_screen_state.focused_pane = ConfigPane::Categories;

    let event = EventHandler::handle_key_event(Chord::new(Key::Enter, Mods::NONE), &mut state);

    assert!(
        matches!(event, Some(AppEvent::ConfigToggleExpand)),
        "enter on the categories pane must toggle the node, got {event:?}"
    );
}

#[test]
fn enter_on_claude_auth_row_opens_the_auth_provider_popup() {
    let mut state = config_screen();
    let screen = &mut state.config.config_screen_state;
    screen.start_search();
    for character in ConfigScreenState::CLAUDE_PROVIDER_KEY.chars() {
        screen.push_search_char(character);
    }
    assert_eq!(
        screen.current_setting().map(|row| row.key.as_str()),
        Some(ConfigScreenState::CLAUDE_PROVIDER_KEY)
    );
    // Leave search with the cursor still on the auth row.
    screen.search = None;
    screen.focused_pane = ConfigPane::Settings;
    assert_eq!(
        screen.current_setting().map(|row| row.key.as_str()),
        Some(ConfigScreenState::CLAUDE_PROVIDER_KEY)
    );

    let event = EventHandler::handle_key_event(Chord::new(Key::Enter, Mods::NONE), &mut state);

    assert!(
        matches!(event, Some(AppEvent::AuthProviderPopupOpen)),
        "enter on the Claude auth row must open the auth provider popup, got {event:?}"
    );
}

#[test]
fn typing_in_search_filters_the_rows() {
    let mut state = config_screen();
    let unfiltered = state.config.config_screen_state.current_settings().len();

    press(&mut state, Key::Char('/'));
    assert!(state.config.config_screen_state.is_searching());
    type_text(&mut state, "branch");

    let screen = &state.config.config_screen_state;
    assert_eq!(screen.search.as_deref(), Some("branch"));
    let rows = screen.current_settings();
    assert!(
        !rows.is_empty() && rows.len() != unfiltered,
        "the filter must narrow the rows (had {unfiltered}, now {})",
        rows.len()
    );
    assert!(
        rows.iter().any(|row| row.key.contains("branch_prefix")),
        "`branch` must match workspace_defaults.branch_prefix"
    );
    assert!(!state.shell.help_visible);
}

#[test]
fn search_letters_that_are_global_or_nav_keys_reach_the_query() {
    let mut state = config_screen();
    press(&mut state, Key::Char('/'));

    // `H` is the global help chord, `j`/`k`/`h`/`l` navigate and `s` saves.
    type_text(&mut state, "Hjkhls");

    assert_eq!(
        state.config.config_screen_state.search.as_deref(),
        Some("Hjkhls")
    );
    assert!(!state.shell.help_visible, "`H` in search must not open help");
}

#[test]
fn enter_in_search_opens_the_editor_for_the_filtered_row() {
    let mut state = config_screen();
    press(&mut state, Key::Char('/'));
    type_text(&mut state, "branch_prefix");
    assert!(
        state
            .config
            .config_screen_state
            .current_setting()
            .is_some_and(|row| row.key.ends_with("branch_prefix"))
    );

    press(&mut state, Key::Enter);

    assert!(state.config.config_popup_state.show_popup);
}
