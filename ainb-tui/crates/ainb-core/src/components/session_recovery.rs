// ABOUTME: Session recovery component for recovering orphaned agent sessions after crash/shutdown
// Displays orphaned sessions (tmux dead, worktree exists) and orphaned worktrees (broken symlinks, no container)
// Allows resume/cleanup actions for both types

use crate::app::ui_state::UiState;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, List, ListItem, Paragraph, Tabs, Wrap},
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;
use uuid::Uuid;

use crate::config::screen_model::fuzzy_matches;
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
    /// Durable session label from session-labels.json, keyed by tmux name.
    #[serde(default)]
    pub label: Option<String>,
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
    /// Durable session label, joined via sessions.json (worktrees carry no
    /// tmux name of their own, so the label has to come through the store).
    #[serde(default)]
    pub label: Option<String>,
}

/// Does an orphaned session pass the filter? Matches the fields an operator
/// would actually type: tmux name, durable label, branch and task text.
fn session_matches(session: &OrphanedSession, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let f = fuzzy_matches;
    f(&session.session, query)
        || session.label.as_deref().is_some_and(|l| f(l, query))
        || session.worktree_branch.as_deref().is_some_and(|b| f(b, query))
        || f(&session.task, query)
}

/// Worktree equivalent. Paths are deliberately left out: every row shares the
/// ~/.agents-in-a-box/worktrees prefix, so matching them makes short queries
/// hit everything.
fn worktree_matches(worktree: &OrphanedWorktree, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let f = fuzzy_matches;
    f(&worktree.name, query)
        || worktree.label.as_deref().is_some_and(|l| f(l, query))
        || worktree.branch.as_deref().is_some_and(|b| f(b, query))
        || worktree.source_repo.as_deref().is_some_and(|r| f(r, query))
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
            label: None,
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

/// A row of the visible (filtered) list, resolved back to its real index in
/// the underlying unfiltered vector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryRow {
    Session(usize),
    Worktree(usize),
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
    /// Whether data is being loaded
    pub loading: bool,
    /// Last error message
    pub last_error: Option<String>,
    /// Last action result message
    pub action_result: Option<String>,
    /// Bulk recovery result overlay (shown after multi-resume)
    pub recovery_overlay: Option<RecoveryOverlay>,
    /// Fuzzy filter query. ONE query shared by all three tabs, applied within
    /// whichever tab is showing: switching tabs re-filters the new tab's rows
    /// rather than each tab remembering a filter the operator cannot see.
    pub search_query: String,
    /// Whether the inline search bar has keyboard focus.
    pub search_active: bool,
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
        Self::empty()
    }
}

impl SessionRecoveryState {
    pub fn new() -> Self {
        let mut state = Self::empty();
        state.refresh();
        state
    }

    fn empty() -> Self {
        Self {
            orphaned_sessions: Vec::new(),
            orphaned_worktrees: Vec::new(),
            view_mode: RecoveryViewMode::default(),
            selected_index: 0,
            selected_items: HashSet::new(),
            loading: false,
            last_error: None,
            action_result: None,
            recovery_overlay: None,
            search_query: String::new(),
            search_active: false,
        }
    }

    /// Real indices into `orphaned_sessions` that pass the current filter.
    /// Recomputed on demand rather than cached: these lists are tens of rows,
    /// and a cache is one more thing to invalidate on every keystroke.
    pub fn visible_sessions(&self) -> Vec<usize> {
        self.orphaned_sessions
            .iter()
            .enumerate()
            .filter(|(_, s)| session_matches(s, &self.search_query))
            .map(|(i, _)| i)
            .collect()
    }

    /// Real indices into `orphaned_worktrees` that pass the current filter.
    pub fn visible_worktrees(&self) -> Vec<usize> {
        self.orphaned_worktrees
            .iter()
            .enumerate()
            .filter(|(_, w)| worktree_matches(w, &self.search_query))
            .map(|(i, _)| i)
            .collect()
    }

    /// Resolve a view position (what `selected_index` and `selected_items`
    /// both store) back to a real index. THE choke point: every action goes
    /// through here, so a filtered list can never resume or delete the row
    /// underneath the one the operator is looking at.
    pub fn row_at(&self, view_index: usize) -> Option<RecoveryRow> {
        match self.view_mode {
            RecoveryViewMode::Sessions => {
                self.visible_sessions().get(view_index).copied().map(RecoveryRow::Session)
            }
            RecoveryViewMode::Worktrees => {
                self.visible_worktrees().get(view_index).copied().map(RecoveryRow::Worktree)
            }
            RecoveryViewMode::All => {
                let sessions = self.visible_sessions();
                if view_index < sessions.len() {
                    return Some(RecoveryRow::Session(sessions[view_index]));
                }
                let worktrees = self.visible_worktrees();
                let offset = usize::from(!sessions.is_empty() && !worktrees.is_empty());
                // checked_sub, not a bare `-`: with the filter hiding every
                // session there is no separator either, so the subtraction
                // would underflow instead of landing on worktree 0.
                let wt_pos = view_index.checked_sub(sessions.len() + offset)?;
                worktrees.get(wt_pos).copied().map(RecoveryRow::Worktree)
            }
        }
    }

