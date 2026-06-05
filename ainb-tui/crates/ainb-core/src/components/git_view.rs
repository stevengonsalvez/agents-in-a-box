// ABOUTME: Git view component for displaying git status, changed files, and diffs with commit/push functionality
// Supports hierarchical file tree view and markdown preview for .md files

#![allow(dead_code)]

use anyhow::Result;
use git2::{DiffFormat, DiffOptions, Repository};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Parser, Tag};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, List, ListItem, ListState, Paragraph, Wrap},
};

// Premium color palette (TUI Style Guide)
const CORNFLOWER_BLUE: Color = Color::Rgb(100, 149, 237); // Primary accent, borders
const GOLD: Color = Color::Rgb(255, 215, 0); // Important CTAs, emphasis
const SELECTION_GREEN: Color = Color::Rgb(100, 200, 100); // Active selections
const WARNING_ORANGE: Color = Color::Rgb(255, 165, 0); // Warnings

// Background colors
const DARK_BG: Color = Color::Rgb(25, 25, 35); // Main UI background
const PANEL_BG: Color = Color::Rgb(30, 30, 40); // Panel backgrounds
const LIST_HIGHLIGHT_BG: Color = Color::Rgb(40, 40, 60); // Selection background

// Text colors
const SOFT_WHITE: Color = Color::Rgb(220, 220, 230); // Primary text
const MUTED_GRAY: Color = Color::Rgb(120, 120, 140); // Secondary text
const SUBDUED_BORDER: Color = Color::Rgb(60, 60, 80); // Secondary borders

// Status colors
const PROGRESS_CYAN: Color = Color::Rgb(100, 200, 230); // Loading/processing
use std::collections::HashSet;
use std::path::PathBuf;
use tracing::{debug, error};

use super::code_review;

#[derive(Debug, Clone)]
pub struct GitViewState {
    pub active_tab: GitTab,
    pub changed_files: Vec<ChangedFile>,
    pub selected_file_index: usize,
    pub diff_content: Vec<String>,
    pub diff_scroll_offset: usize,
    pub worktree_path: PathBuf,
    pub is_dirty: bool,
    pub can_push: bool,
    pub commit_message_input: Option<String>, // None = not in commit mode, Some = commit message being entered
    pub commit_message_cursor: usize,         // Cursor position in commit message
    // File tree state
    pub expanded_folders: HashSet<String>, // Tracks which folders are expanded
    pub file_tree_items: Vec<FileTreeItem>, // Flattened tree for rendering
    pub selected_tree_index: usize,        // Index in the flattened tree
    // Markdown viewer state
    pub markdown_content: Vec<MarkdownLine>, // Rendered markdown lines
    pub markdown_scroll_offset: usize,
    // Commits tab state
    pub commits: Vec<crate::git::operations::CommitInfo>,
    pub selected_commit_index: usize,
    // Warp-style Code Review surface (Review tab)
    pub review: code_review::model::ReviewModel,
    pub review_ui: code_review::render::CodeReviewUi,
}

/// Represents an item in the file tree (either a folder or file)
#[derive(Debug, Clone)]
pub struct FileTreeItem {
    pub display_name: String,          // Just the filename or folder name
    pub full_path: String,             // Full path for file operations
    pub depth: usize,                  // Indentation level
    pub is_folder: bool,               // true = folder, false = file
    pub status: Option<GitFileStatus>, // Only for files
    pub is_last_in_group: bool,        // For tree line characters (└─ vs ├─)
    pub is_expanded: bool,             // Only meaningful for folders
    pub file_count: usize,             // Number of changed files in folder (for folders only)
}

/// A line of rendered markdown content
#[derive(Debug, Clone)]
pub struct MarkdownLine {
    pub content: String,
    pub style: MarkdownStyle,
}

/// Styling categories for markdown content
#[derive(Debug, Clone, PartialEq)]
pub enum MarkdownStyle {
    Heading1,
    Heading2,
    Heading3,
    Paragraph,
    CodeBlock,
    CodeBlockHeader(String), // Language name
    ListItem,
    Bold,
    Italic,
    InlineCode,
    Link,
    BlockQuote,
}

#[derive(Debug, Clone, PartialEq)]
pub enum GitTab {
    Review,   // Warp-style unified code review (default surface)
    Files,    // Legacy file tree — retired from the tab cycle, kept for compatibility
    Diff,     // Legacy per-file diff — retired from the tab cycle
    Commits,  // Branch commits since diverging from main
    Markdown, // Preview for .md files
}

#[derive(Debug, Clone)]
pub struct ChangedFile {
    pub path: String,
    pub status: GitFileStatus,
    pub insertions: usize,
    pub deletions: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub enum GitFileStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Untracked,
}

impl GitFileStatus {
    pub fn symbol(&self) -> &'static str {
        match self {
            GitFileStatus::Added => "A",
            GitFileStatus::Modified => "M",
            GitFileStatus::Deleted => "D",
            GitFileStatus::Renamed => "R",
            GitFileStatus::Untracked => "?",
        }
    }

    pub fn color(&self) -> Color {
        match self {
            GitFileStatus::Added => Color::Green,
            GitFileStatus::Modified => Color::Yellow,
            GitFileStatus::Deleted => Color::Red,
            GitFileStatus::Renamed => Color::Blue,
            GitFileStatus::Untracked => Color::Magenta,
        }
    }
}

impl GitViewState {
    pub fn new(worktree_path: PathBuf) -> Self {
        let mut state = Self {
            active_tab: GitTab::Review,
            changed_files: Vec::new(),
            selected_file_index: 0,
            diff_content: Vec::new(),
            diff_scroll_offset: 0,
            worktree_path,
            is_dirty: false,
            can_push: false,
            commit_message_input: None,
            commit_message_cursor: 0,
            // File tree state - expand all folders by default
            expanded_folders: HashSet::new(),
            file_tree_items: Vec::new(),
            selected_tree_index: 0,
            // Markdown viewer state
            markdown_content: Vec::new(),
            markdown_scroll_offset: 0,
            // Commits tab state
            commits: Vec::new(),
            selected_commit_index: 0,
            // Code Review surface
            review: code_review::model::ReviewModel::default(),
            review_ui: code_review::render::CodeReviewUi::default(),
        };
        // Expand root by default
        state.expanded_folders.insert(String::new());
        state
    }

    /// Rebuild the structured Code Review model from the worktree's open changes.
    /// On error the model is cleared so the surface shows an empty state.
    pub fn refresh_review(&mut self) {
        match code_review::parse::build_review_model(&self.worktree_path) {
            Ok(model) => {
                self.review = model;
                // The file list and row layout changed — reset navigation to the top
                // so no index dangles past the new tree. Folder collapse prefs persist.
                self.review_ui.selected_file = 0;
                self.review_ui.sidebar_selected = 0;
                self.review_ui.current_hunk = 0;
                self.review_ui.scroll = 0;
            }
            Err(e) => {
                error!("Failed to build code review model: {e}");
                self.review = code_review::model::ReviewModel::default();
            }
        }
    }

    /// Scroll the review body down by `n` rows (clamped to the last row).
    pub fn review_scroll_down(&mut self, n: usize) {
        let rows = code_review::render::flatten(&self.review).len();
        let max = rows.saturating_sub(1);
        self.review_ui.scroll = self.review_ui.scroll.saturating_add(n).min(max);
    }

    /// Scroll the review body up by `n` rows (saturating at the top).
    pub const fn review_scroll_up(&mut self, n: usize) {
        self.review_ui.scroll = self.review_ui.scroll.saturating_sub(n);
    }

    /// Activate the sidebar-selected row: toggle a folder, or collapse a file's diff.
    pub fn review_toggle_collapse(&mut self) {
        code_review::render::sidebar_activate(&mut self.review, &mut self.review_ui);
    }

    /// Move the sidebar tree selection down (↓).
    pub fn review_sidebar_down(&mut self) {
        code_review::render::sidebar_nav(&self.review, &mut self.review_ui, true);
    }

    /// Move the sidebar tree selection up (↑).
    pub fn review_sidebar_up(&mut self) {
        code_review::render::sidebar_nav(&self.review, &mut self.review_ui, false);
    }

    /// Collapse every folder in the sidebar tree (`E`).
    pub fn review_collapse_all_folders(&mut self) {
        code_review::render::sidebar_set_all_collapsed(&self.review, &mut self.review_ui, true);
    }

    /// Expand every folder in the sidebar tree (`e`).
    pub fn review_expand_all_folders(&mut self) {
        code_review::render::sidebar_set_all_collapsed(&self.review, &mut self.review_ui, false);
    }

