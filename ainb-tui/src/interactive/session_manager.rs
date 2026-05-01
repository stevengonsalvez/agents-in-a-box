// ABOUTME: Interactive session manager for host-based Docker-free sessions
//
// Manages the lifecycle of Interactive mode sessions which run directly on the host:
// - Creates git worktrees for branch isolation
// - Starts tmux sessions for terminal multiplexing
// - Runs claude CLI directly on the host
// - Discovers existing sessions by scanning tmux
//
// This manager is completely independent of Docker and ContainerManager,
// enabling lightweight, fast development workflows.

#![allow(dead_code)]

use crate::audit::{self, AuditResult, AuditTrigger};
use crate::git::WorktreeManager;
use crate::models::{ClaudeModel, Session, SessionAgentType, SessionMode, SessionStatus};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use thiserror::Error;
use tokio::process::Command;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

#[derive(Error, Debug)]
pub enum InteractiveSessionError {
    #[error("Worktree error: {0}")]
    Worktree(#[from] crate::git::WorktreeError),

    #[error("Tmux error: {0}")]
    Tmux(String),

    #[error("Session not found: {0}")]
    SessionNotFound(Uuid),

    #[error("Session already exists: {0}")]
    SessionAlreadyExists(Uuid),

    #[error("Invalid session state: {0}")]
    InvalidState(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Error: {0}")]
    Other(#[from] anyhow::Error),
}

/// Represents an active Interactive mode session
#[derive(Debug, Clone)]
pub struct InteractiveSession {
    pub session_id: Uuid,
    pub worktree_path: PathBuf,
    pub source_repository: PathBuf, // The original git repository path
    pub tmux_session_name: String,
    pub branch_name: String,
    pub workspace_name: String,
    pub created_at: DateTime<Utc>,
    pub agent_type: SessionAgentType, // The AI agent or shell for this session
    pub model: Option<ClaudeModel>,   // Claude model for this session (only for Claude agent)
}

/// Persisted session metadata for discovery across restarts
///
/// This solves the branch-mismatch problem: when a user changes branches in a worktree,
/// the old tmux session name no longer matches the current branch. By persisting the
/// mapping between session_id, tmux_session_name, and worktree_path, we can reliably
/// rediscover sessions even after branch changes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMetadata {
    pub session_id: Uuid,
    pub tmux_session_name: String,
    pub worktree_path: PathBuf,
    pub workspace_name: String,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub agent_type: SessionAgentType,
}

/// Storage for all persisted session metadata
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct SessionStore {
    pub sessions: HashMap<String, SessionMetadata>, // keyed by tmux_session_name
}

impl SessionStore {
    /// Load session store from disk
    pub fn load() -> Self {
        let path = Self::storage_path();
        if !path.exists() {
            debug!("No sessions.json found at {:?}, returning empty store", path);
            return Self::default();
        }

        match std::fs::read_to_string(&path) {
            Ok(content) => match serde_json::from_str::<SessionStore>(&content) {
                Ok(store) => {
                    debug!("Loaded {} sessions from {:?}", store.sessions.len(), path);
                    store
                }
                Err(e) => {
                    warn!("Failed to parse sessions.json: {}, returning empty store", e);
                    Self::default()
                }
            },
            Err(e) => {
                warn!("Failed to read sessions.json: {}, returning empty store", e);
                Self::default()
            }
        }
    }

    /// Save session store to disk
    pub fn save(&self) -> Result<(), std::io::Error> {
        let path = Self::storage_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let content = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        std::fs::write(&path, content)?;
        debug!("Saved {} sessions to {:?}", self.sessions.len(), path);
        Ok(())
    }

    /// Get the storage file path
    pub fn storage_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".agents-in-a-box")
            .join("sessions.json")
    }

    /// Add or update a session
    pub fn upsert(&mut self, metadata: SessionMetadata) {
        self.sessions.insert(metadata.tmux_session_name.clone(), metadata);
    }

    /// Remove a session by tmux name
    pub fn remove_by_tmux_name(&mut self, tmux_name: &str) {
        self.sessions.remove(tmux_name);
    }

    /// Remove a session by session_id
    pub fn remove_by_session_id(&mut self, session_id: Uuid) {
        self.sessions.retain(|_, v| v.session_id != session_id);
    }

    /// Find session by tmux name
    pub fn find_by_tmux_name(&self, tmux_name: &str) -> Option<&SessionMetadata> {
        self.sessions.get(tmux_name)
    }

    /// Get an iterator over all tracked sessions (tmux_name -> metadata)
    pub fn sessions(&self) -> &HashMap<String, SessionMetadata> {
        &self.sessions
    }

    /// Get all tmux session names that are tracked
    pub fn tracked_tmux_names(&self) -> Vec<&str> {
        self.sessions.keys().map(|s| s.as_str()).collect()
    }
}

/// Manager for Interactive mode sessions (host-based, no Docker)
pub struct InteractiveSessionManager {
    worktree_manager: WorktreeManager,
    active_sessions: HashMap<Uuid, InteractiveSession>,
}

