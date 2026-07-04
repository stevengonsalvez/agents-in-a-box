// ABOUTME: Session recovery component for recovering orphaned agent sessions after crash/shutdown
// Displays orphaned sessions (tmux dead, worktree exists) and orphaned worktrees (broken symlinks, no container)
// Allows resume/cleanup actions for both types

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, List, ListItem, ListState, Paragraph, Tabs, Wrap},
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;
use uuid::Uuid;

use crate::interactive::session_manager::{SessionMetadata, SessionStore};
use crate::models::SessionAgentType;

// Color palette (matching TUI style guide)
const CORNFLOWER_BLUE: Color = Color::Rgb(100, 149, 237);
const GOLD: Color = Color::Rgb(255, 215, 0);
const SELECTION_GREEN: Color = Color::Rgb(100, 200, 100);
const WARNING_ORANGE: Color = Color::Rgb(255, 165, 0);
const DARK_BG: Color = Color::Rgb(25, 25, 35);
const PANEL_BG: Color = Color::Rgb(30, 30, 40);
const LIST_HIGHLIGHT_BG: Color = Color::Rgb(40, 40, 60);
const SOFT_WHITE: Color = Color::Rgb(220, 220, 230);
const MUTED_GRAY: Color = Color::Rgb(120, 120, 140);
const SUBDUED_BORDER: Color = Color::Rgb(60, 60, 80);

/// Represents an orphaned agent session (from ~/.claude/agents/*.json)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrphanedSession {
    pub session: String,
    pub task: String,
    pub directory: String,
    pub created: String,
    pub status: String,
    pub transcript_path: Option<String>,
    pub worktree_branch: Option<String>,
    pub can_resume: bool,
    pub time_ago: String,
}

/// Type of orphaned worktree
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrphanType {
    /// by-session/<uuid> symlink points to missing directory
    BrokenSymlink,
    /// Worktree exists but no Docker container running
    NoContainer,
    /// Worktree exists but no tmux session
    NoTmux,
    /// Worktree exists but no ~/.claude/agents/*.json metadata
    NoMetadata,
}

impl OrphanType {
    pub fn label(&self) -> &'static str {
        match self {
            Self::BrokenSymlink => "Broken Symlink",
            Self::NoContainer => "No Container",
            Self::NoTmux => "No tmux",
            Self::NoMetadata => "No Metadata",
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            Self::BrokenSymlink => "🔗",
            Self::NoContainer => "📦",
            Self::NoTmux => "💤",
            Self::NoMetadata => "📄",
        }
    }
}

/// Represents an orphaned worktree (from ~/.agents-in-a-box/worktrees/)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrphanedWorktree {
    /// UUID from by-session/ symlink (if any)
    pub id: Option<String>,
    /// Actual worktree directory path
    pub path: PathBuf,
    /// Worktree name (from directory name)
    pub name: String,
    /// Git branch
    pub branch: Option<String>,
    /// Last commit message/hash
    pub last_commit: Option<String>,
    /// Original repository (detected from git remote)
    pub source_repo: Option<String>,
    /// Type of orphan
    pub orphan_type: OrphanType,
    /// Time since last modification
    pub time_ago: String,
    /// Agent type from sessions.json (if known)
    pub agent_type: Option<SessionAgentType>,
}

/// Result of a bulk recovery operation
pub struct BulkRecoveryResult {
    pub succeeded: Vec<String>,
    pub failed: Vec<(String, String)>,
}

impl Default for OrphanedWorktree {
    fn default() -> Self {
        Self {
            id: None,
            path: PathBuf::new(),
            name: String::new(),
            branch: None,
            last_commit: None,
            source_repo: None,
            orphan_type: OrphanType::NoMetadata,
            time_ago: String::new(),
            agent_type: None,
        }
    }
}

/// View mode for recovery screen
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RecoveryViewMode {
    /// Show only orphaned sessions (from ~/.claude/agents/)
    #[default]
    Sessions,
    /// Show only orphaned worktrees (from ~/.agents-in-a-box/worktrees/)
    Worktrees,
    /// Show combined view of both
    All,
}

impl RecoveryViewMode {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Sessions => "Sessions",
            Self::Worktrees => "Worktrees",
            Self::All => "All",
        }
    }

    pub fn next(&self) -> Self {
        match self {
            Self::Sessions => Self::Worktrees,
            Self::Worktrees => Self::All,
            Self::All => Self::Sessions,
        }
    }
}

/// State for the session recovery component
#[derive(Debug, Clone)]
pub struct SessionRecoveryState {
    /// Orphaned sessions from ~/.claude/agents/
    pub orphaned_sessions: Vec<OrphanedSession>,
    /// Orphaned worktrees from ~/.agents-in-a-box/worktrees/
    pub orphaned_worktrees: Vec<OrphanedWorktree>,
    /// Current view mode (Sessions, Worktrees, All)
    pub view_mode: RecoveryViewMode,
    /// Selected index in current view
    pub selected_index: usize,
    /// Multi-select: indices of items marked for bulk operations
    pub selected_items: HashSet<usize>,
    /// List state for rendering
    pub list_state: ListState,
    /// Whether data is being loaded
    pub loading: bool,
    /// Last error message
    pub last_error: Option<String>,
    /// Last action result message
    pub action_result: Option<String>,
    /// Bulk recovery result overlay (shown after multi-resume)
    pub recovery_overlay: Option<RecoveryOverlay>,
}

/// Overlay showing results of a bulk recovery operation
#[derive(Debug, Clone)]
pub struct RecoveryOverlay {
    pub title: String,
    pub results: Vec<RecoveryResultLine>,
    pub scroll_offset: usize,
}

#[derive(Debug, Clone)]
pub struct RecoveryResultLine {
    pub name: String,
    pub success: bool,
    pub detail: String,
}

impl Default for SessionRecoveryState {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionRecoveryState {
    pub fn new() -> Self {
        let mut state = Self {
            orphaned_sessions: Vec::new(),
            orphaned_worktrees: Vec::new(),
            view_mode: RecoveryViewMode::default(),
            selected_index: 0,
            selected_items: HashSet::new(),
            list_state: ListState::default(),
            loading: true,
            last_error: None,
            action_result: None,
            recovery_overlay: None,
        };
        state.refresh();
        state
    }

    /// Get the total count of items in current view (includes separator in All view)
    pub fn current_view_count(&self) -> usize {
        match self.view_mode {
            RecoveryViewMode::Sessions => self.orphaned_sessions.len(),
            RecoveryViewMode::Worktrees => self.orphaned_worktrees.len(),
            RecoveryViewMode::All => {
                self.orphaned_sessions.len()
                    + self.orphaned_worktrees.len()
                    + self.separator_offset()
            }
        }
    }

    /// Returns 1 if a separator exists in All view (both sessions and worktrees present), 0 otherwise
    fn separator_offset(&self) -> usize {
        if self.view_mode == RecoveryViewMode::All
            && !self.orphaned_sessions.is_empty()
            && !self.orphaned_worktrees.is_empty()
        {
            1
        } else {
            0
        }
    }

    /// The list index where the separator lives (only valid when separator_offset() == 1)
    fn separator_index(&self) -> usize {
        self.orphaned_sessions.len()
    }

    /// Check if current selection is on the separator row
    fn is_on_separator(&self) -> bool {
        self.separator_offset() == 1 && self.selected_index == self.separator_index()
    }

    /// Check if current selection is in the worktrees section (for All view)
    pub fn is_worktree_selected(&self) -> bool {
        match self.view_mode {
            RecoveryViewMode::Sessions => false,
            RecoveryViewMode::Worktrees => true,
            RecoveryViewMode::All => {
                self.selected_index > self.orphaned_sessions.len() + self.separator_offset() - 1
            }
        }
    }