    /// Handle a mouse click at screen `(x, y)` over the review sidebar.
    pub fn review_sidebar_click(&mut self, x: u16, y: u16) {
        if let Some(row) = code_review::render::sidebar_row_at(&self.review_ui, x, y) {
            code_review::render::sidebar_click(&mut self.review, &mut self.review_ui, row);
        }
    }

    /// Select the next review file and scroll its header to the top.
    pub fn review_next_file(&mut self) {
        code_review::render::select_file(&self.review, &mut self.review_ui, true);
    }

    /// Select the previous review file and scroll its header to the top.
    pub fn review_prev_file(&mut self) {
        code_review::render::select_file(&self.review, &mut self.review_ui, false);
    }

    /// Jump to the next hunk (`n`).
    pub fn review_next_hunk(&mut self) {
        code_review::render::jump_hunk(&self.review, &mut self.review_ui, true);
    }

    /// Jump to the previous hunk (`N`).
    pub fn review_prev_hunk(&mut self) {
        code_review::render::jump_hunk(&self.review, &mut self.review_ui, false);
    }

    /// Reveal more context at the nearest collapsed gap (`z`).
    pub fn review_expand_context(&mut self) {
        code_review::render::expand_context(&mut self.review, &self.review_ui);
    }

    /// `(current_hunk + 1, total_hunks)` for the `Hunk x/y` counter.
    pub fn review_hunk_counter(&self) -> (usize, usize) {
        let total = code_review::render::hunk_anchors(&code_review::render::flatten(&self.review)).len();
        let current = if total == 0 {
            0
        } else {
            (self.review_ui.current_hunk + 1).min(total)
        };
        (current, total)
    }

    pub fn refresh_git_status(&mut self) -> Result<()> {
        debug!(
            "Refreshing git status for worktree: {:?}",
            self.worktree_path
        );

        let repo = Repository::open(&self.worktree_path)?;
        let mut changed_files = Vec::new();

        // Get working directory changes
        let mut opts = DiffOptions::new();
        opts.include_untracked(true);
        opts.include_ignored(false);

        let diff = repo.diff_index_to_workdir(None, Some(&mut opts))?;

        diff.foreach(
            &mut |delta, _progress| {
                if let Some(new_file) = delta.new_file().path() {
                    let path = new_file.to_string_lossy().to_string();
                    let status = match delta.status() {
                        git2::Delta::Added => GitFileStatus::Added,
                        git2::Delta::Modified => GitFileStatus::Modified,
                        git2::Delta::Deleted => GitFileStatus::Deleted,
                        git2::Delta::Renamed => GitFileStatus::Renamed,
                        git2::Delta::Untracked => GitFileStatus::Untracked,
                        _ => GitFileStatus::Modified,
                    };

                    changed_files.push(ChangedFile {
                        path,
                        status,
                        insertions: 0, // Will be calculated in line callback
                        deletions: 0,
                    });
                }
                true
            },
            None,
            None,
            None,
        )?;

        // Check if there are staged changes
        let head_tree = repo.head()?.peel_to_tree()?;
        let staged_diff = repo.diff_tree_to_index(Some(&head_tree), None, None)?;
        let has_staged_changes = staged_diff.deltas().len() > 0;

        self.changed_files = changed_files;
        self.is_dirty = !self.changed_files.is_empty() || has_staged_changes;

        // Check if we can push (has commits ahead of remote)
        self.can_push = self.check_can_push(&repo)?;

        // Build the file tree from changed files
        self.build_file_tree();

        // Reset selection if needed
        if self.selected_tree_index >= self.file_tree_items.len()
            && !self.file_tree_items.is_empty()
        {
            self.selected_tree_index = 0;
        }

        // Update selected_file_index based on tree selection
        self.update_selected_file_from_tree();

        // Refresh diff for selected file
        if !self.changed_files.is_empty() {
            self.refresh_diff_for_selected_file()?;
            // Also load markdown if it's an .md file
            self.load_markdown_if_applicable();
        } else {
            self.diff_content.clear();
            self.markdown_content.clear();
        }

        // Load branch commits (commits since diverging from main)
        self.commits =
            crate::git::operations::get_branch_commits(&self.worktree_path, 50).unwrap_or_default();
        self.selected_commit_index = 0;

        Ok(())
    }

