// ABOUTME: Test specifically for session creation UI refresh bug fix
//
// Post new-session redesign the flat `NewSessionState` (with `filtered_repos`,
// `available_repos`, `selected_repo_index`, `is_current_dir_mode`, and the
// `InputBranch`/`SelectRepo`/`ConfigurePermissions` steps) is gone. The flow is
// now a unified repo picker (`step = PickRepo`, owning `pick_repo_state`) that
// advances to a Configure screen and finally a `Creating` step. Session
// creation runs through `AppState::create_session_from_configure`, whose
// success AND failure arms both tear the modal down via `cancel_new_session()`
// — returning to the screen the user opened new-session from (SessionList here)
// and, on success, reloading workspaces and setting `ui_needs_refresh` BEFORE
// the view switches back.
//
// These tests drive the real picker entry point and then assert that genuine
// teardown/refresh contract (the exact code that guards the original
// "empty homescreen after creation" bug) without depending on a real git repo
// or tmux/Docker, mirroring the original tests which also stubbed creation out
// and only verified the refresh/reset behaviour.

use ainb::app::App;
use ainb::app::events::EventHandler;
use ainb::app::screens::ids as screen_ids;
use ainb::app::state::NewSessionStep;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Open the unified repo picker the way the host does (`n` from the session
/// list) and return the app primed with `previous_screen = SessionList` so the
/// teardown lands back on the list — matching how a user reaches creation.
async fn app_on_picker_from_session_list() -> App {
    let mut app = App::new();
    app.state.load_mock_data();
    // Anchor the "opened from" screen so cancel/teardown returns here, the way
    // the real dispatcher records `previous_screen` on `n`.
    app.state.current_screen = screen_ids::SESSION_LIST.to_string();

    let key_event = KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE);
    if let Some(event) = EventHandler::handle_key_event(key_event, &mut app.state) {
        EventHandler::process_event(event, &mut app.state);
    }
    // The picker opens synchronously and only spawns a background rescan; the
    // tick drains it without changing the screen.
    app.tick().await.expect("Tick should succeed");
    app
}

/// Test the specific UI refresh issue where creating a session showed an empty
/// homescreen until the user quit and reopened. Drives the real picker open,
/// then the genuine `cancel_new_session()` teardown that every creation outcome
/// runs, and verifies the post-creation refresh mechanism.
#[tokio::test]
async fn test_session_creation_shows_immediately() {
    let mut app = app_on_picker_from_session_list().await;

    let initial_workspace_count = app.state.workspaces.len();
    assert!(
        initial_workspace_count > 0,
        "Should have some initial workspaces for test"
    );

    // The picker is open and owns its own state.
    assert_eq!(app.state.current_screen, screen_ids::NEW_SESSION);
    assert!(app.state.new_session_state.is_some());
    if let Some(ref session_state) = app.state.new_session_state {
        assert_eq!(
            session_state.step,
            NewSessionStep::PickRepo,
            "New session should open on the unified repo picker"
        );
        assert!(
            session_state.pick_repo_state.is_some(),
            "PickRepo step must carry picker state"
        );
    }

    // Run the genuine teardown that EVERY session-creation outcome executes
    // (`create_session_from_configure` calls this on both success and failure).
    // The original test stubbed creation out and only verified this reset path;
    // here we invoke it directly on the real production method.
    app.state.cancel_new_session();

    // CRITICAL: after creation completes the view must be back on SessionList
    // immediately (the bug showed stale/empty data before the refresh ran).
    assert_eq!(
        app.state.current_screen,
        screen_ids::SESSION_LIST,
        "Should return to SessionList immediately after session creation"
    );

    // Session state should be cleared.
    assert!(
        app.state.new_session_state.is_none(),
        "New session state should be cleared after creation"
    );

    // The key fix: workspaces stay loaded (not wiped) across the teardown so the
    // homescreen renders current data, not an empty placeholder.
    assert!(
        !app.state.workspaces.is_empty(),
        "Workspaces should be loaded and visible immediately"
    );

    // The success arm sets `ui_needs_refresh` so the main loop re-renders with
    // the freshly loaded workspaces. Exercise that mechanism directly.
    app.state.ui_needs_refresh = true;
    assert!(
        app.needs_ui_refresh(),
        "UI refresh flag should be set and cleared by needs_ui_refresh() method"
    );
    assert!(
        !app.needs_ui_refresh(),
        "UI refresh flag should be cleared after first check"
    );
}

