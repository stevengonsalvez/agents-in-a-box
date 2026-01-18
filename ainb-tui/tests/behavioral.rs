// ABOUTME: Behavioral test suite for shared modules (CLI/TUI compatibility)
//
// These tests verify behavior, not implementation. They must pass against
// the current codebase BEFORE any refactoring begins.
//
// Priority order:
// - P0: tmux_lifecycle, worktree_integration (critical for CLI)
// - P1: session_persistence, git_operations (CLI/TUI compatibility)
// - P2: session_lifecycle, config_loading (completeness)

#[path = "behavioral/fixtures.rs"]
pub mod fixtures;

#[path = "behavioral/tmux_lifecycle.rs"]
mod tmux_lifecycle;

#[path = "behavioral/worktree_integration.rs"]
mod worktree_integration;

#[path = "behavioral/session_persistence.rs"]
mod session_persistence;

#[path = "behavioral/git_operations.rs"]
mod git_operations;

// P2 priority - placeholder modules
#[path = "behavioral/session_lifecycle.rs"]
mod session_lifecycle;
#[path = "behavioral/config_loading.rs"]
mod config_loading;