    /// Build file tree from flat list of changed files
    fn build_file_tree(&mut self) {
        use std::collections::BTreeMap;

        // Track visited paths to prevent symlink loop recursion
        let mut visited_paths: HashSet<std::path::PathBuf> = HashSet::new();

        // Collect all unique folder paths and their file counts
        let mut folders: BTreeMap<String, usize> = BTreeMap::new();
        for file in &self.changed_files {
            // Filter out empty parts for consistent path handling
            let parts: Vec<&str> = file.path.split('/').filter(|p| !p.is_empty()).collect();
            // Add each folder level
            for i in 0..parts.len().saturating_sub(1) {
                let folder_path = parts[..=i].join("/");
                *folders.entry(folder_path).or_insert(0) += 1;
            }
        }

        // Build tree items
        let mut items = Vec::new();
        let mut processed_folders: HashSet<String> = HashSet::new();

        // Sort files by path for consistent ordering
        let mut sorted_files: Vec<&ChangedFile> = self.changed_files.iter().collect();
        sorted_files.sort_by(|a, b| a.path.cmp(&b.path));

        for file in sorted_files {
            // Filter out empty parts (handles paths like "folder//file" or trailing slashes)
            let parts: Vec<&str> = file.path.split('/').filter(|p| !p.is_empty()).collect();

            if parts.is_empty() {
                // Skip files with empty paths
                continue;
            }

            // Add folder entries for each parent folder not yet added
            for i in 0..parts.len().saturating_sub(1) {
                let folder_path = parts[..=i].join("/");
                if !processed_folders.contains(&folder_path) {
                    processed_folders.insert(folder_path.clone());

                    let depth = i;
                    let display_name = parts[i].to_string();
                    let is_expanded = self.expanded_folders.contains(&folder_path);
                    let file_count = *folders.get(&folder_path).unwrap_or(&0);

                    // Calculate if this folder is the last at its depth level
                    // (simplified - could be improved for accuracy)
                    let is_last = false; // Will be recalculated later

                    items.push(FileTreeItem {
                        display_name,
                        full_path: folder_path,
                        depth,
                        is_folder: true,
                        status: None,
                        is_last_in_group: is_last,
                        is_expanded,
                        file_count,
                    });
                }
            }

            // Check if this path is actually a directory on the filesystem
            // Git reports untracked directories without trailing slash
            let full_fs_path = self.worktree_path.join(&file.path);
            let is_directory = full_fs_path.is_dir();

            if is_directory {
                // This is an untracked directory - treat it as a folder
                let folder_path = file.path.clone();
                if !processed_folders.contains(&folder_path) {
                    processed_folders.insert(folder_path.clone());

                    let base_depth = parts.len().saturating_sub(1);
                    let display_name = parts
                        .last()
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| file.path.clone());
                    let is_expanded = self.expanded_folders.contains(&folder_path);

                    // Scan directory contents to get file count
                    let dir_contents = Self::scan_directory_contents(&full_fs_path);
                    let file_count = dir_contents.len();

                    items.push(FileTreeItem {
                        display_name,
                        full_path: folder_path.clone(),
                        depth: base_depth,
                        is_folder: true,
                        status: Some(file.status.clone()), // Keep status for untracked folders
                        is_last_in_group: false,
                        is_expanded,
                        file_count,
                    });

                    // If expanded, add the directory contents as children
                    if is_expanded {
                        Self::add_directory_contents_to_tree(
                            &mut items,
                            &mut processed_folders,
                            &self.expanded_folders,
                            &mut visited_paths,
                            &full_fs_path,
                            &folder_path,
                            base_depth + 1,
                            &file.status,
                        );
                    }
                }
            } else {
                // Regular file
                let depth = parts.len().saturating_sub(1);
                // Use the last part as filename, fallback to full path if empty
                let display_name = parts
                    .last()
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| file.path.clone());

                items.push(FileTreeItem {
                    display_name,
                    full_path: file.path.clone(),
                    depth,
                    is_folder: false,
                    status: Some(file.status.clone()),
                    is_last_in_group: false, // Will be recalculated
                    is_expanded: false,
                    file_count: 0,
                });
            }
        }

        // Recalculate is_last_in_group for proper tree rendering
        self.calculate_last_in_group(&mut items);

        // Filter out items under collapsed folders
        self.file_tree_items = self.filter_collapsed_items(&items);
    }

    /// Calculate which items are last in their group for tree line rendering
    /// Optimized O(n) implementation using reverse iteration with depth tracking
    fn calculate_last_in_group(&self, items: &mut [FileTreeItem]) {
        if items.is_empty() {
            return;
        }

        // Track if we've seen an item at each depth level (from the end)
        // When iterating backwards, the first item we see at a depth is the "last" one
        let max_depth = items.iter().map(|i| i.depth).max().unwrap_or(0);
        let mut seen_at_depth = vec![false; max_depth + 1];

        // Iterate backwards
        for i in (0..items.len()).rev() {
            let depth = items[i].depth;

            // If we haven't seen an item at this depth yet (from the end), it's last in group
            items[i].is_last_in_group = !seen_at_depth[depth];

            // Mark this depth as seen
            seen_at_depth[depth] = true;

            // Reset all deeper depths (they belong to a different subtree)
            for d in (depth + 1)..=max_depth {
                seen_at_depth[d] = false;
            }
        }
    }

    /// Filter out items that are under collapsed folders
    fn filter_collapsed_items(&self, items: &[FileTreeItem]) -> Vec<FileTreeItem> {
        let mut result = Vec::new();
        let mut skip_until_depth: Option<usize> = None;

        for item in items {
            // If we're skipping items under a collapsed folder
            if let Some(skip_depth) = skip_until_depth {
                if item.depth > skip_depth {
                    continue; // Skip this item
                } else {
                    skip_until_depth = None; // We've moved past the collapsed section
                }
            }

            result.push(item.clone());

            // If this is a collapsed folder, start skipping its children
            if item.is_folder && !item.is_expanded {
                skip_until_depth = Some(item.depth);
            }
        }

        result
    }

    /// Scan directory contents recursively and return list of relative file paths
    /// Uses visited set to prevent infinite recursion from symlink loops
    fn scan_directory_contents(dir_path: &std::path::Path) -> Vec<String> {
        let mut visited = HashSet::new();
        Self::scan_directory_contents_inner(dir_path, &mut visited)
    }

    /// Inner recursive function with visited tracking
    fn scan_directory_contents_inner(
        dir_path: &std::path::Path,
        visited: &mut HashSet<std::path::PathBuf>,
    ) -> Vec<String> {
        let mut files = Vec::new();

        // Get canonical path to detect symlink loops
        let canonical = match dir_path.canonicalize() {
            Ok(p) => p,
            Err(_) => return files, // Can't resolve path, skip
        };

        // Check if we've already visited this path (symlink loop detection)
        if visited.contains(&canonical) {
            return files;
        }
        visited.insert(canonical);

        if let Ok(entries) = std::fs::read_dir(dir_path) {
            for entry in entries.flatten() {
                let path = entry.path();
                // Skip symlinks to avoid potential loops
                if path.is_symlink() {
                    continue;
                }
                if path.is_file() {
                    if let Some(name) = path.file_name() {
                        files.push(name.to_string_lossy().to_string());
                    }
                } else if path.is_dir() {
                    // Count files in subdirectories too
                    let sub_files = Self::scan_directory_contents_inner(&path, visited);
                    files.extend(sub_files);
                }
            }
        }

        files
    }

    /// Add directory contents to the tree recursively
    /// Uses visited_paths to prevent infinite recursion from symlink loops
    fn add_directory_contents_to_tree(
        items: &mut Vec<FileTreeItem>,
        processed_folders: &mut HashSet<String>,
        expanded_folders: &HashSet<String>,
        visited_paths: &mut HashSet<std::path::PathBuf>,
        fs_path: &std::path::Path,
        relative_path: &str,
        depth: usize,
        inherited_status: &GitFileStatus,
    ) {
        // Check for symlink loops using canonical path
        let canonical = match fs_path.canonicalize() {
            Ok(p) => p,
            Err(_) => return, // Can't resolve path, skip
        };

        if visited_paths.contains(&canonical) {
            return; // Already visited, skip to prevent infinite recursion
        }
        visited_paths.insert(canonical);

        let mut entries: Vec<_> = match std::fs::read_dir(fs_path) {
            Ok(entries) => entries.flatten().collect(),
            Err(_) => return,
        };

        // Sort entries: directories first, then files, alphabetically
        entries.sort_by(|a, b| {
            let a_is_dir = a.path().is_dir();
            let b_is_dir = b.path().is_dir();
            match (a_is_dir, b_is_dir) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => a.file_name().cmp(&b.file_name()),
            }
        });

        let entry_count = entries.len();
        for (idx, entry) in entries.into_iter().enumerate() {
            let path = entry.path();

            // Skip symlinks to avoid potential loops
            if path.is_symlink() {
                continue;
            }

            let file_name = match path.file_name() {
                Some(name) => name.to_string_lossy().to_string(),
                None => continue,
            };

            // Skip hidden files/directories (starting with .)
            if file_name.starts_with('.') {
                continue;
            }

            let full_relative_path = if relative_path.is_empty() {
                file_name.clone()
            } else {
                format!("{}/{}", relative_path, file_name)
            };

            let is_last = idx == entry_count - 1;

            if path.is_dir() {
                // It's a subdirectory
                if !processed_folders.contains(&full_relative_path) {
                    processed_folders.insert(full_relative_path.clone());

                    let is_expanded = expanded_folders.contains(&full_relative_path);
                    let sub_contents = Self::scan_directory_contents(&path);
                    let file_count = sub_contents.len();

                    items.push(FileTreeItem {
                        display_name: file_name,
                        full_path: full_relative_path.clone(),
                        depth,
                        is_folder: true,
                        status: Some(inherited_status.clone()),
                        is_last_in_group: is_last,
                        is_expanded,
                        file_count,
                    });

                    // Recursively add subdirectory contents if expanded
                    if is_expanded {
                        Self::add_directory_contents_to_tree(
                            items,
                            processed_folders,
                            expanded_folders,
                            visited_paths,
                            &path,
                            &full_relative_path,
                            depth + 1,
                            inherited_status,
                        );
                    }
                }
            } else {
                // It's a file
                items.push(FileTreeItem {
                    display_name: file_name,
                    full_path: full_relative_path,
                    depth,
                    is_folder: false,
                    status: Some(inherited_status.clone()),
                    is_last_in_group: is_last,
                    is_expanded: false,
                    file_count: 0,
                });
            }
        }
    }

    /// Update selected_file_index based on the currently selected tree item
    fn update_selected_file_from_tree(&mut self) {
        if let Some(item) = self.file_tree_items.get(self.selected_tree_index) {
            if !item.is_folder {
                // Find this file in changed_files
                if let Some(idx) = self.changed_files.iter().position(|f| f.path == item.full_path)
                {
                    self.selected_file_index = idx;
                }
            }
        }
    }

    /// Load markdown content if the selected file is a .md file
    fn load_markdown_if_applicable(&mut self) {
        let file_path = self
            .file_tree_items
            .get(self.selected_tree_index)
            .filter(|item| {
                !item.is_folder
                    && (item.full_path.ends_with(".md") || item.full_path.ends_with(".markdown"))
            })
            .map(|item| item.full_path.clone());

        if let Some(path) = file_path {
            self.load_markdown_content(&path);
        } else {
            self.markdown_content.clear();
        }
    }

    /// Load and parse markdown content from a file
    fn load_markdown_content(&mut self, file_path: &str) {
        let full_path = self.worktree_path.join(file_path);
        match std::fs::read_to_string(&full_path) {
            Ok(content) => {
                self.markdown_content = Self::parse_markdown(&content);
                self.markdown_scroll_offset = 0;
            }
            Err(e) => {
                self.markdown_content = vec![MarkdownLine {
                    content: format!("Error reading file: {}", e),
                    style: MarkdownStyle::Paragraph,
                }];
            }
        }
    }

    /// Parse markdown content into styled lines
    fn parse_markdown(content: &str) -> Vec<MarkdownLine> {
        let mut lines = Vec::new();
        let parser = Parser::new(content);

        let mut current_text = String::new();
        let mut in_code_block = false;
        #[allow(unused_assignments)]
        let mut code_block_lang: Option<String> = None;
        let mut list_depth: usize = 0;

        for event in parser {
            match event {
                Event::Start(tag) => {
                    // Flush accumulated text
                    if !current_text.is_empty() && !in_code_block {
                        lines.push(MarkdownLine {
                            content: current_text.clone(),
                            style: MarkdownStyle::Paragraph,
                        });
                        current_text.clear();
                    }

                    match tag {
                        Tag::Heading(..) => {
                            // Add blank line before headings (except first)
                            if !lines.is_empty() {
                                lines.push(MarkdownLine {
                                    content: String::new(),
                                    style: MarkdownStyle::Paragraph,
                                });
                            }
                            current_text.clear();
                        }
                        Tag::CodeBlock(kind) => {
                            in_code_block = true;
                            code_block_lang = match kind {
                                CodeBlockKind::Fenced(lang) => {
                                    let lang_str = lang.to_string();
                                    if !lang_str.is_empty() {
                                        Some(lang_str)
                                    } else {
                                        None
                                    }
                                }
                                _ => None,
                            };
                            // Add code block header with language badge
                            if let Some(ref lang) = code_block_lang {
                                lines.push(MarkdownLine {
                                    content: format!("┌─ [{}] ", lang.to_uppercase()),
                                    style: MarkdownStyle::CodeBlockHeader(lang.clone()),
                                });
                            } else {
                                lines.push(MarkdownLine {
                                    content: "┌────────────────────".to_string(),
                                    style: MarkdownStyle::CodeBlock,
                                });
                            }
                        }
                        Tag::List(_) => {
                            list_depth += 1;
                        }
                        Tag::BlockQuote => {}
                        _ => {}
                    }
                }

                Event::End(tag) => {
                    match tag {
                        Tag::Heading(level, _, _) => {
                            let style = match level {
                                HeadingLevel::H1 => MarkdownStyle::Heading1,
                                HeadingLevel::H2 => MarkdownStyle::Heading2,
                                _ => MarkdownStyle::Heading3,
                            };
                            // Use visual indicators instead of # markers
                            let prefix = match level {
                                HeadingLevel::H1 => "══ ",
                                HeadingLevel::H2 => "── ",
                                HeadingLevel::H3 => "─ ",
                                _ => "• ",
                            };
                            lines.push(MarkdownLine {
                                content: format!("{}{}", prefix, current_text),
                                style,
                            });
                            // Add underline for H1
                            if level == HeadingLevel::H1 {
                                let underline_len = current_text.chars().count() + 3;
                                lines.push(MarkdownLine {
                                    content: "═".repeat(underline_len),
                                    style: MarkdownStyle::Heading1,
                                });
                            }
                            current_text.clear();
                        }
                        Tag::Paragraph => {
                            if !current_text.is_empty() {
                                lines.push(MarkdownLine {
                                    content: current_text.clone(),
                                    style: MarkdownStyle::Paragraph,
                                });
                                current_text.clear();
                            }
                            // Add blank line after paragraphs
                            lines.push(MarkdownLine {
                                content: String::new(),
                                style: MarkdownStyle::Paragraph,
                            });
                        }
                        Tag::CodeBlock(_) => {
                            in_code_block = false;
                            // Add closing line for code block
                            lines.push(MarkdownLine {
                                content: "└────────────────────".to_string(),
                                style: MarkdownStyle::CodeBlock,
                            });
                            // Reset code block lang (value intentionally unused after)
                            let _ = code_block_lang.take();
                        }
                        Tag::List(_) => {
                            list_depth = list_depth.saturating_sub(1);
                        }
                        Tag::Item => {
                            if !current_text.is_empty() {
                                let indent = "  ".repeat(list_depth.saturating_sub(1));
                                lines.push(MarkdownLine {
                                    content: format!("{}• {}", indent, current_text),
                                    style: MarkdownStyle::ListItem,
                                });
                                current_text.clear();
                            }
                        }
                        Tag::Strong => {}
                        Tag::Emphasis => {}
                        Tag::BlockQuote => {}
                        _ => {}
                    }
                }

                Event::Text(text) => {
                    if in_code_block {
                        // Add each line of code separately
                        for line in text.lines() {
                            lines.push(MarkdownLine {
                                content: format!("│ {}", line),
                                style: MarkdownStyle::CodeBlock,
                            });
                        }
                    } else {
                        current_text.push_str(&text);
                    }
                }

                Event::Code(code) => {
                    current_text.push_str(&format!("`{}`", code));
                }

                Event::SoftBreak | Event::HardBreak => {
                    if !in_code_block {
                        current_text.push(' ');
                    }
                }

                _ => {}
            }
        }

        // Flush any remaining text
        if !current_text.is_empty() {
            lines.push(MarkdownLine {
                content: current_text,
                style: MarkdownStyle::Paragraph,
            });
        }

        lines
    }

    fn check_can_push(&self, repo: &Repository) -> Result<bool> {
        // Check if there are commits ahead of the remote
        match repo.head() {
            Ok(head_ref) => {
                let head_oid = match head_ref.target() {
                    Some(oid) => oid,
                    None => return Ok(false), // Symbolic ref pointing to nothing
                };

                // Try to find the upstream branch
                let branch_name = head_ref.shorthand().unwrap_or("HEAD");
                let upstream_name = format!("origin/{}", branch_name);

                match repo.revparse_single(&upstream_name) {
                    Ok(upstream_commit) => {
                        let upstream_oid = upstream_commit.id();

                        // Check if head is ahead of upstream
                        let (ahead, _behind) = repo.graph_ahead_behind(head_oid, upstream_oid)?;
                        Ok(ahead > 0)
                    }
                    Err(_) => {
                        // No upstream, can push if there are commits
                        Ok(true)
                    }
                }
            }
            Err(_) => Ok(false),
        }
    }

    pub fn refresh_diff_for_selected_file(&mut self) -> Result<()> {
        if self.changed_files.is_empty() {
            self.diff_content.clear();
            return Ok(());
        }

        let selected_file = &self.changed_files[self.selected_file_index];
        debug!("Refreshing diff for file: {}", selected_file.path);

        let repo = Repository::open(&self.worktree_path)?;
        let mut diff_content = Vec::new();

        // Create diff options
        let mut opts = DiffOptions::new();
        opts.pathspec(&selected_file.path);

        let diff = match selected_file.status {
            GitFileStatus::Untracked => {
                // For untracked files, show the entire file content as additions
                let file_path = self.worktree_path.join(&selected_file.path);

                // Check if this is a directory
                if file_path.is_dir() {
                    diff_content.push(format!("📁 Directory: {}", selected_file.path));
                    diff_content.push(String::new());
                    diff_content.push("Contents:".to_string());

                    // List directory contents
                    if let Ok(entries) = std::fs::read_dir(&file_path) {
                        for entry in entries.flatten() {
                            let name = entry.file_name().to_string_lossy().to_string();
                            let prefix = if entry.path().is_dir() {
                                "📁"
                            } else {
                                "📄"
                            };
                            diff_content.push(format!("  {} {}", prefix, name));
                        }
                    }
                    self.diff_content = diff_content;
                    return Ok(());
                }

                match std::fs::read_to_string(&file_path) {
                    Ok(content) => {
                        diff_content.push(format!("--- /dev/null"));
                        diff_content.push(format!("+++ b/{}", selected_file.path));
                        diff_content.push(format!("@@ -0,0 +1,{} @@", content.lines().count()));
                        for line in content.lines() {
                            diff_content.push(format!("+{}", line));
                        }
                    }
                    Err(e) => {
                        diff_content.push(format!("Error reading file: {}", e));
                    }
                }
                self.diff_content = diff_content;
                return Ok(());
            }
            _ => repo.diff_index_to_workdir(None, Some(&mut opts))?,
        };

        // Format the diff
        diff.print(DiffFormat::Patch, |_delta, _hunk, line| {
            let content = std::str::from_utf8(line.content()).unwrap_or("<binary>");
            let line_str = match line.origin() {
                '+' => format!("+{}", content.trim_end()),
                '-' => format!("-{}", content.trim_end()),
                ' ' => format!(" {}", content.trim_end()),
                '=' => format!("={}", content.trim_end()),
                '>' => format!(">{}", content.trim_end()),
                '<' => format!("<{}", content.trim_end()),
                'F' => format!("File: {}", content.trim_end()),
                'H' => format!("Hunk: {}", content.trim_end()),
                _ => content.trim_end().to_string(),
            };
            diff_content.push(line_str);
            true
        })?;

        self.diff_content = diff_content;
        self.diff_scroll_offset = 0; // Reset scroll when changing files

        Ok(())
    }

    /// Navigate to the next item in the file tree
    pub fn next_file(&mut self) {
        if !self.file_tree_items.is_empty() {
            self.selected_tree_index = (self.selected_tree_index + 1) % self.file_tree_items.len();
            self.on_tree_selection_changed();
        }
    }

    /// Navigate to the previous item in the file tree
    pub fn previous_file(&mut self) {
        if !self.file_tree_items.is_empty() {
            self.selected_tree_index = if self.selected_tree_index == 0 {
                self.file_tree_items.len() - 1
            } else {
                self.selected_tree_index - 1
            };
            self.on_tree_selection_changed();
        }
    }

    /// Called when the tree selection changes to update diff and markdown
    fn on_tree_selection_changed(&mut self) {
        self.update_selected_file_from_tree();

        // Refresh diff for the selected file (if it's a file, not a folder)
        if let Some(item) = self.file_tree_items.get(self.selected_tree_index) {
            if !item.is_folder {
                if let Err(e) = self.refresh_diff_for_selected_file() {
                    error!("Failed to refresh diff: {}", e);
                }
                // Load markdown if applicable
                self.load_markdown_if_applicable();
            }
        }
    }

    /// Toggle folder expansion/collapse
    pub fn toggle_folder(&mut self) {
        if let Some(item) = self.file_tree_items.get(self.selected_tree_index).cloned() {
            if item.is_folder {
                // Toggle the folder's expanded state
                if self.expanded_folders.contains(&item.full_path) {
                    self.expanded_folders.remove(&item.full_path);
                } else {
                    self.expanded_folders.insert(item.full_path);
                }
                // Rebuild the tree to reflect the change
                self.build_file_tree();
            }
        }
    }

    /// Expand all folders in the tree
    pub fn expand_all_folders(&mut self) {
        // Collect all folder paths
        for file in &self.changed_files {
            let parts: Vec<&str> = file.path.split('/').collect();
            for i in 0..parts.len().saturating_sub(1) {
                let folder_path = parts[..=i].join("/");
                self.expanded_folders.insert(folder_path);
            }
        }
        self.build_file_tree();
    }

    /// Collapse all folders in the tree
    pub fn collapse_all_folders(&mut self) {
        self.expanded_folders.clear();
        // Keep only the root expanded
        self.expanded_folders.insert(String::new());
        self.build_file_tree();
        self.selected_tree_index = 0;
    }

    /// Check if the currently selected item is a folder
    pub fn is_selected_folder(&self) -> bool {
        self.file_tree_items
            .get(self.selected_tree_index)
            .map(|item| item.is_folder)
            .unwrap_or(false)
    }

    /// Check if the currently selected file is a markdown file
    pub fn is_selected_markdown(&self) -> bool {
        self.file_tree_items
            .get(self.selected_tree_index)
            .map(|item| {
                !item.is_folder
                    && (item.full_path.ends_with(".md") || item.full_path.ends_with(".markdown"))
            })
            .unwrap_or(false)
    }

    pub fn scroll_diff_up(&mut self) {
        self.scroll_diff_up_by(1);
    }

    pub fn scroll_diff_down(&mut self) {
        self.scroll_diff_down_by(1);
    }

    /// Scroll diff up by N lines
    pub fn scroll_diff_up_by(&mut self, lines: usize) {
        self.diff_scroll_offset = self.diff_scroll_offset.saturating_sub(lines);
    }

    /// Scroll diff down by N lines
    pub fn scroll_diff_down_by(&mut self, lines: usize) {
        let max_offset = self.diff_content.len().saturating_sub(1);
        self.diff_scroll_offset = (self.diff_scroll_offset + lines).min(max_offset);
    }

    /// Scroll markdown content up
    pub fn scroll_markdown_up(&mut self) {
        self.scroll_markdown_up_by(1);
    }

    /// Scroll markdown content down
    pub fn scroll_markdown_down(&mut self) {
        self.scroll_markdown_down_by(1);
    }

    /// Scroll markdown up by N lines
    pub fn scroll_markdown_up_by(&mut self, lines: usize) {
        self.markdown_scroll_offset = self.markdown_scroll_offset.saturating_sub(lines);
    }

    /// Scroll markdown down by N lines
    pub fn scroll_markdown_down_by(&mut self, lines: usize) {
        let max_offset = self.markdown_content.len().saturating_sub(1);
        self.markdown_scroll_offset = (self.markdown_scroll_offset + lines).min(max_offset);
    }

    pub fn switch_tab(&mut self) {
        // Cycle Review → Commits → (Markdown if applicable) → Review.
        // The legacy Files/Diff tabs are retired from the cycle.
        self.active_tab = match self.active_tab {
            GitTab::Commits => {
                if self.is_selected_markdown() && !self.markdown_content.is_empty() {
                    GitTab::Markdown
                } else {
                    GitTab::Review
                }
            }
            GitTab::Markdown => GitTab::Review,
            // Review (and the retired Files/Diff) advance to Commits.
            _ => GitTab::Commits,
        };
    }

    pub fn start_commit_message_input(&mut self) {
        self.commit_message_input = Some(String::new());
        self.commit_message_cursor = 0;
    }

    pub fn cancel_commit_message_input(&mut self) {
        self.commit_message_input = None;
        self.commit_message_cursor = 0;
    }

    pub fn is_in_commit_mode(&self) -> bool {
        self.commit_message_input.is_some()
    }

    pub fn add_char_to_commit_message(&mut self, ch: char) {
        if let Some(ref mut message) = self.commit_message_input {
            message.insert(self.commit_message_cursor, ch);
            self.commit_message_cursor += 1;
        }
    }

    pub fn backspace_commit_message(&mut self) {
        if let Some(ref mut message) = self.commit_message_input {
            if self.commit_message_cursor > 0 {
                self.commit_message_cursor -= 1;
                message.remove(self.commit_message_cursor);
            }
        }
    }

    pub fn move_commit_cursor_left(&mut self) {
        if self.commit_message_cursor > 0 {
            self.commit_message_cursor -= 1;
        }
    }

    pub fn move_commit_cursor_right(&mut self) {
        if let Some(ref message) = self.commit_message_input {
            if self.commit_message_cursor < message.len() {
                self.commit_message_cursor += 1;
            }
        }
    }

    pub fn commit_and_push(&mut self) -> Result<String> {
        // Get the commit message, or return error if not in commit mode
        let commit_message = match &self.commit_message_input {
            Some(message) if !message.trim().is_empty() => message.trim().to_string(),
            Some(_) => return Err(anyhow::anyhow!("Commit message cannot be empty")),
            None => {
                return Err(anyhow::anyhow!(
                    "Not in commit mode - press 'p' to start commit process"
                ));
            }
        };

        // Use the shared git operations function
        let result =
            crate::git::operations::commit_and_push_changes(&self.worktree_path, &commit_message);

        // Clear commit message input after successful commit
        if result.is_ok() {
            self.commit_message_input = None;
            self.commit_message_cursor = 0;
        }

        result
    }
}

