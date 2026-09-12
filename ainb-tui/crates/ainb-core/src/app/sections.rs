// ABOUTME: The 19 sections AppState is grouped into. Each one sits behind a
// `Versioned<T>` on AppState, so any `&mut` access bumps that section alone.
//
// The grouping is the field audit from the plan (2026-09-05-desktop-p0-surface-safety.md
// at Phase 2), refreshed against the struct as it stands: three fields the plan
// named have since moved to `UiState` or gone, and seventeen that did not exist
// when it was written are placed here for the first time.

use crate::app::SessionLoader;
use crate::app::state::*;
use crate::app::versioned::Versioned;
use crate::audit::{self, AuditResult, AuditTrigger};
use crate::claude::client::ClaudeChatManager;
use crate::claude::types::ClaudeStreamingEvent;
use crate::claude::{ClaudeApiClient, ClaudeMessage};
use crate::components::home_screen_v2::HomeScreenV2State;
use crate::components::live_logs_stream::LogEntry;
use crate::config::screen_model::{self, ConfigTreeNode};
use crate::config::{AppConfig, SessionLabelStore, registry};
use crate::credentials;
use crate::docker::LogStreamingCoordinator;
use crate::fleet::attention::{Answerable, AttentionKind, SessionAttention};
use crate::models::{Session, SessionAgentType, Workspace, is_default_model};
use chrono;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tracing::{debug, error, info, trace, warn};
use uuid::Uuid;

#[derive(Debug)]
pub struct McpPoolSection {
    pub mcp_overlay: Option<McpOverlayState>,
}

impl Default for McpPoolSection {
    fn default() -> Self {
        Self { mcp_overlay: None }
    }
}

#[derive(Debug)]
pub struct RecoverySection {
    pub session_recovery_state: crate::components::SessionRecoveryState,
}

impl Default for RecoverySection {
    fn default() -> Self {
        Self {
            session_recovery_state: crate::components::SessionRecoveryState::default(),
        }
    }
}

#[derive(Debug)]
pub struct GitViewSection {
    pub git_view_state: Option<crate::components::GitViewState>,
    // Previous view for navigation (e.g., to return from GitView)
    pub quick_commit_message: Option<String>, // None = not in quick commit mode, Some = message being entered
    pub quick_commit_cursor: usize,           // Cursor position in quick commit message
    pub is_current_dir_git_repo: bool,
    // Track which session logs were last fetched to avoid unnecessary refetches
}

impl Default for GitViewSection {
    fn default() -> Self {
        Self {
            git_view_state: None,
            quick_commit_message: None,
            quick_commit_cursor: 0,

            // Initialize tmux integration
            is_current_dir_git_repo: false,
        }
    }
}

#[derive(Debug)]
pub struct ClaudeChatSection {
    pub claude_chat_visible: bool,

    // Focus management for panes
    pub claude_chat_state: Option<ClaudeChatState>,
    // Live logs from Docker containers
    pub claude_manager: Option<ClaudeChatManager>,
    // Docker log streaming coordinator
}

impl Default for ClaudeChatSection {
    fn default() -> Self {
        Self {
            claude_chat_visible: false,
            claude_chat_state: None,
            claude_manager: None,
        }
    }
}
