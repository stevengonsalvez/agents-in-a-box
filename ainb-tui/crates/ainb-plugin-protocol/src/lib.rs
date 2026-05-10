//! Wire protocol for the ainb plugin runtime.
//!
//! Source-of-truth wire types shared between SDK (plugin side), runtime
//! (host side), and testkit (test side). JSON-RPC 2.0 over Content-Length
//! framed stdio. Zero host dependencies — no tokio, no ratatui, no clap.
//!
//! ## Module map
//!
//! - [`errors`]      — JSON-RPC error codes + the `RpcError` envelope and `ProtocolError`
//! - [`framing`]     — Content-Length stdio frame encode/decode
//! - [`manifest`]    — Manifest v2 schema (TOML &harr; Rust)
//! - [`methods`]     — JSON-RPC method-name constants
//! - [`params`]      — request/response param structs for every method
//! - [`wire_buffer`] — `WireBuffer` cell-based render output

pub mod errors;
pub mod framing;
pub mod manifest;
pub mod methods;
pub mod params;
pub mod wire_buffer;

pub use errors::{ProtocolError, RpcError};
pub use manifest::Manifest;
pub use wire_buffer::{Cell, Color, Coord, WireBuffer};
