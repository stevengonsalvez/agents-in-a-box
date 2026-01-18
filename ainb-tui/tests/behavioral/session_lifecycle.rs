// ABOUTME: Behavioral tests for session lifecycle state machine, agent types, and model selection
// Complements test_session_model.rs with state machine and agent type coverage.

use ainb::models::{
    ClaudeModel, Session, SessionAgentType, SessionMode, SessionStatus, ShellSession,
};
use std::path::PathBuf;

// =============================================================================
// Test 1: Session state machine valid transitions
// =============================================================================

/// Verifies all valid state transitions in the session lifecycle state machine.
/// State machine: Stopped -> Running -> Idle -> (Stopped | Running)
///                       -> Error -> (Stopped | Running)
#[test]
fn test_session_state_machine_valid_transitions() {
    // Arrange: Create session starting in Stopped state
    let mut session = Session::new("transition-test".to_string(), "/workspace".to_string());
    assert!(matches!(session.status, SessionStatus::Stopped));

    // Act/Assert: Stopped -> Running (valid: starting session)
    session.set_status(SessionStatus::Running);
    assert!(matches!(session.status, SessionStatus::Running));

    // Act/Assert: Running -> Idle (valid: Claude process stops but tmux remains)
    session.set_status(SessionStatus::Idle);
    assert!(matches!(session.status, SessionStatus::Idle));

    // Act/Assert: Idle -> Running (valid: restart Claude in existing tmux)
    session.set_status(SessionStatus::Running);
    assert!(matches!(session.status, SessionStatus::Running));

    // Act/Assert: Running -> Error (valid: runtime error occurs)
    session.set_status(SessionStatus::Error("Connection lost".to_string()));
    assert!(matches!(session.status, SessionStatus::Error(_)));

    // Act/Assert: Error -> Running (valid: recovery after error)
    session.set_status(SessionStatus::Running);
    assert!(matches!(session.status, SessionStatus::Running));

    // Act/Assert: Running -> Stopped (valid: full shutdown)
    session.set_status(SessionStatus::Stopped);
    assert!(matches!(session.status, SessionStatus::Stopped));

    // Act/Assert: Stopped -> Error (valid: startup failure)
    session.set_status(SessionStatus::Error("Startup failed".to_string()));
    assert!(matches!(session.status, SessionStatus::Error(_)));

    // Act/Assert: Error -> Stopped (valid: cleanup after error)
    session.set_status(SessionStatus::Stopped);
    assert!(matches!(session.status, SessionStatus::Stopped));
}

// =============================================================================
// Test 2: Session serialization roundtrip preserves all fields
// =============================================================================

/// Verifies that JSON serialization/deserialization preserves all session fields,
/// including optional fields and newly added fields like agent_type and model.
#[test]
fn test_session_serialization_roundtrip_preserves_all_fields() {
    // Arrange: Create session with all fields populated
    let original = Session::new_with_options(
        "full-session".to_string(),
        "/path/to/workspace".to_string(),
        true, // skip_permissions
        SessionMode::Boss,
        Some("Implement feature X".to_string()), // boss_prompt
        SessionAgentType::Claude,
        Some(ClaudeModel::Opus),
    );

    // Manually set additional fields that new_with_options doesn't set
    let mut original = original;
    original.container_id = Some("container-abc123".to_string());
    original.status = SessionStatus::Idle;
    original.git_changes.added = 5;
    original.git_changes.modified = 3;
    original.git_changes.deleted = 1;
    original.recent_logs = Some("Last log entry".to_string());
    original.tmux_session_name = Some("tmux_full-session".to_string());
    original.preview_content = Some("Preview text".to_string());
    original.is_attached = true;

    // Act: Serialize and deserialize
    let json = serde_json::to_string_pretty(&original).expect("serialization failed");
    let restored: Session = serde_json::from_str(&json).expect("deserialization failed");

    // Assert: All fields preserved
    assert_eq!(restored.id, original.id);
    assert_eq!(restored.name, original.name);
    assert_eq!(restored.workspace_path, original.workspace_path);
    assert_eq!(restored.branch_name, original.branch_name);
    assert_eq!(restored.container_id, original.container_id);
    assert_eq!(restored.status, original.status);
    assert_eq!(restored.created_at, original.created_at);
    assert_eq!(restored.last_accessed, original.last_accessed);
    assert_eq!(restored.git_changes.added, original.git_changes.added);
    assert_eq!(restored.git_changes.modified, original.git_changes.modified);
    assert_eq!(restored.git_changes.deleted, original.git_changes.deleted);
    assert_eq!(restored.recent_logs, original.recent_logs);
    assert_eq!(restored.skip_permissions, original.skip_permissions);
    assert_eq!(restored.mode, original.mode);
    assert_eq!(restored.boss_prompt, original.boss_prompt);
    assert_eq!(restored.agent_type, original.agent_type);
    assert_eq!(restored.model, original.model);
    assert_eq!(restored.tmux_session_name, original.tmux_session_name);
    assert_eq!(restored.preview_content, original.preview_content);
    assert_eq!(restored.is_attached, original.is_attached);
}