impl InteractiveSessionManager {
    /// Create a new Interactive session manager
    ///
    /// NOTE: This does NOT require Docker, unlike SessionLifecycleManager
    pub fn new() -> Result<Self, InteractiveSessionError> {
        let worktree_manager = WorktreeManager::new()
            .map_err(|e| InteractiveSessionError::InvalidState(format!("Failed to create worktree manager: {}", e)))?;

        Ok(Self {
            worktree_manager,
            active_sessions: HashMap::new(),
        })
    }

    /// Create a new Interactive session with worktree and tmux
    ///
    /// # Arguments
    /// * `session_id` - Unique identifier for the session
    /// * `workspace_name` - Name of the workspace
    /// * `workspace_path` - Path to the git repository
    /// * `branch_name` - Branch name to create worktree for
    /// * `base_branch` - Optional base branch to branch from
    ///
    /// # Returns
    /// * `Result<InteractiveSession>` - The created session or an error
    pub async fn create_session(
        &mut self,
        session_id: Uuid,
        workspace_name: String,
        workspace_path: PathBuf,
        branch_name: String,
        base_branch: Option<String>,
        skip_permissions: bool,
        agent_type: SessionAgentType,
        model: Option<ClaudeModel>,
    ) -> Result<InteractiveSession, InteractiveSessionError> {
        info!(
            "Creating Interactive session {} for branch '{}' in workspace '{}' (agent={:?}, model={:?}, skip_permissions={})",
            session_id, branch_name, workspace_name, agent_type, model, skip_permissions
        );

        // Check if session already exists
        if self.active_sessions.contains_key(&session_id) {
            return Err(InteractiveSessionError::SessionAlreadyExists(session_id));
        }

        // Step 1: Create git worktree
        info!("Creating worktree for branch '{}'", branch_name);
        let worktree_info = self.worktree_manager.create_worktree(
            session_id,
            &workspace_path,
            &branch_name,
            base_branch.as_deref(),
        )?;

        info!("Created worktree at: {}", worktree_info.path.display());

        // Step 2: Create tmux session name (format: tmux_{folder}_{branch})
        let worktree_folder = Self::extract_worktree_folder(&worktree_info.path);
        let tmux_session_name = Self::generate_tmux_name(&worktree_folder, &branch_name);

        // Step 3: Start tmux session
        info!("Starting tmux session: {}", tmux_session_name);
        self.start_tmux_session(&tmux_session_name, &worktree_info.path).await?;

        // Step 4: Start CLI in tmux session (for AI agent types)
        match agent_type {
            SessionAgentType::Claude | SessionAgentType::Codex | SessionAgentType::Gemini | SessionAgentType::Copilot => {
                info!("Starting {:?} CLI in tmux session (model={:?}, skip_permissions={})", agent_type, model, skip_permissions);
                self.start_cli_in_tmux(&tmux_session_name, skip_permissions, model, agent_type, None).await?;
            }
            _ => {
                info!("Skipping CLI for agent type: {:?}", agent_type);
            }
        }

        // Step 5: Create session record
        let created_at = Utc::now();
        let session = InteractiveSession {
            session_id,
            worktree_path: worktree_info.path.clone(),
            source_repository: worktree_info.source_repository.clone(),
            tmux_session_name: tmux_session_name.clone(),
            branch_name: branch_name.clone(),
            workspace_name: workspace_name.clone(),
            created_at,
            agent_type,
            model,
        };

        self.active_sessions.insert(session_id, session.clone());

        // Step 6: Persist session metadata to sessions.json for discovery across restarts
        let metadata = SessionMetadata {
            session_id,
            tmux_session_name: tmux_session_name.clone(),
            worktree_path: worktree_info.path.clone(),
            workspace_name: workspace_name.clone(),
            created_at,
            agent_type,
        };
        let mut store = SessionStore::load();
        store.upsert(metadata);
        if let Err(e) = store.save() {
            warn!("Failed to persist session metadata: {}", e);
            // Continue anyway - session is still usable, just won't survive restarts gracefully
        }

        info!("Successfully created Interactive session {}", session_id);

        // Audit log the session creation
        audit::audit_session_created(
            session_id,
            &tmux_session_name,
            &worktree_info.path.display().to_string(),
            &branch_name,
            AuditTrigger::UserKeypress("Enter".to_string()),
            AuditResult::Success,
        );

        Ok(session)
    }

