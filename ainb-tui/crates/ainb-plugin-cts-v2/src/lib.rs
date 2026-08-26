//! Conformance Test Suite v2 for the ainb plugin JSON-RPC subprocess ABI.
//!
//! 18 numbered axes (`a01`-`a18`) covering manifest round-trip, framing,
//! method dispatch, capability gating, render determinism, snapshot
//! get-after-publish, snapshot subscribe, action timeout, log filtering,
//! fs path guard, graceful shutdown, crash recovery, quarantine, CLI
//! dispatch capture, event-stream subscribe, managed-subprocess spawn,
//! unix-socket dial, and secret-store get — plus the
//! `read_paths`/`[config]`, mouse-forwarding, and redraw-hint canaries,
//! for 21 in total. Keep this list in sync with `tests/canaries/`.
//!
//! Each axis consists of:
//! - A canary plugin binary under `tests/canaries/<axis>/main.rs`
//! - A host-side `#[test]` in `tests/axes.rs`

pub mod harness;
pub mod real_plugin;
pub mod wire_surface;
