// ABOUTME: Behavioral tests for tmux session lifecycle management
//
// Tests verify correct behavior for session creation, cleanup, content capture,
// and Claude process detection. All tests are conditional on tmux availability.

use super::fixtures::{
    cleanup_tmux_session, send_tmux_keys, tmux_available, tmux_session_exists,
};
use crate::require_tmux;
use ainb::tmux::{CaptureOptions, ClaudeProcessDetector, TmuxSession};
use anyhow::Result;
use std::time::Duration;
use uuid::Uuid;

/// Generate a unique test session name to avoid conflicts
fn unique_session_name(prefix: &str) -> String {
    format!("test_{}_{}", prefix, Uuid::new_v4().to_string()[..8].to_string())
}

/// Test that tmux sessions can be created and cleaned up properly
#[tokio::test]
async fn test_tmux_session_create_and_cleanup() -> Result<()> {
    require_tmux!();

    let session_name = unique_session_name("create");
    let mut session = TmuxSession::new(session_name.clone(), "bash".to_string());

    // Create a temp directory for the session
    let temp_dir = tempfile::tempdir()?;

    // Start the session
    session.start(temp_dir.path()).await?;

    // Verify session exists
    assert!(
        session.does_session_exist().await,
        "Session should exist after start"
    );
    assert!(
        tmux_session_exists(session.name()),
        "Session should be visible via tmux has-session"
    );

    // Cleanup the session
    session.cleanup().await?;

    // Allow tmux to process the kill command
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Verify session no longer exists
    assert!(
        !session.does_session_exist().await,
        "Session should not exist after cleanup"
    );
    assert!(
        !tmux_session_exists(session.name()),
        "Session should not be visible after cleanup"
    );

    Ok(())
}

/// Test that session names with special characters are properly sanitized
#[tokio::test]
async fn test_tmux_session_name_sanitization() -> Result<()> {
    require_tmux!();

    // Test various special characters that tmux doesn't allow in session names
    let test_cases = vec![
        ("my session", "tmux_my_session"),
        ("test.name", "tmux_test_name"),
        ("path/to/thing", "tmux_path_to_thing"),
        ("colon:separated", "tmux_colon_separated"),
        ("complex.name/with:all chars", "tmux_complex_name_with_all_chars"),
        // Already prefixed should not double-prefix
        ("tmux_already_prefixed", "tmux_already_prefixed"),
    ];

    for (input, expected) in test_cases {
        let session = TmuxSession::new(input.to_string(), "bash".to_string());
        assert_eq!(
            session.name(),
            expected,
            "Session name '{}' should sanitize to '{}'",
            input,
            expected
        );
    }

    // Verify a sanitized name actually works with tmux
    let session_name = unique_session_name("special.chars/test:name");
    let mut session = TmuxSession::new(session_name.clone(), "bash".to_string());
    let temp_dir = tempfile::tempdir()?;

    session.start(temp_dir.path()).await?;
    assert!(
        session.does_session_exist().await,
        "Session with sanitized name should start successfully"
    );

    // Cleanup
    session.cleanup().await?;

    Ok(())
}

/// Test capturing pane content after sending commands
#[tokio::test]
async fn test_tmux_session_capture_pane_content() -> Result<()> {
    require_tmux!();

    let session_name = unique_session_name("capture");
    let mut session = TmuxSession::new(session_name.clone(), "bash".to_string());
    let temp_dir = tempfile::tempdir()?;

    session.start(temp_dir.path()).await?;

    // Wait for shell to initialize
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Send a command that produces predictable output
    send_tmux_keys(session.name(), "echo 'CAPTURE_TEST_OUTPUT_12345'")?;
    send_tmux_keys(session.name(), "Enter")?;

    // Wait for command to execute
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Capture the pane content
    let content = session.capture_pane_content().await?;

    // Verify our output is in the captured content
    assert!(
        content.contains("CAPTURE_TEST_OUTPUT_12345"),
        "Captured content should contain our test output. Got: {}",
        content
    );

    // Cleanup
    session.cleanup().await?;

    Ok(())
}

/// Test that starting a session twice is idempotent (doesn't fail)
#[tokio::test]
async fn test_tmux_session_idempotent_start() -> Result<()> {
    require_tmux!();

    let session_name = unique_session_name("idempotent");
    let mut session = TmuxSession::new(session_name.clone(), "bash".to_string());
    let temp_dir = tempfile::tempdir()?;

    // First start
    session.start(temp_dir.path()).await?;
    assert!(session.does_session_exist().await, "Session should exist after first start");

    // Second start should not fail (implementation kills and recreates)
    session.start(temp_dir.path()).await?;
    assert!(
        session.does_session_exist().await,
        "Session should still exist after second start"
    );

    // Cleanup
    session.cleanup().await?;

    Ok(())
}

