// ABOUTME: Terminal colours for parsed log levels. The parser itself lives in
// `ainb_app::components::log_parser`; only the ratatui colour mapping stays here.

pub use ainb_app::components::log_parser::*;

use ratatui::style::Color;

/// The foreground colour a parsed log line of `level` is drawn in.
#[must_use]
pub const fn level_color(level: LogLevel) -> Color {
    match level {
        LogLevel::Trace => Color::DarkGray,
        LogLevel::Debug => Color::Gray,
        LogLevel::Info => Color::Blue,
        LogLevel::Success => Color::Green,
        LogLevel::Warning => Color::Yellow,
        LogLevel::Error => Color::Red,
        LogLevel::Fatal => Color::Magenta,
    }
}
