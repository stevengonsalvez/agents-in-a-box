// ABOUTME: Application state management and view switching logic for agents-in-a-box TUI

#![allow(dead_code)]

use crate::app::SessionLoader;
use crate::audit::{self, AuditResult, AuditTrigger};
use crate::claude::client::ClaudeChatManager;
use crate::claude::types::ClaudeStreamingEvent;
use crate::claude::{ClaudeApiClient, ClaudeMessage};
use crate::components::fuzzy_file_finder::FuzzyFileFinderState;
use crate::components::home_screen_v2::HomeScreenV2State;
use crate::components::live_logs_stream::LogEntry;
use crate::config::{AppConfig, SshDisplayNameStore, WorktreeCollisionBehavior};
use crate::credentials;
use crate::docker::LogStreamingCoordinator;
use crate::editors;
use crate::git::{ParsedRepo, RemoteBranch, RepoSource};
use crate::models::{ClaudeModel, Session, SessionAgentType, Workspace};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use chrono;
use ratatui::layout::Rect;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tracing::{debug, error, info, trace, warn};
use uuid::Uuid;

/// Location of an attachable row inside `AppState`, independent of the
/// row's current visible position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachableRef {
    WorkspaceSession {
        workspace_idx: usize,
        session_idx: usize,
    },
    WorkspaceShell {
        workspace_idx: usize,
    },
    SshSession {
        ssh_idx: usize,
    },
    OtherTmux {
        other_idx: usize,
    },
}

/// Text editor with cursor support for boss mode prompts
#[derive(Debug, Clone)]
pub struct TextEditor {
    lines: Vec<String>,
    cursor_line: usize,
    cursor_col: usize,
}

impl TextEditor {
    pub fn new() -> Self {
        Self {
            lines: vec![String::new()],
            cursor_line: 0,
            cursor_col: 0,
        }
    }

    pub fn from_string(text: &str) -> Self {
        let lines: Vec<String> = if text.is_empty() {
            vec![String::new()]
        } else {
            text.lines().map(|s| s.to_string()).collect()
        };

        Self {
            lines,
            cursor_line: 0,
            cursor_col: 0,
        }
    }

    pub fn to_string(&self) -> String {
        self.lines.join("\n")
    }

    pub fn is_empty(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].is_empty()
    }

    pub fn insert_char(&mut self, ch: char) {
        if ch == '\n' {
            self.insert_newline();
        } else {
            let line = &mut self.lines[self.cursor_line];
            line.insert(self.cursor_col, ch);
            self.cursor_col += 1;
        }
    }

    pub fn insert_newline(&mut self) {
        let current_line = self.lines[self.cursor_line].clone();
        let (left, right) = current_line.split_at(self.cursor_col);

        self.lines[self.cursor_line] = left.to_string();
        self.lines.insert(self.cursor_line + 1, right.to_string());

        self.cursor_line += 1;
        self.cursor_col = 0;
    }

    pub fn backspace(&mut self) {
        if self.cursor_col > 0 {
            // Delete character before cursor
            self.lines[self.cursor_line].remove(self.cursor_col - 1);
            self.cursor_col -= 1;
        } else if self.cursor_line > 0 {
            // Join with previous line
            let current_line = self.lines.remove(self.cursor_line);
            self.cursor_line -= 1;
            self.cursor_col = self.lines[self.cursor_line].len();
            self.lines[self.cursor_line].push_str(&current_line);
        }
    }

    pub fn move_cursor_left(&mut self) {
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
        } else if self.cursor_line > 0 {
            self.cursor_line -= 1;
            self.cursor_col = self.lines[self.cursor_line].len();
        }
    }

    pub fn move_cursor_right(&mut self) {
        if self.cursor_col < self.lines[self.cursor_line].len() {
            self.cursor_col += 1;
        } else if self.cursor_line < self.lines.len() - 1 {
            self.cursor_line += 1;
            self.cursor_col = 0;
        }
    }

    pub fn move_cursor_up(&mut self) {
        if self.cursor_line > 0 {
            self.cursor_line -= 1;
            self.cursor_col = self.cursor_col.min(self.lines[self.cursor_line].len());
        }
    }

    pub fn move_cursor_down(&mut self) {
        if self.cursor_line < self.lines.len() - 1 {
            self.cursor_line += 1;
            self.cursor_col = self.cursor_col.min(self.lines[self.cursor_line].len());
        }
    }

    pub fn move_to_line_start(&mut self) {
        self.cursor_col = 0;
    }

    pub fn move_to_line_end(&mut self) {
        self.cursor_col = self.lines[self.cursor_line].len();
    }

    pub fn insert_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }

        let mut lines = text.lines();

        // Insert first line of text at current cursor position
        if let Some(first_line) = lines.next() {
            self.lines[self.cursor_line].insert_str(self.cursor_col, first_line);
            self.cursor_col += first_line.len();
        }

        // Insert newlines and subsequent lines
        for line in lines {
            self.insert_newline();
            self.lines[self.cursor_line].insert_str(self.cursor_col, line);
            self.cursor_col += line.len();
        }
    }

    pub fn get_cursor_position(&self) -> (usize, usize) {
        (self.cursor_line, self.cursor_col)
    }

    pub fn get_lines(&self) -> &Vec<String> {
        &self.lines
    }

    pub fn move_cursor_to_end(&mut self) {
        if !self.lines.is_empty() {
            self.cursor_line = self.lines.len() - 1;
            self.cursor_col = self.lines[self.cursor_line].len();
        }
    }

    pub fn set_cursor_position(&mut self, line: usize, col: usize) {
        if line < self.lines.len() {
            self.cursor_line = line;
            self.cursor_col = col.min(self.lines[line].len());
        }
    }

    // Word movement methods
    pub fn move_cursor_word_forward(&mut self) {
        let current_line = &self.lines[self.cursor_line];

        // If at end of line, move to next line
        if self.cursor_col >= current_line.len() {
            if self.cursor_line < self.lines.len() - 1 {
                self.cursor_line += 1;
                self.cursor_col = 0;
                // Find first non-whitespace character
                let next_line = &self.lines[self.cursor_line];
                while self.cursor_col < next_line.len()
                    && next_line.chars().nth(self.cursor_col).unwrap().is_whitespace()
                {
                    self.cursor_col += 1;
                }
            }
            return;
        }

        let chars: Vec<char> = current_line.chars().collect();
        let mut pos = self.cursor_col;

        // Skip current word
        while pos < chars.len()
            && !chars[pos].is_whitespace()
            && chars[pos] != '.'
            && chars[pos] != ','
        {
            pos += 1;
        }

        // Skip whitespace
        while pos < chars.len() && chars[pos].is_whitespace() {
            pos += 1;
        }

        self.cursor_col = pos;
    }

    pub fn move_cursor_word_backward(&mut self) {
        // If at beginning of line, move to end of previous line
        if self.cursor_col == 0 {
            if self.cursor_line > 0 {
                self.cursor_line -= 1;
                self.cursor_col = self.lines[self.cursor_line].len();
            }
            return;
        }

        let current_line = &self.lines[self.cursor_line];
        let chars: Vec<char> = current_line.chars().collect();
        let mut pos = self.cursor_col.saturating_sub(1);

        // Skip whitespace backwards
        while pos > 0 && chars[pos].is_whitespace() {
            pos = pos.saturating_sub(1);
        }

        // Skip word backwards
        while pos > 0 && !chars[pos].is_whitespace() && chars[pos] != '.' && chars[pos] != ',' {
            pos = pos.saturating_sub(1);
        }

        // If we stopped on whitespace or punctuation, move forward one
        if pos > 0 && (chars[pos].is_whitespace() || chars[pos] == '.' || chars[pos] == ',') {
            pos += 1;
        }

        self.cursor_col = pos;
    }

    // Word deletion methods
    pub fn delete_word_forward(&mut self) {
        let current_line_text = self.lines[self.cursor_line].clone();
        let chars: Vec<char> = current_line_text.chars().collect();
        let start_pos = self.cursor_col;

        if start_pos >= chars.len() {
            return;
        }

        let mut end_pos = start_pos;

        // Skip current word
        while end_pos < chars.len()
            && !chars[end_pos].is_whitespace()
            && chars[end_pos] != '.'
            && chars[end_pos] != ','
        {
            end_pos += 1;
        }

        // Skip following whitespace
        while end_pos < chars.len() && chars[end_pos].is_whitespace() {
            end_pos += 1;
        }

        // Remove the text
        let before: String = chars[..start_pos].iter().collect();
        let after: String = chars[end_pos..].iter().collect();
        self.lines[self.cursor_line] = format!("{}{}", before, after);
    }

    pub fn delete_word_backward(&mut self) {
        if self.cursor_col == 0 {
            return;
        }

        let current_line_text = self.lines[self.cursor_line].clone();
        let chars: Vec<char> = current_line_text.chars().collect();
        let end_pos = self.cursor_col;
        let mut start_pos = end_pos.saturating_sub(1);

        // Skip whitespace backwards
        while start_pos > 0 && chars[start_pos].is_whitespace() {
            start_pos = start_pos.saturating_sub(1);
        }

        // Skip word backwards
        while start_pos > 0
            && !chars[start_pos].is_whitespace()
            && chars[start_pos] != '.'
            && chars[start_pos] != ','
        {
            start_pos = start_pos.saturating_sub(1);
        }

        // If we stopped on whitespace or punctuation, move forward one
        if start_pos > 0
            && (chars[start_pos].is_whitespace()
                || chars[start_pos] == '.'
                || chars[start_pos] == ',')
        {
            start_pos += 1;
        }

        // Remove the text
        let before: String = chars[..start_pos].iter().collect();
        let after: String = chars[end_pos..].iter().collect();
        self.lines[self.cursor_line] = format!("{}{}", before, after);
        self.cursor_col = start_pos;
    }
}

/// Notification system for TUI messages
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotificationType {
    Success,
    Error,
    Info,
    Warning,
}

#[derive(Debug, Clone)]
pub struct Notification {
    pub message: String,
    pub notification_type: NotificationType,
    pub created_at: Instant,
    pub duration: Duration,
}

impl Notification {
    pub fn success(message: String) -> Self {
        Self {
            message,
            notification_type: NotificationType::Success,
            created_at: Instant::now(),
            duration: Duration::from_secs(3),
        }
    }

    pub fn error(message: String) -> Self {
        Self {
            message,
            notification_type: NotificationType::Error,
            created_at: Instant::now(),
            duration: Duration::from_secs(5),
        }
    }

    pub fn info(message: String) -> Self {
        Self {
            message,
            notification_type: NotificationType::Info,
            created_at: Instant::now(),
            duration: Duration::from_secs(3),
        }
    }

    pub fn warning(message: String) -> Self {
        Self {
            message,
            notification_type: NotificationType::Warning,
            created_at: Instant::now(),
            duration: Duration::from_secs(4),
        }
    }

    pub fn is_expired(&self) -> bool {
        self.created_at.elapsed() > self.duration
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FocusedPane {
    Sessions, // Left pane - workspace/session list
    LiveLogs, // Right pane - live logs
}

pub const DEFAULT_SESSIONS_SIDEBAR_WIDTH: u16 = 40;
pub const MIN_SESSIONS_SIDEBAR_WIDTH: u16 = 24;
pub const SESSIONS_PREVIEW_RESERVE: u16 = 50;
pub const COLLAPSED_SESSIONS_SIDEBAR_WIDTH: u16 = 5;
pub const SESSIONS_ROW_DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(300);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionListRowTarget {
    WorkspaceHeader { workspace_idx: usize },
    SshHeader,
    OtherTmuxHeader,
    Attachable(AttachableRef),
}

#[derive(Debug, Clone)]
pub struct SessionsPaneState {
    pub preferred_width: u16,
    pub collapsed: bool,
    resize_active: bool,
    edge_hovered: bool,
    last_sessions_rect: Option<Rect>,
    last_preview_rect: Option<Rect>,
    last_list_scroll_offset: usize,
    last_attachable_click: Option<(AttachableRef, Instant)>,
}

impl Default for SessionsPaneState {
    fn default() -> Self {
        Self {
            preferred_width: DEFAULT_SESSIONS_SIDEBAR_WIDTH,
            collapsed: false,
            resize_active: false,
            edge_hovered: false,
            last_sessions_rect: None,
            last_preview_rect: None,
            last_list_scroll_offset: 0,
            last_attachable_click: None,
        }
    }
}

impl SessionsPaneState {
    pub fn restore(&mut self, width: Option<u16>, collapsed: bool) {
        if let Some(width) = width {
            self.preferred_width = width.max(MIN_SESSIONS_SIDEBAR_WIDTH);
        }
        self.collapsed = collapsed;
    }

    pub fn set_layout(&mut self, sessions_rect: Rect, preview_rect: Rect) {
        self.last_sessions_rect = Some(sessions_rect);
        self.last_preview_rect = Some(preview_rect);
    }

    pub fn set_list_scroll_offset(&mut self, offset: usize) {
        self.last_list_scroll_offset = offset;
    }

    pub fn last_content_width(&self) -> Option<u16> {
        Some(self.last_sessions_rect?.width.saturating_add(self.last_preview_rect?.width))
    }

    pub fn effective_width(&self, terminal_width: u16) -> u16 {
        if self.collapsed {
            return COLLAPSED_SESSIONS_SIDEBAR_WIDTH.min(terminal_width);
        }

        Self::clamp_width(self.preferred_width, terminal_width)
    }

    pub fn clamp_width(width: u16, terminal_width: u16) -> u16 {
        if terminal_width <= COLLAPSED_SESSIONS_SIDEBAR_WIDTH {
            return terminal_width;
        }

        let max_width = terminal_width.saturating_sub(SESSIONS_PREVIEW_RESERVE);
        if max_width < MIN_SESSIONS_SIDEBAR_WIDTH {
            return terminal_width.saturating_sub(1).max(1);
        }

        width.clamp(MIN_SESSIONS_SIDEBAR_WIDTH, max_width)
    }

    pub fn expanded_width(&self, terminal_width: u16) -> u16 {
        Self::clamp_width(self.preferred_width, terminal_width)
    }

    pub fn edge_highlighted(&self) -> bool {
        self.edge_hovered || self.resize_active
    }

    pub fn is_on_edge(&self, x: u16, y: u16) -> bool {
        if self.collapsed {
            return false;
        }

        let Some(rect) = self.last_sessions_rect else {
            return false;
        };
        if y < rect.y || y >= rect.y.saturating_add(rect.height) || rect.width == 0 {
            return false;
        }

        let edge_x = rect.x.saturating_add(rect.width.saturating_sub(1));
        x.abs_diff(edge_x) <= 1
    }

    pub fn is_on_toggle(&self, x: u16, y: u16) -> bool {
        let Some(rect) = self.last_sessions_rect else {
            return false;
        };
        if rect.width == 0 {
            return false;
        }

        let on_x = x >= rect.x && x < rect.x.saturating_add(rect.width);
        if !on_x {
            return false;
        }

        if self.collapsed {
            // Expanded pane puts `[-]` in the block title on the top border.
            // Collapsed rail renders `[+]` as first content row inside the block.
            return y == rect.y || y == rect.y.saturating_add(1);
        }

        y == rect.y
    }

    pub fn contains_sessions_point(&self, x: u16, y: u16) -> bool {
        let Some(rect) = self.last_sessions_rect else {
            return false;
        };
        x >= rect.x
            && x < rect.x.saturating_add(rect.width)
            && y >= rect.y
            && y < rect.y.saturating_add(rect.height)
    }

    pub fn contains_preview_point(&self, x: u16, y: u16) -> bool {
        let Some(rect) = self.last_preview_rect else {
            return false;
        };
        x >= rect.x
            && x < rect.x.saturating_add(rect.width)
            && y >= rect.y
            && y < rect.y.saturating_add(rect.height)
    }

    pub fn row_index_at(&self, x: u16, y: u16) -> Option<usize> {
        if self.collapsed {
            return None;
        }
        let rect = self.last_sessions_rect?;
        if x < rect.x
            || x >= rect.x.saturating_add(rect.width)
            || y <= rect.y
            || y >= rect.y.saturating_add(rect.height.saturating_sub(1))
        {
            return None;
        }

        Some(self.last_list_scroll_offset + usize::from(y - rect.y - 1))
    }

    pub fn record_row_click(&mut self, target: SessionListRowTarget, now: Instant) -> bool {
        let SessionListRowTarget::Attachable(target) = target else {
            self.last_attachable_click = None;
            return false;
        };

        let double_click = self
            .last_attachable_click
            .map(|(last_target, last_at)| {
                last_target == target
                    && now.saturating_duration_since(last_at) <= SESSIONS_ROW_DOUBLE_CLICK_WINDOW
            })
            .unwrap_or(false);
        self.last_attachable_click = Some((target, now));
        double_click
    }

    pub fn begin_resize(&mut self, x: u16, y: u16) -> bool {
        if self.is_on_edge(x, y) {
            self.resize_active = true;
            self.edge_hovered = true;
            true
        } else {
            false
        }
    }

    pub fn drag_resize(&mut self, x: u16, terminal_width: u16) {
        if !self.resize_active || self.collapsed {
            return;
        }

        let Some(rect) = self.last_sessions_rect else {
            return;
        };
        let requested = x.saturating_sub(rect.x).saturating_add(1);
        self.preferred_width = Self::clamp_width(requested, terminal_width);
    }

    pub fn finish_resize(&mut self) -> bool {
        let was_active = self.resize_active;
        self.resize_active = false;
        was_active
    }

    pub fn update_hover(&mut self, x: u16, y: u16) {
        self.edge_hovered = self.is_on_edge(x, y);
    }

    pub fn toggle_collapsed(&mut self) {
        self.collapsed = !self.collapsed;
        self.resize_active = false;
        self.edge_hovered = false;
    }
}

// View enum was replaced in Phase 2a by ScreenId (String) + the screens::ids
// constants module. Layout dispatch now goes through app::ScreenRegistry; see
// `crate::app::screens` for the trait + identifier constants.
pub use crate::app::screens::{Screen, ScreenId, ids as screen_ids};

#[derive(Debug, Clone)]
pub struct ConfirmationDialog {
    pub title: String,
    pub message: String,
    pub confirm_action: ConfirmAction,
    pub selected_option: bool,   // true = Yes, false = No (binary mode)
    pub warning: Option<String>, // Optional warning (e.g., uncommitted files in worktree)
    // Tri-option mode: when `options` is `Some`, the dialog renders one button per
    // entry and Left/Right cycles `selected_index`. The final-option index is
    // treated as Cancel by the confirm handler.
    pub options: Option<Vec<DialogOption>>,
    pub selected_index: usize,
}

/// One choice in a tri-option (or n-option) confirmation dialog.
#[derive(Debug, Clone)]
pub struct DialogOption {
    pub label: String,
    pub action: ConfirmAction,
}

#[derive(Debug, Clone)]
pub enum ConfirmAction {
    DeleteSession(Uuid),
    StopSession(Uuid), // Soft-stop interactive session (tmux only; preserves worktree)
    KillOtherTmux(String), // Kill a non-agents-in-a-box tmux session by name
    KillOtherTmuxSessions(Vec<String>), // Kill multiple non-agents-in-a-box tmux sessions by name
    KillWorkspaceShell(usize), // Kill workspace shell by workspace index
    Cancel,            // No-op terminator for tri-option dialogs
}

// ============================================================================
// Home Screen State
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HomeTile {
    Agents,   // Agent selection
    Catalog,  // Browse catalog/marketplace
    Config,   // Settings & presets
    Sessions, // Session manager
    Recovery, // Recover orphaned sessions
    Stats,    // Analytics & usage
    Help,     // Docs & guides
}

impl HomeTile {
    pub fn all() -> Vec<HomeTile> {
        vec![
            HomeTile::Agents,
            HomeTile::Catalog,
            HomeTile::Config,
            HomeTile::Sessions,
            HomeTile::Recovery,
            HomeTile::Stats,
            HomeTile::Help,
        ]
    }

    pub fn label(&self) -> &'static str {
        match self {
            HomeTile::Agents => "Agents",
            HomeTile::Catalog => "Catalog",
            HomeTile::Config => "Config",
            HomeTile::Sessions => "Sessions",
            HomeTile::Recovery => "Recovery",
            HomeTile::Stats => "Stats",
            HomeTile::Help => "Help",
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            HomeTile::Agents => "Select & Configure",
            HomeTile::Catalog => "Browse & Bootstrap",
            HomeTile::Config => "Settings & Presets",
            HomeTile::Sessions => "Manage Active",
            HomeTile::Recovery => "Resume Orphaned",
            HomeTile::Stats => "Usage & Analytics",
            HomeTile::Help => "Docs & Guides",
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            HomeTile::Agents => "🤖",
            HomeTile::Catalog => "📦",
            HomeTile::Config => "⚙️",
            HomeTile::Sessions => "🚀",
            HomeTile::Recovery => "🔄",
            HomeTile::Stats => "📊",
            HomeTile::Help => "❓",
        }
    }
}

#[derive(Debug, Clone)]
pub struct HomeScreenState {
    pub selected_tile: usize,
    pub tiles: Vec<HomeTile>,
}

impl Default for HomeScreenState {
    fn default() -> Self {
        Self {
            selected_tile: 0,
            tiles: HomeTile::all(),
        }
    }
}

impl HomeScreenState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn selected(&self) -> Option<&HomeTile> {
        self.tiles.get(self.selected_tile)
    }

    pub fn select_next(&mut self) {
        if !self.tiles.is_empty() {
            self.selected_tile = (self.selected_tile + 1) % self.tiles.len();
        }
    }

    pub fn select_prev(&mut self) {
        if !self.tiles.is_empty() {
            self.selected_tile = if self.selected_tile == 0 {
                self.tiles.len() - 1
            } else {
                self.selected_tile - 1
            };
        }
    }

    pub fn select_right(&mut self) {
        // 2x3 grid: move right wraps within row
        let col = self.selected_tile % 3;
        let row = self.selected_tile / 3;
        let new_col = (col + 1) % 3;
        self.selected_tile = row * 3 + new_col;
    }

    pub fn select_left(&mut self) {
        // 2x3 grid: move left wraps within row
        let col = self.selected_tile % 3;
        let row = self.selected_tile / 3;
        let new_col = if col == 0 { 2 } else { col - 1 };
        self.selected_tile = row * 3 + new_col;
    }

    pub fn select_down(&mut self) {
        // 2x3 grid: move down wraps to top
        let col = self.selected_tile % 3;
        let row = self.selected_tile / 3;
        let new_row = (row + 1) % 2;
        self.selected_tile = new_row * 3 + col;
    }

    pub fn select_up(&mut self) {
        // 2x3 grid: move up wraps to bottom
        let col = self.selected_tile % 3;
        let row = self.selected_tile / 3;
        let new_row = if row == 0 { 1 } else { 0 };
        self.selected_tile = new_row * 3 + col;
    }
}

// ============================================================================
// Agent Selection State
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderStatus {
    Available,
    ComingSoon,
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CostTier {
    Low,
    Medium,
    High,
    Premium,
}

// ============================================================================
// SESSION AGENT SELECTION (for new session flow)
// ============================================================================

// SessionAgentType is imported from crate::models

/// Option in the agent selection list
#[derive(Debug, Clone)]
pub struct SessionAgentOption {
    pub agent_type: SessionAgentType,
    pub is_current: bool, // Is this the currently selected agent for the app?
}

impl SessionAgentOption {
    pub fn all() -> Vec<Self> {
        vec![
            Self {
                agent_type: SessionAgentType::Claude,
                is_current: true,
            }, // Claude is default
            Self {
                agent_type: SessionAgentType::Shell,
                is_current: false,
            },
            Self {
                agent_type: SessionAgentType::Ssh,
                is_current: false,
            }, // SSH sessions
            Self {
                agent_type: SessionAgentType::Codex,
                is_current: false,
            },
            Self {
                agent_type: SessionAgentType::Gemini,
                is_current: false,
            },
            Self {
                agent_type: SessionAgentType::Copilot,
                is_current: false,
            },
            Self {
                agent_type: SessionAgentType::Kiro,
                is_current: false,
            },
        ]
    }
}

#[derive(Debug, Clone)]
pub struct AgentModel {
    pub name: String,
    pub description: String,
    pub cost_tier: CostTier,
    pub is_recommended: bool,
}

impl AgentModel {
    pub fn new(name: &str, description: &str, cost_tier: CostTier, is_recommended: bool) -> Self {
        Self {
            name: name.to_string(),
            description: description.to_string(),
            cost_tier,
            is_recommended,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AgentProvider {
    pub name: String,
    pub vendor: String,
    pub models: Vec<AgentModel>,
    pub status: ProviderStatus,
}

impl AgentProvider {
    pub fn claude() -> Self {
        Self {
            name: "Claude Code".to_string(),
            vendor: "Anthropic".to_string(),
            models: vec![
                AgentModel::new(
                    "Opus 4.5",
                    "Best reasoning, complex tasks",
                    CostTier::Premium,
                    false,
                ),
                AgentModel::new("Sonnet 4.5", "Balanced (Recommended)", CostTier::High, true),
                AgentModel::new("Haiku 4.5", "Fast, lightweight", CostTier::Medium, false),
            ],
            status: ProviderStatus::Available,
        }
    }

    pub fn codex() -> Self {
        Self {
            name: "Codex CLI".to_string(),
            vendor: "OpenAI".to_string(),
            models: vec![
                AgentModel::new(
                    "gpt-5.2-codex",
                    "Latest frontier agentic coding model",
                    CostTier::Premium,
                    true,
                ),
                AgentModel::new(
                    "gpt-5.1-codex-max",
                    "Deep and fast reasoning flagship",
                    CostTier::High,
                    false,
                ),
                AgentModel::new(
                    "gpt-5.1-codex-mini",
                    "Cheaper, faster, less capable",
                    CostTier::Medium,
                    false,
                ),
                AgentModel::new(
                    "gpt-5.2",
                    "Frontier model, reasoning & coding",
                    CostTier::Premium,
                    false,
                ),
            ],
            status: ProviderStatus::Available,
        }
    }

    pub fn gemini() -> Self {
        Self {
            name: "Gemini CLI".to_string(),
            vendor: "Google".to_string(),
            models: vec![
                AgentModel::new(
                    "gemini-3-pro",
                    "Latest reasoning model (preview)",
                    CostTier::Premium,
                    false,
                ),
                AgentModel::new(
                    "gemini-3-flash",
                    "Fast agentic model (preview)",
                    CostTier::High,
                    false,
                ),
                AgentModel::new(
                    "gemini-2.5-pro",
                    "1M context, adaptive thinking",
                    CostTier::High,
                    true,
                ),
                AgentModel::new(
                    "gemini-2.5-flash",
                    "Fast multimodal model",
                    CostTier::Medium,
                    false,
                ),
                AgentModel::new(
                    "gemini-2.5-flash-lite",
                    "Ultra-efficient, low cost",
                    CostTier::Low,
                    false,
                ),
            ],
            status: ProviderStatus::Available,
        }
    }

    pub fn copilot() -> Self {
        Self {
            name: "GitHub Copilot".to_string(),
            vendor: "GitHub".to_string(),
            models: vec![
                AgentModel::new(
                    "claude-sonnet-4.6",
                    "Claude Sonnet 4.6 (default)",
                    CostTier::High,
                    true,
                ),
                AgentModel::new(
                    "claude-opus-4.6",
                    "Claude Opus 4.6 (premium)",
                    CostTier::Premium,
                    false,
                ),
                AgentModel::new(
                    "claude-haiku-4.5",
                    "Claude Haiku 4.5 (fast)",
                    CostTier::Low,
                    false,
                ),
                AgentModel::new("gpt-5.2", "GPT-5.2", CostTier::High, false),
                AgentModel::new(
                    "gpt-5.1-codex",
                    "GPT-5.1 Codex (coding)",
                    CostTier::High,
                    false,
                ),
                AgentModel::new("gpt-4.1", "GPT-4.1 (stable)", CostTier::Medium, false),
                AgentModel::new(
                    "gemini-3-pro-preview",
                    "Gemini 3 Pro (preview)",
                    CostTier::High,
                    false,
                ),
            ],
            status: ProviderStatus::Available,
        }
    }

    pub fn local() -> Self {
        Self {
            name: "Local Models".to_string(),
            vendor: "Ollama".to_string(),
            models: vec![AgentModel::new(
                "Configurable",
                "Self-hosted models",
                CostTier::Low,
                true,
            )],
            status: ProviderStatus::ComingSoon,
        }
    }

    pub fn all() -> Vec<AgentProvider> {
        vec![
            Self::claude(),
            Self::codex(),
            Self::gemini(),
            Self::copilot(),
            Self::local(),
        ]
    }
}

#[derive(Debug, Clone)]
pub struct AgentSelectionState {
    pub selected_provider: usize,
    pub selected_model: usize,
    pub providers: Vec<AgentProvider>,
    pub expanded_provider: Option<usize>, // Which provider is expanded to show models
}

impl Default for AgentSelectionState {
    fn default() -> Self {
        Self {
            selected_provider: 0,
            selected_model: 0,
            providers: AgentProvider::all(),
            expanded_provider: Some(0), // Claude expanded by default
        }
    }
}

impl AgentSelectionState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn current_provider(&self) -> Option<&AgentProvider> {
        self.providers.get(self.selected_provider)
    }

    pub fn current_model(&self) -> Option<&AgentModel> {
        self.current_provider().and_then(|p| p.models.get(self.selected_model))
    }

    pub fn select_next_provider(&mut self) {
        if !self.providers.is_empty() {
            self.selected_provider = (self.selected_provider + 1) % self.providers.len();
            self.selected_model = 0;
            self.expanded_provider = Some(self.selected_provider);
        }
    }

    pub fn select_prev_provider(&mut self) {
        if !self.providers.is_empty() {
            self.selected_provider = if self.selected_provider == 0 {
                self.providers.len() - 1
            } else {
                self.selected_provider - 1
            };
            self.selected_model = 0;
            self.expanded_provider = Some(self.selected_provider);
        }
    }

    pub fn select_next_model(&mut self) {
        if let Some(provider) = self.current_provider() {
            if !provider.models.is_empty() {
                self.selected_model = (self.selected_model + 1) % provider.models.len();
            }
        }
    }

    pub fn select_prev_model(&mut self) {
        if let Some(provider) = self.current_provider() {
            if !provider.models.is_empty() {
                self.selected_model = if self.selected_model == 0 {
                    provider.models.len() - 1
                } else {
                    self.selected_model - 1
                };
            }
        }
    }

    pub fn toggle_expand(&mut self) {
        if self.expanded_provider == Some(self.selected_provider) {
            self.expanded_provider = None;
        } else {
            self.expanded_provider = Some(self.selected_provider);
        }
    }

    pub fn is_current_available(&self) -> bool {
        self.current_provider()
            .map(|p| p.status == ProviderStatus::Available)
            .unwrap_or(false)
    }
}

// ============================================================================
// Configuration Screen State
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConfigCategory {
    Authentication,
    Workspace,
    Docker,
    AgentDefaults,
    Editor,
    Plugins,
    Permissions,
    Appearance,
    Analytics,
}

impl ConfigCategory {
    pub fn all() -> Vec<ConfigCategory> {
        vec![
            ConfigCategory::Authentication,
            ConfigCategory::Workspace,
            ConfigCategory::Docker,
            ConfigCategory::AgentDefaults,
            ConfigCategory::Editor,
            ConfigCategory::Plugins,
            ConfigCategory::Permissions,
            ConfigCategory::Appearance,
            ConfigCategory::Analytics,
        ]
    }

    pub fn label(&self) -> &'static str {
        match self {
            ConfigCategory::Authentication => "Authentication",
            ConfigCategory::Workspace => "Workspace",
            ConfigCategory::Docker => "Docker",
            ConfigCategory::AgentDefaults => "Agent Defaults",
            ConfigCategory::Editor => "Editor",
            ConfigCategory::Plugins => "Plugins",
            ConfigCategory::Permissions => "Permissions",
            ConfigCategory::Appearance => "Appearance",
            ConfigCategory::Analytics => "Analytics",
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            ConfigCategory::Authentication => "🔐",
            ConfigCategory::Workspace => "📁",
            ConfigCategory::Docker => "🐳",
            ConfigCategory::AgentDefaults => "🤖",
            ConfigCategory::Editor => "📝",
            ConfigCategory::Plugins => "🔌",
            ConfigCategory::Permissions => "🛡️",
            ConfigCategory::Appearance => "🎨",
            ConfigCategory::Analytics => "📊",
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            ConfigCategory::Authentication => "API keys, OAuth, GitHub credentials",
            ConfigCategory::Workspace => "Default paths, git settings, branch prefix",
            ConfigCategory::Docker => "Container host, timeouts",
            ConfigCategory::AgentDefaults => "Model, temperature, max tokens",
            ConfigCategory::Editor => "Preferred code editor for sessions",
            ConfigCategory::Plugins => "Installed plugins, enable/disable",
            ConfigCategory::Permissions => "File write, shell, git approval",
            ConfigCategory::Appearance => "Theme, colors, status indicators",
            ConfigCategory::Analytics => "Usage tracking, cost alerts",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConfigSetting {
    pub key: String,
    pub label: String,
    pub value: ConfigValue,
    pub description: String,
}

#[derive(Debug, Clone)]
pub enum ConfigValue {
    Text(String),
    Secret(String), // Masked display
    Bool(bool),
    Choice(Vec<String>, usize), // Options and selected index
    Number(i64),
}

impl ConfigValue {
    pub fn display(&self) -> String {
        match self {
            ConfigValue::Text(s) => s.clone(),
            ConfigValue::Secret(s) => {
                if s.is_empty() {
                    "Not configured".to_string()
                } else {
                    format!("{}••••••••", &s[..std::cmp::min(8, s.len())])
                }
            }
            ConfigValue::Bool(b) => if *b { "✓ Enabled" } else { "✗ Disabled" }.to_string(),
            ConfigValue::Choice(options, idx) => options.get(*idx).cloned().unwrap_or_default(),
            ConfigValue::Number(n) => n.to_string(),
        }
    }
}

/// Tracks which pane has focus in the config screen
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConfigPane {
    #[default]
    Categories,
    Settings,
}

// Editor detection and mapping now uses the centralized crate::editors module

#[derive(Debug, Clone)]
pub struct ConfigScreenState {
    pub selected_category: usize,
    pub selected_setting: usize,
    pub categories: Vec<ConfigCategory>,
    pub settings: std::collections::HashMap<ConfigCategory, Vec<ConfigSetting>>,
    pub editing: bool,
    pub edit_buffer: String,
    /// True when entering API key (special handling - saves to keychain)
    pub api_key_input_mode: bool,
    /// Which pane currently has focus (Categories or Settings)
    pub focused_pane: ConfigPane,
}

impl Default for ConfigScreenState {
    fn default() -> Self {
        let mut settings = std::collections::HashMap::new();

        // Authentication settings
        // Determine current auth status for display
        let auth_status = match credentials::get_anthropic_api_key() {
            Ok(Some(key)) => {
                let masked = if key.len() > 12 {
                    format!("{}••••••••", &key[..12])
                } else {
                    "••••••••".to_string()
                };
                format!("API Key ({})", masked)
            }
            _ => "System Auth (Pro/Max Plan)".to_string(),
        };

        settings.insert(
            ConfigCategory::Authentication,
            vec![
                ConfigSetting {
                    key: "claude_auth".to_string(),
                    label: "Claude Authentication".to_string(),
                    value: ConfigValue::Text(auth_status),
                    description: "Press Enter to configure authentication provider".to_string(),
                },
                ConfigSetting {
                    key: "github_auth".to_string(),
                    label: "GitHub Credentials".to_string(),
                    value: ConfigValue::Text("System Default".to_string()),
                    description: "Uses git credential helper. PAT support coming soon.".to_string(),
                },
            ],
        );

        // Workspace settings
        settings.insert(
            ConfigCategory::Workspace,
            vec![
                ConfigSetting {
                    key: "default_workspace".to_string(),
                    label: "Default Workspace".to_string(),
                    value: ConfigValue::Text("~/projects".to_string()),
                    description: "Default directory for new sessions".to_string(),
                },
                ConfigSetting {
                    key: "branch_prefix".to_string(),
                    label: "Branch Prefix".to_string(),
                    value: ConfigValue::Text("agents/".to_string()),
                    description: "Prefix for auto-created branch names".to_string(),
                },
                ConfigSetting {
                    key: "exclude_paths".to_string(),
                    label: "Exclude Paths".to_string(),
                    value: ConfigValue::Text("node_modules, .git, target".to_string()),
                    description: "Patterns to exclude from repo scanning (comma-separated)"
                        .to_string(),
                },
                ConfigSetting {
                    key: "max_repositories".to_string(),
                    label: "Max Repositories".to_string(),
                    value: ConfigValue::Number(500),
                    description: "Maximum repositories to show in search results".to_string(),
                },
            ],
        );

        // Docker settings
        settings.insert(
            ConfigCategory::Docker,
            vec![
                ConfigSetting {
                    key: "docker_host".to_string(),
                    label: "Docker Host".to_string(),
                    value: ConfigValue::Text("Auto-detect".to_string()),
                    description: "Docker daemon connection (auto-detect, unix socket, or TCP)"
                        .to_string(),
                },
                ConfigSetting {
                    key: "docker_timeout".to_string(),
                    label: "Connection Timeout".to_string(),
                    value: ConfigValue::Number(60),
                    description: "Docker connection timeout in seconds".to_string(),
                },
            ],
        );

        // Agent defaults
        settings.insert(
            ConfigCategory::AgentDefaults,
            vec![
                ConfigSetting {
                    key: "default_model".to_string(),
                    label: "Default Model".to_string(),
                    value: ConfigValue::Choice(
                        vec![
                            "Opus 4.5".to_string(),
                            "Sonnet 4.5".to_string(),
                            "Haiku 4.5".to_string(),
                        ],
                        1, // Sonnet default
                    ),
                    description: "Default Claude model for new sessions".to_string(),
                },
                ConfigSetting {
                    key: "auto_approve".to_string(),
                    label: "Auto-Approve Actions".to_string(),
                    value: ConfigValue::Bool(false),
                    description: "Automatically approve file writes and commands".to_string(),
                },
            ],
        );

        // Permissions
        settings.insert(
            ConfigCategory::Permissions,
            vec![
                ConfigSetting {
                    key: "allow_file_write".to_string(),
                    label: "Allow File Write".to_string(),
                    value: ConfigValue::Bool(true),
                    description: "Allow agents to write files".to_string(),
                },
                ConfigSetting {
                    key: "allow_shell".to_string(),
                    label: "Allow Shell Commands".to_string(),
                    value: ConfigValue::Bool(true),
                    description: "Allow agents to run shell commands".to_string(),
                },
                ConfigSetting {
                    key: "allow_git".to_string(),
                    label: "Allow Git Operations".to_string(),
                    value: ConfigValue::Bool(true),
                    description: "Allow agents to perform git operations".to_string(),
                },
            ],
        );

        // Editor
        // Detect available editors for the editor preference setting
        let available_editors = editors::get_editor_options();
        let editor_names: Vec<String> =
            available_editors.iter().map(|(name, _)| name.clone()).collect();
        let default_editor_index =
            available_editors.iter().position(|(_, avail)| *avail).unwrap_or(0);

        settings.insert(
            ConfigCategory::Editor,
            vec![ConfigSetting {
                key: "preferred_editor".to_string(),
                label: "Preferred Editor".to_string(),
                value: ConfigValue::Choice(editor_names, default_editor_index),
                description: "Editor for opening sessions (o key)".to_string(),
            }],
        );

        // Appearance
        settings.insert(
            ConfigCategory::Appearance,
            vec![
                ConfigSetting {
                    key: "theme".to_string(),
                    label: "Theme".to_string(),
                    value: ConfigValue::Choice(
                        vec![
                            "Dark".to_string(),
                            "Light".to_string(),
                            "System".to_string(),
                        ],
                        0,
                    ),
                    description: "Color theme for the TUI".to_string(),
                },
                ConfigSetting {
                    key: "show_container_status".to_string(),
                    label: "Show Container Status".to_string(),
                    value: ConfigValue::Bool(true),
                    description: "Show container mode icons in session list".to_string(),
                },
                ConfigSetting {
                    key: "show_git_status".to_string(),
                    label: "Show Git Status".to_string(),
                    value: ConfigValue::Bool(true),
                    description: "Show git changes in session list".to_string(),
                },
            ],
        );

        // Plugins (empty for now)
        settings.insert(
            ConfigCategory::Plugins,
            vec![ConfigSetting {
                key: "installed_plugins".to_string(),
                label: "Installed Plugins".to_string(),
                value: ConfigValue::Text("None installed".to_string()),
                description: "Manage installed plugins from the Catalog".to_string(),
            }],
        );

        // Analytics
        settings.insert(
            ConfigCategory::Analytics,
            vec![
                ConfigSetting {
                    key: "track_usage".to_string(),
                    label: "Track Usage".to_string(),
                    value: ConfigValue::Bool(true),
                    description: "Track session duration and token usage".to_string(),
                },
                ConfigSetting {
                    key: "cost_alerts".to_string(),
                    label: "Cost Alerts".to_string(),
                    value: ConfigValue::Bool(false),
                    description: "Alert when spending exceeds threshold".to_string(),
                },
            ],
        );

        Self {
            selected_category: 0,
            selected_setting: 0,
            categories: ConfigCategory::all(),
            settings,
            editing: false,
            edit_buffer: String::new(),
            api_key_input_mode: false,
            focused_pane: ConfigPane::Categories,
        }
    }
}

impl ConfigScreenState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn current_category(&self) -> Option<&ConfigCategory> {
        self.categories.get(self.selected_category)
    }

    pub fn current_settings(&self) -> Vec<&ConfigSetting> {
        self.current_category()
            .and_then(|cat| self.settings.get(cat))
            .map(|s| s.iter().collect())
            .unwrap_or_default()
    }

    pub fn current_setting(&self) -> Option<&ConfigSetting> {
        self.current_settings().get(self.selected_setting).copied()
    }

    pub fn select_next_category(&mut self) {
        if !self.categories.is_empty() {
            self.selected_category = (self.selected_category + 1) % self.categories.len();
            self.selected_setting = 0;
        }
    }

    pub fn select_prev_category(&mut self) {
        if !self.categories.is_empty() {
            self.selected_category = if self.selected_category == 0 {
                self.categories.len() - 1
            } else {
                self.selected_category - 1
            };
            self.selected_setting = 0;
        }
    }

    pub fn select_next_setting(&mut self) {
        let settings_count = self.current_settings().len();
        if settings_count > 0 {
            self.selected_setting = (self.selected_setting + 1) % settings_count;
        }
    }

    pub fn select_prev_setting(&mut self) {
        let settings_count = self.current_settings().len();
        if settings_count > 0 {
            self.selected_setting = if self.selected_setting == 0 {
                settings_count - 1
            } else {
                self.selected_setting - 1
            };
        }
    }

    pub fn toggle_current_setting(&mut self) {
        if let Some(category) = self.current_category().cloned() {
            if let Some(settings) = self.settings.get_mut(&category) {
                if let Some(setting) = settings.get_mut(self.selected_setting) {
                    match &mut setting.value {
                        ConfigValue::Bool(ref mut b) => *b = !*b,
                        ConfigValue::Choice(options, ref mut idx) => {
                            *idx = (*idx + 1) % options.len();
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    /// Create ConfigScreenState from AppConfig (loads persisted settings)
    pub fn from_app_config(config: &AppConfig) -> Self {
        let mut state = Self::default();

        // Update Authentication settings from config
        if let Some(settings) = state.settings.get_mut(&ConfigCategory::Authentication) {
            for setting in settings.iter_mut() {
                if setting.key == "claude_auth" {
                    // Build status text based on provider and API key presence
                    use crate::config::ClaudeAuthProvider;
                    let status = match &config.authentication.claude_provider {
                        ClaudeAuthProvider::ApiKey => {
                            let masked = credentials::get_anthropic_api_key_masked();
                            if masked == "Not configured" {
                                "API Key (Not configured)".to_string()
                            } else {
                                format!("API Key ({})", masked)
                            }
                        }
                        ClaudeAuthProvider::SystemAuth => "System Auth (Pro/Max Plan)".to_string(),
                        ClaudeAuthProvider::AmazonBedrock => {
                            "Amazon Bedrock [Coming Soon]".to_string()
                        }
                        ClaudeAuthProvider::GoogleVertex => {
                            "Google Vertex [Coming Soon]".to_string()
                        }
                        ClaudeAuthProvider::AzureFoundry => {
                            "Azure Foundry [Coming Soon]".to_string()
                        }
                        ClaudeAuthProvider::GlmZai => "GLM on ZAI [Coming Soon]".to_string(),
                        ClaudeAuthProvider::LlmGateway => "LLM Gateway [Coming Soon]".to_string(),
                    };
                    setting.value = ConfigValue::Text(status);
                }
            }
        }

        // Update Workspace settings from config
        if let Some(settings) = state.settings.get_mut(&ConfigCategory::Workspace) {
            for setting in settings.iter_mut() {
                match setting.key.as_str() {
                    "default_workspace" => {
                        // Use first scan path or default
                        let path = config
                            .workspace_defaults
                            .workspace_scan_paths
                            .first()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| "~/projects".to_string());
                        setting.value = ConfigValue::Text(path);
                    }
                    "branch_prefix" => {
                        setting.value =
                            ConfigValue::Text(config.workspace_defaults.branch_prefix.clone());
                    }
                    "exclude_paths" => {
                        let paths = config.workspace_defaults.exclude_paths.join(", ");
                        setting.value = ConfigValue::Text(if paths.is_empty() {
                            "node_modules, .git, target".to_string()
                        } else {
                            paths
                        });
                    }
                    "max_repositories" => {
                        setting.value =
                            ConfigValue::Number(config.workspace_defaults.max_repositories as i64);
                    }
                    _ => {}
                }
            }
        }

        // Update Docker settings from config
        if let Some(settings) = state.settings.get_mut(&ConfigCategory::Docker) {
            for setting in settings.iter_mut() {
                match setting.key.as_str() {
                    "docker_host" => {
                        let host_display =
                            config.docker.host.clone().unwrap_or_else(|| "Auto-detect".to_string());
                        setting.value = ConfigValue::Text(host_display);
                    }
                    "docker_timeout" => {
                        setting.value = ConfigValue::Number(config.docker.timeout as i64);
                    }
                    _ => {}
                }
            }
        }

        // Update Agent Defaults from config
        if let Some(settings) = state.settings.get_mut(&ConfigCategory::AgentDefaults) {
            for setting in settings.iter_mut() {
                match setting.key.as_str() {
                    "auto_approve" => {
                        // Will be added to AppConfig
                        setting.value = ConfigValue::Bool(false);
                    }
                    _ => {}
                }
            }
        }

        // Update Editor from config
        if let Some(settings) = state.settings.get_mut(&ConfigCategory::Editor) {
            for setting in settings.iter_mut() {
                if setting.key == "preferred_editor" {
                    // Load current preferred editor from config
                    if let Some(ref preferred) = config.ui_preferences.preferred_editor {
                        // Find the index of the preferred editor in our list
                        if let ConfigValue::Choice(ref options, ref mut idx) = setting.value {
                            // Map command to display name
                            let display_name = match preferred.as_str() {
                                "code" => "VS Code",
                                "cursor" => "Cursor",
                                "zed" => "Zed",
                                "nvim" => "Neovim",
                                "vim" => "Vim",
                                "emacs" => "Emacs",
                                "subl" => "Sublime Text",
                                _ => preferred.as_str(),
                            };
                            if let Some(pos) = options.iter().position(|n| n == display_name) {
                                *idx = pos;
                            }
                        }
                    }
                }
            }
        }

        // Update Appearance from config
        if let Some(settings) = state.settings.get_mut(&ConfigCategory::Appearance) {
            for setting in settings.iter_mut() {
                match setting.key.as_str() {
                    "theme" => {
                        let theme_idx = match config.ui_preferences.theme.as_str() {
                            "dark" => 0,
                            "light" => 1,
                            "system" => 2,
                            _ => 0,
                        };
                        setting.value = ConfigValue::Choice(
                            vec![
                                "Dark".to_string(),
                                "Light".to_string(),
                                "System".to_string(),
                            ],
                            theme_idx,
                        );
                    }
                    "show_container_status" => {
                        setting.value =
                            ConfigValue::Bool(config.ui_preferences.show_container_status);
                    }
                    "show_git_status" => {
                        setting.value = ConfigValue::Bool(config.ui_preferences.show_git_status);
                    }
                    _ => {}
                }
            }
        }

        // Update Analytics from config
        if let Some(settings) = state.settings.get_mut(&ConfigCategory::Analytics) {
            for setting in settings.iter_mut() {
                match setting.key.as_str() {
                    "track_usage" => {
                        setting.value = ConfigValue::Bool(true); // Default, not in AppConfig yet
                    }
                    "cost_alerts" => {
                        setting.value = ConfigValue::Bool(false); // Default, not in AppConfig yet
                    }
                    _ => {}
                }
            }
        }

        state
    }

    /// Convert ConfigScreenState back to AppConfig for saving
    pub fn apply_to_app_config(&self, config: &mut AppConfig) {
        // Apply Workspace settings
        if let Some(settings) = self.settings.get(&ConfigCategory::Workspace) {
            for setting in settings {
                match setting.key.as_str() {
                    "default_workspace" => {
                        if let ConfigValue::Text(path) = &setting.value {
                            let expanded = if path.starts_with("~/") {
                                dirs::home_dir()
                                    .map(|h| h.join(&path[2..]))
                                    .unwrap_or_else(|| std::path::PathBuf::from(path))
                            } else {
                                std::path::PathBuf::from(path)
                            };
                            // Add to scan paths if not already present
                            if !config.workspace_defaults.workspace_scan_paths.contains(&expanded) {
                                config.workspace_defaults.workspace_scan_paths.push(expanded);
                            }
                        }
                    }
                    "branch_prefix" => {
                        if let ConfigValue::Text(prefix) = &setting.value {
                            config.workspace_defaults.branch_prefix = prefix.clone();
                        }
                    }
                    "exclude_paths" => {
                        if let ConfigValue::Text(paths) = &setting.value {
                            config.workspace_defaults.exclude_paths = paths
                                .split(',')
                                .map(|s| s.trim().to_string())
                                .filter(|s| !s.is_empty())
                                .collect();
                        }
                    }
                    "max_repositories" => {
                        if let ConfigValue::Number(max) = &setting.value {
                            config.workspace_defaults.max_repositories = *max as usize;
                        }
                    }
                    _ => {}
                }
            }
        }

        // Apply Docker settings
        if let Some(settings) = self.settings.get(&ConfigCategory::Docker) {
            for setting in settings {
                match setting.key.as_str() {
                    "docker_host" => {
                        if let ConfigValue::Text(host) = &setting.value {
                            if host == "Auto-detect" || host.is_empty() {
                                config.docker.host = None;
                            } else {
                                config.docker.host = Some(host.clone());
                            }
                        }
                    }
                    "docker_timeout" => {
                        if let ConfigValue::Number(timeout) = &setting.value {
                            config.docker.timeout = *timeout as u64;
                        }
                    }
                    _ => {}
                }
            }
        }

        // Apply Editor settings
        if let Some(settings) = self.settings.get(&ConfigCategory::Editor) {
            for setting in settings {
                if setting.key == "preferred_editor" {
                    if let ConfigValue::Choice(options, idx) = &setting.value {
                        if let Some(editor_name) = options.get(*idx) {
                            // Convert display name to command
                            if let Some(cmd) = editors::editor_name_to_command(editor_name) {
                                config.ui_preferences.preferred_editor = Some(cmd.to_string());
                            }
                        }
                    }
                }
            }
        }

        // Apply Appearance settings
        if let Some(settings) = self.settings.get(&ConfigCategory::Appearance) {
            for setting in settings {
                match setting.key.as_str() {
                    "theme" => {
                        if let ConfigValue::Choice(options, idx) = &setting.value {
                            if let Some(theme) = options.get(*idx) {
                                config.ui_preferences.theme = theme.to_lowercase();
                            }
                        }
                    }
                    "show_container_status" => {
                        if let ConfigValue::Bool(show) = &setting.value {
                            config.ui_preferences.show_container_status = *show;
                        }
                    }
                    "show_git_status" => {
                        if let ConfigValue::Bool(show) = &setting.value {
                            config.ui_preferences.show_git_status = *show;
                        }
                    }
                    _ => {}
                }
            }
        }

        // Apply Permissions settings
        if let Some(settings) = self.settings.get(&ConfigCategory::Permissions) {
            for setting in settings {
                match setting.key.as_str() {
                    "allow_file_write" | "allow_shell" | "allow_git" => {
                        // These would be added to AppConfig in future
                    }
                    _ => {}
                }
            }
        }
    }
}

// Auth provider option for the popup
#[derive(Debug, Clone)]
pub struct AuthProviderOption {
    pub id: String,
    pub name: String,
    pub icon: String,
    pub description: String,
    pub available: bool,
    pub is_current: bool,
}

impl AuthProviderOption {
    pub fn new(id: &str, name: &str, icon: &str, desc: &str, available: bool) -> Self {
        Self {
            id: id.to_string(),
            name: name.to_string(),
            icon: icon.to_string(),
            description: desc.to_string(),
            available,
            is_current: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AuthProviderPopupState {
    pub providers: Vec<AuthProviderOption>,
    pub selected_index: usize,
    pub is_entering_key: bool,
    pub api_key_input: String,
    pub show_popup: bool,
}

impl Default for AuthProviderPopupState {
    fn default() -> Self {
        // Check current API key status to mark current provider
        let has_api_key =
            credentials::get_anthropic_api_key().map(|opt| opt.is_some()).unwrap_or(false);

        let mut providers = vec![
            AuthProviderOption::new(
                "system",
                "System Auth (Pro/Max Plan)",
                "",
                "Uses 'claude auth' - for Anthropic Pro/Max subscribers",
                true,
            ),
            AuthProviderOption::new(
                "api_key",
                "API Key (Pay-as-you-go)",
                "",
                "Set ANTHROPIC_API_KEY environment variable for pay-per-use",
                true,
            ),
            AuthProviderOption::new(
                "bedrock",
                "Amazon Bedrock",
                "",
                "Use Claude via AWS Bedrock service",
                false, // Coming soon
            ),
            AuthProviderOption::new(
                "vertex",
                "Google Vertex AI",
                "",
                "Use Claude via Google Cloud Vertex AI",
                false, // Coming soon
            ),
            AuthProviderOption::new(
                "azure",
                "Microsoft Azure Foundry",
                "",
                "Use Claude via Azure AI services",
                false, // Coming soon
            ),
            AuthProviderOption::new(
                "glm",
                "GLM on ZAI",
                "",
                "Use GLM models via ZAI platform",
                false, // Coming soon
            ),
            AuthProviderOption::new(
                "gateway",
                "LLM Gateway",
                "",
                "Use custom LLM gateway endpoint",
                false, // Coming soon
            ),
        ];

        // Mark current provider
        if has_api_key {
            if let Some(p) = providers.iter_mut().find(|p| p.id == "api_key") {
                p.is_current = true;
            }
        } else {
            if let Some(p) = providers.iter_mut().find(|p| p.id == "system") {
                p.is_current = true;
            }
        }

        Self {
            providers,
            selected_index: 0,
            is_entering_key: false,
            api_key_input: String::new(),
            show_popup: false,
        }
    }
}

impl AuthProviderPopupState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn select_next(&mut self) {
        if !self.providers.is_empty() {
            self.selected_index = (self.selected_index + 1) % self.providers.len();
        }
    }

    pub fn select_prev(&mut self) {
        if !self.providers.is_empty() {
            self.selected_index = if self.selected_index == 0 {
                self.providers.len() - 1
            } else {
                self.selected_index - 1
            };
        }
    }

    pub fn current_provider(&self) -> Option<&AuthProviderOption> {
        self.providers.get(self.selected_index)
    }

    pub fn is_api_key_selected(&self) -> bool {
        self.current_provider().map(|p| p.id == "api_key").unwrap_or(false)
    }

    pub fn start_key_input(&mut self) {
        self.is_entering_key = true;
        self.api_key_input.clear();
    }

    pub fn cancel_key_input(&mut self) {
        self.is_entering_key = false;
        self.api_key_input.clear();
    }

    /// Create AuthProviderPopupState with current provider marked based on config
    pub fn from_app_config(config: &crate::config::AppConfig) -> Self {
        use crate::config::ClaudeAuthProvider;

        let mut state = Self::default();

        // Clear any auto-detected current flags
        for provider in &mut state.providers {
            provider.is_current = false;
        }

        // Mark the provider from config as current
        let provider_id = match &config.authentication.claude_provider {
            ClaudeAuthProvider::SystemAuth => "system",
            ClaudeAuthProvider::ApiKey => "api_key",
            ClaudeAuthProvider::AmazonBedrock => "amazon_bedrock",
            ClaudeAuthProvider::GoogleVertex => "google_vertex",
            ClaudeAuthProvider::AzureFoundry => "azure_foundry",
            ClaudeAuthProvider::GlmZai => "glm_zai",
            ClaudeAuthProvider::LlmGateway => "llm_gateway",
        };

        if let Some(p) = state.providers.iter_mut().find(|p| p.id == provider_id) {
            p.is_current = true;
        }

        state
    }

    /// Get the current provider ID (the one marked as is_current)
    pub fn get_current_provider_id(&self) -> Option<&str> {
        self.providers.iter().find(|p| p.is_current).map(|p| p.id.as_str())
    }

    pub fn refresh_providers(&mut self) {
        let has_api_key =
            credentials::get_anthropic_api_key().map(|opt| opt.is_some()).unwrap_or(false);

        for p in &mut self.providers {
            p.is_current = false;
        }

        if has_api_key {
            if let Some(p) = self.providers.iter_mut().find(|p| p.id == "api_key") {
                p.is_current = true;
            }
        } else {
            if let Some(p) = self.providers.iter_mut().find(|p| p.id == "system") {
                p.is_current = true;
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthMethod {
    OAuth,
    ApiKey,
    Skip,
}

#[derive(Debug, Clone)]
pub struct AuthSetupState {
    pub selected_method: AuthMethod,
    pub api_key_input: String,
    pub is_processing: bool,
    pub error_message: Option<String>,
    pub show_cursor: bool,
}

#[derive(Debug, Clone)]
pub struct ClaudeChatState {
    pub messages: Vec<ClaudeMessage>,
    pub input_buffer: String,
    pub is_streaming: bool,
    pub current_streaming_response: Option<String>,
    pub associated_session_id: Option<Uuid>,
    pub total_tokens_used: u32,
    pub last_activity: chrono::DateTime<chrono::Utc>,
}

impl ClaudeChatState {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
            input_buffer: String::new(),
            is_streaming: false,
            current_streaming_response: None,
            associated_session_id: None,
            total_tokens_used: 0,
            last_activity: chrono::Utc::now(),
        }
    }

    pub fn add_message(&mut self, message: ClaudeMessage) {
        self.messages.push(message);
        self.last_activity = chrono::Utc::now();
    }

    pub fn start_streaming(&mut self, user_message: String) {
        self.add_message(ClaudeMessage::user(user_message));
        self.is_streaming = true;
        self.current_streaming_response = Some(String::new());
        self.input_buffer.clear();
        self.last_activity = chrono::Utc::now();
    }

    pub fn append_streaming_response(&mut self, text: &str) {
        if let Some(ref mut response) = self.current_streaming_response {
            response.push_str(text);
        }
        self.last_activity = chrono::Utc::now();
    }

    pub fn finish_streaming(&mut self) {
        if let Some(response) = self.current_streaming_response.take() {
            self.add_message(ClaudeMessage::assistant(response));
        }
        self.is_streaming = false;
    }

    pub fn clear_input(&mut self) {
        self.input_buffer.clear();
    }

    pub fn add_char_to_input(&mut self, ch: char) {
        if !self.is_streaming {
            self.input_buffer.push(ch);
        }
    }

    pub fn backspace_input(&mut self) {
        if !self.is_streaming {
            self.input_buffer.pop();
        }
    }
}

/// View filter for the session tree, cycled by `Shift+F` on the sessions screen.
///
/// Phase 2 of `load_interactive_mode_sessions` started surfacing Stopped sessions
/// (tmux-dead but worktree-alive) alongside Running ones. With many worktrees
/// the tree gets crowded; this filter lets the user hide stopped rows or focus
/// on stopped-only without losing access. In-memory only (resets each launch).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SessionFilter {
    #[default]
    All,
    ActiveOnly,
    StoppedOnly,
}

impl SessionFilter {
    /// Cycle order: All → ActiveOnly → StoppedOnly → All.
    pub fn next(self) -> Self {
        match self {
            Self::All => Self::ActiveOnly,
            Self::ActiveOnly => Self::StoppedOnly,
            Self::StoppedOnly => Self::All,
        }
    }

    /// Short label rendered in the workspace panel title (`[active]` etc.).
    /// Returns None for `All` so the default view stays unmarked.
    pub fn title_label(self) -> Option<&'static str> {
        match self {
            Self::All => None,
            Self::ActiveOnly => Some("active"),
            Self::StoppedOnly => Some("stopped"),
        }
    }
}

#[derive(Debug)]
pub struct AppState {
    pub workspaces: Vec<Workspace>,
    pub selected_workspace_index: Option<usize>,
    pub selected_session_index: Option<usize>,
    pub shell_selected: bool, // Whether the workspace shell is currently selected
    pub selected_sessions: HashSet<Uuid>, // Multi-selected session IDs for bulk operations
    pub expand_all_workspaces: bool, // When true, show all sessions across all workspaces
    pub session_filter: SessionFilter, // View filter for Interactive sessions (Shift+F to cycle)
    pub current_screen: ScreenId,
    pub should_quit: bool,
    pub logs: HashMap<Uuid, Vec<String>>,
    pub help_visible: bool,
    // New session creation state
    pub new_session_state: Option<NewSessionState>,
    // Async action processing
    pub pending_async_action: Option<AsyncAction>,
    // Flag to track if user cancelled during async operation
    pub async_operation_cancelled: bool,
    // Confirmation dialog state
    pub confirmation_dialog: Option<ConfirmationDialog>,
    // Flag to force UI refresh after workspace changes
    pub ui_needs_refresh: bool,

    // Claude chat visibility toggle
    pub claude_chat_visible: bool,

    // Focus management for panes
    pub focused_pane: FocusedPane,
    // Mouse/layout state for the Sessions split pane.
    pub sessions_pane_state: SessionsPaneState,
    // Track if current directory is a git repository
    pub is_current_dir_git_repo: bool,
    // Track which session logs were last fetched to avoid unnecessary refetches
    pub last_logs_session_id: Option<Uuid>,
    // Track attached terminal state
    pub attached_session_id: Option<Uuid>,
    // Auth setup state
    pub auth_setup_state: Option<AuthSetupState>,
    // Track when logs were last updated for each session
    pub log_last_updated: HashMap<Uuid, std::time::Instant>,
    // Track the last time we checked for log updates globally
    pub last_log_check: Option<std::time::Instant>,
    // Track the last time we checked for OAuth token refresh
    pub last_token_refresh_check: Option<std::time::Instant>,
    // Claude chat integration
    pub claude_chat_state: Option<ClaudeChatState>,
    // Live logs from Docker containers
    pub live_logs: HashMap<Uuid, Vec<LogEntry>>,
    // Claude API client manager (when initialized)
    pub claude_manager: Option<ClaudeChatManager>,
    // Docker log streaming coordinator
    pub log_streaming_coordinator: Option<LogStreamingCoordinator>,
    // Channel sender for log streaming
    pub log_sender: Option<mpsc::UnboundedSender<(Uuid, LogEntry)>>,
    // Git view state
    pub git_view_state: Option<crate::components::GitViewState>,
    // Previous view for navigation (e.g., to return from GitView)
    pub previous_screen: Option<ScreenId>,
    // Notification system
    pub notifications: Vec<Notification>,
    // Pending event to be processed in next loop iteration
    pub pending_event: Option<crate::app::events::AppEvent>,

    // Quick commit dialog state
    pub quick_commit_message: Option<String>, // None = not in quick commit mode, Some = message being entered
    pub quick_commit_cursor: usize,           // Cursor position in quick commit message

    // Tmux integration
    pub tmux_sessions: HashMap<Uuid, crate::tmux::TmuxSession>,
    pub preview_update_task: Option<tokio::task::JoinHandle<()>>,

    // Other tmux sessions (not managed by agents-in-a-box)
    pub other_tmux_sessions: Vec<crate::models::OtherTmuxSession>,
    pub other_tmux_expanded: bool,
    pub selected_other_tmux_index: Option<usize>,
    pub selected_other_tmux_sessions: HashSet<String>, // Multi-selected external tmux names
    /// Whether we're in rename mode for the selected "Other tmux" session
    pub other_tmux_rename_mode: bool,
    /// Buffer for the new name being typed during rename
    pub other_tmux_rename_buffer: String,

    // SSH Sessions (Claude-managed sessions with agent_type=Ssh)
    /// SSH sessions displayed in their own section
    pub ssh_sessions: Vec<crate::models::Session>,
    /// Whether the SSH sessions section is expanded
    pub ssh_sessions_expanded: bool,
    /// Currently selected SSH session index (within ssh_sessions vec)
    pub selected_ssh_session_index: Option<usize>,
    /// Whether we're in rename mode for the selected SSH session
    pub ssh_session_rename_mode: bool,
    /// Buffer for the new display name being typed during rename
    pub ssh_session_rename_buffer: String,
    /// Persistent store for SSH session display names
    pub ssh_display_name_store: SshDisplayNameStore,

    // AINB 2.0: Home screen and agent selection
    pub home_screen_state: HomeScreenState,
    pub home_screen_v2_state: HomeScreenV2State,
    pub agent_selection_state: AgentSelectionState,
    pub config_screen_state: ConfigScreenState,
    pub auth_provider_popup_state: AuthProviderPopupState,
    /// Config popup state for choice/text input popups in config screen
    pub config_popup_state: crate::components::config_popup::ConfigPopupState,

    // Onboarding wizard state
    pub onboarding_state: Option<crate::components::onboarding::OnboardingState>,

    // Setup menu state
    pub setup_menu_state: crate::components::setup_menu::SetupMenuState,

    // Persistent configuration (saved to ~/.agents-in-a-box/config/config.toml)
    pub app_config: AppConfig,

    // Log history viewer state
    pub log_history_state: crate::components::LogHistoryViewerState,

    // Changelog viewer state
    pub changelog_state: crate::components::ChangelogState,

    // Session recovery state (for orphaned agent sessions)
    pub session_recovery_state: crate::components::SessionRecoveryState,

    /// WireBuffers freshly drained from plugins, keyed by screen id.
    /// `App::tick_plugin_renders` populates this before each frame so
    /// `PluginScreen::render` can paint without needing access to the
    /// plugin runtime (which lives on `App`, not `AppState`).
    pub pending_plugin_renders: std::collections::HashMap<
        crate::app::screens::ScreenId,
        ainb_plugin_runtime::WireBuffer,
    >,

    /// Last `(width, height)` `PluginScreen::render` was handed for each
    /// screen id. `tick_plugin_renders` reads this and forwards it to
    /// `handle.render(..)` so the plugin paints at the actual allocated
    /// size instead of falling back to its hard-coded default. One-frame
    /// stale is fine — the first frame still uses the plugin's fallback,
    /// every subsequent frame matches the host's layout.
    pub plugin_render_areas:
        std::collections::HashMap<crate::app::screens::ScreenId, (u16, u16)>,

    /// Cheap Send + Clone façade onto the plugin runtime, populated by
    /// `App::init`. `None` when running plugin-free (e.g. tests, or
    /// installs that haven't completed bundled-plugin discovery yet).
    ///
    /// Lives on `AppState` rather than `App` so the key-dispatch path
    /// in `app::events::handle_key_event` can forward keystrokes to
    /// the focused plugin without needing access to `App`. `App` still
    /// owns the underlying `Runtime` via `plugin_runtime_owner` so the
    /// tokio executor is torn down when `App` drops.
    pub plugin_runtime: Option<ainb_plugin_runtime::RuntimeHandle>,

    /// Cached result of `detect_statusline_status()` paired with the time
    /// it was read. Refreshed lazily through
    /// [`AppState::statusline_status_cached`] on a 15s TTL so the global
    /// `W` shortcut and host-side statusline CTAs don't re-read
    /// `~/.claude/settings.json` on every render or keystroke.
    ///
    /// Invalidated explicitly after the install event fires so the CTA
    /// flips state on the very next frame instead of waiting out the TTL.
    pub statusline_status_cache: Option<(
        Option<crate::cli::statusline_install::StatuslineStatus>,
        Instant,
    )>,

    /// Background poller for the live OAuth-window snapshot. The render
    /// path reads via `snapshot()` (cheap RwLock read + clone) instead of
    /// calling `live_window::current()` directly — Tier 2's JSONL walk
    /// would otherwise stall input handling on every frame.
    pub live_window_watcher: crate::models::live_window_watcher::LiveWindowWatcher,

    // Usage analytics state: removed. Burndown plugin owns usage state
    // (provider, period, filters, zoom). Host no longer reads or writes
    // `usage_state` / `usage_load_receiver`. Statusline-related state
    // (live_window_watcher, statusline_status_cache) stays in core
    // because that's a host CLI install concern, not a plugin one.

    // Skills browser state
    pub skills_state: crate::components::skills::SkillsViewState,
    /// Channel receiver for background skills+agents scan.
    /// Present only while a scan is in flight; `tick()` drains it.
    pub skills_load_receiver: Option<mpsc::UnboundedReceiver<crate::models::SkillsData>>,

    // Periodic session snapshot tracking
    pub last_snapshot_time: Option<Instant>,

    // Throttled tmux preview updates (avoid spawning subprocesses every 250ms tick)
    pub last_preview_update: Option<Instant>,

    // Background workspace loading state
    pub is_loading_workspaces: bool,
    pub workspace_load_error: Option<String>,
    pub workspace_load_started: Option<Instant>,
    /// Channel receiver for background workspace loading results
    pub workspace_load_receiver: Option<mpsc::UnboundedReceiver<WorkspaceLoadResult>>,
}

/// Result of background workspace loading
#[derive(Debug)]
pub enum WorkspaceLoadResult {
    /// Successfully loaded workspaces
    Success(Vec<Workspace>),
    /// Loading failed with error
    Error(String),
    /// Loading timed out
    Timeout,
}

/// Load workspaces asynchronously (standalone function for use in spawned tasks)
/// This is called from background task to avoid blocking the main thread
async fn load_workspaces_async() -> anyhow::Result<Vec<Workspace>> {
    info!("load_workspaces_async: Starting");

    // Boss-mode (Docker) and Interactive-mode (tmux) sessions are fetched
    // CONCURRENTLY with independent budgets. A slow or stuck Docker daemon
    // must not starve Interactive loading — interactive sessions are
    // tmux-only and have no Docker dependency. Before this split, a 9s
    // `list_agents_containers` call would burn the outer 10s timeout
    // before Interactive even ran, leaving the workspace tree empty even
    // though Interactive would have returned instantly.
    //
    // The merge step (combining Boss and Interactive sessions into the
    // same Workspace by canonical path) runs after both fetches return,
    // since the workspace_map mutation is non-commutative.
    let (boss_workspaces, interactive_sessions) =
        tokio::join!(fetch_boss_mode_workspaces(), fetch_interactive_sessions());

    // Index from canonicalized (or raw-fallback) workspace path to its
    // position in `workspaces`. Computed once per workspace and once per
    // interactive session — O(N+M) canonicalize syscalls instead of the
    // O(N×M) "canonicalize-in-find-loop" pattern. The raw-fallback also
    // fixes a correctness bug: two paths that both fail to canonicalize
    // (e.g., deleted worktrees) previously matched each other via shared
    // `None`, collapsing distinct sessions into one Workspace.
    let canonical_key =
        |p: &std::path::Path| -> PathBuf { p.canonicalize().unwrap_or_else(|_| p.to_path_buf()) };
    let mut workspaces = boss_workspaces;
    let mut path_index: HashMap<PathBuf, usize> = workspaces
        .iter()
        .enumerate()
        .map(|(i, w)| (canonical_key(&w.path), i))
        .collect();

    for interactive_session in interactive_sessions {
        let session = interactive_session.to_session_model();
        let workspace_path = interactive_session.source_repository.clone();
        let workspace_name = interactive_session.workspace_name.clone();
        let key = canonical_key(&workspace_path);

        if let Some(&idx) = path_index.get(&key) {
            workspaces[idx].add_session(session);
        } else {
            let mut workspace = Workspace::new(workspace_name, workspace_path);
            workspace.add_session(session);
            path_index.insert(key, workspaces.len());
            workspaces.push(workspace);
        }
    }

    info!(
        "load_workspaces_async: Complete with {} workspaces",
        workspaces.len()
    );
    Ok(workspaces)
}

/// Fetch Boss-mode (Docker container) workspaces with a strict per-mode
/// timeout. Returns an empty `Vec` on any failure path — caller proceeds
/// with Interactive results. The `docker info` probe runs on a blocking
/// thread so a wedged Docker socket can't pin a tokio runtime thread.
async fn fetch_boss_mode_workspaces() -> Vec<Workspace> {
    const BOSS_MODE_TIMEOUT: Duration = Duration::from_secs(5);

    let docker_available = tokio::task::spawn_blocking(AppState::is_docker_available_sync)
        .await
        .unwrap_or(false);

    if !docker_available {
        info!("load_workspaces_async: Docker not available, skipping Boss mode");
        return Vec::new();
    }

    info!("load_workspaces_async: Docker available, loading Boss mode sessions");
    let load = async {
        let loader = SessionLoader::new().await?;
        loader.load_active_sessions().await
    };

    match tokio::time::timeout(BOSS_MODE_TIMEOUT, load).await {
        Ok(Ok(workspaces)) => {
            info!(
                "load_workspaces_async: Loaded {} Boss mode workspaces",
                workspaces.len()
            );
            workspaces
        }
        Ok(Err(e)) => {
            warn!(
                "load_workspaces_async: Failed to load Boss mode sessions: {}",
                e
            );
            Vec::new()
        }
        Err(_) => {
            warn!(
                "load_workspaces_async: Boss mode load exceeded {}s budget — proceeding with Interactive only",
                BOSS_MODE_TIMEOUT.as_secs()
            );
            Vec::new()
        }
    }
}

/// Fetch Interactive-mode (tmux) sessions. No Docker dependency — must
/// not be gated on Boss-mode completing.
async fn fetch_interactive_sessions() -> Vec<crate::interactive::InteractiveSession> {
    use crate::interactive::InteractiveSessionManager;

    info!("load_workspaces_async: Loading Interactive mode sessions");
    let mut manager = match InteractiveSessionManager::new() {
        Ok(m) => m,
        Err(e) => {
            warn!(
                "load_workspaces_async: Failed to create Interactive session manager: {}",
                e
            );
            return Vec::new();
        }
    };

    match manager.list_sessions().await {
        Ok(sessions) => {
            info!(
                "load_workspaces_async: Found {} Interactive sessions",
                sessions.len()
            );
            sessions
        }
        Err(e) => {
            warn!(
                "load_workspaces_async: Failed to list Interactive sessions: {}",
                e
            );
            Vec::new()
        }
    }
}

/// Focus state for the combined Agent + Model selection panel
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentModelFocus {
    #[default]
    Agent,
    Model,
}

/// Mode for remote branch checkout - create new branch or checkout existing
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BranchCheckoutMode {
    #[default]
    CreateNew, // Create ainb/{uuid} branch from selected (default)
    CheckoutExisting, // Use the remote branch directly
}

#[derive(Debug)]
pub struct NewSessionState {
    pub source_choice: RepoSourceChoice, // Local or Remote repo source
    pub available_repos: Vec<std::path::PathBuf>,
    pub filtered_repos: Vec<(usize, std::path::PathBuf)>, // (original_index, path)
    pub selected_repo_index: Option<usize>,
    pub current_repo_branch: Option<String>, // Current branch of selected local repo
    pub branch_name: String,
    pub step: NewSessionStep,
    pub filter_text: String,
    pub is_current_dir_mode: bool, // true if creating session in current dir
    pub skip_permissions: bool,    // true to use --dangerously-skip-permissions flag
    pub mode: crate::models::SessionMode, // Interactive or Boss mode
    pub boss_prompt: TextEditor,   // The prompt text editor for boss mode execution
    pub file_finder: FuzzyFileFinderState, // Fuzzy file finder for @ symbol
    pub restart_session_id: Option<Uuid>, // If set, this is a restart operation
    // Agent selection
    pub selected_agent: SessionAgentType, // The selected agent for this session
    pub agent_options: Vec<SessionAgentOption>, // List of available agents
    pub selected_agent_index: usize,      // Index in agent_options list
    // Model selection (for Claude agent)
    pub selected_model: ClaudeModel, // The selected model for this session
    pub model_options: Vec<ClaudeModel>, // List of available models
    pub selected_model_index: usize, // Index in model_options list
    pub agent_model_focus: AgentModelFocus, // Which panel has focus (Agent or Model)

    // Remote repository support
    pub repo_input: String,                 // URL or path input from user
    pub repo_source: Option<RepoSource>,    // Parsed repo source
    pub remote_branches: Vec<RemoteBranch>, // Available branches from remote
    pub filtered_branches: Vec<(usize, RemoteBranch)>, // (original_index, branch) after filter
    pub branch_filter_text: String,         // Filter text for fuzzy branch search
    pub selected_branch_index: usize,       // Selected index in filtered branch list
    pub selected_base_branch: Option<String>, // The base branch to create worktree from
    pub cached_repo_path: Option<PathBuf>,  // Path to cached bare clone
    pub repo_validation_error: Option<String>, // Error message for UI display
    pub is_validating: bool,                // Show loading indicator
    pub recent_repos: Vec<ParsedRepo>,      // Recently used repos for suggestions
    pub branch_checkout_mode: BranchCheckoutMode, // Toggle: create new vs checkout existing

    // SSH configuration (only used when agent_type == Ssh)
    pub ssh_host: String,               // SSH host (e.g., "server.example.com")
    pub ssh_port: String,               // SSH port (string for input, parsed to u16)
    pub ssh_user: String,               // SSH username (optional)
    pub ssh_identity_file: String,      // Path to SSH identity file (optional)
    pub ssh_input_focus: SshInputFocus, // Which SSH field has focus

    // Favorites (shown inline in InputRepoSource for quick selection)
    pub favorites_store: crate::config::FavoritesStore, // Cached favorites
    pub selected_favorite_index: Option<usize>,         // Selected favorite (None = typing URL)
}

impl Default for NewSessionState {
    fn default() -> Self {
        Self {
            source_choice: RepoSourceChoice::default(),
            available_repos: vec![],
            filtered_repos: vec![],
            selected_repo_index: None,
            current_repo_branch: None,
            branch_name: String::new(),
            step: NewSessionStep::SelectSource, // Start with source selection
            filter_text: String::new(),
            is_current_dir_mode: false,
            skip_permissions: false,
            mode: crate::models::SessionMode::Interactive,
            boss_prompt: TextEditor::new(),
            file_finder: FuzzyFileFinderState::new(),
            restart_session_id: None,
            // Agent selection defaults
            selected_agent: SessionAgentType::default(),
            agent_options: SessionAgentOption::all(),
            selected_agent_index: 0,
            // Model selection defaults (Sonnet as default)
            selected_model: ClaudeModel::default(),
            model_options: ClaudeModel::all(),
            selected_model_index: 0,
            agent_model_focus: AgentModelFocus::default(),
            // Remote repository support defaults
            repo_input: String::new(),
            repo_source: None,
            remote_branches: Vec::new(),
            filtered_branches: Vec::new(),
            branch_filter_text: String::new(),
            selected_branch_index: 0,
            selected_base_branch: None,
            cached_repo_path: None,
            repo_validation_error: None,
            is_validating: false,
            recent_repos: Vec::new(),
            branch_checkout_mode: BranchCheckoutMode::default(),
            // SSH configuration defaults
            ssh_host: String::new(),
            ssh_port: "22".to_string(),
            ssh_user: String::new(),
            ssh_identity_file: String::new(),
            ssh_input_focus: SshInputFocus::default(),
            // Favorites defaults (shown inline in InputRepoSource)
            favorites_store: crate::config::FavoritesStore::default(),
            selected_favorite_index: None,
        }
    }
}

impl NewSessionState {
    pub fn apply_filter(&mut self) {
        self.filtered_repos.clear();
        let filter_lower = self.filter_text.to_lowercase();

        for (idx, repo) in self.available_repos.iter().enumerate() {
            if let Some(folder_name) = repo.file_name() {
                if let Some(name_str) = folder_name.to_str() {
                    if name_str.to_lowercase().contains(&filter_lower) {
                        self.filtered_repos.push((idx, repo.clone()));
                    }
                }
            }
        }

        // Reset selection if current selection is out of bounds
        if let Some(idx) = self.selected_repo_index {
            if idx >= self.filtered_repos.len() {
                self.selected_repo_index = if self.filtered_repos.is_empty() {
                    None
                } else {
                    Some(0)
                };
            }
        } else if !self.filtered_repos.is_empty() {
            self.selected_repo_index = Some(0);
        }
    }

    // Agent selection helpers
    pub fn next_agent(&mut self) {
        if !self.agent_options.is_empty() {
            self.selected_agent_index = (self.selected_agent_index + 1) % self.agent_options.len();
            // Also update selected_agent to keep in sync
            self.selected_agent = self.current_agent_type();
            self.enforce_mode_constraints();
        }
    }

    pub fn prev_agent(&mut self) {
        if !self.agent_options.is_empty() {
            self.selected_agent_index = if self.selected_agent_index == 0 {
                self.agent_options.len() - 1
            } else {
                self.selected_agent_index - 1
            };
            // Also update selected_agent to keep in sync
            self.selected_agent = self.current_agent_type();
            self.enforce_mode_constraints();
        }
    }

    pub fn current_agent_option(&self) -> Option<&SessionAgentOption> {
        self.agent_options.get(self.selected_agent_index)
    }

    pub fn current_agent_type(&self) -> SessionAgentType {
        self.current_agent_option().map(|o| o.agent_type).unwrap_or_default()
    }

    pub fn is_current_agent_available(&self) -> bool {
        self.current_agent_type().is_available()
    }

    pub fn is_boss_mode_available(&self) -> bool {
        self.selected_agent == SessionAgentType::Claude
    }

    pub fn enforce_mode_constraints(&mut self) {
        if !self.is_boss_mode_available() && self.mode == crate::models::SessionMode::Boss {
            self.mode = crate::models::SessionMode::Interactive;
        }
    }

    /// Select the current agent and update selected_agent field
    pub fn confirm_agent_selection(&mut self) {
        self.selected_agent = self.current_agent_type();
        self.enforce_mode_constraints();
    }

    /// Get the selected repo path
    pub fn get_selected_repo_path(&self) -> Option<std::path::PathBuf> {
        self.selected_repo_index
            .and_then(|idx| self.filtered_repos.get(idx))
            .map(|(_, path)| path.clone())
    }

    // Model selection helpers

    /// Move to next model in the list
    pub fn next_model(&mut self) {
        if !self.model_options.is_empty() {
            self.selected_model_index = (self.selected_model_index + 1) % self.model_options.len();
            self.selected_model = self.model_options[self.selected_model_index];
        }
    }

    /// Move to previous model in the list
    pub fn prev_model(&mut self) {
        if !self.model_options.is_empty() {
            self.selected_model_index = if self.selected_model_index == 0 {
                self.model_options.len() - 1
            } else {
                self.selected_model_index - 1
            };
            self.selected_model = self.model_options[self.selected_model_index];
        }
    }

    /// Get the currently selected model
    pub fn current_model(&self) -> ClaudeModel {
        self.model_options.get(self.selected_model_index).copied().unwrap_or_default()
    }

    /// Toggle focus between Agent and Model panels
    pub fn toggle_agent_model_focus(&mut self) {
        self.agent_model_focus = match self.agent_model_focus {
            AgentModelFocus::Agent => AgentModelFocus::Model,
            AgentModelFocus::Model => AgentModelFocus::Agent,
        };
    }

    /// Toggle between CreateNew and CheckoutExisting branch modes
    pub fn toggle_branch_checkout_mode(&mut self) {
        self.branch_checkout_mode = match self.branch_checkout_mode {
            BranchCheckoutMode::CreateNew => BranchCheckoutMode::CheckoutExisting,
            BranchCheckoutMode::CheckoutExisting => BranchCheckoutMode::CreateNew,
        };
    }

    /// Check if model selection should be shown (only for Claude agent)
    pub fn should_show_model_selection(&self) -> bool {
        self.current_agent_type() == SessionAgentType::Claude
    }

    /// Get the model to use for session creation (None for non-Claude agents)
    pub fn get_session_model(&self) -> Option<ClaudeModel> {
        if self.current_agent_type() == SessionAgentType::Claude {
            Some(self.selected_model)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NewSessionStep {
    SelectSource,    // Choose between Local repos, Remote URL, SSH, or Favorites
    InputRepoSource, // Enter URL for remote repos (shows favorites as suggestions)
    ValidatingRepo,  // Validating URL / cloning
    SelectBranch,    // Pick from remote branches
    SelectRepo,      // Browse/search local repos
    SelectFavorite,  // Pick from saved favorites
    SelectAgent,     // Choose agent (Claude, Shell, SSH, etc.)
    ConfigureSsh,    // Configure SSH connection (host, port, user, key)
    InputBranch,     // Name the session branch (ainb/...)
    SelectMode,      // Choose between Interactive and Boss mode
    InputPrompt,     // Enter prompt for Boss mode
    ConfigurePermissions,
    Creating,
}

/// Which SSH input field has focus
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SshInputFocus {
    #[default]
    Host,
    Port,
    User,
    IdentityFile,
}

/// Choice for repository source in new session flow
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RepoSourceChoice {
    #[default]
    Local, // Browse local repos
    Remote,    // Clone from URL
    Ssh,       // SSH connection to remote server
    Favorites, // Quick access to saved favorites
}

#[derive(Debug, Clone, PartialEq)]
pub enum AsyncAction {
    StartNewSession,         // Old - will be removed
    StartWorkspaceSearch,    // New - search all workspaces
    NewSessionInCurrentDir,  // New - create session in current directory
    NewSessionNormal,        // New - create normal new session with mode selection
    NewSessionWithRepoInput, // NEW: Start with URL/path input
    ValidateRepoSource,      // NEW: Parse and validate repo input
    CloneRemoteRepo,         // NEW: Clone remote repo to cache
    FetchRemoteBranches,     // NEW: Get branch list from remote
    CreateNewSession,
    DeleteSession(Uuid),           // New - delete session with container cleanup
    StopSession(Uuid),             // Soft-stop interactive session (kill tmux only)
    ResumeSession(Uuid, String), // Recreate tmux for a Stopped interactive session; String is the trigger key for audit
    BulkDeleteSessions(Vec<Uuid>), // Bulk delete multiple sessions
    RefreshWorkspaces,           // Manual refresh of workspace data
    FetchContainerLogs(Uuid),    // Fetch container logs for a session
    AttachToContainer(Uuid),     // Attach to a container session
    AttachToTmuxSession(Uuid),   // Attach to a tmux session
    KillContainer(Uuid),         // Kill container for a session
    AuthSetupOAuth,              // Run OAuth authentication setup
    AuthSetupApiKey,             // Save API key authentication
    ReauthenticateCredentials,   // Re-authenticate Claude credentials
    RestartSession(Uuid),        // Restart a stopped session with new container
    CleanupOrphaned,             // Clean up orphaned containers without worktrees
    AttachToOtherTmux(String),   // Attach to a non-agents-in-a-box tmux session by name
    KillOtherTmux(String),       // Kill a non-agents-in-a-box tmux session by name
    KillOtherTmuxSessions(Vec<String>), // Kill multiple non-agents-in-a-box tmux sessions by name
    ConfirmOtherTmuxRename,      // Confirm and execute rename for "Other tmux" session
    // Shell session actions (one shell per workspace)
    OpenWorkspaceShell {
        workspace_index: usize,                 // Index of workspace to open shell for
        target_dir: Option<std::path::PathBuf>, // Optional: cd to this directory (worktree)
    },
    OpenShellAtPath(std::path::PathBuf), // Open shell directly at a path (no workspace required)
    KillWorkspaceShell(usize),           // Kill workspace shell by workspace index
    // SSH session actions
    CreateSshSession, // Create a new SSH session with configured target
    // Editor action
    OpenInEditor(std::path::PathBuf), // Open workspace in preferred editor
    // Onboarding actions
    OnboardingCheckDeps, // Run dependency check during onboarding
}

impl Default for AppState {
    fn default() -> Self {
        // Load persistent configuration
        let app_config = AppConfig::load().unwrap_or_else(|e| {
            warn!("Failed to load config, using defaults: {}", e);
            AppConfig::default()
        });
        let mut home_screen_v2_state = HomeScreenV2State::default();
        home_screen_v2_state.restore_sidebar_width(app_config.ui_preferences.home_sidebar_width);
        let mut sessions_pane_state = SessionsPaneState::default();
        sessions_pane_state.restore(
            app_config.ui_preferences.sessions_sidebar_width,
            app_config.ui_preferences.sessions_sidebar_collapsed.unwrap_or(false),
        );

        Self {
            workspaces: Vec::new(),
            selected_workspace_index: None,
            selected_session_index: None,
            shell_selected: false,
            selected_sessions: HashSet::new(),
            expand_all_workspaces: true, // Default to expanded view
            session_filter: SessionFilter::All,
            current_screen: screen_ids::HOME.to_string(),
            should_quit: false,
            logs: HashMap::new(),
            help_visible: false,
            new_session_state: None,
            pending_async_action: None,
            async_operation_cancelled: false,
            confirmation_dialog: None,
            ui_needs_refresh: false,
            claude_chat_visible: false,
            focused_pane: FocusedPane::Sessions,
            sessions_pane_state,
            is_current_dir_git_repo: false,
            last_logs_session_id: None,
            attached_session_id: None,
            auth_setup_state: None,
            log_last_updated: HashMap::new(),
            last_log_check: None,
            last_token_refresh_check: None,
            claude_chat_state: None,
            live_logs: HashMap::new(),
            claude_manager: None,
            log_streaming_coordinator: None,
            log_sender: None,
            git_view_state: None,
            previous_screen: None,
            notifications: Vec::new(),
            pending_event: None,

            // Initialize quick commit state
            quick_commit_message: None,
            quick_commit_cursor: 0,

            // Initialize tmux integration
            tmux_sessions: HashMap::new(),
            preview_update_task: None,

            // Initialize other tmux sessions
            other_tmux_sessions: Vec::new(),
            other_tmux_expanded: true, // Default to expanded
            selected_other_tmux_index: None,
            selected_other_tmux_sessions: HashSet::new(),
            other_tmux_rename_mode: false,
            other_tmux_rename_buffer: String::new(),

            // Initialize SSH sessions (separate section)
            ssh_sessions: Vec::new(),
            ssh_sessions_expanded: true, // Default to expanded
            selected_ssh_session_index: None,
            ssh_session_rename_mode: false,
            ssh_session_rename_buffer: String::new(),
            ssh_display_name_store: SshDisplayNameStore::load(),

            // AINB 2.0: Home screen and agent selection
            home_screen_state: HomeScreenState::default(),
            home_screen_v2_state,
            agent_selection_state: AgentSelectionState::default(),
            config_screen_state: ConfigScreenState::from_app_config(&app_config),
            auth_provider_popup_state: AuthProviderPopupState::from_app_config(&app_config),
            config_popup_state: crate::components::config_popup::ConfigPopupState::default(),

            // Onboarding wizard state (initialized to None, set during app init)
            onboarding_state: None,

            // Setup menu state
            setup_menu_state: crate::components::setup_menu::SetupMenuState::new(),

            // Persistent configuration
            app_config,

            // Log history viewer state
            log_history_state: crate::components::LogHistoryViewerState::new(),

            // Changelog viewer state
            changelog_state: crate::components::ChangelogState::new(),

            // Session recovery state (lazy-load when entering view)
            session_recovery_state: crate::components::SessionRecoveryState::default(),

            pending_plugin_renders: std::collections::HashMap::new(),
            plugin_render_areas: std::collections::HashMap::new(),
            plugin_runtime: None,

            statusline_status_cache: None,

            live_window_watcher: crate::models::live_window_watcher::LiveWindowWatcher::default(),

            // Skills browser state
            skills_state: crate::components::skills::SkillsViewState::default(),
            skills_load_receiver: None,

            // Periodic session snapshot tracking
            last_snapshot_time: None,

            // Throttled tmux preview updates
            last_preview_update: None,

            // Background workspace loading state
            is_loading_workspaces: false,
            workspace_load_error: None,
            workspace_load_started: None,
            workspace_load_receiver: None,
        }
    }
}

/// Monotonic-min merge for `oldest_call_day`. Keeps the session-wide
/// anchor honest across narrow→wide period switches: a wide load
/// establishes the true extent and a subsequent narrow load only
/// narrows the data view, never the anchor. `None` candidates leave
/// the existing anchor untouched; `None` existing accepts whatever
/// the candidate gives.
fn merge_oldest_call_day(
    existing: Option<chrono::NaiveDate>,
    candidate: Option<chrono::NaiveDate>,
) -> Option<chrono::NaiveDate> {
    match (existing, candidate) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, b) => b,
    }
}

/// TTL for the cached `detect_statusline_status()` result. The Stats
/// screen re-renders at ~30-60Hz and the global `W` shortcut runs on
/// every keystroke; without a cache each frame pays a settings.json
/// read. 15s is short enough that a manual edit of `~/.claude/settings.json`
/// is reflected almost immediately, long enough to coalesce normal
/// scrolling activity.
pub(crate) const STATUSLINE_STATUS_CACHE_TTL_SECS: u64 = 15;

impl AppState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Read the statusline status with a TTL-bounded cache.
    ///
    /// The first call (or any call after the cache has expired) re-reads
    /// `~/.claude/settings.json`; subsequent calls within
    /// [`STATUSLINE_STATUS_CACHE_TTL_SECS`] return the memoised value.
    /// Returns `None` only when status detection itself failed (IO/JSON
    /// error) — in that case both the global `W` shortcut and the
    /// top-of-Stats card no-op rather than guessing.
    pub fn statusline_status_cached(
        &mut self,
    ) -> Option<crate::cli::statusline_install::StatuslineStatus> {
        Self::statusline_status_cached_inner(
            &mut self.statusline_status_cache,
            std::time::Duration::from_secs(STATUSLINE_STATUS_CACHE_TTL_SECS),
            Instant::now(),
            crate::cli::statusline_install::detect_statusline_status,
        )
    }

    /// Test seam for [`statusline_status_cached`]. Lets unit tests inject
    /// a clock and a fake detector to verify TTL coalescing without
    /// touching the filesystem.
    pub(crate) fn statusline_status_cached_inner<F>(
        cache: &mut Option<(
            Option<crate::cli::statusline_install::StatuslineStatus>,
            Instant,
        )>,
        ttl: std::time::Duration,
        now: Instant,
        detect: F,
    ) -> Option<crate::cli::statusline_install::StatuslineStatus>
    where
        F: FnOnce() -> anyhow::Result<crate::cli::statusline_install::StatuslineStatus>,
    {
        if let Some((value, written)) = cache {
            if now.saturating_duration_since(*written) < ttl {
                return value.clone();
            }
        }
        let fresh = detect().ok();
        *cache = Some((fresh.clone(), now));
        fresh
    }

    /// Drop the cached statusline status so the next reader re-detects.
    /// Called after the install event lands so the CTA flips on the very
    /// next frame instead of waiting out the TTL.
    pub fn invalidate_statusline_status_cache(&mut self) {
        self.statusline_status_cache = None;
    }

    /// Get the log directory path for the log history viewer
    pub fn log_dir(&self) -> Option<std::path::PathBuf> {
        dirs::home_dir().map(|h| h.join(".agents-in-a-box").join("logs"))
    }

    /// Initialize Claude integration if authentication is available
    pub async fn init_claude_integration(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        match ClaudeApiClient::load_auth_from_config() {
            Ok(auth) => {
                info!("Initializing Claude API integration");
                match ClaudeApiClient::with_auth(auth) {
                    Ok(client) => {
                        // Test connection
                        match client.test_connection().await {
                            Ok(()) => {
                                let mut manager = ClaudeChatManager::new(client);
                                manager.create_session(None);
                                self.claude_manager = Some(manager);
                                self.claude_chat_state = Some(ClaudeChatState::new());
                                info!("Claude integration initialized successfully");
                                Ok(())
                            }
                            Err(e) => {
                                warn!("Claude API connection test failed: {}", e);
                                Err(format!("Claude API connection failed: {}", e).into())
                            }
                        }
                    }
                    Err(e) => {
                        warn!("Failed to create Claude API client: {}", e);
                        Err(e.into())
                    }
                }
            }
            Err(e) => {
                info!("Claude authentication not configured: {}", e);
                // This is OK - user can set up auth later
                Ok(())
            }
        }
    }

    /// Send a message to Claude
    pub async fn send_claude_message(
        &mut self,
        message: String,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if let (Some(chat_state), Some(manager)) =
            (&mut self.claude_chat_state, &mut self.claude_manager)
        {
            chat_state.start_streaming(message.clone());

            // Start streaming response
            match manager.stream_message(&message).await {
                Ok(mut stream) => {
                    // Handle streaming response
                    while let Some(event) = stream.next().await {
                        match event {
                            Ok(ClaudeStreamingEvent::ContentBlockDelta { delta, .. }) => {
                                chat_state.append_streaming_response(&delta.text);
                                self.ui_needs_refresh = true;
                            }
                            Ok(ClaudeStreamingEvent::MessageStop) => {
                                chat_state.finish_streaming();
                                self.ui_needs_refresh = true;
                                break;
                            }
                            Ok(ClaudeStreamingEvent::Error { error }) => {
                                error!("Claude API error: {}", error.message);
                                chat_state.finish_streaming();
                                return Err(format!("Claude error: {}", error.message).into());
                            }
                            Ok(_) => {
                                // Other events - continue
                            }
                            Err(e) => {
                                error!("Streaming error: {}", e);
                                chat_state.finish_streaming();
                                return Err(e.into());
                            }
                        }
                    }
                    Ok(())
                }
                Err(e) => {
                    chat_state.is_streaming = false;
                    Err(e.into())
                }
            }
        } else {
            Err("Claude integration not initialized".into())
        }
    }

    /// Add a log entry to live logs
    pub fn add_live_log(&mut self, session_id: Uuid, log_entry: LogEntry) {
        self.live_logs.entry(session_id).or_insert_with(Vec::new).push(log_entry);

        // Limit log entries to prevent memory issues (keep last 1000)
        if let Some(logs) = self.live_logs.get_mut(&session_id) {
            if logs.len() > 1000 {
                logs.drain(0..logs.len() - 1000);
            }
        }

        self.ui_needs_refresh = true;
    }

    /// Start log streaming for a session when it becomes active
    pub async fn start_log_streaming_for_session(
        &mut self,
        session_id: Uuid,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(coordinator) = &mut self.log_streaming_coordinator {
            // Find the session to get container info
            let session_info = self
                .workspaces
                .iter()
                .flat_map(|w| &w.sessions)
                .find(|s| s.id == session_id)
                .and_then(|s| {
                    s.container_id.clone().map(|container_id| {
                        (
                            container_id,
                            format!("{}-{}", s.name, s.branch_name),
                            s.mode.clone(),
                        )
                    })
                });

            if let Some((container_id, container_name, session_mode)) = session_info {
                info!(
                    "Starting log streaming for session {} (container: {})",
                    session_id, container_id
                );
                coordinator
                    .start_streaming(session_id, container_id, container_name, session_mode)
                    .await?;
            }
        }
        Ok(())
    }

    /// Stop log streaming for a session when it becomes inactive
    pub async fn stop_log_streaming_for_session(
        &mut self,
        session_id: Uuid,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(coordinator) = &mut self.log_streaming_coordinator {
            info!("Stopping log streaming for session {}", session_id);
            coordinator.stop_streaming(session_id).await?;
        }
        Ok(())
    }

    /// Clear live logs for a session
    pub fn clear_live_logs(&mut self, session_id: Uuid) {
        self.live_logs.remove(&session_id);
        self.ui_needs_refresh = true;
    }

    /// Get total live log count across all sessions
    pub fn total_live_log_count(&self) -> usize {
        self.live_logs.values().map(|logs| logs.len()).sum()
    }

    /// Check if this is first time setup (no auth configured)
    pub fn is_first_time_setup() -> bool {
        let home_dir = match dirs::home_dir() {
            Some(dir) => dir,
            None => return false,
        };

        let auth_dir = home_dir.join(".agents-in-a-box/auth");

        let has_credentials = auth_dir.join(".credentials.json").exists();
        let has_claude_json = auth_dir.join(".claude.json").exists();
        let has_api_key = std::env::var("ANTHROPIC_API_KEY").is_ok();
        let has_env_file = home_dir.join(".agents-in-a-box/.env").exists();

        // Load .env file if it exists to check for API key
        let has_env_api_key = if has_env_file {
            std::fs::read_to_string(home_dir.join(".agents-in-a-box/.env"))
                .map(|contents| contents.contains("ANTHROPIC_API_KEY="))
                .unwrap_or(false)
        } else {
            false
        };

        // For OAuth authentication, we need BOTH .credentials.json AND .claude.json
        // If we have a refresh token, we can refresh expired access tokens, so it's not "first time setup"
        let has_valid_oauth = if has_credentials && has_claude_json {
            // Check if we have OAuth credentials (either valid token OR refresh token to get new one)
            let credentials_path = auth_dir.join(".credentials.json");
            std::fs::read_to_string(&credentials_path)
                .ok()
                .and_then(|contents| serde_json::from_str::<serde_json::Value>(&contents).ok())
                .and_then(|json| json.get("claudeAiOauth").cloned())
                .map(|oauth| {
                    // If we have a refresh token, we can refresh even if access token is expired
                    oauth.get("refreshToken").is_some()
                        || Self::is_oauth_token_valid(&credentials_path)
                })
                .unwrap_or(false)
        } else {
            false
        };

        // Show auth screen if we don't have valid OAuth setup AND no API key alternatives
        !has_valid_oauth && !has_api_key && !has_env_api_key
    }

    /// Check if OAuth token in credentials file is still valid (not expired)
    fn is_oauth_token_valid(credentials_path: &std::path::Path) -> bool {
        use std::fs;

        if let Ok(contents) = fs::read_to_string(credentials_path) {
            // Parse the JSON to extract OAuth token info
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&contents) {
                if let Some(oauth) = json.get("claudeAiOauth") {
                    if let Some(expires_at) = oauth.get("expiresAt").and_then(|v| v.as_u64()) {
                        // Check if current time is before expiration time
                        let current_time = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64;

                        if current_time < expires_at {
                            info!(
                                "OAuth token is valid, expires at: {}",
                                chrono::DateTime::from_timestamp_millis(expires_at as i64)
                                    .map(|dt| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string())
                                    .unwrap_or_else(|| "unknown".to_string())
                            );
                            return true;
                        }
                        warn!(
                            "OAuth token has expired at: {}",
                            chrono::DateTime::from_timestamp_millis(expires_at as i64)
                                .map(|dt| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string())
                                .unwrap_or_else(|| "unknown".to_string())
                        );
                        return false;
                    }
                }
            }
        }

        // If we can't parse or find expiration info, assume invalid
        warn!("Could not validate OAuth token from credentials file");
        false
    }

    /// Check if OAuth token needs refresh (expires within 30 minutes)
    fn oauth_token_needs_refresh(credentials_path: &std::path::Path) -> bool {
        use std::fs;

        if let Ok(contents) = fs::read_to_string(credentials_path) {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&contents) {
                if let Some(oauth) = json.get("claudeAiOauth") {
                    // Check if we have a refresh token
                    if oauth.get("refreshToken").is_none() {
                        info!("No refresh token available");
                        return false;
                    }

                    if let Some(expires_at) = oauth.get("expiresAt").and_then(|v| v.as_u64()) {
                        let current_time = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64;

                        // Refresh if token expires in less than 30 minutes
                        let buffer_time = 30 * 60 * 1000; // 30 minutes in milliseconds

                        if current_time >= (expires_at.saturating_sub(buffer_time)) {
                            info!(
                                "OAuth token needs refresh, expires at: {}",
                                chrono::DateTime::from_timestamp_millis(expires_at as i64)
                                    .map(|dt| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string())
                                    .unwrap_or_else(|| "unknown".to_string())
                            );
                            return true;
                        }
                    }
                }
            }
        }

        false
    }

    /// Check if onboarding wizard should be shown
    /// Returns true if:
    /// - ~/.agents-in-a-box directory doesn't exist
    /// - OR onboarding config doesn't exist
    /// - OR onboarding not completed
    /// - OR major version changed
    pub fn needs_onboarding() -> bool {
        use crate::config::OnboardingConfig;

        // First check: does the base directory exist at all?
        if !OnboardingConfig::base_dir_exists() {
            return true;
        }

        // Second check: load and check onboarding config
        match OnboardingConfig::load() {
            Ok(config) => config.needs_onboarding(),
            Err(_) => true, // If we can't load config, need onboarding
        }
    }

    /// Start the onboarding wizard
    /// Optionally start at a specific step (useful for setup menu shortcuts)
    pub fn start_onboarding(
        &mut self,
        is_factory_reset: bool,
        start_step: Option<crate::components::onboarding::OnboardingStep>,
    ) {
        use crate::components::onboarding::{OnboardingState, OnboardingStep};

        let mut state = if is_factory_reset {
            OnboardingState::for_factory_reset()
        } else {
            OnboardingState::new()
        };

        // If a specific start step is provided, jump to it
        if let Some(step) = start_step {
            state.current_step = step;
            // Initialize editors if starting directly at EditorSelection
            if step == OnboardingStep::EditorSelection {
                state.init_editors_if_needed();
            }
        }

        self.onboarding_state = Some(state);
        self.current_screen = screen_ids::ONBOARDING.to_string();
    }

    /// Complete the onboarding process
    pub fn complete_onboarding(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        use crate::config::OnboardingConfig;

        if let Some(state) = &self.onboarding_state {
            // Save onboarding config
            let mut config = OnboardingConfig::default();
            config.mark_completed();
            config.git_directories = state.get_valid_directories();
            config.skipped_dependencies = state.skipped_dependencies.clone();
            config.save().map_err(|e| format!("Failed to save onboarding config: {}", e))?;

            // Update app config with git directories
            self.app_config.workspace_defaults.workspace_scan_paths = state.get_valid_directories();

            // Save selected editor preference
            if let Some(editor) = state.get_selected_editor() {
                self.app_config.ui_preferences.preferred_editor = Some(editor);
            }

            if let Err(e) = self.app_config.save() {
                warn!(
                    "Failed to save app config during onboarding completion: {}",
                    e
                );
            }
        }

        // Clean up and return to home
        self.onboarding_state = None;
        self.current_screen = screen_ids::HOME.to_string();

        Ok(())
    }

    /// Cancel onboarding and return to home (for factory reset scenario)
    pub fn cancel_onboarding(&mut self) {
        self.onboarding_state = None;
        self.current_screen = screen_ids::HOME.to_string();
    }

    /// Refresh OAuth tokens using the refresh token
    pub async fn refresh_oauth_tokens(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        info!("Attempting to refresh OAuth tokens");

        let home_dir = dirs::home_dir().ok_or("Could not determine home directory")?;
        let auth_dir = home_dir.join(".agents-in-a-box").join("auth");
        let credentials_path = auth_dir.join(".credentials.json");

        // Check if tokens actually need refresh
        if !Self::oauth_token_needs_refresh(&credentials_path) {
            info!("OAuth tokens do not need refresh yet");
            return Ok(());
        }

        // Build the Docker image if needed
        let image_name = "agents-box:agents-dev";
        let image_check = tokio::process::Command::new("docker")
            .args(["image", "inspect", image_name])
            .output()
            .await?;

        if !image_check.status.success() {
            info!("Building agents-dev image for token refresh...");
            let build_status = tokio::process::Command::new("docker")
                .args(["build", "-t", image_name, "docker/agents-dev"])
                .status()
                .await?;

            if !build_status.success() {
                return Err("Failed to build image for token refresh".into());
            }
        }

        // Run the oauth-refresh.js script in a container (with retries built-in)
        info!("Running OAuth token refresh in container");

        // Create the volume mount string that will live long enough
        let volume_mount = format!("{}:/home/claude-user/.claude", auth_dir.display());

        // Build args based on debug mode
        let mut args = vec![
            "run",
            "--rm",
            "-v",
            &volume_mount,
            "-e",
            "PATH=/home/claude-user/.npm-global/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
            "-e",
            "HOME=/home/claude-user",
        ];

        // Add debug env if needed
        // Check if we're in debug mode by checking RUST_LOG env var
        if std::env::var("RUST_LOG").unwrap_or_default().contains("debug") {
            args.push("-e");
            args.push("DEBUG=1");
        }

        args.extend([
            "-w",
            "/home/claude-user",
            "--user",
            "claude-user",
            "--entrypoint",
            "node",
            image_name,
            "/app/scripts/oauth-refresh.js",
        ]);

        let output = tokio::process::Command::new("docker").args(&args).output().await?;

        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            info!("OAuth token refresh successful: {}", stdout.trim());

            // Verify the new token is valid
            if Self::is_oauth_token_valid(&credentials_path) {
                info!("New OAuth token verified as valid");
                Ok(())
            } else {
                Err("Token refresh succeeded but new token is invalid".into())
            }
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            warn!("OAuth token refresh failed");
            warn!("Stderr: {}", stderr.trim());
            warn!("Stdout: {}", stdout.trim());
            Err(format!("Token refresh failed: {}", stderr.trim()).into())
        }
    }

    pub fn check_current_directory_status(&mut self) {
        use crate::git::workspace_scanner::WorkspaceScanner;
        use std::env;

        if let Ok(current_dir) = env::current_dir() {
            self.is_current_dir_git_repo =
                WorkspaceScanner::validate_workspace(&current_dir).unwrap_or(false);

            if self.is_current_dir_git_repo {
                info!(
                    "Current directory is a valid git repository: {:?}",
                    current_dir
                );
            } else {
                info!(
                    "Current directory is not a git repository: {:?}",
                    current_dir
                );
                // No longer auto-trigger workspace search - users can input repos via 'n' key
            }
        } else {
            warn!("Could not determine current directory");
            self.is_current_dir_git_repo = false;
        }
    }

    pub async fn load_real_workspaces(&mut self) {
        info!("Loading active sessions (both Docker and Interactive)");

        // Preserve shell_sessions before clearing workspaces
        // Map workspace path -> shell_session for restoration after reload
        let preserved_shells: std::collections::HashMap<
            std::path::PathBuf,
            crate::models::ShellSession,
        > = self
            .workspaces
            .iter()
            .filter_map(|w| w.shell_session.clone().map(|s| (w.path.clone(), s)))
            .collect();

        // Clear existing workspaces before loading to prevent duplicates
        self.workspaces.clear();

        // Check and refresh OAuth tokens if needed (only if Docker is available)
        let home_dir = dirs::home_dir();
        if let Some(home) = home_dir {
            let credentials_path =
                home.join(".agents-in-a-box").join("auth").join(".credentials.json");

            // Only attempt refresh if we have OAuth credentials AND Docker is available
            if credentials_path.exists() && Self::oauth_token_needs_refresh(&credentials_path) {
                if self.is_docker_available().await {
                    info!("Docker available - attempting OAuth token refresh");
                    match self.refresh_oauth_tokens().await {
                        Ok(()) => info!("OAuth tokens refreshed successfully"),
                        Err(e) => warn!("Failed to refresh OAuth tokens: {}", e),
                    }
                } else {
                    info!("Docker not available - skipping OAuth token refresh");
                }
            }
        }

        // Load Boss mode sessions (Docker-based) if Docker is available.
        // Capped at 5s — a slow or wedged Docker daemon must not block
        // the workspaces panel from rendering. Interactive mode is
        // tmux-only and runs unconditionally after this returns.
        const BOSS_MODE_TIMEOUT: Duration = Duration::from_secs(5);
        if self.is_docker_available().await {
            info!("Docker available - loading Boss mode sessions");
            match tokio::time::timeout(BOSS_MODE_TIMEOUT, self.load_boss_mode_sessions()).await {
                Ok(()) => {}
                Err(_) => warn!(
                    "load_real_workspaces: Boss mode load exceeded {}s budget — proceeding with Interactive only",
                    BOSS_MODE_TIMEOUT.as_secs()
                ),
            }
        } else {
            info!("Docker not available - skipping Boss mode session loading");
        }

        // Load Interactive mode sessions (always attempt, no Docker needed)
        info!("Loading Interactive mode sessions");
        self.load_interactive_mode_sessions().await;

        // Load other tmux sessions (not managed by agents-in-a-box)
        info!("Loading other tmux sessions");
        self.load_other_tmux_sessions().await;

        // Restore preserved shell_sessions to matching workspaces
        if !preserved_shells.is_empty() {
            info!(
                "Restoring {} preserved shell sessions",
                preserved_shells.len()
            );
            for workspace in &mut self.workspaces {
                if let Some(shell) = preserved_shells.get(&workspace.path) {
                    // Only restore if the tmux session still exists
                    let check = tokio::process::Command::new("tmux")
                        .args(["has-session", "-t", &shell.tmux_session_name])
                        .output()
                        .await;

                    if check.map(|o| o.status.success()).unwrap_or(false) {
                        info!(
                            "Restored shell session '{}' for workspace '{}'",
                            shell.tmux_session_name, workspace.name
                        );
                        workspace.set_shell_session(shell.clone());
                    } else {
                        info!(
                            "Shell session '{}' no longer exists, not restoring",
                            shell.tmux_session_name
                        );
                    }
                }
            }
        }

        // Also try to auto-detect workspace shells from tmux
        self.auto_detect_workspace_shells().await;

        // Reset selection state before setting new selection
        // This is critical to avoid stale indices after refresh that break navigation
        self.selected_workspace_index = None;
        self.selected_session_index = None;
        self.shell_selected = false;
        self.selected_ssh_session_index = None;
        self.selected_other_tmux_index = None;

        // Set initial selection
        if !self.workspaces.is_empty() {
            self.selected_workspace_index = Some(0);
            if !self.workspaces[0].sessions.is_empty() {
                self.selected_session_index = Some(0);
            } else if self.workspaces[0].shell_session.is_some() {
                // First workspace has no sessions but has a shell - select it
                self.shell_selected = true;
            }
            // If workspace has neither sessions nor shell, selection indices stay None
            // which is the correct state for an empty workspace
        } else if !self.ssh_sessions.is_empty() {
            // No workspaces but there are SSH sessions - select the first one
            self.selected_ssh_session_index = Some(0);
        } else if !self.other_tmux_sessions.is_empty() {
            // No workspaces or SSH sessions but there are "Other tmux" sessions - select the first one
            self.selected_other_tmux_index = Some(0);
        } else {
            info!("No active sessions found. Use 'n' to create a new session.");
            // Selection indices already reset above
        }

        // Queue logs fetch for the currently selected session if any
        self.queue_logs_fetch();
    }

    /// Timeout for Docker operations in seconds
    const DOCKER_TIMEOUT_SECS: u64 = 10;

    /// Start loading workspaces in the background (non-blocking)
    /// Returns a channel receiver that will receive the result
    pub fn start_background_workspace_loading(
        &mut self,
    ) -> mpsc::UnboundedSender<WorkspaceLoadResult> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.workspace_load_receiver = Some(rx);
        self.is_loading_workspaces = true;
        self.workspace_load_started = Some(Instant::now());
        self.workspace_load_error = None;
        tx
    }

    /// Check for completed background workspace loading and apply results
    /// Returns true if workspaces were updated
    pub fn check_workspace_loading_complete(&mut self) -> bool {
        if let Some(ref mut receiver) = self.workspace_load_receiver {
            match receiver.try_recv() {
                Ok(result) => {
                    self.is_loading_workspaces = false;
                    self.workspace_load_receiver = None;

                    match result {
                        WorkspaceLoadResult::Success(mut workspaces) => {
                            info!(
                                "Background workspace loading completed: {} workspaces",
                                workspaces.len()
                            );

                            // Extract SSH sessions from workspaces into their own section
                            let mut ssh_sessions = Vec::new();
                            for workspace in &mut workspaces {
                                let (ssh, non_ssh): (Vec<_>, Vec<_>) =
                                    workspace.sessions.drain(..).partition(|s| {
                                        s.agent_type == crate::models::SessionAgentType::Ssh
                                    });
                                workspace.sessions = non_ssh;
                                ssh_sessions.extend(ssh);
                            }

                            // Remove empty workspaces (those that only had SSH sessions)
                            workspaces
                                .retain(|w| !w.sessions.is_empty() || w.shell_session.is_some());

                            info!(
                                "Separated {} SSH sessions from {} workspaces",
                                ssh_sessions.len(),
                                workspaces.len()
                            );

                            self.workspaces = workspaces;
                            self.ssh_sessions = ssh_sessions;
                            self.workspace_load_error = None;

                            // Populate tmux_sessions HashMap for Interactive mode sessions
                            // This is needed for update_tmux_previews() to capture pane content
                            for workspace in &self.workspaces {
                                for session in &workspace.sessions {
                                    if session.mode == crate::models::SessionMode::Interactive {
                                        // Use tmux_session_name if available, otherwise generate from session name
                                        let tmux_name = session
                                            .tmux_session_name
                                            .clone()
                                            .unwrap_or_else(|| session.get_tmux_name());
                                        let tmux_session = crate::tmux::TmuxSession::new(
                                            tmux_name,
                                            "claude".to_string(),
                                        );
                                        self.tmux_sessions.insert(session.id, tmux_session);
                                        debug!(
                                            "Populated tmux_sessions for session {}: {}",
                                            session.id, session.name
                                        );
                                    }
                                }
                            }
                            info!(
                                "Populated tmux_sessions with {} entries",
                                self.tmux_sessions.len()
                            );

                            // Set initial selection
                            self.selected_workspace_index = None;
                            self.selected_session_index = None;
                            self.shell_selected = false;
                            self.selected_ssh_session_index = None;
                            self.selected_other_tmux_index = None;

                            if !self.workspaces.is_empty() {
                                self.selected_workspace_index = Some(0);
                                if !self.workspaces[0].sessions.is_empty() {
                                    self.selected_session_index = Some(0);
                                } else if self.workspaces[0].shell_session.is_some() {
                                    self.shell_selected = true;
                                }
                            } else if !self.ssh_sessions.is_empty() {
                                // No workspaces but there are SSH sessions - select the first one
                                self.selected_ssh_session_index = Some(0);
                            } else if !self.other_tmux_sessions.is_empty() {
                                // No workspaces or SSH sessions but there are "Other tmux" sessions
                                self.selected_other_tmux_index = Some(0);
                            }

                            self.add_success_notification("Workspaces loaded".to_string());
                            return true;
                        }
                        WorkspaceLoadResult::Error(err) => {
                            warn!("Background workspace loading failed: {}", err);
                            self.workspace_load_error = Some(err.clone());
                            self.add_warning_notification(format!(
                                "Failed to load sessions: {}",
                                err
                            ));
                            return true;
                        }
                        WorkspaceLoadResult::Timeout => {
                            warn!("Background workspace loading timed out");
                            self.workspace_load_error =
                                Some("Docker operation timed out".to_string());
                            self.add_warning_notification(
                                "Docker is slow - sessions may be incomplete".to_string(),
                            );
                            return true;
                        }
                    }
                }
                Err(mpsc::error::TryRecvError::Empty) => {
                    // Still loading, check for timeout
                    if let Some(started) = self.workspace_load_started {
                        if started.elapsed().as_secs() > Self::DOCKER_TIMEOUT_SECS * 3 {
                            // Hard timeout - stop waiting
                            warn!("Workspace loading hard timeout reached");
                            self.is_loading_workspaces = false;
                            self.workspace_load_receiver = None;
                            self.workspace_load_error = Some("Loading timed out".to_string());
                            self.add_warning_notification(
                                "Session loading timed out - using cached data".to_string(),
                            );
                            return true;
                        }
                    }
                }
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    // Channel closed without result - error
                    self.is_loading_workspaces = false;
                    self.workspace_load_receiver = None;
                    self.workspace_load_error = Some("Loading task failed".to_string());
                    return true;
                }
            }
        }
        false
    }

    /// Kick off a background scan of ~/.claude/skills and ~/.claude/agents.
    /// Skipped if already in-flight, or data is cached and `force` is false.
    /// Returns true if a new scan was spawned, false if coalesced.
    /// Mirrors `start_background_usage_load` so Skills screen navigation
    /// never blocks the event thread.
    pub fn start_background_skills_load(&mut self, force: bool) -> bool {
        if self.skills_load_receiver.is_some() {
            return false;
        }
        if !force && self.skills_state.data.is_some() {
            return false;
        }
        let (tx, rx) = mpsc::unbounded_channel();
        self.skills_load_receiver = Some(rx);
        self.skills_state.loading = true;
        tokio::spawn(async move {
            match tokio::task::spawn_blocking(crate::models::skills::parse_skills).await {
                Ok(data) => {
                    let _ = tx.send(data);
                }
                Err(e) => {
                    warn!("Skills parse task failed: {}", e);
                }
            }
        });
        true
    }

    /// Poll the background scan. Returns true if data was applied this tick.
    pub fn check_skills_load_complete(&mut self) -> bool {
        if let Some(ref mut receiver) = self.skills_load_receiver {
            match receiver.try_recv() {
                Ok(data) => {
                    self.skills_state.data = Some(data);
                    self.skills_state.loading = false;
                    self.skills_load_receiver = None;
                    true
                }
                Err(mpsc::error::TryRecvError::Empty) => false,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    self.skills_state.loading = false;
                    self.skills_load_receiver = None;
                    warn!("Skills parse task dropped its sender without delivering data");
                    self.add_warning_notification(
                        "Failed to parse skills; keeping cached data".to_string(),
                    );
                    true
                }
            }
        } else {
            false
        }
    }

    /// Load Boss mode sessions from Docker containers
    async fn load_boss_mode_sessions(&mut self) {
        // Try to load active Docker sessions
        match SessionLoader::new().await {
            Ok(loader) => {
                match loader.load_active_sessions().await {
                    Ok(mut workspaces) => {
                        // Append to existing workspaces instead of replacing
                        self.workspaces.append(&mut workspaces);
                        info!(
                            "Loaded {} Boss mode workspaces (total: {})",
                            workspaces.len(),
                            self.workspaces.len()
                        );
                    }
                    Err(e) => {
                        warn!("Failed to load Boss mode sessions: {}", e);
                    }
                }
            }
            Err(e) => {
                warn!("Failed to create session loader for Boss mode: {}", e);
            }
        }
    }

    /// Load Interactive mode sessions from tmux
    async fn load_interactive_mode_sessions(&mut self) {
        use crate::interactive::{InteractiveSessionManager, SessionStore};

        // Create Interactive session manager (no Docker needed)
        let mut manager = match InteractiveSessionManager::new() {
            Ok(m) => m,
            Err(e) => {
                warn!("Failed to create Interactive session manager: {}", e);
                return;
            }
        };

        // Track tmux names of live sessions so the stopped-detection pass below
        // does not double-add anything that was already discovered.
        let mut live_tmux_names: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        // Discover Interactive sessions from tmux
        match manager.list_sessions().await {
            Ok(sessions) => {
                info!(
                    "Discovered {} Interactive sessions from tmux",
                    sessions.len()
                );

                // Convert to Session models and add to workspaces
                for interactive_session in sessions {
                    live_tmux_names.insert(interactive_session.tmux_session_name.clone());
                    let session = interactive_session.to_session_model();

                    // Find or create workspace for this session.
                    // Use source_repository (the original git repo) not worktree_path parent.
                    // When the worktree is broken (no `.git`), source_repository was
                    // collapsed onto worktree_path by the discovery fallback — bucket
                    // every such session under a single sentinel path so they all
                    // collapse into one "(broken)" workspace row instead of fanning out.
                    let broken_bucket = std::path::PathBuf::from("__broken_worktrees__");
                    let is_broken = interactive_session.workspace_name
                        == crate::interactive::InteractiveSessionManager::BROKEN_WORKSPACE_NAME;
                    let workspace_path: &std::path::Path = if is_broken {
                        broken_bucket.as_path()
                    } else {
                        interactive_session.source_repository.as_path()
                    };

                    // Remove any stale entries for this session (e.g., added by Boss-mode loader)
                    for workspace in &mut self.workspaces {
                        workspace.sessions.retain(|s| s.id != interactive_session.session_id);
                    }

                    if let Some(workspace) = self.workspaces.iter_mut().find(|w| {
                        std::path::Path::new(&w.path).canonicalize().ok()
                            == workspace_path.canonicalize().ok()
                    }) {
                        // Add to existing workspace
                        workspace.sessions.push(session);
                    } else {
                        // Create new workspace
                        let mut workspace = crate::models::Workspace::new(
                            interactive_session.workspace_name.clone(),
                            workspace_path.to_path_buf(),
                        );
                        workspace.sessions.push(session);
                        self.workspaces.push(workspace);
                    }

                    // Store tmux session for attach operations
                    // Use the actual tmux session name from discovery to ensure captures work
                    // (branch_name may differ from the actual tmux session name)
                    let tmux_session = crate::tmux::TmuxSession::new(
                        interactive_session.tmux_session_name.clone(),
                        "claude".to_string(),
                    );
                    self.tmux_sessions.insert(interactive_session.session_id, tmux_session);
                }
            }
            Err(e) => {
                warn!("Failed to discover Interactive sessions: {}", e);
            }
        }

        // Second pass: discover Stopped sessions. These are entries persisted in
        // sessions.json whose tmux session is no longer alive but whose worktree
        // still exists on disk. Worktree-missing entries fall through to the
        // existing recovery flow.
        //
        // Skip "dead-but-not-deleted" worktrees (dir exists but `.git` file is
        // gone — usually a leftover cache like `.vite/` keeping the dir alive).
        // Without this guard the loader fabricates a phantom workspace named
        // after the sanitized worktree-dir basename and bunches every such
        // session into it, because the previous grouping key was
        // `worktree_path.parent()` (a single shared dir for every flat
        // worktree). These entries should be surfaced via /recover-sessions
        // instead.
        let store = SessionStore::load();
        for metadata in store.sessions().values() {
            if live_tmux_names.contains(&metadata.tmux_session_name) {
                continue;
            }
            if !metadata.worktree_path.exists() {
                continue;
            }

            let Some(source_repo) =
                crate::interactive::InteractiveSessionManager::get_source_repository(
                    &metadata.worktree_path,
                )
            else {
                debug!(
                    "Skipping stopped session {} — worktree {:?} has no `.git` file (broken). Use /recover-sessions to clean up.",
                    metadata.session_id, metadata.worktree_path
                );
                continue;
            };

            let stopped = Self::stopped_session_from_metadata(metadata);
            // Group by the actual source repository (matches Phase 1's
            // grouping above). The previous `worktree_path.parent()` key was
            // always the shared `~/.agents-in-a-box/worktrees/` dir, which
            // collapsed every stopped session into one bucket.
            let workspace_path = source_repo.clone();

            if let Some(workspace) = self.workspaces.iter_mut().find(|w| {
                std::path::Path::new(&w.path).canonicalize().ok()
                    == workspace_path.canonicalize().ok()
            }) {
                if !workspace.sessions.iter().any(|s| s.id == metadata.session_id) {
                    workspace.sessions.push(stopped);
                }
            } else {
                let workspace_name =
                    crate::interactive::InteractiveSessionManager::derive_workspace_name(
                        &metadata.worktree_path,
                        &source_repo,
                    );
                let mut workspace = crate::models::Workspace::new(workspace_name, workspace_path);
                workspace.sessions.push(stopped);
                self.workspaces.push(workspace);
            }
        }
    }

    /// Build a `Session` model in `Stopped` state from persisted metadata.
    /// Used to render sessions whose tmux is dead but whose worktree is alive.
    pub(crate) fn stopped_session_from_metadata(
        metadata: &crate::interactive::SessionMetadata,
    ) -> crate::models::Session {
        use crate::models::{Session, SessionMode, SessionStatus};

        let mut session = Session::new_with_options(
            metadata.workspace_name.clone(),
            metadata.worktree_path.to_string_lossy().to_string(),
            false, // skip_permissions — unknown when offline; safe default
            SessionMode::Interactive,
            None,
            metadata.agent_type,
            None,
        );
        session.id = metadata.session_id;
        session.tmux_session_name = Some(metadata.tmux_session_name.clone());
        session.status = SessionStatus::Stopped;
        session.created_at = metadata.created_at;
        session
    }

    /// Discover tmux sessions that are NOT managed by agents-in-a-box
    /// Also includes orphaned `tmux_` sessions whose worktrees no longer exist
    pub async fn load_other_tmux_sessions(&mut self) {
        use crate::interactive::SessionStore;
        use crate::models::OtherTmuxSession;
        use tokio::process::Command;

        info!("Discovering other tmux sessions");

        // Get all tmux sessions with format: name:attached:windows
        let output = match Command::new("tmux")
            .args([
                "list-sessions",
                "-F",
                "#{session_name}:#{session_attached}:#{session_windows}",
            ])
            .output()
            .await
        {
            Ok(o) => o,
            Err(e) => {
                debug!(
                    "Failed to list tmux sessions: {} (tmux might not be running)",
                    e
                );
                self.other_tmux_sessions.clear();
                self.selected_other_tmux_sessions.clear();
                return;
            }
        };

        if !output.status.success() {
            debug!("No tmux sessions found (tmux might not be running)");
            self.other_tmux_sessions.clear();
            self.selected_other_tmux_sessions.clear();
            return;
        }

        // Load session store to identify orphaned tmux_ sessions
        let session_store = SessionStore::load();

        // Collect tmux names that appear in loaded workspaces (successfully matched)
        let matched_tmux_names: std::collections::HashSet<&str> = self
            .workspaces
            .iter()
            .flat_map(|ws| ws.sessions.iter())
            .filter_map(|s| s.tmux_session_name.as_deref())
            .collect();

        let sessions_output = String::from_utf8_lossy(&output.stdout);
        let mut other_sessions = Vec::new();
        let mut ssh_sessions = Vec::new();

        for line in sessions_output.lines() {
            let parts: Vec<&str> = line.split(':').collect();
            if parts.len() >= 3 {
                // Session name may contain colons, so reconstruct from all parts except last two
                let name = parts[..parts.len() - 2].join(":");

                // Skip shell sessions (ainb-ws-*, ainb-sh-*, ainb-shell-*)
                if name.starts_with("ainb-ws-")
                    || name.starts_with("ainb-sh-")
                    || name.starts_with("ainb-shell-")
                {
                    continue;
                }

                let attached = parts[parts.len() - 2] == "1";
                let windows = parts[parts.len() - 1].parse().unwrap_or_else(|e| {
                    warn!(
                        "Failed to parse window count for tmux session '{}': {}. Defaulting to 1.",
                        name, e
                    );
                    1
                });

                // SSH sessions (ssh-*) go to the SSH Sessions section
                if name.starts_with("ssh-") {
                    // Parse SSH session name to extract target info
                    // Format: ssh-{host}-{port} or ssh-{host}-{user}-{port}
                    let mut ssh_session = crate::models::Session::new_ssh_session(
                        name.clone(),
                        crate::models::SshTarget::default(),
                    );
                    ssh_session.tmux_session_name = Some(name.clone());
                    ssh_session.is_attached = attached;
                    ssh_session.status = if attached {
                        crate::models::SessionStatus::Running
                    } else {
                        crate::models::SessionStatus::Idle
                    };

                    // Try to parse host info from session name
                    // Expected format: ssh-{host}-{port} or ssh-{host}-{user}-{port}
                    let parts: Vec<&str> =
                        name.strip_prefix("ssh-").unwrap_or(&name).split('-').collect();
                    if parts.len() >= 2 {
                        // Last part should be port or timestamp
                        if let Ok(port) = parts[parts.len() - 1].parse::<u16>() {
                            let host = parts[..parts.len() - 1].join("-");
                            ssh_session.ssh_target =
                                Some(crate::models::SshTarget::new(host).with_port(port));
                        } else {
                            // Couldn't parse port, use name as-is
                            let host = parts.join("-");
                            ssh_session.ssh_target = Some(crate::models::SshTarget::new(host));
                        }
                    }

                    // Restore display_name from persistent store
                    if let Some(preserved_name) = self.ssh_display_name_store.get(&name) {
                        ssh_session.display_name = Some(preserved_name.clone());
                    }

                    ssh_sessions.push(ssh_session);
                    continue;
                }

                // For tmux_ sessions, check if they're orphaned
                if name.starts_with("tmux_") {
                    // Skip if this session was successfully matched to a workspace
                    if matched_tmux_names.contains(name.as_str()) {
                        continue;
                    }

                    // Check if we have metadata for this session
                    if let Some(metadata) = session_store.find_by_tmux_name(&name) {
                        // If worktree exists, session should have been discovered by normal flow
                        // If we're here, something went wrong - show as orphaned
                        if metadata.worktree_path.exists() {
                            debug!(
                                "tmux_ session {} has valid worktree but wasn't matched - adding to Other",
                                name
                            );
                        } else {
                            debug!(
                                "tmux_ session {} is orphaned (worktree deleted) - adding to Other",
                                name
                            );
                        }
                    } else {
                        debug!(
                            "tmux_ session {} not in sessions.json - adding to Other",
                            name
                        );
                    }
                    // Fall through to add as "other" session
                }

                other_sessions.push(OtherTmuxSession::new(name, attached, windows));
            }
        }

        info!(
            "Discovered {} other tmux sessions and {} SSH sessions",
            other_sessions.len(),
            ssh_sessions.len()
        );
        self.other_tmux_sessions = other_sessions;
        let live_other_names: HashSet<String> = self
            .other_tmux_sessions
            .iter()
            .map(|session| session.name.clone())
            .collect();
        self.selected_other_tmux_sessions
            .retain(|name| live_other_names.contains(name));
        self.ssh_sessions = ssh_sessions;
    }

    /// Auto-detect workspace shell sessions from tmux
    /// Finds ainb-ws-* sessions and matches them to workspaces
    pub async fn auto_detect_workspace_shells(&mut self) {
        use crate::models::{ShellSession, ShellSessionStatus};
        use tokio::process::Command;

        info!("Auto-detecting workspace shell sessions from tmux");

        // Get all tmux sessions with format: name:path
        // Use pane_current_path to get the current directory of the active pane
        // Note: #{...} is tmux format syntax, not Rust format
        #[allow(clippy::literal_string_with_formatting_args)]
        let tmux_format = "#{session_name}:#{pane_current_path}";
        let output =
            match Command::new("tmux").args(["list-sessions", "-F", tmux_format]).output().await {
                Ok(o) => o,
                Err(e) => {
                    debug!("Failed to list tmux sessions for shell detection: {}", e);
                    return;
                }
            };

        if !output.status.success() {
            debug!("No tmux sessions found for shell detection");
            return;
        }

        let sessions_output = String::from_utf8_lossy(&output.stdout);
        let mut detected_count = 0;

        for line in sessions_output.lines() {
            // Find the last colon to split session name from path
            // (session names can contain colons, paths typically don't at the start)
            if let Some(colon_pos) = line.rfind(':') {
                let session_name = &line[..colon_pos];
                let session_path = &line[colon_pos + 1..];

                // Only process ainb-ws-* sessions (workspace shells)
                if !session_name.starts_with("ainb-ws-") {
                    continue;
                }

                let session_path = std::path::PathBuf::from(session_path);

                // Try to match to a workspace
                // First, try exact path match
                // Then, try parent directory match (for worktree subdirectories)
                let mut matched_workspace_idx = None;

                for (idx, workspace) in self.workspaces.iter().enumerate() {
                    // Skip workspaces that already have a shell session
                    if workspace.shell_session.is_some() {
                        continue;
                    }

                    // Exact match
                    if workspace.path == session_path {
                        matched_workspace_idx = Some(idx);
                        break;
                    }

                    // Check if session path is a subdirectory of workspace
                    if session_path.starts_with(&workspace.path) {
                        matched_workspace_idx = Some(idx);
                        break;
                    }

                    // Check if workspace is a subdirectory of session path
                    // (e.g., shell opened in parent directory)
                    if workspace.path.starts_with(&session_path) {
                        matched_workspace_idx = Some(idx);
                        break;
                    }
                }

                if let Some(idx) = matched_workspace_idx {
                    // Create a ShellSession for this detected session
                    let shell = ShellSession {
                        id: uuid::Uuid::new_v4(),
                        name: format!("🐚 {}", self.workspaces[idx].name),
                        tmux_session_name: session_name.to_string(),
                        workspace_path: self.workspaces[idx].path.clone(),
                        working_dir: session_path.clone(),
                        created_at: chrono::Utc::now(),
                        last_accessed: chrono::Utc::now(),
                        status: ShellSessionStatus::Running,
                        preview_content: None,
                    };

                    info!(
                        "Auto-detected shell session '{}' for workspace '{}'",
                        session_name, self.workspaces[idx].name
                    );

                    self.workspaces[idx].set_shell_session(shell);
                    detected_count += 1;
                } else {
                    debug!(
                        "Could not match tmux session '{}' (path: {:?}) to any workspace",
                        session_name, session_path
                    );
                }
            }
        }

        if detected_count > 0 {
            info!("Auto-detected {} workspace shell sessions", detected_count);
        }
    }

    pub fn load_mock_data(&mut self) {
        let mut workspace1 = Workspace::new(
            "project1".to_string(),
            "/Users/user/projects/project1".into(),
        );

        let mut session1 = Session::new(
            "fix-auth".to_string(),
            workspace1.path.to_string_lossy().to_string(),
        );
        session1.set_status(crate::models::SessionStatus::Running);
        session1.git_changes.added = 42;
        session1.git_changes.deleted = 13;

        let mut session2 = Session::new(
            "add-feature".to_string(),
            workspace1.path.to_string_lossy().to_string(),
        );
        session2.set_status(crate::models::SessionStatus::Stopped);

        let mut session3 = Session::new(
            "debug-issue".to_string(),
            workspace1.path.to_string_lossy().to_string(),
        );
        session3.set_status(crate::models::SessionStatus::Error(
            "Container failed to start".to_string(),
        ));

        workspace1.add_session(session1);
        workspace1.add_session(session2);
        workspace1.add_session(session3);

        let mut workspace2 = Workspace::new(
            "project2".to_string(),
            "/Users/user/projects/project2".into(),
        );

        let mut session4 = Session::new(
            "refactor-api".to_string(),
            workspace2.path.to_string_lossy().to_string(),
        );
        session4.set_status(crate::models::SessionStatus::Running);
        session4.git_changes.modified = 7;

        workspace2.add_session(session4);

        self.workspaces.push(workspace1);
        self.workspaces.push(workspace2);

        // Reset selection state before setting new selection
        self.selected_workspace_index = None;
        self.selected_session_index = None;
        self.shell_selected = false;
        self.selected_ssh_session_index = None;
        self.selected_other_tmux_index = None;

        if !self.workspaces.is_empty() {
            self.selected_workspace_index = Some(0);
            if !self.workspaces[0].sessions.is_empty() {
                self.selected_session_index = Some(0);
            } else if self.workspaces[0].shell_session.is_some() {
                self.shell_selected = true;
            }
        }
    }

    /// Load a large dataset to simulate the 353 repository scenario
    pub fn load_large_mock_data(&mut self) {
        // Load normal mock data first
        self.load_mock_data();

        // Add many more workspaces to simulate large dataset
        for i in 3..=200 {
            let workspace = Workspace::new(
                format!("test-project-{:03}", i),
                format!("/Users/user/projects/test-project-{:03}", i).into(),
            );
            self.workspaces.push(workspace);
        }

        info!(
            "Loaded large mock dataset with {} workspaces",
            self.workspaces.len()
        );
    }

    pub fn selected_session(&self) -> Option<&Session> {
        let workspace_idx = self.selected_workspace_index?;
        let session_idx = self.selected_session_index?;
        self.workspaces.get(workspace_idx)?.sessions.get(session_idx)
    }

    /// Toggle multi-select for the currently highlighted session
    pub fn toggle_select_session(&mut self) {
        if let Some(session) = self.selected_session() {
            let id = session.id;
            if self.selected_sessions.contains(&id) {
                self.selected_sessions.remove(&id);
            } else {
                self.selected_sessions.insert(id);
            }
        } else if self.is_other_tmux_selected() {
            self.toggle_select_other_tmux_session();
        }
    }

    /// Toggle multi-select for the currently highlighted "Other tmux" session.
    pub fn toggle_select_other_tmux_session(&mut self) {
        if let Some(session) = self.selected_other_tmux_session() {
            let name = session.name.clone();
            if self.selected_other_tmux_sessions.contains(&name) {
                self.selected_other_tmux_sessions.remove(&name);
            } else {
                self.selected_other_tmux_sessions.insert(name);
            }
        }
    }

    pub fn selected_shell_session(&self) -> Option<&crate::models::ShellSession> {
        if !self.shell_selected {
            return None;
        }
        let workspace_idx = self.selected_workspace_index?;
        self.workspaces.get(workspace_idx)?.shell_session.as_ref()
    }

    pub fn selected_workspace(&self) -> Option<&Workspace> {
        let workspace_idx = self.selected_workspace_index?;
        self.workspaces.get(workspace_idx)
    }

    /// Every attachable leaf row in the *current* render order.
    /// Numbers shown next to sessions are 1-based positions into this Vec —
    /// recomputed on every render, so reordering or filtering refreshes them.
    pub fn attachable_items_in_order(&self) -> Vec<AttachableRef> {
        let mut out = Vec::new();

        for (workspace_idx, workspace) in self.workspaces.iter().enumerate() {
            let is_selected_workspace = self.selected_workspace_index == Some(workspace_idx);
            let is_expanded = is_selected_workspace || self.expand_all_workspaces;

            // Match session_list: workspaces with no visible content are hidden
            // entirely, and collapsed workspaces don't contribute their leaves.
            let any_visible = workspace.sessions.iter().any(|s| self.session_passes_filter(s))
                || workspace.shell_session.is_some();
            if !any_visible || !is_expanded {
                continue;
            }

            for (session_idx, session) in workspace.sessions.iter().enumerate() {
                if !self.session_passes_filter(session) {
                    continue;
                }
                out.push(AttachableRef::WorkspaceSession {
                    workspace_idx,
                    session_idx,
                });
            }

            if workspace.shell_session.is_some() {
                out.push(AttachableRef::WorkspaceShell { workspace_idx });
            }
        }

        if !self.ssh_sessions.is_empty() && self.ssh_sessions_expanded {
            for ssh_idx in 0..self.ssh_sessions.len() {
                out.push(AttachableRef::SshSession { ssh_idx });
            }
        }

        if !self.other_tmux_sessions.is_empty() && self.other_tmux_expanded {
            for other_idx in 0..self.other_tmux_sessions.len() {
                out.push(AttachableRef::OtherTmux { other_idx });
            }
        }

        out
    }

    /// Move the active selection to the referenced attachable item,
    /// clearing the other section selectors so the exclusive-selection
    /// invariant holds.
    pub fn select_attachable(&mut self, target: AttachableRef) {
        match target {
            AttachableRef::WorkspaceSession {
                workspace_idx,
                session_idx,
            } => {
                self.selected_workspace_index = Some(workspace_idx);
                self.selected_session_index = Some(session_idx);
                self.shell_selected = false;
                self.selected_ssh_session_index = None;
                self.selected_other_tmux_index = None;
            }
            AttachableRef::WorkspaceShell { workspace_idx } => {
                self.selected_workspace_index = Some(workspace_idx);
                self.selected_session_index = None;
                self.shell_selected = true;
                self.selected_ssh_session_index = None;
                self.selected_other_tmux_index = None;
            }
            AttachableRef::SshSession { ssh_idx } => {
                self.selected_workspace_index = None;
                self.selected_session_index = None;
                self.shell_selected = false;
                self.selected_other_tmux_index = None;
                self.selected_ssh_session_index = Some(ssh_idx);
            }
            AttachableRef::OtherTmux { other_idx } => {
                self.selected_workspace_index = None;
                self.selected_session_index = None;
                self.shell_selected = false;
                self.selected_ssh_session_index = None;
                self.selected_other_tmux_index = Some(other_idx);
            }
        }
    }

    pub fn session_list_row_at_mouse(&self, x: u16, y: u16) -> Option<SessionListRowTarget> {
        let row_index = self.sessions_pane_state.row_index_at(x, y)?;
        self.session_list_row_target(row_index)
    }

    pub fn select_session_list_row(&mut self, target: SessionListRowTarget) {
        self.focused_pane = FocusedPane::Sessions;

        match target {
            SessionListRowTarget::WorkspaceHeader { workspace_idx } => {
                self.selected_workspace_index = Some(workspace_idx);
                self.selected_session_index = None;
                self.shell_selected = false;
                self.selected_ssh_session_index = None;
                self.selected_other_tmux_index = None;
            }
            SessionListRowTarget::SshHeader => {
                self.selected_workspace_index = None;
                self.selected_session_index = None;
                self.shell_selected = false;
                self.selected_other_tmux_index = None;
                self.selected_ssh_session_index = None;
                self.ssh_sessions_expanded = !self.ssh_sessions_expanded;
            }
            SessionListRowTarget::OtherTmuxHeader => {
                self.selected_workspace_index = None;
                self.selected_session_index = None;
                self.shell_selected = false;
                self.selected_ssh_session_index = None;
                self.selected_other_tmux_index = None;
                self.other_tmux_expanded = !self.other_tmux_expanded;
            }
            SessionListRowTarget::Attachable(target) => {
                self.select_attachable(target);
                if matches!(target, AttachableRef::WorkspaceSession { .. }) {
                    self.queue_logs_fetch();
                }
            }
        }
    }

    /// Handle mouse-wheel or trackpad scrolling over the sessions screen.
    ///
    /// Returns true when the scroll was consumed by session-list navigation.
    /// Returns false when the caller should preserve the existing live-log
    /// scroll behavior.
    pub fn scroll_session_list_by_mouse(
        &mut self,
        x: u16,
        y: u16,
        is_down: bool,
        steps: usize,
    ) -> bool {
        if self.current_screen != screen_ids::SESSION_LIST || self.help_visible {
            return false;
        }

        if self.sessions_pane_state.contains_preview_point(x, y) {
            self.focused_pane = FocusedPane::LiveLogs;
            return false;
        }

        let over_sessions = self.sessions_pane_state.contains_sessions_point(x, y)
            && !self.sessions_pane_state.collapsed;
        let should_scroll_sessions =
            over_sessions || matches!(self.focused_pane, FocusedPane::Sessions);

        if !should_scroll_sessions {
            return false;
        }

        self.focused_pane = FocusedPane::Sessions;
        for _ in 0..steps.max(1) {
            if is_down {
                self.next_session();
            } else {
                self.previous_session();
            }
        }
        self.last_preview_update = None;
        true
    }

    pub fn session_list_row_target(&self, row_index: usize) -> Option<SessionListRowTarget> {
        let mut current_row = 0usize;

        for (workspace_idx, workspace) in self.workspaces.iter().enumerate() {
            let is_selected_workspace = self.selected_workspace_index == Some(workspace_idx);
            let is_expanded = is_selected_workspace || self.expand_all_workspaces;

            let visible_sessions: Vec<(usize, &Session)> = workspace
                .sessions
                .iter()
                .enumerate()
                .filter(|(_, s)| self.session_passes_filter(s))
                .collect();
            let total_count =
                visible_sessions.len() + usize::from(workspace.shell_session.is_some());
            if total_count == 0 {
                continue;
            }

            if current_row == row_index {
                return Some(SessionListRowTarget::WorkspaceHeader { workspace_idx });
            }
            current_row += 1;

            if is_expanded {
                for (session_idx, _) in visible_sessions {
                    if current_row == row_index {
                        return Some(SessionListRowTarget::Attachable(
                            AttachableRef::WorkspaceSession {
                                workspace_idx,
                                session_idx,
                            },
                        ));
                    }
                    current_row += 1;
                }

                if workspace.shell_session.is_some() {
                    if current_row == row_index {
                        return Some(SessionListRowTarget::Attachable(
                            AttachableRef::WorkspaceShell { workspace_idx },
                        ));
                    }
                    current_row += 1;
                }
            }
        }

        if !self.ssh_sessions.is_empty() {
            if current_row > 0 {
                if current_row == row_index {
                    return None;
                }
                current_row += 1;
            }

            if current_row == row_index {
                return Some(SessionListRowTarget::SshHeader);
            }
            current_row += 1;

            if self.ssh_sessions_expanded {
                for ssh_idx in 0..self.ssh_sessions.len() {
                    if current_row == row_index {
                        return Some(SessionListRowTarget::Attachable(
                            AttachableRef::SshSession { ssh_idx },
                        ));
                    }
                    current_row += 1;
                }
            }
        }

        if !self.other_tmux_sessions.is_empty() {
            if current_row > 0 {
                if current_row == row_index {
                    return None;
                }
                current_row += 1;
            }

            if current_row == row_index {
                return Some(SessionListRowTarget::OtherTmuxHeader);
            }
            current_row += 1;

            if self.other_tmux_expanded {
                for other_idx in 0..self.other_tmux_sessions.len() {
                    if current_row == row_index {
                        return Some(SessionListRowTarget::Attachable(AttachableRef::OtherTmux {
                            other_idx,
                        }));
                    }
                    current_row += 1;
                }
            }
        }

        None
    }

    pub fn next_session(&mut self) {
        // Check if we're in the "Other tmux" section
        if self.selected_other_tmux_index.is_some() {
            // Navigate within other tmux sessions
            let current = self.selected_other_tmux_index.unwrap_or(0);
            if current + 1 < self.other_tmux_sessions.len() {
                self.selected_other_tmux_index = Some(current + 1);
            }
            // At the end - stay at last item (no wrap)
            return;
        }

        // Check if we're in the "SSH Sessions" section
        if self.selected_ssh_session_index.is_some() {
            // Navigate within SSH sessions
            let current = self.selected_ssh_session_index.unwrap_or(0);
            if current + 1 < self.ssh_sessions.len() {
                self.selected_ssh_session_index = Some(current + 1);
            } else if !self.other_tmux_sessions.is_empty() {
                // At end of SSH sessions - move to "Other tmux"
                self.selected_ssh_session_index = None;
                self.selected_other_tmux_index = Some(0);
            }
            // Else: stay at last SSH session (no wrap)
            return;
        }

        // If nothing is selected, try SSH sessions first, then "Other tmux"
        if self.selected_workspace_index.is_none() {
            if !self.ssh_sessions.is_empty() {
                self.selected_ssh_session_index = Some(0);
                return;
            } else if !self.other_tmux_sessions.is_empty() {
                self.selected_other_tmux_index = Some(0);
                return;
            }
        }

        if let Some(workspace_idx) = self.selected_workspace_index {
            if let Some(workspace) = self.workspaces.get(workspace_idx) {
                // Currently on shell session?
                if self.shell_selected {
                    // Shell is last in workspace - try next workspace first
                    self.shell_selected = false;
                    self.move_to_next_workspace_first_item(workspace_idx);
                    return;
                }

                // Currently in regular sessions. Find the next *visible*
                // session (skipping any that the active filter hides) so j/k
                // doesn't land on a row that isn't rendered.
                if let Some(session_idx) = self.selected_session_index {
                    let next_visible = workspace
                        .sessions
                        .iter()
                        .enumerate()
                        .skip(session_idx + 1)
                        .find(|(_, s)| self.session_passes_filter(s))
                        .map(|(i, _)| i);
                    if let Some(next_idx) = next_visible {
                        self.selected_session_index = Some(next_idx);
                        self.queue_logs_fetch();
                    } else if workspace.shell_session.is_some() {
                        self.selected_session_index = None;
                        self.shell_selected = true;
                    } else {
                        self.move_to_next_workspace_first_item(workspace_idx);
                    }
                } else {
                    let first_visible =
                        workspace.sessions.iter().position(|s| self.session_passes_filter(s));
                    if let Some(first_idx) = first_visible {
                        self.selected_session_index = Some(first_idx);
                        self.queue_logs_fetch();
                    } else if workspace.shell_session.is_some() {
                        self.shell_selected = true;
                    }
                }
            }
        }
    }

    /// Helper: Move to next workspace's first session/shell, or SSH sessions, or Other tmux
    fn move_to_next_workspace_first_item(&mut self, current_workspace_idx: usize) {
        // Try to find next workspace with content
        for next_idx in (current_workspace_idx + 1)..self.workspaces.len() {
            if let Some(next_ws) = self.workspaces.get(next_idx) {
                if !next_ws.sessions.is_empty() {
                    // Next workspace has sessions
                    self.selected_workspace_index = Some(next_idx);
                    self.selected_session_index = Some(0);
                    self.shell_selected = false;
                    self.queue_logs_fetch();
                    return;
                } else if next_ws.shell_session.is_some() {
                    // Next workspace only has shell
                    self.selected_workspace_index = Some(next_idx);
                    self.selected_session_index = None;
                    self.shell_selected = true;
                    return;
                }
                // Empty workspace - skip it
            }
        }

        // No more workspaces - move to SSH sessions if available
        if !self.ssh_sessions.is_empty() {
            self.selected_workspace_index = None;
            self.selected_session_index = None;
            self.shell_selected = false;
            self.selected_ssh_session_index = Some(0);
            return;
        }

        // No SSH sessions - move to "Other tmux" if available
        if !self.other_tmux_sessions.is_empty() {
            self.selected_workspace_index = None;
            self.selected_session_index = None;
            self.shell_selected = false;
            self.selected_other_tmux_index = Some(0);
        }
        // Else: stay at current position (no wrap)
    }

    pub fn previous_session(&mut self) {
        // Check if we're in the "Other tmux" section
        if let Some(other_idx) = self.selected_other_tmux_index {
            if other_idx > 0 {
                // Move up within other tmux sessions
                self.selected_other_tmux_index = Some(other_idx - 1);
            } else {
                // At first other_tmux session - move to SSH sessions if available
                self.selected_other_tmux_index = None;
                if !self.ssh_sessions.is_empty() {
                    self.selected_ssh_session_index = Some(self.ssh_sessions.len() - 1);
                } else if !self.workspaces.is_empty() {
                    // No SSH sessions - move back to workspaces
                    let last_workspace_idx = self.workspaces.len() - 1;
                    let workspace = &self.workspaces[last_workspace_idx];
                    self.selected_workspace_index = Some(last_workspace_idx);

                    // Go to shell session if exists, else last regular session
                    if workspace.shell_session.is_some() {
                        self.selected_session_index = None;
                        self.shell_selected = true;
                    } else if !workspace.sessions.is_empty() {
                        self.selected_session_index = Some(workspace.sessions.len() - 1);
                        self.shell_selected = false;
                        self.queue_logs_fetch();
                    }
                }
            }
            return;
        }

        // Check if we're in the "SSH Sessions" section
        if let Some(ssh_idx) = self.selected_ssh_session_index {
            if ssh_idx > 0 {
                // Move up within SSH sessions
                self.selected_ssh_session_index = Some(ssh_idx - 1);
            } else {
                // At first SSH session - move back to workspaces
                self.selected_ssh_session_index = None;
                if !self.workspaces.is_empty() {
                    let last_workspace_idx = self.workspaces.len() - 1;
                    let workspace = &self.workspaces[last_workspace_idx];
                    self.selected_workspace_index = Some(last_workspace_idx);

                    // Go to shell session if exists, else last regular session
                    if workspace.shell_session.is_some() {
                        self.selected_session_index = None;
                        self.shell_selected = true;
                    } else if !workspace.sessions.is_empty() {
                        self.selected_session_index = Some(workspace.sessions.len() - 1);
                        self.shell_selected = false;
                        self.queue_logs_fetch();
                    }
                }
            }
            return;
        }

        // If nothing is selected, try SSH sessions, then "Other tmux"
        if self.selected_workspace_index.is_none() {
            if !self.ssh_sessions.is_empty() {
                self.selected_ssh_session_index = Some(self.ssh_sessions.len() - 1);
                return;
            } else if !self.other_tmux_sessions.is_empty() {
                self.selected_other_tmux_index = Some(self.other_tmux_sessions.len() - 1);
                return;
            }
        }

        if let Some(workspace_idx) = self.selected_workspace_index {
            if let Some(workspace) = self.workspaces.get(workspace_idx) {
                // Currently on shell session?
                if self.shell_selected {
                    if !workspace.sessions.is_empty() {
                        // Go back to last regular session
                        self.shell_selected = false;
                        self.selected_session_index = Some(workspace.sessions.len() - 1);
                        self.queue_logs_fetch();
                    }
                    // Else: stay at shell session (it's the only item)
                    return;
                }

                // Currently in regular sessions. Find the previous *visible*
                // session under the active filter so k doesn't land on a
                // hidden row.
                if let Some(session_idx) = self.selected_session_index {
                    let prev_visible = workspace
                        .sessions
                        .iter()
                        .enumerate()
                        .take(session_idx)
                        .rev()
                        .find(|(_, s)| self.session_passes_filter(s))
                        .map(|(i, _)| i);
                    if let Some(prev_idx) = prev_visible {
                        self.selected_session_index = Some(prev_idx);
                        self.queue_logs_fetch();
                    } else {
                        // At first session - try to move to previous workspace's last item
                        if workspace_idx > 0 {
                            let prev_idx = workspace_idx - 1;
                            self.selected_workspace_index = Some(prev_idx);
                            // Select last item in previous workspace (shell or last session)
                            if let Some(prev_ws) = self.workspaces.get(prev_idx) {
                                if prev_ws.shell_session.is_some() {
                                    self.shell_selected = true;
                                    self.selected_session_index = None;
                                } else if !prev_ws.sessions.is_empty() {
                                    self.selected_session_index = Some(prev_ws.sessions.len() - 1);
                                    self.shell_selected = false;
                                    self.queue_logs_fetch();
                                } else {
                                    // Empty workspace - select workspace header
                                    self.selected_session_index = None;
                                    self.shell_selected = false;
                                }
                            }
                        }
                        // else: at first workspace, first session - stay (no wrap)
                    }
                }
            }
        }
    }

    pub fn next_workspace(&mut self) {
        if !self.workspaces.is_empty() {
            let current = self.selected_workspace_index.unwrap_or(0);
            self.selected_workspace_index = Some((current + 1) % self.workspaces.len());
            self.selected_session_index =
                if !self.workspaces[self.selected_workspace_index.unwrap()].sessions.is_empty() {
                    Some(0)
                } else {
                    None
                };
            // Queue container logs fetch for the newly selected session
            self.queue_logs_fetch();
        }
    }

    pub fn previous_workspace(&mut self) {
        if !self.workspaces.is_empty() {
            let current = self.selected_workspace_index.unwrap_or(0);
            self.selected_workspace_index = Some(if current == 0 {
                self.workspaces.len() - 1
            } else {
                current - 1
            });
            self.selected_session_index =
                if !self.workspaces[self.selected_workspace_index.unwrap()].sessions.is_empty() {
                    Some(0)
                } else {
                    None
                };
            // Queue container logs fetch for the newly selected session
            self.queue_logs_fetch();
        }
    }

    pub fn toggle_help(&mut self) {
        self.help_visible = !self.help_visible;
    }

    pub fn toggle_expand_all_workspaces(&mut self) {
        self.expand_all_workspaces = !self.expand_all_workspaces;
    }

    /// Cycle the session-status filter (Shift+F): All → ActiveOnly → StoppedOnly → All.
    /// Resets the session selection so it doesn't point to a now-hidden row.
    pub fn cycle_session_filter(&mut self) {
        self.session_filter = self.session_filter.next();
        // Selection indices are positional over the *displayed* list. Resetting
        // to the first session of the first workspace is simplest and matches
        // what `load_real_workspaces` already does after a refresh.
        self.selected_session_index = None;
        self.shell_selected = false;
        if let Some(idx) = self.selected_workspace_index {
            // Clamp workspace index too, in case the active workspace gets
            // hidden (no sessions match the filter and no shell).
            if self.workspaces.get(idx).map(|w| {
                w.sessions.iter().any(|s| self.session_passes_filter(s))
                    || w.shell_session.is_some()
            }) != Some(true)
            {
                let new_idx = self.workspaces.iter().position(|w| {
                    w.sessions.iter().any(|s| self.session_passes_filter(s))
                        || w.shell_session.is_some()
                });
                self.selected_workspace_index = new_idx;
            }
        }
        self.last_preview_update = None;
    }

    /// Predicate used by both rendering and counts so the displayed list and
    /// the workspace-header `(N)` count never drift apart.
    ///
    /// The filter only applies to Interactive sessions (the ones for which
    /// Stopped is meaningful). Boss-mode sessions and other variants pass
    /// through regardless.
    pub fn session_passes_filter(&self, session: &crate::models::Session) -> bool {
        use crate::models::{SessionMode, SessionStatus};
        if !matches!(session.mode, SessionMode::Interactive) {
            return true;
        }
        match self.session_filter {
            SessionFilter::All => true,
            SessionFilter::ActiveOnly => !matches!(session.status, SessionStatus::Stopped),
            SessionFilter::StoppedOnly => matches!(session.status, SessionStatus::Stopped),
        }
    }

    /// Toggle the expand/collapse state of the "Other tmux" section
    pub fn toggle_other_tmux_expanded(&mut self) {
        self.other_tmux_expanded = !self.other_tmux_expanded;
    }

    /// Get the currently selected other tmux session, if any
    pub fn selected_other_tmux_session(&self) -> Option<&crate::models::OtherTmuxSession> {
        self.selected_other_tmux_index.and_then(|idx| self.other_tmux_sessions.get(idx))
    }

    /// Selected "Other tmux" names in current render order.
    pub fn selected_other_tmux_names_in_order(&self) -> Vec<String> {
        self.other_tmux_sessions
            .iter()
            .filter(|session| self.selected_other_tmux_sessions.contains(&session.name))
            .map(|session| session.name.clone())
            .collect()
    }

    /// Check if the selection is in the "Other tmux" section
    pub fn is_other_tmux_selected(&self) -> bool {
        self.selected_other_tmux_index.is_some() && self.selected_workspace_index.is_none()
    }

    /// Start rename mode for the selected "Other tmux" session
    pub fn start_other_tmux_rename(&mut self) {
        if let Some(session) = self.selected_other_tmux_session() {
            self.other_tmux_rename_buffer = session.name.clone();
            self.other_tmux_rename_mode = true;
        }
    }

    /// Cancel rename mode
    pub fn cancel_other_tmux_rename(&mut self) {
        self.other_tmux_rename_mode = false;
        self.other_tmux_rename_buffer.clear();
    }

    /// Add a character to the rename buffer
    pub fn other_tmux_rename_char(&mut self, c: char) {
        if self.other_tmux_rename_mode {
            self.other_tmux_rename_buffer.push(c);
        }
    }

    /// Remove a character from the rename buffer
    pub fn other_tmux_rename_backspace(&mut self) {
        if self.other_tmux_rename_mode {
            self.other_tmux_rename_buffer.pop();
        }
    }

    /// Execute the rename using tmux rename-session
    pub async fn confirm_other_tmux_rename(&mut self) -> Result<(), String> {
        if !self.other_tmux_rename_mode {
            return Err("Not in rename mode".to_string());
        }

        let new_name = self.other_tmux_rename_buffer.trim().to_string();
        if new_name.is_empty() {
            return Err("Name cannot be empty".to_string());
        }

        if let Some(idx) = self.selected_other_tmux_index {
            if let Some(session) = self.other_tmux_sessions.get(idx) {
                let old_name = session.name.clone();

                // Sanitize new name (tmux compatible)
                let sanitized_name = new_name.replace(' ', "_").replace('.', "_").replace(':', "_");

                // Execute tmux rename-session
                let output = tokio::process::Command::new("tmux")
                    .args(["rename-session", "-t", &old_name, &sanitized_name])
                    .output()
                    .await
                    .map_err(|e| e.to_string())?;

                if output.status.success() {
                    // Exit rename mode
                    self.other_tmux_rename_mode = false;
                    self.other_tmux_rename_buffer.clear();

                    // Reload other tmux sessions to reflect the change
                    self.load_other_tmux_sessions().await;
                    Ok(())
                } else {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    Err(format!("tmux rename-session failed: {}", stderr))
                }
            } else {
                Err("No session selected".to_string())
            }
        } else {
            Err("No session selected".to_string())
        }
    }

    // ==================== SSH Sessions Section Helpers ====================

    /// Toggle the expand/collapse state of the "SSH Sessions" section
    pub fn toggle_ssh_sessions_expanded(&mut self) {
        self.ssh_sessions_expanded = !self.ssh_sessions_expanded;
    }

    /// Get the currently selected SSH session, if any
    pub fn selected_ssh_session(&self) -> Option<&crate::models::Session> {
        self.selected_ssh_session_index.and_then(|idx| self.ssh_sessions.get(idx))
    }

    /// Check if the selection is in the "SSH Sessions" section
    pub fn is_ssh_session_selected(&self) -> bool {
        self.selected_ssh_session_index.is_some()
            && self.selected_workspace_index.is_none()
            && self.selected_other_tmux_index.is_none()
    }

    /// Start rename mode for the selected SSH session
    pub fn start_ssh_session_rename(&mut self) {
        if let Some(session) = self.selected_ssh_session() {
            // Start with existing display_name or ssh_target display
            self.ssh_session_rename_buffer = session.display_name.clone().unwrap_or_else(|| {
                session
                    .ssh_target
                    .as_ref()
                    .map(|t| t.display_name())
                    .unwrap_or_else(|| session.name.clone())
            });
            self.ssh_session_rename_mode = true;
        }
    }

    /// Cancel SSH session rename mode
    pub fn cancel_ssh_session_rename(&mut self) {
        self.ssh_session_rename_mode = false;
        self.ssh_session_rename_buffer.clear();
    }

    /// Add a character to the SSH session rename buffer
    pub fn ssh_session_rename_char(&mut self, c: char) {
        if self.ssh_session_rename_mode {
            self.ssh_session_rename_buffer.push(c);
        }
    }

    /// Remove a character from the SSH session rename buffer
    pub fn ssh_session_rename_backspace(&mut self) {
        if self.ssh_session_rename_mode {
            self.ssh_session_rename_buffer.pop();
        }
    }

    /// Confirm the SSH session rename (updates display_name in memory)
    pub fn confirm_ssh_session_rename(&mut self) {
        if !self.ssh_session_rename_mode {
            return;
        }

        let new_name = self.ssh_session_rename_buffer.trim().to_string();
        if let Some(idx) = self.selected_ssh_session_index {
            if let Some(session) = self.ssh_sessions.get_mut(idx) {
                // Get tmux session name for persistence key
                let tmux_name = session.tmux_session_name.clone();

                if new_name.is_empty() {
                    // Empty = clear custom name, revert to auto-generated
                    session.display_name = None;
                } else {
                    session.display_name = Some(new_name.clone());
                }

                // Persist to disk
                if let Some(key) = tmux_name {
                    self.ssh_display_name_store.set(key, session.display_name.clone());
                    if let Err(e) = self.ssh_display_name_store.save() {
                        warn!("Failed to save SSH display names: {}", e);
                    }
                }
            }
        }

        self.ssh_session_rename_mode = false;
        self.ssh_session_rename_buffer.clear();
    }

    pub fn toggle_claude_chat(&mut self) {
        if self.current_screen == screen_ids::CLAUDE_CHAT {
            // Close Claude chat popup and return to main view
            self.current_screen = screen_ids::SESSION_LIST.to_string();
            self.claude_chat_visible = false;
        } else {
            // Open Claude chat popup
            self.current_screen = screen_ids::CLAUDE_CHAT.to_string();
            self.claude_chat_visible = true;
        }
    }

    pub fn quit(&mut self) {
        self.should_quit = true;
    }

    pub fn show_delete_confirmation(&mut self, session_id: Uuid) {
        info!(
            "!!! SHOWING DELETE CONFIRMATION DIALOG for session: {}",
            session_id
        );

        // Check for uncommitted changes in worktree (only for non-Shell sessions)
        let warning = self.check_session_uncommitted_warning(session_id);

        self.confirmation_dialog = Some(ConfirmationDialog {
            title: "Delete Session".to_string(),
            message: "Are you sure you want to delete this session? This will stop the container and remove the git worktree.".to_string(),
            confirm_action: ConfirmAction::DeleteSession(session_id),
            selected_option: false, // Default to "No"
            warning,
            options: None,
            selected_index: 0,
        });
    }

    /// Show a tri-option Stop / Delete / Cancel dialog for an interactive session.
    ///
    /// Stop is the default (selected) action: it kills only tmux and keeps the
    /// worktree, sessions.json entry, and `by-session/<uuid>` symlink intact.
    /// Delete maps to the existing destructive flow.
    pub fn show_delete_or_stop_confirmation(&mut self, session_id: Uuid) {
        info!(
            "Showing Stop/Delete/Cancel dialog for session: {}",
            session_id
        );

        let warning = self.check_session_uncommitted_warning(session_id);

        let options = vec![
            DialogOption {
                label: "Stop".to_string(),
                action: ConfirmAction::StopSession(session_id),
            },
            DialogOption {
                label: "Delete".to_string(),
                action: ConfirmAction::DeleteSession(session_id),
            },
            DialogOption {
                label: "Cancel".to_string(),
                action: ConfirmAction::Cancel,
            },
        ];

        self.confirmation_dialog = Some(ConfirmationDialog {
            title: "Stop or Delete Session".to_string(),
            message: "Stop keeps the worktree and resumes later. Delete removes the worktree."
                .to_string(),
            // confirm_action mirrors the default (Stop) so the legacy ConfirmationConfirm
            // handler still has something sensible if it ever runs without options.
            confirm_action: ConfirmAction::StopSession(session_id),
            selected_option: true,
            warning,
            options: Some(options),
            selected_index: 0, // Default = Stop (safe option)
        });
    }

    /// Check if a session's worktree has uncommitted changes.
    /// Returns None for Shell sessions (no dedicated worktree) or if no uncommitted changes.
    fn check_session_uncommitted_warning(&self, session_id: Uuid) -> Option<String> {
        use crate::git::WorktreeManager;
        use crate::models::SessionAgentType;

        // Find the session to check its type
        let session = self.find_session(session_id)?;

        // Skip for Shell sessions - they don't have dedicated worktrees
        if matches!(session.agent_type, SessionAgentType::Shell) {
            return None;
        }

        // Try to check worktree status
        let worktree_manager = WorktreeManager::new().ok()?;
        let count = worktree_manager.uncommitted_file_count(session_id).ok()?;

        if count > 0 {
            Some(format!("⚠️ {} uncommitted file(s) in worktree", count))
        } else {
            None
        }
    }

    /// Show confirmation dialog for killing an "other" tmux session
    pub fn show_kill_other_tmux_confirmation(&mut self, session_name: String) {
        info!(
            "Showing kill confirmation for other tmux session: {}",
            session_name
        );
        self.confirmation_dialog = Some(ConfirmationDialog {
            title: "Kill tmux Session".to_string(),
            message: format!(
                "Are you sure you want to kill tmux session '{}'?",
                session_name
            ),
            confirm_action: ConfirmAction::KillOtherTmux(session_name),
            selected_option: false, // Default to "No"
            warning: None,
            options: None,
            selected_index: 0,
        });
    }

    /// Show confirmation dialog for killing multiple "other" tmux sessions
    pub fn show_kill_other_tmux_sessions_confirmation(&mut self, session_names: Vec<String>) {
        let count = session_names.len();
        info!(
            "Showing kill confirmation for {} other tmux sessions",
            count
        );
        self.confirmation_dialog = Some(ConfirmationDialog {
            title: "Kill tmux Sessions".to_string(),
            message: format!("Are you sure you want to kill {} tmux session(s)?", count),
            confirm_action: ConfirmAction::KillOtherTmuxSessions(session_names),
            selected_option: false,
            warning: Some("This closes all selected external tmux sessions.".to_string()),
            options: None,
            selected_index: 0,
        });
    }

    /// Show confirmation dialog for killing an SSH session (which is a tmux session)
    pub fn show_kill_ssh_session_confirmation(&mut self, session_name: String) {
        // Get the display name if available for a friendlier message
        let display_text = self
            .selected_ssh_session()
            .and_then(|s| s.display_name.clone())
            .unwrap_or_else(|| session_name.clone());

        info!(
            "Showing kill confirmation for SSH session: {} (display: {})",
            session_name, display_text
        );
        self.confirmation_dialog = Some(ConfirmationDialog {
            title: "Kill SSH Session".to_string(),
            message: format!(
                "Are you sure you want to kill SSH session '{}'?",
                display_text
            ),
            confirm_action: ConfirmAction::KillOtherTmux(session_name), // Reuse KillOtherTmux since SSH sessions are tmux sessions
            selected_option: false,                                     // Default to "No"
            warning: None,
            options: None,
            selected_index: 0,
        });
    }

    /// Show confirmation dialog for killing a workspace shell session
    pub fn show_kill_shell_confirmation(&mut self, workspace_index: usize) {
        let shell_name = self
            .workspaces
            .get(workspace_index)
            .and_then(|w| w.shell_session.as_ref())
            .map(|s| s.name.clone())
            .unwrap_or_else(|| "shell".to_string());

        let workspace_name = self
            .workspaces
            .get(workspace_index)
            .map(|w| w.name.clone())
            .unwrap_or_else(|| "workspace".to_string());

        info!(
            "Showing kill confirmation for workspace shell: {} in {}",
            shell_name, workspace_name
        );
        self.confirmation_dialog = Some(ConfirmationDialog {
            title: "Kill Shell Session".to_string(),
            message: format!(
                "Are you sure you want to kill shell '{}' in workspace '{}'?",
                shell_name, workspace_name
            ),
            confirm_action: ConfirmAction::KillWorkspaceShell(workspace_index),
            selected_option: false, // Default to "No"
            warning: None,
            options: None,
            selected_index: 0,
        });
    }

    /// Queue fetching container logs for the currently selected session if needed
    fn queue_logs_fetch(&mut self) {
        // Get session ID without borrowing self
        if let Some(session_id) = self.get_selected_session_id() {
            // Only fetch if we haven't already fetched logs for this session
            if self.last_logs_session_id != Some(session_id) {
                self.pending_async_action = Some(AsyncAction::FetchContainerLogs(session_id));
                self.last_logs_session_id = Some(session_id);
            }
        }
    }

    /// Get the ID of the currently selected session without borrowing self
    pub fn get_selected_session_id(&self) -> Option<Uuid> {
        let workspace_idx = self.selected_workspace_index?;
        let session_idx = self.selected_session_index?;
        self.workspaces.get(workspace_idx)?.sessions.get(session_idx).map(|s| s.id)
    }

    /// Get a reference to the currently selected session
    pub fn get_selected_session(&self) -> Option<&crate::models::Session> {
        let workspace_idx = self.selected_workspace_index?;
        let session_idx = self.selected_session_index?;

        self.workspaces.get(workspace_idx)?.sessions.get(session_idx)
    }

    /// Attach to a container session using docker exec with proper terminal handling
    pub async fn attach_to_container(
        &mut self,
        session_id: Uuid,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use crate::docker::ContainerManager;

        // Find the session to get container ID
        let container_id = self
            .workspaces
            .iter()
            .flat_map(|w| &w.sessions)
            .find(|s| s.id == session_id)
            .and_then(|s| s.container_id.as_ref())
            .cloned();

        if let Some(container_id) = container_id {
            info!(
                "Attaching to container {} for session {}",
                container_id, session_id
            );

            // Check if container is running
            let container_manager = ContainerManager::new().await?;
            let status = container_manager.get_container_status(&container_id).await?;

            match status {
                crate::docker::ContainerStatus::Running => {
                    // Start an interactive bash shell instead of Claude CLI directly
                    // This gives users more flexibility to run claude when needed
                    // Force bash to read .bashrc to load custom session environment
                    let exec_command = vec![
                        "/bin/bash".to_string(),
                        "-l".to_string(), // Login shell to read .bash_profile/.bashrc
                        "-i".to_string(), // Interactive shell
                    ];

                    match container_manager
                        .exec_interactive_blocking(&container_id, exec_command)
                        .await
                    {
                        Ok(_exit_status) => {
                            info!(
                                "Successfully detached from container {} for session {}",
                                container_id, session_id
                            );
                            // The container session has ended, stay in current view
                            Ok(())
                        }
                        Err(e) => {
                            error!("Failed to exec into container {}: {}", container_id, e);
                            Err(format!("Failed to attach to container: {}", e).into())
                        }
                    }
                }
                _ => {
                    warn!(
                        "Cannot attach to container {} - it is not running (status: {:?})",
                        container_id, status
                    );
                    Err(format!("Container is not running (status: {:?})", status).into())
                }
            }
        } else {
            warn!(
                "Cannot attach to session {} - no container ID found",
                session_id
            );
            Err("No container associated with this session".into())
        }
    }

    /// Kill the container for a session (force stop and cleanup)
    pub async fn kill_container(
        &mut self,
        session_id: Uuid,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use crate::docker::ContainerManager;

        // Find the session to get container ID
        let container_id = self
            .workspaces
            .iter()
            .flat_map(|w| &w.sessions)
            .find(|s| s.id == session_id)
            .and_then(|s| s.container_id.as_ref())
            .cloned();

        if let Some(container_id) = container_id {
            info!(
                "Killing container {} for session {}",
                container_id, session_id
            );

            // Clear attached session if we're currently attached to this session
            if self.attached_session_id == Some(session_id) {
                self.attached_session_id = None;
                self.current_screen = crate::app::screens::ids::SESSION_LIST.to_string();
                self.ui_needs_refresh = true;
            }

            let container_manager = ContainerManager::new().await?;

            // Force stop the container
            if let Some(mut session_container) = self.find_session_container_mut(session_id) {
                if let Err(e) = container_manager.stop_container(&mut session_container).await {
                    warn!("Failed to stop container gracefully: {}", e);
                }

                // Force remove the container
                if let Err(e) = container_manager.remove_container(&mut session_container).await {
                    error!("Failed to remove container: {}", e);
                    return Err(format!("Failed to remove container: {}", e).into());
                }

                info!(
                    "Successfully killed and removed container {} for session {}",
                    container_id, session_id
                );
            }

            Ok(())
        } else {
            warn!(
                "Cannot kill container for session {} - no container ID found",
                session_id
            );
            Err("No container associated with this session".into())
        }
    }

    /// Helper method to find a session container by session ID
    fn find_session_container_mut(
        &mut self,
        _session_id: Uuid,
    ) -> Option<&mut crate::docker::SessionContainer> {
        // This is a simplified approach - in a real implementation you'd need to track
        // SessionContainer objects separately or modify the Session model to include them
        None // Placeholder - would need container tracking
    }

    /// Fetch container logs for a session
    pub async fn fetch_container_logs(
        &mut self,
        session_id: Uuid,
    ) -> Result<Vec<String>, Box<dyn std::error::Error>> {
        use crate::docker::ContainerManager;

        // Find the session to get container ID
        let container_id = self
            .workspaces
            .iter()
            .flat_map(|w| &w.sessions)
            .find(|s| s.id == session_id)
            .and_then(|s| s.container_id.as_ref())
            .cloned();

        if let Some(container_id) = container_id {
            let container_manager = ContainerManager::new().await?;
            let logs = container_manager.get_container_logs(&container_id, Some(50)).await?;

            // Update the logs cache
            self.logs.insert(session_id, logs.clone());

            Ok(logs)
        } else {
            // No container ID - return session creation logs if available
            Ok(self
                .logs
                .get(&session_id)
                .cloned()
                .unwrap_or_else(|| vec!["No container associated with this session".to_string()]))
        }
    }

    /// Fetch Claude-specific logs from the container
    pub async fn fetch_claude_logs(
        &mut self,
        session_id: Uuid,
    ) -> Result<String, Box<dyn std::error::Error>> {
        use crate::docker::ContainerManager;

        // Find the session to get container ID and update recent_logs
        let container_id = self
            .workspaces
            .iter_mut()
            .flat_map(|w| &mut w.sessions)
            .find(|s| s.id == session_id)
            .and_then(|s| {
                let id = s.container_id.clone();
                // We'll update recent_logs after fetching
                id
            });

        if let Some(container_id) = container_id {
            let container_manager = ContainerManager::new().await?;
            let logs = container_manager.tail_logs(&container_id, 20).await?;

            // Update the session's recent_logs field
            if let Some(session) = self
                .workspaces
                .iter_mut()
                .flat_map(|w| &mut w.sessions)
                .find(|s| s.id == session_id)
            {
                session.recent_logs = Some(logs.clone());
            }

            Ok(logs)
        } else {
            Ok("No container associated with this session".to_string())
        }
    }

    pub async fn new_session_normal(&mut self) {
        use crate::git::WorkspaceScanner;
        use std::env;

        info!("=== new_session_normal called ===");

        // REMOVED: Auth check moved to Boss mode selection only
        // Interactive mode works without Docker authentication (uses host ~/.claude)
        // Boss mode will check auth when selected during session creation
        info!("Proceeding with session creation (auth deferred to Boss mode selection)");

        // Check if current directory is a git repository
        let current_dir = match env::current_dir() {
            Ok(dir) => {
                info!("Current directory: {:?}", dir);
                dir
            }
            Err(e) => {
                warn!("Failed to get current directory: {}", e);
                return;
            }
        };

        match WorkspaceScanner::validate_workspace(&current_dir) {
            Ok(true) => {
                info!(
                    "Current directory is a valid git repository: {:?}",
                    current_dir
                );
            }
            Ok(false) => {
                warn!(
                    "Current directory is not a git repository: {:?}",
                    current_dir
                );
                info!("Falling back to workspace search");
                // Fall back to workspace search since current directory is not a git repository
                self.start_workspace_search().await;
                return;
            }
            Err(e) => {
                error!("Failed to validate workspace: {}", e);
                info!("Falling back to workspace search due to validation error");
                // Fall back to workspace search on validation error
                self.start_workspace_search().await;
                return;
            }
        }

        // Generate branch name with UUID
        let branch_base = format!(
            "ainb/{}",
            uuid::Uuid::new_v4().to_string().split('-').next().unwrap_or("session")
        );

        // Create new session state for normal new session (NOT current directory mode)
        self.new_session_state = Some(NewSessionState {
            available_repos: vec![current_dir.clone()],
            filtered_repos: vec![(0, current_dir.clone())],
            selected_repo_index: Some(0),
            branch_name: branch_base.clone(),
            step: NewSessionStep::InputBranch,
            ..Default::default()
        });

        self.current_screen = screen_ids::NEW_SESSION.to_string();

        info!(
            "Successfully created normal new session state with branch: {}",
            branch_base
        );
    }

    /// Start new session - shows source selection (Local or Remote)
    pub async fn new_session_with_repo_input(&mut self) {
        info!("Starting new session - showing source selection");

        // Create new session state with SelectSource step (default)
        self.new_session_state = Some(NewSessionState::default());
        self.current_screen = screen_ids::NEW_SESSION.to_string();
        info!("New session state created with SelectSource step");
    }

    /// Validate the repo input (URL or path) and proceed accordingly
    pub async fn validate_repo_source(&mut self) {
        use crate::git::RepoSource;

        let input = if let Some(ref state) = self.new_session_state {
            state.repo_input.trim().to_string()
        } else {
            error!("validate_repo_source called but no new_session_state");
            return;
        };

        if input.is_empty() {
            if let Some(ref mut state) = self.new_session_state {
                state.repo_validation_error =
                    Some("Please enter a repository URL or path".to_string());
            }
            return;
        }

        // Set validating state
        if let Some(ref mut state) = self.new_session_state {
            state.is_validating = true;
            state.repo_validation_error = None;
        }

        // Parse the input
        let source = match RepoSource::from_input(&input) {
            Ok(s) => s,
            Err(e) => {
                if let Some(ref mut state) = self.new_session_state {
                    state.is_validating = false;
                    state.repo_validation_error = Some(e.to_string());
                }
                return;
            }
        };

        info!(
            "Parsed repo source: {:?}, is_remote: {}",
            source,
            source.is_remote()
        );

        if source.is_remote() {
            // Remote URL - try to fetch branches
            self.handle_remote_repo_source(source).await;
        } else {
            // Local path - validate and proceed
            self.handle_local_repo_source(source).await;
        }
    }

    /// Handle a remote repository source (URL)
    async fn handle_remote_repo_source(&mut self, source: RepoSource) {
        use crate::git::RemoteRepoManager;

        let manager = match RemoteRepoManager::new() {
            Ok(m) => m,
            Err(e) => {
                if let Some(ref mut state) = self.new_session_state {
                    state.is_validating = false;
                    state.repo_validation_error =
                        Some(format!("Failed to init repo manager: {}", e));
                }
                return;
            }
        };

        info!("Fetching branches for remote repo...");

        // Try to list branches
        match manager.list_remote_branches(&source) {
            Ok(branches) => {
                info!("Found {} branches", branches.len());
                if let Some(ref mut state) = self.new_session_state {
                    state.repo_source = Some(source);
                    // Initialize filtered_branches with all branches (no filter yet)
                    state.filtered_branches = branches.iter().cloned().enumerate().collect();
                    state.remote_branches = branches;
                    state.branch_filter_text.clear();
                    state.selected_branch_index = 0;
                    state.is_validating = false;
                    state.step = NewSessionStep::SelectBranch;
                }
            }
            Err(crate::git::RemoteRepoError::AuthFailed) => {
                // Auth failed - let user enter branch manually
                warn!("Auth failed for remote repo, allowing manual branch entry");
                if let Some(ref mut state) = self.new_session_state {
                    state.repo_source = Some(source);
                    state.is_validating = false;
                    // Generate a branch name
                    state.branch_name = format!(
                        "ainb/{}",
                        uuid::Uuid::new_v4().to_string().split('-').next().unwrap_or("session")
                    );
                    // Default to main as base branch
                    state.selected_base_branch = Some("main".to_string());
                    state.repo_validation_error = Some(
                        "Auth required. Defaulting to 'main' branch. Change base branch in next step if needed.".to_string()
                    );
                    state.step = NewSessionStep::InputBranch;
                }
            }
            Err(e) => {
                error!("Failed to list branches: {}", e);
                if let Some(ref mut state) = self.new_session_state {
                    state.is_validating = false;
                    state.repo_validation_error = Some(e.to_string());
                }
            }
        }
    }

    /// Handle a local repository source (path)
    async fn handle_local_repo_source(&mut self, source: RepoSource) {
        use crate::git::WorkspaceScanner;

        if let RepoSource::LocalPath(ref path) = source {
            // Validate path exists
            if !path.exists() {
                if let Some(ref mut state) = self.new_session_state {
                    state.is_validating = false;
                    state.repo_validation_error =
                        Some(format!("Path not found: {}", path.display()));
                }
                return;
            }

            // Validate it's a git repo
            if !WorkspaceScanner::validate_workspace(path).unwrap_or(false) {
                if let Some(ref mut state) = self.new_session_state {
                    state.is_validating = false;
                    state.repo_validation_error =
                        Some(format!("Not a git repository: {}", path.display()));
                }
                return;
            }

            // Clone path before moving source
            let path_clone = path.clone();

            // Valid local repo - proceed to agent selection
            info!("Valid local repo: {}", path_clone.display());
            if let Some(ref mut state) = self.new_session_state {
                state.repo_source = Some(source);
                state.available_repos = vec![path_clone];
                state.selected_repo_index = Some(0);
                state.branch_name = format!(
                    "ainb/{}",
                    uuid::Uuid::new_v4().to_string().split('-').next().unwrap_or("session")
                );
                state.is_validating = false;
                state.step = NewSessionStep::SelectAgent;
            }
        }
    }

    /// Clone the selected remote repo and proceed
    pub async fn clone_remote_repo(&mut self) {
        use crate::git::RemoteRepoManager;

        let (source, base_branch, checkout_mode) = if let Some(ref state) = self.new_session_state {
            // Get branch from filtered list (which stores (original_idx, branch) tuples)
            let branch = state
                .filtered_branches
                .get(state.selected_branch_index)
                .map(|(_, b)| b.name.clone())
                .or_else(|| state.selected_base_branch.clone())
                .unwrap_or_else(|| "main".to_string());
            (
                state.repo_source.clone(),
                branch,
                state.branch_checkout_mode,
            )
        } else {
            error!("clone_remote_repo called but no state");
            return;
        };

        let Some(source) = source else {
            error!("No repo source set");
            return;
        };

        // Set validating state
        if let Some(ref mut state) = self.new_session_state {
            state.is_validating = true;
            state.step = NewSessionStep::ValidatingRepo;
        }

        let manager = match RemoteRepoManager::new() {
            Ok(m) => m,
            Err(e) => {
                if let Some(ref mut state) = self.new_session_state {
                    state.is_validating = false;
                    state.repo_validation_error =
                        Some(format!("Failed to init repo manager: {}", e));
                    state.step = NewSessionStep::SelectBranch;
                }
                return;
            }
        };

        let parsed = match source.parse_components() {
            Ok(p) => p,
            Err(e) => {
                if let Some(ref mut state) = self.new_session_state {
                    state.is_validating = false;
                    state.repo_validation_error = Some(format!("Failed to parse repo: {}", e));
                    state.step = NewSessionStep::SelectBranch;
                }
                return;
            }
        };

        info!("Cloning repo: {}/{}", parsed.owner, parsed.repo_name);

        match manager.clone_repo(&source, &parsed) {
            Ok(cache_path) => {
                info!("Cloned to: {}", cache_path.display());
                if let Some(ref mut state) = self.new_session_state {
                    state.cached_repo_path = Some(cache_path);
                    state.selected_base_branch = Some(base_branch.clone());
                    // Set branch name based on checkout mode
                    state.branch_name = match checkout_mode {
                        BranchCheckoutMode::CreateNew => format!(
                            "ainb/{}",
                            uuid::Uuid::new_v4().to_string().split('-').next().unwrap_or("session")
                        ),
                        BranchCheckoutMode::CheckoutExisting => base_branch,
                    };
                    state.is_validating = false;
                    state.step = NewSessionStep::SelectAgent;
                }
            }
            Err(e) => {
                error!("Clone failed: {}", e);
                if let Some(ref mut state) = self.new_session_state {
                    state.is_validating = false;
                    state.repo_validation_error = Some(e.to_string());
                    state.step = NewSessionStep::SelectBranch;
                }
            }
        }
    }

    /// Fetch branches from remote (called when user wants to refresh branch list)
    pub async fn fetch_remote_branches(&mut self) {
        use crate::git::RemoteRepoManager;

        let source = if let Some(ref state) = self.new_session_state {
            state.repo_source.clone()
        } else {
            return;
        };

        let Some(source) = source else { return };

        let manager = match RemoteRepoManager::new() {
            Ok(m) => m,
            Err(_) => return,
        };

        if let Ok(branches) = manager.list_remote_branches(&source) {
            if let Some(ref mut state) = self.new_session_state {
                state.filtered_branches = branches.iter().cloned().enumerate().collect();
                state.remote_branches = branches;
                state.branch_filter_text.clear();
                state.selected_branch_index = 0;
            }
        }
    }

    /// Navigate to next branch in branch picker (uses filtered list)
    pub fn branch_select_next(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::SelectBranch && !state.filtered_branches.is_empty() {
                state.selected_branch_index =
                    (state.selected_branch_index + 1) % state.filtered_branches.len();
            }
        }
    }

    /// Navigate to previous branch in branch picker (uses filtered list)
    pub fn branch_select_prev(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::SelectBranch && !state.filtered_branches.is_empty() {
                state.selected_branch_index = state
                    .selected_branch_index
                    .checked_sub(1)
                    .unwrap_or(state.filtered_branches.len() - 1);
            }
        }
    }

    /// Update branch filter text and re-filter the list
    pub fn branch_filter_update(&mut self, ch: char) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::SelectBranch {
                state.branch_filter_text.push(ch);
                Self::apply_branch_filter(state);
            }
        }
    }

    /// Handle branch filter backspace
    pub fn branch_filter_backspace(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::SelectBranch {
                state.branch_filter_text.pop();
                Self::apply_branch_filter(state);
            }
        }
    }

    /// Apply fuzzy filter to branch list
    fn apply_branch_filter(state: &mut NewSessionState) {
        let filter = state.branch_filter_text.to_lowercase();
        if filter.is_empty() {
            // No filter - show all branches
            state.filtered_branches = state.remote_branches.iter().cloned().enumerate().collect();
        } else {
            // Fuzzy filter - match if filter chars appear in order
            state.filtered_branches = state
                .remote_branches
                .iter()
                .enumerate()
                .filter(|(_, branch)| {
                    let name = branch.name.to_lowercase();
                    // Simple substring match (can enhance to fuzzy later)
                    name.contains(&filter)
                })
                .map(|(idx, branch)| (idx, branch.clone()))
                .collect();
        }
        // Reset selection to 0 (or keep valid if possible)
        if state.selected_branch_index >= state.filtered_branches.len() {
            state.selected_branch_index = 0;
        }
    }

    /// Update repo input text
    pub fn repo_input_update(&mut self, ch: char) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::InputRepoSource {
                state.repo_input.push(ch);
                state.repo_validation_error = None;
            }
        }
    }

    /// Handle repo input backspace
    pub fn repo_input_backspace(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::InputRepoSource {
                state.repo_input.pop();
                state.repo_validation_error = None;
            }
        }
    }

    /// Handle repo input backspace word
    pub fn repo_input_backspace_word(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::InputRepoSource && !state.repo_input.is_empty() {
                // Remove trailing whitespace first
                while state.repo_input.ends_with(' ') {
                    state.repo_input.pop();
                }
                // Remove word characters until whitespace or start
                while !state.repo_input.is_empty() && !state.repo_input.ends_with(' ') {
                    state.repo_input.pop();
                }
                state.repo_validation_error = None;
            }
        }
    }

    /// Go back from branch selection to repo input
    pub fn branch_select_back(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::SelectBranch {
                state.step = NewSessionStep::InputRepoSource;
                state.repo_source = None;
                state.remote_branches.clear();
                state.selected_branch_index = 0;
            }
        }
    }

    pub async fn new_session_in_current_dir(&mut self) {
        use crate::git::WorkspaceScanner;
        use std::env;

        info!("Starting new session in current directory");

        // Check if authentication is set up first
        if Self::is_first_time_setup() {
            info!("Authentication not set up, switching to auth setup view");
            self.current_screen = screen_ids::AUTH_SETUP.to_string();
            self.auth_setup_state = Some(AuthSetupState {
                selected_method: AuthMethod::OAuth,
                api_key_input: String::new(),
                is_processing: false,
                error_message: Some("Authentication required before creating sessions.\n\nPlease set up Claude authentication to continue.".to_string()),
                show_cursor: false,
            });
            return;
        }

        // Check if current directory is a git repository
        let current_dir = match env::current_dir() {
            Ok(dir) => {
                info!("Current directory: {:?}", dir);
                dir
            }
            Err(e) => {
                warn!("Failed to get current directory: {}", e);
                return;
            }
        };

        match WorkspaceScanner::validate_workspace(&current_dir) {
            Ok(true) => {
                info!(
                    "Current directory is a valid git repository: {:?}",
                    current_dir
                );
            }
            Ok(false) => {
                warn!(
                    "Current directory is not a git repository: {:?}",
                    current_dir
                );
                info!("Falling back to workspace search");
                // Fall back to workspace search since current directory is not a git repository
                self.start_workspace_search().await;
                return;
            }
            Err(e) => {
                error!("Failed to validate workspace: {}", e);
                info!("Falling back to workspace search due to validation error");
                // Fall back to workspace search on validation error
                self.start_workspace_search().await;
                return;
            }
        }

        // Generate branch name with UUID
        let branch_base = format!(
            "ainb/{}",
            uuid::Uuid::new_v4().to_string().split('-').next().unwrap_or("session")
        );

        // Create new session state for current directory
        self.new_session_state = Some(NewSessionState {
            available_repos: vec![current_dir.clone()],
            filtered_repos: vec![(0, current_dir.clone())],
            selected_repo_index: Some(0),
            branch_name: branch_base.clone(),
            step: NewSessionStep::InputBranch,
            is_current_dir_mode: true,
            ..Default::default()
        });

        self.current_screen = screen_ids::NEW_SESSION.to_string();

        info!(
            "Successfully created new session state with branch: {}",
            branch_base
        );
    }

    pub async fn start_workspace_search(&mut self) {
        info!("Starting workspace search");

        // Only transition to SessionList if coming from NonGitNotification
        // (preserve current view for new session flow which handles its own transitions)
        if self.current_screen == screen_ids::NON_GIT_NOTIFICATION {
            self.current_screen = screen_ids::SESSION_LIST.to_string();
        }

        // Scan for repositories directly without Docker dependency.
        // SessionLoader::new() requires Docker (ContainerManager), but repository
        // discovery only needs filesystem scanning via WorkspaceScanner + AppConfig.
        use crate::git::WorkspaceScanner;

        let repos = match AppConfig::load() {
            Ok(config) => {
                let scanner = WorkspaceScanner::with_additional_paths(
                    config.workspace_defaults.workspace_scan_paths.clone(),
                )
                .with_exclude_paths(config.workspace_defaults.exclude_paths.clone());

                match scanner.scan() {
                    Ok(scan_result) => {
                        let max_repos = config.workspace_defaults.max_repositories;
                        let total_found = scan_result.workspaces.len();
                        let repos: Vec<std::path::PathBuf> = scan_result
                            .workspaces
                            .into_iter()
                            .map(|w| w.path)
                            .take(max_repos)
                            .collect();
                        info!(
                            "Found {} repositories (showing {} of {}, limit: {})",
                            total_found,
                            repos.len(),
                            total_found,
                            max_repos
                        );
                        repos
                    }
                    Err(e) => {
                        warn!("Failed to scan for repositories: {}", e);
                        vec![]
                    }
                }
            }
            Err(e) => {
                warn!("Failed to load config for workspace search: {}", e);
                vec![]
            }
        };

        if repos.is_empty() {
            warn!("No repositories found in default search paths");
            info!("Showing empty search interface - user can type to add paths");
        }

        // Generate branch name with UUID
        let branch_base = format!(
            "ainb/{}",
            uuid::Uuid::new_v4().to_string().split('-').next().unwrap_or("session")
        );

        // Initialize filtered repos with all repos (even if empty)
        let filtered_repos: Vec<(usize, std::path::PathBuf)> =
            repos.iter().enumerate().map(|(idx, path)| (idx, path.clone())).collect();

        // Check if user has already cancelled (e.g., pressed escape while loading)
        if self.async_operation_cancelled {
            info!("Operation was cancelled by user");
            return;
        }

        let has_repos = !filtered_repos.is_empty();
        self.new_session_state = Some(NewSessionState {
            available_repos: repos,
            filtered_repos,
            selected_repo_index: if has_repos { Some(0) } else { None },
            branch_name: branch_base,
            step: NewSessionStep::SelectRepo,
            source_choice: RepoSourceChoice::Local,
            ..Default::default()
        });

        self.current_screen = screen_ids::SEARCH_WORKSPACE.to_string();
        info!("Successfully transitioned to SearchWorkspace view");
    }

    pub async fn start_new_session(&mut self) {
        info!("Starting new session creation");

        // Scan for repositories directly without Docker dependency.
        use crate::git::WorkspaceScanner;

        let repos = match AppConfig::load() {
            Ok(config) => {
                let scanner = WorkspaceScanner::with_additional_paths(
                    config.workspace_defaults.workspace_scan_paths.clone(),
                )
                .with_exclude_paths(config.workspace_defaults.exclude_paths.clone());

                match scanner.scan() {
                    Ok(scan_result) => {
                        let max_repos = config.workspace_defaults.max_repositories;
                        scan_result.workspaces.into_iter().map(|w| w.path).take(max_repos).collect()
                    }
                    Err(e) => {
                        warn!("Failed to scan for repositories: {}", e);
                        vec![]
                    }
                }
            }
            Err(e) => {
                warn!("Failed to load config: {}", e);
                vec![]
            }
        };

        let has_repos = !repos.is_empty();
        let filtered_repos: Vec<(usize, std::path::PathBuf)> =
            repos.iter().enumerate().map(|(idx, path)| (idx, path.clone())).collect();

        self.new_session_state = Some(NewSessionState {
            available_repos: repos,
            filtered_repos,
            selected_repo_index: if has_repos { Some(0) } else { None },
            ..Default::default()
        });
        self.current_screen = screen_ids::NEW_SESSION.to_string();
    }

    pub fn cancel_new_session(&mut self) {
        self.new_session_state = None;
        self.current_screen = screen_ids::SESSION_LIST.to_string();
        // Also clear any pending async actions to prevent race conditions
        self.pending_async_action = None;
        // Set cancellation flag to prevent race conditions
        self.async_operation_cancelled = true;
    }

    pub fn new_session_next_repo(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            if !state.filtered_repos.is_empty() {
                let current = state.selected_repo_index.unwrap_or(0);
                state.selected_repo_index = Some((current + 1) % state.filtered_repos.len());
            }
        }
    }

    pub fn new_session_prev_repo(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            if !state.filtered_repos.is_empty() {
                let current = state.selected_repo_index.unwrap_or(0);
                state.selected_repo_index = Some(if current == 0 {
                    state.filtered_repos.len() - 1
                } else {
                    current - 1
                });
            }
        }
    }

    pub fn new_session_confirm_repo(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            if state.selected_repo_index.is_some() {
                tracing::info!(
                    "Confirming repository selection - selected_repo_index: {:?}",
                    state.selected_repo_index
                );
                tracing::info!(
                    "Available repos count: {}, Filtered repos count: {}",
                    state.available_repos.len(),
                    state.filtered_repos.len()
                );

                if let Some(repo_index) = state.selected_repo_index {
                    if let Some((_, repo_path)) = state.filtered_repos.get(repo_index) {
                        tracing::info!("Selected repository path: {:?}", repo_path);

                        // Fetch current branch from the repository
                        state.current_repo_branch = Self::get_repo_current_branch(repo_path);
                        tracing::info!("Current branch: {:?}", state.current_repo_branch);
                    } else {
                        tracing::error!(
                            "Failed to get repository at index {} from filtered_repos",
                            repo_index
                        );
                        return;
                    }
                }

                // Skip agent selection step (agent/model selected on search screen)
                // Go directly to branch input
                state.step = NewSessionStep::InputBranch;

                // Change view from SearchWorkspace to NewSession
                self.current_screen = screen_ids::NEW_SESSION.to_string();
                tracing::info!(
                    "Repository confirmed (agent: {:?}, model: {:?}), transitioning to branch input",
                    state.selected_agent,
                    state.selected_model
                );
            }
        }
    }

    /// Get the current branch of a local repository
    fn get_repo_current_branch(repo_path: &std::path::Path) -> Option<String> {
        match git2::Repository::open(repo_path) {
            Ok(repo) => {
                match repo.head() {
                    Ok(head) => {
                        if head.is_branch() {
                            head.shorthand().map(|s| s.to_string())
                        } else {
                            // Detached HEAD - show short commit hash
                            head.target().map(|oid| format!("detached:{}", &oid.to_string()[..7]))
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Failed to get HEAD for {:?}: {}", repo_path, e);
                        None
                    }
                }
            }
            Err(e) => {
                tracing::warn!("Failed to open repository {:?}: {}", repo_path, e);
                None
            }
        }
    }

    /// Navigate to next agent in new session flow
    pub fn new_session_next_agent(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            // Allow agent selection on both SelectAgent and InputBranch steps
            if state.step == NewSessionStep::SelectAgent
                || state.step == NewSessionStep::InputBranch
            {
                state.next_agent();
            }
        }
    }

    /// Navigate to previous agent in new session flow
    pub fn new_session_prev_agent(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            // Allow agent selection on both SelectAgent and InputBranch steps
            if state.step == NewSessionStep::SelectAgent
                || state.step == NewSessionStep::InputBranch
            {
                state.prev_agent();
            }
        }
    }

    /// Navigate to next model in new session flow
    pub fn new_session_next_model(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            // Allow model selection on both SelectAgent and InputBranch steps
            if state.step == NewSessionStep::SelectAgent
                || state.step == NewSessionStep::InputBranch
            {
                state.next_model();
            }
        }
    }

    /// Navigate to previous model in new session flow
    pub fn new_session_prev_model(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            // Allow model selection on both SelectAgent and InputBranch steps
            if state.step == NewSessionStep::SelectAgent
                || state.step == NewSessionStep::InputBranch
            {
                state.prev_model();
            }
        }
    }

    /// Toggle focus between agent and model panels
    pub fn new_session_toggle_agent_model_focus(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::SelectAgent
                || state.step == NewSessionStep::InputBranch
            {
                state.toggle_agent_model_focus();
            }
        }
    }

    /// Select agent and proceed to next step (or create shell/SSH session)
    /// Returns true if Shell was selected (needs async handling)
    pub fn new_session_select_agent(&mut self) -> bool {
        if let Some(ref mut state) = self.new_session_state {
            if state.step != NewSessionStep::SelectAgent {
                return false;
            }

            let agent_type = state.current_agent_type();

            // Check if agent is available
            if !agent_type.is_available() {
                self.add_info_notification(format!(
                    "{} {} - Coming Soon!",
                    agent_type.icon(),
                    agent_type.name()
                ));
                return false;
            }

            // Store the selected agent
            state.confirm_agent_selection();
            tracing::info!("Selected agent: {:?}", state.selected_agent);

            // If Shell is selected, we need async handling to create shell session
            if agent_type == SessionAgentType::Shell {
                tracing::info!("Shell agent selected - will create shell session");
                return true; // Signal that async shell creation is needed
            }

            // If SSH is selected, go to SSH configuration step
            if agent_type == SessionAgentType::Ssh {
                tracing::info!("SSH agent selected - transitioning to SSH configuration");
                state.step = NewSessionStep::ConfigureSsh;
                // Reset SSH fields to defaults
                state.ssh_host.clear();
                state.ssh_port = "22".to_string();
                state.ssh_user.clear();
                state.ssh_identity_file.clear();
                state.ssh_input_focus = SshInputFocus::Host;
                return false;
            }

            // For AI agents, check if we should skip branch input
            // In CheckoutExisting mode for remote repos, the branch name is already set
            let skip_branch_input = state.branch_checkout_mode
                == BranchCheckoutMode::CheckoutExisting
                && state.cached_repo_path.is_some()
                && !state.branch_name.is_empty();

            if skip_branch_input {
                // Skip InputBranch step - go directly to mode selection
                state.step = NewSessionStep::SelectMode;
                tracing::info!(
                    "Agent selected: {:?}, skipping branch input (CheckoutExisting mode), branch: {}",
                    state.selected_agent,
                    state.branch_name
                );
            } else {
                // Normal flow: proceed to branch input
                state.step = NewSessionStep::InputBranch;
                let uuid_str = uuid::Uuid::new_v4().to_string();
                // Use branch prefix from config (default: "agents/")
                let prefix = &self.app_config.workspace_defaults.branch_prefix;
                state.branch_name = format!("{}session-{}", prefix, &uuid_str[..8]);

                tracing::info!(
                    "Agent selected: {:?}, transitioning to branch input with branch: {}",
                    state.selected_agent,
                    state.branch_name
                );
            }
        }
        false
    }

    pub fn new_session_update_branch(&mut self, ch: char) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::InputBranch {
                state.branch_name.push(ch);
            }
        }
    }

    pub fn new_session_backspace(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::InputBranch {
                state.branch_name.pop();
            }
        }
    }

    /// Delete word backward in branch name (Shift+Backspace)
    pub fn new_session_backspace_word(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::InputBranch && !state.branch_name.is_empty() {
                // Find the last word boundary (space, slash, dash, underscore)
                let s = &state.branch_name;
                // First, skip any trailing delimiters
                let trimmed_end =
                    s.trim_end_matches(|c: char| c == ' ' || c == '/' || c == '-' || c == '_');
                if trimmed_end.is_empty() {
                    state.branch_name.clear();
                    return;
                }
                // Find the last word boundary
                let last_boundary =
                    trimmed_end.rfind(|c: char| c == ' ' || c == '/' || c == '-' || c == '_');
                match last_boundary {
                    Some(idx) => {
                        // Keep up to and including the delimiter
                        state.branch_name = trimmed_end[..=idx].to_string();
                    }
                    None => {
                        // No boundary found, clear all
                        state.branch_name.clear();
                    }
                }
            }
        }
    }

    pub fn new_session_proceed_to_mode_selection(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::InputBranch {
                tracing::info!(
                    "Proceeding from InputBranch to SelectMode with branch: {}",
                    state.branch_name
                );
                state.step = NewSessionStep::SelectMode;
            }
        }
    }

    // ============================================================================
    // SSH Configuration Methods
    // ============================================================================

    /// Navigate to next SSH input field (Tab)
    pub fn new_session_ssh_next_field(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::ConfigureSsh {
                state.ssh_input_focus = match state.ssh_input_focus {
                    SshInputFocus::Host => SshInputFocus::Port,
                    SshInputFocus::Port => SshInputFocus::User,
                    SshInputFocus::User => SshInputFocus::IdentityFile,
                    SshInputFocus::IdentityFile => SshInputFocus::Host,
                };
            }
        }
    }

    /// Navigate to previous SSH input field (Shift+Tab)
    pub fn new_session_ssh_prev_field(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::ConfigureSsh {
                state.ssh_input_focus = match state.ssh_input_focus {
                    SshInputFocus::Host => SshInputFocus::IdentityFile,
                    SshInputFocus::Port => SshInputFocus::Host,
                    SshInputFocus::User => SshInputFocus::Port,
                    SshInputFocus::IdentityFile => SshInputFocus::User,
                };
            }
        }
    }

    /// Handle character input for SSH configuration
    pub fn new_session_ssh_input_char(&mut self, ch: char) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::ConfigureSsh {
                match state.ssh_input_focus {
                    SshInputFocus::Host => state.ssh_host.push(ch),
                    SshInputFocus::Port => {
                        // Only allow digits for port
                        if ch.is_ascii_digit() {
                            state.ssh_port.push(ch);
                        }
                    }
                    SshInputFocus::User => state.ssh_user.push(ch),
                    SshInputFocus::IdentityFile => state.ssh_identity_file.push(ch),
                }
            }
        }
    }

    /// Handle backspace for SSH configuration
    pub fn new_session_ssh_backspace(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::ConfigureSsh {
                match state.ssh_input_focus {
                    SshInputFocus::Host => {
                        state.ssh_host.pop();
                    }
                    SshInputFocus::Port => {
                        state.ssh_port.pop();
                    }
                    SshInputFocus::User => {
                        state.ssh_user.pop();
                    }
                    SshInputFocus::IdentityFile => {
                        state.ssh_identity_file.pop();
                    }
                }
            }
        }
    }

    /// Validate and confirm SSH configuration, returns true if ready to create session
    pub fn new_session_ssh_confirm(&mut self) -> bool {
        if let Some(ref mut state) = self.new_session_state {
            if state.step != NewSessionStep::ConfigureSsh {
                return false;
            }

            // Validate host is not empty
            if state.ssh_host.trim().is_empty() {
                self.add_error_notification("SSH host is required".to_string());
                return false;
            }

            // Validate port is a valid number
            let port: u16 = state.ssh_port.parse().unwrap_or(22);
            if port == 0 {
                self.add_error_notification("Invalid port number".to_string());
                return false;
            }

            tracing::info!(
                "SSH configuration confirmed: {}@{}:{}",
                if state.ssh_user.is_empty() {
                    "(no user)"
                } else {
                    &state.ssh_user
                },
                state.ssh_host,
                port
            );

            // SSH sessions go directly to Creating step (no branch/mode needed)
            state.step = NewSessionStep::Creating;
            return true;
        }
        false
    }

    /// Go back from SSH configuration to agent selection
    pub fn new_session_ssh_go_back(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::ConfigureSsh {
                state.step = NewSessionStep::SelectAgent;
                tracing::info!("Going back from SSH config to agent selection");
            }
        }
    }

    /// Build SSH command preview string
    pub fn get_ssh_command_preview(&self) -> String {
        if let Some(ref state) = self.new_session_state {
            let mut cmd = String::from("ssh");

            let port: u16 = state.ssh_port.parse().unwrap_or(22);
            if port != 22 {
                cmd.push_str(&format!(" -p {}", port));
            }

            if !state.ssh_identity_file.is_empty() {
                cmd.push_str(&format!(" -i {}", state.ssh_identity_file));
            }

            if !state.ssh_user.is_empty() {
                cmd.push_str(&format!(" {}@{}", state.ssh_user, state.ssh_host));
            } else if !state.ssh_host.is_empty() {
                cmd.push_str(&format!(" {}", state.ssh_host));
            }

            cmd
        } else {
            String::from("ssh")
        }
    }

    /// Build SshTarget from current state
    pub fn build_ssh_target(&self) -> Option<crate::models::SshTarget> {
        if let Some(ref state) = self.new_session_state {
            if state.ssh_host.trim().is_empty() {
                return None;
            }

            let port: u16 = state.ssh_port.parse().unwrap_or(22);
            let user = if state.ssh_user.is_empty() {
                None
            } else {
                Some(state.ssh_user.clone())
            };
            let identity_file = if state.ssh_identity_file.is_empty() {
                None
            } else {
                Some(std::path::PathBuf::from(&state.ssh_identity_file))
            };

            Some(crate::models::SshTarget::with_config(
                state.ssh_host.clone(),
                port,
                user,
                identity_file,
            ))
        } else {
            None
        }
    }

    pub fn new_session_proceed_from_mode(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::SelectMode {
                state.enforce_mode_constraints();
                tracing::info!(
                    "Proceeding from SelectMode to next step with mode: {:?}",
                    state.mode
                );
                match state.mode {
                    crate::models::SessionMode::Interactive => {
                        // Interactive mode: go directly to permissions
                        state.step = NewSessionStep::ConfigurePermissions;
                        tracing::info!("Interactive mode selected, going to ConfigurePermissions");
                    }
                    crate::models::SessionMode::Boss => {
                        // Boss mode: go to prompt input first
                        state.step = NewSessionStep::InputPrompt;
                        tracing::info!("Boss mode selected, going to InputPrompt");
                    }
                }
            }
        }
    }

    pub fn new_session_proceed_to_permissions(&mut self) {
        tracing::info!("new_session_proceed_to_permissions called");
        if let Some(ref mut state) = self.new_session_state {
            tracing::debug!("Current session state step: {:?}", state.step);
            if state.step == NewSessionStep::InputPrompt {
                tracing::info!("Advancing from InputPrompt to ConfigurePermissions");
                state.step = NewSessionStep::ConfigurePermissions;
                self.ui_needs_refresh = true;
            } else {
                tracing::warn!(
                    "Cannot proceed to permissions - not in InputPrompt step (current: {:?})",
                    state.step
                );
            }
        } else {
            tracing::error!("Cannot proceed to permissions - no session state found");
        }
    }

    /// Toggle between Local and Remote source choice
    /// Toggle source choice forward (↓ key): Local → Remote → Ssh → Favorites → Local
    pub fn new_session_toggle_source(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::SelectSource {
                state.source_choice = match state.source_choice {
                    RepoSourceChoice::Local => RepoSourceChoice::Remote,
                    RepoSourceChoice::Remote => RepoSourceChoice::Ssh,
                    RepoSourceChoice::Ssh => RepoSourceChoice::Favorites,
                    RepoSourceChoice::Favorites => RepoSourceChoice::Local,
                };
                tracing::info!("Source choice toggled to: {:?}", state.source_choice);
            }
        }
    }

    /// Toggle source choice backward (↑ key): Local → Favorites → Ssh → Remote → Local
    pub fn new_session_toggle_source_reverse(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::SelectSource {
                state.source_choice = match state.source_choice {
                    RepoSourceChoice::Local => RepoSourceChoice::Favorites,
                    RepoSourceChoice::Remote => RepoSourceChoice::Local,
                    RepoSourceChoice::Ssh => RepoSourceChoice::Remote,
                    RepoSourceChoice::Favorites => RepoSourceChoice::Ssh,
                };
                tracing::info!(
                    "Source choice toggled (reverse) to: {:?}",
                    state.source_choice
                );
            }
        }
    }

    /// Proceed from source selection to appropriate next step
    pub fn new_session_proceed_from_source(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::SelectSource {
                match state.source_choice {
                    RepoSourceChoice::Local => {
                        tracing::info!("Proceeding with Local source - loading repos");
                        // Reset cancellation flag for fresh search (prevents stale flag from previous session)
                        self.async_operation_cancelled = false;
                        // Set step to SelectRepo and trigger async repo loading
                        state.step = NewSessionStep::SelectRepo;
                        self.pending_async_action = Some(AsyncAction::StartWorkspaceSearch);
                    }
                    RepoSourceChoice::Remote => {
                        tracing::info!(
                            "Proceeding with Remote source - showing URL input with favorites"
                        );
                        // Load favorites for quick selection in InputRepoSource
                        state.favorites_store = crate::config::FavoritesStore::load();
                        state.selected_favorite_index = None; // Start with URL input focused
                        state.step = NewSessionStep::InputRepoSource;
                    }
                    RepoSourceChoice::Ssh => {
                        tracing::info!("Proceeding with SSH source - showing SSH config");
                        // Skip repo selection - go directly to SSH config
                        state.step = NewSessionStep::ConfigureSsh;
                        state.selected_agent = SessionAgentType::Ssh;
                        // Reset SSH fields to defaults
                        state.ssh_host.clear();
                        state.ssh_port = "22".to_string();
                        state.ssh_user.clear();
                        state.ssh_identity_file.clear();
                        state.ssh_input_focus = SshInputFocus::Host;
                    }
                    RepoSourceChoice::Favorites => {
                        tracing::info!(
                            "Proceeding with Favorites source - showing favorites picker"
                        );
                        // Load favorites and go to favorites picker
                        state.favorites_store = crate::config::FavoritesStore::load();
                        state.selected_favorite_index = if state.favorites_store.is_empty() {
                            None
                        } else {
                            Some(0)
                        };
                        state.step = NewSessionStep::SelectFavorite;
                    }
                }
            }
        }
    }

    /// Quick select Local source and proceed
    pub fn new_session_quick_select_local(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::SelectSource {
                state.source_choice = RepoSourceChoice::Local;
                tracing::info!("Quick select: Local source");
            }
        }
        self.new_session_proceed_from_source();
    }

    /// Quick select Remote source and proceed
    pub fn new_session_quick_select_remote(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::SelectSource {
                state.source_choice = RepoSourceChoice::Remote;
                tracing::info!("Quick select: Remote source");
            }
        }
        self.new_session_proceed_from_source();
    }

    /// Quick select SSH source and proceed directly to SSH config
    pub fn new_session_quick_select_ssh(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::SelectSource {
                state.source_choice = RepoSourceChoice::Ssh;
                tracing::info!("Quick select: SSH source");
            }
        }
        self.new_session_proceed_from_source();
    }

    /// Quick select Favorites source and proceed to favorites picker
    pub fn new_session_quick_select_favorites(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::SelectSource {
                state.source_choice = RepoSourceChoice::Favorites;
                tracing::info!("Quick select: Favorites source");
            }
        }
        self.new_session_proceed_from_source();
    }

    // === Favorites picker methods (used in SelectFavorite step) ===

    /// Move to next favorite in picker
    pub fn favorite_select_next(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::SelectFavorite && !state.favorites_store.is_empty() {
                let count = state.favorites_store.len();
                state.selected_favorite_index = match state.selected_favorite_index {
                    None => Some(0),
                    Some(idx) if idx + 1 < count => Some(idx + 1),
                    Some(idx) => Some(idx), // Stay at last
                };
            }
        }
    }

    /// Move to previous favorite in picker
    pub fn favorite_select_prev(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::SelectFavorite && !state.favorites_store.is_empty() {
                state.selected_favorite_index = match state.selected_favorite_index {
                    None => Some(0),
                    Some(0) => Some(0), // Stay at first
                    Some(idx) => Some(idx - 1),
                };
            }
        }
    }

    /// Confirm selected favorite and proceed
    pub fn favorite_select_confirm(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::SelectFavorite {
                if let Some(idx) = state.selected_favorite_index {
                    let sorted = state.favorites_store.sorted_by_usage();
                    if let Some(favorite) = sorted.get(idx) {
                        // Copy favorite data before mutating
                        let alias = favorite.alias.clone();
                        let source = favorite.source.clone();

                        // Record usage
                        state.favorites_store.record_use(&alias);
                        let _ = state.favorites_store.save();

                        // Set the repo input and trigger validation
                        // ValidateRepoSource handles both local paths and remote URLs
                        state.repo_input = source.clone();
                        state.step = NewSessionStep::ValidatingRepo;
                        self.pending_async_action = Some(AsyncAction::ValidateRepoSource);

                        tracing::info!("Selected favorite: {} -> {}", alias, source);
                    }
                }
            }
        }
    }

    /// Go back from favorites picker to source selection
    pub fn favorite_go_back(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::SelectFavorite {
                state.step = NewSessionStep::SelectSource;
                state.selected_favorite_index = None;
            }
        }
    }

    /// Delete selected favorite
    pub fn favorite_delete(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::SelectFavorite {
                if let Some(idx) = state.selected_favorite_index {
                    let sorted = state.favorites_store.sorted_by_usage();
                    if let Some(favorite) = sorted.get(idx) {
                        let alias = favorite.alias.clone();
                        state.favorites_store.remove(&alias);
                        let _ = state.favorites_store.save();
                        tracing::info!("Deleted favorite: {}", alias);

                        // Adjust selection index
                        let new_count = state.favorites_store.len();
                        if new_count == 0 {
                            state.selected_favorite_index = None;
                        } else if idx >= new_count {
                            state.selected_favorite_index = Some(new_count - 1);
                        }
                    }
                }
            }
        }
    }

    // === Inline favorites methods (used in InputRepoSource) ===

    /// Move focus to next favorite in inline list
    pub fn repo_input_favorite_next(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::InputRepoSource && !state.favorites_store.is_empty() {
                let count = state.favorites_store.len();
                state.selected_favorite_index = match state.selected_favorite_index {
                    None => Some(0), // Move from input to first favorite
                    Some(idx) if idx + 1 < count => Some(idx + 1),
                    Some(_) => None, // Wrap back to input
                };
            }
        }
    }

    /// Move focus to previous favorite in inline list
    pub fn repo_input_favorite_prev(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::InputRepoSource && !state.favorites_store.is_empty() {
                let count = state.favorites_store.len();
                state.selected_favorite_index = match state.selected_favorite_index {
                    None => Some(count - 1), // Move from input to last favorite
                    Some(0) => None,         // Move back to input
                    Some(idx) => Some(idx - 1),
                };
            }
        }
    }

    /// Select a favorite and fill in the repo input
    pub fn repo_input_select_favorite(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::InputRepoSource {
                if let Some(idx) = state.selected_favorite_index {
                    let sorted = state.favorites_store.sorted_by_usage();
                    if let Some(favorite) = sorted.get(idx) {
                        // Fill repo input with favorite's source
                        state.repo_input = favorite.source.clone();
                        // Record usage
                        let alias = favorite.alias.clone();
                        state.favorites_store.record_use(&alias);
                        let _ = state.favorites_store.save();
                        // Move focus back to input
                        state.selected_favorite_index = None;
                        tracing::info!("Selected favorite: {} -> {}", alias, state.repo_input);
                    }
                }
            }
        }
    }

    /// Toggle favorite status for current repo_input
    pub fn repo_input_toggle_favorite(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            if !state.repo_input.is_empty() {
                let source = state.repo_input.clone();
                // Check if already a favorite
                if let Some(existing) =
                    state.favorites_store.favorites.iter().find(|f| f.source == source)
                {
                    let alias = existing.alias.clone();
                    state.favorites_store.remove(&alias);
                    tracing::info!("Removed from favorites: {}", source);
                } else {
                    // Add as new favorite with auto-generated alias
                    let alias = Self::generate_favorite_alias(&source);
                    let source_type = Self::detect_source_type(&source);
                    let favorite =
                        crate::config::Favorite::new(alias.clone(), source.clone(), source_type);
                    let _ = state.favorites_store.add(favorite);
                    tracing::info!("Added to favorites: {} as {}", source, alias);
                }
                let _ = state.favorites_store.save();
            }
        }
    }

    /// Generate an alias from a repo URL/path
    fn generate_favorite_alias(source: &str) -> String {
        // Extract repo name from URL or path
        let name = source
            .trim_end_matches('/')
            .split('/')
            .last()
            .unwrap_or("repo")
            .trim_end_matches(".git");
        name.to_string()
    }

    /// Detect source type from URL/path
    fn detect_source_type(source: &str) -> crate::config::FavoriteSourceType {
        if source.starts_with("https://") || source.starts_with("http://") {
            crate::config::FavoriteSourceType::HttpsUrl
        } else if source.starts_with("git@") || source.contains("@") && source.contains(":") {
            crate::config::FavoriteSourceType::SshUrl
        } else if source.starts_with('/') || source.starts_with('~') || source.starts_with('.') {
            crate::config::FavoriteSourceType::LocalPath
        } else if source.contains('/') && !source.contains(':') {
            // owner/repo format
            crate::config::FavoriteSourceType::GithubShorthand
        } else {
            crate::config::FavoriteSourceType::HttpsUrl
        }
    }

    /// Check if current repo_input is a favorite
    pub fn is_repo_input_favorite(&self) -> bool {
        if let Some(ref state) = self.new_session_state {
            if !state.repo_input.is_empty() {
                return state
                    .favorites_store
                    .favorites
                    .iter()
                    .any(|f| f.source == state.repo_input);
            }
        }
        false
    }

    /// Toggle favorite status for currently selected local repo
    pub fn local_repo_toggle_favorite(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            // Get the selected repo from filtered_repos
            if let Some(idx) = state.selected_repo_index {
                if let Some((_, repo_path)) = state.filtered_repos.get(idx) {
                    let source = repo_path.display().to_string();

                    // Check if already a favorite
                    if let Some(existing) =
                        state.favorites_store.favorites.iter().find(|f| f.source == source)
                    {
                        let alias = existing.alias.clone();
                        state.favorites_store.remove(&alias);
                        tracing::info!("Removed local repo from favorites: {}", source);
                    } else {
                        // Add as new favorite with auto-generated alias
                        let alias = Self::generate_favorite_alias(&source);
                        let source_type = crate::config::FavoriteSourceType::LocalPath;
                        let favorite = crate::config::Favorite::new(
                            alias.clone(),
                            source.clone(),
                            source_type,
                        );
                        let _ = state.favorites_store.add(favorite);
                        tracing::info!("Added local repo to favorites: {} as {}", source, alias);
                    }
                    let _ = state.favorites_store.save();
                }
            }
        }
    }

    /// Check if a local repo path is a favorite
    pub fn is_local_repo_favorite(&self, path: &std::path::Path) -> bool {
        if let Some(ref state) = self.new_session_state {
            let path_str = path.display().to_string();
            return state.favorites_store.favorites.iter().any(|f| f.source == path_str);
        }
        false
    }

    pub fn new_session_toggle_mode(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::SelectMode {
                if !state.is_boss_mode_available() {
                    state.mode = crate::models::SessionMode::Interactive;
                    return;
                }
                state.mode = match state.mode {
                    crate::models::SessionMode::Interactive => crate::models::SessionMode::Boss,
                    crate::models::SessionMode::Boss => crate::models::SessionMode::Interactive,
                };
            }
        }
    }

    pub fn new_session_add_char_to_prompt(&mut self, ch: char) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::InputPrompt {
                if ch == '@' {
                    // Activate fuzzy file finder (supports multiple @ references)
                    let workspace_root = if let Some(selected_idx) = state.selected_repo_index {
                        state.filtered_repos.get(selected_idx).map(|(_, path)| path.clone())
                    } else {
                        None
                    };
                    // If already active, deactivate current search and start new one
                    if state.file_finder.is_active {
                        state.file_finder.deactivate();
                    }
                    state.file_finder.activate(state.boss_prompt.to_string().len(), workspace_root);
                    state.boss_prompt.insert_char(ch);
                } else if state.file_finder.is_active {
                    // File finder is active, handle character input for filtering
                    if ch == ' ' || ch == '\t' || ch == '\n' {
                        // Whitespace deactivates file finder
                        state.file_finder.deactivate();
                        state.boss_prompt.insert_char(ch);
                    } else {
                        state.file_finder.add_char_to_query(ch);
                    }
                } else {
                    // Normal character input
                    state.boss_prompt.insert_char(ch);
                }
            }
        }
    }

    pub fn new_session_backspace_prompt(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::InputPrompt {
                if state.file_finder.is_active {
                    if !state.file_finder.query.is_empty() {
                        // Remove character from file finder query
                        state.file_finder.backspace_query();
                    } else {
                        // Query is empty, deactivate file finder and remove @ symbol
                        state.file_finder.deactivate();
                        state.boss_prompt.backspace();
                    }
                } else {
                    // Normal backspace
                    state.boss_prompt.backspace();
                }
            }
        }
    }

    pub fn new_session_move_cursor_left(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::InputPrompt && !state.file_finder.is_active {
                state.boss_prompt.move_cursor_left();
            }
        }
    }

    pub fn new_session_move_cursor_right(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::InputPrompt && !state.file_finder.is_active {
                state.boss_prompt.move_cursor_right();
            }
        }
    }

    pub fn new_session_move_cursor_up(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::InputPrompt && !state.file_finder.is_active {
                state.boss_prompt.move_cursor_up();
            }
        }
    }

    pub fn new_session_move_cursor_down(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::InputPrompt && !state.file_finder.is_active {
                state.boss_prompt.move_cursor_down();
            }
        }
    }

    pub fn new_session_move_to_line_start(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::InputPrompt && !state.file_finder.is_active {
                state.boss_prompt.move_to_line_start();
            }
        }
    }

    pub fn new_session_move_to_line_end(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::InputPrompt && !state.file_finder.is_active {
                state.boss_prompt.move_to_line_end();
            }
        }
    }

    pub fn new_session_move_cursor_word_left(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::InputPrompt && !state.file_finder.is_active {
                state.boss_prompt.move_cursor_word_backward();
            }
        }
    }

    pub fn new_session_move_cursor_word_right(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::InputPrompt && !state.file_finder.is_active {
                state.boss_prompt.move_cursor_word_forward();
            }
        }
    }

    pub fn new_session_delete_word_forward(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::InputPrompt && !state.file_finder.is_active {
                state.boss_prompt.delete_word_forward();
            }
        }
    }

    pub fn new_session_delete_word_backward(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::InputPrompt && !state.file_finder.is_active {
                state.boss_prompt.delete_word_backward();
            }
        }
    }

    pub fn new_session_insert_newline(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::InputPrompt && !state.file_finder.is_active {
                state.boss_prompt.insert_newline();
            }
        }
    }

    pub fn new_session_paste_text(&mut self, text: String) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::InputPrompt && !state.file_finder.is_active {
                // Insert the pasted text at the current cursor position
                state.boss_prompt.insert_text(&text);
            }
        }
    }

    /// Paste text into the remote-repo URL input. URLs are single-line, so
    /// CR/LF are stripped rather than splitting the field.
    pub fn repo_input_paste_text(&mut self, text: &str) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::InputRepoSource {
                for ch in text.chars().filter(|c| *c != '\r' && *c != '\n') {
                    state.repo_input.push(ch);
                }
                state.repo_validation_error = None;
            }
        }
    }

    /// Paste text into the currently-focused SSH field. CR/LF are stripped
    /// because each SSH field is a single-line value; for the port field we
    /// additionally drop non-digit characters.
    pub fn new_session_ssh_paste_text(&mut self, text: &str) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::ConfigureSsh {
                let filtered = text.chars().filter(|c| *c != '\r' && *c != '\n');
                match state.ssh_input_focus {
                    SshInputFocus::Host => state.ssh_host.extend(filtered),
                    SshInputFocus::Port => {
                        state.ssh_port.extend(filtered.filter(char::is_ascii_digit));
                    }
                    SshInputFocus::User => state.ssh_user.extend(filtered),
                    SshInputFocus::IdentityFile => state.ssh_identity_file.extend(filtered),
                }
            }
        }
    }

    /// Paste text into the branch-name input. Branch names are single-line,
    /// so CR/LF are stripped.
    pub fn new_session_input_paste_text(&mut self, text: &str) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::InputBranch {
                for ch in text.chars().filter(|c| *c != '\r' && *c != '\n') {
                    state.branch_name.push(ch);
                }
            }
        }
    }

    pub fn new_session_toggle_permissions(&mut self) {
        if let Some(ref mut state) = self.new_session_state {
            if state.step == NewSessionStep::ConfigurePermissions {
                state.skip_permissions = !state.skip_permissions;
            }
        }
    }

    pub async fn new_session_create(&mut self) {
        // Check session mode FIRST to determine if auth is needed
        let session_mode = if let Some(ref state) = self.new_session_state {
            state.mode.clone()
        } else {
            tracing::error!("new_session_create called but new_session_state is None");
            return;
        };

        // ONLY check authentication for Boss mode (Docker-based sessions)
        // Interactive mode uses host ~/.claude and doesn't need Docker auth
        if session_mode == crate::models::SessionMode::Boss {
            // First check if Docker is available (Boss mode requires Docker)
            if !self.is_docker_available().await {
                error!("Boss mode requires Docker but Docker is not running");
                self.add_error_notification(
                    "Boss mode requires Docker.\n\nPlease start Docker and try again, or use Interactive mode instead.".to_string()
                );
                // Stay in current view so user can go back and select Interactive mode
                return;
            }

            // Check if tokens need refresh (Docker is available at this point)
            if let Some(home) = dirs::home_dir() {
                let credentials_path = home.join(".agents-in-a-box/auth/.credentials.json");
                if credentials_path.exists() && Self::oauth_token_needs_refresh(&credentials_path) {
                    info!("Boss mode selected - OAuth tokens need refresh, attempting refresh");
                    match self.refresh_oauth_tokens().await {
                        Ok(()) => info!("OAuth tokens refreshed successfully for Boss mode"),
                        Err(e) => {
                            error!("Failed to refresh OAuth tokens for Boss mode: {}", e);
                            self.add_error_notification(
                                format!("Failed to refresh OAuth tokens: {}\n\nPlease check Docker and try again.", e)
                            );
                            return;
                        }
                    }
                }
            }

            // Then check if authentication is set up
            if Self::is_first_time_setup() {
                info!(
                    "Boss mode selected but authentication not set up, switching to auth setup view"
                );
                self.current_screen = screen_ids::AUTH_SETUP.to_string();
                self.auth_setup_state = Some(AuthSetupState {
                    selected_method: AuthMethod::OAuth,
                    api_key_input: String::new(),
                    is_processing: false,
                    error_message: Some("Boss mode requires Docker authentication.\n\nPlease set up Claude authentication to continue.".to_string()),
                    show_cursor: false,
                });
                // Clear new session state
                self.new_session_state = None;
                return;
            }
        } else {
            info!(
                "Interactive mode selected - skipping Docker auth check (will use host ~/.claude)"
            );
        }

        let (
            repo_path,
            branch_name,
            session_id,
            skip_permissions,
            mode,
            boss_prompt,
            restart_session_id,
            agent_type,
            session_model,
            existing_worktree,
            deferred_notice,
        ) = {
            if let Some(ref mut state) = self.new_session_state {
                tracing::info!("new_session_create called with step: {:?}", state.step);

                // Handle both ConfigurePermissions step (normal flow) and InputBranch step (current dir mode)
                let can_create = match state.step {
                    NewSessionStep::ConfigurePermissions => true,
                    NewSessionStep::InputBranch if state.is_current_dir_mode => {
                        // For current directory mode, skip to permissions step with defaults
                        state.step = NewSessionStep::ConfigurePermissions;
                        state.skip_permissions = false; // Default to safe permissions
                        state.mode = crate::models::SessionMode::Interactive; // Default mode
                        true
                    }
                    _ => false,
                };

                if can_create {
                    // Check if this is a remote repo flow (cached_repo_path is set)
                    // For remote repos, we create worktree here and pass it to session creation
                    // For local repos, session creation will create the worktree
                    let (repo_path, existing_worktree, worktree_notice) = if let Some(
                        ref cached_path,
                    ) =
                        state.cached_repo_path
                    {
                        // Remote repo flow - create worktree from bare cache
                        use crate::git::RemoteRepoManager;

                        let base_branch = state.selected_base_branch.as_deref().unwrap_or("main");
                        let branch_name = &state.branch_name;

                        // Determine worktree location in ~/.agents-in-a-box/worktrees/
                        let worktree_base = match dirs::home_dir() {
                            Some(dir) => dir.join(".agents-in-a-box").join("worktrees"),
                            None => {
                                tracing::error!("Home directory not found, cannot create worktree");
                                state.repo_validation_error =
                                    Some("Home directory not found".to_string());
                                state.step = NewSessionStep::InputRepoSource;
                                return;
                            }
                        };

                        // Create unique worktree path using repo name and branch
                        let repo_name = state
                            .repo_source
                            .as_ref()
                            .map(|s| s.display_name().replace('/', "_"))
                            .unwrap_or_else(|| "unknown".to_string());
                        let base_worktree_name =
                            format!("{}_{}", repo_name, branch_name.replace('/', "_"));
                        let mut worktree_path = worktree_base.join(&base_worktree_name);
                        let mut worktree_notice: Option<String> = None;

                        if worktree_path.exists() {
                            match self.app_config.workspace_defaults.worktree_collision_behavior {
                                WorktreeCollisionBehavior::AutoRename => {
                                    let mut suffix = Uuid::new_v4().to_string();
                                    suffix.truncate(8);
                                    let mut candidate = worktree_base
                                        .join(format!("{}-{}", base_worktree_name, suffix));
                                    while candidate.exists() {
                                        let mut next_suffix = Uuid::new_v4().to_string();
                                        next_suffix.truncate(8);
                                        candidate = worktree_base.join(format!(
                                            "{}-{}",
                                            base_worktree_name, next_suffix
                                        ));
                                    }
                                    worktree_notice = Some(format!(
                                        "⚠️ Worktree already exists. Creating new worktree at {}",
                                        candidate.display()
                                    ));
                                    tracing::info!(
                                        "Worktree path exists, auto-renaming: {} -> {}",
                                        worktree_path.display(),
                                        candidate.display()
                                    );
                                    worktree_path = candidate;
                                }
                                WorktreeCollisionBehavior::Error => {
                                    let message = format!(
                                        "Worktree path already exists: {}",
                                        worktree_path.display()
                                    );
                                    tracing::error!("{}", message);
                                    state.repo_validation_error = Some(message);
                                    state.step = NewSessionStep::InputRepoSource;
                                    return;
                                }
                            }
                        }

                        let manager = match RemoteRepoManager::new() {
                            Ok(m) => m,
                            Err(e) => {
                                tracing::error!("Failed to create RemoteRepoManager: {}", e);
                                return;
                            }
                        };

                        // Route worktree creation based on checkout mode
                        let checkout_mode = state.branch_checkout_mode;
                        tracing::info!(
                            "Creating worktree from cache: {} -> {} (branch: {}, base: {}, mode: {:?})",
                            cached_path.display(),
                            worktree_path.display(),
                            branch_name,
                            base_branch,
                            checkout_mode
                        );

                        // Handle worktree creation based on checkout mode
                        let final_worktree_path = match checkout_mode {
                            BranchCheckoutMode::CreateNew => {
                                match manager.create_worktree_from_cache(
                                    cached_path,
                                    &worktree_path,
                                    branch_name,
                                    base_branch,
                                ) {
                                    Ok(()) => worktree_path.clone(),
                                    Err(e) => {
                                        tracing::error!("Failed to create worktree: {}", e);
                                        state.repo_validation_error =
                                            Some(format!("Failed to create worktree: {}", e));
                                        state.step = NewSessionStep::InputRepoSource;
                                        return;
                                    }
                                }
                            }
                            BranchCheckoutMode::CheckoutExisting => {
                                match manager.checkout_existing_branch_worktree(
                                    cached_path,
                                    &worktree_path,
                                    base_branch,
                                ) {
                                    Ok(None) => worktree_path.clone(),
                                    Ok(Some((suffixed_path, suffixed_branch))) => {
                                        // Created worktree with suffixed branch due to collision
                                        tracing::info!(
                                            "Created suffixed worktree '{}' at: {}",
                                            suffixed_branch,
                                            suffixed_path.display()
                                        );
                                        worktree_notice = Some(format!(
                                            "⚠️ Branch already has worktree. Created '{}' at {}",
                                            suffixed_branch,
                                            suffixed_path.display()
                                        ));
                                        suffixed_path
                                    }
                                    Err(e) => {
                                        tracing::error!("Failed to create worktree: {}", e);
                                        state.repo_validation_error =
                                            Some(format!("Failed to create worktree: {}", e));
                                        state.step = NewSessionStep::InputRepoSource;
                                        return;
                                    }
                                }
                            }
                        };

                        tracing::info!("Using worktree at: {}", final_worktree_path.display());
                        // Return worktree path and the existing worktree info (worktree_path, source_repo)
                        (
                            final_worktree_path.clone(),
                            Some((final_worktree_path, cached_path.clone())),
                            worktree_notice,
                        )
                    } else if let Some(repo_index) = state.selected_repo_index {
                        // Local repo flow - no existing worktree, session creation will create it
                        if let Some((_, repo_path)) = state.filtered_repos.get(repo_index) {
                            (repo_path.clone(), None, None)
                        } else {
                            tracing::error!(
                                "Failed to get repository path from filtered_repos at index: {}",
                                repo_index
                            );
                            return;
                        }
                    } else {
                        tracing::error!("No repository selected and no cached repo path");
                        return;
                    };

                    tracing::info!(
                        "Creating session for repository: {:?}, branch: {}, existing_worktree: {}",
                        repo_path,
                        state.branch_name,
                        existing_worktree.is_some()
                    );
                    state.step = NewSessionStep::Creating;

                    // Use existing session ID for restart, or generate new one
                    let session_id =
                        state.restart_session_id.unwrap_or_else(|| uuid::Uuid::new_v4());

                    (
                        repo_path,
                        state.branch_name.clone(),
                        session_id,
                        state.skip_permissions,
                        state.mode.clone(),
                        if state.mode == crate::models::SessionMode::Boss {
                            Some(state.boss_prompt.to_string())
                        } else {
                            None
                        },
                        state.restart_session_id,  // Pass restart session ID
                        state.selected_agent,      // Agent type for session
                        state.get_session_model(), // Model (only for Claude agent)
                        existing_worktree,         // Existing worktree for remote repos
                        worktree_notice,
                    )
                } else {
                    tracing::warn!(
                        "new_session_create called but step is not valid for creation, current step: {:?}, is_current_dir_mode: {}",
                        state.step,
                        state.is_current_dir_mode
                    );
                    return;
                }
            } else {
                tracing::error!("new_session_create called but new_session_state is None");
                return;
            }
        };

        if let Some(notice) = deferred_notice {
            self.add_info_notification(notice);
        }

        // Create the session with log streaming
        tracing::info!(
            "Calling create_session_with_logs for session {} (mode: {:?}, restart: {})",
            session_id,
            mode,
            restart_session_id.is_some()
        );

        let result = if let Some(restart_id) = restart_session_id {
            // This is a restart - try to reuse existing worktree
            info!(
                "Restarting session {} with potentially updated configuration",
                restart_id
            );
            self.create_restart_session_with_logs(
                &repo_path,
                &branch_name,
                session_id,
                skip_permissions,
                mode,
                boss_prompt,
                agent_type,
                session_model,
            )
            .await
        } else {
            // Normal new session creation
            self.create_session_with_logs(
                &repo_path,
                &branch_name,
                session_id,
                skip_permissions,
                mode,
                boss_prompt,
                agent_type,
                session_model,
                existing_worktree,
            )
            .await
        };

        match result {
            Ok(()) => {
                info!("Session created successfully");
                // Reload workspaces BEFORE switching view to ensure UI shows new session immediately
                self.load_real_workspaces().await;

                // Start log streaming for the newly created session
                if let Err(e) = self.start_log_streaming_for_session(session_id).await {
                    warn!(
                        "Failed to start log streaming for session {}: {}",
                        session_id, e
                    );
                }

                // Force UI refresh to show new session immediately
                self.ui_needs_refresh = true;
                self.cancel_new_session();
            }
            Err(e) => {
                error!("Failed to create session: {}", e);
                self.cancel_new_session();
            }
        }
    }

    async fn create_restart_session_with_logs(
        &mut self,
        repo_path: &std::path::Path,
        branch_name: &str,
        session_id: Uuid,
        skip_permissions: bool,
        mode: crate::models::SessionMode,
        boss_prompt: Option<String>,
        agent_type: crate::models::SessionAgentType,
        model: Option<crate::models::ClaudeModel>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use crate::docker::session_lifecycle::{SessionLifecycleManager, SessionRequest};
        use std::path::PathBuf;

        info!(
            "Creating restart session {} with updated configuration",
            session_id
        );

        // Create a channel for build logs
        let (log_sender, mut log_receiver) = mpsc::unbounded_channel::<String>();

        // Initialize logs for this session
        self.logs.insert(
            session_id,
            vec!["Restarting session with updated configuration...".to_string()],
        );

        // Create a shared vector for logs
        let session_logs = Arc::new(Mutex::new(Vec::new()));
        let logs_clone = session_logs.clone();

        // Spawn a task to collect logs
        let session_id_clone = session_id;
        tokio::spawn(async move {
            while let Some(log_message) = log_receiver.recv().await {
                if let Ok(mut logs) = logs_clone.lock() {
                    logs.push(log_message.clone());
                }
                info!(
                    "Restart log for session {}: {}",
                    session_id_clone, log_message
                );
            }
        });

        let workspace_name =
            repo_path.file_name().and_then(|n| n.to_str()).unwrap_or("unknown").to_string();

        // Clone mode so we can use it later for tmux check
        let mode_clone = mode.clone();

        let request = SessionRequest {
            session_id,
            workspace_name,
            workspace_path: repo_path.to_path_buf(),
            branch_name: branch_name.to_string(),
            base_branch: None,
            container_config: None,
            skip_permissions,
            mode,
            boss_prompt,
            agent_type,
            model,
        };

        // Add initial log message
        if let Some(session_logs) = self.logs.get_mut(&session_id) {
            session_logs.push("Checking for existing worktree...".to_string());
        }

        let mut manager = SessionLifecycleManager::new().await?;

        // Check if worktree exists from the previous session
        let existing_worktree_path = self
            .workspaces
            .iter()
            .flat_map(|w| &w.sessions)
            .find(|s| s.id == session_id)
            .map(|s| PathBuf::from(&s.workspace_path));

        let result = if let Some(worktree_path) = existing_worktree_path {
            if worktree_path.exists() {
                info!(
                    "Found existing worktree at {}, reusing it",
                    worktree_path.display()
                );

                if let Some(logs) = self.logs.get_mut(&session_id) {
                    logs.push(format!(
                        "Reusing existing worktree at {}",
                        worktree_path.display()
                    ));
                }

                let worktree_info = crate::git::WorktreeInfo {
                    id: session_id, // Use session ID as worktree ID
                    path: worktree_path.clone(),
                    session_path: worktree_path.clone(), // Same as path for existing worktrees
                    branch_name: branch_name.to_string(),
                    source_repository: repo_path.to_path_buf(),
                    commit_hash: None, // We don't track this for existing worktrees
                };

                manager.create_session_with_existing_worktree(request, worktree_info).await
            } else {
                info!("Worktree path no longer exists, creating fresh session");

                if let Some(logs) = self.logs.get_mut(&session_id) {
                    logs.push("Worktree not found, creating fresh session...".to_string());
                }

                manager.create_session_with_logs(request, Some(log_sender.clone())).await
            }
        } else {
            info!("No existing worktree info found, creating fresh session");

            if let Some(logs) = self.logs.get_mut(&session_id) {
                logs.push("Creating fresh session...".to_string());
            }

            manager.create_session_with_logs(request, Some(log_sender.clone())).await
        };

        // Wait a moment for logs to be collected
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Transfer collected logs to our main logs HashMap
        if let Ok(collected_logs) = session_logs.lock() {
            if let Some(logs) = self.logs.get_mut(&session_id) {
                logs.extend(collected_logs.clone());
            }
        }

        // Add completion log based on result
        if let Some(logs) = self.logs.get_mut(&session_id) {
            match &result {
                Ok(_) => logs
                    .push("Session restarted successfully with updated configuration!".to_string()),
                Err(e) => logs.push(format!("Session restart failed: {}", e)),
            }
        }

        // If Docker session creation succeeded AND this is Interactive mode, create corresponding tmux session
        // Boss mode sessions should NOT have tmux integration
        if let Ok(ref session_state) = result {
            if mode_clone == crate::models::SessionMode::Interactive {
                if let Some(ref worktree_info) = session_state.worktree_info {
                    info!(
                        "Creating tmux session for restarted Interactive mode session {}",
                        session_id
                    );

                    // Send log message about tmux session creation
                    let _ = log_sender
                        .send("Creating tmux session for interactive mode...".to_string());

                    // Create tmux session name from session info
                    let tmux_name =
                        format!("tmux_{}", branch_name.replace('/', "_").replace(' ', "_"));

                    let mut tmux_session =
                        crate::tmux::TmuxSession::new(tmux_name.clone(), "claude".to_string());
                    let tmux_session_name = tmux_session.name().to_string();

                    // Start tmux session in the worktree directory
                    match tmux_session.start(&worktree_info.path).await {
                        Ok(_) => {
                            info!("Successfully started tmux session: {}", tmux_session_name);

                            // Store tmux session name in the actual session model
                            if let Some(session) = self.find_session_mut(session_id) {
                                session.set_tmux_session_name(tmux_session_name.clone());
                            }

                            // Store tmux session in our map
                            self.tmux_sessions.insert(session_id, tmux_session);

                            let _ =
                                log_sender.send("Tmux session created successfully!".to_string());
                        }
                        Err(e) => {
                            warn!("Failed to start tmux session: {}", e);
                            let _ = log_sender
                                .send(format!("Warning: Failed to create tmux session: {}", e));
                            // Don't fail the whole session creation if tmux fails
                        }
                    }
                } else {
                    warn!("Session state has no worktree info, skipping tmux creation");
                }
            } else {
                info!(
                    "Skipping tmux creation for Boss mode session {}",
                    session_id
                );
            }
        }

        result.map(|_| ())?;
        Ok(())
    }

    async fn create_session_with_logs(
        &mut self,
        repo_path: &std::path::Path,
        branch_name: &str,
        session_id: Uuid,
        skip_permissions: bool,
        mode: crate::models::SessionMode,
        boss_prompt: Option<String>,
        agent_type: crate::models::SessionAgentType,
        model: Option<crate::models::ClaudeModel>,
        existing_worktree: Option<(std::path::PathBuf, std::path::PathBuf)>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Branch based on session mode
        match mode {
            crate::models::SessionMode::Interactive => {
                self.create_interactive_session(
                    repo_path,
                    branch_name,
                    session_id,
                    skip_permissions,
                    agent_type,
                    model,
                    existing_worktree,
                )
                .await
            }
            crate::models::SessionMode::Boss => {
                self.create_boss_session(
                    repo_path,
                    branch_name,
                    session_id,
                    skip_permissions,
                    boss_prompt,
                )
                .await
            }
        }
    }

    /// Create an Interactive mode session (host-based, no Docker)
    ///
    /// # Arguments
    /// * `repo_path` - Path to the repository (or existing worktree for remote repos)
    /// * `branch_name` - Branch name for the session
    /// * `session_id` - Unique session identifier
    /// * `skip_permissions` - Whether to skip permission prompts
    /// * `agent_type` - Type of agent (Claude, Shell, etc.)
    /// * `model` - Claude model to use
    /// * `existing_worktree` - For remote repos: (worktree_path, source_repo_path)
    async fn create_interactive_session(
        &mut self,
        repo_path: &std::path::Path,
        branch_name: &str,
        session_id: Uuid,
        skip_permissions: bool,
        agent_type: crate::models::SessionAgentType,
        model: Option<crate::models::ClaudeModel>,
        existing_worktree: Option<(std::path::PathBuf, std::path::PathBuf)>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use crate::interactive::InteractiveSessionManager;

        info!(
            "Creating Interactive mode session {} for branch '{}' (skip_permissions={}, existing_worktree={})",
            session_id,
            branch_name,
            skip_permissions,
            existing_worktree.is_some()
        );

        // Create a channel for logs
        let (log_sender, mut log_receiver) = mpsc::unbounded_channel::<String>();

        // Initialize logs for this session
        self.logs.insert(
            session_id,
            vec!["Starting Interactive session creation...".to_string()],
        );

        // Create a shared vector for logs
        let session_logs = Arc::new(Mutex::new(Vec::new()));
        let logs_clone = session_logs.clone();

        // Spawn a task to collect logs
        let session_id_clone = session_id;
        tokio::spawn(async move {
            while let Some(log_message) = log_receiver.recv().await {
                if let Ok(mut logs) = logs_clone.lock() {
                    logs.push(log_message.clone());
                }
                info!(
                    "Interactive session log for {}: {}",
                    session_id_clone, log_message
                );
            }
        });

        let workspace_name =
            repo_path.file_name().and_then(|n| n.to_str()).unwrap_or("unknown").to_string();

        // Create Interactive session manager (NO Docker dependency)
        let mut manager = InteractiveSessionManager::new()?;

        // Create the session - use existing worktree for remote repos, create new for local
        let result = if let Some((worktree_path, source_repo_path)) = existing_worktree {
            // Remote repo flow - worktree already created from bare cache
            let _ = log_sender.send("Using existing worktree...".to_string());
            manager
                .create_session_with_worktree(
                    session_id,
                    workspace_name.clone(),
                    worktree_path,
                    source_repo_path,
                    branch_name.to_string(),
                    skip_permissions,
                    agent_type,
                    model,
                )
                .await
        } else {
            // Local repo flow - create new worktree
            let _ = log_sender.send("Creating git worktree...".to_string());
            manager
                .create_session(
                    session_id,
                    workspace_name.clone(),
                    repo_path.to_path_buf(),
                    branch_name.to_string(),
                    None, // base_branch
                    skip_permissions,
                    agent_type,
                    model,
                )
                .await
        };

        // Wait for logs to be collected
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Transfer collected logs
        if let Ok(collected_logs) = session_logs.lock() {
            if let Some(logs) = self.logs.get_mut(&session_id) {
                logs.extend(collected_logs.clone());
            }
        }

        match result {
            Ok(interactive_session) => {
                // Send success log
                if let Some(logs) = self.logs.get_mut(&session_id) {
                    logs.push("Interactive session created successfully!".to_string());
                }

                // Convert to Session model and add to workspaces
                let session = interactive_session.to_session_model();

                // Find or create workspace for this repo
                if let Some((ws_idx, workspace)) =
                    self.workspaces.iter_mut().enumerate().find(|(_, w)| {
                        std::path::Path::new(&w.path).canonicalize().ok()
                            == repo_path.canonicalize().ok()
                    })
                {
                    workspace.sessions.push(session);
                    // Auto-select the new session so the list scrolls to show it
                    self.selected_workspace_index = Some(ws_idx);
                    self.selected_session_index = Some(workspace.sessions.len() - 1);
                } else {
                    // Create new workspace
                    let mut workspace =
                        crate::models::Workspace::new(workspace_name, repo_path.to_path_buf());
                    workspace.sessions.push(session);
                    self.workspaces.push(workspace);
                    // Auto-select the new workspace and session
                    self.selected_workspace_index = Some(self.workspaces.len() - 1);
                    self.selected_session_index = Some(0);
                }

                // Store tmux session for attach operations
                // Pass branch name (NOT tmux-prefixed name) to TmuxSession::new()
                // because TmuxSession::sanitize_name() will add the tmux_ prefix
                let tmux_session = crate::tmux::TmuxSession::new(
                    interactive_session.branch_name.clone(),
                    "claude".to_string(),
                );
                self.tmux_sessions.insert(session_id, tmux_session);

                info!("Successfully created Interactive session {}", session_id);
                Ok(())
            }
            Err(e) => {
                error!("Failed to create Interactive session: {}", e);
                if let Some(logs) = self.logs.get_mut(&session_id) {
                    logs.push(format!("Session creation failed: {}", e));
                }
                Err(Box::new(e))
            }
        }
    }

    /// Create a Boss mode session (Docker-based)
    async fn create_boss_session(
        &mut self,
        repo_path: &std::path::Path,
        branch_name: &str,
        session_id: Uuid,
        skip_permissions: bool,
        boss_prompt: Option<String>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use crate::docker::session_lifecycle::{SessionLifecycleManager, SessionRequest};

        info!(
            "Creating Boss mode session {} for branch '{}'",
            session_id, branch_name
        );

        // Create a channel for build logs
        let (log_sender, mut log_receiver) = mpsc::unbounded_channel::<String>();

        // Initialize logs for this session
        self.logs.insert(
            session_id,
            vec!["Starting Boss session creation...".to_string()],
        );

        // Create a shared vector for logs
        let session_logs = Arc::new(Mutex::new(Vec::new()));
        let logs_clone = session_logs.clone();

        // Spawn a task to collect logs
        let session_id_clone = session_id;
        tokio::spawn(async move {
            while let Some(log_message) = log_receiver.recv().await {
                if let Ok(mut logs) = logs_clone.lock() {
                    logs.push(log_message.clone());
                }
                info!(
                    "Build log for session {}: {}",
                    session_id_clone, log_message
                );
            }
        });

        let workspace_name =
            repo_path.file_name().and_then(|n| n.to_str()).unwrap_or("unknown").to_string();

        let request = SessionRequest {
            session_id,
            workspace_name,
            workspace_path: repo_path.to_path_buf(),
            branch_name: branch_name.to_string(),
            base_branch: None,
            container_config: None,
            skip_permissions,
            mode: crate::models::SessionMode::Boss,
            boss_prompt,
            agent_type: crate::models::SessionAgentType::Claude, // Boss mode is Docker-based Claude
            model: None, // Boss mode manages model separately
        };

        // Add initial log message
        if let Some(session_logs) = self.logs.get_mut(&session_id) {
            session_logs.push("Creating worktree...".to_string());
        }

        // Create Docker-based session manager
        let mut manager = SessionLifecycleManager::new().await?;

        // Pass the log sender to the session lifecycle manager
        let result = manager.create_session_with_logs(request, Some(log_sender)).await;

        // Wait a moment for logs to be collected
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Transfer collected logs to our main logs HashMap
        if let Ok(collected_logs) = session_logs.lock() {
            if let Some(logs) = self.logs.get_mut(&session_id) {
                logs.extend(collected_logs.clone());
            }
        }

        // Add completion log based on result
        if let Some(logs) = self.logs.get_mut(&session_id) {
            match &result {
                Ok(_) => logs.push("Boss session created successfully!".to_string()),
                Err(e) => logs.push(format!("Session creation failed: {}", e)),
            }
        }

        result.map(|_| ())?;
        Ok(())
    }

    /// Clean up orphaned containers (containers without worktrees) AND orphaned session state
    pub async fn cleanup_orphaned_containers(&mut self) -> anyhow::Result<usize> {
        use crate::docker::ContainerManager;

        info!("Starting cleanup of orphaned containers and state entries");

        let container_manager = ContainerManager::new().await?;
        let containers = container_manager.list_agents_containers().await?;

        let mut cleaned_up = 0;

        // Step 1: Clean up orphaned containers (containers without worktrees)
        for container in containers {
            if let Some(session_id_str) =
                container.labels.as_ref().and_then(|labels| labels.get("agents-session-id"))
            {
                if let Ok(session_id) = uuid::Uuid::parse_str(session_id_str) {
                    // Check if worktree exists for this session
                    let worktree_manager = crate::git::WorktreeManager::new()?;
                    // Only process if worktree is missing (orphaned container)
                    if worktree_manager.get_worktree_info(session_id).is_err() {
                        info!(
                            "Found orphaned container for session {}, removing it",
                            session_id
                        );

                        if let Some(container_id) = &container.id {
                            // Remove the orphaned container (this will stop it first)
                            if let Err(e) =
                                container_manager.remove_container_by_id(container_id).await
                            {
                                warn!(
                                    "Failed to remove orphaned container {}: {}",
                                    container_id, e
                                );
                            } else {
                                cleaned_up += 1;
                                info!("Successfully removed orphaned container {}", container_id);
                            }
                        }
                    }
                }
            }
        }

        // Step 2: Clean up orphaned session state (sessions in workspace list without worktrees)
        let worktree_manager = crate::git::WorktreeManager::new()?;
        let mut orphaned_sessions = Vec::new();

        // Collect all session IDs from all workspaces
        for workspace in &self.workspaces {
            for session in &workspace.sessions {
                // Check if this session's name starts with "orphaned-"
                if session.name.starts_with("orphaned-") {
                    orphaned_sessions.push(session.id);
                } else {
                    // Also check if the worktree actually exists
                    if let Err(_) = worktree_manager.get_worktree_info(session.id) {
                        info!(
                            "Found session without worktree: {} ({})",
                            session.name, session.id
                        );
                        orphaned_sessions.push(session.id);
                    }
                }
            }
        }

        // Remove orphaned session state entries
        for session_id in &orphaned_sessions {
            info!("Removing orphaned session state: {}", session_id);

            // Remove from workspaces
            for workspace in &mut self.workspaces {
                workspace.sessions.retain(|s| s.id != *session_id);
            }

            // Clean up any remaining state
            self.live_logs.remove(session_id);

            cleaned_up += 1;
        }

        // Step 3: Prune git worktrees (removes stale git references for deleted worktrees)
        info!("Pruning git worktrees to remove stale references");
        use tokio::process::Command;
        match Command::new("git").arg("worktree").arg("prune").arg("-v").output().await {
            Ok(output) => {
                if output.status.success() {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    if !stdout.trim().is_empty() {
                        info!("Git worktree prune output: {}", stdout.trim());
                        // Count lines that start with "Removing" to track pruned worktrees
                        let pruned_count =
                            stdout.lines().filter(|line| line.contains("Removing")).count();
                        if pruned_count > 0 {
                            info!("Pruned {} stale git worktree references", pruned_count);
                            cleaned_up += pruned_count;

                            // Audit log the prune operation
                            audit::audit_git_worktree_prune(
                                AuditTrigger::UserKeypress("Ctrl+X".to_string()),
                                AuditResult::Success,
                                Some(pruned_count),
                            );
                        }
                    } else {
                        info!("No stale git worktree references to prune");
                    }
                } else {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    warn!("Git worktree prune failed: {}", stderr);

                    // Audit log the failed prune
                    audit::audit_git_worktree_prune(
                        AuditTrigger::UserKeypress("Ctrl+X".to_string()),
                        AuditResult::Failed(stderr.to_string()),
                        None,
                    );
                }
            }
            Err(e) => {
                warn!("Failed to run git worktree prune: {}", e);

                // Audit log the failed prune
                audit::audit_git_worktree_prune(
                    AuditTrigger::UserKeypress("Ctrl+X".to_string()),
                    AuditResult::Failed(e.to_string()),
                    None,
                );
            }
        }

        // Step 4: Clean up orphaned tmux shell sessions (ainb-ws-* and ainb-shell-*)
        info!("Cleaning up orphaned tmux shell sessions");
        let shells_cleaned = self.cleanup_orphaned_tmux_shells().await;
        cleaned_up += shells_cleaned;

        if cleaned_up > 0 {
            info!(
                "Cleaned up {} orphaned items (containers + state + git refs + tmux shells)",
                cleaned_up
            );
            self.add_success_notification(format!("🧹 Cleaned up {} orphaned items", cleaned_up));

            // Reload workspaces to reflect changes
            self.load_real_workspaces().await;
            self.ui_needs_refresh = true;

            // Audit log the overall cleanup
            audit::audit_orphaned_cleanup(
                AuditTrigger::UserKeypress("Ctrl+X".to_string()),
                AuditResult::Success,
                format!(
                    "Cleaned up {} orphaned items (containers + state + git refs + tmux shells)",
                    cleaned_up
                ),
            );
        } else {
            info!("No orphaned containers or sessions found");
            self.add_info_notification("✅ No orphaned items found".to_string());
        }

        Ok(cleaned_up)
    }

    /// Clean up orphaned tmux shell sessions (ainb-ws-* and ainb-shell-*)
    /// Returns the number of sessions killed
    async fn cleanup_orphaned_tmux_shells(&mut self) -> usize {
        use tokio::process::Command;

        // Get list of all tmux sessions
        let output = match Command::new("tmux")
            .args(["list-sessions", "-F", "#{session_name}"])
            .output()
            .await
        {
            Ok(output) if output.status.success() => output,
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                // "no server running" is not an error - just means no tmux sessions
                if !stderr.contains("no server running") {
                    warn!("tmux list-sessions failed: {}", stderr);
                }
                return 0;
            }
            Err(e) => {
                warn!("Failed to run tmux list-sessions: {}", e);
                return 0;
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let orphaned_shells: Vec<String> = stdout
            .lines()
            .filter(|name| name.starts_with("ainb-ws-") || name.starts_with("ainb-shell-"))
            .map(|s| s.to_string())
            .collect();

        if orphaned_shells.is_empty() {
            info!("No orphaned tmux shell sessions found");
            return 0;
        }

        info!(
            "Found {} orphaned tmux shell sessions to clean up",
            orphaned_shells.len()
        );
        let mut killed_count = 0;

        for session_name in &orphaned_shells {
            info!("Killing orphaned tmux shell session: {}", session_name);
            match Command::new("tmux").args(["kill-session", "-t", session_name]).output().await {
                Ok(output) if output.status.success() => {
                    killed_count += 1;
                    info!("Successfully killed tmux session: {}", session_name);
                }
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    warn!("Failed to kill tmux session {}: {}", session_name, stderr);
                }
                Err(e) => {
                    warn!(
                        "Failed to run tmux kill-session for {}: {}",
                        session_name, e
                    );
                }
            }
        }

        if killed_count > 0 {
            // Reload other tmux sessions to reflect changes
            self.load_other_tmux_sessions().await;
        }

        killed_count
    }

    /// Core deletion logic without workspace refresh — used by bulk delete
    async fn delete_session_core(&mut self, session_id: Uuid) -> anyhow::Result<()> {
        info!("Deleting session (core): {}", session_id);

        // Capture session details for audit logging BEFORE deletion
        let session_details = self.find_session(session_id).map(|s| {
            (
                s.mode.clone(),
                s.tmux_session_name.clone(),
                s.workspace_path.clone(),
            )
        });

        // Determine session mode by finding the session
        let session_mode = session_details.as_ref().map(|(mode, _, _)| mode.clone());

        // Track deletion result but don't early-return on error
        let deletion_result: anyhow::Result<()> = if let Some(mode) = session_mode {
            match mode {
                crate::models::SessionMode::Interactive => {
                    self.delete_interactive_session(session_id).await
                }
                crate::models::SessionMode::Boss => self.delete_boss_session(session_id).await,
            }
        } else {
            // Session not found in UI, try both cleanup methods
            warn!(
                "Session {} not found in UI, attempting cleanup anyway",
                session_id
            );

            // Try Interactive cleanup first (no Docker needed)
            if let Err(e) = self.delete_interactive_session(session_id).await {
                debug!("Interactive cleanup failed (expected if Boss mode): {}", e);
            }

            // Try Boss cleanup if Docker is available
            if self.is_docker_available().await {
                if let Err(e) = self.delete_boss_session(session_id).await {
                    debug!("Boss cleanup failed (expected if Interactive mode): {}", e);
                }
            }
            Ok(())
        };

        // Audit log the deletion
        let audit_result = if let Err(e) = &deletion_result {
            error!("Session deletion encountered error: {}", e);
            AuditResult::Failed(e.to_string())
        } else {
            info!("Successfully deleted session: {}", session_id);
            AuditResult::Success
        };

        let (tmux_session, worktree_path) = session_details
            .map(|(_, tmux, path)| (tmux, Some(path)))
            .unwrap_or((None, None));

        audit::audit_session_deleted(
            session_id,
            tmux_session,
            worktree_path,
            AuditTrigger::UserKeypress("D".to_string()),
            audit_result,
        );

        deletion_result
    }

    async fn delete_session(&mut self, session_id: Uuid) -> anyhow::Result<()> {
        let result = self.delete_session_core(session_id).await;

        // ALWAYS reload workspaces to ensure UI reflects the actual state
        self.load_real_workspaces().await;
        self.ui_needs_refresh = true;

        result
    }

    /// Delete an Interactive mode session
    async fn delete_interactive_session(&mut self, session_id: Uuid) -> anyhow::Result<()> {
        use crate::interactive::InteractiveSessionManager;

        info!("=== DELETE INTERACTIVE SESSION START: {} ===", session_id);

        // Cleanup tmux session if it exists
        if let Some(mut tmux_session) = self.tmux_sessions.remove(&session_id) {
            info!("Found tmux session in state, cleaning up: {}", session_id);
            if let Err(e) = tmux_session.cleanup().await {
                warn!("Failed to cleanup tmux session from state: {}", e);
            }
        } else {
            info!("No tmux session found in state for: {}", session_id);
        }

        // Use Interactive session manager to remove session
        info!(
            "Creating InteractiveSessionManager for session: {}",
            session_id
        );
        let mut manager = InteractiveSessionManager::new()?;
        info!("Calling manager.remove_session() for: {}", session_id);
        match manager.remove_session(session_id).await {
            Ok(()) => info!("manager.remove_session() succeeded for: {}", session_id),
            Err(e) => {
                error!("manager.remove_session() failed for {}: {}", session_id, e);
                return Err(e.into());
            }
        }

        info!(
            "=== DELETE INTERACTIVE SESSION COMPLETE: {} ===",
            session_id
        );
        Ok(())
    }

    /// Soft-stop an interactive session by killing only its tmux session.
    ///
    /// Unlike `delete_interactive_session`, this preserves:
    ///   - the worktree on disk
    ///   - the `~/.agents-in-a-box/sessions.json` entry
    ///   - the `by-session/<uuid>` symlink
    ///
    /// The session is rediscovered as `Stopped` on the next workspace reload (and
    /// across TUI restarts) and can be resumed via `resume_interactive_session`.
    async fn stop_interactive_session(&mut self, session_id: Uuid) -> anyhow::Result<()> {
        use crate::interactive::SessionStore;
        use crate::models::SessionStatus;

        info!("Soft-stopping interactive session: {}", session_id);

        // Resolve tmux session name preferring the in-memory map, falling back to
        // sessions.json (handles edge case where the live map is out of sync).
        let tmux_name = self
            .tmux_sessions
            .get(&session_id)
            .map(|t| t.name().to_string())
            .or_else(|| self.find_session(session_id).and_then(|s| s.tmux_session_name.clone()))
            .or_else(|| {
                let store = SessionStore::load();
                store
                    .sessions()
                    .values()
                    .find(|m| m.session_id == session_id)
                    .map(|m| m.tmux_session_name.clone())
            });

        let worktree_path = self.find_session(session_id).map(|s| s.workspace_path.clone());

        let result: anyhow::Result<()> = if let Some(ref name) = tmux_name {
            // Hard constraint: kill only the exact named session. NEVER kill-server.
            let output = tokio::process::Command::new("tmux")
                .args(["kill-session", "-t", name])
                .output()
                .await?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                // tmux returns non-zero when the session is already gone — treat as success
                // since the post-condition (no tmux session) is what we care about.
                if stderr.contains("can't find session") || stderr.contains("no server running") {
                    info!("tmux session '{}' already gone — proceeding", name);
                } else {
                    return Err(anyhow::anyhow!(
                        "Failed to kill tmux session '{}': {}",
                        name,
                        stderr
                    ));
                }
            } else {
                info!("Killed tmux session: {}", name);
            }
            Ok(())
        } else {
            warn!(
                "No tmux_session_name for {} — nothing to kill, just marking Stopped",
                session_id
            );
            Ok(())
        };

        // Drop the live tmux handle but DO NOT touch SessionStore or worktree.
        self.tmux_sessions.remove(&session_id);

        if let Some(session) = self.find_session_mut(session_id) {
            session.set_status(SessionStatus::Stopped);
            session.is_attached = false;
        }

        let audit_result = match &result {
            Ok(()) => AuditResult::Success,
            Err(e) => AuditResult::Failed(e.to_string()),
        };
        audit::audit_session_stopped(
            session_id,
            tmux_name,
            worktree_path,
            AuditTrigger::UserKeypress("D→Stop".to_string()),
            audit_result,
        );

        // Mirror delete_session: refresh workspace view so the Stopped indicator is rendered.
        self.load_real_workspaces().await;
        self.ui_needs_refresh = true;

        result
    }

    /// Resume a previously-stopped interactive session.
    ///
    /// Recreates the tmux session at the original worktree and re-launches the
    /// agent CLI. For Claude, attempts to discover the latest transcript and
    /// pass `--resume <path>` to continue the conversation. Other agents start
    /// fresh because their CLIs do not support transcript-based resume.
    async fn resume_interactive_session(
        &mut self,
        session_id: Uuid,
        trigger_key: String,
    ) -> anyhow::Result<()> {
        use crate::interactive::{InteractiveSessionManager, SessionStore};
        use crate::models::SessionStatus;

        info!(
            "Resuming interactive session: {} (trigger={})",
            session_id, trigger_key
        );

        // Resolve metadata up-front so we can audit even if subsequent steps fail.
        let store = SessionStore::load();
        let metadata = store.sessions().values().find(|m| m.session_id == session_id).cloned();

        // Pull skip_permissions and model from the in-memory Session if available.
        let (skip_permissions, model) = self
            .find_session(session_id)
            .map(|s| (s.skip_permissions, s.model))
            .unwrap_or((false, None));

        // Capture audit context before any fallible step so we can record both
        // success and failure with the same fields.
        let tmux_name_for_audit = metadata.as_ref().map(|m| m.tmux_session_name.clone());
        let worktree_for_audit = metadata.as_ref().map(|m| m.worktree_path.display().to_string());

        let mut transcript: Option<PathBuf> = None;
        let result: anyhow::Result<()> = async {
            let metadata = metadata.ok_or_else(|| {
                anyhow::anyhow!("Session {} not found in sessions.json", session_id)
            })?;

            // Recreate the tmux session at the worktree. If something is already
            // listening on this name (shouldn't be, since we set Stopped), kill it
            // first so we get a clean shell — narrow target, never wildcard.
            let _ = tokio::process::Command::new("tmux")
                .args(["kill-session", "-t", &metadata.tmux_session_name])
                .output()
                .await;

            let new_output = tokio::process::Command::new("tmux")
                .args([
                    "new-session",
                    "-d",
                    "-s",
                    &metadata.tmux_session_name,
                    "-c",
                    metadata
                        .worktree_path
                        .to_str()
                        .ok_or_else(|| anyhow::anyhow!("Worktree path is not valid UTF-8"))?,
                    "-x",
                    "120",
                    "-y",
                    "40",
                ])
                .output()
                .await?;

            if !new_output.status.success() {
                let stderr = String::from_utf8_lossy(&new_output.stderr);
                return Err(anyhow::anyhow!(
                    "Failed to create tmux session '{}': {}",
                    metadata.tmux_session_name,
                    stderr
                ));
            }

            transcript = if metadata.agent_type == SessionAgentType::Claude {
                Self::find_latest_transcript(&metadata.worktree_path)
            } else {
                None
            };

            let manager = InteractiveSessionManager::new()?;
            manager
                .start_cli_in_tmux(
                    &metadata.tmux_session_name,
                    skip_permissions,
                    model,
                    metadata.agent_type,
                    transcript.clone(),
                )
                .await?;

            // Re-register live tmux handle and flip status to Running.
            let tmux_session = crate::tmux::TmuxSession::new(
                metadata.tmux_session_name.clone(),
                metadata.agent_type.name().to_string(),
            );
            self.tmux_sessions.insert(session_id, tmux_session);

            if let Some(session) = self.find_session_mut(session_id) {
                session.set_status(SessionStatus::Running);
                session.tmux_session_name = Some(metadata.tmux_session_name.clone());
            }

            // Inline status banner. Encoded path is shown only on the no-transcript
            // path so the user can locate (or rule out) Claude's project directory.
            let banner: String = match (metadata.agent_type, transcript.as_ref()) {
                (SessionAgentType::Claude, Some(_)) => "Resumed".to_string(),
                (SessionAgentType::Claude, None) => {
                    let encoded = Self::encode_claude_project_dir(&metadata.worktree_path);
                    format!(
                        "No transcript found at ~/.claude/projects/{} - starting fresh",
                        encoded
                    )
                }
                (other, _) => format!(
                    "Resumed (fresh session - {} doesn't support --resume)",
                    other.name()
                ),
            };
            self.add_info_notification(banner);

            Ok(())
        }
        .await;

        let audit_result = match &result {
            Ok(()) => AuditResult::Success,
            Err(e) => AuditResult::Failed(e.to_string()),
        };
        audit::audit_session_resumed(
            session_id,
            tmux_name_for_audit,
            worktree_for_audit,
            transcript.as_ref().map(|p| p.display().to_string()),
            AuditTrigger::UserKeypress(trigger_key),
            audit_result,
        );

        if result.is_ok() {
            self.load_real_workspaces().await;
            self.ui_needs_refresh = true;
        }

        result
    }

    /// Encode an absolute worktree path the same way Claude Code does for its
    /// `~/.claude/projects/-{encoded}/` transcript directory:
    ///   - replace `/` with `-`
    ///   - strip leading slash
    /// Then prefix with `-` (callers do this).
    ///
    /// Mirror of `find_transcript_path()` in
    /// `toolkit/packages/utilities/utils/spawn-agent-lib.sh:30-69`.
    pub(crate) fn encode_claude_project_dir(worktree_path: &std::path::Path) -> String {
        let s = worktree_path.to_string_lossy();
        let stripped = s.strip_prefix('/').unwrap_or(&s);
        format!("-{}", stripped.replace('/', "-"))
    }

    /// Find the most recently modified Claude transcript (`*.jsonl`) for the
    /// given worktree under `~/.claude/projects/-{encoded}/`.
    ///
    /// Returns `None` when the project directory is missing or contains no
    /// transcripts.
    pub fn find_latest_transcript(worktree_path: &std::path::Path) -> Option<std::path::PathBuf> {
        let home = dirs::home_dir()?;
        Self::find_latest_transcript_in(&home, worktree_path)
    }

    /// Test-friendly variant: caller supplies the home directory so unit tests
    /// don't have to mutate process-wide environment.
    pub(crate) fn find_latest_transcript_in(
        home: &std::path::Path,
        worktree_path: &std::path::Path,
    ) -> Option<std::path::PathBuf> {
        let project_dir = home
            .join(".claude")
            .join("projects")
            .join(Self::encode_claude_project_dir(worktree_path));

        let read = std::fs::read_dir(&project_dir).ok()?;
        let mut candidates: Vec<(std::path::PathBuf, std::time::SystemTime)> = Vec::new();
        for entry in read.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                continue;
            }
            if let Ok(meta) = entry.metadata() {
                if let Ok(mtime) = meta.modified() {
                    candidates.push((path, mtime));
                }
            }
        }
        candidates.into_iter().max_by_key(|(_, t)| *t).map(|(p, _)| p)
    }

    /// Delete a Boss mode session
    async fn delete_boss_session(&mut self, session_id: Uuid) -> anyhow::Result<()> {
        use crate::docker::{ContainerManager, SessionLifecycleManager};
        use crate::git::WorktreeManager;

        info!("Deleting Boss mode session: {}", session_id);

        // Cleanup tmux session if it exists (Boss mode might have tmux for attach)
        if let Some(mut tmux_session) = self.tmux_sessions.remove(&session_id) {
            info!("Cleaning up tmux session for Boss session {}", session_id);
            if let Err(e) = tmux_session.cleanup().await {
                warn!("Failed to cleanup tmux session: {}", e);
            }
        }

        // First, try to find and remove the container directly
        let container_name = format!("agents-session-{}", session_id);
        let container_manager = ContainerManager::new().await?;

        info!("Looking for container: {}", container_name);
        if let Ok(containers) = container_manager.list_agents_containers().await {
            for container in containers {
                if let Some(names) = &container.names {
                    if names.iter().any(|n| n.trim_start_matches('/') == container_name) {
                        info!("Found container for session {}, removing it", session_id);
                        if let Some(container_id) = &container.id {
                            match container_manager.remove_container_by_id(container_id).await {
                                Ok(_) => info!("Successfully removed container {}", container_id),
                                Err(e) => {
                                    warn!("Failed to remove container {}: {}", container_id, e)
                                }
                            }
                        }
                        break;
                    }
                }
            }
        }

        // Create session lifecycle manager
        let mut manager = SessionLifecycleManager::new().await?;

        // Try to remove the session through lifecycle manager (this will handle worktree)
        match manager.remove_session(session_id).await {
            Ok(_) => {
                info!("Session removed through lifecycle manager");
            }
            Err(e) => {
                warn!("Session not found in lifecycle manager: {}", e);
                info!("Attempting to remove orphaned worktree directly");

                // Remove the worktree directly
                let worktree_manager = WorktreeManager::new()?;
                if let Err(worktree_err) = worktree_manager.remove_worktree(session_id) {
                    warn!("Failed to remove worktree: {}", worktree_err);
                } else {
                    info!("Successfully removed orphaned worktree");
                }
            }
        }

        info!("Successfully deleted Boss session: {}", session_id);
        Ok(())
    }

    pub async fn process_async_action(&mut self) -> anyhow::Result<()> {
        if let Some(action) = self.pending_async_action.take() {
            info!(
                ">>> process_async_action() called with action: {:?}",
                action
            );
            match action {
                AsyncAction::StartNewSession => {
                    self.start_new_session().await;
                }
                AsyncAction::StartWorkspaceSearch => {
                    // Add timeout to prevent hanging
                    use tokio::time::{Duration, timeout};
                    match timeout(Duration::from_secs(10), self.start_workspace_search()).await {
                        Ok(_) => {}
                        Err(_) => {
                            warn!("Workspace search timed out after 10 seconds");
                            // Return to safe state
                            self.new_session_state = None;
                            self.current_screen = screen_ids::SESSION_LIST.to_string();
                            return Err(anyhow::anyhow!("Workspace search timed out"));
                        }
                    }
                }
                AsyncAction::NewSessionInCurrentDir => {
                    self.new_session_in_current_dir().await;
                }
                AsyncAction::NewSessionNormal => {
                    self.new_session_normal().await;
                }
                AsyncAction::NewSessionWithRepoInput => {
                    self.new_session_with_repo_input().await;
                }
                AsyncAction::ValidateRepoSource => {
                    self.validate_repo_source().await;
                }
                AsyncAction::CloneRemoteRepo => {
                    self.clone_remote_repo().await;
                }
                AsyncAction::FetchRemoteBranches => {
                    self.fetch_remote_branches().await;
                }
                AsyncAction::CreateNewSession => {
                    self.new_session_create().await;
                }
                AsyncAction::DeleteSession(session_id) => {
                    if let Err(e) = self.delete_session(session_id).await {
                        error!("Failed to delete session {}: {}", session_id, e);
                    }
                }
                AsyncAction::StopSession(session_id) => {
                    if let Err(e) = self.stop_interactive_session(session_id).await {
                        error!("Failed to stop session {}: {}", session_id, e);
                        self.add_error_notification(format!("Stop failed: {}", e));
                    }
                }
                AsyncAction::ResumeSession(session_id, trigger) => {
                    if let Err(e) = self.resume_interactive_session(session_id, trigger).await {
                        error!("Failed to resume session {}: {}", session_id, e);
                        self.add_error_notification(format!("Resume failed: {}", e));
                    }
                }
                AsyncAction::BulkDeleteSessions(session_ids) => {
                    let total = session_ids.len();
                    let mut deleted = 0;
                    let mut failed = 0;
                    for id in session_ids {
                        if let Err(e) = self.delete_session_core(id).await {
                            error!("Failed to delete session {}: {}", id, e);
                            failed += 1;
                        } else {
                            deleted += 1;
                        }
                    }
                    // Refresh once after all deletions
                    self.load_real_workspaces().await;
                    if failed > 0 {
                        self.add_warning_notification(format!(
                            "Deleted {}/{} sessions ({} failed)",
                            deleted, total, failed
                        ));
                    } else {
                        self.add_success_notification(format!("Deleted {} session(s)", deleted));
                    }
                    self.ui_needs_refresh = true;
                }
                AsyncAction::RefreshWorkspaces => {
                    info!("Manual refresh triggered");
                    // Reload workspace data and force UI refresh
                    self.load_real_workspaces().await;
                    self.ui_needs_refresh = true;
                }
                AsyncAction::FetchContainerLogs(session_id) => {
                    info!("Fetching container logs for session {}", session_id);
                    if let Err(e) = self.fetch_container_logs(session_id).await {
                        warn!(
                            "Failed to fetch container logs for session {}: {}",
                            session_id, e
                        );
                    }
                    self.ui_needs_refresh = true;
                }
                AsyncAction::AttachToContainer(session_id) => {
                    info!("Attaching to container for session {}", session_id);
                    if let Err(e) = self.attach_to_container(session_id).await {
                        error!(
                            "Failed to attach to container for session {}: {}",
                            session_id, e
                        );
                    }
                    self.ui_needs_refresh = true;
                }
                AsyncAction::AttachToTmuxSession(_session_id) => {
                    // NOTE: This action must be handled in main.rs where terminal access is available
                    // The terminal handle is needed to call attach_to_tmux_session
                    warn!("AttachToTmuxSession action should be handled in main loop, not here");
                    self.ui_needs_refresh = true;
                }
                AsyncAction::KillContainer(session_id) => {
                    info!("Killing container for session {}", session_id);
                    if let Err(e) = self.kill_container(session_id).await {
                        error!("Failed to kill container for session {}: {}", session_id, e);
                    }
                    self.ui_needs_refresh = true;
                }
                AsyncAction::AuthSetupOAuth => {
                    info!("Starting OAuth authentication setup");
                    if let Err(e) = self.run_oauth_setup().await {
                        error!("Failed to setup OAuth authentication: {}", e);
                        if let Some(ref mut auth_state) = self.auth_setup_state {
                            auth_state.error_message = Some(format!("OAuth setup failed: {}", e));
                            auth_state.is_processing = false;
                        }
                    }
                }
                AsyncAction::AuthSetupApiKey => {
                    info!("Saving API key authentication");
                    if let Err(e) = self.save_api_key().await {
                        error!("Failed to save API key: {}", e);
                        if let Some(ref mut auth_state) = self.auth_setup_state {
                            auth_state.error_message =
                                Some(format!("Failed to save API key: {}", e));
                            auth_state.is_processing = false;
                        }
                    }
                }
                AsyncAction::ReauthenticateCredentials => {
                    info!("Starting re-authentication process");
                    if let Err(e) = self.handle_reauthenticate().await {
                        error!("Failed to re-authenticate: {}", e);
                    }
                }
                AsyncAction::RestartSession(session_id) => {
                    info!("Starting session restart for session {}", session_id);
                    if let Err(e) = self.handle_restart_session(session_id).await {
                        error!("Failed to restart session: {}", e);
                    }
                }
                AsyncAction::CleanupOrphaned => {
                    info!("Starting cleanup of orphaned containers");
                    if let Err(e) = self.cleanup_orphaned_containers().await {
                        error!("Failed to cleanup orphaned containers: {}", e);
                        self.add_error_notification(format!(
                            "❌ Failed to cleanup orphaned containers: {}",
                            e
                        ));
                    }
                }
                // Terminal actions - must be handled in main.rs where terminal access is available
                // PUT THE ACTION BACK so main loop can handle it
                action @ AsyncAction::AttachToOtherTmux(_) => {
                    debug!("AttachToOtherTmux action deferred to main loop");
                    self.pending_async_action = Some(action);
                }
                action @ AsyncAction::KillOtherTmux(_) => {
                    debug!("KillOtherTmux action deferred to main loop");
                    self.pending_async_action = Some(action);
                }
                action @ AsyncAction::KillOtherTmuxSessions(_) => {
                    debug!("KillOtherTmuxSessions action deferred to main loop");
                    self.pending_async_action = Some(action);
                }
                AsyncAction::ConfirmOtherTmuxRename => {
                    info!("Executing Other tmux rename");
                    match self.confirm_other_tmux_rename().await {
                        Ok(()) => {
                            self.add_success_notification(
                                "Session renamed successfully".to_string(),
                            );
                            self.ui_needs_refresh = true;
                        }
                        Err(e) => {
                            warn!("Failed to rename session: {}", e);
                            self.add_error_notification(format!("Rename failed: {}", e));
                        }
                    }
                }
                action @ AsyncAction::OpenWorkspaceShell { .. } => {
                    debug!("OpenWorkspaceShell action deferred to main loop");
                    self.pending_async_action = Some(action);
                }
                action @ AsyncAction::OpenShellAtPath(_) => {
                    debug!("OpenShellAtPath action deferred to main loop");
                    self.pending_async_action = Some(action);
                }
                action @ AsyncAction::CreateSshSession => {
                    debug!("CreateSshSession action deferred to main loop");
                    self.pending_async_action = Some(action);
                }
                action @ AsyncAction::KillWorkspaceShell(_) => {
                    debug!("KillWorkspaceShell action deferred to main loop");
                    self.pending_async_action = Some(action);
                }
                action @ AsyncAction::OpenInEditor(_) => {
                    debug!("OpenInEditor action deferred to main loop");
                    self.pending_async_action = Some(action);
                }
                AsyncAction::OnboardingCheckDeps => {
                    info!("Running onboarding dependency check");
                    use crate::components::onboarding::DependencyChecker;
                    // Run blocking I/O on dedicated thread pool to avoid blocking async runtime
                    match tokio::task::spawn_blocking(DependencyChecker::check_all).await {
                        Ok(status) => {
                            if let Some(ref mut onboarding_state) = self.onboarding_state {
                                onboarding_state.dependency_status = Some(status);
                                onboarding_state.dependency_check_running = false;
                                self.ui_needs_refresh = true;
                            }
                        }
                        Err(e) => {
                            warn!("Dependency check task failed: {}", e);
                            if let Some(ref mut onboarding_state) = self.onboarding_state {
                                onboarding_state.dependency_check_running = false;
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Run OAuth authentication setup
    async fn run_oauth_setup(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        use crossterm::{
            execute,
            terminal::{LeaveAlternateScreen, disable_raw_mode},
        };

        // Create auth directory
        let home_dir = dirs::home_dir().ok_or("Could not determine home directory")?;
        let auth_dir = home_dir.join(".agents-in-a-box/auth");

        info!("Creating auth directory: {}", auth_dir.display());
        std::fs::create_dir_all(&auth_dir)?;

        // Update UI state to show we're starting
        if let Some(ref mut auth_state) = self.auth_setup_state {
            auth_state.is_processing = true;
            auth_state.error_message = Some("Preparing authentication setup...".to_string());
        }

        // First check if Docker is available
        if !self.is_docker_available().await {
            warn!("Docker is not available or not running");
            if let Some(ref mut auth_state) = self.auth_setup_state {
                auth_state.error_message = Some(
                    "❌ Docker is not available\n\n\
                     Please start Docker and try again."
                        .to_string(),
                );
                auth_state.is_processing = false;
            }
            return Err("Docker not available".into());
        }

        // Check if image exists
        let image_name = "agents-box:agents-dev";
        let image_check = std::process::Command::new("docker")
            .args(["image", "inspect", image_name])
            .output()?;

        if !image_check.status.success() {
            info!("Building agents-dev image...");
            let build_status = std::process::Command::new("docker")
                .args(["build", "-t", image_name, "docker/agents-dev"])
                .status()?;

            if !build_status.success() {
                if let Some(ref mut auth_state) = self.auth_setup_state {
                    auth_state.error_message = Some(
                        "❌ Failed to build claude-dev image\n\n\
                         Please check Docker and try again."
                            .to_string(),
                    );
                    auth_state.is_processing = false;
                }
                return Err("Failed to build image".into());
            }
        }

        // Temporarily exit TUI to run interactive container
        info!("Exiting TUI to run interactive authentication");

        // Disable raw mode and restore terminal
        let _ = disable_raw_mode();
        let _ = execute!(std::io::stdout(), LeaveAlternateScreen);

        println!("\n🔐 Claude Authentication Setup\n");
        println!("This will guide you through the OAuth authentication process.");
        println!("You'll be prompted to open a URL in your browser to complete authentication.\n");

        // Run the auth container interactively
        // Use inherit for stdin/stdout/stderr to ensure proper TTY forwarding
        let status = std::process::Command::new("docker")
            .args([
                "run",
                "--rm",
                "-it",
                "-v",
                &format!("{}:/home/claude-user/.claude", auth_dir.display()),
                "-e",
                "PATH=/home/claude-user/.npm-global/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
                "-e",
                "HOME=/home/claude-user",
                "-e",
                "AUTH_METHOD=oauth",  // Specify OAuth method
                "-w",
                "/home/claude-user",
                "--user",
                "claude-user",
                "--entrypoint",
                "bash",
                image_name,
                "-c",
                "/app/scripts/auth-setup.sh",
            ])
            .stdin(std::process::Stdio::inherit())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .status()?;

        // Check if authentication was successful
        let credentials_path = auth_dir.join(".credentials.json");
        let success =
            status.success() && credentials_path.exists() && credentials_path.metadata()?.len() > 0;

        if success {
            println!("\n✅ Authentication successful!");
            println!("Press Enter to continue...");
            let _ = std::io::stdin().read_line(&mut String::new());

            // Success - transition to main view
            self.auth_setup_state = None;
            self.current_screen = screen_ids::SESSION_LIST.to_string();
            self.check_current_directory_status();
            self.pending_async_action = Some(AsyncAction::RefreshWorkspaces);
        } else {
            println!("\n❌ Authentication failed!");
            println!("Press Enter to return to the authentication menu...");
            let _ = std::io::stdin().read_line(&mut String::new());

            if let Some(ref mut auth_state) = self.auth_setup_state {
                auth_state.error_message = Some(
                    "❌ Authentication failed\n\n\
                     Please try again or use API Key method."
                        .to_string(),
                );
                auth_state.is_processing = false;
            }
        }

        // Re-enable raw mode and return to TUI
        use crossterm::terminal::{EnterAlternateScreen, enable_raw_mode};
        let _ = enable_raw_mode();
        let _ = execute!(std::io::stdout(), EnterAlternateScreen);

        // Force UI refresh
        self.ui_needs_refresh = true;

        Ok(())
    }

    /// Check if Docker is available and running (synchronous, static version)
    pub fn is_docker_available_sync() -> bool {
        use std::process::{Command, Stdio};

        // Spawn the process and wait with a timeout to avoid hanging
        // when Docker Desktop is installed but not running
        match Command::new("docker")
            .arg("info")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(mut child) => {
                // Wait up to 3 seconds for docker info to respond
                let start = std::time::Instant::now();
                let timeout = std::time::Duration::from_secs(3);
                loop {
                    match child.try_wait() {
                        Ok(Some(status)) => return status.success(),
                        Ok(None) => {
                            if start.elapsed() > timeout {
                                let _ = child.kill();
                                warn!("docker info timed out after 3s - Docker not available");
                                return false;
                            }
                            std::thread::sleep(std::time::Duration::from_millis(50));
                        }
                        Err(_) => return false,
                    }
                }
            }
            Err(_) => false,
        }
    }

    /// Check if Docker is available and running
    async fn is_docker_available(&self) -> bool {
        // Use spawn + timeout to avoid hanging when Docker daemon isn't responding
        match std::process::Command::new("docker")
            .args(["version", "--format", "{{.Server.Version}}"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
        {
            Ok(child) => {
                // Wrap in tokio timeout
                match tokio::time::timeout(
                    std::time::Duration::from_secs(3),
                    tokio::task::spawn_blocking(move || child.wait_with_output()),
                )
                .await
                {
                    Ok(Ok(Ok(output))) => {
                        if output.status.success() {
                            let version = String::from_utf8_lossy(&output.stdout);
                            info!("Docker is available, version: {}", version.trim());
                            true
                        } else {
                            let error = String::from_utf8_lossy(&output.stderr);
                            warn!("Docker command failed: {}", error);
                            false
                        }
                    }
                    Ok(Ok(Err(e))) => {
                        warn!("Docker command error: {}", e);
                        false
                    }
                    Ok(Err(e)) => {
                        warn!("Docker task join error: {}", e);
                        false
                    }
                    Err(_) => {
                        warn!("Docker version timed out after 3s - Docker not available");
                        false
                    }
                }
            }
            Err(e) => {
                warn!("Docker not found or not accessible: {}", e);
                false
            }
        }
    }

    /// Save API key authentication
    async fn save_api_key(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let api_key = match &self.auth_setup_state {
            Some(auth_state) => auth_state.api_key_input.clone(),
            None => return Err("No API key to save".into()),
        };

        // Validate API key format
        if !api_key.starts_with("sk-") || api_key.len() < 20 {
            return Err("Invalid API key format".into());
        }

        // Create .env file in agents-in-a-box directory
        let home_dir = dirs::home_dir().ok_or("Could not determine home directory")?;
        let claude_box_dir = home_dir.join(".agents-in-a-box");
        std::fs::create_dir_all(&claude_box_dir)?;

        let env_path = claude_box_dir.join(".env");
        std::fs::write(&env_path, format!("ANTHROPIC_API_KEY={}\n", api_key))?;

        info!("API key saved to {:?}", env_path);

        // Success - transition to main view
        self.auth_setup_state = None;
        self.current_screen = screen_ids::SESSION_LIST.to_string();
        self.check_current_directory_status();
        self.pending_async_action = Some(AsyncAction::RefreshWorkspaces);

        Ok(())
    }

    /// Handle re-authentication of Claude credentials
    async fn handle_reauthenticate(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        // Check if any sessions are currently running
        let running_session_count =
            self.workspaces.iter().map(|w| w.running_sessions().len()).sum::<usize>();

        if running_session_count > 0 {
            warn!(
                "Found {} running sessions - re-authentication will affect them",
                running_session_count
            );

            // For now, we'll show an error and require manual session cleanup
            // TODO: Add confirmation dialog with option to stop sessions automatically
            if let Some(ref mut auth_state) = self.auth_setup_state {
                auth_state.error_message = Some(format!(
                    "❌ Cannot re-authenticate with {} running sessions\n\n\
                     Running sessions use the current credentials.\n\
                     Please stop all sessions before re-authenticating.\n\n\
                     Use 'd' to delete sessions or wait for them to complete.",
                    running_session_count
                ));
                auth_state.is_processing = false;
            } else {
                // Create auth state to show the error
                self.auth_setup_state = Some(AuthSetupState {
                    selected_method: AuthMethod::OAuth,
                    api_key_input: String::new(),
                    is_processing: false,
                    show_cursor: false,
                    error_message: Some(format!(
                        "❌ Cannot re-authenticate with {} running sessions\n\n\
                         Running sessions use the current credentials.\n\
                         Please stop all sessions before re-authenticating.\n\n\
                         Use 'd' to delete sessions or wait for them to complete.",
                        running_session_count
                    )),
                });
                self.current_screen = screen_ids::AUTH_SETUP.to_string();
            }
            return Ok(());
        }

        // No running sessions - safe to proceed with re-authentication
        info!("No running sessions found - proceeding with re-authentication");

        // Create backup of existing credentials
        let home_dir = dirs::home_dir().ok_or("Could not determine home directory")?;
        let auth_dir = home_dir.join(".agents-in-a-box/auth");

        let credentials_path = auth_dir.join(".credentials.json");
        let claude_json_path = auth_dir.join(".claude.json");
        let backup_suffix = format!(".backup-{}", chrono::Utc::now().timestamp());

        // Create backups if files exist
        if credentials_path.exists() {
            let backup_path = credentials_path.with_extension(&format!("json{}", backup_suffix));
            std::fs::copy(&credentials_path, &backup_path)?;
            info!("Backed up credentials to {:?}", backup_path);
        }

        if claude_json_path.exists() {
            let backup_path = claude_json_path.with_extension(&format!("json{}", backup_suffix));
            std::fs::copy(&claude_json_path, &backup_path)?;
            info!("Backed up claude.json to {:?}", backup_path);
        }

        // Remove existing credentials to trigger re-authentication
        if credentials_path.exists() {
            std::fs::remove_file(&credentials_path)?;
            info!("Removed existing credentials");
        }

        if claude_json_path.exists() {
            std::fs::remove_file(&claude_json_path)?;
            info!("Removed existing claude.json");
        }

        // Initialize auth setup state and switch to auth view
        self.auth_setup_state = Some(AuthSetupState {
            selected_method: AuthMethod::OAuth, // Default to OAuth
            api_key_input: String::new(),
            is_processing: false,
            show_cursor: false,
            error_message: Some(
                "🔄 Previous credentials cleared - please authenticate again".to_string(),
            ),
        });
        self.current_screen = screen_ids::AUTH_SETUP.to_string();

        info!("Re-authentication initiated - switched to auth setup view");
        Ok(())
    }

    async fn handle_restart_session(
        &mut self,
        session_id: Uuid,
    ) -> Result<(), Box<dyn std::error::Error>> {
        info!("Initiating restart UI flow for session {}", session_id);

        // Find the session in our workspace list
        let session_info = self.workspaces.iter().find_map(|workspace| {
            workspace
                .sessions
                .iter()
                .find(|s| s.id == session_id)
                .map(|session| (workspace, session))
        });

        if let Some((workspace, session)) = session_info {
            match &session.status {
                crate::models::SessionStatus::Stopped => {
                    info!(
                        "Session {} is stopped, starting restart UI flow",
                        session_id
                    );

                    // Start the new session UI flow with pre-populated data from the existing session
                    self.current_screen = screen_ids::NEW_SESSION.to_string();
                    self.new_session_state = Some(NewSessionState {
                        available_repos: vec![workspace.path.clone()],
                        filtered_repos: vec![(0, workspace.path.clone())],
                        selected_repo_index: Some(0),
                        branch_name: session.branch_name.clone(),
                        step: NewSessionStep::InputBranch, // Start at branch input since repo is pre-selected (skip agent selection for restart)
                        filter_text: String::new(),
                        is_current_dir_mode: false,
                        skip_permissions: session.skip_permissions,
                        mode: session.mode.clone(),
                        boss_prompt: if let Some(ref prompt) = session.boss_prompt {
                            TextEditor::from_string(prompt)
                        } else {
                            TextEditor::new()
                        },
                        file_finder: FuzzyFileFinderState::new(),
                        restart_session_id: Some(session_id), // Mark this as a restart operation
                        // Agent selection - default to Claude for restart
                        selected_agent: SessionAgentType::Claude,
                        agent_options: SessionAgentOption::all(),
                        selected_agent_index: 0,
                        // Model selection - use session's model or default to Sonnet
                        selected_model: session.model.unwrap_or_default(),
                        model_options: crate::models::ClaudeModel::all(),
                        selected_model_index: 0,
                        agent_model_focus: AgentModelFocus::default(),
                        // Remote repo fields - not used for restart
                        ..Default::default()
                    });

                    self.add_info_notification(
                        "🔄 Restarting session - review and update settings as needed".to_string(),
                    );
                }
                crate::models::SessionStatus::Idle => {
                    info!(
                        "Session {} is idle (tmux running but CLI stopped), restarting CLI in tmux",
                        session_id
                    );

                    // For Idle sessions, we restart the original CLI within the existing tmux session
                    match self.restart_cli_in_tmux(session_id).await {
                        Ok(name) => {
                            self.add_success_notification(format!(
                                "✓ {} restarted successfully",
                                name
                            ));
                        }
                        Err(e) => {
                            error!(
                                "Failed to restart CLI in tmux for session {}: {}",
                                session_id, e
                            );
                            self.add_error_notification(format!(
                                "❌ Failed to restart CLI: {}",
                                e
                            ));
                        }
                    }
                }
                status => {
                    warn!(
                        "Cannot restart session {} - current status: {:?}",
                        session_id, status
                    );
                    self.add_error_notification(format!(
                        "❌ Cannot restart session - current status: {:?}",
                        status
                    ));
                }
            }
        } else {
            error!("Session {} not found in workspaces", session_id);
            self.add_error_notification("❌ Session not found".to_string());
        }

        Ok(())
    }

    pub fn show_git_view(&mut self) {
        // Get the selected session's workspace path
        if let Some(session) = self.get_selected_session() {
            let worktree_path = std::path::PathBuf::from(&session.workspace_path);
            let mut git_state = crate::components::GitViewState::new(worktree_path);

            // Refresh git status
            if let Err(e) = git_state.refresh_git_status() {
                tracing::error!("Failed to refresh git status: {}", e);
                return;
            }

            self.git_view_state = Some(git_state);
            // Store current view so we can return to it
            self.previous_screen = Some(self.current_screen.clone());
            self.current_screen = screen_ids::GIT_VIEW.to_string();
        } else {
            tracing::warn!("No session selected for git view");
        }
    }

    pub fn git_commit_and_push(&mut self) {
        let result = if let Some(git_state) = self.git_view_state.as_mut() {
            git_state.commit_and_push()
        } else {
            return;
        };

        match result {
            Ok(message) => {
                tracing::info!("Git commit and push successful: {}", message);
                // Set pending event to be processed in next loop iteration
                self.pending_event = Some(crate::app::events::AppEvent::GitCommitSuccess(message));
                // Refresh git status after successful push
                if let Some(git_state) = self.git_view_state.as_mut() {
                    if let Err(e) = git_state.refresh_git_status() {
                        tracing::error!("Failed to refresh git status after push: {}", e);
                        self.add_warning_notification(
                            "⚠️ Push successful but failed to refresh git status".to_string(),
                        );
                    }
                }
            }
            Err(e) => {
                tracing::error!("Git commit and push failed: {}", e);
                self.add_error_notification(format!("❌ Git push failed: {}", e));
            }
        }
    }

    // Quick commit dialog methods
    pub fn is_in_quick_commit_mode(&self) -> bool {
        self.quick_commit_message.is_some()
    }

    pub fn start_quick_commit(&mut self) {
        // Only start quick commit if we have a selected session and it's in a git repository
        if let Some(session) = self.get_selected_session() {
            // Check if the workspace path is a git repository
            let workspace_path = std::path::Path::new(&session.workspace_path);
            let git_dir = workspace_path.join(".git");

            if git_dir.exists() {
                self.quick_commit_message = Some(String::new());
                self.quick_commit_cursor = 0;
                self.add_info_notification(
                    "📝 Enter commit message and press Enter to commit & push".to_string(),
                );
            } else {
                self.add_warning_notification(
                    "⚠️ Selected workspace is not a git repository".to_string(),
                );
            }
        } else {
            self.add_warning_notification("⚠️ No session selected".to_string());
        }
    }

    pub fn cancel_quick_commit(&mut self) {
        self.quick_commit_message = None;
        self.quick_commit_cursor = 0;
        self.add_info_notification("❌ Quick commit cancelled".to_string());
    }

    pub fn add_char_to_quick_commit(&mut self, ch: char) {
        if let Some(ref mut message) = self.quick_commit_message {
            message.insert(self.quick_commit_cursor, ch);
            self.quick_commit_cursor += 1;
        }
    }

    pub fn backspace_quick_commit(&mut self) {
        if let Some(ref mut message) = self.quick_commit_message {
            if self.quick_commit_cursor > 0 {
                self.quick_commit_cursor -= 1;
                message.remove(self.quick_commit_cursor);
            }
        }
    }

    pub fn move_quick_commit_cursor_left(&mut self) {
        if self.quick_commit_cursor > 0 {
            self.quick_commit_cursor -= 1;
        }
    }

    pub fn move_quick_commit_cursor_right(&mut self) {
        if let Some(ref message) = self.quick_commit_message {
            if self.quick_commit_cursor < message.len() {
                self.quick_commit_cursor += 1;
            }
        }
    }

    pub fn confirm_quick_commit(&mut self) {
        if let Some(ref message) = self.quick_commit_message {
            if message.trim().is_empty() {
                self.add_warning_notification("⚠️ Commit message cannot be empty".to_string());
                return;
            }

            // Perform the quick commit
            self.perform_quick_commit(message.trim().to_string());
        }
    }

    fn perform_quick_commit(&mut self, commit_message: String) {
        let worktree_path = if let Some(session) = self.get_selected_session() {
            std::path::PathBuf::from(&session.workspace_path)
        } else {
            tracing::warn!("Quick commit failed: no session selected");
            self.add_error_notification("❌ No session selected for commit".to_string());
            self.quick_commit_message = None;
            self.quick_commit_cursor = 0;
            return;
        };

        // Use the shared git operations function - DRY compliance!
        match crate::git::operations::commit_and_push_changes(&worktree_path, &commit_message) {
            Ok(success_message) => {
                tracing::info!("Quick commit successful: {}", success_message);
                // Set pending event to be processed in next loop iteration
                self.pending_event = Some(crate::app::events::AppEvent::GitCommitSuccess(
                    success_message,
                ));
                // Clear quick commit state
                self.quick_commit_message = None;
                self.quick_commit_cursor = 0;
            }
            Err(e) => {
                tracing::error!("Quick commit failed: {}", e);
                self.add_error_notification(format!("❌ Quick commit failed: {}", e));
                // Keep quick commit dialog open so user can try again
            }
        }
    }

    /// Add a notification to the notification queue
    pub fn add_notification(&mut self, notification: Notification) {
        self.notifications.push(notification);
    }

    /// Add a success notification
    pub fn add_success_notification(&mut self, message: String) {
        self.add_notification(Notification::success(message));
    }

    /// Add an error notification
    pub fn add_error_notification(&mut self, message: String) {
        self.add_notification(Notification::error(message));
    }

    /// Add an info notification
    pub fn add_info_notification(&mut self, message: String) {
        self.add_notification(Notification::info(message));
    }

    /// Add a warning notification
    pub fn add_warning_notification(&mut self, message: String) {
        self.add_notification(Notification::warning(message));
    }

    /// Remove expired notifications
    pub fn cleanup_expired_notifications(&mut self) {
        self.notifications.retain(|n| !n.is_expired());
    }

    /// Get current notifications (non-expired)
    pub fn get_current_notifications(&self) -> Vec<&Notification> {
        self.notifications.iter().filter(|n| !n.is_expired()).collect()
    }

    // ============================================================================
    // Tmux Integration Methods
    // ============================================================================

    /// Start background task to update tmux preview content every 100ms
    /// NOTE: This is now handled via the main update loop calling update_tmux_previews()
    /// This method is kept for compatibility but does nothing
    pub fn start_preview_updates(&mut self) {
        // Preview updates are now handled by calling update_tmux_previews() from main loop
        // No background task needed
        info!("Preview updates will be handled via main update loop");
    }

    /// Stop the preview update task
    pub fn stop_preview_updates(&mut self) {
        if let Some(task) = self.preview_update_task.take() {
            task.abort();
        }
    }

    /// Update preview content for all tmux sessions (called from main update loop)
    pub async fn update_tmux_previews(&mut self) -> anyhow::Result<()> {
        use crate::tmux::ClaudeProcessDetector;
        use crate::tmux::capture::{CaptureOptions, capture_pane};

        // THROTTLE: Only update previews every 5 seconds (not every 250ms tick)
        // This prevents spawning N tmux capture-pane subprocesses per tick
        const PREVIEW_INTERVAL_SECS: u64 = 5;
        let now = std::time::Instant::now();
        if let Some(last) = self.last_preview_update {
            if now.duration_since(last).as_secs() < PREVIEW_INTERVAL_SECS {
                return Ok(());
            }
        }
        self.last_preview_update = Some(now);

        // updates: (session_id, content, claude_running) for the selected session
        let mut updates = Vec::new();
        // status_updates: (session_id, claude_running) for non-selected sessions (status only)
        let mut status_updates = Vec::new();
        let detector = ClaudeProcessDetector::new();

        // OPTIMIZATION: Only capture the SELECTED session's full preview.
        // For all other sessions, just do a quick status check (visible area only).
        let selected_session_id = self.get_selected_session_id();

        for (session_id, tmux_session) in &self.tmux_sessions {
            let should_update = self
                .workspaces
                .iter()
                .flat_map(|w| &w.sessions)
                .find(|s| s.id == *session_id)
                .map(|s| !s.is_attached)
                .unwrap_or(false);

            if !should_update {
                continue;
            }

            let is_selected = selected_session_id == Some(*session_id);

            if is_selected {
                // Selected session: capture last 200 lines (not full history)
                // Full history can be megabytes for long-running sessions
                let opts = CaptureOptions {
                    start_line: Some("-200".to_string()),
                    end_line: Some("-".to_string()),
                    include_escape_sequences: true,
                    join_wrapped_lines: true,
                };
                match capture_pane(tmux_session.name(), opts).await {
                    Ok(content) => {
                        let claude_running = detector.has_claude_status_bar(&content);
                        updates.push((*session_id, content, claude_running));
                    }
                    Err(e) => {
                        debug!("Failed to capture selected session {}: {}", session_id, e);
                    }
                }
            } else {
                // Non-selected sessions: only capture visible area for status detection
                // Much cheaper — just the last screenful (~50 lines)
                match tmux_session.capture_pane_content().await {
                    Ok(content) => {
                        let claude_running = detector.has_claude_status_bar(&content);
                        status_updates.push((*session_id, claude_running));
                    }
                    Err(e) => {
                        trace!("Non-selected session {} capture skipped: {}", session_id, e);
                    }
                }
            }
        }

        // Apply status-only updates for non-selected sessions
        for (session_id, claude_running) in status_updates {
            if let Some(session) = self.find_session_mut(session_id) {
                use crate::models::SessionStatus;
                let new_status = if claude_running {
                    SessionStatus::Running
                } else {
                    SessionStatus::Idle
                };
                if session.status != new_status {
                    session.set_status(new_status);
                    self.ui_needs_refresh = true;
                }
            }
        }

        // Apply updates for the selected session
        for (session_id, content, claude_running) in updates {
            if let Some(session) = self.find_session_mut(session_id) {
                session.set_preview(content);

                use crate::models::SessionStatus;
                let new_status = if claude_running {
                    SessionStatus::Running
                } else {
                    SessionStatus::Idle
                };

                if session.status != new_status {
                    session.set_status(new_status);
                }

                self.ui_needs_refresh = true;
            }
        }

        // Update shell session preview (only the selected workspace's shell)
        let selected_workspace_idx = self.selected_workspace_index;
        if let Some(ws_idx) = selected_workspace_idx {
            if let Some(tmux_name) = self
                .workspaces
                .get(ws_idx)
                .and_then(|w| w.shell_session.as_ref())
                .map(|s| s.tmux_session_name.clone())
            {
                let opts = CaptureOptions {
                    start_line: Some("-100".to_string()),
                    end_line: Some("-".to_string()),
                    include_escape_sequences: true,
                    join_wrapped_lines: true,
                };
                match capture_pane(&tmux_name, opts).await {
                    Ok(content) => {
                        if let Some(workspace) = self.workspaces.get_mut(ws_idx) {
                            if let Some(shell) = workspace.shell_session.as_mut() {
                                shell.preview_content = Some(content);
                                self.ui_needs_refresh = true;
                            }
                        }
                    }
                    Err(e) => {
                        debug!(
                            "Failed to capture shell session content for {}: {}",
                            tmux_name, e
                        );
                    }
                }
            }
        }

        Ok(())
    }

    /// Restart Claude in an existing tmux session (for Idle sessions)
    async fn restart_cli_in_tmux(&mut self, session_id: Uuid) -> anyhow::Result<String> {
        use crate::config::CliProvider;
        use crate::models::session::SessionAgentType;
        use anyhow::Context;
        use std::process::Command;

        let session = self
            .find_session(session_id)
            .ok_or_else(|| anyhow::anyhow!("Session not found"))?;

        let tmux_session_name = session
            .tmux_session_name
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No tmux session associated with this session"))?
            .clone();

        let workspace_path = session.workspace_path.clone();
        let skip_permissions = session.skip_permissions;
        let agent_type = session.agent_type;

        let provider = match agent_type {
            SessionAgentType::Claude => CliProvider::Claude,
            SessionAgentType::Codex => CliProvider::Codex,
            SessionAgentType::Gemini => CliProvider::Gemini,
            SessionAgentType::Copilot => CliProvider::Copilot,
            SessionAgentType::Shell | SessionAgentType::Ssh | SessionAgentType::Kiro => {
                anyhow::bail!("Restart unsupported for agent type {:?}", agent_type);
            }
        };

        let mut cmd_parts = vec![provider.command().to_string()];
        if skip_permissions {
            cmd_parts.push(provider.skip_permissions_flag().to_string());
        }
        let cli_cmd = cmd_parts.join(" ");

        info!(
            "Restarting {} in tmux session '{}' for workspace '{}' (cmd: {})",
            provider.display_name(),
            tmux_session_name,
            workspace_path,
            cli_cmd
        );

        let output = Command::new("tmux")
            .args(&["send-keys", "-t", &tmux_session_name, &cli_cmd, "C-m"])
            .output()
            .context("Failed to send CLI restart command to tmux")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Failed to send command to tmux: {}", stderr);
        }

        if let Some(session) = self.find_session_mut(session_id) {
            session.set_status(crate::models::SessionStatus::Running);
        }

        info!(
            "Successfully sent {} restart command to tmux session '{}'",
            provider.display_name(),
            tmux_session_name
        );
        Ok(provider.display_name().to_string())
    }

    /// Helper to find a session by ID across all workspaces
    fn find_session(&self, session_id: uuid::Uuid) -> Option<&crate::models::session::Session> {
        for workspace in &self.workspaces {
            for session in &workspace.sessions {
                if session.id == session_id {
                    return Some(session);
                }
            }
        }
        None
    }

    /// Helper to find a mutable session by ID across all workspaces
    fn find_session_mut(
        &mut self,
        session_id: uuid::Uuid,
    ) -> Option<&mut crate::models::session::Session> {
        for workspace in &mut self.workspaces {
            for session in &mut workspace.sessions {
                if session.id == session_id {
                    return Some(session);
                }
            }
        }
        None
    }
}

pub struct App {
    pub state: AppState,
    /// Owning handle to the plugin runtime's tokio executor. Held by `App`
    /// so dropping `App` joins every plugin task and tears down the runtime.
    /// `None` until [`App::init`] runs. The cheap Send + Clone façade lives
    /// on `state.plugin_runtime` so dispatchers reach it without needing
    /// access to `App`.
    plugin_runtime_owner: Option<ainb_plugin_runtime::Runtime>,
}

impl App {
    pub fn new() -> Self {
        Self {
            state: AppState::new(),
            plugin_runtime_owner: None,
        }
    }

    /// Move the plugin runtime out so the caller can call
    /// [`ainb_plugin_runtime::Runtime::shutdown`] from a non-async
    /// context. Without this, dropping `App` inside `#[tokio::main]`
    /// trips the tokio "Cannot drop a runtime in a context where
    /// blocking is not allowed" panic on every clean exit. See the
    /// `Runtime::shutdown` doc for the wider picture.
    pub fn take_plugin_runtime(&mut self) -> Option<ainb_plugin_runtime::Runtime> {
        self.plugin_runtime_owner.take()
    }

    /// Drain any freshly-painted plugin frames into
    /// `state.pending_plugin_renders` so the next `terminal.draw` paints
    /// the latest buffer per plugin-owned screen.
    ///
    /// Architectural contract (enforced by `build.rs` lint): this method
    /// stays *synchronous* — plugin tasks render on the tokio runtime in
    /// the background, the TUI thread only ever `try_recv`s the cached
    /// frame and dispatches a fresh render request. No `.await` on the
    /// render thread, ever.
    ///
    /// Phase 7b: no subprocess plugins are packaged yet (Phase 7c reships
    /// burndown + session-reader as Rust subprocess binaries). Loop is
    /// no-op when discovery returned empty; once 7c lands, the screen
    /// routing table below populates again.
    pub fn tick_plugin_renders(&mut self) {
        // Clone the cheap Send + Clone handle so we can hold a reference
        // to the runtime while also mutably borrowing the various
        // `state.*` plugin caches below.
        let Some(handle) = self.state.plugin_runtime.clone() else {
            return;
        };

        // Static plugin-screen routing table. Pairs a stable screen id
        // (consumed by `PluginScreen` and matched against
        // `state.current_screen`) with the plugin id that owns it.
        const PLUGIN_SCREENS: &[(&str, &str)] = &[
            (crate::app::screens::ids::ANALYTICS, "burndown"),
        ];

        for (screen_id, plugin_id) in PLUGIN_SCREENS {
            let pid = ainb_plugin_runtime::PluginId::from(*plugin_id);

            // Skip plugins the runtime doesn't know about — keeps the
            // loop cheap and resilient when discovery comes up empty.
            if handle.lifecycle_state(&pid).is_none() {
                continue;
            }

            // Drain the cached frame (if any) into the screen map. The
            // plugin task pushes a fresh frame each time it returns from
            // `plugin/render`; `try_recv_render` is the non-blocking
            // hand-off the render thread relies on.
            if let Some(buf) = handle.try_recv_render(&pid) {
                self.state
                    .pending_plugin_renders
                    .insert((*screen_id).to_string(), buf);
            }

            // Kick the next render ONLY when something has actually
            // changed since the last paint — a keystroke landed
            // (`send_key`), a snapshot event arrived (host or plugin
            // `publish_snapshot`), or the screen has never painted
            // yet (registration seeds the flag to `true`).
            //
            // Before this gate the loop fired a render kick every
            // tick (~4/s at the 250 ms cadence) regardless of state
            // changes, which compounded with the per-tick `event::poll`
            // idle wait to produce ~250-300 ms of perceived lag per
            // keystroke. With the gate the kick is event-driven, so
            // each key arrives at the plugin within one tick interval
            // and the response paints on the *next* tick.
            if !handle.take_render_dirty(&pid) {
                continue;
            }

            // Viewport comes from the previous frame's allocated area
            // (stashed by `PluginScreen::render`). Falls back to (0, 0)
            // before the first paint — the plugin treats that as "use
            // your own fallback size", which keeps the first frame
            // sensible until the area cache fills in.
            let (width, height) = self
                .state
                .plugin_render_areas
                .get(*screen_id)
                .copied()
                .unwrap_or((0, 0));
            let viewport = ainb_plugin_runtime::Viewport { width, height };
            // Returned oneshot is intentionally dropped — the cache
            // pickup happens via `try_recv_render` next tick.
            let _ = handle.render(&pid, viewport, 0);
        }
    }

    pub async fn init(&mut self) {
        // Discover + register bundled plugins (best-effort). Each plugin
        // task lazy-spawns its subprocess on first command; the runtime
        // comes up cheap and stays empty when no plugins are installed.
        match crate::plugins::init_plugin_runtime() {
            Ok((runtime, handle, outcome)) => {
                if !outcome.loaded.is_empty() {
                    info!(loaded = ?outcome.loaded, "plugin runtime initialised");
                }
                for (name, err) in &outcome.failed {
                    warn!(plugin = %name, error = %err, "plugin failed to load");
                }
                self.plugin_runtime_owner = Some(runtime);
                self.state.plugin_runtime = Some(handle);
            }
            Err(e) => {
                warn!(error = %e, "plugin runtime init failed — running plugin-free");
            }
        }

        // Kick off the live-window background poller. Render path reads
        // from its snapshot — never calls live_window::current() inline.
        self.state.live_window_watcher.start();

        // Initialize log streaming coordinator
        let (mut coordinator, log_sender) = LogStreamingCoordinator::new();

        // Only initialize the streaming manager if Docker is available
        // (log streaming requires Docker for Boss mode containers)
        if AppState::is_docker_available_sync() {
            info!("Docker available - initializing log streaming manager");
            if let Err(e) = coordinator.init_manager(log_sender.clone()) {
                warn!("Failed to initialize log streaming manager: {}", e);
            } else {
                info!("Log streaming coordinator initialized successfully");
            }
        } else {
            info!("Docker not available - skipping log streaming manager initialization");
            info!("Log streaming will be available when Docker is started");
        }

        self.state.log_streaming_coordinator = Some(coordinator);
        self.state.log_sender = Some(log_sender);

        // Try to refresh OAuth tokens if they're expired (before checking first-time setup)
        let home_dir = dirs::home_dir();
        if let Some(home) = home_dir {
            let credentials_path =
                home.join(".agents-in-a-box").join("auth").join(".credentials.json");

            // Only attempt refresh if we have OAuth credentials that need refreshing
            // AND Docker is available (token refresh requires Docker for Boss mode)
            if credentials_path.exists() && AppState::oauth_token_needs_refresh(&credentials_path) {
                if AppState::is_docker_available_sync() {
                    info!("Docker available - attempting OAuth token refresh on startup");
                    match self.state.refresh_oauth_tokens().await {
                        Ok(()) => info!("OAuth tokens refreshed successfully on startup"),
                        Err(e) => warn!("Failed to refresh OAuth tokens: {}", e),
                    }
                } else {
                    info!(
                        "Docker not available - skipping OAuth token refresh (Boss mode will require Docker)"
                    );
                    // Don't show error - user might only use Interactive mode which doesn't need Docker
                }
            }
        }

        // REMOVED: Auth check moved to Boss mode selection only
        // Interactive mode should work without Docker authentication
        // Authentication is only required for Boss mode (Docker-based sessions)
        info!("App::init() - skipping upfront auth check (deferred to Boss mode selection)");

        // Always start with SessionList view
        info!("Starting with SessionList view (auth deferred until Boss mode)");
        // Initialize Claude integration
        if let Err(e) = self.state.init_claude_integration().await {
            warn!("Failed to initialize Claude integration: {}", e);
        }

        self.state.check_current_directory_status();

        // Start loading workspaces in the background (non-blocking)
        // This prevents the app from hanging if Docker is slow
        info!("Starting background workspace loading");
        let result_sender = self.state.start_background_workspace_loading();

        // Spawn the background loading task with timeout
        tokio::spawn(async move {
            let timeout_duration = Duration::from_secs(AppState::DOCKER_TIMEOUT_SECS);

            // Load workspaces with timeout
            let load_result = tokio::time::timeout(timeout_duration, load_workspaces_async()).await;

            let result = match load_result {
                Ok(Ok(workspaces)) => {
                    info!(
                        "Background workspace loading succeeded: {} workspaces",
                        workspaces.len()
                    );
                    WorkspaceLoadResult::Success(workspaces)
                }
                Ok(Err(e)) => {
                    warn!("Background workspace loading failed: {}", e);
                    WorkspaceLoadResult::Error(e.to_string())
                }
                Err(_) => {
                    warn!(
                        "Background workspace loading timed out after {}s",
                        AppState::DOCKER_TIMEOUT_SECS
                    );
                    WorkspaceLoadResult::Timeout
                }
            };

            // Send result (ignore error if receiver dropped)
            let _ = result_sender.send(result);
        });

        // Note: Log streaming will be initialized after workspaces are loaded
        // This happens in tick() when check_workspace_loading_complete() returns true
    }

    /// Initialize log streaming for all running sessions
    async fn init_log_streaming_for_sessions(&mut self) -> anyhow::Result<()> {
        if let Some(coordinator) = &mut self.state.log_streaming_coordinator {
            // Collect session info for streaming
            let sessions: Vec<(Uuid, String, String, crate::models::SessionMode)> = self
                .state
                .workspaces
                .iter()
                .flat_map(|w| &w.sessions)
                .filter(|s| s.status == crate::models::SessionStatus::Running)
                .filter_map(|s| {
                    s.container_id.clone().map(|container_id| {
                        (
                            s.id,
                            container_id,
                            format!("{}-{}", s.name, s.branch_name),
                            s.mode.clone(),
                        )
                    })
                })
                .collect();

            if !sessions.is_empty() {
                info!(
                    "Starting log streaming for {} running sessions",
                    sessions.len()
                );
                for (session_id, container_id, container_name, session_mode) in &sessions {
                    if let Err(e) = coordinator
                        .start_streaming(
                            *session_id,
                            container_id.clone(),
                            container_name.clone(),
                            session_mode.clone(),
                        )
                        .await
                    {
                        warn!(
                            "Failed to start log streaming for session {}: {}",
                            session_id, e
                        );
                    }
                }
            }
        }
        Ok(())
    }

    pub async fn tick(&mut self) -> anyhow::Result<()> {
        // Clean up expired notifications
        self.state.cleanup_expired_notifications();

        // Check for completed background workspace loading
        if self.state.check_workspace_loading_complete() {
            info!("Background workspace loading completed, initializing log streaming");
            // Now that workspaces are loaded, initialize log streaming
            if let Err(e) = self.init_log_streaming_for_sessions().await {
                warn!("Failed to initialize log streaming: {}", e);
            }
            // Also load other tmux sessions (quick operation)
            self.state.load_other_tmux_sessions().await;
            self.state.ui_needs_refresh = true;
        }

        // Check for completed background skills scan
        if self.state.check_skills_load_complete() {
            self.state.ui_needs_refresh = true;
        }

        // Periodic OAuth token refresh check (every 5 minutes)
        let now = Instant::now();
        let should_check_token = self
            .state
            .last_token_refresh_check
            .map(|last| now.duration_since(last).as_secs() >= 300) // Check every 5 minutes
            .unwrap_or(true); // First time

        if should_check_token {
            self.state.last_token_refresh_check = Some(now);

            // Check if we need to refresh OAuth tokens
            let home_dir = dirs::home_dir();
            if let Some(home) = home_dir {
                let credentials_path =
                    home.join(".agents-in-a-box").join("auth").join(".credentials.json");

                if credentials_path.exists()
                    && AppState::oauth_token_needs_refresh(&credentials_path)
                {
                    info!("OAuth token needs refresh (periodic check)");

                    // Only attempt refresh if Docker is available
                    if self.state.is_docker_available().await {
                        // Refresh tokens inline (this is quick enough not to block UI)
                        match self.state.refresh_oauth_tokens().await {
                            Ok(()) => {
                                info!("OAuth tokens refreshed successfully (periodic)");
                                // Add a notification to inform the user
                                self.state.add_notification(Notification {
                                    message: "✅ OAuth tokens refreshed automatically".to_string(),
                                    notification_type: NotificationType::Success,
                                    created_at: Instant::now(),
                                    duration: Duration::from_secs(5),
                                });
                            }
                            Err(e) => {
                                warn!("Failed to refresh OAuth tokens (periodic): {}", e);
                                // Add a warning notification
                                self.state.add_notification(Notification {
                                    message: format!("⚠️ Token refresh failed: {}", e),
                                    notification_type: NotificationType::Warning,
                                    created_at: Instant::now(),
                                    duration: Duration::from_secs(10),
                                });
                            }
                        }
                    } else {
                        info!("Docker not available - skipping periodic OAuth token refresh");
                    }
                }
            }
        }

        // Periodic session snapshot (every 30 minutes)
        let should_snapshot = self
            .state
            .last_snapshot_time
            .map(|last| now.duration_since(last).as_secs() >= 1800)
            .unwrap_or(true);

        if should_snapshot {
            self.state.last_snapshot_time = Some(now);
            tokio::spawn(async {
                match crate::app::snapshot::SnapshotManager::take_snapshot().await {
                    Ok(snapshot) => {
                        if let Err(e) =
                            crate::app::snapshot::SnapshotManager::save_snapshot(&snapshot).await
                        {
                            tracing::warn!("Failed to save session snapshot: {}", e);
                        } else if let Err(e) =
                            crate::app::snapshot::SnapshotManager::prune_snapshots(48).await
                        {
                            tracing::warn!("Failed to prune old snapshots: {}", e);
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Failed to take session snapshot: {}", e);
                    }
                }
            });
        }

        // Process incoming log entries (non-blocking)
        let mut log_entries = Vec::new();
        if let Some(coordinator) = &mut self.state.log_streaming_coordinator {
            // Collect all available log entries without blocking
            while let Some((session_id, log_entry)) = coordinator.try_next_log() {
                log_entries.push((session_id, log_entry));
            }
        }

        // Add log entries to the state
        for (session_id, log_entry) in log_entries {
            self.state.add_live_log(session_id, log_entry);
        }

        // Update tmux session previews for Interactive mode sessions
        // This captures pane content from tmux and updates session.preview_content
        if let Err(e) = self.state.update_tmux_previews().await {
            warn!("Failed to update tmux previews: {}", e);
        }

        // Process any pending async actions
        if self.state.pending_async_action.is_some() {
            info!(
                ">>> tick() detected pending_async_action: {:?}",
                self.state.pending_async_action
            );
        }
        match self.state.process_async_action().await {
            Ok(()) => {
                if self.state.pending_async_action.is_some() {
                    info!(
                        ">>> After process_async_action, still pending: {:?}",
                        self.state.pending_async_action
                    );
                }
            }
            Err(e) => {
                warn!("Error processing async action: {}", e);
                // Return to safe state if there was an error
                // BUT don't interrupt onboarding wizard or setup menu
                if self.state.current_screen != screen_ids::ONBOARDING
                    && self.state.current_screen != screen_ids::SETUP_MENU
                {
                    self.state.new_session_state = None;
                    self.state.current_screen = screen_ids::SESSION_LIST.to_string();
                }
                self.state.pending_async_action = None;
            }
        }

        // Update logic for the app (e.g., refresh container status)

        // Periodic log updates for attached sessions
        let now = Instant::now();
        let should_update_logs = self
            .state
            .last_log_check
            .map(|last| now.duration_since(last).as_secs() >= 3) // Update every 3 seconds
            .unwrap_or(true); // First time

        if should_update_logs {
            self.state.last_log_check = Some(now);

            // If we have an attached session, fetch its logs
            if let Some(attached_id) = self.state.attached_session_id {
                // Check if we should update this session's logs (don't spam updates)
                let should_update_session = self
                    .state
                    .log_last_updated
                    .get(&attached_id)
                    .map(|last| now.duration_since(*last).as_secs() >= 2) // Update session logs every 2 seconds
                    .unwrap_or(true);

                if should_update_session {
                    // Fetch logs in the background (don't block the UI)
                    if let Err(e) = self.state.fetch_claude_logs(attached_id).await {
                        warn!("Failed to fetch logs for session {}: {}", attached_id, e);
                    } else {
                        self.state.log_last_updated.insert(attached_id, now);
                        // Set flag to refresh UI with new logs
                        self.state.ui_needs_refresh = true;
                    }
                }
            }
        }

        Ok(())
    }

    /// Check if UI needs immediate refresh and clear the flag
    pub fn needs_ui_refresh(&mut self) -> bool {
        if self.state.ui_needs_refresh {
            self.state.ui_needs_refresh = false;
            true
        } else {
            false
        }
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

// Include the test module inline
#[cfg(test)]
#[path = "state_tests.rs"]
mod state_tests;