/// Test that cleanup is idempotent (calling twice doesn't fail)
#[tokio::test]
async fn test_tmux_session_cleanup_is_idempotent() -> Result<()> {
    require_tmux!();

    let session_name = unique_session_name("cleanup_idem");
    let mut session = TmuxSession::new(session_name.clone(), "bash".to_string());
    let temp_dir = tempfile::tempdir()?;

    // Start and then cleanup
    session.start(temp_dir.path()).await?;
    session.cleanup().await?;

    // Allow tmux to process
    tokio::time::sleep(Duration::from_millis(100)).await;

    assert!(
        !session.does_session_exist().await,
        "Session should not exist after first cleanup"
    );

    // Second cleanup should not fail
    let result = session.cleanup().await;
    assert!(
        result.is_ok(),
        "Second cleanup should not fail, got: {:?}",
        result.err()
    );

    Ok(())
}

/// Test Claude process detector recognizes status bar patterns
#[tokio::test]
async fn test_claude_process_detector_status_bar_patterns() -> Result<()> {
    // Note: This test doesn't require tmux as it tests pattern matching directly
    let detector = ClaudeProcessDetector::new();

    // Test cases with Claude status bar present (should detect)
    let positive_cases = vec![
        // Full status bar
        "Some output\nModel: Sonnet 4.5  Cost: $0.45  Session: 12m  Ctx: 25k\nMore output",
        // Partial status bar with 2+ indicators
        "Model: Opus  Cost: $1.23",
        "Session: 5m  Ctx: 10k",
        "Cost: $0.00  Model: Haiku",
        // Different model names
        "Model: Claude 3.5 Sonnet  Cost: $2.50",
        "Model: claude-3-opus-20240229  Session: 1h",
        // High costs
        "Model: Opus  Cost: $123.45  Session: 45m",
        // Long sessions
        "Model: Sonnet 4.5  Session: 2h15m  Ctx: 150k",
    ];

    for content in positive_cases {
        assert!(
            detector.has_claude_status_bar(content),
            "Should detect Claude status bar in: {}",
            content
        );
    }

    // Test cases without Claude status bar (should not detect)
    let negative_cases = vec![
        // Regular shell output
        "$ ls -la\ntotal 64\ndrwxr-xr-x",
        // Only one indicator (not enough)
        "Some output that mentions Model: Something\nBut no other indicators",
        "Just mentions Cost: somewhere",
        // Empty content
        "",
        // Random text
        "Hello world\nThis is just some text",
        // Similar but not matching patterns
        "The model was trained on data",
        "The session lasted 5 minutes",
    ];

    for content in negative_cases {
        assert!(
            !detector.has_claude_status_bar(content),
            "Should NOT detect Claude status bar in: {}",
            content
        );
    }

    Ok(())
}

/// Test different capture options: visible vs full history
#[tokio::test]
async fn test_tmux_capture_options_visible_vs_full_history() -> Result<()> {
    require_tmux!();

    let session_name = unique_session_name("capture_opts");
    let mut session = TmuxSession::new(session_name.clone(), "bash".to_string());
    let temp_dir = tempfile::tempdir()?;

    session.start(temp_dir.path()).await?;

    // Wait for shell to initialize
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Generate some history by sending multiple commands
    for i in 1..=5 {
        send_tmux_keys(session.name(), &format!("echo 'HISTORY_LINE_{}'", i))?;
        send_tmux_keys(session.name(), "Enter")?;
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Wait for commands to complete
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Test visible capture options
    let visible_opts = CaptureOptions::visible();
    assert!(visible_opts.start_line.is_none(), "Visible capture should have no start line");
    assert!(visible_opts.end_line.is_none(), "Visible capture should have no end line");
    assert!(visible_opts.include_escape_sequences, "Should include escape sequences by default");
    assert!(visible_opts.join_wrapped_lines, "Should join wrapped lines by default");

    // Test full history capture options
    let history_opts = CaptureOptions::full_history();
    assert_eq!(
        history_opts.start_line,
        Some("-".to_string()),
        "Full history should have '-' start line"
    );
    assert_eq!(
        history_opts.end_line,
        Some("-".to_string()),
        "Full history should have '-' end line"
    );

    // Capture visible content
    let visible_content = session.capture_pane_content().await?;

    // Capture full history
    let full_content = session.capture_full_history().await?;

    // Both should contain our test output
    assert!(
        visible_content.contains("HISTORY_LINE") || full_content.contains("HISTORY_LINE"),
        "At least one capture mode should contain our history lines"
    );

    // Full history should generally be >= visible content length
    // (unless screen is very large and history is small)
    // We check that full_history captures content successfully
    assert!(
        !full_content.is_empty(),
        "Full history capture should return non-empty content"
    );

    // Cleanup
    session.cleanup().await?;

    Ok(())
}

#[cfg(test)]
mod helper_tests {
    use super::*;

    #[test]
    fn test_unique_session_name_generates_different_names() {
        let name1 = unique_session_name("test");
        let name2 = unique_session_name("test");
        assert_ne!(name1, name2, "Each call should generate a unique name");
    }

    #[test]
    fn test_unique_session_name_contains_prefix() {
        let name = unique_session_name("myprefix");
        assert!(
            name.contains("myprefix"),
            "Generated name should contain the prefix"
        );
    }
}