    /// Create an Interactive session using an existing worktree
    ///
    /// This is used for remote repository flows where the worktree has already been
    /// created from the bare cache. Unlike `create_session()`, this skips worktree creation.
    ///
    /// # Arguments
    /// * `session_id` - Unique identifier for the session
    /// * `workspace_name` - Name of the workspace (for display)
    /// * `existing_worktree_path` - Path to the already-created worktree
    /// * `source_repo_path` - Path to the source repository (bare cache for remote repos)
    /// * `branch_name` - Branch name for the session
    /// * `skip_permissions` - Whether to skip permission prompts in claude CLI
    /// * `agent_type` - Type of agent (Claude, Shell, etc.)
    /// * `model` - Claude model to use (only for Claude agent)
    ///
    /// # Returns
    /// * `Result<InteractiveSession>` - The created session or an error
    pub async fn create_session_with_worktree(
        &mut self,
        session_id: Uuid,
        workspace_name: String,
        existing_worktree_path: PathBuf,
        source_repo_path: PathBuf,
        branch_name: String,
        skip_permissions: bool,
        agent_type: SessionAgentType,
        model: Option<ClaudeModel>,
    ) -> Result<InteractiveSession, InteractiveSessionError> {
        info!(
            "Creating Interactive session {} with existing worktree at '{}' (agent={:?}, model={:?})",
            session_id, existing_worktree_path.display(), agent_type, model
        );

        // Check if session already exists
        if self.active_sessions.contains_key(&session_id) {
            return Err(InteractiveSessionError::SessionAlreadyExists(session_id));
        }

        // Verify the worktree exists
        if !existing_worktree_path.exists() {
            return Err(InteractiveSessionError::Worktree(
                crate::git::WorktreeError::NotFound(existing_worktree_path.display().to_string())
            ));
        }

        info!("Using existing worktree at: {}", existing_worktree_path.display());

        // Create session-based symlink for easy lookup
        let session_path = self.worktree_manager.base_dir().join("by-session").join(session_id.to_string());
        if !session_path.exists() {
            if let Some(parent) = session_path.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            #[cfg(unix)]
            std::os::unix::fs::symlink(&existing_worktree_path, &session_path).ok();
        }

        // Step 1: Create tmux session name (format: tmux_{folder}_{branch})
        let worktree_folder = Self::extract_worktree_folder(&existing_worktree_path);
        let tmux_session_name = Self::generate_tmux_name(&worktree_folder, &branch_name);

        // Step 2: Start tmux session
        info!("Starting tmux session: {}", tmux_session_name);
        self.start_tmux_session(&tmux_session_name, &existing_worktree_path).await?;

        // Step 3: Start CLI in tmux session (for AI agent types)
        match agent_type {
            SessionAgentType::Claude | SessionAgentType::Codex | SessionAgentType::Gemini | SessionAgentType::Copilot => {
                info!("Starting {:?} CLI in tmux session (model={:?}, skip_permissions={})", agent_type, model, skip_permissions);
                self.start_cli_in_tmux(&tmux_session_name, skip_permissions, model, agent_type, None).await?;
            }
            _ => {
                info!("Skipping CLI for agent type: {:?}", agent_type);
            }
        }

        // Step 4: Create session record
        let created_at = Utc::now();
        let worktree_path_clone = existing_worktree_path.clone();
        let session = InteractiveSession {
            session_id,
            worktree_path: existing_worktree_path,
            source_repository: source_repo_path,
            tmux_session_name: tmux_session_name.clone(),
            branch_name: branch_name.clone(),
            workspace_name: workspace_name.clone(),
            created_at,
            agent_type,
            model,
        };

        self.active_sessions.insert(session_id, session.clone());

        // Step 5: Persist session metadata to sessions.json for discovery across restarts
        let metadata = SessionMetadata {
            session_id,
            tmux_session_name: tmux_session_name.clone(),
            worktree_path: worktree_path_clone,
            workspace_name: workspace_name.clone(),
            created_at,
            agent_type,
        };
        let mut store = SessionStore::load();
        store.upsert(metadata);
        if let Err(e) = store.save() {
            warn!("Failed to persist session metadata: {}", e);
        }

        info!("Successfully created Interactive session {} with existing worktree", session_id);

        // Audit log the session creation
        audit::audit_session_created(
            session_id,
            &tmux_session_name,
            &session.worktree_path.display().to_string(),
            &branch_name,
            AuditTrigger::UserKeypress("Enter".to_string()),
            AuditResult::Success,
        );

        Ok(session)
    }

    /// Discover and list all active Interactive sessions by scanning tmux
    ///
    /// This enables stateless recovery - we can discover sessions created in
    /// previous app instances by matching tmux session names to worktrees.
    ///
    /// # Returns
    /// * `Result<Vec<InteractiveSession>>` - List of discovered sessions
    pub async fn list_sessions(&mut self) -> Result<Vec<InteractiveSession>, InteractiveSessionError> {
        info!("Discovering Interactive sessions from tmux");

        // Get all tmux sessions
        let output = Command::new("tmux")
            .args(["list-sessions", "-F", "#{session_name}"])
            .output()
            .await?;

        if !output.status.success() {
            // No tmux server running or no sessions
            debug!("No tmux sessions found (tmux might not be running)");
            return Ok(Vec::new());
        }

        let tmux_sessions = String::from_utf8_lossy(&output.stdout);
        let mut discovered_sessions = Vec::new();

        // Filter for our tmux sessions (prefix: tmux_)
        for tmux_name in tmux_sessions.lines() {
            if !tmux_name.starts_with("tmux_") {
                continue;
            }

            debug!("Found tmux session: {}", tmux_name);

            // Try to find corresponding worktree
            if let Ok(session) = self.discover_session_from_tmux(tmux_name).await {
                discovered_sessions.push(session);
            }
        }

        info!("Discovered {} Interactive sessions", discovered_sessions.len());
        Ok(discovered_sessions)
    }

