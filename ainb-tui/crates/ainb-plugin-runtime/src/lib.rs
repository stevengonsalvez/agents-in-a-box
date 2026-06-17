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
//! - [`event_stream`] — host-side cap-gated event-stream registry
//! - [`managed_subprocess`] — host-side cap-gated managed-subprocess registry
//! - [`unix_socket`] — host-side cap-gated unix-socket dial registry
//! - [`registry`]  — action → plugin registry
//! - [`rpc`]       — JSON-RPC 2.0 envelope helpers
//! - [`framing`]   — async Content-Length frame I/O over tokio pipes
//! - [`process`]   — subprocess spawn + leak guard (Linux PDEATHSIG, macOS pgrp)
//! - [`plugin_task`] — per-plugin tokio task: reader + writer + lifecycle FSM
//! - [`runtime`]   — public [`Runtime`] entry point
//! - [`handle`]    — public [`RuntimeHandle`] sync façade

#![deny(unsafe_op_in_unsafe_fn)]

pub mod error;
pub mod event_stream;
pub mod framing;
pub mod handle;
pub mod managed_subprocess;
pub mod plugin_task;
pub mod process;
pub mod registry;
pub mod rpc;
pub mod runtime;
pub mod secret_store;
pub mod snapshot;
pub mod types;
pub mod unix_socket;
pub mod warnings;
pub mod workspace_store;

pub use error::RuntimeError;
pub use handle::RuntimeHandle;
pub use registry::RegisteredPlugin;
pub use runtime::Runtime;
pub use types::{
    ActionOutcome, CliOutcome, LifecycleState, PluginId, RenderOutcome, RuntimeConfig, Topic,
};

// Re-export wire types so callers don't need to depend on the protocol
// crate directly. Single source of truth still lives in the protocol
// crate — these are passthrough only.
pub use ainb_plugin_protocol::params::{
    HandleKeyParams, HandleMouseParams, KEY_MOD_ALT, KEY_MOD_CTRL, KEY_MOD_SHIFT, KEY_MOD_SUPER,
    KeyCode, KeyEvent, KeyKind, MouseButton, MouseEvent, MouseKind, Viewport,
};
pub use ainb_plugin_protocol::topics;
pub use ainb_plugin_protocol::wire_buffer::WireBuffer;
