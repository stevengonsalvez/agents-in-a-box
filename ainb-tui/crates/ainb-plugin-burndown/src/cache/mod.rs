// ABOUTME: No-op cache stub. The plugin's persistent SQLite cache was
// retired in Phase 3 (host pushes UsageData snapshots; plugin doesn't
// own its own DB). The surface here keeps `parse_usage_for_*` compiling.

//! Usage analytics cache — no-op stub.
//!
//! Public surface kept for API compatibility:
//! * [`Cache`] — open / disabled / get_or_parse / clear / info (all no-op).
//! * [`store::default_db_path`] — resolve the historical cache location
//!   (only used for `cache info` reporting; nothing is written there).

pub mod store;

pub use store::{BlobFormat, Cache, CacheError, CacheInfo, ParseHint, ParseResult};
