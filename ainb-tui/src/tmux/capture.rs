// ABOUTME: Tmux pane content capture utilities
//
// Provides functions for capturing tmux pane content with various options,
// including full scrollback history, visible pane content, and ANSI escape
// sequence preservation.

#![allow(dead_code)]

use anyhow::Result;
use lazy_static::lazy_static;
use regex::Regex;
use tokio::process::Command;

lazy_static! {
    /// Regex pattern to match ANSI escape sequences
    /// Matches: ESC [ ... m (color/style codes)
    /// Reference: https://en.wikipedia.org/wiki/ANSI_escape_code
    static ref ANSI_REGEX: Regex = Regex::new(r"\x1b\[[0-9;]*m").unwrap();
}

/// Options for capturing tmux pane content
#[derive(Debug, Clone)]
pub struct CaptureOptions {
    /// Start line for capture ("-" for start of history, None for current visible area)
    pub start_line: Option<String>,
    /// End line for capture ("-" for end of history, None for current visible area)
    pub end_line: Option<String>,
    /// Whether to include ANSI escape sequences in the output
    pub include_escape_sequences: bool,
    /// Whether to join wrapped lines
    pub join_wrapped_lines: bool,
}

impl Default for CaptureOptions {
    fn default() -> Self {
        Self {
            start_line: None,
            end_line: None,
            include_escape_sequences: true,
            join_wrapped_lines: true,
        }
    }
}

impl CaptureOptions {
    /// Create options for capturing visible pane content only
    pub fn visible() -> Self {
        Self::default()
    }

    /// Create options for capturing full scrollback history
    pub fn full_history() -> Self {
        Self {
            start_line: Some("-".to_string()),
            end_line: Some("-".to_string()),
            include_escape_sequences: true,
            join_wrapped_lines: true,
        }
    }
}

/// Strip ANSI escape sequences from text
///
/// Removes color codes, cursor movements, and other terminal control sequences
/// that would appear as literal text in ratatui (which expects plain text or styled spans).
///
/// # Arguments
/// * `text` - The text containing ANSI escape sequences
///
/// # Returns
/// * Clean text without ANSI codes
///
/// # Example
/// ```ignore
/// let colored = "\x1b[38;5;123mHello\x1b[0m";
/// let plain = strip_ansi_codes(colored);
/// assert_eq!(plain, "Hello");
/// ```
fn strip_ansi_codes(text: &str) -> String {
    ANSI_REGEX.replace_all(text, "").to_string()
}

/// Filter out Claude Code UI noise from captured tmux content
///
/// Removes:
/// - Permission dialog boxes
/// - Security warnings
/// - Box-drawing characters around dialogs
/// - Claude Code meta-UI elements
///
/// Preserves ANSI color codes for colored output display.
fn filter_claude_ui_noise(content: &str) -> String {
    let original_lines: Vec<&str> = content.lines().collect();
    let mut filtered_lines = Vec::new();
    let mut skip_until_blank = false;

    for original_line in original_lines {
        // Strip ANSI codes just for pattern matching, but keep original line
        let clean_line = strip_ansi_codes(original_line);

        // Skip permission dialog markers
        // NOTE: Patterns must be SPECIFIC to Claude's permission dialogs.
        // Generic words like "Security" or "details" will filter legitimate output!
        if clean_line.contains("Do you want to work in this folder?")
            || clean_line.contains("In order to work in this folder, we need your permission")
            || clean_line.contains("If this folder has malicious code")
            || clean_line.contains("Only continue if this is your code")
            || clean_line.contains("Security Information")
            || clean_line.contains("View security details")
            || clean_line.contains("https://docs.claude.com/s/claude-code-security")
            || clean_line.contains("Yes, continue")
            || clean_line.contains("No, exit")
            || clean_line.contains("Enter to confirm")
            || clean_line.contains("Esc to exit") {
            skip_until_blank = true;
            continue;
        }

        // Skip box-drawing lines (often part of dialogs)
        if clean_line.chars().all(|c| {
            matches!(c, '─' | '│' | '┌' | '┐' | '└' | '┘' | '├' | '┤' | '┬' | '┴' | '┼' | ' ' | '>' | '1' | '2' | '.' | ',')
        }) && !clean_line.trim().is_empty() {
            continue;
        }

        // Reset skip flag on blank line
        if clean_line.trim().is_empty() {
            if skip_until_blank {
                skip_until_blank = false;
                continue;
            }
        }

        // Skip if we're in skip mode
        if skip_until_blank {
            continue;
        }

        // Keep the original line WITH ANSI codes preserved
        filtered_lines.push(original_line);
    }

    filtered_lines.join("\n")
}