// =============================================================================
// Test 3: Session agent type availability
// =============================================================================

/// Verifies that only Claude and Shell agents are currently available,
/// while Codex, Gemini, and Kiro are marked as coming soon.
#[test]
fn test_session_agent_type_availability() {
    // Assert: Available agents
    assert!(
        SessionAgentType::Claude.is_available(),
        "Claude should be available"
    );
    assert!(
        SessionAgentType::Shell.is_available(),
        "Shell should be available"
    );

    // Assert: Coming soon agents
    assert!(
        !SessionAgentType::Codex.is_available(),
        "Codex should not yet be available"
    );
    assert!(
        !SessionAgentType::Gemini.is_available(),
        "Gemini should not yet be available"
    );
    assert!(
        !SessionAgentType::Kiro.is_available(),
        "Kiro should not yet be available"
    );

    // Assert: All agent types have proper metadata
    for agent_type in [
        SessionAgentType::Claude,
        SessionAgentType::Shell,
        SessionAgentType::Codex,
        SessionAgentType::Gemini,
        SessionAgentType::Kiro,
    ] {
        assert!(!agent_type.icon().is_empty(), "Agent should have an icon");
        assert!(!agent_type.name().is_empty(), "Agent should have a name");
        assert!(
            !agent_type.description().is_empty(),
            "Agent should have a description"
        );
    }
}

// =============================================================================
// Test 4: Claude model CLI values
// =============================================================================

/// Verifies that ClaudeModel CLI values match what the Claude CLI expects
/// and that all models provide proper display information.
#[test]
fn test_claude_model_cli_values() {
    // Assert: CLI values match expected strings
    assert_eq!(
        ClaudeModel::Sonnet.cli_value(),
        "sonnet",
        "Sonnet CLI value should be 'sonnet'"
    );
    assert_eq!(
        ClaudeModel::Opus.cli_value(),
        "opus",
        "Opus CLI value should be 'opus'"
    );
    assert_eq!(
        ClaudeModel::Haiku.cli_value(),
        "haiku",
        "Haiku CLI value should be 'haiku'"
    );

    // Assert: Display names are capitalized versions
    assert_eq!(ClaudeModel::Sonnet.display_name(), "Sonnet");
    assert_eq!(ClaudeModel::Opus.display_name(), "Opus");
    assert_eq!(ClaudeModel::Haiku.display_name(), "Haiku");

    // Assert: All models have descriptions and icons
    for model in ClaudeModel::all() {
        assert!(
            !model.description().is_empty(),
            "Model {:?} should have a description",
            model
        );
        assert!(
            !model.icon().is_empty(),
            "Model {:?} should have an icon",
            model
        );
    }

    // Assert: Sonnet is the default model
    assert_eq!(ClaudeModel::default(), ClaudeModel::Sonnet);

    // Assert: all() returns all three models
    let all_models = ClaudeModel::all();
    assert_eq!(all_models.len(), 3);
    assert!(all_models.contains(&ClaudeModel::Sonnet));
    assert!(all_models.contains(&ClaudeModel::Opus));
    assert!(all_models.contains(&ClaudeModel::Haiku));
}

// =============================================================================
// Test 5: Shell session naming conventions
// =============================================================================

/// Verifies that ShellSession tmux names follow the expected patterns
/// for different creation scenarios.
#[test]
fn test_shell_session_naming_conventions() {
    // Scenario 1: Shell session with branch name
    let session_with_branch = ShellSession::new(
        PathBuf::from("/workspace"),
        PathBuf::from("/workspace/worktrees/feature-auth"),
        Some("feature/auth-refactor".to_string()),
    );

    // Assert: Name includes "shell-" prefix and sanitized branch
    assert!(
        session_with_branch.name.starts_with("shell-"),
        "Shell session name should start with 'shell-'"
    );
    assert!(
        session_with_branch.name.contains("feature"),
        "Shell session name should contain branch info"
    );
    // Branch slashes should be replaced with dashes
    assert!(
        !session_with_branch.name.contains('/'),
        "Shell session name should not contain slashes"
    );

    // Assert: Tmux name follows ainb-sh-{short_id} pattern
    assert!(
        session_with_branch.tmux_session_name.starts_with("ainb-sh-"),
        "Tmux session name should start with 'ainb-sh-'"
    );
    assert_eq!(
        session_with_branch.tmux_session_name.len(),
        "ainb-sh-".len() + 8,
        "Tmux session name should be 'ainb-sh-' + 8-char UUID"
    );

    // Scenario 2: Workspace shell
    let workspace_shell =
        ShellSession::new_workspace_shell(PathBuf::from("/workspace/my-project"), "my-project");

    // Assert: Workspace shell has $ prefix
    assert!(
        workspace_shell.name.starts_with("$ "),
        "Workspace shell name should start with '$ '"
    );
    assert!(
        workspace_shell.name.contains("my-project"),
        "Workspace shell name should contain workspace name"
    );

    // Assert: Tmux name follows ainb-ws-{short_id} pattern
    assert!(
        workspace_shell.tmux_session_name.starts_with("ainb-ws-"),
        "Workspace tmux name should start with 'ainb-ws-'"
    );

    // Scenario 3: Shell session without branch (falls back to directory name)
    let session_no_branch = ShellSession::new(
        PathBuf::from("/workspace"),
        PathBuf::from("/workspace/worktrees/my-feature"),
        None,
    );

    // Assert: Uses directory name when no branch provided
    assert!(
        session_no_branch.name.contains("my-feature"),
        "Shell session should use directory name when no branch provided"
    );
}

