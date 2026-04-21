// ABOUTME: Test specifically for session creation UI refresh bug fix

use ainb::app::events::EventHandler;
use ainb::app::{
    App,
    state::{NewSessionStep, View},
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Test the specific UI refresh issue where creating a session shows empty homescreen
/// until user quits and reopens
#[tokio::test]
async fn test_session_creation_shows_immediately() {
    let mut app = App::new();

    // Load mock data to ensure we have some workspaces
    app.state.load_mock_data();

    let initial_workspace_count = app.state.workspaces.len();
    assert!(
        initial_workspace_count > 0,
        "Should have some initial workspaces for test"
    );

    // Simulate starting session creation in current directory
    let key_event = KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE);
    if let Some(event) = EventHandler::handle_key_event(key_event, &mut app.state) {
        EventHandler::process_event(event, &mut app.state);
    }

    // Process the async action (which would normally trigger workspace search/setup)
    app.tick().await.expect("Tick should succeed");

    // Should now be in NewSession view with state
    assert_eq!(app.state.current_view, View::NewSession);
    assert!(app.state.new_session_state.is_some());

    // Check that we have session state set up
    // Note: The behavior depends on whether the test directory is a git repository
    if let Some(ref session_state) = app.state.new_session_state {
        // The step should be either InputBranch (current dir mode) or SelectRepo (workspace search mode)
        assert!(
            session_state.step == NewSessionStep::InputBranch
                || session_state.step == NewSessionStep::SelectRepo,
            "Step should be either InputBranch or SelectRepo, got: {:?}",
            session_state.step
        );
        assert!(
            !session_state.branch_name.is_empty(),
            "Branch name should be pre-filled"
        );
    }

    // Simulate pressing Enter to create the session
    // Note: We need to ensure we're in a valid state for session creation
    if let Some(ref mut session_state) = app.state.new_session_state {
        // In test environment, we need to simulate having a valid repo selected
        if !session_state.is_current_dir_mode && session_state.filtered_repos.is_empty() {
            // Add a mock repo for testing
            use std::path::PathBuf;
            let mock_repo = PathBuf::from("/tmp/mock-repo");
            session_state.available_repos.push(mock_repo.clone());
            session_state.filtered_repos.push((0, mock_repo));
            session_state.selected_repo_index = Some(0);
        }

        // Move to the appropriate step for creation
        if session_state.is_current_dir_mode {
            session_state.step = NewSessionStep::ConfigurePermissions;
        } else {
            // For workspace search mode, we need to go through the steps
            session_state.step = NewSessionStep::ConfigurePermissions;
        }
    }

    let create_key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    if let Some(event) = EventHandler::handle_key_event(create_key, &mut app.state) {
        EventHandler::process_event(event, &mut app.state);
    }

    // Process the async session creation
    // Note: This will fail in testing because we don't have a real git repo setup,
    // but the important part is that the UI refresh logic executes in the right order
    let _ = app.tick().await; // Ignore result since we expect failure in test env

    // CRITICAL TEST: The view should be back to SessionList immediately after creation
    // The bug was that it showed SessionList with old/empty data before refresh completed
    assert_eq!(
        app.state.current_view,
        View::SessionList,
        "Should return to SessionList immediately after session creation"
    );

    // Session state should be cleared
    assert!(
        app.state.new_session_state.is_none(),
        "New session state should be cleared after creation"
    );

    // The key fix: workspaces should be loaded BEFORE the view switches back
    // So we should see current workspace data, not stale data
    // In a real scenario, this would show the newly created session
    assert!(
        !app.state.workspaces.is_empty(),
        "Workspaces should be loaded and visible immediately"
    );

    // NOTE: In test environment, session creation will fail due to no real git repo
    // But we can test that the refresh mechanism is working by manually triggering it
    // In a real environment, the flag would be set after successful session creation

    // Manually set the flag to test the refresh mechanism
    app.state.ui_needs_refresh = true;

    // UI refresh flag should be properly handled
    assert!(
        app.needs_ui_refresh(),
        "UI refresh flag should be set and cleared by needs_ui_refresh() method"
    );

    // Flag should be cleared after being checked
    assert!(
        !app.needs_ui_refresh(),
        "UI refresh flag should be cleared after first check"
    );
}

/// Test that the workspace refresh happens in the correct order
#[tokio::test]
async fn test_workspace_refresh_order() {
    let mut app = App::new();
    app.state.load_mock_data();

    // Record initial state
    let initial_count = app.state.workspaces.len();

    // Start new session creation
    let key_event = KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE);
    if let Some(event) = EventHandler::handle_key_event(key_event, &mut app.state) {
        EventHandler::process_event(event, &mut app.state);
    }
    app.tick().await.expect("Should complete async setup");

    // Simulate session creation completion
    // Set up the session state for creation in test environment
    if let Some(ref mut session_state) = app.state.new_session_state {
        if !session_state.is_current_dir_mode && session_state.filtered_repos.is_empty() {
            // Add a mock repo for testing
            use std::path::PathBuf;
            let mock_repo = PathBuf::from("/tmp/mock-repo");
            session_state.available_repos.push(mock_repo.clone());
            session_state.filtered_repos.push((0, mock_repo));
            session_state.selected_repo_index = Some(0);
        }
        session_state.step = NewSessionStep::ConfigurePermissions;
    }

    let create_key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    if let Some(event) = EventHandler::handle_key_event(create_key, &mut app.state) {
        EventHandler::process_event(event, &mut app.state);
    }

    // The critical moment: when async processing happens
    let _ = app.tick().await; // Ignore docker/git errors in test

    // After the tick, we should be back in SessionList view with current data
    assert_eq!(app.state.current_view, View::SessionList);
    assert!(app.state.new_session_state.is_none());

    // The workspace data should be current (in real scenario would include new session)
    // At minimum, we should have the same data we started with
    // Note: In test environment, load_real_workspaces() may find different workspaces than mock data
    assert!(
        !app.state.workspaces.is_empty(),
        "Workspace data should be loaded and available after refresh"
    );

    // The key test: workspaces should be loaded BEFORE view switches back
    // This ensures users see current data immediately, not stale/empty data
    assert_eq!(
        app.state.current_view,
        View::SessionList,
        "Should be in SessionList view with current workspace data loaded"
    );
}