pub struct GitViewComponent;

impl GitViewComponent {
    pub fn render(frame: &mut Frame, area: Rect, git_state: &GitViewState) {
        // Create main layout - adjust constraints based on commit mode
        let constraints = if git_state.is_in_commit_mode() {
            vec![
                Constraint::Length(3), // Tabs
                Constraint::Min(0),    // Content
                Constraint::Length(5), // Commit message input
                Constraint::Length(3), // Status/Actions
            ]
        } else {
            vec![
                Constraint::Length(3), // Tabs
                Constraint::Min(0),    // Content
                Constraint::Length(3), // Status/Actions
            ]
        };

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(area);

        // Render raised tab style - dynamically include Markdown tab if applicable
        let tab_titles: Vec<&str> =
            if git_state.is_selected_markdown() && !git_state.markdown_content.is_empty() {
                vec!["Review", "Commits", "Markdown"]
            } else {
                vec!["Review", "Commits"]
            };

        let selected_tab = match git_state.active_tab {
            GitTab::Review | GitTab::Files | GitTab::Diff => 0,
            GitTab::Commits => 1,
            GitTab::Markdown => {
                if tab_titles.len() > 2 {
                    2
                } else {
                    0
                }
            }
        };

        Self::render_raised_tabs(frame, chunks[0], &tab_titles, selected_tab);

        // Render content based on active tab
        match git_state.active_tab {
            GitTab::Review => {
                code_review::render::render(frame, chunks[1], &git_state.review, &git_state.review_ui);
            }
            GitTab::Files => Self::render_files_tab(frame, chunks[1], git_state),
            GitTab::Diff => Self::render_diff_tab(frame, chunks[1], git_state),
            GitTab::Commits => Self::render_commits_tab(frame, chunks[1], git_state),
            GitTab::Markdown => Self::render_markdown_tab(frame, chunks[1], git_state),
        }

        // Render commit message input if in commit mode
        if git_state.is_in_commit_mode() {
            Self::render_commit_input(frame, chunks[2], git_state);
            // Status bar is at index 3 when commit input is shown
            Self::render_status_bar(frame, chunks[3], git_state);
        } else {
            // Status bar is at index 2 when no commit input
            Self::render_status_bar(frame, chunks[2], git_state);
        }
    }