    /// Discover a session from a tmux session name
    ///
    /// Uses a two-phase approach:
    /// 1. First, try to find the session in sessions.json (handles branch-mismatch case)
    /// 2. If not found, fall back to reverse-engineering the branch name from tmux session name
    async fn discover_session_from_tmux(&self, tmux_name: &str) -> Result<InteractiveSession, InteractiveSessionError> {
        // Phase 1: Try to find session in persisted sessions.json
        // This handles the branch-mismatch case where the user changed branches in the worktree
        let store = SessionStore::load();
        if let Some(metadata) = store.find_by_tmux_name(tmux_name) {
            // Verify the worktree still exists
            if metadata.worktree_path.exists() {
                debug!("Found session {} in sessions.json for tmux {}", metadata.session_id, tmux_name);

                // Try to get current branch name from the worktree
                let branch_name = self.get_current_branch(&metadata.worktree_path)
                    .unwrap_or_else(|| "unknown".to_string());

                // Try to get source repository from worktree
                let source_repository = Self::get_source_repository(&metadata.worktree_path)
                    .unwrap_or_else(|| metadata.worktree_path.clone());

                // Use persisted agent_type, fall back to tmux process detection
                let agent_type = if metadata.agent_type == SessionAgentType::Claude {
                    // Could be a real Claude session or a legacy default — try detecting from tmux
                    Self::detect_agent_from_tmux(tmux_name).await.unwrap_or(metadata.agent_type)
                } else {
                    metadata.agent_type
                };

                // Re-derive workspace_name from current paths rather than trusting
                // the persisted value — older sessions.json entries may carry the
                // full sanitized worktree dir name as workspace_name.
                let workspace_name = Self::derive_workspace_name(
                    &metadata.worktree_path,
                    &source_repository,
                );

                return Ok(InteractiveSession {
                    session_id: metadata.session_id,
                    worktree_path: metadata.worktree_path.clone(),
                    source_repository,
                    tmux_session_name: tmux_name.to_string(),
                    branch_name,
                    workspace_name,
                    created_at: metadata.created_at,
                    agent_type,
                    model: None,
                });
            } else {
                debug!("Session {} in sessions.json but worktree no longer exists at {:?}",
                       metadata.session_id, metadata.worktree_path);
            }
        }

        // Phase 2: Fall back to branch-name matching (original logic)
        // Remove "tmux_" prefix and reverse sanitization
        let sanitized = tmux_name.strip_prefix("tmux_").unwrap_or(tmux_name);
        let branch_guess = sanitized.replace('_', "/");

        // Try to find worktree with matching branch
        // Use list_all_worktrees() which scans by-session directory with UUID symlinks
        let worktrees = self.worktree_manager.list_all_worktrees()
            .map_err(|e| InteractiveSessionError::InvalidState(format!("Failed to list worktrees: {}", e)))?;

        for (session_id, worktree) in worktrees {
            // Try matching both new format (tmux_{folder}_{branch}) and legacy format (tmux_{branch})
            let worktree_folder = Self::extract_worktree_folder(&worktree.path);
            let matches_new_format = Self::generate_tmux_name(&worktree_folder, &worktree.branch_name) == tmux_name;
            let matches_legacy_format = Self::generate_tmux_name_legacy(&worktree.branch_name) == tmux_name;
            let matches_branch_guess = worktree.branch_name.contains(&branch_guess);

            if matches_new_format || matches_legacy_format || matches_branch_guess {

                let workspace_name = Self::derive_workspace_name(
                    &worktree.path,
                    &worktree.source_repository,
                );

                // Try to detect agent from tmux process, default to Claude
                let agent_type = Self::detect_agent_from_tmux(tmux_name).await
                    .unwrap_or(SessionAgentType::Claude);

                return Ok(InteractiveSession {
                    session_id,
                    worktree_path: worktree.path,
                    source_repository: worktree.source_repository,
                    tmux_session_name: tmux_name.to_string(),
                    branch_name: worktree.branch_name,
                    workspace_name,
                    created_at: Utc::now(),
                    agent_type,
                    model: None,
                });
            }
        }

        Err(InteractiveSessionError::InvalidState(
            format!("No matching worktree found for tmux session {}", tmux_name)
        ))
    }

    /// Detect agent type by inspecting the running process in a tmux session
    ///
    /// Runs `tmux list-panes -t <session> -F '#{pane_current_command}'` to get the
    /// active process, then matches it against known CLI commands.
    async fn detect_agent_from_tmux(tmux_name: &str) -> Option<SessionAgentType> {
        let output = Command::new("tmux")
            .args(["list-panes", "-t", tmux_name, "-F", "#{pane_current_command}"])
            .output()
            .await
            .ok()?;

        if !output.status.success() {
            return None;
        }

        let commands = String::from_utf8_lossy(&output.stdout);
        for line in commands.lines() {
            let cmd = line.trim().to_lowercase();
            if cmd.contains("claude") {
                return Some(SessionAgentType::Claude);
            } else if cmd.contains("codex") {
                return Some(SessionAgentType::Codex);
            } else if cmd.contains("gemini") {
                return Some(SessionAgentType::Gemini);
            } else if cmd.contains("copilot") {
                return Some(SessionAgentType::Copilot);
            }
        }

        None
    }