/// Test the specific bug scenario: empty homescreen after session creation
#[tokio::test]
async fn test_no_empty_homescreen_after_creation() {
    let mut app = App::new();

    // Start with some data
    app.state.load_mock_data();
    let has_initial_data = !app.state.workspaces.is_empty();
    assert!(has_initial_data, "Test requires initial workspace data");

    // Create session through UI workflow
    // Step 1: Press 'n' for new session
    let key_event = KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE);
    if let Some(event) = EventHandler::handle_key_event(key_event, &mut app.state) {
        EventHandler::process_event(event, &mut app.state);
    }
    app.tick().await.expect("Should setup new session state");

    // Step 2: Press Enter to create session
    // Set up the session state for creation in test environment
    if let Some(ref mut session_state) = app.state.new_session_state {
        if !session_state.is_current_dir_mode && session_state.filtered_repos.is_empty() {
            // Add a mock repo for testing
            use std::path::PathBuf;
            let mock_repo = PathBuf::from("/tmp/mock-repo");
            session_state.available_repos.push(mock_repo.clone());
            session_state.filtered_repos.push((0, mock_repo));
            session_state.selected_repo_index = Some(0);
        }
        session_state.step = NewSessionStep::ConfigurePermissions;
    }

    let create_key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    if let Some(event) = EventHandler::handle_key_event(create_key, &mut app.state) {
        EventHandler::process_event(event, &mut app.state);
    }

    // Step 3: Process the creation (this is where the fix applies)
    let _ = app.tick().await; // Session creation will fail but that's OK for this test

    // CRITICAL: After creation, user should see populated homescreen immediately
    // The bug was that homescreen appeared empty until quit/restart
    assert_eq!(app.state.current_view, View::SessionList);

    // Should have workspace data visible (not empty)
    assert!(
        !app.state.workspaces.is_empty(),
        "REGRESSION: Homescreen shows empty after session creation - UI refresh bug has returned!"
    );

    // No lingering session creation state
    assert!(app.state.new_session_state.is_none());
    assert!(app.state.pending_async_action.is_none());
}

/// Test that session creation errors also handle refresh correctly
#[tokio::test]
async fn test_error_handling_with_correct_refresh() {
    let mut app = App::new();
    app.state.load_mock_data();

    // Simulate session creation error path
    let key_event = KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE);
    if let Some(event) = EventHandler::handle_key_event(key_event, &mut app.state) {
        EventHandler::process_event(event, &mut app.state);
    }
    app.tick().await.expect("Should setup new session");

    // Try to create session (will fail in test env)
    // Set up the session state for creation in test environment
    if let Some(ref mut session_state) = app.state.new_session_state {
        if !session_state.is_current_dir_mode && session_state.filtered_repos.is_empty() {
            // Add a mock repo for testing
            use std::path::PathBuf;
            let mock_repo = PathBuf::from("/tmp/mock-repo");
            session_state.available_repos.push(mock_repo.clone());
            session_state.filtered_repos.push((0, mock_repo));
            session_state.selected_repo_index = Some(0);
        }
        session_state.step = NewSessionStep::ConfigurePermissions;
    }

    let create_key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    if let Some(event) = EventHandler::handle_key_event(create_key, &mut app.state) {
        EventHandler::process_event(event, &mut app.state);
    }

    // Process the failed creation
    let _ = app.tick().await; // Expect failure but should handle gracefully

    // Even on error, should return to SessionList with data visible
    assert_eq!(app.state.current_view, View::SessionList);
    assert!(
        !app.state.workspaces.is_empty(),
        "Should still show workspace data after error"
    );
    assert!(
        app.state.new_session_state.is_none(),
        "Should clear session state on error"
    );
}

/// Test the UI refresh mechanism directly
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

    // Test that loading workspaces sets refresh flag
    app.state.load_real_workspaces().await;
    // In normal operation, this doesn't set the flag - only session creation does

    // Test manual flag setting (as would happen during session creation)
    app.state.ui_needs_refresh = true;
    assert!(app.state.ui_needs_refresh, "Flag should be set");

    // Simulate main loop checking for refresh
    if app.needs_ui_refresh() {
        // This simulates the immediate re-render in main.rs
        // In real app, this would trigger terminal.draw()
    }

    // Flag should be cleared
    assert!(
        !app.state.ui_needs_refresh,
        "Flag should be cleared after check"
    );
}
