//! ainb witr plugin — wraps the `witr` CLI to surface process causality
//! tracing inside ainb's TUI.
//!
//! Architecture is a thin wrapper: the host spawns this crate's binary
//! (`ainb-plugin-witr`), the binary speaks JSON-RPC 2.0 over
//! Content-Length-framed stdio via `ainb-plugin-sdk-rust`, and on
//! demand exec's the external `witr --json <target>` binary, parses
//! stdout, and paints a `WireBuffer` for the host to composite.
//!
//! The plugin owns the full data plane (exec + parse + render +
//! publish). `ainb-core` has zero refs to witr — that's the gate that
//! makes this a real plugin rather than a host-baked feature.

pub mod detect;
pub mod exec;
pub mod model;
pub mod plugin;
pub mod render;
pub mod state;