    /// Render tabs in classic raised tab style
    /// Active tab has a raised box that connects to content below
    fn render_raised_tabs(frame: &mut Frame, area: Rect, tabs: &[&str], selected: usize) {
        // We need exactly 3 lines for the raised tab effect
        if area.height < 3 {
            return;
        }

        let area_width = area.width as usize;

        // Calculate consistent tab cell widths (each tab occupies same structure across all lines)
        // Structure: " · TabName " where separator is 3 chars, tab name varies, trailing space 1
        // For active: "│ TabName │" where bars are 1 char each, padding 1 each side

        // Calculate the display width each tab cell needs (must be consistent across all lines)
        // Each cell = separator(3) + name + padding = OR = bar(1) + padding(1) + name + padding(1) + bar(1)
        // We use: 3 chars before name + name + 1 char after for inactive
        // We use: 1 bar + 1 space + name + 1 space + 1 bar for active (= 4 + name)
        // Make them equal by using max
        let tab_cell_widths: Vec<usize> = tabs
            .iter()
            .map(|t| {
                let name_len = t.chars().count();
                // Cell width = separator space (3) + name + trailing space (1) = name + 4
                // This matches active: │(1) + space(1) + name + space(1) + │(1) = name + 4
                name_len + 4
            })
            .collect();

        let icon_width = 4; // "[G] "

        // Line 1: Top border - spaces for inactive, ╭───╮ for active
        let mut top_spans: Vec<Span> = vec![];
        top_spans.push(Span::raw(" ".repeat(icon_width))); // space above icon

        for (i, &cell_width) in tab_cell_widths.iter().enumerate() {
            if i == selected {
                // Active tab top: ╭───────╮
                top_spans.push(Span::styled("╭", Style::default().fg(GOLD)));
                top_spans.push(Span::styled(
                    "─".repeat(cell_width - 2),
                    Style::default().fg(GOLD),
                ));
                top_spans.push(Span::styled("╮", Style::default().fg(GOLD)));
            } else {
                // Inactive: just spaces
                top_spans.push(Span::raw(" ".repeat(cell_width)));
            }
        }

        // Fill remaining with spaces
        let top_used: usize = icon_width + tab_cell_widths.iter().sum::<usize>();
        if top_used < area_width {
            top_spans.push(Span::raw(" ".repeat(area_width - top_used)));
        }

        // Line 2: Tab names - │ Name │ for active, " · Name" for inactive
        let mut mid_spans: Vec<Span> = vec![];
        mid_spans.push(Span::styled(
            "[G] ",
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ));

        for (i, &tab_name) in tabs.iter().enumerate() {
            let cell_width = tab_cell_widths[i];
            let name_len = tab_name.chars().count();

            if i == selected {
                // Active: │ Name │
                let inner_width = cell_width - 2; // minus the two │ bars
                let left_pad = (inner_width - name_len) / 2;
                let right_pad = inner_width - name_len - left_pad;

                mid_spans.push(Span::styled("│", Style::default().fg(GOLD)));
                mid_spans.push(Span::raw(" ".repeat(left_pad)));
                mid_spans.push(Span::styled(
                    tab_name.to_string(),
                    Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                ));
                mid_spans.push(Span::raw(" ".repeat(right_pad)));
                mid_spans.push(Span::styled("│", Style::default().fg(GOLD)));
            } else {
                // Inactive: " · Name " (total = cell_width)
                let remaining = cell_width - 3 - name_len; // 3 for " · "
                mid_spans.push(Span::styled(" · ", Style::default().fg(SUBDUED_BORDER)));
                mid_spans.push(Span::styled(
                    tab_name.to_string(),
                    Style::default().fg(MUTED_GRAY),
                ));
                if remaining > 0 {
                    mid_spans.push(Span::raw(" ".repeat(remaining)));
                }
            }
        }

        // Add "Tab switch" hint
        let mid_used: usize = icon_width + tab_cell_widths.iter().sum::<usize>();
        let hint_text = "Tab switch";
        let hint_len = hint_text.len() + 1;
        if mid_used + hint_len < area_width {
            let padding = area_width - mid_used - hint_len;
            mid_spans.push(Span::raw(" ".repeat(padding)));
            mid_spans.push(Span::styled(
                "Tab",
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ));
            mid_spans.push(Span::styled(" switch", Style::default().fg(MUTED_GRAY)));
        }

        // Line 3: Bottom border - ─── for inactive, ┘   └ for active
        let mut bot_spans: Vec<Span> = vec![];
        bot_spans.push(Span::styled(
            "─".repeat(icon_width),
            Style::default().fg(CORNFLOWER_BLUE),
        ));

        for (i, &cell_width) in tab_cell_widths.iter().enumerate() {
            if i == selected {
                // Active: ┘ spaces └
                let inner_width = cell_width - 2;
                bot_spans.push(Span::styled("┘", Style::default().fg(GOLD)));
                bot_spans.push(Span::raw(" ".repeat(inner_width)));
                bot_spans.push(Span::styled("└", Style::default().fg(GOLD)));
            } else {
                // Inactive: continuous line
                bot_spans.push(Span::styled(
                    "─".repeat(cell_width),
                    Style::default().fg(CORNFLOWER_BLUE),
                ));
            }
        }

        // Fill remaining with line
        let bot_used: usize = icon_width + tab_cell_widths.iter().sum::<usize>();
        if bot_used < area_width {
            bot_spans.push(Span::styled(
                "─".repeat(area_width - bot_used),
                Style::default().fg(CORNFLOWER_BLUE),
            ));
        }

        // Render
        let tab_lines = vec![
            Line::from(top_spans),
            Line::from(mid_spans),
            Line::from(bot_spans),
        ];

        let tab_paragraph = Paragraph::new(tab_lines).style(Style::default().bg(DARK_BG));

        frame.render_widget(tab_paragraph, area);
    }