/// Capture content from a tmux pane
///
/// # Arguments
/// * `session_name` - The name of the tmux session
/// * `options` - Capture options specifying what and how to capture
///
/// # Returns
/// * `Result<String>` - The captured content or an error
pub async fn capture_pane(session_name: &str, options: CaptureOptions) -> Result<String> {
    let mut args = vec!["capture-pane", "-p", "-t", session_name];

    // Add escape sequence flag if requested
    if options.include_escape_sequences {
        args.push("-e");
    }

    // Add join wrapped lines flag if requested
    if options.join_wrapped_lines {
        args.push("-J");
    }

    // Add start line if specified
    let start_arg;
    if let Some(start) = &options.start_line {
        start_arg = format!("-S{}", start);
        args.push(&start_arg);
    }

    // Add end line if specified
    let end_arg;
    if let Some(end) = &options.end_line {
        end_arg = format!("-E{}", end);
        args.push(&end_arg);
    }

    let output = Command::new("tmux").args(&args).output().await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Failed to capture pane content: {}", stderr);
    }

    let raw_content = String::from_utf8_lossy(&output.stdout).to_string();

    // Filter out Claude Code UI noise to show cleaner preview
    let filtered_content = filter_claude_ui_noise(&raw_content);

    Ok(filtered_content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_capture_options_default() {
        let opts = CaptureOptions::default();
        assert!(opts.include_escape_sequences);
        assert!(opts.join_wrapped_lines);
        assert!(opts.start_line.is_none());
        assert!(opts.end_line.is_none());
    }

    #[test]
    fn test_capture_options_visible() {
        let opts = CaptureOptions::visible();
        assert!(opts.include_escape_sequences);
        assert!(opts.join_wrapped_lines);
        assert!(opts.start_line.is_none());
        assert!(opts.end_line.is_none());
    }

    #[test]
    fn test_capture_options_full_history() {
        let opts = CaptureOptions::full_history();
        assert!(opts.include_escape_sequences);
        assert!(opts.join_wrapped_lines);
        assert_eq!(opts.start_line, Some("-".to_string()));
        assert_eq!(opts.end_line, Some("-".to_string()));
    }

    #[test]
    fn test_strip_ansi_codes_simple() {
        let input = "\x1b[38;5;123mHello\x1b[0m World";
        let expected = "Hello World";
        assert_eq!(strip_ansi_codes(input), expected);
    }

    #[test]
    fn test_strip_ansi_codes_multiple() {
        let input = "\x1b[1m\x1b[31mRed Bold\x1b[0m Normal \x1b[32mGreen\x1b[0m";
        let expected = "Red Bold Normal Green";
        assert_eq!(strip_ansi_codes(input), expected);
    }

    #[test]
    fn test_strip_ansi_codes_no_codes() {
        let input = "Plain text with no codes";
        let expected = "Plain text with no codes";
        assert_eq!(strip_ansi_codes(input), expected);
    }

    #[test]
    fn test_strip_ansi_codes_empty() {
        let input = "";
        let expected = "";
        assert_eq!(strip_ansi_codes(input), expected);
    }

    #[test]
    fn test_filter_claude_ui_noise_permission_dialog() {
        let input = r#"Some output
Do you want to work in this folder?
/Users/stevengonsalvez/.agents-in-a-box/worktrees/by-name/agents-in-a-box--session-90112e0b--89d6e40c

In order to work in this folder, we need your permission for Claude Code to read, edit, and execute files.

Yes, continue
No, exit

More output"#;
        let result = filter_claude_ui_noise(input);
        assert!(result.contains("Some output"));
        assert!(result.contains("More output"));
        assert!(!result.contains("Do you want to work"));
        assert!(!result.contains("permission"));
    }

    #[test]
    fn test_filter_claude_ui_noise_with_ansi() {
        let input = "\x1b[38;5;123mColored\x1b[0m text\nDo you want to work in this folder?\n\nMore text";
        let result = filter_claude_ui_noise(input);
        // ANSI codes are preserved (per function docstring)
        assert!(result.contains("\x1b[38;5;123mColored\x1b[0m text"));
        assert!(result.contains("More text"));
        assert!(!result.contains("Do you want to work"));
    }
}