    /// Get the worktree index relative to orphaned_worktrees list
    pub fn worktree_index(&self) -> Option<usize> {
        if !self.is_worktree_selected() {
            return None;
        }
        match self.view_mode {
            RecoveryViewMode::Worktrees => Some(self.selected_index),
            RecoveryViewMode::All => {
                Some(self.selected_index - self.orphaned_sessions.len() - self.separator_offset())
            }
            RecoveryViewMode::Sessions => None,
        }
    }

    /// Toggle to next view mode
    pub fn toggle_view_mode(&mut self) {
        self.view_mode = self.view_mode.next();
        self.selected_index = 0;
        self.selected_items.clear();
        let count = self.current_view_count();
        self.list_state.select(if count == 0 { None } else { Some(0) });
    }

    /// Refresh the list of orphaned sessions and worktrees
    pub fn refresh(&mut self) {
        self.loading = true;
        self.last_error = None;
        self.action_result = None;
        self.selected_items.clear();

        // Load orphaned sessions
        match Self::load_orphaned_sessions() {
            Ok(sessions) => {
                self.orphaned_sessions = sessions;
            }
            Err(e) => {
                self.last_error = Some(format!("Sessions: {}", e));
            }
        }

        // Load orphaned worktrees
        match Self::load_orphaned_worktrees() {
            Ok(worktrees) => {
                self.orphaned_worktrees = worktrees;
            }
            Err(e) => {
                let err_msg = format!("Worktrees: {}", e);
                if let Some(ref mut existing) = self.last_error {
                    existing.push_str(&format!("; {}", err_msg));
                } else {
                    self.last_error = Some(err_msg);
                }
            }
        }

        self.loading = false;

        // Adjust selected index
        let count = self.current_view_count();
        if count > 0 && self.selected_index >= count {
            self.selected_index = count - 1;
        }
        self.list_state.select(if count == 0 {
            None
        } else {
            Some(self.selected_index)
        });
    }

    /// Load orphaned sessions from ~/.claude/agents/
    fn load_orphaned_sessions() -> Result<Vec<OrphanedSession>, String> {
        let agents_dir = dirs::home_dir()
            .ok_or("Could not find home directory")?
            .join(".claude")
            .join("agents");

        if !agents_dir.exists() {
            return Ok(Vec::new());
        }

        let mut orphaned = Vec::new();

        for entry in std::fs::read_dir(&agents_dir).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path();

            if path.extension().map(|e| e == "json").unwrap_or(false) {
                if path.file_name().map(|n| n == "registry.jsonl").unwrap_or(false) {
                    continue;
                }

                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Ok(meta) = serde_json::from_str::<serde_json::Value>(&content) {
                        let session = meta["session"].as_str().unwrap_or("").to_string();
                        let status = meta["status"].as_str().unwrap_or("unknown").to_string();

                        // Skip completed/archived sessions
                        if status == "completed" || status == "archived" {
                            continue;
                        }

                        // Check if tmux session exists
                        let tmux_alive = Command::new("tmux")
                            .args(["has-session", "-t", &session])
                            .output()
                            .map(|o| o.status.success())
                            .unwrap_or(false);

                        if tmux_alive {
                            continue; // Not orphaned
                        }

                        // Check if worktree exists
                        let directory = meta["directory"].as_str().unwrap_or("").to_string();
                        if directory.is_empty() || !PathBuf::from(&directory).exists() {
                            continue;
                        }

                        // Check for transcript
                        let transcript_path =
                            meta["transcript_path"].as_str().map(|s| s.to_string());
                        let can_resume = transcript_path
                            .as_ref()
                            .map(|p| PathBuf::from(p).exists())
                            .unwrap_or(false);

                        // Calculate time ago
                        let created = meta["created"].as_str().unwrap_or("").to_string();
                        let time_ago = Self::calculate_time_ago(&created);

                        // Get branch - from metadata or detect from worktree
                        let worktree_branch = meta["worktree_branch"]
                            .as_str()
                            .map(|s| s.to_string())
                            .or_else(|| Self::detect_branch_from_directory(&directory));

                        orphaned.push(OrphanedSession {
                            session,
                            task: meta["task"].as_str().unwrap_or("Unknown task").to_string(),
                            directory,
                            created,
                            status,
                            transcript_path,
                            worktree_branch,
                            can_resume,
                            time_ago,
                        });
                    }
                }
            }
        }

        // Sort by created date (newest first)
        orphaned.sort_by(|a, b| b.created.cmp(&a.created));

