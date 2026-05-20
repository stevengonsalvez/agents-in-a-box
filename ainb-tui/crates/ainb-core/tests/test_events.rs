// ABOUTME: Unit tests for event handling to ensure keyboard inputs map to correct app actions

use ainb::app::events::AppEvent;
use ainb::app::screens::ids as screen_ids;
use ainb::app::{AppState, EventHandler};
// UsagePeriod / UsageProviderFilter no longer used in this file —
// the test that referenced them has moved into the burndown plugin.
use chrono::NaiveDate;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

const fn create_key_event(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

const fn create_key_event_with_modifiers(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
    KeyEvent::new(code, modifiers)
}

#[test]
fn test_quit_key_events() {
    use ainb::app::screens::ids as screen_ids;
    let mut state = AppState::default();
    state.current_screen = screen_ids::SESSION_LIST.to_string();

    let quit_event1 =
        EventHandler::handle_key_event(create_key_event(KeyCode::Char('q')), &mut state);
    assert!(quit_event1.is_some());

    let quit_event2 = EventHandler::handle_key_event(create_key_event(KeyCode::Esc), &mut state);
    assert!(quit_event2.is_some());

    let quit_event3 = EventHandler::handle_key_event(
        create_key_event_with_modifiers(KeyCode::Char('c'), KeyModifiers::CONTROL),
        &mut state,
    );
    assert!(quit_event3.is_some());
}

#[test]
fn test_navigation_key_events() {
    use ainb::app::screens::ids as screen_ids;
    let mut state = AppState::default();
    state.current_screen = screen_ids::SESSION_LIST.to_string();

    // The session-list screen no longer accepts vim-style h/j/k/l —
    // navigation is on arrow keys (Down/Up/Left/Right) per the modern
    // ainb keymap. Verify the arrow-key contract instead; the hjkl
    // semantics were removed when the home screen was overhauled.
    let down_event = EventHandler::handle_key_event(create_key_event(KeyCode::Down), &mut state);
    assert!(down_event.is_some());

    let up_event = EventHandler::handle_key_event(create_key_event(KeyCode::Up), &mut state);
    assert!(up_event.is_some());

    let left_event = EventHandler::handle_key_event(create_key_event(KeyCode::Left), &mut state);
    assert!(left_event.is_some());

    let right_event = EventHandler::handle_key_event(create_key_event(KeyCode::Right), &mut state);
    assert!(right_event.is_some());
}

#[tokio::test]
async fn test_n_key_triggers_new_session() {
    use ainb::app::screens::ids as screen_ids;
    use ainb::app::state::AsyncAction;

    let mut state = AppState::default();
    state.current_screen = screen_ids::SESSION_LIST.to_string();

    // Simulate pressing 'n' key
    let key_event = KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE);

    // Handle the key event
    let app_event = EventHandler::handle_key_event(key_event, &mut state);

    // Should return NewSession event
    assert!(app_event.is_some());

    // Process the event
    if let Some(event) = app_event {
        EventHandler::process_event(event, &mut state);
    }

    // Should have set pending async action
    assert!(state.pending_async_action.is_some());

    // After PR-XXXX (repo-input flow), 'n' on SESSION_LIST queues
    // NewSessionWithRepoInput; if a repo-less path was taken it'd be
    // NewSessionNormal. Accept either variant — the contract under
    // test is "n triggers a new-session async action", not the
    // specific variant.
    match state.pending_async_action {
        Some(AsyncAction::NewSessionNormal) | Some(AsyncAction::NewSessionWithRepoInput) => {}
        ref other => panic!("Expected NewSession* async action, got: {other:?}"),
    }

    // Process the async action to complete the flow
    if let Err(e) = state.process_async_action().await {
        panic!("Failed to process async action: {e}");
    }

    // After processing, the behavior depends on whether current dir is a git repo
    // If it is, we should be in NewSession view with current dir
    // If it's not, we should be in SearchWorkspace view
    // Or we might still be in SessionList if auth setup is required
    assert!(
        state.current_screen == screen_ids::NEW_SESSION
            || state.current_screen == screen_ids::SEARCH_WORKSPACE
            || state.current_screen == screen_ids::SESSION_LIST
            || state.current_screen == screen_ids::AUTH_SETUP,
        "Unexpected screen: {:?}",
        state.current_screen
    );
    // The new session state might not be set if auth setup is required
    assert!(state.pending_async_action.is_none());
}

