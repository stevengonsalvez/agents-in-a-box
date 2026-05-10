//! Host-side runtime for ainb subprocess plugins.
//!
//! Owns a tokio runtime, the per-plugin lifecycle FSM, the JSON-RPC
//! stdio client, the snapshot store, the channel registry, and the
//! request ledger. The TUI sees only [`RuntimeHandle`] — a Send + Clone
//! façade with strictly *non-blocking* surface methods (`try_recv_*`,
//! `snapshot_get`) so the ratatui render thread never `.await`s.
//!
//! ## Module map
//!
//! - [`error`]     — [`RuntimeError`] crate-wide error enum
//! - [`types`]     — [`PluginId`], [`Topic`], `RegisteredPlugin`, outcome enums
//! - [`snapshot`]  — versioned snapshot store keyed by topic
//! - [`registry`]  — action → plugin registry
//! - [`rpc`]       — JSON-RPC 2.0 envelope helpers
//! - [`framing`]   — async Content-Length frame I/O over tokio pipes
//! - [`process`]   — subprocess spawn + leak guard (Linux PDEATHSIG, macOS pgrp)
//! - [`plugin_task`] — per-plugin tokio task: reader + writer + lifecycle FSM
//! - [`runtime`]   — public [`Runtime`] entry point
//! - [`handle`]    — public [`RuntimeHandle`] sync façade

#![deny(unsafe_op_in_unsafe_fn)]

pub mod error;
pub mod framing;
pub mod handle;
pub mod plugin_task;
pub mod process;
pub mod registry;
pub mod rpc;
pub mod runtime;
pub mod snapshot;
pub mod types;

pub use error::RuntimeError;
pub use handle::RuntimeHandle;
pub use registry::RegisteredPlugin;
pub use runtime::Runtime;
pub use types::{
    CliOutcome, LifecycleState, PluginId, RenderOutcome, RuntimeConfig, Topic,
};

// Re-export wire types so callers don't need to depend on the protocol
// crate directly. Single source of truth still lives in the protocol
// crate — these are passthrough only.
pub use ainb_plugin_protocol::wire_buffer::WireBuffer;
pub use ainb_plugin_protocol::params::Viewport;