    /// Get the current branch name from a worktree path
    fn get_current_branch(&self, worktree_path: &Path) -> Option<String> {
        use git2::Repository;

        let repo = Repository::open(worktree_path).ok()?;
        let head = repo.head().ok()?;

        if head.is_branch() {
            head.shorthand().map(|s| s.to_string())
        } else {
            // Detached HEAD - get the commit hash
            head.target().map(|oid| oid.to_string()[..8].to_string())
        }
    }

    /// Derive a workspace display name from a worktree path.
    ///
    /// Why: only the legacy `<repo>--<hash>--<session>` worktree layout encodes
    /// the repo in the directory name. Newer flat-format paths (no `--`) made
    /// `path.split("--").next()` return the entire directory, producing bogus
    /// workspace groups like `shotclubhouse_shotclubhouse_temp_debug`.
    pub fn derive_workspace_name(worktree_path: &Path, source_repository: &Path) -> String {
        let from_dir = worktree_path
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|name| name.contains("--"))
            .and_then(|name| name.split("--").next());

        from_dir
            .or_else(|| {
                source_repository
                    .file_name()
                    .and_then(|n| n.to_str())
            })
            .unwrap_or("unknown")
            .to_string()
    }

    /// Get the source repository path from a worktree
    pub fn get_source_repository(worktree_path: &Path) -> Option<PathBuf> {
        // Read the .git file in the worktree to find the main repo
        let git_file = worktree_path.join(".git");
        if !git_file.exists() || !git_file.is_file() {
            return None;
        }

        let content = std::fs::read_to_string(&git_file).ok()?;
        // Format: "gitdir: /path/to/main/repo/.git/worktrees/name"
        let gitdir = content.trim().strip_prefix("gitdir: ")?;

        // Navigate from .git/worktrees/name to the main repo
        let worktree_git_path = PathBuf::from(gitdir);
        let main_git = worktree_git_path.parent()?.parent()?.parent()?;

        // The main git dir might be .git or bare repo
        if main_git.file_name()? == ".git" {
            main_git.parent().map(|p| p.to_path_buf())
        } else {
            Some(main_git.to_path_buf())
        }
    }

    /// Remove an Interactive session (cleanup tmux and worktree)
    ///
    /// # Arguments
    /// * `session_id` - UUID of the session to remove
    ///
    /// # Returns
    /// * `Result<()>` - Success or an error
    pub async fn remove_session(&mut self, session_id: Uuid) -> Result<(), InteractiveSessionError> {
        info!(">>> InteractiveSessionManager::remove_session() START: {}", session_id);

        // Try to get session from active_sessions first
        let session_opt = self.active_sessions.remove(&session_id);
        info!("Session in active_sessions: {}", session_opt.is_some());

        // Step 1: Kill tmux session
        // If we have the session in memory, use its tmux_session_name
        // Otherwise, try to find it by discovering from worktree
        let tmux_session_name = if let Some(ref session) = session_opt {
            info!("Using tmux session name from memory: {}", session.tmux_session_name);
            session.tmux_session_name.clone()
        } else {
            // Try to get worktree info and derive tmux session name
            info!("Session not in memory, discovering from worktree");
            match self.worktree_manager.get_worktree_info(session_id) {
                Ok(worktree) => {
                    info!("Found worktree with branch: {}", worktree.branch_name);
                    let worktree_folder = Self::extract_worktree_folder(&worktree.path);
                    let tmux_name = Self::generate_tmux_name(&worktree_folder, &worktree.branch_name);
                    let legacy_name = Self::generate_tmux_name_legacy(&worktree.branch_name);
                    // Check if new format session exists, otherwise try legacy
                    let check_new = std::process::Command::new("tmux")
                        .args(["has-session", "-t", &tmux_name])
                        .output();
                    let final_name = if check_new.map(|o| o.status.success()).unwrap_or(false) {
                        info!("Found tmux session with new format: {}", tmux_name);
                        tmux_name
                    } else {
                        info!("Trying legacy tmux session name: {}", legacy_name);
                        legacy_name
                    };
                    final_name
                }
                Err(e) => {
                    // Couldn't find worktree, can't determine tmux session name
                    error!("Could not find worktree for session {}: {}", session_id, e);
                    // Still try to remove worktree in case it exists
                    if let Err(remove_err) = self.worktree_manager.remove_worktree(session_id) {
                        warn!("Failed to remove worktree: {}", remove_err);
                    }
                    return Err(InteractiveSessionError::SessionNotFound(session_id));
                }
            }
        };

        info!("Attempting to kill tmux session: {}", tmux_session_name);
        let output = Command::new("tmux")
            .args(["kill-session", "-t", &tmux_session_name])
            .output()
            .await?;

        if output.status.success() {
            info!("Successfully killed tmux session: {}", tmux_session_name);
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!("Failed to kill tmux session '{}': {}", tmux_session_name, stderr);
            // Continue anyway - session might already be dead
        }

        // Step 2: Remove worktree
        info!("Attempting to remove worktree for session {}", session_id);
        match self.worktree_manager.remove_worktree(session_id) {
            Ok(()) => info!("Successfully removed worktree for session {}", session_id),
            Err(e) => {
                error!("Failed to remove worktree for session {}: {}", session_id, e);
                return Err(e.into());
            }
        }

        // Step 3: Remove from sessions.json
        let mut store = SessionStore::load();
        store.remove_by_tmux_name(&tmux_session_name);
        store.remove_by_session_id(session_id); // Also remove by ID in case tmux name changed
        if let Err(e) = store.save() {
            warn!("Failed to update sessions.json after removal: {}", e);
            // Continue anyway - removal was successful
        }

        info!("<<< InteractiveSessionManager::remove_session() COMPLETE: {}", session_id);
        Ok(())
    }

    /// Check if a session is still alive (tmux session exists)
    ///
    /// # Arguments
    /// * `session_id` - UUID of the session to check
    ///
    /// # Returns
    /// * `Result<bool>` - True if session is alive, false otherwise
    pub async fn is_session_alive(&self, session_id: Uuid) -> Result<bool, InteractiveSessionError> {
        let session = self.active_sessions
            .get(&session_id)
            .ok_or(InteractiveSessionError::SessionNotFound(session_id))?;

        let output = Command::new("tmux")
            .args(["has-session", "-t", &session.tmux_session_name])
            .output()
            .await?;

        Ok(output.status.success())
    }

    /// Get a session by ID
    pub fn get_session(&self, session_id: Uuid) -> Option<&InteractiveSession> {
        self.active_sessions.get(&session_id)
    }

    /// Get all active sessions
    pub fn get_all_sessions(&self) -> Vec<&InteractiveSession> {
        self.active_sessions.values().collect()
    }

    // ===== Private Helper Methods =====

    /// Generate a tmux session name from worktree folder and branch name
    ///
    /// Format: tmux_{folder}_{branch}
    /// Sanitizes both folder and branch to be tmux-compatible
    fn generate_tmux_name(worktree_folder: &str, branch_name: &str) -> String {
        let sanitized_folder = worktree_folder
            .replace(' ', "_")
            .replace('.', "_")
            .replace('/', "_")
            .replace(':', "_");
        let sanitized_branch = branch_name
            .replace(' ', "_")
            .replace('.', "_")
            .replace('/', "_")
            .replace(':', "_");
        format!("tmux_{}_{}", sanitized_folder, sanitized_branch)
    }

    /// Generate legacy tmux session name (branch only) for backwards compatibility
    fn generate_tmux_name_legacy(branch_name: &str) -> String {
        let sanitized = branch_name
            .replace(' ', "_")
            .replace('.', "_")
            .replace('/', "_")
            .replace(':', "_");
        format!("tmux_{}", sanitized)
    }

    /// Extract folder name from a worktree path
    fn extract_worktree_folder(path: &Path) -> String {
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("session")
            .to_string()
    }

    /// Start a new tmux session
    async fn start_tmux_session(&self, session_name: &str, work_dir: &Path) -> Result<(), InteractiveSessionError> {
        // Check if session already exists
        let check = Command::new("tmux")
            .args(["has-session", "-t", session_name])
            .output()
            .await?;

        if check.status.success() {
            warn!("Tmux session '{}' already exists, killing it first", session_name);
            Command::new("tmux")
                .args(["kill-session", "-t", session_name])
                .output()
                .await?;
        }

        // Create new detached tmux session
        let output = Command::new("tmux")
            .args([
                "new-session",
                "-d",              // Detached
                "-s", session_name,
                "-c", work_dir.to_str().context("Invalid work directory path")?,
                "-x", "120",       // Width
                "-y", "40",        // Height
            ])
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(InteractiveSessionError::Tmux(
                format!("Failed to create tmux session '{}': {}", session_name, stderr)
            ));
        }

        // Configure tmux session
        self.configure_tmux_session(session_name).await?;

        info!("Started tmux session: {}", session_name);
        Ok(())
    }

    /// Configure tmux session settings
    async fn configure_tmux_session(&self, session_name: &str) -> Result<(), InteractiveSessionError> {
        // Set history limit
        Command::new("tmux")
            .args([
                "set-option", "-t", session_name,
                "history-limit", "50000"
            ])
            .status()
            .await?;

        // Enable mouse scrolling
        Command::new("tmux")
            .args([
                "set-option", "-t", session_name,
                "mouse", "on"
            ])
            .status()
            .await?;

        // Configure clipboard integration
        crate::tmux::configure_clipboard(session_name)
            .await
            .map_err(|e| InteractiveSessionError::Tmux(format!("Failed to configure clipboard: {}", e)))?;

        // macOS: Configure reattach-to-user-namespace for audio/clipboard access
        // Uses centralized function with shell validation and proper error handling
        if let Err(e) = crate::tmux::configure_macos_user_namespace(session_name).await {
            warn!(
                "Failed to configure macOS user namespace for session {}: {}",
                session_name,
                e
            );
            // Continue anyway - this is optional functionality
        }

        Ok(())
    }

    /// Wait for the shell prompt to be ready in a tmux session
    ///
    /// Polls the tmux pane content until a shell prompt character appears,
    /// indicating the shell has initialized and is ready to receive commands.
    async fn wait_for_shell_ready(&self, session_name: &str) -> Result<(), InteractiveSessionError> {
        use tokio::time::{sleep, Duration};

        debug!("Waiting for shell prompt in session {}", session_name);

        // Wait up to 3 seconds for shell to initialize (30 * 100ms)
        for attempt in 0..30 {
            // Capture the pane content - target pane explicitly with :0
            let output = Command::new("tmux")
                .args(["capture-pane", "-t", &format!("{}:0", session_name), "-p"])
                .output()
                .await?;

            let content = String::from_utf8_lossy(&output.stdout);

            // Check for common shell prompt indicators ($ % > #)
            // These typically appear at the end of the prompt when shell is ready
            if content.contains('$') || content.contains('%') || content.contains('>') || content.contains('#') {
                debug!("Shell prompt detected in session {} after {} attempts", session_name, attempt + 1);
                return Ok(());
            }

            sleep(Duration::from_millis(100)).await;
        }

        // Proceed anyway after timeout - shell might be ready without standard prompt
        warn!("Timeout waiting for shell prompt in session {}, proceeding anyway", session_name);
        Ok(())
    }

    /// Start AI CLI in the tmux session (Claude, Codex, or Gemini)
    ///
    /// `resume_transcript`, when supplied for `agent_type == Claude`, appends
    /// `--resume <path>` to the CLI invocation. Mirrors `spawn-agent-lib.sh:508`.
    /// Ignored for other agent types since they have no equivalent flag yet.
    pub async fn start_cli_in_tmux(
        &self,
        session_name: &str,
        skip_permissions: bool,
        model: Option<ClaudeModel>,
        agent_type: SessionAgentType,
        resume_transcript: Option<PathBuf>,
    ) -> Result<(), InteractiveSessionError> {
        use crate::config::CliProvider;

        // Wait for shell to be ready before sending command
        // This prevents the race condition where send-keys fires before shell initializes
        self.wait_for_shell_ready(session_name).await?;

        // Build environment setup for API key injection
        let env_setup = Self::build_env_setup_for_provider(agent_type);

        // Determine CLI provider from agent type
        let provider = match agent_type {
            SessionAgentType::Claude => CliProvider::Claude,
            SessionAgentType::Codex => CliProvider::Codex,
            SessionAgentType::Gemini => CliProvider::Gemini,
            SessionAgentType::Copilot => CliProvider::Copilot,
            _ => return Ok(()), // Shell and other types don't need CLI
        };

        // Build the CLI command with appropriate flags
        let mut cmd_parts = vec![provider.command().to_string()];

        // Add model flag if specified (Claude-specific for now)
        // TODO: Add model selection support for Codex and Gemini
        if agent_type == SessionAgentType::Claude {
            if let Some(m) = model {
                cmd_parts.push("--model".to_string());
                cmd_parts.push(m.cli_value().to_string());
            }
        }

        // Add skip permissions flag if specified (provider-specific)
        if skip_permissions {
            cmd_parts.push(provider.skip_permissions_flag().to_string());
        }

        // Resume only supported by Claude today; other CLIs simply start fresh.
        if agent_type == SessionAgentType::Claude {
            if let Some(path) = resume_transcript.as_ref() {
                cmd_parts.push("--resume".to_string());
                cmd_parts.push(
                    path.to_str()
                        .ok_or_else(|| {
                            InteractiveSessionError::Tmux(format!(
                                "Resume transcript path is not valid UTF-8: {:?}",
                                path
                            ))
                        })?
                        .to_string(),
                );
            }
        }

        let cli_cmd = cmd_parts.join(" ");
        let full_cmd = format!("{}{}", env_setup, cli_cmd);

        info!("Starting {} with command: {}", provider.display_name(), cli_cmd);

        // Send command to tmux to start CLI - target pane explicitly with :0
        let target = format!("{}:0", session_name);
        let output = Command::new("tmux")
            .args([
                "send-keys", "-t", &target,
                &full_cmd, "C-m"  // C-m = Enter key
            ])
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(InteractiveSessionError::Tmux(
                format!("Failed to start {} in tmux: {}", provider.display_name(), stderr)
            ));
        }

        info!(
            "Started {} CLI in tmux session: {} (model={:?}, skip_permissions={})",
            provider.display_name(), session_name, model, skip_permissions
        );
        Ok(())
    }

    /// Build environment setup for injecting API key if using ApiKey auth mode
    fn build_env_setup() -> String {
        use crate::config::{AppConfig, ClaudeAuthProvider};
        use crate::credentials;

        // Check auth provider from config
        let auth_provider = AppConfig::load()
            .map(|c| c.authentication.claude_provider.clone())
            .unwrap_or(ClaudeAuthProvider::SystemAuth);

        // Only inject API key if using ApiKey auth mode (not Pro/Max subscription)
        if matches!(auth_provider, ClaudeAuthProvider::ApiKey) {
            if let Ok(Some(api_key)) = credentials::get_anthropic_api_key() {
                info!("Injecting ANTHROPIC_API_KEY for API key auth mode");
                return format!("export ANTHROPIC_API_KEY='{}' && ", api_key);
            } else {
                warn!("ApiKey auth mode configured but no API key found in keychain");
            }
        }

        String::new()
    }

    /// Build environment setup for injecting API key based on provider
    fn build_env_setup_for_provider(agent_type: SessionAgentType) -> String {
        use crate::credentials;

        match agent_type {
            SessionAgentType::Claude => {
                // Use existing logic for Claude
                Self::build_env_setup()
            }
            SessionAgentType::Codex => {
                // Inject OpenAI API key if available
                if let Ok(Some(api_key)) = credentials::get_openai_api_key() {
                    info!("Injecting OPENAI_API_KEY for Codex CLI");
                    return format!("export OPENAI_API_KEY='{}' && ", api_key);
                }
                String::new()
            }
            SessionAgentType::Gemini => {
                // Inject Gemini API key if available
                if let Ok(Some(api_key)) = credentials::get_gemini_api_key() {
                    info!("Injecting GEMINI_API_KEY for Gemini CLI");
                    return format!("export GEMINI_API_KEY='{}' && ", api_key);
                }
                String::new()
            }
            SessionAgentType::Copilot => {
                // Copilot uses gh OAuth — no API key injection needed
                // gh auth handles authentication transparently
                String::new()
            }
            _ => String::new(),
        }
    }
}

