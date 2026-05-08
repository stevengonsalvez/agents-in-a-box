//! Centralized editor detection and configuration.
//!
//! This module provides a single source of truth for supported editors,
//! their command-line executables, and cross-platform detection logic.

/// Supported editors with their display names and command-line executables.
pub const EDITORS: &[(&str, &str)] = &[
    ("VS Code", "code"),
    ("Cursor", "cursor"),
    ("Zed", "zed"),
    ("Neovim", "nvim"),
    ("Vim", "vim"),
    ("Emacs", "emacs"),
    ("Sublime Text", "subl"),
];

/// Check if a command exists on the system (cross-platform).
#[must_use]
pub fn command_exists(cmd: &str) -> bool {
    which::which(cmd).is_ok()
}

/// Convert editor display name to command.
#[must_use]
pub fn editor_name_to_command(name: &str) -> Option<&'static str> {
    EDITORS
        .iter()
        .find(|(display_name, _)| *display_name == name)
        .map(|(_, cmd)| *cmd)
}

/// Convert command to editor display name.
#[must_use]
pub fn editor_command_to_name(command: &str) -> Option<&'static str> {
    EDITORS
        .iter()
        .find(|(_, cmd)| *cmd == command)
        .map(|(name, _)| *name)
}

/// Detect which editors are available on the system.
/// Returns a list of (display_name, command, is_available) tuples.
#[must_use]
pub fn detect_available_editors() -> Vec<(String, String, bool)> {
    EDITORS
        .iter()
        .map(|(name, cmd)| {
            let available = command_exists(cmd);
            (name.to_string(), cmd.to_string(), available)
        })
        .collect()
}

/// Get only the editors that are installed on the system.
/// Returns a list of (display_name, command) tuples.
#[must_use]
pub fn get_installed_editors() -> Vec<(String, String)> {
    detect_available_editors()
        .into_iter()
        .filter(|(_, _, available)| *available)
        .map(|(name, cmd, _)| (name, cmd))
        .collect()
}

/// Get editor names for display in UI (with availability info).
/// Returns (name, is_available) pairs.
#[must_use]
pub fn get_editor_options() -> Vec<(String, bool)> {
    detect_available_editors()
        .into_iter()
        .map(|(name, _, available)| (name, available))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_editor_name_to_command() {
        assert_eq!(editor_name_to_command("VS Code"), Some("code"));
        assert_eq!(editor_name_to_command("Cursor"), Some("cursor"));
        assert_eq!(editor_name_to_command("Unknown"), None);
    }

    #[test]
    fn test_editor_command_to_name() {
        assert_eq!(editor_command_to_name("code"), Some("VS Code"));
        assert_eq!(editor_command_to_name("nvim"), Some("Neovim"));
        assert_eq!(editor_command_to_name("unknown"), None);
    }

    #[test]
    fn test_editors_constant_not_empty() {
        assert!(!EDITORS.is_empty());
    }
}
