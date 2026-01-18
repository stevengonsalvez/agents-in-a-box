// ABOUTME: Unit tests for event handling to ensure keyboard inputs map to correct app actions

use ainb::app::{AppState, EventHandler};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

const fn create_key_event(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

const fn create_key_event_with_modifiers(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
    KeyEvent::new(code, modifiers)
}

#[test]
fn test_quit_key_events() {
    let mut state = AppState::default();

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
    let mut state = AppState::default();

    let down_event =
        EventHandler::handle_key_event(create_key_event(KeyCode::Char('j')), &mut state);
    assert!(down_event.is_some());

    let up_event = EventHandler::handle_key_event(create_key_event(KeyCode::Char('k')), &mut state);
    assert!(up_event.is_some());

    let left_event =
        EventHandler::handle_key_event(create_key_event(KeyCode::Char('h')), &mut state);
    assert!(left_event.is_some());

    let right_event =
        EventHandler::handle_key_event(create_key_event(KeyCode::Char('l')), &mut state);
    assert!(right_event.is_some());
}

#[tokio::test]
async fn test_n_key_triggers_new_session() {
    use ainb::app::state::{AsyncAction, View};

    let mut state = AppState::default();

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

    // Should be NewSessionNormal
    if state.pending_async_action == Some(AsyncAction::NewSessionNormal) {
        // Test passed
    } else {
        panic!(
            "Expected AsyncAction::NewSessionNormal, got: {:?}",
            state.pending_async_action
        );
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
        state.current_view == View::NewSession
            || state.current_view == View::SearchWorkspace
            || state.current_view == View::SessionList
            || state.current_view == View::AuthSetup,
        "Unexpected view: {:?}",
        state.current_view
    );
    // The new session state might not be set if auth setup is required
    assert!(state.pending_async_action.is_none());
}

#[test]
fn test_arrow_key_navigation() {
    let mut state = AppState::default();

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
    let mut state = AppState::default();

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
    let mut state = AppState::default();

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
