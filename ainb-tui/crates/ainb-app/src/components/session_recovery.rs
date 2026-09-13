// ABOUTME: Renderer-agnostic half of the `session_recovery` component: its
// state types and the logic that does not draw. The renderer lives in
// `ainb-core::components::session_recovery`, which re-exports this module.

use crate::config::screen_model::fuzzy_matches;
use crate::interactive::session_manager::{SessionMetadata, SessionStore};
use crate::models::SessionAgentType;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;
use uuid::Uuid;

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
    pub fn enrich_worktrees(
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
