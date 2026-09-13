// ABOUTME: Library crate for ainb (Agents-in-a-Box) exposing public API for testing and external use

#![allow(missing_docs)]

// The renderer-agnostic layer lives in `ainb-app`. Re-exporting it at the root
// keeps every `ainb::config::..` and `crate::config::..` path unchanged.
pub use ainb_app::*;

pub mod app;
pub mod cli;
pub mod components;
pub mod terminal_handoff;
pub mod tmux;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
