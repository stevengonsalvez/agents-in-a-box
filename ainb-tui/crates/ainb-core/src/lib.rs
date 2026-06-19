// ABOUTME: Library crate for ainb (Agents-in-a-Box) exposing public API for testing and external use

#![allow(missing_docs)]

pub mod agent_parsers;
pub mod agents;
pub mod app;
pub mod audit;
pub mod claude;
pub mod cli;
pub mod components;
pub mod config;
pub mod credentials;
pub mod docker;
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
pub mod tmux;
pub mod usage_cache;
pub mod widgets;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