/// Convert InteractiveSession to Session model for UI
impl InteractiveSession {
    pub fn to_session_model(&self) -> Session {
        let mut session = Session::new_with_options(
            self.workspace_name.clone(),
            self.worktree_path.to_string_lossy().to_string(),
            false, // skip_permissions
            SessionMode::Interactive,
            None, // boss_prompt
            self.agent_type,
            self.model,
        );

        session.id = self.session_id;
        session.branch_name = self.branch_name.clone();
        session.tmux_session_name = Some(self.tmux_session_name.clone());
        session.container_id = None; // No Docker container
        session.status = SessionStatus::Running; // If tmux session exists, it's running
        session.created_at = self.created_at;

        session
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_derive_workspace_name() {
        // Legacy worktree layout: <repo>--<hash>--<session-id>
        assert_eq!(
            InteractiveSessionManager::derive_workspace_name(
                Path::new("/wt/by-name/nanoclaw--ops-main--5950b4bd"),
                Path::new("/repos/nanoclaw"),
            ),
            "nanoclaw"
        );

        // Flat layout (no `--`): must fall back to source_repository basename,
        // not return the full directory name.
        assert_eq!(
            InteractiveSessionManager::derive_workspace_name(
                Path::new("/wt/shotclubhouse_shotclubhouse_temp_debug"),
                Path::new("/repos/shotclubhouse"),
            ),
            "shotclubhouse"
        );

        // Both unusable → "unknown"
        assert_eq!(
            InteractiveSessionManager::derive_workspace_name(Path::new("/"), Path::new("/")),
            "unknown"
        );
    }

    #[test]
    fn test_generate_tmux_name() {
        // New format: tmux_{folder}_{branch}
        assert_eq!(
            InteractiveSessionManager::generate_tmux_name("myrepo--abc123", "feature/my-feature"),
            "tmux_myrepo--abc123_feature_my-feature"
        );

        assert_eq!(
            InteractiveSessionManager::generate_tmux_name("project--xyz789", "fix.bug:test"),
            "tmux_project--xyz789_fix_bug_test"
        );

        assert_eq!(
            InteractiveSessionManager::generate_tmux_name("simple-folder", "simple"),
            "tmux_simple-folder_simple"
        );

        // Test folder with special chars
        assert_eq!(
            InteractiveSessionManager::generate_tmux_name("my.repo/path:foo", "main"),
            "tmux_my_repo_path_foo_main"
        );
    }

    #[test]
    fn test_generate_tmux_name_legacy() {
        // Legacy format: tmux_{branch}
        assert_eq!(
            InteractiveSessionManager::generate_tmux_name_legacy("feature/my-feature"),
            "tmux_feature_my-feature"
        );

        assert_eq!(
            InteractiveSessionManager::generate_tmux_name_legacy("fix.bug:test"),
            "tmux_fix_bug_test"
        );

        assert_eq!(
            InteractiveSessionManager::generate_tmux_name_legacy("simple"),
            "tmux_simple"
        );
    }

    #[test]
    fn test_extract_worktree_folder() {
        use std::path::PathBuf;

        let path = PathBuf::from("/home/user/worktrees/myrepo--abc123--uuid");
        assert_eq!(
            InteractiveSessionManager::extract_worktree_folder(&path),
            "myrepo--abc123--uuid"
        );

        let root_path = PathBuf::from("/");
        assert_eq!(
            InteractiveSessionManager::extract_worktree_folder(&root_path),
            "session" // fallback
        );
    }

    #[test]
    fn test_session_manager_creation() {
        let manager = InteractiveSessionManager::new();
        assert!(manager.is_ok(), "Should create manager without Docker");
    }
}
