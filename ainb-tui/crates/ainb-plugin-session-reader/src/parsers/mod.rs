//! Per-provider JSONL parsers.
//!
//! Each provider module exposes a `parse_dir(root: &str) -> Vec<ProviderCall>`
//! entry point. The implementations are best-effort: a single broken
//! file logs a warning via `host::log` and gets skipped, but the rest
//! of the directory keeps parsing. A directory that doesn't exist or
//! is unreadable returns an empty vec without error — that's how
//! Gemini / Copilot stubs degrade cleanly when the user doesn't use
//! those providers.

pub mod claude;
pub mod codex;
pub mod copilot;
pub mod cost;
pub mod gemini;

pub(crate) use cost::estimate_cost_usd;

/// Common helper: parse an RFC 3339 timestamp into UTC.
pub(crate) fn parse_timestamp(timestamp: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .ok()
}