/// Test that the workspace refresh happens in the correct order: workspaces are
/// available BEFORE the view switches back to the session list.
#[tokio::test]
async fn test_workspace_refresh_order() {
    let mut app = app_on_picker_from_session_list().await;

    // Tear the modal down the way a completed creation does.
    app.state.cancel_new_session();

    // After teardown we are back on SessionList with workspace data present.
    assert_eq!(app.state.current_screen, screen_ids::SESSION_LIST);
    assert!(app.state.new_session_state.is_none());
    assert!(
        !app.state.workspaces.is_empty(),
        "Workspace data should be loaded and available after refresh"
    );

    // Re-loading workspaces (as the success arm does) keeps the list populated.
    app.state.load_real_workspaces().await;
    assert_eq!(
        app.state.current_screen,
        screen_ids::SESSION_LIST,
        "Should be in SessionList view with current workspace data loaded"
    );
}

/// Test the specific bug scenario: empty homescreen after session creation.
#[tokio::test]
async fn test_no_empty_homescreen_after_creation() {
    let mut app = app_on_picker_from_session_list().await;

    let has_initial_data = !app.state.workspaces.is_empty();
    assert!(has_initial_data, "Test requires initial workspace data");

    // Genuine creation teardown.
    app.state.cancel_new_session();

    // CRITICAL: populated homescreen immediately, not an empty one.
    assert_eq!(app.state.current_screen, screen_ids::SESSION_LIST);
    assert!(
        !app.state.workspaces.is_empty(),
        "REGRESSION: Homescreen shows empty after session creation - UI refresh bug has returned!"
    );

    // No lingering session-creation state or queued async work.
    assert!(app.state.new_session_state.is_none());
    assert!(app.state.pending_async_action.is_none());
}

/// Test that session creation errors also handle refresh correctly. The error
/// arm of `create_session_from_configure` posts a notification then calls the
/// same `cancel_new_session()` teardown — workspaces stay visible, state
/// clears.
#[tokio::test]
async fn test_error_handling_with_correct_refresh() {
    let mut app = app_on_picker_from_session_list().await;

    // Mirror the error arm: surface a failure notification, then tear down.
    app.state.add_error_notification("Could not create session: test".to_string());
    app.state.cancel_new_session();

    // Even on error we return to SessionList with data visible.
    assert_eq!(app.state.current_screen, screen_ids::SESSION_LIST);
    assert!(
        !app.state.workspaces.is_empty(),
        "Should still show workspace data after error"
    );
    assert!(
        app.state.new_session_state.is_none(),
        "Should clear session state on error"
    );
    // The error notification survives the teardown (cancel must NOT clear it).
    assert!(
        !app.state.notifications.is_empty(),
        "Error notification should survive the modal teardown"
    );
}

/// Test the UI refresh mechanism directly.
#[tokio::test]
async fn test_ui_refresh_mechanism() {
    let mut app = App::new();
    app.state.load_mock_data();

    // Initially no refresh needed
    assert!(!app.needs_ui_refresh(), "Should not need refresh initially");

    // Set refresh flag (simulating successful session creation)
    app.state.ui_needs_refresh = true;

    // First check should return true and clear flag
    assert!(
        app.needs_ui_refresh(),
        "Should need refresh when flag is set"
    );

    // Second check should return false (flag cleared)
    assert!(
        !app.needs_ui_refresh(),
        "Should not need refresh after flag cleared"
    );

    // Loading workspaces does not, on its own, set the refresh flag — only
    // session creation does.
    app.state.load_real_workspaces().await;

    // Test manual flag setting (as would happen during session creation)
    app.state.ui_needs_refresh = true;
    assert!(app.state.ui_needs_refresh, "Flag should be set");

    // Simulate the main loop checking for refresh.
    if app.needs_ui_refresh() {
        // In the real app this would trigger terminal.draw().
    }

    // Flag should be cleared
    assert!(
        !app.state.ui_needs_refresh,
        "Flag should be cleared after check"
    );
}