    fn render_files_tab(frame: &mut Frame, area: Rect, git_state: &GitViewState) {
        if git_state.file_tree_items.is_empty() {
            let no_changes = Paragraph::new(vec![
                Line::from(Span::styled(
                    "✨ No changes detected",
                    Style::default().fg(MUTED_GRAY),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "Working directory is clean",
                    Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
                )),
            ])
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(CORNFLOWER_BLUE))
                    .style(Style::default().bg(DARK_BG))
                    .title(Line::from(vec![
                        Span::styled(" 📁 ", Style::default().fg(GOLD)),
                        Span::styled(
                            "Changed Files",
                            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                        ),
                    ])),
            )
            .wrap(Wrap { trim: true });
            frame.render_widget(no_changes, area);
            return;
        }

        let items: Vec<ListItem> = git_state
            .file_tree_items
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let is_selected = i == git_state.selected_tree_index;

                // Build indentation with tree lines (styled)
                let indent = Self::build_tree_indent(item.depth, item.is_last_in_group);
                let indent_style = Style::default().fg(SUBDUED_BORDER);

                if item.is_folder {
                    // Folder rendering with premium styling
                    let expand_symbol = if item.is_expanded { "▼" } else { "▶" };

                    let (folder_color, expand_color) = if is_selected {
                        (SELECTION_GREEN, SELECTION_GREEN)
                    } else {
                        (CORNFLOWER_BLUE, MUTED_GRAY)
                    };

                    let folder_style = Style::default().fg(folder_color);
                    let folder_name_style = if is_selected {
                        Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(SOFT_WHITE)
                    };

                    let count_text = if item.file_count > 0 {
                        format!(" ({})", item.file_count)
                    } else {
                        String::new()
                    };

                    // Show status badge for untracked directories
                    let status_prefix = if let Some(ref status) = item.status {
                        let status_style =
                            Style::default().fg(status.color()).add_modifier(Modifier::BOLD);
                        vec![
                            Span::styled(format!("[{}]", status.symbol()), status_style),
                            Span::raw(" "),
                        ]
                    } else {
                        vec![]
                    };

                    let folder_icon = if item.is_expanded { "📂" } else { "📁" };

                    let mut spans = vec![Span::styled(indent, indent_style)];
                    if is_selected {
                        spans.insert(0, Span::styled("▶ ", Style::default().fg(SELECTION_GREEN)));
                    } else {
                        spans.insert(0, Span::raw("  "));
                    }
                    spans.extend(status_prefix);
                    spans.extend(vec![
                        Span::styled(expand_symbol, Style::default().fg(expand_color)),
                        Span::raw(" "),
                        Span::styled(folder_icon, folder_style),
                        Span::raw(" "),
                        Span::styled(&item.display_name, folder_name_style),
                        Span::styled(count_text, Style::default().fg(MUTED_GRAY)),
                    ]);

                    let base_style = if is_selected {
                        Style::default().bg(LIST_HIGHLIGHT_BG)
                    } else {
                        Style::default()
                    };

                    ListItem::new(Line::from(spans)).style(base_style)
                } else {
                    // File rendering with premium styling
                    let status = item.status.as_ref().unwrap_or(&GitFileStatus::Modified);
                    let status_style =
                        Style::default().fg(status.color()).add_modifier(Modifier::BOLD);

                    // Use display_name, fallback to full_path if empty
                    let filename = if item.display_name.is_empty() {
                        &item.full_path
                    } else {
                        &item.display_name
                    };

                    let file_icon = Self::get_file_icon(filename);
                    let file_style = if is_selected {
                        Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(SOFT_WHITE)
                    };

                    let mut spans = vec![];
                    if is_selected {
                        spans.push(Span::styled("▶ ", Style::default().fg(SELECTION_GREEN)));
                    } else {
                        spans.push(Span::raw("  "));
                    }
                    spans.extend(vec![
                        Span::styled(indent.clone(), indent_style),
                        Span::styled(format!("[{}]", status.symbol()), status_style),
                        Span::raw(" "),
                        Span::raw(file_icon),
                        Span::raw(" "),
                        Span::styled(filename, file_style),
                    ]);

                    let base_style = if is_selected {
                        Style::default().bg(LIST_HIGHLIGHT_BG)
                    } else {
                        Style::default()
                    };

                    ListItem::new(Line::from(spans)).style(base_style)
                }
            })
            .collect();

        let files_list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(CORNFLOWER_BLUE))
                    .style(Style::default().bg(DARK_BG))
                    .title(Line::from(vec![
                        Span::styled(" 📁 ", Style::default().fg(GOLD)),
                        Span::styled(
                            "Changed Files ",
                            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            format!("({})", git_state.changed_files.len()),
                            Style::default().fg(CORNFLOWER_BLUE).add_modifier(Modifier::BOLD),
                        ),
                    ]))
                    .title_bottom(Line::from(vec![
                        Span::styled(
                            " Enter",
                            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" toggle ", Style::default().fg(MUTED_GRAY)),
                        Span::styled("│", Style::default().fg(SUBDUED_BORDER)),
                        Span::styled(" e", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
                        Span::styled(" expand ", Style::default().fg(MUTED_GRAY)),
                        Span::styled("│", Style::default().fg(SUBDUED_BORDER)),
                        Span::styled(" E", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
                        Span::styled(" collapse ", Style::default().fg(MUTED_GRAY)),
                        Span::styled("│", Style::default().fg(SUBDUED_BORDER)),
                        Span::styled(
                            " Tab",
                            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" switch tab ", Style::default().fg(MUTED_GRAY)),
                    ])),
            )
            .highlight_style(Style::default().bg(LIST_HIGHLIGHT_BG));

        let mut list_state = ListState::default();
        list_state.select(Some(git_state.selected_tree_index));

        frame.render_stateful_widget(files_list, area, &mut list_state);
    }

    /// Build tree indentation string with proper line characters
    fn build_tree_indent(depth: usize, is_last: bool) -> String {
        if depth == 0 {
            return String::new();
        }

        let mut indent = String::new();
        // Add vertical lines for all but the last level
        for _ in 0..(depth - 1) {
            indent.push_str("│  ");
        }
        // Add the final branch character
        if is_last {
            indent.push_str("└─ ");
        } else {
            indent.push_str("├─ ");
        }
        indent
    }

    /// Get file icon based on extension
    fn get_file_icon(filename: &str) -> &'static str {
        if filename.ends_with(".rs") {
            "🦀"
        } else if filename.ends_with(".py") {
            "🐍"
        } else if filename.ends_with(".js") || filename.ends_with(".jsx") {
            "📜"
        } else if filename.ends_with(".ts") || filename.ends_with(".tsx") {
            "📘"
        } else if filename.ends_with(".md") || filename.ends_with(".markdown") {
            "📝"
        } else if filename.ends_with(".json") {
            "📋"
        } else if filename.ends_with(".toml")
            || filename.ends_with(".yaml")
            || filename.ends_with(".yml")
        {
            "⚙️"
        } else if filename.ends_with(".sh") || filename.ends_with(".bash") {
            "🖥️"
        } else if filename.ends_with(".html") {
            "🌐"
        } else if filename.ends_with(".css") || filename.ends_with(".scss") {
            "🎨"
        } else if filename.ends_with(".go") {
            "🐹"
        } else if filename.ends_with(".java") {
            "☕"
        } else {
            "📄"
        }
    }

    fn render_diff_tab(frame: &mut Frame, area: Rect, git_state: &GitViewState) {
        if git_state.diff_content.is_empty() {
            let no_diff = Paragraph::new(vec![
                Line::from(Span::styled(
                    "📋 No diff available",
                    Style::default().fg(MUTED_GRAY),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "Select a file in the Files tab to view its diff",
                    Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
                )),
            ])
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(CORNFLOWER_BLUE))
                    .style(Style::default().bg(DARK_BG))
                    .title(Line::from(vec![
                        Span::styled(" 📋 ", Style::default().fg(GOLD)),
                        Span::styled(
                            "Diff",
                            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                        ),
                    ])),
            )
            .wrap(Wrap { trim: true });
            frame.render_widget(no_diff, area);
            return;
        }

        // Calculate visible lines
        let content_height = area.height.saturating_sub(2) as usize; // Account for borders
        let start_line = git_state.diff_scroll_offset;
        let end_line = (start_line + content_height).min(git_state.diff_content.len());

        // Diff colors (enhanced for visibility)
        let addition_color = Color::Rgb(100, 200, 100); // Softer green
        let deletion_color = Color::Rgb(230, 100, 100); // Softer red
        let hunk_color = PROGRESS_CYAN;
        let file_header_color = WARNING_ORANGE;

        let visible_lines: Vec<Line> = git_state.diff_content[start_line..end_line]
            .iter()
            .map(|line| {
                let style = if line.starts_with('+') && !line.starts_with("+++") {
                    Style::default().fg(addition_color)
                } else if line.starts_with('-') && !line.starts_with("---") {
                    Style::default().fg(deletion_color)
                } else if line.starts_with("@@") {
                    Style::default().fg(hunk_color).add_modifier(Modifier::BOLD)
                } else if line.starts_with("+++") || line.starts_with("---") {
                    Style::default().fg(file_header_color).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(SOFT_WHITE)
                };

                Line::from(Span::styled(line.clone(), style))
            })
            .collect();

        let selected_file_name = git_state
            .changed_files
            .get(git_state.selected_file_index)
            .map(|f| f.path.as_str())
            .unwrap_or("No file selected");

        let scroll_info = format!(
            " [{}/{}]",
            git_state.diff_scroll_offset + 1,
            git_state.diff_content.len().max(1)
        );

        let diff_paragraph = Paragraph::new(visible_lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(CORNFLOWER_BLUE))
                    .style(Style::default().bg(DARK_BG))
                    .title(Line::from(vec![
                        Span::styled(" 📋 ", Style::default().fg(GOLD)),
                        Span::styled(
                            "Diff: ",
                            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(selected_file_name, Style::default().fg(SOFT_WHITE)),
                        Span::styled(scroll_info, Style::default().fg(MUTED_GRAY)),
                    ]))
                    .title_bottom(Line::from(vec![
                        Span::styled(
                            " j/k",
                            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" scroll ", Style::default().fg(MUTED_GRAY)),
                        Span::styled("│", Style::default().fg(SUBDUED_BORDER)),
                        Span::styled(
                            " Tab",
                            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" switch tab ", Style::default().fg(MUTED_GRAY)),
                    ])),
            )
            .wrap(Wrap { trim: false });

        frame.render_widget(diff_paragraph, area);
    }

    fn render_commits_tab(frame: &mut Frame, area: Rect, git_state: &GitViewState) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(CORNFLOWER_BLUE))
            .style(Style::default().bg(DARK_BG))
            .title(Line::from(vec![
                Span::styled(" 📜 ", Style::default().fg(GOLD)),
                Span::styled(
                    "Branch Commits",
                    Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                ),
            ]));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        if git_state.commits.is_empty() {
            let msg = Paragraph::new(vec![
                Line::from(Span::styled(
                    "No commits on this branch yet",
                    Style::default().fg(MUTED_GRAY),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "Commits since diverging from main/master will appear here",
                    Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
                )),
            ]);
            frame.render_widget(msg, inner);
            return;
        }

        // Build list items
        let items: Vec<ListItem> = git_state
            .commits
            .iter()
            .enumerate()
            .map(|(i, commit)| {
                let is_selected = i == git_state.selected_commit_index;
                let prefix = if is_selected { "▶ " } else { "  " };

                let line = Line::from(vec![
                    Span::raw(prefix),
                    Span::styled(&commit.hash_short, Style::default().fg(GOLD)),
                    Span::raw(" "),
                    Span::styled(
                        truncate_string(&commit.message, 50),
                        Style::default().fg(SOFT_WHITE),
                    ),
                    Span::raw(" - "),
                    Span::styled(&commit.author, Style::default().fg(MUTED_GRAY)),
                    Span::raw(" "),
                    Span::styled(
                        &commit.date,
                        Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
                    ),
                ]);

                let style = if is_selected {
                    Style::default().bg(LIST_HIGHLIGHT_BG)
                } else {
                    Style::default()
                };

                ListItem::new(line).style(style)
            })
            .collect();

        let mut list_state = ListState::default();
        list_state.select(Some(git_state.selected_commit_index));

        let list = List::new(items).highlight_style(Style::default().bg(LIST_HIGHLIGHT_BG));

        frame.render_stateful_widget(list, inner, &mut list_state);
    }

    fn render_markdown_tab(frame: &mut Frame, area: Rect, git_state: &GitViewState) {
        if git_state.markdown_content.is_empty() {
            let no_content = Paragraph::new(vec![
                Line::from(Span::styled(
                    "📝 No markdown content available",
                    Style::default().fg(MUTED_GRAY),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "Select a .md file in the Files tab",
                    Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
                )),
            ])
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(CORNFLOWER_BLUE))
                    .style(Style::default().bg(DARK_BG))
                    .title(Line::from(vec![
                        Span::styled(" 📝 ", Style::default().fg(GOLD)),
                        Span::styled(
                            "Markdown Preview",
                            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                        ),
                    ])),
            )
            .wrap(Wrap { trim: true });
            frame.render_widget(no_content, area);
            return;
        }

        // Calculate visible lines
        let content_height = area.height.saturating_sub(2) as usize; // Account for borders
        let start_line = git_state.markdown_scroll_offset;
        let end_line = (start_line + content_height).min(git_state.markdown_content.len());

        // Premium markdown colors
        let heading1_color = PROGRESS_CYAN;
        let heading2_color = CORNFLOWER_BLUE;
        let heading3_color = Color::Rgb(150, 150, 220);
        let code_bg = Color::Rgb(35, 35, 45);
        let code_fg = SELECTION_GREEN;
        let link_color = Color::Rgb(100, 180, 255);
        let quote_color = MUTED_GRAY;

        let visible_lines: Vec<Line> = git_state.markdown_content[start_line..end_line]
            .iter()
            .map(|md_line| {
                let style = match &md_line.style {
                    MarkdownStyle::Heading1 => Style::default()
                        .fg(heading1_color)
                        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
                    MarkdownStyle::Heading2 => {
                        Style::default().fg(heading2_color).add_modifier(Modifier::BOLD)
                    }
                    MarkdownStyle::Heading3 => {
                        Style::default().fg(heading3_color).add_modifier(Modifier::BOLD)
                    }
                    MarkdownStyle::Paragraph => Style::default().fg(SOFT_WHITE),
                    MarkdownStyle::CodeBlock => Style::default().fg(code_fg).bg(code_bg),
                    MarkdownStyle::CodeBlockHeader(_) => {
                        Style::default().fg(GOLD).add_modifier(Modifier::BOLD)
                    }
                    MarkdownStyle::ListItem => Style::default().fg(SOFT_WHITE),
                    MarkdownStyle::Bold => {
                        Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD)
                    }
                    MarkdownStyle::Italic => {
                        Style::default().fg(SOFT_WHITE).add_modifier(Modifier::ITALIC)
                    }
                    MarkdownStyle::InlineCode => {
                        Style::default().fg(Color::Rgb(220, 150, 220)).bg(code_bg)
                    }
                    MarkdownStyle::Link => {
                        Style::default().fg(link_color).add_modifier(Modifier::UNDERLINED)
                    }
                    MarkdownStyle::BlockQuote => {
                        Style::default().fg(quote_color).add_modifier(Modifier::ITALIC)
                    }
                };

                Line::from(Span::styled(md_line.content.clone(), style))
            })
            .collect();

        // Get selected file name for title
        let file_name = git_state
            .file_tree_items
            .get(git_state.selected_tree_index)
            .map(|item| item.display_name.as_str())
            .unwrap_or("Markdown");

        let scroll_info = format!(
            " [{}/{}]",
            git_state.markdown_scroll_offset + 1,
            git_state.markdown_content.len().max(1)
        );

        let markdown_paragraph = Paragraph::new(visible_lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(CORNFLOWER_BLUE))
                    .style(Style::default().bg(DARK_BG))
                    .title(Line::from(vec![
                        Span::styled(" 📝 ", Style::default().fg(GOLD)),
                        Span::styled(
                            file_name,
                            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(scroll_info, Style::default().fg(MUTED_GRAY)),
                    ]))
                    .title_bottom(Line::from(vec![
                        Span::styled(
                            " j/k",
                            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" scroll ", Style::default().fg(MUTED_GRAY)),
                        Span::styled("│", Style::default().fg(SUBDUED_BORDER)),
                        Span::styled(
                            " Tab",
                            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" switch tab ", Style::default().fg(MUTED_GRAY)),
                    ])),
            )
            .wrap(Wrap { trim: false });

        frame.render_widget(markdown_paragraph, area);
    }

    fn render_commit_input(frame: &mut Frame, area: Rect, git_state: &GitViewState) {
        let empty_string = String::new();
        let commit_message = git_state.commit_message_input.as_ref().unwrap_or(&empty_string);

        // Create spans with cursor visualization
        let (before_cursor, after_cursor) =
            commit_message.split_at(git_state.commit_message_cursor.min(commit_message.len()));

        let input_line = Line::from(vec![
            Span::styled(before_cursor, Style::default().fg(SOFT_WHITE)),
            Span::styled("█", Style::default().fg(SELECTION_GREEN)),
            Span::styled(after_cursor, Style::default().fg(SOFT_WHITE)),
        ]);

        let input_paragraph = Paragraph::new(input_line)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(SELECTION_GREEN))
                    .style(Style::default().bg(Color::Rgb(35, 35, 45)))
                    .title(Line::from(vec![
                        Span::styled(" ✏️ ", Style::default().fg(GOLD)),
                        Span::styled(
                            "Commit Message",
                            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                        ),
                    ]))
                    .title_bottom(Line::from(vec![
                        Span::styled(
                            " Enter",
                            Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" commit ", Style::default().fg(MUTED_GRAY)),
                        Span::styled("│", Style::default().fg(SUBDUED_BORDER)),
                        Span::styled(
                            " Esc",
                            Style::default().fg(WARNING_ORANGE).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" cancel ", Style::default().fg(MUTED_GRAY)),
                        Span::styled("│", Style::default().fg(SUBDUED_BORDER)),
                        Span::styled(
                            " ←/→",
                            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" cursor ", Style::default().fg(MUTED_GRAY)),
                    ])),
            )
            .wrap(Wrap { trim: false });

        frame.render_widget(input_paragraph, area);
    }

    fn render_status_bar(frame: &mut Frame, area: Rect, git_state: &GitViewState) {
        let (status_icon, status_text, status_color) = if git_state.is_dirty {
            (
                "🔄",
                format!("{} files changed", git_state.changed_files.len()),
                WARNING_ORANGE,
            )
        } else {
            ("✓", "Working directory clean".to_string(), SELECTION_GREEN)
        };

        let (push_icon, push_text, push_color) = if git_state.can_push {
            ("🚀", "Ready to push", SELECTION_GREEN)
        } else {
            ("✓", "Up to date", MUTED_GRAY)
        };

        // Build the status line with rich formatting
        let status_line = Line::from(vec![
            Span::styled(
                format!(" {} ", status_icon),
                Style::default().fg(status_color),
            ),
            Span::styled(&status_text, Style::default().fg(status_color)),
            Span::styled("  │  ", Style::default().fg(SUBDUED_BORDER)),
            Span::styled(format!("{} ", push_icon), Style::default().fg(push_color)),
            Span::styled(push_text, Style::default().fg(push_color)),
            Span::styled("  │  ", Style::default().fg(SUBDUED_BORDER)),
            Span::styled("p", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
            Span::styled(" push ", Style::default().fg(MUTED_GRAY)),
            Span::styled("│", Style::default().fg(SUBDUED_BORDER)),
            Span::styled(
                " Esc",
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" back ", Style::default().fg(MUTED_GRAY)),
        ]);

        let status_paragraph = Paragraph::new(status_line)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(SUBDUED_BORDER))
                    .style(Style::default().bg(PANEL_BG)),
            )
            .wrap(Wrap { trim: true });

        frame.render_widget(status_paragraph, area);
    }
}

/// Truncate a string to at most `max_len` displayed chars. Delegates to
/// the canonical `truncate_with_ellipsis` (uses `…`, single Unicode char).
/// Renamed-on-merge from a per-file copy that used `...` (3 ASCII chars).
fn truncate_string(s: &str, max_len: usize) -> String {
    crate::widgets::truncate_with_ellipsis(s, max_len).into_owned()
}