        Ok(orphaned)
    }

    /// Load orphaned worktrees from ~/.agents-in-a-box/worktrees/
    fn load_orphaned_worktrees() -> Result<Vec<OrphanedWorktree>, String> {
        let worktrees_dir = dirs::home_dir()
            .ok_or("Could not find home directory")?
            .join(".agents-in-a-box")
            .join("worktrees");

        if !worktrees_dir.exists() {
            return Ok(Vec::new());
        }

        let mut orphaned = Vec::new();
        let by_session = worktrees_dir.join("by-session");
        let by_name = worktrees_dir.join("by-name");

        // Track which worktrees are referenced by valid symlinks
        let mut referenced_worktrees: std::collections::HashSet<PathBuf> =
            std::collections::HashSet::new();

        // 1. Scan by-session/ for broken symlinks and valid symlinks to inactive worktrees
        if by_session.exists() {
            if let Ok(entries) = std::fs::read_dir(&by_session) {
                for entry in entries.flatten() {
                    let path = entry.path();

                    // Check if it's a symlink
                    if path.symlink_metadata().map(|m| m.file_type().is_symlink()).unwrap_or(false)
                    {
                        let session_id = path.file_name().map(|n| n.to_string_lossy().to_string());

                        match std::fs::read_link(&path) {
                            Ok(target) => {
                                // Resolve relative paths
                                let resolved_target = if target.is_relative() {
                                    by_session.join(&target)
                                } else {
                                    target.clone()
                                };

                                if !resolved_target.exists() {
                                    // Broken symlink
                                    orphaned.push(OrphanedWorktree {
                                        id: session_id,
                                        path: target,
                                        name: path
                                            .file_name()
                                            .map(|n| n.to_string_lossy().to_string())
                                            .unwrap_or_default(),
                                        orphan_type: OrphanType::BrokenSymlink,
                                        ..Default::default()
                                    });
                                } else {
                                    // Valid symlink - check if tmux session exists
                                    referenced_worktrees.insert(resolved_target.clone());

                                    if let Some(ref id) = session_id {
                                        let tmux_alive = Command::new("tmux")
                                            .args(["has-session", "-t", id])
                                            .output()
                                            .map(|o| o.status.success())
                                            .unwrap_or(false);

                                        if !tmux_alive {
                                            // Worktree exists but no tmux session
                                            if let Some(worktree) = Self::extract_worktree_info(
                                                &resolved_target,
                                                session_id.clone(),
                                                OrphanType::NoTmux,
                                            ) {
                                                orphaned.push(worktree);
                                            }
                                        }
                                    }
                                }
                            }
                            Err(_) => {}
                        }
                    }
                }
            }
        }

        // 2. Scan by-name/ for unreferenced worktrees (no symlink pointing to them)
        if by_name.exists() {
            if let Ok(entries) = std::fs::read_dir(&by_name) {
                for entry in entries.flatten() {
                    let path = entry.path();

                    // Only check directories that look like git worktrees
                    if path.is_dir() && (path.join(".git").exists() || path.join(".git").is_file())
                    {
                        // Canonicalize to compare properly
                        let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());

                        // Check if this worktree is referenced by any by-session symlink
                        if !referenced_worktrees.contains(&canonical) {
                            if let Some(worktree) =
                                Self::extract_worktree_info(&path, None, OrphanType::NoMetadata)
                            {
                                orphaned.push(worktree);
                            }
                        }
                    }
                }
            }
        }

        // Enrich orphaned worktrees with agent_type from sessions.json
        let store = SessionStore::load();
        for worktree in &mut orphaned {
            // Try to find matching session by worktree path
            for (_, metadata) in store.sessions() {
                if metadata.worktree_path == worktree.path {
                    worktree.agent_type = Some(metadata.agent_type);
                    break;
                }
            }
        }

        // Sort by time_ago (most recent first based on directory mtime)
        orphaned.sort_by(|a, b| {
            let a_mtime = std::fs::metadata(&a.path).and_then(|m| m.modified()).ok();
            let b_mtime = std::fs::metadata(&b.path).and_then(|m| m.modified()).ok();
            b_mtime.cmp(&a_mtime)
        });

        Ok(orphaned)
    }

    /// Extract information from a worktree directory
    fn extract_worktree_info(
        path: &PathBuf,
        session_id: Option<String>,
        orphan_type: OrphanType,
    ) -> Option<OrphanedWorktree> {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());

        // Get branch
        let branch = Self::detect_branch_from_directory(&path.to_string_lossy());

        // Get last commit
        let last_commit = Command::new("git")
            .args(["log", "-1", "--format=%s", "--no-walk"])
            .current_dir(path)
            .output()
            .ok()
            .and_then(|o| {
                if o.status.success() {
                    let msg = String::from_utf8_lossy(&o.stdout).trim().to_string();
                    if !msg.is_empty() { Some(msg) } else { None }
                } else {
                    None
                }
            });

        // Get source repo from git remote
        let source_repo = Command::new("git")
            .args(["remote", "get-url", "origin"])
            .current_dir(path)
            .output()
            .ok()
            .and_then(|o| {
                if o.status.success() {
                    let url = String::from_utf8_lossy(&o.stdout).trim().to_string();
                    // Extract just the repo name from URL
                    url.split('/').last().map(|s| s.trim_end_matches(".git").to_string())
                } else {
                    None
                }
            });

        // Calculate time ago from mtime
        let time_ago = std::fs::metadata(path)
            .and_then(|m| m.modified())
            .ok()
            .map(|mtime| {
                use std::time::SystemTime;
                let elapsed = SystemTime::now().duration_since(mtime).unwrap_or_default();
                let hours = elapsed.as_secs() / 3600;
                if hours < 1 {
                    let minutes = elapsed.as_secs() / 60;
                    format!("{}m ago", minutes)
                } else if hours < 24 {
                    format!("{}h ago", hours)
                } else {
                    let days = hours / 24;
                    format!("{}d ago", days)
                }
            })
            .unwrap_or_default();

        Some(OrphanedWorktree {
            id: session_id,
            path: path.clone(),
            name,
            branch,
            last_commit,
            source_repo,
            orphan_type,
            time_ago,
            agent_type: None, // Enriched later from sessions.json
        })
    }

    fn calculate_time_ago(created: &str) -> String {
        use chrono::{DateTime, Utc};

        if let Ok(dt) = DateTime::parse_from_rfc3339(created) {
            let now = Utc::now();
            let duration = now.signed_duration_since(dt.with_timezone(&Utc));

            let hours = duration.num_hours();
            if hours < 1 {
                let minutes = duration.num_minutes();
                return format!("{}m ago", minutes);
            } else if hours < 24 {
                return format!("{}h ago", hours);
            }
            let days = hours / 24;
            return format!("{}d ago", days);
        }

        String::new()
    }

    /// Detect the git branch from a worktree directory
    fn detect_branch_from_directory(directory: &str) -> Option<String> {
        let output = Command::new("git")
            .args(["branch", "--show-current"])
            .current_dir(directory)
            .output()
            .ok()?;

        if output.status.success() {
            let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !branch.is_empty() {
                return Some(branch);
            }
        }

        // Fallback: try to get branch from HEAD
        let output = Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(directory)
            .output()
            .ok()?;

        if output.status.success() {
            let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !branch.is_empty() && branch != "HEAD" {
                return Some(branch);
            }
        }

        None
    }

    pub fn next(&mut self) {
        let count = self.current_view_count();
        if count == 0 {
            return;
        }
        self.selected_index = (self.selected_index + 1) % count;
        // Skip separator row in All view
        if self.is_on_separator() {
            self.selected_index = (self.selected_index + 1) % count;
        }
        self.list_state.select(Some(self.selected_index));
    }

    pub fn previous(&mut self) {
        let count = self.current_view_count();
        if count == 0 {
            return;
        }
        if self.selected_index == 0 {
            self.selected_index = count - 1;
        } else {
            self.selected_index -= 1;
        }
        // Skip separator row in All view
        if self.is_on_separator() {
            if self.selected_index == 0 {
                self.selected_index = count - 1;
            } else {
                self.selected_index -= 1;
            }
        }
        self.list_state.select(Some(self.selected_index));
    }

    /// Get the selected session (only valid when not in worktree selection)
    pub fn selected(&self) -> Option<&OrphanedSession> {
        if self.is_worktree_selected() {
            return None;
        }
        self.orphaned_sessions.get(self.selected_index)
    }

    /// Get the selected worktree
    pub fn selected_worktree(&self) -> Option<&OrphanedWorktree> {
        self.worktree_index().and_then(|idx| self.orphaned_worktrees.get(idx))
    }

    /// Resume the selected session
    pub fn resume_selected(&mut self) -> Result<String, String> {
        let session = self.selected().ok_or("No session selected")?.clone();
        let result = Self::resume_single_session(&session)?;
        self.action_result = Some(format!("Resumed as: {}", result));
        self.refresh();
        Ok(result)
    }

    /// Generate a tmux-compatible session name from folder and branch
    /// Matches the naming convention in InteractiveSessionManager
    fn generate_tmux_name(folder: &str, branch: &str) -> String {
        let sanitized_folder =
            folder.replace(' ', "_").replace('.', "_").replace('/', "_").replace(':', "_");
        let sanitized_branch =
            branch.replace(' ', "_").replace('.', "_").replace('/', "_").replace(':', "_");
        format!("tmux_{}_{}", sanitized_folder, sanitized_branch)
    }

    /// Resume a single orphaned worktree by creating a new tmux session and starting Claude.
    /// This is a static method with no `&mut self` dependency, enabling bulk recovery.
    /// The session is registered in sessions.json so it appears as a proper Workspace.
    fn resume_single_worktree(worktree: &OrphanedWorktree) -> Result<String, String> {
        // Can't resume broken symlinks (directory doesn't exist)
        if worktree.orphan_type == OrphanType::BrokenSymlink {
            return Err("Cannot resume: worktree directory no longer exists".to_string());
        }

        // Verify the directory exists
        if !worktree.path.exists() {
            return Err("Cannot resume: worktree directory no longer exists".to_string());
        }

        // Extract worktree folder name and branch for proper tmux naming
        let worktree_folder = worktree
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("session")
            .to_string();
        let branch = worktree.branch.clone().unwrap_or_else(|| "main".to_string());

        // Generate proper tmux session name (tmux_{folder}_{branch})
        // This matches InteractiveSessionManager naming convention
        let new_session = Self::generate_tmux_name(&worktree_folder, &branch);

        // Check if session with this name already exists and kill it
        let check_result = Command::new("tmux").args(["has-session", "-t", &new_session]).output();
        if check_result.map(|o| o.status.success()).unwrap_or(false) {
            // Kill existing session to avoid conflicts
            let _ = Command::new("tmux").args(["kill-session", "-t", &new_session]).output();
        }

        // Create new tmux session in the worktree directory
        let create_result = Command::new("tmux")
            .args([
                "new-session",
                "-d",
                "-s",
                &new_session,
                "-c",
                &worktree.path.to_string_lossy(),
            ])
            .output()
            .map_err(|e| e.to_string())?;

        if !create_result.status.success() {
            return Err(format!(
                "Failed to create tmux session: {}",
                String::from_utf8_lossy(&create_result.stderr)
            ));
        }

        // Parse or generate session UUID
        let session_id = worktree
            .id
            .as_ref()
            .and_then(|id| Uuid::parse_str(id).ok())
            .unwrap_or_else(Uuid::new_v4);

        // Register session in sessions.json so it appears as a Workspace
        // Preserve the original agent_type if known (e.g., Copilot, Codex), otherwise default to Claude
        let agent_type = worktree.agent_type.unwrap_or_default();
        let metadata = SessionMetadata {
            session_id,
            tmux_session_name: new_session.clone(),
            worktree_path: worktree.path.clone(),
            workspace_name: worktree.source_repo.clone().unwrap_or_else(|| worktree.name.clone()),
            created_at: chrono::Utc::now(),
            agent_type,
            headroom_enabled: false,
            rtk_enabled: false,
        };

        // Locked RMW (pu4): serialise this recovery re-register against live
        // create/kill writers so neither lost-updates the other.
        if let Err(e) = SessionStore::mutate(|store| store.upsert(metadata)) {
            // Log warning but continue - session still works, just won't show as Workspace
            tracing::warn!("Failed to persist session metadata: {}", e);
        }

        // Ensure symlink exists in by-session/ for session discovery
        if let Some(home) = dirs::home_dir() {
            let by_session_dir = home.join(".agents-in-a-box").join("worktrees").join("by-session");
            let symlink_path = by_session_dir.join(session_id.to_string());

            // Create/update symlink if needed
            if !symlink_path.exists() {
                std::fs::create_dir_all(&by_session_dir).ok();
                #[cfg(unix)]
                std::os::unix::fs::symlink(&worktree.path, &symlink_path).ok();
            }
        }

        // Try to find a transcript to resume from (only for Claude sessions)
        let transcript_path = if agent_type == SessionAgentType::Claude {
            Self::find_transcript_for_worktree(worktree)
        } else {
            None
        };

        // Build agent command based on agent_type
        let agent_cmd = match agent_type {
            SessionAgentType::Copilot => "github-copilot-cli".to_string(),
            SessionAgentType::Codex => "codex --dangerously-skip-permissions".to_string(),
            SessionAgentType::Gemini => "gemini".to_string(),
            SessionAgentType::Shell => String::new(), // Just open a shell, no command
            _ => {
                // Claude (default)
                if let Some(transcript) = transcript_path {
                    format!(
                        "claude --dangerously-skip-permissions --resume \"{}\"",
                        transcript
                    )
                } else {
                    "claude --dangerously-skip-permissions".to_string()
                }
            }
        };

        // Send command to tmux (skip for Shell sessions — just leave the shell open)
        if !agent_cmd.is_empty() {
            let send_result = Command::new("tmux")
                .args(["send-keys", "-t", &new_session, &agent_cmd, "C-m"])
                .output()
                .map_err(|e| e.to_string())?;

            if !send_result.status.success() {
                return Err(format!(
                    "Failed to start {}: {}",
                    agent_type.name(),
                    String::from_utf8_lossy(&send_result.stderr)
                ));
            }
        }

        Ok(new_session)
    }

    /// Resume the currently selected orphaned worktree
    pub fn resume_worktree(&mut self) -> Result<String, String> {
        let worktree = self.selected_worktree().ok_or("No worktree selected")?.clone();
        let result = Self::resume_single_worktree(&worktree)?;
        self.action_result = Some(format!("Resumed as: {}", result));
        self.refresh();
        Ok(result)
    }

    /// Recover all orphaned worktrees in bulk (skipping broken symlinks)
    pub fn recover_all_worktrees(&mut self) -> BulkRecoveryResult {
        let mut result = BulkRecoveryResult {
            succeeded: vec![],
            failed: vec![],
        };
        let worktrees: Vec<_> = self.orphaned_worktrees.clone();
        for worktree in &worktrees {
            if worktree.orphan_type == OrphanType::BrokenSymlink {
                continue;
            }
            match Self::resume_single_worktree(worktree) {
                Ok(name) => result.succeeded.push(name),
                Err(e) => result.failed.push((worktree.name.clone(), e)),
            }
        }
        self.refresh();
        result
    }

    /// Try to find a transcript file associated with this worktree
    fn find_transcript_for_worktree(worktree: &OrphanedWorktree) -> Option<String> {
        // Strategy 1: Check ~/.claude/agents/<session-id>.json for transcript_path
        if let Some(ref id) = worktree.id {
            if let Some(home) = dirs::home_dir() {
                let meta_file = home.join(".claude").join("agents").join(format!("{}.json", id));
                if meta_file.exists() {
                    if let Ok(content) = std::fs::read_to_string(&meta_file) {
                        if let Ok(meta) = serde_json::from_str::<serde_json::Value>(&content) {
                            if let Some(path) = meta["transcript_path"].as_str() {
                                if std::path::Path::new(path).exists() {
                                    return Some(path.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }

        // Strategy 2: Look for .claude-transcript.jsonl in worktree directory
        let local_transcript = worktree.path.join(".claude-transcript.jsonl");
        if local_transcript.exists() {
            return Some(local_transcript.to_string_lossy().to_string());
        }

        // Strategy 3: Look for any .jsonl file in .claude/ subdirectory of worktree
        let claude_dir = worktree.path.join(".claude");
        if claude_dir.exists() {
            if let Ok(entries) = std::fs::read_dir(&claude_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().map(|e| e == "jsonl").unwrap_or(false) {
                        return Some(path.to_string_lossy().to_string());
                    }
                }
            }
        }

        None
    }

    /// Archive the selected session
    pub fn archive_selected(&mut self) -> Result<(), String> {
        let session = self.selected().ok_or("No session selected")?.clone();

        let agents_dir = dirs::home_dir()
            .ok_or("Could not find home directory")?
            .join(".claude")
            .join("agents");

        let archived_dir = agents_dir.join("archived");
        std::fs::create_dir_all(&archived_dir).map_err(|e| e.to_string())?;

        let meta_file = agents_dir.join(format!("{}.json", session.session));
        let archived_file = archived_dir.join(format!("{}.json", session.session));

        if meta_file.exists() {
            // Update status to archived
            if let Ok(content) = std::fs::read_to_string(&meta_file) {
                if let Ok(mut meta) = serde_json::from_str::<serde_json::Value>(&content) {
                    meta["status"] = serde_json::Value::String("archived".to_string());
                    meta["archived_at"] =
                        serde_json::Value::String(chrono::Utc::now().to_rfc3339());

                    std::fs::write(&archived_file, serde_json::to_string_pretty(&meta).unwrap())
                        .map_err(|e| e.to_string())?;
                    std::fs::remove_file(&meta_file).map_err(|e| e.to_string())?;
                }
            }
        }

        self.action_result = Some(format!("Archived: {}", session.session));
        self.refresh();

        Ok(())
    }

    /// Delete the selected worktree and its symlink
    pub fn cleanup_worktree(&mut self) -> Result<(), String> {
        let worktree = self.selected_worktree().ok_or("No worktree selected")?.clone();

        let worktrees_base = dirs::home_dir()
            .ok_or("Could not find home directory")?
            .join(".agents-in-a-box")
            .join("worktrees");

        // 1. Remove the worktree directory (if it exists and is not a broken symlink target)
        if worktree.path.exists() && worktree.orphan_type != OrphanType::BrokenSymlink {
            std::fs::remove_dir_all(&worktree.path)
                .map_err(|e| format!("Failed to remove worktree directory: {}", e))?;
        }

        // 2. Remove the symlink in by-session/ if it exists
        if let Some(ref id) = worktree.id {
            let symlink_path = worktrees_base.join("by-session").join(id);
            if symlink_path.symlink_metadata().is_ok() {
                std::fs::remove_file(&symlink_path)
                    .map_err(|e| format!("Failed to remove symlink: {}", e))?;
            }
        }

        // 3. Also check by-name/ for directories matching the worktree name
        let by_name_path = worktrees_base.join("by-name").join(&worktree.name);
        if by_name_path.exists() || by_name_path.symlink_metadata().is_ok() {
            if by_name_path.is_dir() {
                std::fs::remove_dir_all(&by_name_path)
                    .map_err(|e| format!("Failed to remove by-name directory: {}", e))?;
            } else {
                std::fs::remove_file(&by_name_path)
                    .map_err(|e| format!("Failed to remove by-name symlink: {}", e))?;
            }
        }

        self.action_result = Some(format!("Deleted: {}", worktree.name));
        self.refresh();

        Ok(())
    }

    /// Perform the appropriate action for the current selection (archive session or cleanup worktree)
    pub fn delete_selected(&mut self) -> Result<(), String> {
        if self.is_worktree_selected() {
            self.cleanup_worktree()
        } else {
            self.archive_selected()
        }
    }

    /// Toggle multi-select for the current item
    pub fn toggle_select(&mut self) {
        // Don't allow selecting the separator
        if self.is_on_separator() {
            return;
        }
        let idx = self.selected_index;
        if self.selected_items.contains(&idx) {
            self.selected_items.remove(&idx);
        } else {
            self.selected_items.insert(idx);
        }
    }

    /// Check if any items are multi-selected
    pub fn has_multi_selection(&self) -> bool {
        !self.selected_items.is_empty()
    }

    /// Resume a single orphaned session by creating a tmux session and starting Claude with --resume.
    /// Static method to enable bulk recovery.
    fn resume_single_session(session: &OrphanedSession) -> Result<String, String> {
        if !session.can_resume {
            return Err("Cannot resume: no transcript found".to_string());
        }

        let transcript = session.transcript_path.as_ref().ok_or("No transcript path")?;

        let new_session = format!(
            "{}-resumed-{}",
            session.session,
            chrono::Utc::now().timestamp()
        );
        let directory = &session.directory;

        let create_result = Command::new("tmux")
            .args(["new-session", "-d", "-s", &new_session, "-c", directory])
            .output()
            .map_err(|e| e.to_string())?;

        if !create_result.status.success() {
            return Err(format!(
                "Failed to create tmux session: {}",
                String::from_utf8_lossy(&create_result.stderr)
            ));
        }

        let claude_cmd = format!(
            "claude --dangerously-skip-permissions --resume \"{}\"",
            transcript
        );
        let send_result = Command::new("tmux")
            .args(["send-keys", "-t", &new_session, &claude_cmd, "C-m"])
            .output()
            .map_err(|e| e.to_string())?;

        if !send_result.status.success() {
            return Err(format!(
                "Failed to start Claude: {}",
                String::from_utf8_lossy(&send_result.stderr)
            ));
        }

        Ok(new_session)
    }

    /// Resume all multi-selected items, showing results in an overlay
    pub fn resume_multi_selected(&mut self) -> (usize, usize) {
        let mut indices: Vec<usize> = self.selected_items.iter().copied().collect();
        indices.sort_unstable();

        let mut resumed = 0;
        let mut failed = 0;
        let mut results = Vec::new();

        for idx in indices {
            match self.view_mode {
                RecoveryViewMode::Sessions => {
                    if let Some(session) = self.orphaned_sessions.get(idx) {
                        let session = session.clone();
                        let name = session.session.clone();
                        match Self::resume_single_session(&session) {
                            Ok(tmux_name) => {
                                resumed += 1;
                                results.push(RecoveryResultLine {
                                    name,
                                    success: true,
                                    detail: format!("→ {}", tmux_name),
                                });
                            }
                            Err(e) => {
                                failed += 1;
                                results.push(RecoveryResultLine {
                                    name,
                                    success: false,
                                    detail: e,
                                });
                            }
                        }
                    }
                }
                RecoveryViewMode::Worktrees => {
                    if let Some(worktree) = self.orphaned_worktrees.get(idx) {
                        let worktree = worktree.clone();
                        let name = worktree.name.clone();
                        match Self::resume_single_worktree(&worktree) {
                            Ok(tmux_name) => {
                                resumed += 1;
                                results.push(RecoveryResultLine {
                                    name,
                                    success: true,
                                    detail: format!("→ {}", tmux_name),
                                });
                            }
                            Err(e) => {
                                failed += 1;
                                results.push(RecoveryResultLine {
                                    name,
                                    success: false,
                                    detail: e,
                                });
                            }
                        }
                    }
                }
                RecoveryViewMode::All => {
                    let session_count = self.orphaned_sessions.len();
                    let sep = self.separator_offset();
                    if idx < session_count {
                        let session = self.orphaned_sessions[idx].clone();
                        let name = session.session.clone();
                        match Self::resume_single_session(&session) {
                            Ok(tmux_name) => {
                                resumed += 1;
                                results.push(RecoveryResultLine {
                                    name,
                                    success: true,
                                    detail: format!("→ {}", tmux_name),
                                });
                            }
                            Err(e) => {
                                failed += 1;
                                results.push(RecoveryResultLine {
                                    name,
                                    success: false,
                                    detail: e,
                                });
                            }
                        }
                    } else if idx >= session_count + sep {
                        let worktree_idx = idx - session_count - sep;
                        if let Some(worktree) = self.orphaned_worktrees.get(worktree_idx) {
                            let worktree = worktree.clone();
                            let name = worktree.name.clone();
                            match Self::resume_single_worktree(&worktree) {
                                Ok(tmux_name) => {
                                    resumed += 1;
                                    results.push(RecoveryResultLine {
                                        name,
                                        success: true,
                                        detail: format!("→ {}", tmux_name),
                                    });
                                }
                                Err(e) => {
                                    failed += 1;
                                    results.push(RecoveryResultLine {
                                        name,
                                        success: false,
                                        detail: e,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }

        self.recovery_overlay = Some(RecoveryOverlay {
            title: format!("Recovery Results — {} resumed, {} failed", resumed, failed),
            results,
            scroll_offset: 0,
        });

        self.selected_items.clear();
        self.refresh();
        (resumed, failed)
    }

    /// Dismiss the recovery overlay
    pub fn dismiss_overlay(&mut self) {
        self.recovery_overlay = None;
    }

    /// Delete all multi-selected items
    pub fn delete_multi_selected(&mut self) -> (usize, usize) {
        // Collect items to delete in reverse order (so indices stay valid)
        let mut indices: Vec<usize> = self.selected_items.iter().copied().collect();
        indices.sort_unstable();
        indices.reverse();

        let mut deleted = 0;
        let mut failed = 0;

        for idx in indices {
            match self.view_mode {
                RecoveryViewMode::Sessions => {
                    if idx < self.orphaned_sessions.len() {
                        // Archive the session
                        let session = self.orphaned_sessions[idx].clone();
                        if Self::archive_session_by_name(&session.session).is_ok() {
                            deleted += 1;
                        } else {
                            failed += 1;
                        }
                    }
                }
                RecoveryViewMode::Worktrees => {
                    if idx < self.orphaned_worktrees.len() {
                        let worktree = self.orphaned_worktrees[idx].clone();
                        if Self::cleanup_single_worktree(&worktree).is_ok() {
                            deleted += 1;
                        } else {
                            failed += 1;
                        }
                    }
                }
                RecoveryViewMode::All => {
                    let session_count = self.orphaned_sessions.len();
                    let sep = self.separator_offset();
                    if idx < session_count {
                        // Session item
                        let session = self.orphaned_sessions[idx].clone();
                        if Self::archive_session_by_name(&session.session).is_ok() {
                            deleted += 1;
                        } else {
                            failed += 1;
                        }
                    } else if idx >= session_count + sep {
                        // Worktree item (skip separator)
                        let worktree_idx = idx - session_count - sep;
                        if worktree_idx < self.orphaned_worktrees.len() {
                            let worktree = self.orphaned_worktrees[worktree_idx].clone();
                            if Self::cleanup_single_worktree(&worktree).is_ok() {
                                deleted += 1;
                            } else {
                                failed += 1;
                            }
                        }
                    }
                    // idx == separator_index is skipped (can't be selected)
                }
            }
        }

        self.refresh();
        (deleted, failed)
    }

    /// Archive a single session by name (static, no &mut self)
    fn archive_session_by_name(session_name: &str) -> Result<(), String> {
        let agents_dir = dirs::home_dir()
            .ok_or("Could not find home directory")?
            .join(".claude")
            .join("agents");

        let archived_dir = agents_dir.join("archived");
        std::fs::create_dir_all(&archived_dir).map_err(|e| e.to_string())?;

        let meta_file = agents_dir.join(format!("{}.json", session_name));
        let archived_file = archived_dir.join(format!("{}.json", session_name));

        if meta_file.exists() {
            if let Ok(content) = std::fs::read_to_string(&meta_file) {
                if let Ok(mut meta) = serde_json::from_str::<serde_json::Value>(&content) {
                    meta["status"] = serde_json::Value::String("archived".to_string());
                    meta["archived_at"] =
                        serde_json::Value::String(chrono::Utc::now().to_rfc3339());
                    std::fs::write(&archived_file, serde_json::to_string_pretty(&meta).unwrap())
                        .map_err(|e| e.to_string())?;
                    std::fs::remove_file(&meta_file).map_err(|e| e.to_string())?;
                }
            }
        }
        Ok(())
    }

    /// Cleanup a single worktree (static, no &mut self)
    fn cleanup_single_worktree(worktree: &OrphanedWorktree) -> Result<(), String> {
        let worktrees_base = dirs::home_dir()
            .ok_or("Could not find home directory")?
            .join(".agents-in-a-box")
            .join("worktrees");

        // Remove the worktree directory
        if worktree.path.exists() && worktree.orphan_type != OrphanType::BrokenSymlink {
            std::fs::remove_dir_all(&worktree.path)
                .map_err(|e| format!("Failed to remove worktree: {}", e))?;
        }

        // Remove symlink in by-session/
        if let Some(ref id) = worktree.id {
            let symlink_path = worktrees_base.join("by-session").join(id);
            if symlink_path.symlink_metadata().is_ok() {
                std::fs::remove_file(&symlink_path).ok();
            }
        }

        // Remove by-name/ entry
        let by_name_path = worktrees_base.join("by-name").join(&worktree.name);
        if by_name_path.exists() || by_name_path.symlink_metadata().is_ok() {
            if by_name_path.is_dir() {
                std::fs::remove_dir_all(&by_name_path).ok();
            } else {
                std::fs::remove_file(&by_name_path).ok();
            }
        }

        // Remove from sessions.json (locked RMW — pu4). The path-match +
        // removal runs inside the lock so a concurrent writer can't re-add the
        // worktree between our load and save.
        let worktree_path = worktree.path.clone();
        let _ = SessionStore::mutate(|store| {
            let keys_to_remove: Vec<String> = store
                .sessions()
                .iter()
                .filter(|(_, m)| m.worktree_path == worktree_path)
                .map(|(k, _)| k.clone())
                .collect();
            for key in &keys_to_remove {
                store.remove_by_tmux_name(key);
            }
        });

        Ok(())
    }
}

/// Session recovery component renderer
pub struct SessionRecovery;

impl SessionRecovery {
    pub fn render(frame: &mut Frame, area: Rect, state: &mut SessionRecoveryState) {
        // Main layout: list on left, details on right
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
            .split(area);

        Self::render_session_list(frame, chunks[0], state);
        Self::render_session_details(frame, chunks[1], state);
    }

    fn render_session_list(frame: &mut Frame, area: Rect, state: &mut SessionRecoveryState) {
        let session_count = state.orphaned_sessions.len();
        let worktree_count = state.orphaned_worktrees.len();
        let total_count = state.current_view_count();

        // Build title based on view mode
        let title_text = match state.view_mode {
            RecoveryViewMode::Sessions => "Sessions",
            RecoveryViewMode::Worktrees => "Worktrees",
            RecoveryViewMode::All => "All Orphans",
        };

        let count_text = match state.view_mode {
            RecoveryViewMode::Sessions => format!("({})", session_count),
            RecoveryViewMode::Worktrees => format!("({})", worktree_count),
            RecoveryViewMode::All => format!("({}/{})", session_count, worktree_count),
        };

        // Dynamic action label based on what's selected
        let action_label = if state.is_worktree_selected() {
            " delete "
        } else {
            " archive "
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(CORNFLOWER_BLUE))
            .style(Style::default().bg(DARK_BG))
            .title(Line::from(vec![
                Span::styled("🔄 ", Style::default().fg(GOLD)),
                Span::styled(
                    title_text,
                    Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                ),
                Span::styled(" ", Style::default()),
                Span::styled(
                    count_text,
                    Style::default()
                        .fg(if total_count > 0 {
                            WARNING_ORANGE
                        } else {
                            SELECTION_GREEN
                        })
                        .add_modifier(Modifier::BOLD),
                ),
            ]))
            .title_bottom(Line::from(vec![
                Span::styled(
                    "Tab",
                    Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                ),
                Span::styled(" view ", Style::default().fg(MUTED_GRAY)),
                Span::styled("|", Style::default().fg(SUBDUED_BORDER)),
                Span::styled(" r", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
                Span::styled(" resume ", Style::default().fg(MUTED_GRAY)),
                Span::styled("|", Style::default().fg(SUBDUED_BORDER)),
                Span::styled(" d", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
                Span::styled(action_label, Style::default().fg(MUTED_GRAY)),
                Span::styled("|", Style::default().fg(SUBDUED_BORDER)),
                Span::styled(" R", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
                Span::styled(" refresh ", Style::default().fg(MUTED_GRAY)),
                Span::styled("|", Style::default().fg(SUBDUED_BORDER)),
                Span::styled(" A", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
                Span::styled(" recover all ", Style::default().fg(MUTED_GRAY)),
                Span::styled("|", Style::default().fg(SUBDUED_BORDER)),
                Span::styled(
                    " Space",
                    Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                ),
                Span::styled(" select ", Style::default().fg(MUTED_GRAY)),
                Span::styled("|", Style::default().fg(SUBDUED_BORDER)),
                Span::styled(" D", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
                Span::styled(" del selected ", Style::default().fg(MUTED_GRAY)),
            ]));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        // Layout: tabs at top, then list
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(2), Constraint::Min(0)])
            .split(inner);

        // Render tabs
        Self::render_view_tabs(frame, layout[0], state);

        // Loading state
        if state.loading {
            let loading = Paragraph::new("Loading...").style(Style::default().fg(MUTED_GRAY));
            frame.render_widget(loading, layout[1]);
            return;
        }

        // Empty state
        if total_count == 0 {
            let empty_msg = match state.view_mode {
                RecoveryViewMode::Sessions => "No orphaned sessions found",
                RecoveryViewMode::Worktrees => "No orphaned worktrees found",
                RecoveryViewMode::All => "No orphaned items found",
            };
            let empty_state = Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled(
                    format!("✓ {}", empty_msg),
                    Style::default().fg(SELECTION_GREEN),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "All items are either active or cleaned up.",
                    Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
                )),
            ]);
            frame.render_widget(empty_state, layout[1]);
            return;
        }

        // Build list items based on view mode
        let items = Self::build_list_items(state);
        let list = List::new(items);

        frame.render_stateful_widget(list, layout[1], &mut state.list_state);

        // Render recovery overlay on top if present
        if let Some(ref overlay) = state.recovery_overlay {
            Self::render_recovery_overlay(frame, area, overlay);
        }
    }

    /// Render the recovery results overlay as a centered popup
    fn render_recovery_overlay(frame: &mut Frame, area: Rect, overlay: &RecoveryOverlay) {
        // Size the popup
        let popup_width = (area.width * 70 / 100).min(80).max(40);
        let popup_height = (overlay.results.len() as u16 + 6).min(area.height * 70 / 100).max(8);
        let x = (area.width.saturating_sub(popup_width)) / 2 + area.x;
        let y = (area.height.saturating_sub(popup_height)) / 2 + area.y;
        let popup_area = Rect::new(x, y, popup_width, popup_height);

        // Clear background
        frame.render_widget(ratatui::widgets::Clear, popup_area);

        let has_failures = overlay.results.iter().any(|r| !r.success);
        let border_color = if has_failures {
            WARNING_ORANGE
        } else {
            SELECTION_GREEN
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(border_color))
            .style(Style::default().bg(PANEL_BG))
            .title(Line::from(vec![
                Span::styled(" ", Style::default()),
                Span::styled(
                    &overlay.title,
                    Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                ),
                Span::styled(" ", Style::default()),
            ]))
            .title_bottom(Line::from(vec![
                Span::styled(
                    " Esc",
                    Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                ),
                Span::styled(" dismiss ", Style::default().fg(MUTED_GRAY)),
            ]));

        let inner = block.inner(popup_area);
        frame.render_widget(block, popup_area);

        let mut lines = Vec::new();
        lines.push(Line::from(""));

        for result in &overlay.results {
            let (icon, color) = if result.success {
                ("✓", SELECTION_GREEN)
            } else {
                ("✗", WARNING_ORANGE)
            };

            // Truncate name to fit
            let max_name = (inner.width as usize).saturating_sub(6);
            let display_name = if result.name.len() > max_name {
                format!("{}…", &result.name[..max_name.saturating_sub(1)])
            } else {
                result.name.clone()
            };

            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {} ", icon),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(display_name, Style::default().fg(SOFT_WHITE)),
            ]));

            // Show detail on next line for failures
            if !result.success {
                let detail_max = (inner.width as usize).saturating_sub(8);
                let detail = if result.detail.len() > detail_max {
                    format!("{}…", &result.detail[..detail_max.saturating_sub(1)])
                } else {
                    result.detail.clone()
                };
                lines.push(Line::from(vec![
                    Span::styled("      ", Style::default()),
                    Span::styled(
                        detail,
                        Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
                    ),
                ]));
            }
        }

        let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
        frame.render_widget(paragraph, inner);
    }

    /// Render the view mode tabs
    fn render_view_tabs(frame: &mut Frame, area: Rect, state: &SessionRecoveryState) {
        let session_count = state.orphaned_sessions.len();
        let worktree_count = state.orphaned_worktrees.len();

        let tab_titles = vec![
            format!("Sessions ({})", session_count),
            format!("Worktrees ({})", worktree_count),
            format!("All ({})", session_count + worktree_count),
        ];

        let selected_idx = match state.view_mode {
            RecoveryViewMode::Sessions => 0,
            RecoveryViewMode::Worktrees => 1,
            RecoveryViewMode::All => 2,
        };

        let tabs = Tabs::new(tab_titles)
            .select(selected_idx)
            .style(Style::default().fg(MUTED_GRAY))
            .highlight_style(
                Style::default()
                    .fg(GOLD)
                    .add_modifier(Modifier::BOLD)
                    .add_modifier(Modifier::UNDERLINED),
            )
            .divider(Span::styled(" │ ", Style::default().fg(SUBDUED_BORDER)));

        frame.render_widget(tabs, area);
    }

    /// Build list items based on current view mode
    fn build_list_items(state: &SessionRecoveryState) -> Vec<ListItem<'static>> {
        let mut items = Vec::new();
        let mut current_idx = 0;

        // Add sessions if in Sessions or All view
        if matches!(
            state.view_mode,
            RecoveryViewMode::Sessions | RecoveryViewMode::All
        ) {
            for session in &state.orphaned_sessions {
                let is_selected = current_idx == state.selected_index;
                let is_marked = state.selected_items.contains(&current_idx);
                items.push(Self::render_session_item(session, is_selected, is_marked));
                current_idx += 1;
            }
        }

        // Add worktrees if in Worktrees or All view
        if matches!(
            state.view_mode,
            RecoveryViewMode::Worktrees | RecoveryViewMode::All
        ) {
            // Add separator in All view
            if state.view_mode == RecoveryViewMode::All
                && !state.orphaned_sessions.is_empty()
                && !state.orphaned_worktrees.is_empty()
            {
                items.push(ListItem::new(Line::from(vec![
                    Span::styled("── ", Style::default().fg(SUBDUED_BORDER)),
                    Span::styled(
                        "Worktrees ",
                        Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
                    ),
                    Span::styled("──────────────────", Style::default().fg(SUBDUED_BORDER)),
                ])));
                current_idx += 1; // Separator occupies a list position
            }

            for worktree in &state.orphaned_worktrees {
                let is_selected = current_idx == state.selected_index;
                let is_marked = state.selected_items.contains(&current_idx);
                items.push(Self::render_worktree_item(worktree, is_selected, is_marked));
                current_idx += 1;
            }
        }

        items
    }

    /// Render a single session list item
    fn render_session_item(
        session: &OrphanedSession,
        is_selected: bool,
        is_marked: bool,
    ) -> ListItem<'static> {
        let resume_indicator = if session.can_resume { "📄" } else { "⚠" };
        let time_indicator = if session.time_ago.is_empty() {
            String::new()
        } else {
            format!(" ({})", session.time_ago)
        };

        let task_preview: String =
            session.task.chars().take(30).collect::<String>().replace('\n', " ");

        let mut spans = vec![];
        // Cursor indicator
        if is_selected {
            spans.push(Span::styled("▶ ", Style::default().fg(SELECTION_GREEN)));
        } else {
            spans.push(Span::raw("  "));
        }
        // Multi-select checkbox
        if is_marked {
            spans.push(Span::styled(
                "[x] ",
                Style::default().fg(WARNING_ORANGE).add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::styled("[ ] ", Style::default().fg(MUTED_GRAY)));
        }

        spans.push(Span::styled(
            resume_indicator,
            if session.can_resume {
                Style::default().fg(SELECTION_GREEN)
            } else {
                Style::default().fg(WARNING_ORANGE)
            },
        ));
        spans.push(Span::raw(" "));

        spans.push(Span::styled(
            session.session.clone(),
            if is_selected {
                Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(SOFT_WHITE)
            },
        ));

        spans.push(Span::styled(
            time_indicator,
            Style::default().fg(MUTED_GRAY),
        ));

        let base_style = if is_selected {
            Style::default().bg(LIST_HIGHLIGHT_BG)
        } else {
            Style::default()
        };

        ListItem::new(vec![
            Line::from(spans),
            Line::from(vec![
                Span::raw("    "),
                Span::styled(
                    format!("{}...", task_preview),
                    Style::default().fg(MUTED_GRAY),
                ),
            ]),
        ])
        .style(base_style)
    }

    /// Render a single worktree list item
    fn render_worktree_item(
        worktree: &OrphanedWorktree,
        is_selected: bool,
        is_marked: bool,
    ) -> ListItem<'static> {
        // Determine if worktree is resumable (not a broken symlink)
        let can_resume = worktree.orphan_type != OrphanType::BrokenSymlink;
        let resume_indicator = if can_resume { "▶" } else { "✗" };
        let time_indicator = if worktree.time_ago.is_empty() {
            String::new()
        } else {
            format!(" ({})", worktree.time_ago)
        };

        let mut spans = vec![];
        // Cursor indicator
        if is_selected {
            spans.push(Span::styled("▶ ", Style::default().fg(SELECTION_GREEN)));
        } else {
            spans.push(Span::raw("  "));
        }
        // Multi-select checkbox
        if is_marked {
            spans.push(Span::styled(
                "[x] ",
                Style::default().fg(WARNING_ORANGE).add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::styled("[ ] ", Style::default().fg(MUTED_GRAY)));
        }

        // Show resume indicator
        spans.push(Span::styled(
            resume_indicator,
            if can_resume {
                Style::default().fg(SELECTION_GREEN)
            } else {
                Style::default().fg(WARNING_ORANGE)
            },
        ));
        spans.push(Span::raw(" "));

        // Truncate name if too long
        let display_name: String = worktree.name.chars().take(25).collect();
        spans.push(Span::styled(
            display_name,
            if is_selected {
                Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(SOFT_WHITE)
            },
        ));

        spans.push(Span::styled(
            time_indicator,
            Style::default().fg(MUTED_GRAY),
        ));

        let base_style = if is_selected {
            Style::default().bg(LIST_HIGHLIGHT_BG)
        } else {
            Style::default()
        };

        // Second line: branch or type label
        let branch_line = if let Some(ref branch) = worktree.branch {
            let branch_display: String = branch.chars().take(30).collect();
            Line::from(vec![
                Span::raw("    "),
                Span::styled(" ", Style::default().fg(SELECTION_GREEN)),
                Span::styled(branch_display, Style::default().fg(MUTED_GRAY)),
            ])
        } else {
            Line::from(vec![
                Span::raw("    "),
                Span::styled(
                    worktree.orphan_type.label().to_string(),
                    Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
                ),
            ])
        };

        ListItem::new(vec![Line::from(spans), branch_line]).style(base_style)
    }

    fn render_session_details(frame: &mut Frame, area: Rect, state: &SessionRecoveryState) {
        // Dynamic title based on what's selected
        let title = if state.is_worktree_selected() {
            "Worktree Details"
        } else {
            "Session Details"
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(CORNFLOWER_BLUE))
            .style(Style::default().bg(DARK_BG))
            .title(Line::from(vec![
                Span::styled("📋 ", Style::default().fg(GOLD)),
                Span::styled(
                    title,
                    Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                ),
            ]));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        // Show action result if any
        if let Some(ref result) = state.action_result {
            let result_area = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(3), Constraint::Min(0)])
                .split(inner);

            let result_widget = Paragraph::new(Line::from(vec![
                Span::styled("✓ ", Style::default().fg(SELECTION_GREEN)),
                Span::styled(result, Style::default().fg(SELECTION_GREEN)),
            ]))
            .wrap(Wrap { trim: true });

            frame.render_widget(result_widget, result_area[0]);

            // Show details below result
            if let Some(session) = state.selected() {
                Self::render_session_info(frame, result_area[1], session);
            } else if let Some(worktree) = state.selected_worktree() {
                Self::render_worktree_info(frame, result_area[1], worktree);
            }
            return;
        }

        // Show error if any
        if let Some(ref error) = state.last_error {
            let error_widget = Paragraph::new(Line::from(vec![
                Span::styled("⚠ Error: ", Style::default().fg(WARNING_ORANGE)),
                Span::styled(error, Style::default().fg(SOFT_WHITE)),
            ]))
            .wrap(Wrap { trim: true });

            frame.render_widget(error_widget, inner);
            return;
        }

        // Show selected item details
        if let Some(session) = state.selected() {
            Self::render_session_info(frame, inner, session);
        } else if let Some(worktree) = state.selected_worktree() {
            Self::render_worktree_info(frame, inner, worktree);
        } else {
            let empty = Paragraph::new(Span::styled(
                "Select an item to view details",
                Style::default().fg(MUTED_GRAY),
            ));
            frame.render_widget(empty, inner);
        }
    }

    fn render_session_info(frame: &mut Frame, area: Rect, session: &OrphanedSession) {
        let lines = vec![
            Line::from(vec![
                Span::styled("Session:  ", Style::default().fg(MUTED_GRAY)),
                Span::styled(&session.session, Style::default().fg(SOFT_WHITE)),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("Status:   ", Style::default().fg(MUTED_GRAY)),
                Span::styled(
                    if session.can_resume {
                        " Resumable"
                    } else {
                        " No transcript"
                    },
                    if session.can_resume {
                        Style::default().fg(SELECTION_GREEN)
                    } else {
                        Style::default().fg(WARNING_ORANGE)
                    },
                ),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("Created:  ", Style::default().fg(MUTED_GRAY)),
                Span::styled(&session.created, Style::default().fg(SOFT_WHITE)),
                if !session.time_ago.is_empty() {
                    Span::styled(
                        format!(" ({})", session.time_ago),
                        Style::default().fg(MUTED_GRAY),
                    )
                } else {
                    Span::raw("")
                },
            ]),
            Line::from(""),
            Line::from(vec![Span::styled(
                "Directory:",
                Style::default().fg(MUTED_GRAY),
            )]),
            Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(&session.directory, Style::default().fg(CORNFLOWER_BLUE)),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("Branch:   ", Style::default().fg(MUTED_GRAY)),
                if let Some(ref branch) = session.worktree_branch {
                    Span::styled(format!(" {}", branch), Style::default().fg(SELECTION_GREEN))
                } else {
                    Span::styled(
                        " Unknown",
                        Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
                    )
                },
            ]),
            Line::from(""),
            Line::from(vec![Span::styled("Task:", Style::default().fg(MUTED_GRAY))]),
            Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(
                    session.task.chars().take(200).collect::<String>(),
                    Style::default().fg(SOFT_WHITE),
                ),
            ]),
        ];

        let paragraph = Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .style(Style::default().bg(PANEL_BG));

        frame.render_widget(paragraph, area);
    }

    fn render_worktree_info(frame: &mut Frame, area: Rect, worktree: &OrphanedWorktree) {
        let mut lines = vec![
            Line::from(vec![
                Span::styled("Name:     ", Style::default().fg(MUTED_GRAY)),
                Span::styled(&worktree.name, Style::default().fg(SOFT_WHITE)),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("Type:     ", Style::default().fg(MUTED_GRAY)),
                Span::styled(
                    format!(
                        "{} {}",
                        worktree.orphan_type.icon(),
                        worktree.orphan_type.label()
                    ),
                    Style::default().fg(WARNING_ORANGE),
                ),
            ]),
            Line::from(""),
            Line::from(vec![Span::styled("Path:", Style::default().fg(MUTED_GRAY))]),
            Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(
                    worktree.path.to_string_lossy().to_string(),
                    Style::default().fg(CORNFLOWER_BLUE),
                ),
            ]),
            Line::from(""),
        ];

        // Branch
        lines.push(Line::from(vec![
            Span::styled("Branch:   ", Style::default().fg(MUTED_GRAY)),
            if let Some(ref branch) = worktree.branch {
                Span::styled(format!(" {}", branch), Style::default().fg(SELECTION_GREEN))
            } else {
                Span::styled(
                    " Unknown",
                    Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
                )
            },
        ]));
        lines.push(Line::from(""));

        // Source repo
        if let Some(ref repo) = worktree.source_repo {
            lines.push(Line::from(vec![
                Span::styled("Repo:     ", Style::default().fg(MUTED_GRAY)),
                Span::styled(repo, Style::default().fg(SOFT_WHITE)),
            ]));
            lines.push(Line::from(""));
        }

        // Last commit
        if let Some(ref commit) = worktree.last_commit {
            lines.push(Line::from(vec![Span::styled(
                "Commit:   ",
                Style::default().fg(MUTED_GRAY),
            )]));
            lines.push(Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(
                    commit.chars().take(60).collect::<String>(),
                    Style::default().fg(SOFT_WHITE),
                ),
            ]));
            lines.push(Line::from(""));
        }

        // Session ID if available
        if let Some(ref id) = worktree.id {
            lines.push(Line::from(vec![
                Span::styled("ID:       ", Style::default().fg(MUTED_GRAY)),
                Span::styled(id, Style::default().fg(MUTED_GRAY)),
            ]));
            lines.push(Line::from(""));
        }

        // Time ago
        if !worktree.time_ago.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("Modified: ", Style::default().fg(MUTED_GRAY)),
                Span::styled(&worktree.time_ago, Style::default().fg(SOFT_WHITE)),
            ]));
        }

        let paragraph = Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .style(Style::default().bg(PANEL_BG));

        frame.render_widget(paragraph, area);
    }
}
