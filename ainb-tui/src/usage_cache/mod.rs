// ABOUTME: Persistent SQLite-backed cache for parsed usage analytics.
// Per-file fingerprint + append-only incremental parse so unchanged Claude/Codex
// JSONL sessions are served instantly and changed files only re-parse new bytes.

//! Usage analytics cache.
//!
//! Refer to `~/.claude/projects/.../reference_codeburn_caching.md` for the
//! competitive analysis that motivates this module — codeburn re-parses every
//! Claude JSONL on every invocation; ainb-tui does not.
//!
//! Public surface:
//! * [`Cache`] — open, query, write, clear the on-disk cache.
//! * [`store::default_db_path`] — resolve the standard cache location.

pub mod db;
pub mod store;

#[cfg(test)]
mod tests;

pub use store::{BlobFormat, Cache, CacheError, CacheInfo};
