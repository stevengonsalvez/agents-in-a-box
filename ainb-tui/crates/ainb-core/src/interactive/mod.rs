// ABOUTME: Interactive session management module for host-based claude sessions
//
// This module provides Docker-free session management using:
// - Git worktrees for branch isolation
// - Tmux sessions for terminal multiplexing
// - Host claude CLI for interactions
//
// This is a clean alternative to Docker-based Boss mode sessions,
// enabling fast, lightweight development workflows.

pub mod session_manager;

#[allow(unused_imports)]
pub use session_manager::{InteractiveSession, InteractiveSessionManager, SessionMetadata, SessionStore};
