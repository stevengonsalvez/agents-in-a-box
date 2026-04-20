// ABOUTME: Tests for tmux preview pane behavior — content assignment and refresh throttling

use agents_box::app::AppState;
use agents_box::models::Session;

#[test]
fn test_session_preview_content_starts_empty() {
    let session = Session::new("test".to_string(), "/workspace".to_string());
    assert!(session.preview_content.is_none());
}

#[test]
fn test_session_set_preview_stores_content() {
    let mut session = Session::new("test".to_string(), "/workspace".to_string());
    let original_access = session.last_accessed;

    std::thread::sleep(std::time::Duration::from_millis(1));
    session.set_preview("line1\nline2\n".to_string());

    assert_eq!(session.preview_content.as_deref(), Some("line1\nline2\n"));
    // set_preview should also bump last_accessed
    assert!(session.last_accessed > original_access);
}

#[test]
fn test_session_set_preview_overwrites() {
    let mut session = Session::new("test".to_string(), "/workspace".to_string());
    session.set_preview("first".to_string());
    session.set_preview("second".to_string());
    assert_eq!(session.preview_content.as_deref(), Some("second"));
}

#[tokio::test]
async fn test_preview_throttle_gates_rapid_calls() {
    // With no tmux sessions registered, update_tmux_previews should:
    //   1) set last_preview_update on first call
    //   2) early-return (throttled) on an immediate second call without advancing the timestamp
    let mut state = AppState::default();
    assert!(state.last_preview_update.is_none());

    state
        .update_tmux_previews()
        .await
        .expect("first update should succeed");
    let first_ts = state.last_preview_update;
    assert!(first_ts.is_some(), "first call must set timestamp");

    // Immediate re-call: well within the 5s throttle window
    state
        .update_tmux_previews()
        .await
        .expect("throttled update should also succeed");

    assert_eq!(
        state.last_preview_update, first_ts,
        "second call within throttle window must NOT advance last_preview_update",
    );
}
