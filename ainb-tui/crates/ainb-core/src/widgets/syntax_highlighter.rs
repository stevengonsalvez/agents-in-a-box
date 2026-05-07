// ABOUTME: Syntax highlighting for code blocks in TUI display
// Provides ANSI color codes for different programming languages

#![allow(dead_code)]

use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;
use syntect::util::{as_24_bit_terminal_escaped, LinesWithEndings};
use lazy_static::lazy_static;

lazy_static! {
    static ref SYNTAX_SET: SyntaxSet = SyntaxSet::load_defaults_newlines();
    static ref THEME_SET: ThemeSet = ThemeSet::load_defaults();
}

/// Detect language from file extension or content
pub fn detect_language(file_path: Option<&str>, content: Option<&str>) -> Option<&'static str> {
    // Try to detect from file extension first
    if let Some(path) = file_path {
        if let Some(ext) = std::path::Path::new(path).extension() {
            let ext_str = ext.to_str().unwrap_or("").to_lowercase();
            let lang = match ext_str.as_str() {
                "rs" => "rust",
                "py" => "python",
                "js" => "javascript",
                "jsx" => "jsx",
                "ts" => "typescript",
                "tsx" => "tsx",
                "java" => "java",
                "c" => "c",
                "cpp" | "cc" | "cxx" => "cpp",
                "cs" => "csharp",
                "go" => "go",
                "rb" => "ruby",
                "php" => "php",
                "swift" => "swift",
                "kt" | "kts" => "kotlin",
                "scala" => "scala",
                "sh" | "bash" | "zsh" => "bash",
                "sql" => "sql",
                "html" | "htm" => "html",
                "css" => "css",
                "scss" | "sass" => "scss",
                "json" => "json",
                "xml" => "xml",
                "yaml" | "yml" => "yaml",
                "toml" => "toml",
                "md" | "markdown" => "markdown",
                "dockerfile" => "dockerfile",
                "makefile" => "makefile",
                _ => return None,
            };
            return Some(lang);
        }
    }

    // Try to detect from content patterns
    if let Some(text) = content {
        if text.starts_with("#!/usr/bin/env python") || text.starts_with("#!/usr/bin/python") {
            return Some("python");
        }
        if text.starts_with("#!/bin/bash") || text.starts_with("#!/bin/sh") {
            return Some("bash");
        }
        if text.starts_with("#!/usr/bin/env node") {
            return Some("javascript");
        }
        if text.contains("fn main()") && text.contains("let ") {
            return Some("rust");
        }
        if text.contains("def ") && text.contains("import ") {
            return Some("python");
        }
        if text.contains("function ") || text.contains("const ") || text.contains("var ") {
            return Some("javascript");
        }
    }

    None
}

/// Apply syntax highlighting to code and return ANSI-colored string
pub fn highlight_code(code: &str, language: Option<&str>) -> String {
    let syntax = if let Some(lang) = language {
        SYNTAX_SET.find_syntax_by_token(lang)
    } else {
        None
    }.unwrap_or_else(|| SYNTAX_SET.find_syntax_plain_text());

    let theme = &THEME_SET.themes["base16-ocean.dark"];
    let mut highlighter = HighlightLines::new(syntax, theme);

    let mut colored = String::new();
    for line in LinesWithEndings::from(code) {
        let ranges = highlighter.highlight_line(line, &SYNTAX_SET).unwrap();
        let escaped = as_24_bit_terminal_escaped(&ranges[..], false);
        colored.push_str(&escaped);
    }
    colored.push_str("\x1b[0m"); // Reset colors

    colored
}

/// Get a simple color code for a language (for basic TUI coloring)
pub fn get_language_color(language: &str) -> &'static str {
    match language {
        "rust" => "\x1b[38;5;208m",      // Orange
        "python" => "\x1b[38;5;226m",    // Yellow
        "javascript" | "typescript" => "\x1b[38;5;220m", // Gold
        "java" => "\x1b[38;5;202m",      // Red-orange
        "go" => "\x1b[38;5;51m",         // Cyan
        "ruby" => "\x1b[38;5;196m",      // Red
        "php" => "\x1b[38;5;99m",        // Purple
        "c" | "cpp" => "\x1b[38;5;33m",  // Blue
        "bash" | "sh" => "\x1b[38;5;46m", // Green
        "sql" => "\x1b[38;5;214m",       // Orange-yellow
        "html" => "\x1b[38;5;202m",      // HTML orange
        "css" => "\x1b[38;5;39m",        // CSS blue
        "json" | "yaml" | "toml" => "\x1b[38;5;35m", // Data green
        "markdown" => "\x1b[38;5;250m",  // Gray
        _ => "\x1b[38;5;255m",           // White (default)
    }
}

/// Format code block with line numbers and optional highlighting
pub fn format_code_block(
    code: &str,
    language: Option<&str>,
    start_line: usize,
    use_highlighting: bool,
) -> Vec<String> {
    let mut lines = Vec::new();

    // Apply syntax highlighting if requested
    let highlighted = if use_highlighting && language.is_some() {
        highlight_code(code, language)
    } else {
        code.to_string()
    };

    // Split into lines and add line numbers
    for (i, line) in highlighted.lines().enumerate() {
        let line_num = start_line + i;
        let formatted = if use_highlighting {
            format!("{:>4} │ {}", line_num, line)
        } else {
            // Simple format without colors
            format!("{:>4} │ {}", line_num, line)
        };
        lines.push(formatted);
    }

    lines
}

/// Create a language badge for display
pub fn language_badge(language: &str) -> String {
    let color = get_language_color(language);
    format!("{}[{}]\x1b[0m", color, language.to_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_language_from_extension() {
        assert_eq!(detect_language(Some("test.rs"), None), Some("rust"));
        assert_eq!(detect_language(Some("script.py"), None), Some("python"));
        assert_eq!(detect_language(Some("app.js"), None), Some("javascript"));
        assert_eq!(detect_language(Some("style.css"), None), Some("css"));
    }

    #[test]
    fn test_detect_language_from_content() {
        assert_eq!(
            detect_language(None, Some("#!/usr/bin/env python\nprint('hello')")),
            Some("python")
        );
        assert_eq!(
            detect_language(None, Some("fn main() {\n    let x = 5;\n}")),
            Some("rust")
        );
    }

    #[test]
    fn test_format_code_block() {
        let code = "fn main() {\n    println!(\"Hello\");\n}";
        let formatted = format_code_block(code, Some("rust"), 1, false);

        assert_eq!(formatted.len(), 3);
        assert!(formatted[0].contains("1 │"));
        assert!(formatted[1].contains("2 │"));
        assert!(formatted[2].contains("3 │"));
    }

    #[test]
    fn test_language_badge() {
        let badge = language_badge("rust");
        assert!(badge.contains("[RUST]"));
        assert!(badge.contains("\x1b[")); // Contains ANSI color code
    }
}
