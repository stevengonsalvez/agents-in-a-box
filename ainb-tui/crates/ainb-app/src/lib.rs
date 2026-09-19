// ABOUTME: Renderer-agnostic core of Agents-in-a-Box: sessions, config, git,
// docker, tmux, fleet and the other services every surface drives. Depends on
// neither ratatui nor crossterm; `ainb-core` renders from it and re-exports it.

#![allow(missing_docs)]

pub mod agent_parsers;
pub mod agents;
pub mod app;
pub mod audit;
pub mod claude;
pub mod cli;
pub mod clipboard;
pub mod components;
pub mod config;
pub mod credentials;
pub mod docker;
pub mod docs;
pub mod editors;
pub mod fleet;
pub mod geometry;
pub mod git;
pub mod headroom;
pub mod interactive;
pub mod mcp_pool;
pub mod models;
pub mod otel;
pub mod perf;
pub mod plugins;
pub mod providers;
pub mod rtk;
pub mod self_exec_guard;
pub mod setup;
pub mod text_editor;
pub mod tmux;
pub mod usage_cache;
pub mod widgets;
pub mod wire;

pub mod env_lock;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

// The crate's own unit tests reach the scoped home guard the same way the
// integration tests do, so the guard file has one spelling for both.
#[cfg(test)]
extern crate self as ainb_app;

/// The scoped home directory guard, shared with the integration tests by file
/// so this process has one home lock rather than two.
#[cfg(test)]
#[path = "../tests/support/home.rs"]
mod test_home;

pub use app::{
    AppState, Btn, Chord, CommandId, Effect, Intent, Key, Keymap, Mods, Pos, SectionId, Versioned,
    dispatch,
};
