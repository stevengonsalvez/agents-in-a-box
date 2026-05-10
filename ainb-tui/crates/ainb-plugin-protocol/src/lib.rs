//! Wire protocol for the ainb plugin runtime.
//!
//! Source-of-truth wire types shared between SDK (plugin side), runtime
//! (host side), and testkit (test side). JSON-RPC 2.0 over Content-Length
//! framed stdio. Zero host dependencies — no tokio, no ratatui, no clap.

pub mod errors;
