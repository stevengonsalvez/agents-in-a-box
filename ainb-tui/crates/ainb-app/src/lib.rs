// ABOUTME: Renderer-agnostic core of Agents-in-a-Box: sessions, config, git,
// docker, tmux, fleet and the other services every surface drives. Depends on
// neither ratatui nor crossterm; `ainb-core` renders from it and re-exports it.

#![allow(missing_docs)]

pub mod agent_parsers;
pub mod agents;
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
pub mod tmux;
pub mod usage_cache;
pub mod viewport;
pub mod widgets;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
