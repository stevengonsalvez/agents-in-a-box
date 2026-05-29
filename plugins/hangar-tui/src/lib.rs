//! ainb Hangar plugin — TUI-first managed-agents control plane.
//!
//! P3.6 scaffold: a subprocess plugin built against
//! `ainb-plugin-sdk-rust`. `src/main.rs` runs
//! `Server::new(HangarPlugin::default()).run_stdio()`; the [`plugin`]
//! module wires the (currently stub) [`HangarPlugin`] onto the SDK's
//! `Plugin` trait. The connection state machine + daemon JSON-RPC
//! client land in P3.7.

pub mod connection;
pub mod jsonrpc_over_socket;
pub mod plugin;

pub use connection::{ConnState, Connection};
pub use plugin::{HangarPlugin, MANIFEST_TOML};