// =============================================================================
// Test 6: Session status can_restart logic
// =============================================================================

/// Verifies the can_restart() method returns true only for states
/// where restarting makes sense (Idle and Error).
#[test]
fn test_session_status_can_restart_logic() {
    // Assert: Idle sessions can be restarted (Claude stopped but tmux exists)
    assert!(
        SessionStatus::Idle.can_restart(),
        "Idle sessions should be restartable"
    );

    // Assert: Error sessions can be restarted (recovery attempt)
    assert!(
        SessionStatus::Error("Some error".to_string()).can_restart(),
        "Error sessions should be restartable"
    );
    assert!(
        SessionStatus::Error(String::new()).can_restart(),
        "Error sessions with empty message should be restartable"
    );

    // Assert: Running sessions cannot be restarted (already running)
    assert!(
        !SessionStatus::Running.can_restart(),
        "Running sessions should not be restartable"
    );

    // Assert: Stopped sessions cannot be restarted (need to start fresh)
    assert!(
        !SessionStatus::Stopped.can_restart(),
        "Stopped sessions should not be restartable - they need full startup"
    );
}

// =============================================================================
// Test 7: Session idle status behavior
// =============================================================================

/// Verifies Idle status specific behavior including indicator and state checks.
/// Idle represents a state where tmux exists but Claude process has stopped.
#[test]
fn test_session_idle_status_behavior() {
    let idle_status = SessionStatus::Idle;

    // Assert: Idle has its own indicator (empty circle)
    assert_eq!(
        idle_status.indicator(),
        "○",
        "Idle should have empty circle indicator"
    );

    // Assert: Idle is not considered "running"
    assert!(
        !idle_status.is_running(),
        "Idle should not be considered running"
    );

    // Assert: Idle can be restarted
    assert!(
        idle_status.can_restart(),
        "Idle sessions should be restartable"
    );

    // Assert: Session can transition to and from Idle
    let mut session = Session::new("idle-test".to_string(), "/workspace".to_string());

    // Simulate: Start -> Run -> Idle -> Restart cycle
    session.set_status(SessionStatus::Running);
    assert!(session.status.is_running());

    session.set_status(SessionStatus::Idle);
    assert!(!session.status.is_running());
    assert!(session.status.can_restart());

    session.set_status(SessionStatus::Running);
    assert!(session.status.is_running());
}

// =============================================================================
// Test 8: Session error status preserves message
// =============================================================================

/// Verifies that Error status preserves the error message through serialization
/// and provides expected behavior.
#[test]
fn test_session_error_status_preserves_message() {
    let error_message = "Connection timeout after 30 seconds";

    // Arrange: Create session with error status
    let mut session = Session::new("error-test".to_string(), "/workspace".to_string());
    session.set_status(SessionStatus::Error(error_message.to_string()));

    // Assert: Error message is preserved in status
    match &session.status {
        SessionStatus::Error(msg) => {
            assert_eq!(msg, error_message, "Error message should be preserved");
        }
        _ => panic!("Expected Error status"),
    }

    // Assert: Error has correct indicator
    assert_eq!(session.status.indicator(), "✗");

    // Assert: Error is not running
    assert!(!session.status.is_running());

    // Act: Serialize and deserialize
    let json = serde_json::to_string(&session).expect("serialization failed");
    let restored: Session = serde_json::from_str(&json).expect("deserialization failed");

    // Assert: Error message preserved through serialization
    match &restored.status {
        SessionStatus::Error(msg) => {
            assert_eq!(
                msg, error_message,
                "Error message should survive serialization roundtrip"
            );
        }
        _ => panic!("Expected Error status after deserialization"),
    }

    // Assert: Different error messages are distinct
    let error1 = SessionStatus::Error("Error A".to_string());
    let error2 = SessionStatus::Error("Error B".to_string());
    assert_ne!(error1, error2, "Different error messages should not be equal");

    // Assert: Same error messages are equal
    let error3 = SessionStatus::Error("Error A".to_string());
    assert_eq!(
        error1, error3,
        "Same error messages should be equal"
    );
}