    /// Get the total count of visible items in current view (includes separator in All view)
    pub fn current_view_count(&self) -> usize {
        match self.view_mode {
            RecoveryViewMode::Sessions => self.visible_sessions().len(),
            RecoveryViewMode::Worktrees => self.visible_worktrees().len(),
            RecoveryViewMode::All => {
                self.visible_sessions().len()
                    + self.visible_worktrees().len()
                    + self.separator_offset()
            }
        }
    }

    /// Returns 1 if a separator exists in All view (both sessions and worktrees visible), 0 otherwise
    fn separator_offset(&self) -> usize {
        if self.view_mode == RecoveryViewMode::All
            && !self.visible_sessions().is_empty()
            && !self.visible_worktrees().is_empty()
        {
            1
        } else {
            0
        }
    }

    /// The list index where the separator lives (only valid when separator_offset() == 1)
    fn separator_index(&self) -> usize {
        self.visible_sessions().len()
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
                matches!(
                    self.row_at(self.selected_index),
                    Some(RecoveryRow::Worktree(_))
                )
            }
        }
    }

    /// Get the index of the selected worktree within `orphaned_worktrees`
    /// (a REAL index, already mapped back through the filter).
    pub fn worktree_index(&self) -> Option<usize> {
        match self.row_at(self.selected_index)? {
            RecoveryRow::Worktree(idx) => Some(idx),
            RecoveryRow::Session(_) => None,
        }
    }

    /// Push a char into the filter. View positions mean something different
    /// after every keystroke, so the selection and the marks are re-anchored.
    pub fn search_push(&mut self, c: char) {
        self.search_query.push(c);
        self.after_query_change();
    }

    pub fn search_pop(&mut self) {
        self.search_query.pop();
        self.after_query_change();
    }

    /// Esc: drop the filter entirely and close the bar. Enter, by contrast,
    /// only clears `search_active` so the narrowed list stays actionable.
    pub fn search_cancel(&mut self) {
        self.search_query.clear();
        self.search_active = false;
        self.after_query_change();
    }

    fn after_query_change(&mut self) {
        self.selected_index = 0;
        // Marks are view positions, not identities: a query change silently
        // re-points them at other rows, and `D` deletes worktrees. Drop them.
        self.selected_items.clear();
    }

    /// Toggle to next view mode
    pub fn toggle_view_mode(&mut self) {
        self.view_mode = self.view_mode.next();
        self.selected_index = 0;
        self.selected_items.clear();
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
        // Loaded once for the whole scan: the store is a single JSON file and
        // this loop runs per orphan file.
        let labels = crate::config::SessionLabelStore::load();

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
                            .args(["has-session", "-t", &format!("={session}")])
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
                            // `session` IS the tmux session name, which is the
                            // key the label store is written under.
                            label: labels.get(&session).cloned(),
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
                                            .args(["has-session", "-t", &format!("={id}")])
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

        // Enrich orphaned worktrees with agent_type and label from sessions.json
        Self::enrich_worktrees(
            &mut orphaned,
            &SessionStore::load(),
            &crate::config::SessionLabelStore::load(),
        );

        // Sort by time_ago (most recent first based on directory mtime)
        orphaned.sort_by(|a, b| {
            let a_mtime = std::fs::metadata(&a.path).and_then(|m| m.modified()).ok();
            let b_mtime = std::fs::metadata(&b.path).and_then(|m| m.modified()).ok();
            b_mtime.cmp(&a_mtime)
        });

        Ok(orphaned)
    }

    /// Join each orphaned worktree back to its sessions.json entry. The path is
    /// the only bridge a worktree has: it carries no tmux name of its own, and
    /// the tmux name is the key the label store is written under.
    ///
    /// Split out of the scan so the join itself is testable without a home
    /// directory full of real worktrees.
    fn enrich_worktrees(
        worktrees: &mut [OrphanedWorktree],
        store: &SessionStore,
        labels: &crate::config::SessionLabelStore,
    ) {
        for worktree in worktrees {
            for metadata in store.sessions().values() {
                if metadata.worktree_path == worktree.path {
                    worktree.agent_type = Some(metadata.agent_type);
                    worktree.label = labels.get(&metadata.tmux_session_name).cloned();
                    break;
                }
            }
        }
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
            label: None,      // Enriched later from sessions.json
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
    }

    /// Get the selected session (only valid when not in worktree selection)
    pub fn selected(&self) -> Option<&OrphanedSession> {
        if self.is_worktree_selected() {
            return None;
        }
        match self.row_at(self.selected_index)? {
            RecoveryRow::Session(idx) => self.orphaned_sessions.get(idx),
            RecoveryRow::Worktree(_) => None,
        }
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
        let check_result = Command::new("tmux")
            .args(["has-session", "-t", &format!("={new_session}")])
            .output();
        if check_result.map(|o| o.status.success()).unwrap_or(false) {
            // Kill existing session to avoid conflicts
            // Exact target, never a prefix match.
            let _ = Command::new("tmux")
                .args(["kill-session", "-t", &format!("={new_session}")])
                .output();
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
            skip_permissions: None,
            model: None,
            model_source: Default::default(),
            codex_model: None,
            codex_thread_id: None,
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

        // Orphan recovery is always a resume. Build the command via the shared
        // launch/resume assembler so this screen stays in lock-step with the
        // main resume path: Claude `--continue`, Codex `resume --last`, and the
        // correct per-provider yolo flag (the old hardcoded strings used the
        // wrong codex flag and the broken `claude --resume <path>`). The pane's
        // cwd is the worktree (`tmux new-session -c <worktree>`), so the
        // cwd-scoped resume resolves to this worktree's latest session.
        // `has_history` gates Claude's `--continue` — no history → fresh launch,
        // avoiding a dead pane.
        use crate::app::state::AppState;
        let has_history = agent_type == SessionAgentType::Claude
            && AppState::find_latest_transcript(&worktree.path).is_some();
        if agent_type == SessionAgentType::Claude && !has_history {
            tracing::info!(
                worktree = %worktree.path.display(),
                "no Claude transcript found for worktree; recovery launches a fresh session"
            );
        }

        let agent_cmd = match agent_type {
            SessionAgentType::Shell | SessionAgentType::Ssh | SessionAgentType::Kiro => {
                String::new() // Just open a shell, no command
            }
            _ => {
                use crate::config::CliProvider;
                use crate::interactive::session_manager::InteractiveSessionManager;
                let provider = match agent_type {
                    SessionAgentType::Codex => CliProvider::Codex,
                    SessionAgentType::Gemini => CliProvider::Gemini,
                    SessionAgentType::Copilot => CliProvider::Copilot,
                    SessionAgentType::Antigravity => CliProvider::Antigravity,
                    _ => CliProvider::Claude,
                };
                InteractiveSessionManager::build_cli_cmd_parts(
                    &provider,
                    agent_type,
                    true, // skip_permissions — recovery launches yolo, as before
                    None, // model
                    true, // resume_requested — orphan recovery is a resume
                    has_history,
                )
                .join(" ")
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
        // Acts on the VISIBLE rows: under a filter, "recover all" quietly
        // recovering rows the operator cannot see is worse than the filter.
        let worktrees: Vec<_> = self
            .visible_worktrees()
            .into_iter()
            .map(|i| self.orphaned_worktrees[i].clone())
            .collect();
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

        // Guard: `can_resume` already implies a transcript exists; double-check.
        session.transcript_path.as_ref().ok_or("No transcript path")?;

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

        // `--continue` re-opens the most recent conversation in `directory`
        // (the pane's cwd). The old `--resume "<transcript path>"` silently fell
        // through to the claude picker — the current CLI `--resume` expects a
        // session id, not a path. A transcript is known to exist (can_resume).
        let claude_cmd = "claude --dangerously-skip-permissions --continue".to_string();
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
            // Marks are view positions; row_at maps each back to the row the
            // operator actually marked, filter or no filter.
            let (name, outcome) = match self.row_at(idx) {
                Some(RecoveryRow::Session(i)) => {
                    let session = self.orphaned_sessions[i].clone();
                    (
                        session.session.clone(),
                        Self::resume_single_session(&session),
                    )
                }
                Some(RecoveryRow::Worktree(i)) => {
                    let worktree = self.orphaned_worktrees[i].clone();
                    (
                        worktree.name.clone(),
                        Self::resume_single_worktree(&worktree),
                    )
                }
                None => continue,
            };
            match outcome {
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
            // Same mapping as resume: this arm deletes worktree directories,
            // so acting on a raw view index under a filter is destructive.
            let ok = match self.row_at(idx) {
                Some(RecoveryRow::Session(i)) => {
                    let session = self.orphaned_sessions[i].clone();
                    Self::archive_session_by_name(&session.session).is_ok()
                }
                Some(RecoveryRow::Worktree(i)) => {
                    let worktree = self.orphaned_worktrees[i].clone();
                    Self::cleanup_single_worktree(&worktree).is_ok()
                }
                None => continue,
            };
            if ok {
                deleted += 1;
            } else {
                failed += 1;
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
    pub fn render(frame: &mut Frame, area: Rect, state: &SessionRecoveryState, ui: &mut UiState) {
        // Main layout: list on left, details on right
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
            .split(area);

        Self::render_session_list(frame, chunks[0], state, ui);
        Self::render_session_details(frame, chunks[1], state);
    }

    fn render_session_list(
        frame: &mut Frame,
        area: Rect,
        state: &SessionRecoveryState,
        ui: &mut UiState,
    ) {
        // Visible counts, not raw ones: a header saying (5) over a filtered
        // list of 1 reads as a broken filter.
        let session_count = state.visible_sessions().len();
        let worktree_count = state.visible_worktrees().len();
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
                // Filter leads the footer. Appended last it fell off the right
                // edge at 100 and 140 cols, hiding the only on-screen
                // affordance for the feature.
                Span::styled("/", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
                Span::styled(" filter ", Style::default().fg(MUTED_GRAY)),
                Span::styled("|", Style::default().fg(SUBDUED_BORDER)),
                Span::styled(
                    " Tab",
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

        // Layout: tabs at top, optional search row, then list. The search row
        // is zero-height while no filter is in play, so an empty query renders
        // exactly the panel it rendered before search existed.
        let show_search = state.search_active || !state.search_query.is_empty();
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2),
                Constraint::Length(u16::from(show_search)),
                Constraint::Min(0),
            ])
            .split(inner);

        // Render tabs
        Self::render_view_tabs(frame, layout[0], state);

        if show_search {
            let mut spans = vec![
                Span::styled("/", Style::default().fg(GOLD)),
                Span::styled(
                    state.search_query.clone(),
                    Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD),
                ),
            ];
            if state.search_active {
                spans.push(Span::styled("▏", Style::default().fg(SELECTION_GREEN)));
            }
            frame.render_widget(Paragraph::new(Line::from(spans)), layout[1]);
        }

        // Loading state
        if state.loading {
            let loading = Paragraph::new("Loading...").style(Style::default().fg(MUTED_GRAY));
            frame.render_widget(loading, layout[2]);
            return;
        }

        // Empty state. A filter that hides everything is not the same news as
        // having nothing to recover, so it gets its own line.
        if total_count == 0 && !state.search_query.is_empty() {
            let no_matches = Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled(
                    format!("No matches for \"{}\"", state.search_query),
                    Style::default().fg(WARNING_ORANGE),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "Esc clears the filter.",
                    Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
                )),
            ]);
            frame.render_widget(no_matches, layout[2]);
            return;
        }

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
            frame.render_widget(empty_state, layout[2]);
            return;
        }

        // Build list items based on view mode
        let items = Self::build_list_items(state, layout[2].width);
        let list = List::new(items);

        // Selection is core's; only the scroll offset is the widget's own.
        // The clamp is what `after_query_change` used to do by resetting the
        // offset: a filter can shrink the list under a scrolled viewport, and
        // an offset past the end paints an empty pane.
        let list_state = &mut ui.session_recovery_list;
        list_state.select((total_count > 0).then_some(state.selected_index));
        let offset = list_state.offset_mut();
        *offset = (*offset).min(total_count.saturating_sub(1));
        frame.render_stateful_widget(list, layout[2], list_state);

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
        let session_count = state.visible_sessions().len();
        let worktree_count = state.visible_worktrees().len();

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
    fn build_list_items(state: &SessionRecoveryState, pane_width: u16) -> Vec<ListItem<'static>> {
        let budget = Self::row_name_budget(pane_width);
        let mut items = Vec::new();
        let mut current_idx = 0;
        // Iterate the SAME filtered sets row_at resolves against, so a list
        // position and a selected_index always mean the same row.
        let sessions = state.visible_sessions();
        let worktrees = state.visible_worktrees();

        // Add sessions if in Sessions or All view
        if matches!(
            state.view_mode,
            RecoveryViewMode::Sessions | RecoveryViewMode::All
        ) {
            for session in sessions.iter().map(|i| &state.orphaned_sessions[*i]) {
                let is_selected = current_idx == state.selected_index;
                let is_marked = state.selected_items.contains(&current_idx);
                items.push(Self::render_session_item(
                    session,
                    is_selected,
                    is_marked,
                    budget,
                ));
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
                && !sessions.is_empty()
                && !worktrees.is_empty()
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

            for worktree in worktrees.iter().map(|i| &state.orphaned_worktrees[*i]) {
                let is_selected = current_idx == state.selected_index;
                let is_marked = state.selected_items.contains(&current_idx);
                items.push(Self::render_worktree_item(
                    worktree,
                    is_selected,
                    is_marked,
                    budget,
                ));
                current_idx += 1;
            }
        }

        items
    }

    /// Columns a list row may spend on "<label> · <name>", derived from the
    /// pane it is painted into. A row spends 8 columns on cursor, checkbox and
    /// status glyph and 10 on the trailing " (2h ago)", so the string gets
    /// whatever is left. Floor of 12 keeps a pathologically narrow pane from
    /// collapsing the name to nothing.
    ///
    /// Fixed at 25 before this was width-aware, which was right at 100 columns
    /// and wasted half a wide terminal's pane on every labelled row. The 10
    /// reproduces that 25 exactly at 100 columns; a four-digit age
    /// (" (1440m ago)") is one column over and clips as it did before.
    fn row_name_budget(pane_width: u16) -> usize {
        (pane_width as usize).saturating_sub(8 + 10).max(12)
    }

    /// Name with its durable session label in front, matching the session
    /// list's "<label> · <name>" shape. An unlabeled row returns the bare name,
    /// so it keeps exactly the columns it has today with no stray separator.
    fn with_label(label: Option<&String>, name: &str, budget: usize) -> String {
        let Some(label) = label else {
            return name.chars().take(budget).collect();
        };
        // The two halves share ONE budget. Giving each of them the full budget
        // pushed " (2h ago)" off the row, and age is the signal an operator
        // sorts a recovery list by. Label is capped at half so the name it
        // prefixes never collapses to a couple of characters.
        let label: String = label.chars().take(budget / 2).collect();
        let name_budget = budget.saturating_sub(label.chars().count() + 3);
        format!(
            "{label} · {}",
            name.chars().take(name_budget).collect::<String>()
        )
    }

    /// Render a single session list item
    fn render_session_item(
        session: &OrphanedSession,
        is_selected: bool,
        is_marked: bool,
        budget: usize,
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
            Self::with_label(session.label.as_ref(), &session.session, budget),
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
        budget: usize,
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

        // with_label owns the width budget for both halves of the string.
        spans.push(Span::styled(
            Self::with_label(worktree.label.as_ref(), &worktree.name, budget),
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

    /// Detail-pane row for a durable session label, or a blank line when the
    /// item has none, so an unlabeled item's layout does not shift.
    fn label_line(label: Option<&String>) -> Line<'static> {
        match label {
            Some(label) => Line::from(vec![
                Span::styled("Label:    ", Style::default().fg(MUTED_GRAY)),
                Span::styled(label.clone(), Style::default().fg(GOLD)),
            ]),
            None => Line::from(""),
        }
    }

    fn render_session_info(frame: &mut Frame, area: Rect, session: &OrphanedSession) {
        let lines = vec![
            Line::from(vec![
                Span::styled("Session:  ", Style::default().fg(MUTED_GRAY)),
                Span::styled(&session.session, Style::default().fg(SOFT_WHITE)),
            ]),
            // The label is what the operator named this run; the tmux name
            // above is machinery. A row that carries one in the list has to
            // carry it here too, or the two halves of the panel disagree
            // about which session is selected.
            Self::label_line(session.label.as_ref()),
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
            Self::label_line(worktree.label.as_ref()),
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

#[cfg(test)]
mod tests {
    use super::{
        OrphanType, OrphanedSession, OrphanedWorktree, RecoveryRow, RecoveryViewMode,
        SessionMetadata, SessionRecovery, SessionRecoveryState, SessionStore,
    };
    use crate::config::SessionLabelStore;
    use crate::models::SessionAgentType;
    use ratatui::{Terminal, backend::TestBackend};
    use std::path::PathBuf;
    use uuid::Uuid;

    fn orphan_session(name: &str, label: Option<&str>) -> OrphanedSession {
        OrphanedSession {
            session: name.to_string(),
            task: "some task".to_string(),
            directory: "/tmp/ainb-recovery".to_string(),
            created: String::new(),
            status: "running".to_string(),
            transcript_path: None,
            worktree_branch: None,
            can_resume: false,
            time_ago: String::new(),
            label: label.map(str::to_string),
        }
    }

    /// Render the panel and return the buffer as lines, so a row assertion can
    /// name the row rather than only ask whether text exists somewhere.
    fn render_to_lines(state: &SessionRecoveryState, w: u16, h: u16) -> Vec<String> {
        let mut ui = crate::app::ui_state::UiState::default();
        let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("terminal");
        terminal
            .draw(|frame| SessionRecovery::render(frame, frame.area(), state, &mut ui))
            .expect("draw");
        let buf = terminal.backend().buffer().clone();
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buf.cell((x, y)).map_or(" ", |c| c.symbol()).to_string())
                    .collect::<String>()
            })
            .collect()
    }

    /// A recoverable session row carries the same durable label the session
    /// list shows, in the same "<label> · <name>" shape, and the age column it
    /// had before labels existed still fits beside it.
    ///
    /// Sized like a real row on purpose: a 30-char tmux name, a 23-char label
    /// and a narrow 100-col terminal. A short name in a wide terminal cannot
    /// catch the label overflowing the pane.
    #[test]
    fn recovery_session_row_paints_its_label() {
        let mut state = SessionRecoveryState::default();
        state.view_mode = RecoveryViewMode::Sessions;
        let mut session = orphan_session(
            "ainb-save-prefix-abeb7e09-a02f",
            Some("RPC flake investigation"),
        );
        session.time_ago = "2h ago".to_string();
        state.orphaned_sessions = vec![session];

        let lines = render_to_lines(&mut state, 100, 20);
        let row = lines
            .iter()
            .find(|line| line.contains("[ ] ") && line.contains("RPC flake in"))
            .unwrap_or_else(|| panic!("recovery row lost its label: {lines:#?}"));
        assert!(
            row.contains(" · ainb-save"),
            "label ate the whole name: {row}"
        );
        assert!(
            row.contains("(2h ago)"),
            "label pushed the age off the row: {row}"
        );
    }

    /// An unlabeled row must render exactly as it did before labels existed:
    /// bare name, no dangling separator.
    #[test]
    fn recovery_row_without_a_label_is_unchanged() {
        let mut state = SessionRecoveryState::default();
        state.view_mode = RecoveryViewMode::Sessions;
        state.orphaned_sessions = vec![orphan_session("tmux_plain", None)];

        let lines = render_to_lines(&mut state, 120, 20);
        // Anchor on the checkbox: the details pane on the right prints the bare
        // session name too, and matching that line instead means this guard
        // never looks at the list row it is guarding.
        let row = lines
            .iter()
            .find(|line| line.contains("[ ] ") && line.contains("tmux_plain"))
            .expect("row rendered");
        assert!(!row.contains('·'), "unlabeled row grew a separator: {row}");
    }

    /// Worktree rows join their label through sessions.json, so they get the
    /// same prefix treatment as session rows, under the same width budget.
    /// Real worktree names run past 30 chars, which is exactly where an
    /// unbudgeted label prefix starts eating the age column.
    #[test]
    fn recovery_worktree_row_paints_its_label() {
        let mut state = SessionRecoveryState::default();
        state.view_mode = RecoveryViewMode::Worktrees;
        state.orphaned_worktrees = vec![OrphanedWorktree {
            name: "agents-in-a-box--save-prefix-abeb".to_string(),
            orphan_type: OrphanType::NoTmux,
            label: Some("prove-hangar-acp-leg-b-p3".to_string()),
            time_ago: "2h ago".to_string(),
            ..Default::default()
        }];

        let lines = render_to_lines(&mut state, 100, 20);
        let row = lines
            .iter()
            .find(|line| line.contains("[ ] ") && line.contains("prove-hangar"))
            .unwrap_or_else(|| panic!("recovery worktree row lost its label: {lines:#?}"));
        assert!(
            row.contains(" · agents-in"),
            "label ate the whole name: {row}"
        );
        assert!(
            row.contains("(2h ago)"),
            "label pushed the age off the row: {row}"
        );
    }

    /// A wide terminal gives the list pane far more than the 25 columns the
    /// budget was once hard-coded to, and a labelled row is exactly the row
    /// that pays for the shortfall. At 200 cols both halves must survive whole.
    #[test]
    fn a_wide_terminal_stops_truncating_a_labelled_row() {
        let mut state = SessionRecoveryState::default();
        state.view_mode = RecoveryViewMode::Worktrees;
        state.orphaned_worktrees = vec![OrphanedWorktree {
            name: "agents-in-a-box--save-prefix-abeb".to_string(),
            orphan_type: OrphanType::NoTmux,
            label: Some("prove-hangar-acp-leg-b-p3".to_string()),
            time_ago: "2h ago".to_string(),
            ..Default::default()
        }];

        let lines = render_to_lines(&mut state, 200, 20);
        let row = lines
            .iter()
            .find(|line| line.contains("[ ] ") && line.contains("prove-hangar"))
            .unwrap_or_else(|| panic!("wide row lost its label: {lines:#?}"));
        assert!(
            row.contains("prove-hangar-acp-leg-b-p3 · agents-in-a-box--save-prefix-abeb"),
            "a 200-col pane still truncated a row that fits: {row}"
        );
        assert!(row.contains("(2h ago)"), "wide row lost its age: {row}");
    }

    /// The details pane is the other half of the recovery panel. A row that
    /// carries a label in the list has to carry it here too, or the operator
    /// reads a bare tmux name and cannot tell which run is selected.
    #[test]
    fn the_details_pane_names_the_selected_row_by_its_label() {
        let mut state = SessionRecoveryState::default();
        state.view_mode = RecoveryViewMode::Worktrees;
        state.orphaned_worktrees = vec![OrphanedWorktree {
            name: "agents-in-a-box--save-prefix-abeb".to_string(),
            orphan_type: OrphanType::NoTmux,
            label: Some("prove-hangar-acp-leg-b-p3".to_string()),
            time_ago: "2h ago".to_string(),
            ..Default::default()
        }];
        state.selected_index = 0;

        let lines = render_to_lines(&mut state, 200, 20);
        assert!(
            lines
                .iter()
                .any(|line| line.contains("Label:") && line.contains("prove-hangar-acp-leg-b-p3")),
            "details pane shows no label for the selected row: {lines:#?}"
        );
    }

    /// The footer is the only place that tells an operator `/` filters. It has
    /// more hints than fit, so the one the bug report asked for has to survive
    /// a narrow terminal.
    #[test]
    fn the_filter_hint_survives_a_narrow_terminal() {
        let mut state = SessionRecoveryState::default();
        state.view_mode = RecoveryViewMode::Sessions;
        state.orphaned_sessions = vec![orphan_session("alpha_one", None)];

        let lines = render_to_lines(&mut state, 100, 20);
        assert!(
            lines.iter().any(|line| line.contains("/ filter")),
            "the filter hint is off-screen at 100 cols: {lines:#?}"
        );
    }

    /// The production join for a worktree label: path -> sessions.json entry ->
    /// tmux name -> label store. Every other label test hands the row a `label`
    /// it built itself, which proves the renderer and nothing about the join.
    #[test]
    fn a_worktree_joins_its_label_through_sessions_json() {
        let worktree_path = PathBuf::from("/tmp/ainb-recovery/by-name/save-prefix");
        let mut store = SessionStore::default();
        store.upsert(SessionMetadata {
            session_id: Uuid::nil(),
            tmux_session_name: "ainb-save-prefix".to_string(),
            worktree_path: worktree_path.clone(),
            workspace_name: "save-prefix".to_string(),
            created_at: chrono::Utc::now(),
            agent_type: SessionAgentType::default(),
            headroom_enabled: false,
            rtk_enabled: false,
            skip_permissions: None,
            model: None,
            model_source: Default::default(),
            codex_model: None,
            codex_thread_id: None,
        });
        let mut labels = SessionLabelStore::default();
        labels.set(
            "ainb-save-prefix".to_string(),
            Some("RPC flake".to_string()),
        );

        let mut rows = vec![
            OrphanedWorktree {
                path: worktree_path,
                ..orphan_worktree("save-prefix", None)
            },
            // No sessions.json entry: nothing bridges this row to a label.
            orphan_worktree("stranger", None),
        ];
        SessionRecoveryState::enrich_worktrees(&mut rows, &store, &labels);

        assert_eq!(rows[0].label.as_deref(), Some("RPC flake"));
        assert_eq!(
            rows[1].label, None,
            "a row with no metadata invented a label"
        );
    }

    fn orphan_worktree(name: &str, branch: Option<&str>) -> OrphanedWorktree {
        OrphanedWorktree {
            name: name.to_string(),
            branch: branch.map(str::to_string),
            orphan_type: OrphanType::NoTmux,
            ..Default::default()
        }
    }

    /// Three sessions, a query that only the third can match. The list must
    /// show that row and nothing else.
    #[test]
    fn filter_narrows_the_visible_rows() {
        let mut state = SessionRecoveryState::default();
        state.view_mode = RecoveryViewMode::Sessions;
        state.orphaned_sessions = vec![
            orphan_session("alpha_one", None),
            orphan_session("beta_two", None),
            orphan_session("gamma_three", None),
        ];
        for c in "gamma".chars() {
            state.search_push(c);
        }

        let lines = render_to_lines(&mut state, 120, 20);
        let joined = lines.join("\n");
        assert!(
            joined.contains("gamma_three"),
            "filtered row missing: {lines:#?}"
        );
        assert!(
            !joined.contains("alpha_one"),
            "filtered-out row still painted: {lines:#?}"
        );
        assert!(
            !joined.contains("beta_two"),
            "filtered-out row still painted: {lines:#?}"
        );
    }

    /// THE trap: under a filter, view row 0 is NOT raw row 0. Resume and
    /// archive both read `selected()`, so this is what stops the panel acting
    /// on a session the operator never looked at.
    #[test]
    fn filtered_selection_resolves_to_the_underlying_row() {
        let mut state = SessionRecoveryState::default();
        state.view_mode = RecoveryViewMode::Sessions;
        state.orphaned_sessions = vec![
            orphan_session("alpha_one", None),
            orphan_session("beta_two", None),
            orphan_session("gamma_three", None),
        ];
        for c in "gamma".chars() {
            state.search_push(c);
        }

        assert_eq!(state.selected_index, 0, "filter must re-anchor the cursor");
        assert_eq!(
            state.selected().map(|s| s.session.as_str()),
            Some("gamma_three"),
            "view row 0 resolved to the wrong session"
        );
        // Multi-select marks are view positions too, so they map the same way.
        assert_eq!(state.row_at(0), Some(RecoveryRow::Session(2)));
        // And the highlighted row in the painted buffer is that same row.
        let lines = render_to_lines(&mut state, 120, 20);
        let cursor_row = lines.iter().find(|l| l.contains('▶')).expect("a row is selected");
        assert!(
            cursor_row.contains("gamma_three"),
            "cursor is on the wrong row: {cursor_row}"
        );
    }

    /// A label from bug 1 is a filter field: the operator types the prefix
    /// they see, not the tmux name they do not.
    #[test]
    fn filter_matches_the_durable_label() {
        let mut state = SessionRecoveryState::default();
        state.view_mode = RecoveryViewMode::Sessions;
        state.orphaned_sessions = vec![
            orphan_session("tmux_aaa", Some("RPC flake")),
            orphan_session("tmux_bbb", None),
        ];
        for c in "rpc".chars() {
            state.search_push(c);
        }

        assert_eq!(state.current_view_count(), 1);
        assert_eq!(
            state.selected().map(|s| s.session.as_str()),
            Some("tmux_aaa")
        );
    }

    /// All view with every session filtered out. The old index math computed
    /// `0 + 0 - 1` here; this must resolve to the first worktree instead of
    /// panicking or silently selecting nothing.
    #[test]
    fn all_view_survives_filtering_every_session_away() {
        let mut state = SessionRecoveryState::default();
        state.view_mode = RecoveryViewMode::All;
        state.orphaned_sessions = vec![orphan_session("alpha_one", None)];
        state.orphaned_worktrees = vec![
            orphan_worktree("wt-keep", Some("feat/keep")),
            orphan_worktree("wt-drop", None),
        ];
        for c in "keep".chars() {
            state.search_push(c);
        }

        assert_eq!(
            state.current_view_count(),
            1,
            "only the matching worktree is visible"
        );
        assert!(state.is_worktree_selected());
        assert_eq!(state.worktree_index(), Some(0));
        assert_eq!(
            state.selected_worktree().map(|w| w.name.as_str()),
            Some("wt-keep")
        );
        let lines = render_to_lines(&mut state, 120, 20);
        assert!(lines.join("\n").contains("wt-keep"));
    }

    /// A filtered All view still maps worktree rows past the separator back to
    /// the right worktree, not the one at the same raw offset.
    #[test]
    fn all_view_maps_worktree_rows_past_the_separator() {
        let mut state = SessionRecoveryState::default();
        state.view_mode = RecoveryViewMode::All;
        state.orphaned_sessions = vec![
            orphan_session("drop_me", None),
            orphan_session("keep_session", None),
        ];
        state.orphaned_worktrees = vec![
            orphan_worktree("drop-wt", None),
            orphan_worktree("keep-wt", None),
        ];
        for c in "keep".chars() {
            state.search_push(c);
        }

        // View is [keep_session][separator][keep-wt].
        assert_eq!(state.row_at(0), Some(RecoveryRow::Session(1)));
        assert_eq!(state.row_at(1), None, "the separator is not an item");
        assert_eq!(state.row_at(2), Some(RecoveryRow::Worktree(1)));
    }

    /// A query change re-points every view position, so stale marks would
    /// delete the wrong worktree. They get dropped instead.
    #[test]
    fn a_query_change_drops_multi_select_marks() {
        let mut state = SessionRecoveryState::default();
        state.view_mode = RecoveryViewMode::Sessions;
        state.orphaned_sessions = vec![
            orphan_session("alpha_one", None),
            orphan_session("beta_two", None),
        ];
        state.toggle_select();
        assert!(state.has_multi_selection());

        state.search_push('b');
        assert!(
            !state.has_multi_selection(),
            "marks survived a query change"
        );
    }

    /// Esc drops the filter and puts every row back, byte for byte.
    #[test]
    fn escape_restores_the_unfiltered_list() {
        let mut state = SessionRecoveryState::default();
        state.view_mode = RecoveryViewMode::Sessions;
        state.orphaned_sessions = vec![
            orphan_session("alpha_one", None),
            orphan_session("beta_two", None),
            orphan_session("gamma_three", None),
        ];
        let baseline = render_to_lines(&mut state, 120, 20);

        state.search_active = true;
        for c in "gamma".chars() {
            state.search_push(c);
        }
        // Not assert_ne! against the baseline: the search bar row alone makes
        // the buffers differ, so that check passes even with the filter
        // disconnected. Name the rows that must and must not survive.
        let filtered = render_to_lines(&mut state, 120, 20).join("\n");
        assert!(
            filtered.contains("gamma_three"),
            "filter hid the match: {filtered}"
        );
        assert!(
            !filtered.contains("alpha_one"),
            "filter kept a non-match: {filtered}"
        );

        state.search_cancel();
        assert!(!state.search_active);
        assert_eq!(
            render_to_lines(&mut state, 120, 20),
            baseline,
            "an empty query must render exactly the unfiltered panel"
        );
    }

    /// A filter that matches nothing says so. A blank panel reads as a crash.
    #[test]
    fn no_matches_renders_a_legible_empty_state() {
        let mut state = SessionRecoveryState::default();
        state.view_mode = RecoveryViewMode::Sessions;
        state.orphaned_sessions = vec![orphan_session("alpha_one", None)];
        for c in "zzzz".chars() {
            state.search_push(c);
        }

        let lines = render_to_lines(&mut state, 120, 20);
        let joined = lines.join("\n");
        assert!(
            joined.contains("No matches for"),
            "no empty-state line: {lines:#?}"
        );
        assert!(
            joined.contains("zzzz"),
            "empty state does not echo the query: {lines:#?}"
        );
    }

    /// The query is visible while it is being typed, or the operator cannot
    /// tell a filtered list from an empty one.
    #[test]
    fn the_search_bar_paints_the_query() {
        let mut state = SessionRecoveryState::default();
        state.view_mode = RecoveryViewMode::Sessions;
        state.orphaned_sessions = vec![orphan_session("alpha_one", None)];
        state.search_active = true;
        for c in "alp".chars() {
            state.search_push(c);
        }

        let lines = render_to_lines(&mut state, 120, 20);
        assert!(
            lines.iter().any(|l| l.contains("/alp")),
            "search bar missing: {lines:#?}"
        );
    }

    #[test]
    fn default_defers_recovery_scan_until_the_screen_is_opened() {
        let state = SessionRecoveryState::default();

        assert!(!state.loading);
        assert!(state.orphaned_sessions.is_empty());
        assert!(state.orphaned_worktrees.is_empty());
    }
}