#[test]
fn test_arrow_key_navigation() {
    use ainb::app::screens::ids as screen_ids;
    let mut state = AppState::default();
    // These tests exercise the SESSION_LIST screen's navigation contract;
    // HomeScreen V2 (the v1 default) doesn't route arrow keys to session
    // navigation — its handler is sidebar/content-panel focused. Force
    // the screen to the relevant ID for the assertions to be meaningful.
    state.current_screen = screen_ids::SESSION_LIST.to_string();

    let down_arrow = EventHandler::handle_key_event(create_key_event(KeyCode::Down), &mut state);
    assert!(down_arrow.is_some());

    let up_arrow = EventHandler::handle_key_event(create_key_event(KeyCode::Up), &mut state);
    assert!(up_arrow.is_some());

    let left_arrow = EventHandler::handle_key_event(create_key_event(KeyCode::Left), &mut state);
    assert!(left_arrow.is_some());

    let right_arrow = EventHandler::handle_key_event(create_key_event(KeyCode::Right), &mut state);
    assert!(right_arrow.is_some());
}

#[test]
fn test_action_key_events() {
    use ainb::app::screens::ids as screen_ids;
    let mut state = AppState::default();
    // HomeScreen V2 reserves single-letter shortcuts for screen-nav (a, c,
    // i, etc.) — the session-list action set ('n', 'a', 's', 'd') only
    // resolves under the SESSION_LIST screen. Pin the screen first.
    state.current_screen = screen_ids::SESSION_LIST.to_string();

    let new_event =
        EventHandler::handle_key_event(create_key_event(KeyCode::Char('n')), &mut state);
    assert!(new_event.is_some());

    let attach_event =
        EventHandler::handle_key_event(create_key_event(KeyCode::Char('a')), &mut state);
    assert!(attach_event.is_some());

    let start_stop_event =
        EventHandler::handle_key_event(create_key_event(KeyCode::Char('s')), &mut state);
    assert!(start_stop_event.is_some());

    let delete_event =
        EventHandler::handle_key_event(create_key_event(KeyCode::Char('d')), &mut state);
    assert!(delete_event.is_some());
}

#[test]
fn test_help_key_event() {
    let mut state = AppState::default();

    let help_event =
        EventHandler::handle_key_event(create_key_event(KeyCode::Char('?')), &mut state);
    assert!(help_event.is_some());
}

#[test]
fn test_help_visible_only_responds_to_help_and_esc() {
    let mut state = AppState::default();
    state.help_visible = true;

    let help_event =
        EventHandler::handle_key_event(create_key_event(KeyCode::Char('?')), &mut state);
    assert!(help_event.is_some());

    let esc_event = EventHandler::handle_key_event(create_key_event(KeyCode::Esc), &mut state);
    assert!(esc_event.is_some());

    let other_event =
        EventHandler::handle_key_event(create_key_event(KeyCode::Char('j')), &mut state);
    assert!(other_event.is_none());
}

#[test]
fn test_go_to_top_bottom() {
    use ainb::app::screens::ids as screen_ids;
    let mut state = AppState::default();
    state.current_screen = screen_ids::SESSION_LIST.to_string();

    let go_top = EventHandler::handle_key_event(create_key_event(KeyCode::Home), &mut state);
    assert!(go_top.is_some());

    let go_bottom = EventHandler::handle_key_event(create_key_event(KeyCode::End), &mut state);
    assert!(go_bottom.is_some());
}

#[test]
fn test_unknown_key_returns_none() {
    let mut state = AppState::default();

    // Test with a truly unmapped key like 'z'
    let unknown_event =
        EventHandler::handle_key_event(create_key_event(KeyCode::Char('z')), &mut state);
    assert!(unknown_event.is_none());

    let unknown_f_key = EventHandler::handle_key_event(create_key_event(KeyCode::F(1)), &mut state);
    assert!(unknown_f_key.is_none());
}

#[test]
fn test_process_quit_event() {
    let mut state = AppState::default();

    assert!(!state.should_quit);

    if let Some(event) =
        EventHandler::handle_key_event(create_key_event(KeyCode::Char('q')), &mut state)
    {
        EventHandler::process_event(event, &mut state);
    }

    assert!(state.should_quit);
}

#[test]
fn test_process_help_toggle_event() {
    let mut state = AppState::default();

    assert!(!state.help_visible);

    if let Some(event) =
        EventHandler::handle_key_event(create_key_event(KeyCode::Char('?')), &mut state)
    {
        EventHandler::process_event(event, &mut state);
    }

    assert!(state.help_visible);
}

// test_usage_period_and_provider_events removed — the burndown plugin
// owns Analytics-screen key handling now. The host-side AppEvent::Usage*
// variants and state.usage_state were both removed in Phase 3 cutover.
// Equivalent coverage (period switch + provider cycle) lives in the
// plugin's own test suite.
