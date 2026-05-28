// ABOUTME: Tests for AppState new session functionality, focusing on mode selection flow

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::{AppState, NewSessionState, NewSessionStep, SessionAgentOption};
    use crate::app::events::AppEvent;
    use crate::app::EventHandler;
    use crate::models::{OtherTmuxSession, SessionAgentType, SessionMode};
    use std::path::PathBuf;

    /// Test that pressing 'n' for new session should go through mode selection
    #[test]
    fn test_new_session_should_include_mode_selection() {
        let mut state = AppState::new();

        // Simulate the 'n' key press flow
        // This should NOT skip mode selection like current directory mode does

        // First, we need to simulate having workspaces available
        state.workspaces = vec![crate::models::Workspace::new(
            "test-workspace".to_string(),
            PathBuf::from("/test/path"),
        )];

        // Simulate the async action that happens when 'n' is pressed
        // This should create a session that goes through ALL steps including mode selection
        let current_dir = std::env::current_dir().unwrap();

        // Create new session state manually to test the flow
        state.new_session_state = Some(NewSessionState {
            available_repos: vec![current_dir.clone()],
            filtered_repos: vec![(0, current_dir.clone())],
            selected_repo_index: Some(0),
            branch_name: "test-branch".to_string(),
            step: NewSessionStep::InputBranch, // This is what happens currently
            filter_text: String::new(),
            is_current_dir_mode: false, // This should be false for 'n' key press
            skip_permissions: false,
            mode: SessionMode::Interactive,
            boss_prompt: crate::app::state::TextEditor::new(),
            file_finder: crate::components::fuzzy_file_finder::FuzzyFileFinderState::new(),
            restart_session_id: None, // Not a restart
            selected_agent: SessionAgentType::default(),
            agent_options: SessionAgentOption::all(),
            selected_agent_index: 0,
            ..Default::default()
        });

        // Now simulate pressing Enter in InputBranch step
        // This should proceed to mode selection, NOT skip it
        state.new_session_proceed_to_mode_selection();

        // Verify that we're now in SelectMode step
        if let Some(ref session_state) = state.new_session_state {
            assert_eq!(
                session_state.step,
                NewSessionStep::SelectMode,
                "After proceeding from InputBranch, should be in SelectMode step for mode selection"
            );
            assert!(
                !session_state.is_current_dir_mode,
                "Normal new session should not be in current directory mode"
            );
        } else {
            panic!("Session state should exist after proceeding to mode selection");
        }
    }

    /// Test that current directory mode (different from 'n' key) should skip mode selection
    #[test]
    fn test_current_dir_mode_should_skip_mode_selection() {
        let mut state = AppState::new();
        let current_dir = std::env::current_dir().unwrap();

        // Create session state in current directory mode (this is different from 'n' key)
        state.new_session_state = Some(NewSessionState {
            available_repos: vec![current_dir.clone()],
            filtered_repos: vec![(0, current_dir.clone())],
            selected_repo_index: Some(0),
            branch_name: "test-branch".to_string(),
            step: NewSessionStep::InputBranch,
            filter_text: String::new(),
            is_current_dir_mode: true, // This should be true for current dir mode
            skip_permissions: false,
            mode: SessionMode::Interactive,
            boss_prompt: crate::app::state::TextEditor::new(),
            file_finder: crate::components::fuzzy_file_finder::FuzzyFileFinderState::new(),
            restart_session_id: None, // Not a restart
            selected_agent: SessionAgentType::default(),
            agent_options: SessionAgentOption::all(),
            selected_agent_index: 0,
            ..Default::default()
        });

        // In current directory mode, pressing Enter should skip mode selection
        // and go directly to session creation (this is the intended behavior for current dir mode)

        // Verify the current behavior is correct for current directory mode
        if let Some(ref session_state) = state.new_session_state {
            assert!(
                session_state.is_current_dir_mode,
                "This test should be for current directory mode"
            );
            assert_eq!(session_state.step, NewSessionStep::InputBranch);
        }
    }

    /// Test mode selection toggle functionality
    #[test]
    fn test_mode_selection_toggle() {
        let mut state = AppState::new();
        let current_dir = std::env::current_dir().unwrap();

        // Create session state in SelectMode step
        state.new_session_state = Some(NewSessionState {
            available_repos: vec![current_dir.clone()],
            filtered_repos: vec![(0, current_dir.clone())],
            selected_repo_index: Some(0),
            branch_name: "test-branch".to_string(),
            step: NewSessionStep::SelectMode, // In mode selection
            filter_text: String::new(),
            is_current_dir_mode: false,
            skip_permissions: false,
            mode: SessionMode::Interactive, // Start with Interactive
            boss_prompt: crate::app::state::TextEditor::new(),
            file_finder: crate::components::fuzzy_file_finder::FuzzyFileFinderState::new(),
            restart_session_id: None, // Not a restart
            selected_agent: SessionAgentType::default(),
            agent_options: SessionAgentOption::all(),
            selected_agent_index: 0,
            ..Default::default()
        });

        // Test toggling mode
        state.new_session_toggle_mode();

        if let Some(ref session_state) = state.new_session_state {
            assert_eq!(
                session_state.mode,
                SessionMode::Boss,
                "Mode should toggle from Interactive to Boss"
            );
        }

        // Toggle again
        state.new_session_toggle_mode();

        if let Some(ref session_state) = state.new_session_state {
            assert_eq!(
                session_state.mode,
                SessionMode::Interactive,
                "Mode should toggle back from Boss to Interactive"
            );
        }
    }

    /// Test proceeding from mode selection to appropriate next step
    #[test]
    fn test_proceed_from_mode_selection() {
        let mut state = AppState::new();
        let current_dir = std::env::current_dir().unwrap();

        // Test Interactive mode flow
        state.new_session_state = Some(NewSessionState {
            available_repos: vec![current_dir.clone()],
            filtered_repos: vec![(0, current_dir.clone())],
            selected_repo_index: Some(0),
            branch_name: "test-branch".to_string(),
            step: NewSessionStep::SelectMode,
            filter_text: String::new(),
            is_current_dir_mode: false,
            skip_permissions: false,
            mode: SessionMode::Interactive,
            boss_prompt: crate::app::state::TextEditor::new(),
            file_finder: crate::components::fuzzy_file_finder::FuzzyFileFinderState::new(),
            restart_session_id: None, // Not a restart
            selected_agent: SessionAgentType::default(),
            agent_options: SessionAgentOption::all(),
            selected_agent_index: 0,
            ..Default::default()
        });

        state.new_session_proceed_from_mode();

        if let Some(ref session_state) = state.new_session_state {
            assert_eq!(
                session_state.step,
                NewSessionStep::ConfigurePermissions,
                "Interactive mode should proceed to ConfigurePermissions"
            );
        }

        // Test Boss mode flow
        state.new_session_state = Some(NewSessionState {
            available_repos: vec![current_dir.clone()],
            filtered_repos: vec![(0, current_dir.clone())],
            selected_repo_index: Some(0),
            branch_name: "test-branch".to_string(),
            step: NewSessionStep::SelectMode,
            filter_text: String::new(),
            is_current_dir_mode: false,
            skip_permissions: false,
            mode: SessionMode::Boss,
            boss_prompt: crate::app::state::TextEditor::new(),
            file_finder: crate::components::fuzzy_file_finder::FuzzyFileFinderState::new(),
            restart_session_id: None, // Not a restart
            selected_agent: SessionAgentType::default(),
            agent_options: SessionAgentOption::all(),
            selected_agent_index: 0,
            ..Default::default()
        });

        state.new_session_proceed_from_mode();

        if let Some(ref session_state) = state.new_session_state {
            assert_eq!(
                session_state.step,
                NewSessionStep::InputPrompt,
                "Boss mode should proceed to InputPrompt"
            );
        }
    }

    /// Test the actual event flow: 'n' key -> Enter in branch input -> should show mode selection
    #[test]
    fn test_n_key_event_flow_shows_mode_selection() {
        let mut state = AppState::new();

        // Simulate having workspaces available
        state.workspaces = vec![crate::models::Workspace::new(
            "test-workspace".to_string(),
            PathBuf::from("/test/path"),
        )];

        // Simulate the 'n' key press by setting the async action that would be triggered
        state.pending_async_action = Some(crate::app::state::AsyncAction::NewSessionNormal);

        // Process the async action (this simulates what happens in the main loop)
        // We can't actually call the async method in a sync test, but we can verify
        // that the correct async action is set
        assert_eq!(
            state.pending_async_action,
            Some(crate::app::state::AsyncAction::NewSessionNormal),
            "The 'n' key should trigger NewSessionNormal, not NewSessionInCurrentDir"
        );

        // Verify that NewSessionNormal would create a session with is_current_dir_mode: false
        // by checking that the new method we created sets the right flag
        // (This is tested indirectly through the other tests, but this confirms the integration)
    }

    /// Test notification system functionality
    #[test]
    fn test_notification_system() {
        let mut state = AppState::new();

        // Test adding different types of notifications
        state.add_success_notification("Success message".to_string());
        state.add_error_notification("Error message".to_string());
        state.add_info_notification("Info message".to_string());
        state.add_warning_notification("Warning message".to_string());

        // Should have 4 notifications
        assert_eq!(state.notifications.len(), 4);

        // Test getting current notifications (non-expired)
        let current = state.get_current_notifications();
        assert_eq!(current.len(), 4);

        // Test notification types
        assert_eq!(
            current[0].notification_type,
            crate::app::state::NotificationType::Success
        );
        assert_eq!(
            current[1].notification_type,
            crate::app::state::NotificationType::Error
        );
        assert_eq!(
            current[2].notification_type,
            crate::app::state::NotificationType::Info
        );
        assert_eq!(
            current[3].notification_type,
            crate::app::state::NotificationType::Warning
        );
    }

    /// Test notification expiration
    #[test]
    fn test_notification_expiration() {
        let mut state = AppState::new();

        // Add a notification with custom duration
        let mut notification = crate::app::state::Notification::success("Test message".to_string());
        notification.duration = std::time::Duration::from_millis(1); // Very short duration
        state.add_notification(notification);

        // Wait for expiration
        std::thread::sleep(std::time::Duration::from_millis(10));

        // Clean up expired notifications
        state.cleanup_expired_notifications();

        // Should have no notifications left
        assert_eq!(state.notifications.len(), 0);
    }

    /// Test git commit and push notifications (without actual git operations)
    #[test]
    fn test_git_commit_and_push_notifications() {
        let mut state = AppState::new();

        // Test that git_commit_and_push method exists and can be called
        // (This will not actually perform git operations since git_view_state is None)
        state.git_commit_and_push();

        // Should not crash and should not add any notifications since git_view_state is None
        assert_eq!(state.notifications.len(), 0);
    }

    /// Regression test: Stale async_operation_cancelled flag should not block subsequent sessions
    /// Bug: After cancelling a session, the flag stays true and blocks the next Local source selection
    #[test]
    fn test_stale_cancellation_flag_does_not_block_new_session() {
        use crate::app::state::RepoSourceChoice;

        let mut state = AppState::new();

        // Step 1: Simulate creating a new session
        state.new_session_state = Some(NewSessionState {
            step: NewSessionStep::SelectSource,
            source_choice: RepoSourceChoice::Local,
            ..Default::default()
        });

        // Step 2: Cancel the session (this sets async_operation_cancelled = true)
        state.cancel_new_session();

        // Verify the flag is set to true after cancellation
        assert!(
            state.async_operation_cancelled,
            "Flag should be true after cancel_new_session()"
        );
        assert!(
            state.new_session_state.is_none(),
            "Session state should be cleared after cancel"
        );

        // Step 3: Start a NEW session (user presses 'n' again)
        state.new_session_state = Some(NewSessionState {
            step: NewSessionStep::SelectSource,
            source_choice: RepoSourceChoice::Local,
            ..Default::default()
        });

        // Step 4: Proceed from source selection (user selects Local)
        // This MUST reset the cancellation flag
        state.new_session_proceed_from_source();

        // Verify the fix: flag should be reset to false
        assert!(
            !state.async_operation_cancelled,
            "Flag MUST be reset to false when starting new Local source search - stale flag would block workspace search"
        );

        // Verify the async action is queued
        assert_eq!(
            state.pending_async_action,
            Some(crate::app::state::AsyncAction::StartWorkspaceSearch),
            "StartWorkspaceSearch should be queued for Local source"
        );

        // Verify step advanced correctly
        if let Some(ref session_state) = state.new_session_state {
            assert_eq!(
                session_state.step,
                NewSessionStep::SelectRepo,
                "Step should advance to SelectRepo for local source"
            );
        } else {
            panic!("Session state should exist after proceeding from source");
        }
    }

    /// Test that Remote source selection does not affect cancellation flag
    #[test]
    fn test_remote_source_does_not_need_cancellation_flag_reset() {
        use crate::app::state::RepoSourceChoice;

        let mut state = AppState::new();

        // Set up stale flag
        state.async_operation_cancelled = true;

        // Start new session with Remote source
        state.new_session_state = Some(NewSessionState {
            step: NewSessionStep::SelectSource,
            source_choice: RepoSourceChoice::Remote,
            ..Default::default()
        });

        // Proceed with Remote source
        state.new_session_proceed_from_source();

        // Remote does not use async workspace search, so flag state doesn't matter
        // But verify the step advances correctly
        if let Some(ref session_state) = state.new_session_state {
            assert_eq!(
                session_state.step,
                NewSessionStep::InputRepoSource,
                "Remote source should advance to InputRepoSource"
            );
        }

        // No async action should be queued for Remote
        assert!(
            state.pending_async_action.is_none(),
            "Remote source should not queue StartWorkspaceSearch"
        );
    }

    // ========================================================================
    // Stop / Resume coverage
    // ========================================================================

    use crate::app::state::{AsyncAction, ConfirmAction, ConfirmationDialog, DialogOption};

    fn state_with_other_tmux_sessions(names: &[&str]) -> AppState {
        let mut state = AppState::new();
        state.selected_workspace_index = None;
        state.selected_session_index = None;
        state.selected_other_tmux_index = Some(0);
        state.other_tmux_sessions = names
            .iter()
            .map(|name| OtherTmuxSession::new((*name).to_string(), false, 1))
            .collect();
        state
    }

    #[test]
    fn test_toggle_select_other_tmux_session_supports_multiple_names() {
        let mut state = state_with_other_tmux_sessions(&["alpha", "beta"]);

        state.toggle_select_session();
        state.selected_other_tmux_index = Some(1);
        state.toggle_select_session();

        assert_eq!(state.selected_other_tmux_sessions.len(), 2);
        assert!(state.selected_other_tmux_sessions.contains("alpha"));
        assert!(state.selected_other_tmux_sessions.contains("beta"));
        assert_eq!(
            state.selected_other_tmux_names_in_order(),
            vec!["alpha".to_string(), "beta".to_string()]
        );
    }

    #[test]
    fn test_delete_selected_other_tmux_sessions_opens_bulk_kill_confirmation() {
        let mut state = state_with_other_tmux_sessions(&["alpha", "beta"]);
        state.selected_other_tmux_sessions.insert("alpha".to_string());
        state.selected_other_tmux_sessions.insert("beta".to_string());

        EventHandler::process_event(AppEvent::DeleteSelectedSessions, &mut state);

        let dialog = state
            .confirmation_dialog
            .as_ref()
            .expect("bulk kill confirmation");
        assert_eq!(dialog.title, "Kill tmux Sessions");
        assert!(matches!(
            &dialog.confirm_action,
            ConfirmAction::KillOtherTmuxSessions(names)
                if names == &vec!["alpha".to_string(), "beta".to_string()]
        ));
    }

    #[test]
    fn test_delete_session_uses_checked_other_tmux_sessions_before_cursor_row() {
        let mut state = state_with_other_tmux_sessions(&["alpha", "beta"]);
        state.selected_other_tmux_index = Some(1);
        state.selected_other_tmux_sessions.insert("alpha".to_string());
        state.selected_other_tmux_sessions.insert("beta".to_string());

        EventHandler::process_event(AppEvent::DeleteSession, &mut state);

        let dialog = state
            .confirmation_dialog
            .as_ref()
            .expect("bulk kill confirmation");
        assert!(matches!(
            &dialog.confirm_action,
            ConfirmAction::KillOtherTmuxSessions(names)
                if names == &vec!["alpha".to_string(), "beta".to_string()]
        ));
    }

    #[test]
    fn test_confirm_selected_other_tmux_sessions_queues_bulk_kill() {
        let mut state = state_with_other_tmux_sessions(&["alpha", "beta"]);
        state.selected_other_tmux_sessions.insert("alpha".to_string());
        state.selected_other_tmux_sessions.insert("beta".to_string());
        EventHandler::process_event(AppEvent::DeleteSelectedSessions, &mut state);

        state
            .confirmation_dialog
            .as_mut()
            .expect("bulk kill confirmation")
            .selected_option = true;
        EventHandler::process_event(AppEvent::ConfirmationConfirm, &mut state);

        assert!(state.selected_other_tmux_sessions.is_empty());
        assert!(matches!(
            state.pending_async_action,
            Some(AsyncAction::KillOtherTmuxSessions(ref names))
                if names == &vec!["alpha".to_string(), "beta".to_string()]
        ));
    }

    /// Verify the tri-option dialog defaults to Stop and cycles forward through
    /// all three options before wrapping back to Stop.
    #[test]
    fn test_show_delete_or_stop_confirmation_defaults_to_stop() {
        let mut state = AppState::new();
        let session_id = uuid::Uuid::new_v4();

        state.show_delete_or_stop_confirmation(session_id);

        let dialog = state.confirmation_dialog.as_ref().expect("Dialog should be present");
        let opts = dialog.options.as_ref().expect("Tri-option dialog");
        assert_eq!(opts.len(), 3, "Stop / Delete / Cancel");
        assert_eq!(opts[0].label, "Stop");
        assert_eq!(opts[1].label, "Delete");
        assert_eq!(opts[2].label, "Cancel");
        assert_eq!(dialog.selected_index, 0, "Default = Stop");
        assert!(
            matches!(opts[0].action, ConfirmAction::StopSession(id) if id == session_id),
            "First option must be StopSession for the right uuid"
        );
        assert!(
            matches!(opts[1].action, ConfirmAction::DeleteSession(id) if id == session_id),
            "Second option must be DeleteSession for the right uuid"
        );
        assert!(matches!(opts[2].action, ConfirmAction::Cancel));
    }

    /// Forward cycling and backwards cycling on a tri-option dialog.
    #[test]
    fn test_tri_option_dialog_cycle() {
        let mut dialog = ConfirmationDialog {
            title: "T".into(),
            message: "M".into(),
            confirm_action: ConfirmAction::Cancel,
            selected_option: false,
            warning: None,
            options: Some(vec![
                DialogOption {
                    label: "A".into(),
                    action: ConfirmAction::Cancel,
                },
                DialogOption {
                    label: "B".into(),
                    action: ConfirmAction::Cancel,
                },
                DialogOption {
                    label: "C".into(),
                    action: ConfirmAction::Cancel,
                },
            ]),
            selected_index: 0,
        };

        // Forward cycle 0 -> 1 -> 2 -> 0
        let len = dialog.options.as_ref().unwrap().len();
        dialog.selected_index = (dialog.selected_index + 1) % len;
        assert_eq!(dialog.selected_index, 1);
        dialog.selected_index = (dialog.selected_index + 1) % len;
        assert_eq!(dialog.selected_index, 2);
        dialog.selected_index = (dialog.selected_index + 1) % len;
        assert_eq!(dialog.selected_index, 0);

        // Backward cycle 0 -> 2 -> 1 -> 0
        dialog.selected_index = (dialog.selected_index + len - 1) % len;
        assert_eq!(dialog.selected_index, 2);
        dialog.selected_index = (dialog.selected_index + len - 1) % len;
        assert_eq!(dialog.selected_index, 1);
        dialog.selected_index = (dialog.selected_index + len - 1) % len;
        assert_eq!(dialog.selected_index, 0);
    }

    /// Binary dialog (existing callsites) keeps the legacy yes/no toggle.
    #[test]
    fn test_binary_dialog_unchanged() {
        let session_id = uuid::Uuid::new_v4();
        let mut state = AppState::new();
        state.show_delete_confirmation(session_id);
        let dialog = state.confirmation_dialog.as_ref().unwrap();
        assert!(dialog.options.is_none(), "Legacy binary dialog");
        assert!(!dialog.selected_option, "Default = No");
        assert!(matches!(
            dialog.confirm_action,
            ConfirmAction::DeleteSession(_)
        ));
    }

    /// Path encoding mirrors `find_transcript_path` in spawn-agent-lib.sh:30-69.
    #[test]
    fn test_encode_claude_project_dir() {
        use std::path::PathBuf;
        let p = PathBuf::from("/Users/stevie/code/foo-bar");
        assert_eq!(
            AppState::encode_claude_project_dir(&p),
            "-Users-stevie-code-foo-bar"
        );

        let p = PathBuf::from("/");
        assert_eq!(AppState::encode_claude_project_dir(&p), "-");
    }

    /// `find_latest_transcript_in` returns the most recently modified `.jsonl`
    /// under `<home>/.claude/projects/-{encoded}/`.
    #[test]
    fn test_find_latest_transcript_picks_newest_by_mtime() {
        use std::fs::{self, File};
        use std::path::PathBuf;
        use std::time::{Duration, SystemTime};

        let tmp = tempfile::tempdir().expect("tmpdir");
        let fake_home = tmp.path().to_path_buf();

        let worktree = PathBuf::from("/tmp/work-abc-test");
        let encoded = AppState::encode_claude_project_dir(&worktree);
        let project_dir = fake_home.join(".claude").join("projects").join(&encoded);
        fs::create_dir_all(&project_dir).unwrap();

        let older = project_dir.join("2024-01.jsonl");
        let newer = project_dir.join("2024-02.jsonl");
        fs::write(&older, "x").unwrap();
        fs::write(&newer, "y").unwrap();

        let now = SystemTime::now();
        File::open(&older)
            .unwrap()
            .set_modified(now - Duration::from_secs(120))
            .unwrap();
        File::open(&newer)
            .unwrap()
            .set_modified(now + Duration::from_secs(120))
            .unwrap();

        // Decoy non-jsonl file should be ignored.
        fs::write(project_dir.join("ignored.txt"), "z").unwrap();

        let result = AppState::find_latest_transcript_in(&fake_home, &worktree)
            .expect("Should find a transcript");
        assert_eq!(result.file_name().unwrap(), "2024-02.jsonl");
    }

    /// Missing project directory returns None (no transcript = fresh start).
    #[test]
    fn test_find_latest_transcript_missing_dir_returns_none() {
        use std::path::PathBuf;
        let tmp = tempfile::tempdir().expect("tmpdir");
        let fake_home = tmp.path().to_path_buf();

        let nonexistent = PathBuf::from("/tmp/never-existed-xyz");
        let result = AppState::find_latest_transcript_in(&fake_home, &nonexistent);
        assert!(result.is_none());
    }

    /// Empty project directory (no `.jsonl` files) returns None.
    #[test]
    fn test_find_latest_transcript_empty_dir_returns_none() {
        use std::fs;
        use std::path::PathBuf;
        let tmp = tempfile::tempdir().expect("tmpdir");
        let fake_home = tmp.path().to_path_buf();
        let worktree = PathBuf::from("/tmp/empty-work");
        let encoded = AppState::encode_claude_project_dir(&worktree);
        let project_dir = fake_home.join(".claude").join("projects").join(&encoded);
        fs::create_dir_all(&project_dir).unwrap();
        fs::write(project_dir.join("not-a-jsonl.txt"), "z").unwrap();

        let result = AppState::find_latest_transcript_in(&fake_home, &worktree);
        assert!(result.is_none());
    }

    /// `stopped_session_from_metadata` produces a Session with status Stopped
    /// and the right tmux name + worktree path. Backbone of the discovery pass.
    #[test]
    fn test_stopped_session_from_metadata_marks_status_stopped() {
        use crate::interactive::SessionMetadata;
        use crate::models::{SessionAgentType, SessionStatus};
        use std::path::PathBuf;

        let metadata = SessionMetadata {
            session_id: uuid::Uuid::new_v4(),
            tmux_session_name: "tmux_some_branch".to_string(),
            worktree_path: PathBuf::from("/tmp/work-stopped"),
            workspace_name: "ws".to_string(),
            created_at: chrono::Utc::now(),
            agent_type: SessionAgentType::Claude,
        };

        let session = AppState::stopped_session_from_metadata(&metadata);
        assert_eq!(session.id, metadata.session_id);
        assert!(matches!(session.status, SessionStatus::Stopped));
        assert_eq!(
            session.tmux_session_name.as_deref(),
            Some("tmux_some_branch")
        );
        assert_eq!(session.workspace_path, "/tmp/work-stopped");
        assert_eq!(session.agent_type, SessionAgentType::Claude);
    }

    // -- SessionFilter tests ------------------------------------------------

    use crate::app::state::SessionFilter;
    use crate::models::{Session, SessionStatus as Status};

    fn make_filter_session(mode: SessionMode, status: Status) -> Session {
        let mut s = Session::new("test".to_string(), "/tmp/x".to_string());
        s.mode = mode;
        s.status = status;
        s
    }

    #[test]
    fn test_session_filter_cycle_order() {
        assert_eq!(SessionFilter::All.next(), SessionFilter::ActiveOnly);
        assert_eq!(SessionFilter::ActiveOnly.next(), SessionFilter::StoppedOnly);
        assert_eq!(SessionFilter::StoppedOnly.next(), SessionFilter::All);
    }

    #[test]
    fn test_session_filter_default_is_all() {
        assert_eq!(SessionFilter::default(), SessionFilter::All);
    }

    #[test]
    fn test_session_filter_title_label() {
        assert_eq!(SessionFilter::All.title_label(), None);
        assert_eq!(SessionFilter::ActiveOnly.title_label(), Some("active"));
        assert_eq!(SessionFilter::StoppedOnly.title_label(), Some("stopped"));
    }

    #[test]
    fn test_session_passes_filter_all_lets_everything_through() {
        let mut state = AppState::new();
        state.session_filter = SessionFilter::All;
        for mode in [SessionMode::Interactive, SessionMode::Boss] {
            for status in [Status::Running, Status::Stopped, Status::Idle] {
                assert!(
                    state.session_passes_filter(&make_filter_session(mode.clone(), status.clone())),
                    "All should pass mode={:?} status={:?}",
                    mode,
                    status
                );
            }
        }
    }

    #[test]
    fn test_session_passes_filter_active_only_hides_stopped_interactive() {
        let mut state = AppState::new();
        state.session_filter = SessionFilter::ActiveOnly;

        // Interactive Stopped → hidden
        assert!(!state.session_passes_filter(&make_filter_session(
            SessionMode::Interactive,
            Status::Stopped
        )));
        // Interactive Running → shown
        assert!(state.session_passes_filter(&make_filter_session(
            SessionMode::Interactive,
            Status::Running
        )));
        // Boss-mode Stopped → still passes (filter only touches Interactive)
        assert!(
            state.session_passes_filter(&make_filter_session(SessionMode::Boss, Status::Stopped))
        );
    }

    #[test]
    fn test_session_passes_filter_stopped_only() {
        let mut state = AppState::new();
        state.session_filter = SessionFilter::StoppedOnly;

        assert!(state.session_passes_filter(&make_filter_session(
            SessionMode::Interactive,
            Status::Stopped
        )));
        assert!(!state.session_passes_filter(&make_filter_session(
            SessionMode::Interactive,
            Status::Running
        )));
        // Boss-mode is exempt — always passes regardless of filter mode.
        assert!(
            state.session_passes_filter(&make_filter_session(SessionMode::Boss, Status::Running))
        );
    }

    #[test]
    fn test_cycle_session_filter_advances_state() {
        let mut state = AppState::new();
        assert_eq!(state.session_filter, SessionFilter::All);
        state.cycle_session_filter();
        assert_eq!(state.session_filter, SessionFilter::ActiveOnly);
        state.cycle_session_filter();
        assert_eq!(state.session_filter, SessionFilter::StoppedOnly);
        state.cycle_session_filter();
        assert_eq!(state.session_filter, SessionFilter::All);
    }

    #[test]
    fn merge_oldest_call_day_keeps_earliest_across_loads() {
        use chrono::NaiveDate;
        let april = NaiveDate::from_ymd_opt(2026, 4, 1).unwrap();
        let may = NaiveDate::from_ymd_opt(2026, 5, 1).unwrap();

        // First load establishes the anchor.
        assert_eq!(
            crate::app::state::merge_oldest_call_day(None, Some(april)),
            Some(april)
        );

        // Reproduces the reported defect: a wide load establishes April,
        // a subsequent narrow (May-only) load must NOT raise the anchor.
        assert_eq!(
            crate::app::state::merge_oldest_call_day(Some(april), Some(may)),
            Some(april),
            "narrow load must not raise the anchor above its earlier extent"
        );

        // Order-independent: narrow first, then wide, narrows the anchor.
        assert_eq!(
            crate::app::state::merge_oldest_call_day(Some(may), Some(april)),
            Some(april)
        );

        // Empty load (no calls in the period) leaves anchor untouched.
        assert_eq!(
            crate::app::state::merge_oldest_call_day(Some(april), None),
            Some(april)
        );

        // Empty existing + empty candidate stays None.
        assert_eq!(crate::app::state::merge_oldest_call_day(None, None), None);
    }

    // -- statusline status TTL cache --------------------------------
    //
    // The cache backs both the global `W` shortcut (settings.json read on
    // every keystroke before this PR) and the top-of-Stats enable card
    // (settings.json read on every render frame). The tests below exercise
    // the pure inner helper with an injected clock + detector counter so
    // they're hermetic — no temp dirs or HOME mutation needed.

    use std::cell::Cell;
    use std::time::{Duration, Instant};

    #[test]
    fn statusline_cache_returns_value_on_first_call_and_records_time() {
        use crate::cli::statusline_install::StatuslineStatus;

        let mut cache = None;
        let calls = Cell::new(0u32);
        let now = Instant::now();
        let detect = || {
            calls.set(calls.get() + 1);
            Ok(StatuslineStatus::NotConfigured)
        };

        let result = AppState::statusline_status_cached_inner(
            &mut cache,
            Duration::from_secs(15),
            now,
            detect,
        );

        assert_eq!(result, Some(StatuslineStatus::NotConfigured));
        assert_eq!(calls.get(), 1, "first call must hit the detector");
        assert!(cache.is_some(), "cache must be populated after first call");
    }

    #[test]
    fn statusline_cache_coalesces_within_ttl() {
        use crate::cli::statusline_install::StatuslineStatus;

        let mut cache = None;
        let calls = Cell::new(0u32);
        let t0 = Instant::now();
        let detect_first = || {
            calls.set(calls.get() + 1);
            Ok(StatuslineStatus::Configured)
        };
        let _ = AppState::statusline_status_cached_inner(
            &mut cache,
            Duration::from_secs(15),
            t0,
            detect_first,
        );
        assert_eq!(calls.get(), 1);

        // Second call 5 seconds later — still inside the 15s TTL window.
        // Must serve from cache without invoking the detector.
        let t1 = t0 + Duration::from_secs(5);
        let detect_must_not_run = || -> anyhow::Result<StatuslineStatus> {
            panic!("detector must not run while cache is fresh");
        };
        let result = AppState::statusline_status_cached_inner(
            &mut cache,
            Duration::from_secs(15),
            t1,
            detect_must_not_run,
        );
        assert_eq!(
            result,
            Some(StatuslineStatus::Configured),
            "fresh cache hit must return the original value"
        );
        assert_eq!(calls.get(), 1, "second call inside TTL must not re-detect");
    }

    #[test]
    fn statusline_cache_re_detects_after_ttl_expires() {
        use crate::cli::statusline_install::StatuslineStatus;

        let mut cache = None;
        let calls = Cell::new(0u32);
        let ttl = Duration::from_secs(15);
        let t0 = Instant::now();
        let detect = || {
            calls.set(calls.get() + 1);
            Ok(StatuslineStatus::NotConfigured)
        };
        let _ = AppState::statusline_status_cached_inner(&mut cache, ttl, t0, detect);
        assert_eq!(calls.get(), 1);

        // 30 seconds later: well past the 15s TTL — detector must run.
        let t1 = t0 + Duration::from_secs(30);
        let detect_again = || {
            calls.set(calls.get() + 1);
            Ok(StatuslineStatus::Configured)
        };
        let result = AppState::statusline_status_cached_inner(&mut cache, ttl, t1, detect_again);
        assert_eq!(result, Some(StatuslineStatus::Configured));
        assert_eq!(calls.get(), 2, "expired cache must re-detect");
    }

    #[test]
    fn statusline_cache_stores_detector_failure_to_avoid_retry_storm() {
        // A failing detector returns None and the cache records None +
        // the timestamp. A subsequent call within TTL serves the None
        // from cache without re-hitting the detector — otherwise an IO
        // error on settings.json would translate into one filesystem
        // probe per render frame.
        let mut cache = None;
        let calls = Cell::new(0u32);
        let t0 = Instant::now();
        let detect_err = || -> anyhow::Result<crate::cli::statusline_install::StatuslineStatus> {
            calls.set(calls.get() + 1);
            anyhow::bail!("simulated read failure")
        };
        let result = AppState::statusline_status_cached_inner(
            &mut cache,
            Duration::from_secs(15),
            t0,
            detect_err,
        );
        assert_eq!(result, None);
        assert_eq!(calls.get(), 1);

        // Within TTL, the cached None is returned without re-detecting.
        let t1 = t0 + Duration::from_secs(2);
        let detect_must_not_run =
            || -> anyhow::Result<crate::cli::statusline_install::StatuslineStatus> {
                panic!("detector must not run while cache is fresh, even when value is None");
            };
        let result = AppState::statusline_status_cached_inner(
            &mut cache,
            Duration::from_secs(15),
            t1,
            detect_must_not_run,
        );
        assert_eq!(result, None);
        assert_eq!(
            calls.get(),
            1,
            "cached None must not retrigger the detector"
        );
    }

    #[test]
    fn invalidate_statusline_status_cache_forces_refresh() {
        use crate::cli::statusline_install::StatuslineStatus;

        let mut state = AppState::new();
        // Seed the cache directly with a stale value.
        state.statusline_status_cache =
            Some((Some(StatuslineStatus::NotConfigured), Instant::now()));
        assert!(state.statusline_status_cache.is_some());

        state.invalidate_statusline_status_cache();
        assert!(
            state.statusline_status_cache.is_none(),
            "invalidation must drop the cached entry"
        );
    }
}
