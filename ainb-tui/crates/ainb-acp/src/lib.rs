//! ACP (Agent Client Protocol) client library for the hangar daemon.
//!
//! Five deep modules behind shallow interfaces, in the order a turn flows
//! through them:
//!
//! 1. [`client`] spawns an adapter process (`claude-agent-acp`, `codex-acp`)
//!    and speaks the protocol to it over stdio, using the UPSTREAM
//!    `agent-client-protocol` crate. Nothing here re-implements the wire.
//! 2. [`reducer`] turns the adapter's `session/update` stream into normalized
//!    [`reducer::TranscriptChunk`]s and extracts the turn's final message.
//! 3. [`store_writer`] batches those chunks into `fleet_provider_event` rows
//!    through the Phase 1 repo functions.
//! 4. [`reprime`] renders the resume prelude for adapters that cannot
//!    `session/load`.
//! 5. [`circuit`] is the per-provider-process crash breaker.
//!
//! ## Boundary
//!
//! This crate is a PURE LIBRARY. It holds no `EventBroker`, opens no socket
//! and issues no raw SQL (the store-fence test in `tests/store_fence.rs`
//! asserts `sqlx` never reaches this crate's dependency set). Everything that
//! needs to wake a subscriber is expressed as a returned high-water mark, so
//! the daemon's pool owns every notification and this crate could be promoted
//! to a standalone process without a redesign.

pub mod circuit;
pub mod client;
pub mod config;
pub mod reducer;
pub mod reprime;
pub mod store_writer;
