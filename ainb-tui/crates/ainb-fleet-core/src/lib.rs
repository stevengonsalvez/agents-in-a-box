// ABOUTME: Fleet orchestration core — the lower half of `ainb`'s fleet stack,
// extracted so the hangar daemon can reuse it without a crate cycle.
//
// WHY THIS CRATE EXISTS
// `ainb-core` depends on `ainb-hangar-daemon` (the CLI drives `ainb daemon`),
// so the daemon can NOT depend back on `ainb-core`. The converged control
// centre (P2) needs the daemon to run the SAME needs classifier and the SAME
// verified tmux send path the TUI/CLI already use — one send path is an
// architecture invariant. Those libs therefore live HERE, below both, and are
// re-exported from `ainb-core::fleet` so every existing `crate::fleet::…` path
// in the TUI/CLI keeps compiling untouched.
//
// LAYERS (all pure libraries, no daemon/UI coupling):
// - `types`        — Session / SessionState / Signal / SendOutcome records.
// - `enrich_cache` — content-addressed drafted-suggestion cache.
// - `read`         — state signals: pane capture, error regex, JSONL tail, the
//                    needs classifier (ASK > ERR > IDLE > WAIT).
// - `send`         — verified multi-line tmux send-keys + broker fallback.
// - `discover`     — unified session enumeration (ainb + peers + bg jobs).
//
// The inner `fleet` module keeps the ORIGINAL path shape (`crate::fleet::…`) so
// the moved files needed no edits; the top-level re-exports below give callers
// the flatter `ainb_fleet_core::{read,send,discover,types,enrich_cache}` names.

#![allow(missing_docs)]

pub mod fleet;

pub use fleet::{
    discover, enrich_cache, read, repo_clone, repo_roster, send, session_registry, types,
};
