//! Conformance Test Suite v2 for the ainb plugin JSON-RPC subprocess ABI.
//!
//! 14 core axes covering manifest round-trip, framing, method dispatch,
//! capability gating, render determinism, snapshot pub/sub, action
//! timeout, log filtering, fs path guard, graceful shutdown, crash
//! recovery, quarantine, and CLI dispatch capture — plus the
//! `read_paths`/`[config]`, mouse-forwarding, and redraw-hint canaries.
//!
//! Each axis consists of:
//! - A canary plugin binary under `tests/canaries/<axis>/main.rs`
//! - A host-side `#[test]` in `tests/axes.rs`

pub mod harness;
pub mod wire_surface;
