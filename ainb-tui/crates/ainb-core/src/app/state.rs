// ABOUTME: Application state management and view switching logic for agents-in-a-box TUI

#![allow(dead_code)]

use crate::app::SessionLoader;
use crate::audit::{self, AuditResult, AuditTrigger};
use crate::claude::client::ClaudeChatManager;
use crate::claude::types::ClaudeStreamingEvent;
use crate::claude::{ClaudeApiClient, ClaudeMessage};
// Phase 6 (new-session redesign): FuzzyFileFinderState was used by the legacy
// boss-mode @-trigger; removed along with the prompt textarea.
use crate::components::home_screen_v2::HomeScreenV2State;
use crate::components::live_logs_stream::LogEntry;
use crate::config::{AppConfig, SshDisplayNameStore};
use crate::credentials;
use crate::docker::LogStreamingCoordinator;
use crate::editors;
// Phase 6 (new-session redesign): ParsedRepo / RemoteBranch / legacy
// `RepoSource` import retired with the legacy remote-clone flow.
use crate::models::{Session, SessionAgentType, Workspace};
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

    /// Return the editor contents as `Some(String)` iff non-empty, else `None`.
    ///
    /// Collapses the "empty prompt → no prompt" idiom that was inlined at
    /// multiple sites in `configure.rs` (finding #17). Callers that need
    /// `Option<&str>` can chain `.as_deref()`.
    #[must_use]
    pub fn to_non_empty_string(&self) -> Option<String> {
        let s = self.to_string();
        if s.is_empty() { None } else { Some(s) }
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
    Preview,  // Right pane - interactive embedded tmux attach (in-place)
}

pub const DEFAULT_SESSIONS_SIDEBAR_WIDTH: u16 = 40;
pub const MIN_SESSIONS_SIDEBAR_WIDTH: u16 = 24;
pub const SESSIONS_PREVIEW_RESERVE: u16 = 50;
pub const COLLAPSED_SESSIONS_SIDEBAR_WIDTH: u16 = 5;
pub const SESSIONS_ROW_DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(300);

impl AppState {
    /// Enter interactive mode: attach a live embed client to the selected
    /// session's tmux session and focus the preview pane. Returns false (no-op)
    /// if the selection has no tmux session or the attach fails — the pane stays
    /// on the read-only preview.
    pub fn enter_interactive_pane(&mut self, rows: u16, cols: u16) -> bool {
        if self.embed.is_some() {
            if self.selected_tmux_name() == self.embed_session {
                // Self-healing re-entry on the SAME row: a live embed with
                // focus drifted off the preview pane is a leaked state —
                // restore the mode invariant instead of silently no-opping.
                self.focused_pane = FocusedPane::Preview;
                return true;
            }
            // Different row: the user means "attach HERE" — release the stale
            // client and fall through to a fresh attach, instead of
            // refocusing an embed that renders some other session.
            self.release_interactive_pane();
        }
        let Some(name) = self.selected_tmux_name() else {
            self.add_warning_notification("No tmux session on this row".to_string());
            return false;
        };
        // tmux mirrors a session to every attached client, but all clients
        // fight over its size — attaching alongside an existing client is the
        // user's call, so allow it and warn (never block).
        let attached_elsewhere = self.selected_session_attached_elsewhere();
        match crate::tmux::EmbedClient::attach(&name, rows, cols) {
            Ok(client) => {
                self.embed = Some(client);
                self.embed_session = Some(name);
                self.focused_pane = FocusedPane::Preview;
                if attached_elsewhere {
                    self.add_warning_notification(
                        "Note: session attached elsewhere — screen sizes may fight".to_string(),
                    );
                }
                true
            }
            Err(e) => {
                tracing::warn!("failed to attach interactive embed to {name}: {e}");
                self.add_error_notification(format!("Live attach to '{name}' failed: {e}"));
                false
            }
        }
    }

    /// Whether the current selection's tmux session already has another client
    /// attached. Every kind that tracks attachment reports it (Claude and SSH
    /// rows via `is_attached`, other-tmux rows via tmux's attached flag);
    /// shell selections have no liveness flag and report false.
    fn selected_session_attached_elsewhere(&self) -> bool {
        if self.is_ssh_session_selected() {
            self.selected_ssh_session().map(|s| s.is_attached).unwrap_or(false)
        } else if self.shell_selected {
            false
        } else if self.is_other_tmux_selected() {
            self.selected_other_tmux_session().map(|s| s.attached).unwrap_or(false)
        } else {
            self.get_selected_session().map(|s| s.is_attached).unwrap_or(false)
        }
    }

    /// Release interactive mode: kill the ephemeral embed client (NEVER the tmux
    /// session) and return focus to the session list.
    pub fn release_interactive_pane(&mut self) {
        if let Some(mut client) = self.embed.take() {
            client.shutdown();
        }
        self.embed_session = None;
        self.embed_pane_area = None;
        if self.focused_pane == FocusedPane::Preview {
            self.focused_pane = FocusedPane::Sessions;
        }
    }

    /// Resolve the tmux session name for whatever is currently selected, across
    /// session kinds (Claude session, other-tmux, SSH, workspace shell). Mirrors
    /// the resolution in the `a` (AttachTmuxSession) handler so `l` attaches the
    /// same target `a` would.
    pub fn selected_tmux_name(&self) -> Option<String> {
        if self.is_ssh_session_selected() {
            self.selected_ssh_session().and_then(|s| s.tmux_session_name.clone())
        } else if self.is_other_tmux_selected() {
            self.selected_other_tmux_session().map(|s| s.name.clone())
        } else if self.shell_selected {
            self.selected_workspace_index
                .and_then(|i| self.workspaces.get(i))
                .and_then(|w| w.shell_session.as_ref())
                .map(|sh| sh.tmux_session_name.clone())
        } else {
            self.get_selected_session().and_then(|s| s.tmux_session_name.clone())
        }
    }

    /// True while an interactive embed is focused.
    pub fn is_interactive_pane(&self) -> bool {
        self.embed.is_some() && self.focused_pane == FocusedPane::Preview
    }

    /// If the embed has ended (detach / session gone / EOF), auto-release so the
    /// pane reverts to the read-only preview rather than a dead screen. Also
    /// releases defensively when the current screen is no longer the session
    /// list — keys must never be forwarded to an invisible PTY.
    ///
    /// Returns true when it released (the layout changed → repaint needed).
    pub fn poll_embed_exit(&mut self) -> bool {
        if self.embed.is_none() {
            return false;
        }
        let exited = self.embed.as_ref().is_some_and(|e| e.has_exited());
        let invisible = self.current_screen != screen_ids::SESSION_LIST;
        if exited || invisible {
            self.release_interactive_pane();
            if exited {
                self.add_info_notification("Live session ended — released".to_string());
            }
            return true;
        }
        false
    }

    /// New embed output since the last call? Clears the embed's dirty flag.
    /// The render loop polls this as a repaint trigger: live PTY output
    /// arrives without host input, so the dirty-gate (perf bead `wai`) would
    /// otherwise hold the pane at the 250ms animation floor.
    pub fn embed_take_dirty(&self) -> bool {
        self.embed.as_ref().is_some_and(|e| e.take_dirty())
    }
}

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
    InstallNotifyHooks, // Install the ainb-hooks notification plugin into Claude Code + Codex
    DismissNotifyPrompt, // Remember "don't ask again" for the notify-install prompt
    McpStopServer(String), // Stop one pooled MCP server (reaps its child)
    McpStopDaemon,     // Stop the whole MCP pool daemon
    SetupAbtopRateLimits, // Run `abtop --setup` (rate-limit StatusLine hook) then open abtop
    OpenAbtopSkipSetup, // Open abtop now without running `abtop --setup`
    DismissAbtopSetup, // Remember "don't ask again" for the abtop setup offer, then open abtop
    Cancel,            // No-op terminator for tri-option dialogs
}

// ============================================================================
// MCP Pool observability overlay
// ============================================================================

/// Result of one off-thread fetch of the daemon's control-socket `status`.
#[derive(Debug, Clone)]
pub struct McpFetchResult {
    pub daemon_running: bool,
    pub servers: Vec<crate::mcp_pool::proxy::ServerStatus>,
    pub error: Option<String>,
    /// One-shot status line for an action that produced this result (e.g.
    /// `import`). `None` for plain refreshes — the overlay keeps its prior
    /// `last_action` so a refresh doesn't wipe the import summary.
    pub action_msg: Option<String>,
}

/// Live, lazily-refreshed snapshot of the shared MCP pool. Present only while
/// the overlay is open; dropping it (on close) stops all refresh activity —
/// nothing polls the daemon when the overlay isn't showing.
pub struct McpOverlayState {
    pub pool_enabled: bool,
    pub daemon_running: bool,
    pub servers: Vec<crate::mcp_pool::proxy::ServerStatus>,
    pub selected: usize,
    pub loading: bool,
    pub last_refreshed: Option<std::time::Instant>,
    /// Auto-refresh cadence while open; 0 = on-open + manual (`r`) only.
    pub refresh_secs: u64,
    /// Receiver for the in-flight fetch (None when no fetch is pending — the
    /// one-outstanding-request guard).
    pub fetch_rx: Option<mpsc::UnboundedReceiver<McpFetchResult>>,
    /// Status line from the last in-overlay action (e.g. `import`). Sticky
    /// across plain refreshes; cleared only when the overlay closes.
    pub last_action: Option<String>,
}

impl std::fmt::Debug for McpOverlayState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpOverlayState")
            .field("pool_enabled", &self.pool_enabled)
            .field("daemon_running", &self.daemon_running)
            .field("servers", &self.servers.len())
            .field("selected", &self.selected)
            .field("loading", &self.loading)
            .field("fetch_pending", &self.fetch_rx.is_some())
            .field("last_action", &self.last_action)
            .finish()
    }
}

impl McpOverlayState {
    pub fn selected_server_name(&self) -> Option<String> {
        self.servers.get(self.selected).map(|s| s.name.clone())
    }
}

/// Blocking control-socket fetch — always run via `spawn_blocking`. Probes the
/// daemon and parses its `status` JSON into the snapshot.
pub(crate) fn mcp_fetch_blocking() -> McpFetchResult {
    if !crate::mcp_pool::client::daemon_alive() {
        return McpFetchResult {
            daemon_running: false,
            servers: Vec::new(),
            error: None,
            action_msg: None,
        };
    }
    match crate::mcp_pool::client::daemon_status() {
        Ok(json) => match serde_json::from_str::<serde_json::Value>(&json) {
            Ok(v) => {
                let servers = v
                    .get("servers")
                    .and_then(|s| serde_json::from_value(s.clone()).ok())
                    .unwrap_or_default();
                McpFetchResult {
                    daemon_running: true,
                    servers,
                    error: None,
                    action_msg: None,
                }
            }
            Err(e) => McpFetchResult {
                daemon_running: true,
                servers: Vec::new(),
                error: Some(format!("parse status: {e}")),
                action_msg: None,
            },
        },
        Err(e) => McpFetchResult {
            daemon_running: false,
            servers: Vec::new(),
            error: Some(e.to_string()),
            action_msg: None,
        },
    }
}

/// Blocking `import` action for the overlay — runs `ainb mcp import` (project
/// scope, or user config when `to_user`), then makes the freshly-imported
/// servers show up in the table immediately: if the pool daemon is already
/// running they're registered with it; if it isn't, the daemon is started
/// (it loads every configured server — including the new import — on boot).
/// Without this an import into a down pool wrote config but left the overlay
/// empty, which read as a no-op. Always run via `spawn_blocking`. Returns a
/// fresh status snapshot tagged with a summary.
pub(crate) fn mcp_import_blocking(to_user: bool) -> McpFetchResult {
    let summary = match crate::mcp_pool::import::execute(to_user) {
        Ok(report) => {
            let mut extra = String::new();
            if !report.imported.is_empty() {
                if crate::mcp_pool::client::daemon_alive() {
                    // Daemon up: push the new definitions so they appear now,
                    // without waiting for a new session.
                    if let Ok(config) = crate::config::AppConfig::load() {
                        let fresh: Vec<_> = crate::mcp_pool::pooled_servers(&config)
                            .into_iter()
                            .filter(|s| report.imported.contains(&s.name))
                            .collect();
                        if !fresh.is_empty() {
                            let _ = crate::mcp_pool::client::register_servers(&fresh);
                        }
                    }
                } else {
                    // Daemon down: start it. ensure_daemon spawns it detached
                    // and polls until its control socket is up (~3s), and the
                    // daemon registers every configured server on boot — so the
                    // just-imported one is live by the time we re-fetch below.
                    extra = match crate::mcp_pool::client::ensure_daemon() {
                        Ok(()) => " · started pool".to_string(),
                        Err(e) => format!(" · pool start failed: {e}"),
                    };
                }
            }
            let mut parts = Vec::new();
            if report.imported.is_empty() {
                parts.push("nothing new to import".to_string());
            } else {
                parts.push(format!("imported {}", report.imported.join(", ")));
            }
            if !report.skipped_existing.is_empty() {
                parts.push(format!(
                    "already configured: {}",
                    report.skipped_existing.join(", ")
                ));
            }
            if !report.skipped_unresolvable.is_empty() {
                parts.push(format!(
                    "skipped (not on host): {}",
                    report.skipped_unresolvable.join(", ")
                ));
            }
            format!(
                "{}{} → {}",
                parts.join(" · "),
                extra,
                report.target.display()
            )
        }
        Err(e) => format!("import failed: {e}"),
    };
    let mut result = mcp_fetch_blocking();
    result.action_msg = Some(summary);
    result
}

// ============================================================================
// Daemons overlay (MCP pool + Headroom proxy — read-only status)
// ============================================================================

/// Fetched snapshot delivered through the daemons overlay channel.
#[derive(Debug, Clone)]
pub struct DaemonsFetchResult {
    pub mcp_alive: bool,
    pub headroom: crate::headroom::ProxyStatus,
    pub headroom_consumers: Vec<String>,
    /// Every running `notifyd` process, classified live / stale / orphan.
    pub notifyd: Vec<ainb_plugin_notifyd::ClassifiedDaemon>,
    /// approve.sock liveness: serving? + the probe's health reason (carries the
    /// pending-waiter count). Sockets are tracked here too, not just daemons.
    pub approve_running: bool,
    pub approve_reason: String,
}

/// Live, lazily-refreshed snapshot for the Daemons overlay. Present only while
/// the overlay is open; dropping it stops all refresh activity.
#[derive(Debug)]
pub struct DaemonsOverlayState {
    pub mcp_alive: bool,
    pub headroom: crate::headroom::ProxyStatus,
    pub headroom_consumers: Vec<String>,
    /// Every running `notifyd` process, classified live / stale / orphan.
    pub notifyd: Vec<ainb_plugin_notifyd::ClassifiedDaemon>,
    /// approve.sock liveness + health reason (see [`DaemonsFetchResult`]).
    pub approve_running: bool,
    pub approve_reason: String,
    pub loading: bool,
    pub last_refreshed: Option<std::time::Instant>,
    /// Receiver for the in-flight fetch (None = no fetch pending).
    pub fetch_rx: Option<mpsc::UnboundedReceiver<DaemonsFetchResult>>,
    /// Receiver for an in-flight `notifyd` restart (None = no restart pending).
    /// Carries the one-line outcome to show under the notifyd section.
    pub notifyd_restart_rx: Option<mpsc::UnboundedReceiver<String>>,
    /// Last restart outcome line (transient, shown until the next refresh).
    pub notifyd_restart_status: Option<String>,
}

/// Blocking portion of the daemons fetch: MCP alive probe + SessionStore read
/// + notifyd process scan. These are sync calls (the notifyd scan shells out
/// to `ps`) so they run on the blocking thread pool.
pub(crate) fn daemons_sync_probe() -> (
    bool,
    Vec<String>,
    Vec<ainb_plugin_notifyd::ClassifiedDaemon>,
    (bool, String),
) {
    let mcp_alive = crate::mcp_pool::client::daemon_alive();
    let headroom_consumers = crate::interactive::SessionStore::load()
        .sessions
        .into_values()
        .filter(|m| m.headroom_enabled)
        .map(|m| m.tmux_session_name.clone())
        .collect::<Vec<_>>();
    let notifyd = ainb_plugin_notifyd::scan_daemons();
    // approve.sock — same probe the `ainb fleet daemons` health view uses, so
    // the two surfaces can't drift. Reason carries the pending-waiter count.
    let approve = match ainb_plugin_notifyd::Paths::from_home() {
        Ok(paths) => {
            let s = crate::fleet::daemons::probe::probe_approve_broker(
                &paths.base,
                crate::fleet::daemons::heartbeat::now_ms(),
            );
            (
                s.state == crate::fleet::daemons::DaemonState::Running,
                s.reason,
            )
        }
        Err(e) => (false, format!("home unresolved: {e}")),
    };
    (mcp_alive, headroom_consumers, notifyd, approve)
}

// ============================================================================
// Home Screen State
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HomeTile {
    SkillManager, // Install / sync / doctor (spec §10.1)
    Config,       // Settings & presets
    Sessions,     // Session manager
    Recovery,     // Recover orphaned sessions
    Mcp,          // Shared MCP pool overlay
    Stats,        // Analytics & usage
    Help,         // Docs & guides
}

impl HomeTile {
    pub fn all() -> Vec<HomeTile> {
        vec![
            HomeTile::SkillManager,
            HomeTile::Config,
            HomeTile::Sessions,
            HomeTile::Recovery,
            HomeTile::Mcp,
            HomeTile::Stats,
            HomeTile::Help,
        ]
    }

    pub fn label(&self) -> &'static str {
        match self {
            HomeTile::SkillManager => "Skills (manager)",
            HomeTile::Config => "Config",
            HomeTile::Sessions => "Sessions",
            HomeTile::Recovery => "Recovery",
            HomeTile::Mcp => "MCP",
            HomeTile::Stats => "Stats",
            HomeTile::Help => "Help",
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            HomeTile::SkillManager => "Install / sync / doctor (Z)",
            HomeTile::Config => "Settings & Presets",
            HomeTile::Sessions => "Manage Active",
            HomeTile::Recovery => "Resume Orphaned",
            HomeTile::Mcp => "Shared Pool",
            HomeTile::Stats => "Usage & Analytics",
            HomeTile::Help => "Docs & Guides",
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            HomeTile::SkillManager => "🧰",
            HomeTile::Config => "⚙️",
            HomeTile::Sessions => "🚀",
            HomeTile::Recovery => "🔄",
            HomeTile::Mcp => "🧬",
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
            // Greyed-out / non-launchable in the Agents picker for now —
            // `is_current_available()` blocks selection of non-Available
            // providers (kept consistent with the new-session Configure wizard).
            status: ProviderStatus::Disabled,
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
    McpPool,
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
            ConfigCategory::McpPool,
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
            ConfigCategory::McpPool => "MCP Pool",
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
            ConfigCategory::McpPool => "🧬",
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
            ConfigCategory::McpPool => "Shared MCP servers: one process across sessions",
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

/// Render a TOML scalar from a `[plugins.<name>]` value table as the plain
/// string the Settings widgets edit. Non-scalar values (tables/arrays) can't
/// appear under the flat-scalars-only plugin config schema, so they fall back
/// to their TOML debug form rather than panicking.
fn toml_scalar_to_string(value: &toml::Value) -> String {
    match value {
        toml::Value::String(s) => s.clone(),
        toml::Value::Integer(i) => i.to_string(),
        toml::Value::Boolean(b) => b.to_string(),
        toml::Value::Float(f) => f.to_string(),
        other => other.to_string(),
    }
}

/// Map a plugin's [`ConfigField`](ainb_plugin_protocol::manifest::ConfigField)
/// to the [`ConfigValue`] widget the Settings screen renders, seeding it from
/// `saved` (the persisted `[plugins.<name>].<key>` value) when present, else
/// the schema `default`. The kind drives the widget:
/// `path`/`string` → [`ConfigValue::Text`], `bool` → [`ConfigValue::Bool`],
/// `enum` → [`ConfigValue::Choice`], `int` → [`ConfigValue::Number`].
fn config_value_for_field(
    field: &ainb_plugin_protocol::manifest::ConfigField,
    saved: Option<&str>,
) -> ConfigValue {
    use ainb_plugin_protocol::manifest::ConfigKind;

    let raw = saved.unwrap_or(field.default.as_str());
    match field.kind {
        ConfigKind::Path | ConfigKind::String => ConfigValue::Text(raw.to_string()),
        ConfigKind::Bool => ConfigValue::Bool(raw.eq_ignore_ascii_case("true")),
        ConfigKind::Int => {
            // A non-numeric stored value (corrupt config.toml) would otherwise be
            // silently coerced to 0; warn so the bad value surfaces in the logs
            // rather than vanishing on reset.
            let n = raw.trim().parse().unwrap_or_else(|_| {
                warn!(
                    field = %field.key,
                    value = %raw,
                    "non-integer value for int config field; falling back to 0"
                );
                0
            });
            ConfigValue::Number(n)
        }
        ConfigKind::Enum => {
            // An unknown stored choice (corrupt config.toml or a renamed enum
            // variant) would otherwise be silently coerced to the first choice;
            // warn so the bad value surfaces in the logs.
            let idx = field.choices.iter().position(|c| c == raw).unwrap_or_else(|| {
                warn!(
                    field = %field.key,
                    value = %raw,
                    "unknown choice for enum config field; falling back to first option"
                );
                0
            });
            ConfigValue::Choice(field.choices.clone(), idx)
        }
    }
}

/// Convert an edited [`ConfigValue`] back into the TOML scalar persisted under
/// `[plugins.<name>].<key>`. Bool/enum/int keep their native TOML type;
/// `Text`/`Secret` serialize as strings. (`Secret` never appears in a plugin
/// `[[config]]` schema today, but is handled for totality.)
fn config_value_to_toml(value: &ConfigValue) -> toml::Value {
    match value {
        ConfigValue::Text(s) | ConfigValue::Secret(s) => toml::Value::String(s.clone()),
        ConfigValue::Bool(b) => toml::Value::Boolean(*b),
        ConfigValue::Number(n) => toml::Value::Integer(*n),
        ConfigValue::Choice(options, idx) => {
            toml::Value::String(options.get(*idx).cloned().unwrap_or_default())
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

        // MCP Pool (per-server `shared.*` toggles appended in from_app_config)
        settings.insert(
            ConfigCategory::McpPool,
            vec![
                ConfigSetting {
                    key: "pool_enabled".to_string(),
                    label: "Shared MCP Pool".to_string(),
                    value: ConfigValue::Bool(true),
                    description: "One MCP server process shared across all host sessions"
                        .to_string(),
                },
                ConfigSetting {
                    key: "idle_grace_secs".to_string(),
                    label: "Idle Grace (seconds)".to_string(),
                    value: ConfigValue::Number(300),
                    description: "Reap a pooled server this long after its last session detaches"
                        .to_string(),
                },
            ],
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

        // Update MCP Pool from config + append one shared-toggle per server
        if let Some(settings) = state.settings.get_mut(&ConfigCategory::McpPool) {
            for setting in settings.iter_mut() {
                match setting.key.as_str() {
                    "pool_enabled" => {
                        setting.value = ConfigValue::Bool(config.mcp_pool.enabled);
                    }
                    "idle_grace_secs" => {
                        setting.value = ConfigValue::Number(config.mcp_pool.idle_grace_secs as i64);
                    }
                    _ => {}
                }
            }
            let mut names: Vec<&String> = config.mcp_servers.keys().collect();
            names.sort();
            for name in names {
                let server = &config.mcp_servers[name];
                settings.push(ConfigSetting {
                    key: format!("shared.{name}"),
                    label: format!("Share: {name}"),
                    value: ConfigValue::Bool(server.shared),
                    description: format!(
                        "Pool '{name}' across sessions (disable for stateful servers)"
                    ),
                });
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

    /// Prefix that marks a [`ConfigCategory::Plugins`] row as a per-plugin
    /// `[[config]]` field (vs. the static enable/disable placeholder rows).
    /// The row key is `plugin:<plugin_name>:<field_key>` — unique across
    /// plugins that share a field name, and reversible in
    /// [`apply_to_app_config`](Self::apply_to_app_config) so the edit lands
    /// under `plugins.values[plugin_name][field_key]`.
    const PLUGIN_ROW_PREFIX: &'static str = "plugin:";

    /// Compose the Plugins-category row key for a plugin's config field.
    fn plugin_row_key(plugin: &str, field_key: &str) -> String {
        format!("{}{plugin}:{field_key}", Self::PLUGIN_ROW_PREFIX)
    }

    /// Split a Plugins-category row key back into `(plugin_name, field_key)`,
    /// or `None` for the static placeholder rows. The plugin name and the
    /// field key are joined by the *first* `:` after the prefix, so plugin
    /// names never contain `:` but field keys may.
    fn parse_plugin_row_key(key: &str) -> Option<(&str, &str)> {
        let rest = key.strip_prefix(Self::PLUGIN_ROW_PREFIX)?;
        rest.split_once(':')
    }

    /// Append per-plugin `[[config]]` rows to the Plugins category from the
    /// loaded plugin manifests, keeping the existing enable/disable rows.
    ///
    /// One [`ConfigSetting`] is produced per [`ConfigField`], mapping the
    /// field `kind` to the matching [`ConfigValue`] widget
    /// (`path`/`string` → `Text`, `bool` → `Bool`, `enum` → `Choice`,
    /// `int` → `Number`). The displayed value defaults from
    /// `plugins.values[plugin][key]` when present, else the schema `default`.
    ///
    /// Idempotent: re-invoking it (e.g. after the plugin runtime finishes
    /// discovery) rebuilds the per-plugin rows from scratch rather than
    /// duplicating them — only the static placeholder rows are retained.
    pub fn apply_plugin_manifests(
        &mut self,
        manifests: &[ainb_plugin_protocol::manifest::Manifest],
        plugins_cfg: &crate::config::PluginsConfig,
    ) {
        let rows = self.settings.entry(ConfigCategory::Plugins).or_default();
        // Drop any previously-appended plugin rows so repeated calls are
        // idempotent; keep the static enable/disable placeholders.
        rows.retain(|s| Self::parse_plugin_row_key(&s.key).is_none());

        for manifest in manifests {
            let plugin = manifest.plugin.name.as_str();
            // The resolved [plugins.<name>] value table, if the user has set
            // any keys — drives the displayed default ahead of the schema's.
            let saved = plugins_cfg.values.get(plugin).and_then(toml::Value::as_table);

            for field in &manifest.config {
                // Saved string value (config.toml only stores TOML scalars; we
                // render every kind from its string form for the widget).
                let saved_str = saved.and_then(|t| t.get(&field.key)).map(toml_scalar_to_string);
                let value = config_value_for_field(field, saved_str.as_deref());

                rows.push(ConfigSetting {
                    key: Self::plugin_row_key(plugin, &field.key),
                    label: field.label.clone(),
                    value,
                    description: format!("{} · plugin: {}", field.label, plugin),
                });
            }
        }
    }

    /// Convert ConfigScreenState back to AppConfig for saving
    pub fn apply_to_app_config(&self, config: &mut AppConfig) {
        // Apply Workspace settings (extracted to keep this method under the
        // clippy `too_many_lines` threshold).
        self.apply_workspace_rows(config);

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

        // Apply MCP Pool settings
        if let Some(settings) = self.settings.get(&ConfigCategory::McpPool) {
            for setting in settings {
                match setting.key.as_str() {
                    "pool_enabled" => {
                        if let ConfigValue::Bool(enabled) = &setting.value {
                            config.mcp_pool.enabled = *enabled;
                        }
                    }
                    "idle_grace_secs" => {
                        if let ConfigValue::Number(secs) = &setting.value {
                            config.mcp_pool.idle_grace_secs = (*secs).max(0) as u64;
                        }
                    }
                    key => {
                        if let (Some(name), ConfigValue::Bool(shared)) =
                            (key.strip_prefix("shared."), &setting.value)
                        {
                            if let Some(server) = config.mcp_servers.get_mut(name) {
                                server.shared = *shared;
                            }
                        }
                    }
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

        // Apply per-plugin [[config]] edits (extracted to keep this method under
        // the clippy `too_many_lines` threshold).
        self.apply_plugin_rows(config);
    }

    /// Route the Workspace-category rows (`default_workspace`, `branch_prefix`,
    /// `exclude_paths`, `max_repositories`) back into `config.workspace_defaults`.
    fn apply_workspace_rows(&self, config: &mut AppConfig) {
        let Some(settings) = self.settings.get(&ConfigCategory::Workspace) else {
            return;
        };
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
                        // "Default Workspace" is surfaced as
                        // `workspace_scan_paths.first()` in `from_app_config`, so
                        // it must be written back as the *primary* entry. The old
                        // code pushed to the end, leaving `first()` pointing at the
                        // stale path — editing the field then appeared to do
                        // nothing on reopen.
                        //
                        // Rebuild as: edited path first, then every other distinct
                        // scan dir. This replaces the old primary (index 0),
                        // de-dups the edited path, and — crucially — preserves the
                        // remaining scan dirs even on a no-op confirm (drop the old
                        // primary, never the tail).
                        let paths = &mut config.workspace_defaults.workspace_scan_paths;
                        let tail: Vec<std::path::PathBuf> =
                            paths.iter().skip(1).filter(|p| *p != &expanded).cloned().collect();
                        paths.clear();
                        paths.push(expanded);
                        paths.extend(tail);
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

    /// Route every Plugins-category row whose key is `plugin:<name>:<field_key>`
    /// (see [`Self::plugin_row_key`]) into `config.plugins.values[<name>]
    /// [<field_key>]` — NOT a top-level field. The static enable/disable
    /// placeholder rows have no such prefix and are skipped. The serialized
    /// `[plugins.<name>]` table round-trips through the existing
    /// `AppConfig::save()` pipeline.
    fn apply_plugin_rows(&self, config: &mut AppConfig) {
        let Some(settings) = self.settings.get(&ConfigCategory::Plugins) else {
            return;
        };
        for setting in settings {
            let Some((plugin, field_key)) = Self::parse_plugin_row_key(&setting.key) else {
                continue;
            };
            let toml_value = config_value_to_toml(&setting.value);
            let entry = config
                .plugins
                .values
                .entry(plugin.to_string())
                .or_insert_with(|| toml::Value::Table(toml::value::Table::new()));
            // Coerce a non-table entry (shouldn't happen for a well-formed
            // config) into a table so the write always lands somewhere sane.
            if !entry.is_table() {
                *entry = toml::Value::Table(toml::value::Table::new());
            }
            if let Some(table) = entry.as_table_mut() {
                table.insert(field_key.to_string(), toml_value);
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
    // Shared MCP pool observability overlay (None = closed; no refresh runs).
    pub mcp_overlay: Option<McpOverlayState>,
    // Daemons status overlay (MCP pool + Headroom proxy, read-only).
    pub daemons_overlay: Option<DaemonsOverlayState>,
    // Flag to force UI refresh after workspace changes
    pub ui_needs_refresh: bool,

    // Claude chat visibility toggle
    pub claude_chat_visible: bool,

    // Focus management for panes
    pub focused_pane: FocusedPane,
    // Live interactive embedded tmux-attach client for the preview pane.
    // Enforced invariants (focus can drift, so none of these are assumed):
    //  - Input forwards to the PTY only while `is_interactive_pane()` holds
    //    (embed Some AND focused_pane == Preview).
    //  - Ctrl+Q releases whenever the embed exists, regardless of focus/mode.
    //  - `poll_embed_exit` (run before every draw) releases on client death
    //    or when the session-list screen is no longer current, so keys are
    //    never forwarded to an invisible PTY.
    // Dropping it kills the ephemeral tmux client (never the session).
    pub embed: Option<crate::tmux::EmbedClient>,
    // The tmux session name the live embed is attached to. Some iff `embed`
    // is Some. Re-entering on a DIFFERENT row releases the old client and
    // attaches to the new target instead of silently refocusing the stale
    // one (see `enter_interactive_pane`).
    pub embed_session: Option<String>,
    // Interior screen rect (inside the border) the embed's PseudoTerminal
    // occupies, published by the interactive render branch each frame. Drives
    // mouse-coordinate translation into 1-based pane-local SGR sequences.
    // None whenever the embed is not rendering.
    pub embed_pane_area: Option<Rect>,
    // Bottom keymap-legend rect (or its collapsed hint row), published each
    // frame on the Sessions screen so a mouse click on it toggles visibility.
    pub menu_bar_area: Option<Rect>,
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
    // Track the last Headroom proxy watchdog tick (re-ensure if a Headroom
    // session is live but the proxy died).
    pub last_headroom_watchdog: Option<std::time::Instant>,
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
    /// Last `ui.close_request` snapshot version consumed by
    /// `tick_panel_close_requests`. The poll acts at most once per
    /// plugin publish: a version is consumed (recorded here) on first
    /// sight whether or not it triggered a navigation, so a close
    /// request that arrives while the user is on a different screen is
    /// absorbed instead of firing later.
    pub last_panel_close_version: Option<u64>,
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

    /// Inbox screen state (ainb-hooks notifications: selection,
    /// filters, in-process SQLite store handle).
    pub inbox_state: crate::components::inbox::InboxState,

    /// Daemons screen state (cached runtime-health snapshot + poll tick).
    pub daemons_state: crate::components::daemons::DaemonsState,

    /// Fleet control-panel state (cached `current_state` rows + selection +
    /// shared action-feedback cell).
    pub fleet_panel_state: crate::components::fleet_panel::FleetPanelState,

    /// WireBuffers freshly drained from plugins, keyed by screen id.
    /// `App::tick_plugin_renders` populates this before each frame so
    /// `PluginScreen::render` can paint without needing access to the
    /// plugin runtime (which lives on `App`, not `AppState`).
    pub pending_plugin_renders:
        std::collections::HashMap<crate::app::screens::ScreenId, ainb_plugin_runtime::WireBuffer>,

    /// Cache of workspace paths that are currently favorited (starred).
    /// Computed by `recompute_favorite_workspaces()` whenever the workspace
    /// list or the favorites store changes — NOT in the render path. The
    /// session-list render reads this set with an O(1) lookup, so it never
    /// re-parses `favorites.yaml` or opens a git repo per frame.
    pub favorite_workspace_paths: HashSet<PathBuf>,

    /// Last `(width, height)` `PluginScreen::render` was handed for each
    /// screen id. `tick_plugin_renders` reads this and forwards it to
    /// `handle.render(..)` so the plugin paints at the actual allocated
    /// size instead of falling back to its hard-coded default. One-frame
    /// stale is fine — the first frame still uses the plugin's fallback,
    /// every subsequent frame matches the host's layout.
    pub plugin_render_areas: std::collections::HashMap<crate::app::screens::ScreenId, (u16, u16)>,

    /// Top-left `(x, y)` origin `PluginScreen::render` painted each screen
    /// id at, stashed alongside `plugin_render_areas`. The mouse forwarder
    /// (`forward_mouse_to_focused_plugin`) subtracts this from the absolute
    /// terminal click coordinates so the plugin receives a click in its own
    /// viewport space (`(0, 0)` = top-left of its buffer). Separate from
    /// `plugin_render_areas` to keep that tuple's `(width, height)` meaning
    /// unchanged for the render-tick loop.
    pub plugin_render_origins: std::collections::HashMap<crate::app::screens::ScreenId, (u16, u16)>,

    /// Viewport `(width, height)` the last `plugin/render` kick used for
    /// each screen id. `tick_plugin_renders` forces a fresh render kick
    /// whenever the live area (from `plugin_render_areas`) differs from
    /// this — covering the first paint (the seed `(0, 0)` render becomes
    /// the real allocated size once `PluginScreen::render` runs) and any
    /// later resize. Without this, a plugin screen whose dirty flag was
    /// already consumed at `(0, 0)` (e.g. one with no host-published
    /// snapshot to re-mark it) would paint blank forever.
    pub plugin_last_render_viewport:
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

    // Skill-manager screen state (spec §10.1)
    pub skill_manager_state: crate::components::skill_manager_screen::SkillsScreenData,
    /// Background drift-poll receiver. Present only while a drift scan
    /// (kicked off by `GoToSkillManager`) is in flight; `tick()`
    /// drains it into `skill_manager_state.drift_cache`.
    pub drift_load_receiver: Option<
        mpsc::UnboundedReceiver<
            std::collections::BTreeMap<String, ainb_skill_core::drift::DriftStatus>,
        >,
    >,
    /// Background base-branch refresh for the Configure picker. The fetch +
    /// re-list runs on `spawn_blocking`; the result lands here and is applied
    /// by `check_branch_refresh_complete` on the next tick. The `u64` is a
    /// generation guard — results from a closed/reopened picker are dropped.
    pub branch_refresh_receiver: Option<
        mpsc::UnboundedReceiver<(
            u64,
            Result<Vec<crate::git::branch_list::BranchEntry>, String>,
        )>,
    >,
    /// Current branch-refresh generation (bumped on every picker open).
    pub branch_refresh_seq: u64,

    // Periodic session snapshot tracking
    pub last_snapshot_time: Option<Instant>,

    // Throttled tmux preview updates (avoid spawning subprocesses every 250ms tick)
    pub last_preview_update: Option<Instant>,

    // Throttle for the cheaper non-selected-session status sweep. Status
    // (running/idle) is not time-critical, so it polls on a longer cadence than
    // the selected session's live preview — one `capture-pane` subprocess per
    // non-selected session is only spawned every `STATUS_INTERVAL_SECS`, not on
    // every 5s preview refresh. (perf: bead 9pb)
    pub last_status_check: Option<Instant>,

    // Background workspace loading state
    pub is_loading_workspaces: bool,
    pub workspace_load_error: Option<String>,
    pub workspace_load_started: Option<Instant>,
    /// Channel receiver for background workspace loading results
    pub workspace_load_receiver: Option<mpsc::UnboundedReceiver<WorkspaceLoadResult>>,

    /// Per-session "cleared up to" timestamp (epoch ms). A hook event
    /// only marks a session if its `ts` is newer than this. Defaults to
    /// `0` (any event in the lookback window can mark); bumped to "now"
    /// while the user is attached, so re-marking only happens for
    /// activity that arrives after they look away.
    pub attention_baseline: HashMap<Uuid, i64>,
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
    /// The current step in the redesigned 2-screen flow (PickRepo →
    /// Configure → Creating).
    pub step: NewSessionStep,

    /// Screen-1 (unified repo picker) state. `Some` while the user is on
    /// `PickRepo`; populated by the `AppEvent::NewSession` handler from disk
    /// (favorites + session-defaults).
    pub pick_repo_state: Option<crate::components::new_session::pick_repo::PickRepoState>,

    /// Screen-2 (Configure) state. `Some` after PickRepo's
    /// `AdvanceTo` / `StartClone` resolves to a clonable or local source;
    /// holds the launch payload that `create_session_from_configure` reads.
    pub configure_state: Option<crate::components::new_session::configure::ConfigureState>,
}

impl Default for NewSessionState {
    fn default() -> Self {
        Self {
            // Phase 6 (new-session redesign): default is the unified picker;
            // callers that need a different step must override explicitly.
            step: NewSessionStep::PickRepo,
            pick_repo_state: None,
            configure_state: None,
        }
    }
}

/// Phase 6 (new-session redesign): launch payload built from `ConfigureState`
/// inside `create_session_from_configure`. Kept private to `state.rs` since the
/// only producer/consumer is the configure-launch async path.
#[derive(Debug, Clone)]
struct ConfigureLaunchSnapshot {
    repo_source: crate::git::repo_source::RepoSource,
    branch_name: String,
    skip_permissions: bool,
    mode: crate::models::SessionMode,
    boss_prompt: Option<String>,
    agent_type: crate::models::SessionAgentType,
    /// Claude model for Claude-agent sessions. `Some(SystemDefault)` / `None`
    /// both omit `--model` from the spawned `claude` command (the CLI's own
    /// default applies). Set only when `agent_type == Claude`.
    session_model: Option<crate::models::ClaudeModel>,
    /// Codex model for Codex-agent sessions. Same omit-on-default semantics
    /// as `session_model`. Set only when `agent_type == Codex`.
    codex_model: Option<crate::models::CodexModel>,
    /// The base-branch popup pick (2026-06). `None` = legacy base policy:
    /// HEAD for local repos, origin/HEAD for remote/star launches.
    base: Option<crate::components::new_session::configure::BaseSelection>,
    headroom_enabled: bool,
    rtk_enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NewSessionStep {
    /// Phase 6 (new-session redesign): the unified repo picker (screen 1).
    /// Owns its own state via `NewSessionState.pick_repo_state`.
    PickRepo,
    /// Phase 6 (new-session redesign): the consolidated Configure screen
    /// (screen 2). Owns its own state via `NewSessionState.configure_state`.
    Configure,
    /// In-flight session creation — the legacy render dispatcher draws an
    /// "Creating session…" panel while `create_session_from_configure`
    /// resolves.
    Creating,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AsyncAction {
    // Phase 6 (new-session redesign): legacy `StartNewSession`,
    // `StartWorkspaceSearch`, `NewSessionInCurrentDir`, `NewSessionNormal`,
    // `NewSessionWithRepoInput`, `ValidateRepoSource`, `CloneRemoteRepo`,
    // `FetchRemoteBranches`, and `CreateNewSession` variants have been
    // removed. The redesigned new-session flow uses
    // `CreateSessionFromConfigure` exclusively.
    /// Launch a session from the Configure screen — the sole session-creation
    /// entry point post-Phase-6. The payload is the already-built
    /// `LaunchSpec` returned by `ConfigureOutcome::Launch`, threaded through
    /// the dispatcher so the async path doesn't re-derive it from
    /// `configure_state` (finding #7).
    CreateSessionFromConfigure(crate::components::new_session::configure::LaunchSpec),
    /// Pre-check GitHub auth via `gh auth status` before allowing remote clone.
    CheckGitAuth,
    DeleteSession(Uuid),         // New - delete session with container cleanup
    StopSession(Uuid),           // Soft-stop interactive session (kill tmux only)
    ResumeSession(Uuid, String), // Recreate tmux for a Stopped interactive session; String is the trigger key for audit
    BulkResumeSessions(Vec<Uuid>, String), // Resume multiple Stopped interactive sessions; String is the trigger key for audit
    BulkDeleteSessions(Vec<Uuid>),         // Bulk delete multiple sessions
    RefreshWorkspaces,                     // Manual refresh of workspace data
    FetchContainerLogs(Uuid),              // Fetch container logs for a session
    AttachToContainer(Uuid),               // Attach to a container session
    AttachToTmuxSession(Uuid),             // Attach to a tmux session
    KillContainer(Uuid),                   // Kill container for a session
    AuthSetupOAuth,                        // Run OAuth authentication setup
    AuthSetupApiKey,                       // Save API key authentication
    ReauthenticateCredentials,             // Re-authenticate Claude credentials
    RestartSession(Uuid),                  // Restart a stopped session with new container
    /// Flip headroom off in the SessionStore, then respawn the session's CLI
    /// process with `tmux respawn-pane -k` (no proxy env) so the running
    /// process is replaced. Claude gets `--continue` to preserve the
    /// conversation; Codex restarts fresh (no continue flag exists).
    DowngradeHeadroom(Uuid),
    CleanupOrphaned,           // Clean up orphaned containers without worktrees
    AttachToOtherTmux(String), // Attach to a non-agents-in-a-box tmux session by name
    AttachWitr, // Launch `witr -i` (process-causality browser) in a dedicated tmux session and attach full-screen
    AttachAbtop, // Launch `abtop --exit-on-jump` (top-for-agents monitor) in a dedicated tmux session and attach full-screen
    SetupAbtopRateLimits, // Run `abtop --setup` (rate-limit StatusLine hook) in a detached tmux pane, then queue AttachAbtop
    KillOtherTmux(String), // Kill a non-agents-in-a-box tmux session by name
    KillOtherTmuxSessions(Vec<String>), // Kill multiple non-agents-in-a-box tmux sessions by name
    ConfirmOtherTmuxRename, // Confirm and execute rename for "Other tmux" session
    // Shell session actions (one shell per workspace)
    OpenWorkspaceShell {
        workspace_index: usize,                 // Index of workspace to open shell for
        target_dir: Option<std::path::PathBuf>, // Optional: cd to this directory (worktree)
    },
    OpenShellAtPath(std::path::PathBuf), // Open shell directly at a path (no workspace required)
    KillWorkspaceShell(usize),           // Kill workspace shell by workspace index
    // Editor action
    OpenInEditor(std::path::PathBuf), // Open workspace in preferred editor
    // Onboarding actions
    OnboardingCheckDeps,          // Run dependency check during onboarding
    OnboardingInstallDep(String), // Install one dep (by id) from the deps screen
    /// Fetch + parse a skill source (git clone) off the event loop, then
    /// open the Skill Manager's source-preview picker with the result.
    SkillPreviewFetch(String),
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
            mcp_overlay: None,
            daemons_overlay: None,
            ui_needs_refresh: false,
            claude_chat_visible: false,
            focused_pane: FocusedPane::Sessions,
            embed: None,
            embed_session: None,
            embed_pane_area: None,
            menu_bar_area: None,
            sessions_pane_state,
            is_current_dir_git_repo: false,
            last_logs_session_id: None,
            attached_session_id: None,
            auth_setup_state: None,
            log_last_updated: HashMap::new(),
            last_log_check: None,
            last_token_refresh_check: None,
            last_headroom_watchdog: None,
            claude_chat_state: None,
            live_logs: HashMap::new(),
            claude_manager: None,
            log_streaming_coordinator: None,
            log_sender: None,
            git_view_state: None,
            previous_screen: None,
            last_panel_close_version: None,
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

            // ainb-hooks inbox (lazy-opens SQLite on first refresh)
            inbox_state: crate::components::inbox::InboxState::default(),

            // Daemons observability (collects health on first/periodic render)
            daemons_state: crate::components::daemons::DaemonsState::default(),

            // Fleet control panel (reads current_state on entry/tick)
            fleet_panel_state: crate::components::fleet_panel::FleetPanelState::default(),

            pending_plugin_renders: std::collections::HashMap::new(),
            favorite_workspace_paths: HashSet::new(),
            plugin_render_areas: std::collections::HashMap::new(),
            plugin_render_origins: std::collections::HashMap::new(),
            plugin_last_render_viewport: std::collections::HashMap::new(),
            plugin_runtime: None,

            statusline_status_cache: None,

            live_window_watcher: crate::models::live_window_watcher::LiveWindowWatcher::default(),

            // Skills browser state
            skills_state: crate::components::skills::SkillsViewState::default(),
            skills_load_receiver: None,

            // Skill-manager screen state (spec §10.1)
            skill_manager_state: crate::components::skill_manager_screen::SkillsScreenData::default(
            ),
            drift_load_receiver: None,
            // Configure base-branch picker background refresh
            branch_refresh_receiver: None,
            branch_refresh_seq: 0,

            // Periodic session snapshot tracking
            last_snapshot_time: None,

            // Throttled tmux preview updates
            last_preview_update: None,
            last_status_check: None,

            // Background workspace loading state
            is_loading_workspaces: false,
            workspace_load_error: None,
            workspace_load_started: None,
            workspace_load_receiver: None,

            // Per-session attention markers, driven by ainb-hooks events.
            attention_baseline: HashMap::new(),
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

        // Seed git directories from the last saved paths so re-opening onboarding
        // shows what the user set previously, not a fresh default scan. Prefer the
        // onboarding record; fall back to the app-config scan paths.
        let saved = crate::config::OnboardingConfig::load()
            .map(|c| c.git_directories)
            .unwrap_or_default();
        let saved = if saved.is_empty() {
            self.app_config.workspace_defaults.workspace_scan_paths.clone()
        } else {
            saved
        };
        state.set_git_directories(&saved);

        // Re-populate the OTEL form from previously-saved Grafana creds so
        // re-opening onboarding shows what was configured, not blank fields
        // (same remember-on-reopen contract as git directories).
        if let Some(creds) = crate::otel::read_grafana_creds() {
            state.otel_otlp_endpoint = creds.otlp_endpoint;
            state.otel_instance_id = creds.instance_id;
            state.otel_api_token = creds.api_token;
            state.otel_skip = false;
        }

        // If a specific start step is provided, jump to it
        if let Some(step) = start_step {
            state.current_step = step;
            // Initialize editors if starting directly at EditorSelection
            if step == OnboardingStep::EditorSelection {
                state.init_editors_if_needed();
            }
        }

        // Detect current per-agent auth up front so the Authentication step
        // always opens showing real current values (config + keychain).
        state.refresh_auth_statuses();

        self.onboarding_state = Some(state);
        self.current_screen = screen_ids::ONBOARDING.to_string();
    }

    /// Persist the onboarding git directories immediately — called when leaving
    /// the Git Directories step in any direction (Next / Back / to menu) so the
    /// user's edit is saved without having to finish the whole wizard.
    ///
    /// Only writes when at least one path is VALID, so invalid/empty input never
    /// clobbers previously-saved config.
    pub fn persist_onboarding_git_dirs(&mut self) {
        use crate::config::OnboardingConfig;

        let Some(state) = self.onboarding_state.as_ref() else {
            return;
        };
        let valid = state.get_valid_directories();
        if valid.is_empty() {
            return;
        }

        // Onboarding record — load first so we preserve completed/version/etc.
        let mut cfg = OnboardingConfig::load().unwrap_or_default();
        cfg.git_directories = valid.clone();
        if let Err(e) = cfg.save() {
            warn!("Failed to persist onboarding git directories: {}", e);
        }

        // App-config scan paths (what session creation actually reads).
        self.app_config.workspace_defaults.workspace_scan_paths = valid;
        if let Err(e) = self.app_config.save() {
            warn!("Failed to persist workspace scan paths: {}", e);
        }
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

            // Optional OpenTelemetry -> Grafana Cloud setup. Best-effort: a
            // failure here must never block finishing onboarding. The TUI does
            // NOT brew-install Alloy (interactive brew in the alt-screen is
            // hostile) — if Alloy is missing we still write the config and the
            // user finishes with `ainb otel setup` / `ainb otel start` later.
            if state.otel_should_setup() {
                let creds = crate::otel::GrafanaCloudCreds {
                    otlp_endpoint: state.otel_otlp_endpoint.trim().to_string(),
                    instance_id: state.otel_instance_id.trim().to_string(),
                    api_token: state.otel_api_token.trim().to_string(),
                };
                let host = crate::otel::detect_host_name();
                let result = (|| -> anyhow::Result<()> {
                    crate::otel::write_assets()?;
                    crate::otel::write_env_file(&creds, &host)?;
                    crate::otel::ensure_settings_env()?;
                    let _ = crate::otel::ensure_shell_rc_sources_env();
                    if crate::otel::alloy_installed() {
                        let _ = crate::otel::start_alloy();
                    }
                    Ok(())
                })();
                match result {
                    Ok(()) => info!("OTEL setup written (host.name={host})"),
                    Err(e) => warn!("OTEL setup during onboarding failed (non-fatal): {e}"),
                }
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

        // New-user path: now that onboarding is done, offer to install
        // the ainb-hooks notification plugin (existing users get this at
        // startup in main.rs instead). No-op if declined or up to date.
        self.maybe_prompt_notify_install();

        Ok(())
    }

    /// Cancel onboarding and return to home (for factory reset scenario)
    pub fn cancel_onboarding(&mut self) {
        self.onboarding_state = None;
        self.current_screen = screen_ids::HOME.to_string();
    }

    /// Leave the onboarding wizard and drop into the Setup menu.
    ///
    /// This is the wizard's `Esc` behaviour: rather than abandoning setup all
    /// the way back to Home, the user lands on the Setup menu where they can
    /// pick a specific step (re-run wizard, check deps, configure paths, …) or
    /// back out to Home from there. The menu state is reset so the landing is
    /// always clean (selection at the top, no stale confirmation open).
    pub fn onboarding_to_menu(&mut self) {
        use crate::components::setup_menu::SetupMenuState;
        self.onboarding_state = None;
        self.setup_menu_state = SetupMenuState::new();
        self.current_screen = screen_ids::SETUP_MENU.to_string();
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

                            // Resolve favorite status once per workspace now
                            // that the list changed, so the session-list render
                            // never opens a git repo or parses favorites.yaml
                            // per frame (perf: bead 9ov + 8rn).
                            self.recompute_favorite_workspaces();

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

                            // The fast startup loader (`load_workspaces_async`)
                            // only surfaces LIVE boss/interactive sessions — it
                            // skips the stopped-session second-pass that
                            // `load_real_workspaces` runs (reading sessions.json
                            // for dead-tmux entries whose worktree still exists).
                            // Without this, stopped sessions stay hidden until
                            // the user happens to trigger a full refresh (stop a
                            // session, delete one, press `f`). Enqueue exactly one
                            // full refresh so the complete picture (stopped
                            // sessions included) appears right after first paint.
                            // Fires once per launch: this branch runs a single
                            // time (the receiver is cleared above), and
                            // `load_real_workspaces` doesn't re-arm it — no loop.
                            // Guard on `None` so a user-queued action is never
                            // clobbered.
                            if self.pending_async_action.is_none() {
                                self.pending_async_action = Some(AsyncAction::RefreshWorkspaces);
                            }

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

    /// Recompute which workspaces are favorited and cache the result in
    /// `favorite_workspace_paths`. Loads `favorites.yaml` ONCE and resolves
    /// each workspace's favorite status (by local path or git remote) here,
    /// off the render path. Call after the workspace list changes or a
    /// favorite is toggled — never per frame. (perf: beads 9ov + 8rn)
    pub fn recompute_favorite_workspaces(&mut self) {
        let favorites = crate::config::FavoritesStore::load();
        let mut starred: HashSet<PathBuf> = HashSet::new();
        for workspace in &self.workspaces {
            if Self::workspace_is_favorite(&workspace.path, &favorites) {
                starred.insert(workspace.path.clone());
            }
        }
        self.favorite_workspace_paths = starred;
    }

    /// True if `path` is favorited, by local-path match or by the repo's git
    /// remote (owner/repo shorthand or full URL). The git remote lookup is the
    /// expensive part (libgit2 open + config read), so this MUST stay out of
    /// the render path — it runs only from `recompute_favorite_workspaces`.
    fn workspace_is_favorite(
        path: &std::path::Path,
        favorites: &crate::config::FavoritesStore,
    ) -> bool {
        let path_str = path.display().to_string();
        if favorites.favorites.iter().any(|f| f.source == path_str) {
            return true;
        }
        // Fall back to the repo's `origin` remote. `from_input` is deprecated
        // for free-form input, but the URL comes from `get_remote_url()` so the
        // legacy contract holds.
        crate::perf::record_git_resolve();
        let Ok(git_repo) = crate::git::RepositoryManager::open(path) else {
            return false;
        };
        let Ok(Some(remote_url)) = git_repo.get_remote_url() else {
            return false;
        };
        #[allow(deprecated)]
        let Ok(repo_source) = crate::git::RepoSource::from_input(&remote_url) else {
            return false;
        };
        if let Ok(parsed) = repo_source.parse_components() {
            let shorthand = format!("{}/{}", parsed.owner, parsed.repo_name);
            favorites
                .favorites
                .iter()
                .any(|f| f.source == shorthand || f.source == remote_url)
        } else {
            favorites.favorites.iter().any(|f| f.source == remote_url)
        }
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

    /// Kick off a background drift scan against `home` (the ainb data
    /// dir holding `manifest.yaml` + `lock.yaml`), using `backend` as
    /// the DriftBackend. Skipped if a scan is already in flight
    /// (`drift_load_receiver` is `Some`). Returns true if a new scan
    /// was spawned, false if coalesced.
    ///
    /// Called by the `GoToSkillManager` handler on every screen-open
    /// so out-of-band edits to the manifest / lockfile show up the
    /// next tick. Tests inject a `MockBackend`; the production
    /// dispatch uses `GitLsRemoteBackend`.
    pub fn start_background_drift_load(
        &mut self,
        home: &std::path::Path,
        backend: std::sync::Arc<dyn ainb_skill_core::drift::DriftBackend + Send + Sync>,
    ) -> bool {
        if self.drift_load_receiver.is_some() {
            return false;
        }
        let (tx, rx) = mpsc::unbounded_channel();
        self.drift_load_receiver = Some(rx);
        let home = home.to_path_buf();
        tokio::spawn(async move {
            let result = tokio::task::spawn_blocking(move || {
                use ainb_skill_core::lockfile::Lockfile;
                use ainb_skill_core::manifest::Manifest;
                use ainb_skill_core::paths::{lockfile_path_in, manifest_path_in};
                let manifest = Manifest::load_from(&manifest_path_in(&home)).unwrap_or_default();
                let lockfile = Lockfile::load_from(&lockfile_path_in(&home)).unwrap_or_default();
                ainb_skill_core::drift::detect_all(&manifest, &lockfile, backend.as_ref())
            })
            .await;
            match result {
                Ok(map) => {
                    let _ = tx.send(map);
                }
                Err(e) => {
                    warn!("Drift detect task failed: {e}");
                }
            }
        });
        true
    }

    /// Poll the background drift scan. Returns true if results were
    /// applied this tick. Drains a single message — backend returns
    /// the whole map in one go so a single drain is enough.
    pub fn check_drift_load_complete(&mut self) -> bool {
        if let Some(ref mut receiver) = self.drift_load_receiver {
            match receiver.try_recv() {
                Ok(map) => {
                    self.skill_manager_state.drift_cache = map;
                    self.drift_load_receiver = None;
                    true
                }
                Err(mpsc::error::TryRecvError::Empty) => false,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    self.drift_load_receiver = None;
                    warn!("Drift detect task dropped its sender without delivering data");
                    true
                }
            }
        } else {
            false
        }
    }

    // ── Shared MCP pool overlay ────────────────────────────────────────────
    // The overlay is opened on demand and refreshed lazily. All daemon I/O
    // runs off-thread (spawn_blocking); the render loop only reads the cached
    // snapshot. Nothing polls while the overlay is closed.

    /// Toggle the MCP pool overlay. Opening seeds config + fires the first
    /// fetch; closing drops the snapshot (and thus all refresh activity).
    pub fn toggle_mcp_overlay(&mut self) {
        if self.mcp_overlay.is_some() {
            self.mcp_overlay = None;
            return;
        }
        let config = crate::config::AppConfig::load().unwrap_or_default();
        self.mcp_overlay = Some(McpOverlayState {
            pool_enabled: config.mcp_pool.enabled,
            daemon_running: false,
            servers: Vec::new(),
            selected: 0,
            loading: true,
            last_refreshed: None,
            refresh_secs: config.mcp_pool.monitor_refresh_secs,
            fetch_rx: None,
            last_action: None,
        });
        self.spawn_mcp_fetch();
    }

    pub fn close_mcp_overlay(&mut self) {
        self.mcp_overlay = None;
    }

    pub fn mcp_overlay_move(&mut self, delta: i32) {
        if let Some(o) = self.mcp_overlay.as_mut() {
            if o.servers.is_empty() {
                return;
            }
            let n = o.servers.len() as i32;
            o.selected = ((o.selected as i32 + delta).rem_euclid(n)) as usize;
        }
    }

    /// Spawn one off-thread fetch of the daemon status, unless one is already
    /// in flight (the one-outstanding-request guard). The blocking control
    /// socket call runs on the blocking pool so the executor never stalls.
    pub fn spawn_mcp_fetch(&mut self) {
        let Some(o) = self.mcp_overlay.as_mut() else {
            return;
        };
        if o.fetch_rx.is_some() {
            return; // a fetch is already pending
        }
        let (tx, rx) = mpsc::unbounded_channel();
        o.fetch_rx = Some(rx);
        o.loading = true;
        tokio::spawn(async move {
            let result =
                tokio::task::spawn_blocking(mcp_fetch_blocking).await.unwrap_or_else(|e| {
                    McpFetchResult {
                        daemon_running: false,
                        servers: Vec::new(),
                        error: Some(format!("fetch task failed: {e}")),
                        action_msg: None,
                    }
                });
            let _ = tx.send(result);
        });
    }

    /// Drain a completed fetch and, while the overlay is open, fire the next
    /// lazy refresh when the cadence has elapsed. Cheap and non-blocking:
    /// `try_recv` never waits, and no fetch is spawned when one is pending or
    /// the cadence is disabled. Called from the 250ms app tick.
    pub fn check_mcp_overlay(&mut self) {
        let Some(o) = self.mcp_overlay.as_mut() else {
            return;
        };

        if let Some(rx) = o.fetch_rx.as_mut() {
            if let Ok(result) = rx.try_recv() {
                o.fetch_rx = None;
                o.loading = false;
                o.daemon_running = result.daemon_running;
                o.servers = result.servers;
                // Sticky: only an action (import) sets a message; plain
                // refreshes carry None and leave the prior summary in place.
                if result.action_msg.is_some() {
                    o.last_action = result.action_msg;
                }
                o.last_refreshed = Some(std::time::Instant::now());
                if o.selected >= o.servers.len() {
                    o.selected = o.servers.len().saturating_sub(1);
                }
            }
        }

        // Lazy auto-refresh: only while open, only when nothing is pending,
        // only if a cadence is configured and it has elapsed.
        let due = o.refresh_secs > 0
            && o.fetch_rx.is_none()
            && o.last_refreshed
                .map(|t| t.elapsed().as_secs() >= o.refresh_secs)
                .unwrap_or(false);
        if due {
            self.spawn_mcp_fetch();
        }
    }

    /// Stop the selected pooled server (off-thread), then refresh.
    pub fn mcp_stop_server(&mut self, name: &str) {
        let name = name.to_string();
        self.mcp_stop_then_refresh(move || {
            let _ = crate::mcp_pool::client::stop_server(&name);
        });
    }

    /// Import MCP servers into ainb config (off-thread), register the new
    /// ones with the live daemon, then refresh the table. `to_user` targets
    /// the user config (`~/.agents-in-a-box/config/config.toml`) instead of
    /// the project's `./.ainb/config.toml`. Never blocks the TUI — the
    /// import + control-socket calls run on the blocking pool and the result
    /// (summary + fresh snapshot) is delivered through the overlay channel.
    pub fn mcp_import(&mut self, to_user: bool) {
        let Some(o) = self.mcp_overlay.as_mut() else {
            return;
        };
        let (tx, rx) = mpsc::unbounded_channel();
        o.fetch_rx = Some(rx); // replaces any in-flight fetch
        o.loading = true;
        tokio::spawn(async move {
            let result = tokio::task::spawn_blocking(move || mcp_import_blocking(to_user))
                .await
                .unwrap_or_else(|e| McpFetchResult {
                    daemon_running: false,
                    servers: Vec::new(),
                    error: Some(format!("import task failed: {e}")),
                    action_msg: Some(format!("import failed: {e}")),
                });
            let _ = tx.send(result);
        });
    }

    /// Stop the whole pool daemon (off-thread), then refresh.
    pub fn mcp_stop_daemon(&mut self) {
        self.mcp_stop_then_refresh(|| {
            let _ = crate::mcp_pool::client::daemon_stop();
        });
    }

    /// Run a blocking stop action off-thread, then fetch fresh status and
    /// deliver it through the overlay's channel — so the table reflects the
    /// change as soon as the stop completes (no immediate-fetch race that
    /// reads pre-stop state).
    fn mcp_stop_then_refresh<F: FnOnce() + Send + 'static>(&mut self, stop: F) {
        let Some(o) = self.mcp_overlay.as_mut() else {
            return;
        };
        let (tx, rx) = mpsc::unbounded_channel();
        o.fetch_rx = Some(rx); // replaces any in-flight fetch (its result is discarded)
        o.loading = true;
        tokio::spawn(async move {
            let _ = tokio::task::spawn_blocking(stop).await;
            let result =
                tokio::task::spawn_blocking(mcp_fetch_blocking).await.unwrap_or_else(|e| {
                    McpFetchResult {
                        daemon_running: false,
                        servers: Vec::new(),
                        error: Some(format!("fetch task failed: {e}")),
                        action_msg: None,
                    }
                });
            let _ = tx.send(result);
        });
    }

    // ── Daemons overlay ──────────────────────────────────────────────────────

    /// Open the Daemons overlay and fire the first fetch; idempotent (toggle).
    pub fn toggle_daemons_overlay(&mut self) {
        if self.daemons_overlay.is_some() {
            self.daemons_overlay = None;
            return;
        }
        self.daemons_overlay = Some(DaemonsOverlayState {
            mcp_alive: false,
            headroom: crate::headroom::ProxyStatus {
                running: false,
                port: crate::headroom::proxy_port(),
                pid: None,
                tokens_saved: None,
            },
            headroom_consumers: Vec::new(),
            notifyd: Vec::new(),
            approve_running: false,
            approve_reason: "probing…".to_string(),
            loading: true,
            last_refreshed: None,
            fetch_rx: None,
            notifyd_restart_rx: None,
            notifyd_restart_status: None,
        });
        self.spawn_daemons_fetch();
    }

    pub fn close_daemons_overlay(&mut self) {
        self.daemons_overlay = None;
    }

    /// Spawn one off-thread fetch of both daemon statuses (one-outstanding guard).
    /// Runs MCP + SessionStore probes on the blocking pool; headroom::status()
    /// is async so it runs directly in the spawned task.
    pub fn spawn_daemons_fetch(&mut self) {
        let Some(o) = self.daemons_overlay.as_mut() else {
            return;
        };
        if o.fetch_rx.is_some() {
            return;
        }
        let (tx, rx) = mpsc::unbounded_channel();
        o.fetch_rx = Some(rx);
        o.loading = true;
        tokio::spawn(async move {
            // Blocking I/O (control socket + file read + `ps` scan) on the
            // blocking pool.
            let (mcp_alive, headroom_consumers, notifyd, approve) =
                tokio::task::spawn_blocking(daemons_sync_probe).await.unwrap_or((
                    false,
                    Vec::new(),
                    Vec::new(),
                    (false, "probe failed".to_string()),
                ));
            // Async HTTP probe of the Headroom /health + /stats endpoints.
            let headroom = crate::headroom::status().await;
            let result = DaemonsFetchResult {
                mcp_alive,
                headroom,
                headroom_consumers,
                notifyd,
                approve_running: approve.0,
                approve_reason: approve.1,
            };
            let _ = tx.send(result);
        });
    }

    /// Restart the notifyd daemon from the Daemons overlay — the single
    /// resume/repair lever. Runs [`ainb_plugin_notifyd::procs::restart`] off the
    /// UI thread (it SIGTERMs the old owner, reaps stragglers, respawns, and
    /// polls the approve socket, up to a few seconds). Once the socket rebinds,
    /// every still-blocked permission waiter re-dials and resumes on its own —
    /// so this one action both repairs a dead socket and resumes pending
    /// prompts. One-outstanding guard mirrors the fetch path.
    pub fn spawn_notifyd_restart(&mut self) {
        let Some(o) = self.daemons_overlay.as_mut() else {
            return;
        };
        if o.notifyd_restart_rx.is_some() {
            return;
        }
        let (tx, rx) = mpsc::unbounded_channel();
        o.notifyd_restart_rx = Some(rx);
        o.notifyd_restart_status = Some("restarting notifyd…".to_string());
        tokio::spawn(async move {
            let line = tokio::task::spawn_blocking(|| {
                match ainb_plugin_notifyd::procs::restart(std::time::Duration::from_secs(3)) {
                    Ok(out) => {
                        let spawned = out
                            .spawned
                            .map(|p| format!("pid {p}"))
                            .unwrap_or_else(|| "spawn failed".to_string());
                        if out.socket_bound {
                            format!("restarted notifyd ({spawned}) — approve socket live, pending prompts resume")
                        } else {
                            format!("restarted notifyd ({spawned}) — socket not yet rebound; hooks keep re-dialling")
                        }
                    }
                    Err(e) => format!("restart failed: {e:#}"),
                }
            })
            .await
            .unwrap_or_else(|e| format!("restart task panicked: {e}"));
            let _ = tx.send(line);
        });
    }

    /// Drain a completed daemons fetch. Called from the 250ms app tick.
    pub fn check_daemons_overlay(&mut self) {
        let Some(o) = self.daemons_overlay.as_mut() else {
            return;
        };
        if let Some(rx) = o.fetch_rx.as_mut() {
            if let Ok(result) = rx.try_recv() {
                o.fetch_rx = None;
                o.loading = false;
                o.mcp_alive = result.mcp_alive;
                o.headroom = result.headroom;
                o.headroom_consumers = result.headroom_consumers;
                o.notifyd = result.notifyd;
                o.approve_running = result.approve_running;
                o.approve_reason = result.approve_reason;
                o.last_refreshed = Some(std::time::Instant::now());
            }
        }
        // A finished restart updates the status line and triggers a fresh scan
        // so the new pid shows up in the notifyd section.
        if let Some(rx) = o.notifyd_restart_rx.as_mut() {
            if let Ok(line) = rx.try_recv() {
                o.notifyd_restart_rx = None;
                o.notifyd_restart_status = Some(line);
                self.spawn_daemons_fetch();
            }
        }
    }

    /// Headroom proxy watchdog. If a Headroom-enabled session is live but the
    /// shared proxy went down, re-ensure it. Throttled to ~10s, async, and
    /// best-effort so it never blocks the render loop.
    ///
    /// This is a self-heal, not a zero-loss guarantee: a request a session
    /// makes while the proxy is down (before the next ~10s tick respawns it)
    /// fails at the CLI and is retried by the agent/user — recovery is "the
    /// next request succeeds", not "the in-flight request is rescued". The
    /// statusline reflects actual routing, so an outage surfaces rather than
    /// silently dropping compression.
    ///
    /// In-loop tick, NOT a separate daemon — surfaced as the "watched" marker
    /// on the Headroom row of the Daemons screen, per the daemons-screen rule.
    pub fn headroom_watchdog(&mut self) {
        const INTERVAL_SECS: u64 = 10;
        let now = std::time::Instant::now();
        let due = self
            .last_headroom_watchdog
            .map(|last| now.duration_since(last).as_secs() >= INTERVAL_SECS)
            .unwrap_or(true);
        if !due {
            return;
        }
        self.last_headroom_watchdog = Some(now);

        let has_headroom_session = crate::interactive::SessionStore::load()
            .sessions
            .values()
            .any(|m| m.headroom_enabled);
        if !has_headroom_session {
            return;
        }

        tokio::spawn(async {
            if !crate::headroom::is_healthy().await {
                warn!("Headroom proxy down with a live session — watchdog respawning");
                let _ = crate::headroom::ensure_proxy_running().await;
            }
        });
    }

    /// Open the Configure screen's base-branch popup: seed entries from
    /// cached refs (disk-only — instant, offline-safe), then kick a
    /// background fetch + re-list whose result is applied by
    /// `check_branch_refresh_complete` on a later tick (interview pick
    /// 2026-06-03: cached-first, async refresh).
    pub fn open_branch_picker(&mut self) {
        use crate::components::new_session::configure::{BranchPickerState, PickerBranchEntry};
        use crate::git::branch_list::{self, BranchEntry};
        use crate::git::repo_source::RepoSource;

        let Some(cfg) = self.new_session_state.as_mut().and_then(|ns| ns.configure_state.as_mut())
        else {
            return;
        };

        // Where do cached refs live? The repo itself for local picks; the
        // clone cache for remote/star sources (when already cloned). A
        // not-yet-cloned remote has no cached refs — the popup opens empty
        // with the spinner and the ls-remote refresh fills it.
        let source = cfg.repo_source.clone();
        let list_path: Option<std::path::PathBuf> = match &source {
            RepoSource::LocalPath(p) => Some(p.clone()),
            // Remote sources resolve to their clone cache when already cloned;
            // SshSession / Filter never show the Branch row (resolver → None).
            _ => crate::git::RemoteRepoManager::new()
                .ok()
                .and_then(|m| m.cached_source_path(&source)),
        };

        let existing = cfg.existing_branches.clone();
        let mark_in_use = |entries: Vec<BranchEntry>| -> Vec<PickerBranchEntry> {
            entries
                .into_iter()
                .map(|e| PickerBranchEntry {
                    in_use: existing.iter().any(|b| b == &e.short_name),
                    entry: e,
                })
                .collect()
        };

        let cached = list_path.as_deref().map(branch_list::list_repo_branches).unwrap_or_default();
        // Feed the base-off "⚠ exists" guard: every branch the picker knows
        // about (empty for a not-yet-cached remote — the refresh below fills
        // it via ls-remote).
        if !cached.is_empty() {
            cfg.repo_branch_names = cached.iter().map(|e| e.short_name.clone()).collect();
        }
        cfg.branch_picker = Some(BranchPickerState::new(mark_in_use(cached), true));

        // Background refresh — generation-guarded so a stale result can't
        // repopulate a closed/reopened picker.
        self.branch_refresh_seq += 1;
        let seq = self.branch_refresh_seq;
        let (tx, rx) = mpsc::unbounded_channel();
        self.branch_refresh_receiver = Some(rx);
        tokio::spawn(async move {
            let join = tokio::task::spawn_blocking(move || -> Result<Vec<BranchEntry>, String> {
                match list_path {
                    Some(p) => Ok(branch_list::fetch_and_list(&p)),
                    None => {
                        // Remote source with no cache yet: ls-remote.
                        let manager =
                            crate::git::RemoteRepoManager::new().map_err(|e| e.to_string())?;
                        let remote =
                            manager.list_remote_branches(&source).map_err(|e| e.to_string())?;
                        Ok(remote
                            .into_iter()
                            .map(|b| BranchEntry {
                                display: format!("origin/{}", b.name),
                                short_name: b.name,
                                is_remote: true,
                                is_default: b.is_default,
                            })
                            .collect())
                    }
                }
            })
            .await;
            let payload = match join {
                Ok(r) => r,
                Err(join_err) => Err(format!("branch refresh task panicked: {join_err}")),
            };
            let _ = tx.send((seq, payload));
        });
    }

    /// Poll the background branch refresh. Applies the fresh list to the
    /// popup (if still open) and stops the spinner. Refresh errors keep the
    /// cached entries — offline-safe — and surface a non-blocking warning.
    /// Returns true when state changed this tick.
    pub fn check_branch_refresh_complete(&mut self) -> bool {
        use crate::components::new_session::configure::PickerBranchEntry;

        let Some(ref mut receiver) = self.branch_refresh_receiver else {
            return false;
        };
        let (seq, result) = match receiver.try_recv() {
            Ok(payload) => payload,
            Err(mpsc::error::TryRecvError::Empty) => return false,
            Err(mpsc::error::TryRecvError::Disconnected) => {
                self.branch_refresh_receiver = None;
                return false;
            }
        };
        self.branch_refresh_receiver = None;
        if seq != self.branch_refresh_seq {
            // A newer picker session superseded this refresh.
            return false;
        }

        let mut warn_msg: Option<String> = None;
        if let Some(cfg) =
            self.new_session_state.as_mut().and_then(|ns| ns.configure_state.as_mut())
        {
            let existing = cfg.existing_branches.clone();
            // Capture the fresh branch names for the base-off "⚠ exists" guard
            // before `result` is consumed below. Only on success — a failed
            // refresh keeps whatever the guard already had.
            let refreshed_names: Option<Vec<String>> = match &result {
                Ok(entries) => Some(entries.iter().map(|e| e.short_name.clone()).collect()),
                Err(_) => None,
            };
            if let Some(picker) = cfg.branch_picker.as_mut() {
                picker.loading = false;
                match result {
                    Ok(entries) => {
                        picker.entries = entries
                            .into_iter()
                            .map(|e| PickerBranchEntry {
                                in_use: existing.iter().any(|b| b == &e.short_name),
                                entry: e,
                            })
                            .collect();
                        picker.clamp_selection();
                    }
                    Err(msg) => {
                        // Keep the cached list usable; just warn.
                        warn_msg = Some(msg);
                    }
                }
            }
            if let Some(names) = refreshed_names {
                cfg.repo_branch_names = names;
            }
        }
        if let Some(msg) = warn_msg {
            warn!("branch refresh failed: {msg}");
            self.add_warning_notification(format!("Branch refresh failed: {msg}"));
        }
        true
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
            // Recover the created-with yolo flag; None (legacy metadata) → yolo.
            metadata.skip_permissions.unwrap_or(true),
            SessionMode::Interactive,
            None,
            metadata.agent_type,
            metadata.model,
        );
        session.codex_model = metadata.codex_model;
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
        let live_other_names: HashSet<String> =
            self.other_tmux_sessions.iter().map(|session| session.name.clone()).collect();
        self.selected_other_tmux_sessions.retain(|name| live_other_names.contains(name));
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

    /// Of the multi-selected managed sessions, return the IDs that can actually
    /// be resumed: interactive agent sessions that are currently Stopped.
    /// Running sessions are excluded so a bulk resume never kills+recreates a
    /// live tmux session. Order is unspecified (sourced from a HashSet).
    pub fn selected_resumable_session_ids(&self) -> Vec<Uuid> {
        use crate::models::{SessionAgentType, SessionMode, SessionStatus};
        self.selected_sessions
            .iter()
            .copied()
            .filter(|id| {
                self.find_session(*id)
                    .map(|s| {
                        let is_interactive = matches!(s.mode, SessionMode::Interactive)
                            && matches!(
                                s.agent_type,
                                SessionAgentType::Claude
                                    | SessionAgentType::Codex
                                    | SessionAgentType::Gemini
                                    | SessionAgentType::Copilot
                            );
                        is_interactive && matches!(s.status, SessionStatus::Stopped)
                    })
                    .unwrap_or(false)
            })
            .collect()
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

    /// Hide/show the Sessions bottom keymap legend (⇧M) and persist the choice.
    pub fn toggle_session_menu_bar(&mut self) {
        let show = !self.app_config.ui_preferences.show_session_menu_bar;
        self.app_config.ui_preferences.show_session_menu_bar = show;
        if let Err(e) = self.app_config.save() {
            warn!("Failed to persist show_session_menu_bar: {}", e);
        }
        self.add_info_notification(if show {
            "Keymap legend shown".to_string()
        } else {
            "Keymap legend hidden — ⇧M to show".to_string()
        });
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

    /// `true` if ainb should offer to run `abtop --setup` (the Claude
    /// rate-limit StatusLine hook) before opening abtop for the first time.
    /// Offered until the hook has run (its `~/.claude/abtop-rate-limits.json`
    /// exists) or the user chose "don't ask again".
    #[must_use]
    pub fn should_offer_abtop_setup(&self) -> bool {
        let Some(home) = dirs::home_dir() else {
            return false;
        };
        let already_done = home.join(".claude").join("abtop-rate-limits.json").exists();
        let dismissed = home.join(".agents-in-a-box").join("abtop-setup-dismissed").exists();
        !already_done && !dismissed
    }

    /// Persist the user's "don't ask again" choice for the abtop setup offer.
    pub fn dismiss_abtop_setup(&self) {
        if let Some(home) = dirs::home_dir() {
            let dir = home.join(".agents-in-a-box");
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(dir.join("abtop-setup-dismissed"), b"1");
        }
    }

    /// One-time consent dialog offering `abtop --setup` (Claude rate-limit
    /// tracking) the first time the user opens abtop. Every option proceeds to
    /// open abtop; only "Enable" also runs the setup, and "Don't ask again"
    /// suppresses the offer permanently.
    pub fn show_abtop_setup_prompt(&mut self) {
        self.confirmation_dialog = Some(ConfirmationDialog {
            title: "Enable abtop rate-limit tracking?".to_string(),
            message: "abtop can show Claude rate-limit usage (5-hour + weekly \
                      windows). This installs a StatusLine hook into \
                      ~/.claude/settings.json via `abtop --setup`. abtop works \
                      without it — the rate-limit panel just stays empty."
                .to_string(),
            confirm_action: ConfirmAction::SetupAbtopRateLimits,
            selected_option: false,
            warning: None,
            options: Some(vec![
                DialogOption {
                    label: "Enable".to_string(),
                    action: ConfirmAction::SetupAbtopRateLimits,
                },
                DialogOption {
                    label: "Just open abtop".to_string(),
                    action: ConfirmAction::OpenAbtopSkipSetup,
                },
                DialogOption {
                    label: "Don't ask again".to_string(),
                    action: ConfirmAction::DismissAbtopSetup,
                },
            ]),
            selected_index: 0,
        });
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

    /// First-run / post-upgrade prompt for the ainb-hooks notification
    /// plugin. Reads `ainb_plugin_notifyd::prompt_state`: offers to
    /// install when nothing is set up (and the user hasn't declined),
    /// or to update when this binary embeds a newer manifest than what's
    /// on disk. A no-op when there's nothing to prompt, so callers can
    /// fire it unconditionally on startup / after onboarding.
    ///
    /// Never shown while a dialog is already up or the user is mid-
    /// onboarding — callers gate on that.
    pub fn maybe_prompt_notify_install(&mut self) {
        use ainb_plugin_notifyd::{InstallPrompt, Paths, prompt_state};

        if self.confirmation_dialog.is_some() {
            return;
        }
        let Ok(paths) = Paths::from_home() else {
            return;
        };
        let (title, message, install_label) = match prompt_state(&paths) {
            InstallPrompt::OfferInstall => (
                "Get notified when a session needs you?".to_string(),
                "Install ainb-hooks so the Inbox (press b) and the per-session \
                 badges light up when an agent is awaiting input ([?]) or has \
                 finished ([✓]). Only actionable events are captured — no \
                 activity-log noise. Works with Claude Code today (registered \
                 via the claude CLI); Codex support is experimental."
                    .to_string(),
                "Install",
            ),
            InstallPrompt::OfferUpdate {
                installed,
                embedded,
            } => (
                "Update notification hooks?".to_string(),
                format!(
                    "ainb-hooks is installed at v{installed}, but this build \
                     ships v{embedded}. Re-install to pick up the latest hook \
                     set."
                ),
                "Update",
            ),
            InstallPrompt::None => return,
        };
        self.confirmation_dialog = Some(ConfirmationDialog {
            title,
            message,
            // Binary mode is unused here; tri-option drives the choice.
            confirm_action: ConfirmAction::InstallNotifyHooks,
            selected_option: false,
            warning: None,
            options: Some(vec![
                DialogOption {
                    label: install_label.to_string(),
                    action: ConfirmAction::InstallNotifyHooks,
                },
                DialogOption {
                    label: "Not now".to_string(),
                    action: ConfirmAction::Cancel,
                },
                DialogOption {
                    label: "Don't ask again".to_string(),
                    action: ConfirmAction::DismissNotifyPrompt,
                },
            ]),
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

    pub fn cancel_new_session(&mut self) {
        // INVARIANT: must NOT clear `self.notifications`. Callers post an error
        // toast immediately before cancelling (e.g. the worktree-create failure
        // arm in `create_session_from_configure`) and rely on it surviving the
        // teardown — clearing here would re-introduce the silent-flash bug
        // (Stevie 2026-06-06).
        self.new_session_state = None;
        // Return to whichever screen the user opened new-session from
        // (Home / Sessions / …). Falls back to SESSION_LIST if no
        // previous screen was recorded — matches the pre-redesign
        // contract for the legacy 13-step wizard's Cancel path.
        let prev = self
            .previous_screen
            .take()
            .unwrap_or_else(|| screen_ids::SESSION_LIST.to_string());
        self.current_screen = prev;
        // Also clear any pending async actions to prevent race conditions
        self.pending_async_action = None;
        // Set cancellation flag to prevent race conditions
        self.async_operation_cancelled = true;
    }

    pub async fn create_session_from_configure(
        &mut self,
        spec: crate::components::new_session::configure::LaunchSpec,
    ) {
        use crate::models::{ClaudeModel, CodexModel, SessionAgentType, SessionMode};

        // Finding #7: `LaunchSpec` is the single source of truth — no more
        // reaching back into `configure_state` to re-derive what the
        // component already built. The dispatcher passed it via
        // `AsyncAction::CreateSessionFromConfigure(spec)`.
        let preset = &spec.preset;
        let agent_type = match preset.agent_provider.as_str() {
            "claude" => SessionAgentType::Claude,
            "shell" => SessionAgentType::Shell,
            "ssh" => SessionAgentType::Ssh,
            "codex" => SessionAgentType::Codex,
            "gemini" => SessionAgentType::Gemini,
            "copilot" => SessionAgentType::Copilot,
            _ => SessionAgentType::Claude,
        };
        // Per-agent model parsing. `preset.agent_model` is a free-form String
        // (TOML schema stability) — each enum's `parse()` accepts aliases,
        // canonical IDs, and `""` / `"default"` (→ SystemDefault). For
        // non-Claude / non-Codex agents the model concept doesn't apply at
        // CLI launch time, so we leave both as `None`.
        let session_model = if agent_type == SessionAgentType::Claude {
            Some(ClaudeModel::parse(&preset.agent_model))
        } else {
            None
        };
        let codex_model = if agent_type == SessionAgentType::Codex {
            Some(CodexModel::parse(&preset.agent_model))
        } else {
            None
        };
        let mode = preset.mode;
        let boss_prompt = if mode == SessionMode::Boss {
            spec.prompt.clone()
        } else {
            None
        };
        let snapshot = ConfigureLaunchSnapshot {
            repo_source: spec.repo_source.clone(),
            branch_name: spec.branch_worktree.clone(),
            skip_permissions: preset.permissions.skip_all,
            mode,
            boss_prompt,
            agent_type,
            session_model,
            codex_model,
            base: spec.base.clone(),
            headroom_enabled: spec.headroom_enabled,
            rtk_enabled: spec.rtk_enabled,
        };

        // Boss mode builds its own Docker workspace from `repo_path` and
        // doesn't consume the worktree machinery — a picked base can't be
        // honored there yet. Be honest about it rather than silently using
        // the default.
        if snapshot.base.is_some() && snapshot.mode == SessionMode::Boss {
            self.add_warning_notification(
                "Base-branch pick is not applied in Boss mode yet — session uses the default base"
                    .to_string(),
            );
        }

        // ONLY check authentication for Boss mode (Docker-based sessions)
        if snapshot.mode == SessionMode::Boss {
            if !self.is_docker_available().await {
                error!("Boss mode requires Docker but Docker is not running");
                self.add_error_notification(
                    "Boss mode requires Docker.\n\nPlease start Docker and try again, or use Interactive mode instead.".to_string()
                );
                return;
            }

            if let Some(home) = dirs::home_dir() {
                let credentials_path = home.join(".agents-in-a-box/auth/.credentials.json");
                if credentials_path.exists() && Self::oauth_token_needs_refresh(&credentials_path) {
                    info!("Boss mode selected - OAuth tokens need refresh, attempting refresh");
                    match self.refresh_oauth_tokens().await {
                        Ok(()) => info!("OAuth tokens refreshed successfully for Boss mode"),
                        Err(e) => {
                            error!("Failed to refresh OAuth tokens for Boss mode: {}", e);
                            self.add_error_notification(format!(
                                "Failed to refresh OAuth tokens: {}\n\nPlease check Docker and try again.",
                                e
                            ));
                            return;
                        }
                    }
                }
            }

            if Self::is_first_time_setup() {
                info!(
                    "Boss mode selected but authentication not set up, switching to auth setup view"
                );
                self.current_screen = screen_ids::AUTH_SETUP.to_string();
                self.auth_setup_state = Some(AuthSetupState {
                    selected_method: AuthMethod::OAuth,
                    api_key_input: String::new(),
                    is_processing: false,
                    error_message: Some(
                        "Boss mode requires Docker authentication.\n\nPlease set up Claude authentication to continue.".to_string()
                    ),
                    show_cursor: false,
                });
                self.new_session_state = None;
                return;
            }
        } else {
            info!(
                "Interactive mode selected - skipping Docker auth check (will use host ~/.claude)"
            );
        }

        // Resolve the repo path. LocalPath uses the path directly. HttpsUrl,
        // SshUrl, and GithubShorthand clone via `RemoteRepoManager` into the
        // `~/.agents-in-a-box/repos/<host>/<owner>/<repo>` cache (Phase 6.5).
        // SshSession is a launch-only path that bypasses worktree creation
        // entirely — it's handled in a dedicated branch below.
        let repo_path = match snapshot.repo_source.clone() {
            crate::git::repo_source::RepoSource::LocalPath(p) => p,
            crate::git::repo_source::RepoSource::SshSession(url) => {
                self.launch_ssh_session_from_configure(&url).await;
                return;
            }
            ref remote @ (crate::git::repo_source::RepoSource::HttpsUrl(_)
            | crate::git::repo_source::RepoSource::SshUrl(_)
            | crate::git::repo_source::RepoSource::GithubShorthand { .. }) => {
                match self.clone_remote_for_configure(remote).await {
                    Ok(path) => path,
                    Err(()) => return,
                }
            }
            crate::git::repo_source::RepoSource::Filter(s) => {
                tracing::warn!(filter = %s, "create_session_from_configure: Filter source not launchable");
                self.add_error_notification(format!(
                    "Could not resolve repository from '{}'. Try a path, owner/repo, or full URL.",
                    s
                ));
                return;
            }
        };

        let session_id = uuid::Uuid::new_v4();

        // Mark step = Creating so the existing render machinery (legacy.rs)
        // picks up the in-flight UI.
        if let Some(ns) = self.new_session_state.as_mut() {
            ns.step = NewSessionStep::Creating;
        }

        tracing::info!(
            "create_session_from_configure: launching session {} for {:?}, branch {} (mode: {:?})",
            session_id,
            repo_path,
            snapshot.branch_name,
            snapshot.mode
        );

        // Remote sources (every star is now remote, plus typed URLs / shorthand)
        // branch their worktree off the remote's DEFAULT branch (origin/HEAD),
        // freshly fetched — never a stale local `main` or the cache's checked-out
        // HEAD. Local-path picks keep the legacy `get_default_branch` flow
        // (`existing_worktree = None`).
        //
        // Gated to Interactive mode: only `create_interactive_session` consumes
        // `existing_worktree`. Boss mode (`create_boss_session`) ignores it and
        // builds its own Docker request from `repo_path`, so preparing a worktree
        // there would orphan it on disk and leave a stray cache branch.
        // Checkout-direct picks from the remote flow can land on a suffixed
        // branch (`feature-x-ab12cd34`) when the branch already has a
        // worktree — track the branch the worktree actually got.
        let mut effective_branch = snapshot.branch_name.clone();
        let existing_worktree =
            if snapshot.repo_source.is_remote() && snapshot.mode == SessionMode::Interactive {
                match self.prepare_remote_worktree(session_id, &repo_path, &snapshot).await {
                    Ok((worktree_path, source_repo, branch)) => {
                        effective_branch = branch;
                        Some((worktree_path, source_repo))
                    }
                    Err(()) => return, // already notified + cancelled
                }
            } else {
                None
            };

        // Local repos: hand the picked base ref (if any) to the worktree
        // machinery. The display form is revparse-able for both kinds —
        // `origin/feature-x` (remote pick) or `feature-x` (local pick).
        let base_start_point = snapshot.base.as_ref().map(|b| b.display.clone());

        let result = self
            .create_session_with_logs(
                &repo_path,
                &effective_branch,
                session_id,
                snapshot.skip_permissions,
                snapshot.mode,
                snapshot.boss_prompt,
                snapshot.agent_type,
                snapshot.session_model,
                snapshot.codex_model,
                existing_worktree,
                base_start_point,
                snapshot.headroom_enabled,
                snapshot.rtk_enabled,
            )
            .await;

        match result {
            Ok(()) => {
                info!("Session created successfully via configure flow");
                self.load_real_workspaces().await;
                if let Err(e) = self.start_log_streaming_for_session(session_id).await {
                    warn!(
                        "Failed to start log streaming for session {}: {}",
                        session_id, e
                    );
                }
                self.ui_needs_refresh = true;
                self.cancel_new_session();
            }
            Err(e) => {
                error!("Failed to create session via configure flow: {}", e);
                // Surface the real failure (e.g. "branch already used by
                // worktree", "worktree already exists", invalid name) BEFORE
                // tearing down the modal. Without this the error only hit the
                // log and the modal closed silently — the user saw a flash and
                // never learned why (Stevie 2026-06-06). cancel_new_session()
                // leaves self.notifications intact, so the 5s toast survives.
                self.add_error_notification(format!("Could not create session: {e}"));
                self.cancel_new_session();
            }
        }
    }

    /// Build a worktree for a remote/star launch. Base policy:
    ///   * no pick — NEW branch off the remote's default (`origin/HEAD`),
    ///     freshly fetched (legacy star policy);
    ///   * base-off pick — NEW branch off `origin/<picked>`;
    ///   * checkout pick — the picked branch itself, as a local tracking
    ///     branch (suffixed when the branch already has a worktree).
    ///
    /// Returns `(worktree_path, source_repo_path, effective_branch)` for
    /// `create_session_with_logs`. On failure: notifies + cancels the
    /// new-session flow and returns `Err(())`.
    ///
    /// Runs on `spawn_blocking` because the `git` CLI calls are synchronous.
    async fn prepare_remote_worktree(
        &mut self,
        session_id: uuid::Uuid,
        cache_path: &std::path::Path,
        snapshot: &ConfigureLaunchSnapshot,
    ) -> Result<(std::path::PathBuf, std::path::PathBuf, String), ()> {
        use crate::components::new_session::configure::BaseMode;
        use crate::git::{RemoteRepoManager, WorktreeManager};

        let cache = cache_path.to_path_buf();
        let branch = snapshot.branch_name.clone();
        let source = snapshot.repo_source.clone();
        let base = snapshot.base.clone();

        let join = tokio::task::spawn_blocking(
            move || -> Result<(std::path::PathBuf, std::path::PathBuf, String), String> {
                let wt_manager =
                    WorktreeManager::new().map_err(|e| format!("worktree manager init: {e}"))?;
                let worktree_path = wt_manager
                    .generate_worktree_path(session_id, &cache, &branch)
                    .map_err(|e| format!("worktree path: {e}"))?;
                let remote_manager =
                    RemoteRepoManager::new().map_err(|e| format!("remote manager init: {e}"))?;
                match base {
                    Some(b) if b.mode == BaseMode::Checkout => {
                        // `clone_repo` already fetched on cache reuse, so
                        // origin/<branch> is fresh. Suffix-collision handling
                        // lives inside checkout_existing_branch_worktree.
                        match remote_manager
                            .checkout_existing_branch_worktree(
                                &cache,
                                &worktree_path,
                                &b.short_name,
                            )
                            .map_err(|e| format!("{e}"))?
                        {
                            Some((suffixed_path, suffixed_branch)) => {
                                Ok((suffixed_path, cache, suffixed_branch))
                            }
                            None => Ok((worktree_path, cache, b.short_name.clone())),
                        }
                    }
                    Some(b) => {
                        remote_manager
                            .create_worktree_off_remote_branch(
                                &cache,
                                &worktree_path,
                                &branch,
                                Some(&b.short_name),
                                &source,
                            )
                            .map_err(|e| format!("{e}"))?;
                        Ok((worktree_path, cache, branch))
                    }
                    None => {
                        remote_manager
                            .create_worktree_off_remote_default(
                                &cache,
                                &worktree_path,
                                &branch,
                                &source,
                            )
                            .map_err(|e| format!("{e}"))?;
                        Ok((worktree_path, cache, branch))
                    }
                }
            },
        )
        .await;

        match join {
            Ok(Ok(paths)) => Ok(paths),
            Ok(Err(msg)) => {
                tracing::error!(error = %msg, "prepare_remote_worktree failed");
                self.add_error_notification(format!("Could not prepare worktree off main: {msg}"));
                self.cancel_new_session();
                Err(())
            }
            Err(join_err) => {
                tracing::error!(error = %join_err, "prepare_remote_worktree task panicked");
                self.add_error_notification(format!("Worktree task panicked: {join_err}"));
                self.cancel_new_session();
                Err(())
            }
        }
    }

    /// Clone a remote repository (HttpsUrl / SshUrl / GithubShorthand) into the
    /// `~/.agents-in-a-box/repos/<host>/<owner>/<repo>` cache and return the
    /// cache path. Posts user-visible notifications for in-flight progress and
    /// terminal errors. On failure: notifies the user, calls
    /// `cancel_new_session()` so the picker reopens, returns `Err(())`.
    ///
    /// Transition from PickRepo → Configure for a given source. Extracted so
    /// both the events.rs dispatcher and the async auth-check handler can call it.
    pub fn advance_pick_repo_to_configure(&mut self, source: crate::git::repo_source::RepoSource) {
        use crate::components::new_session::configure::ConfigureState;
        use crate::config::session_defaults::SessionDefaults;
        use crate::git::repo_source::head_branch;

        if let Some(pick) =
            self.new_session_state.as_ref().and_then(|ns| ns.pick_repo_state.as_ref())
        {
            let path = SessionDefaults::default_path();
            if let Err(err) = pick.defaults.save_to(&path) {
                tracing::warn!(error = %err, "advance_pick_repo_to_configure: persist session-defaults failed");
            }
        }
        let defaults = SessionDefaults::load_from(&SessionDefaults::default_path());
        let label = crate::app::events::derive_repo_label(&source);
        let branch_source = match &source {
            crate::git::repo_source::RepoSource::LocalPath(p) => head_branch(p),
            _ => None,
        };
        let branch_prefix = self.app_config.workspace_defaults.branch_prefix.clone();
        // Every branch already checked out in any worktree (ainb's by-session
        // worktrees + the repo's own checkout + manual worktrees). Single
        // source of truth so the collision guard matches what `git worktree
        // add` will accept — the legacy `list_worktrees()` alone missed by-name
        // worktrees (Stevie 2026-05-27: feat/blog re-launch slipped through;
        // review P1, PR #211 added the repo's own checkout; deduped in #232).
        let repo_path: Option<std::path::PathBuf> = match &source {
            crate::git::repo_source::RepoSource::LocalPath(p) => Some(p.clone()),
            // Remote/star picks: when the clone cache already exists, its refs
            // ARE the repo — seed both guards from it so typing an existing
            // branch warns inline BEFORE launch instead of dying at
            // `git worktree add -b` (Stevie 2026-06-09: feat/ota on the cached
            // shotclubhouse pick slipped through and failed only after Launch).
            // A not-yet-cached remote still starts empty; the base-picker
            // ls-remote refresh backfills `repo_branch_names` later.
            _ => crate::git::RemoteRepoManager::new()
                .ok()
                .and_then(|m| m.cached_source_path(&source)),
        };
        let repo_path = repo_path.as_deref();
        let existing_branches = crate::git::branch_list::in_use_branch_names(repo_path);
        // All existing branch names (local heads + remote-tracking) for the
        // base-off "⚠ exists" guard. Cheap for a local repo or a cached
        // remote; a not-yet-cached remote pick fills this in later when the
        // base picker lists/fetches branches.
        let repo_branch_names: Vec<String> = repo_path
            .map(|p| {
                crate::git::branch_list::list_repo_branches(p)
                    .into_iter()
                    .map(|e| e.short_name)
                    .collect()
            })
            .unwrap_or_default();
        let cfg = ConfigureState::from_pick_repo(
            source.clone(),
            label,
            &defaults,
            branch_source,
            &branch_prefix,
            existing_branches,
            repo_branch_names,
        );
        if let Some(ns) = self.new_session_state.as_mut() {
            ns.configure_state = Some(cfg);
            ns.step = NewSessionStep::Configure;
        }
        tracing::debug!(?source, "advance_pick_repo_to_configure → Configure");
        self.ui_needs_refresh = true;
    }

    /// Pre-check GitHub authentication via `gh auth status`. Updates the
    /// `git_auth_status` field on PickRepoState. If authenticated and a
    /// `pending_clone_source` is waiting, automatically advances to Configure.
    async fn check_git_auth(&mut self) {
        use crate::components::new_session::pick_repo::GitAuthStatus;

        // Bound the probe: a hung `gh` (network stall, credential helper
        // wedged) must not leave the picker stuck in `Checking` forever.
        // Timeout and task-panic both fail closed → NotAuthenticated. Capture
        // the EXACT `gh auth status` output (stderr+stdout) so the failure
        // modal can show the real reason instead of a generic "auth failed".
        let auth_check = tokio::task::spawn_blocking(|| {
            match std::process::Command::new("gh")
                .args(["auth", "status", "--hostname", "github.com"])
                .env("GIT_TERMINAL_PROMPT", "0")
                .output()
            {
                Ok(out) => {
                    // gh writes its human status to stderr; fold in stdout too
                    // in case a future version moves it.
                    let mut msg = String::from_utf8_lossy(&out.stderr).into_owned();
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    if !stdout.trim().is_empty() {
                        if !msg.is_empty() && !msg.ends_with('\n') {
                            msg.push('\n');
                        }
                        msg.push_str(&stdout);
                    }
                    (out.status.success(), msg.trim().to_string())
                }
                Err(e) => (
                    false,
                    format!(
                        "could not run `gh`: {e}\nInstall the GitHub CLI: https://cli.github.com"
                    ),
                ),
            }
        });
        let (auth_ok, auth_msg) =
            match tokio::time::timeout(Duration::from_secs(5), auth_check).await {
                Ok(Ok(res)) => res,
                Ok(Err(join_err)) => {
                    tracing::warn!(error = %join_err, "GitHub auth check task panicked");
                    (false, format!("auth check task panicked: {join_err}"))
                }
                Err(_) => {
                    tracing::warn!("GitHub auth check timed out after 5s");
                    (false, "`gh auth status` timed out after 5s".to_string())
                }
            };

        if let Some(pick) =
            self.new_session_state.as_mut().and_then(|ns| ns.pick_repo_state.as_mut())
        {
            if auth_ok {
                tracing::info!("GitHub auth check passed");
                pick.git_auth_status = Some(GitAuthStatus::Authenticated);
                pick.git_auth_error = None;
                // Auto-advance: take the pending source and emit StartClone
                // via the advance-to-configure path. We replicate the
                // AdvanceTo → Configure transition inline here.
                if let Some(source) = pick.pending_clone_source.take() {
                    pick.git_auth_status = None;
                    self.advance_pick_repo_to_configure(source);
                }
            } else {
                tracing::warn!(error = %auth_msg, "GitHub auth check failed");
                pick.git_auth_status = Some(GitAuthStatus::NotAuthenticated);
                pick.git_auth_error = Some(auth_msg);
            }
        }
        self.ui_needs_refresh = true;
    }

    /// The clone itself runs on `spawn_blocking` because `git2` / `git` CLI
    /// are synchronous and would otherwise block the async runtime.
    async fn clone_remote_for_configure(
        &mut self,
        source: &crate::git::repo_source::RepoSource,
    ) -> Result<std::path::PathBuf, ()> {
        use crate::git::remote_repo_manager::RemoteRepoManager;

        let parsed = match source.parse_components() {
            Ok(p) => p,
            Err(e) => {
                tracing::error!(?source, error = %e, "clone_remote_for_configure: parse_components failed");
                self.add_error_notification(format!("Could not parse repository: {}", e));
                self.cancel_new_session();
                return Err(());
            }
        };

        self.add_info_notification(format!(
            "Cloning {}/{}/{}…",
            parsed.host, parsed.owner, parsed.repo_name
        ));
        self.ui_needs_refresh = true;

        let manager = match RemoteRepoManager::new() {
            Ok(m) => m,
            Err(e) => {
                tracing::error!(error = %e, "clone_remote_for_configure: RemoteRepoManager::new failed");
                self.add_error_notification(format!("Could not initialise repo cache: {}", e));
                self.cancel_new_session();
                return Err(());
            }
        };

        let source_owned = source.clone();
        let parsed_owned = parsed.clone();
        // git2 / git CLI are synchronous — push the clone onto a blocking
        // thread so the async runtime stays responsive.
        let clone_result =
            tokio::task::spawn_blocking(move || manager.clone_repo(&source_owned, &parsed_owned))
                .await;

        let cache_path = match clone_result {
            Ok(Ok(path)) => path,
            Ok(Err(e)) => {
                tracing::error!(error = %e, "clone_remote_for_configure: clone_repo failed");
                self.add_error_notification(format!(
                    "Clone failed for {}/{}: {}",
                    parsed.owner, parsed.repo_name, e
                ));
                self.cancel_new_session();
                return Err(());
            }
            Err(join_err) => {
                tracing::error!(error = %join_err, "clone_remote_for_configure: blocking task panicked");
                self.add_error_notification(format!("Clone task panicked: {}", join_err));
                self.cancel_new_session();
                return Err(());
            }
        };

        self.add_info_notification(format!(
            "Cloned {}/{} → {}",
            parsed.owner,
            parsed.repo_name,
            cache_path.display()
        ));
        self.ui_needs_refresh = true;
        Ok(cache_path)
    }

    /// Launch an interactive SSH session inside tmux from a `ssh://user@host[:port]`
    /// URL. Parses the URL into an `SshTarget`, spawns
    /// `tmux new-session -d -s <name> "<ssh ...>"`, adds the session to the
    /// SSH bucket on `AppState`, and resets the new-session flow. Failure paths
    /// surface a notification and cancel the new-session flow.
    async fn launch_ssh_session_from_configure(&mut self, url: &str) {
        use crate::models::Session;
        use tokio::process::Command;

        let Some(target) = crate::git::repo_source::parse_ssh_session_url(url) else {
            tracing::error!(url = %url, "launch_ssh_session_from_configure: parse failed");
            self.add_error_notification(format!(
                "Could not parse SSH URL: {} (expected ssh://[user@]host[:port])",
                url
            ));
            self.cancel_new_session();
            return;
        };

        // Mark step = Creating so the in-flight UI is shown until tmux returns.
        if let Some(ns) = self.new_session_state.as_mut() {
            ns.step = NewSessionStep::Creating;
        }
        self.ui_needs_refresh = true;

        // tmux session name: `ssh-<host>-<port>` matches the convention parsed
        // by `auto-detect` in load_real_workspaces (search "name.starts_with(\"ssh-\")").
        let safe_host = target.host.replace(['.', '/', ' '], "-");
        let tmux_name = format!("ssh-{}-{}", safe_host, target.port);
        let ssh_cmd = target.to_ssh_command();

        tracing::info!(
            "launch_ssh_session_from_configure: spawning tmux session '{}' running `{}`",
            tmux_name,
            ssh_cmd
        );

        let spawn_result = Command::new("tmux")
            .args(["new-session", "-d", "-s", &tmux_name, &ssh_cmd])
            .status()
            .await;

        match spawn_result {
            Ok(status) if status.success() => {
                let display = target.display_name();
                let mut session = Session::new_ssh_session(display.clone(), target);
                session.tmux_session_name = Some(tmux_name.clone());
                session.status = crate::models::SessionStatus::Idle;
                self.ssh_sessions.push(session);
                self.add_info_notification(format!("SSH session ready: {}", display));
                self.ui_needs_refresh = true;
                // Refresh workspaces / sessions list so the new bucket entry is
                // discoverable through the normal flow too.
                self.load_real_workspaces().await;
                self.cancel_new_session();
            }
            Ok(status) => {
                tracing::error!(
                    "launch_ssh_session_from_configure: tmux exited with {:?}",
                    status.code()
                );
                self.add_error_notification(format!(
                    "Failed to launch SSH session (tmux exit {:?})",
                    status.code()
                ));
                self.cancel_new_session();
            }
            Err(e) => {
                tracing::error!(error = %e, "launch_ssh_session_from_configure: tmux spawn failed");
                self.add_error_notification(format!("Failed to spawn tmux for SSH session: {}", e));
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
        codex_model: Option<crate::models::CodexModel>,
        existing_worktree: Option<(std::path::PathBuf, std::path::PathBuf)>,
        base_start_point: Option<String>,
        headroom_enabled: bool,
        rtk_enabled: bool,
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
                    codex_model,
                    existing_worktree,
                    base_start_point,
                    headroom_enabled,
                    rtk_enabled,
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
    /// * `base_start_point` - For local repos: revparse-able ref the new
    ///   branch is cut from (`origin/feature-x` / `feature-x`); `None` keeps
    ///   the legacy default-branch policy
    async fn create_interactive_session(
        &mut self,
        repo_path: &std::path::Path,
        branch_name: &str,
        session_id: Uuid,
        skip_permissions: bool,
        agent_type: crate::models::SessionAgentType,
        model: Option<crate::models::ClaudeModel>,
        codex_model: Option<crate::models::CodexModel>,
        existing_worktree: Option<(std::path::PathBuf, std::path::PathBuf)>,
        base_start_point: Option<String>,
        headroom_enabled: bool,
        rtk_enabled: bool,
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
                    codex_model,
                    headroom_enabled,
                    rtk_enabled,
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
                    base_start_point,
                    skip_permissions,
                    agent_type,
                    model,
                    codex_model,
                    headroom_enabled,
                    rtk_enabled,
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

        // Recover the exact launch settings the session was CREATED with from
        // persisted metadata (authoritative — survives stop + full TUI restart,
        // unlike the in-memory Session which is rebuilt with defaults once a
        // session goes Stopped). `skip_permissions` is `Option`: `None` (legacy
        // metadata predating the field) → default to yolo
        // (`--dangerously-skip-permissions`), per the "default dangerously-skip"
        // requirement. `Some(v)` preserves the value the session was started with.
        let (skip_permissions, model, codex_model) = metadata
            .as_ref()
            .map(|m| (m.skip_permissions.unwrap_or(true), m.model, m.codex_model))
            .unwrap_or((true, None, None));

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
                    codex_model,
                    metadata.agent_type,
                    transcript.clone(),
                    true, // resume_requested — Enter/r on a Stopped session
                    metadata.headroom_enabled,
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
                // Codex resumes via `codex resume --last`, Copilot via `--continue` —
                // both continue the most recent session in the worktree cwd.
                (SessionAgentType::Codex, _) | (SessionAgentType::Copilot, _) => {
                    "Resuming most recent session".to_string()
                }
                (other, _) => {
                    format!("Started fresh ({} has no resume support)", other.name())
                }
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
    /// `ainb-toolkit utilities/utils/spawn-agent-lib.sh:30-69`.
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
                AsyncAction::CreateSessionFromConfigure(spec) => {
                    self.create_session_from_configure(spec).await;
                }
                AsyncAction::CheckGitAuth => {
                    self.check_git_auth().await;
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
                AsyncAction::BulkResumeSessions(session_ids, trigger) => {
                    let total = session_ids.len();
                    let mut resumed = 0;
                    let mut failed = 0;
                    for id in session_ids {
                        if let Err(e) = self.resume_interactive_session(id, trigger.clone()).await {
                            error!("Failed to resume session {}: {}", id, e);
                            failed += 1;
                        } else {
                            resumed += 1;
                        }
                    }
                    // resume_interactive_session refreshes per-session on success;
                    // refresh once more so a final all-failed batch still repaints.
                    self.load_real_workspaces().await;
                    if failed > 0 {
                        self.add_warning_notification(format!(
                            "Resumed {}/{} sessions ({} failed)",
                            resumed, total, failed
                        ));
                    } else {
                        self.add_success_notification(format!("Resumed {} session(s)", resumed));
                    }
                    self.ui_needs_refresh = true;
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
                AsyncAction::DowngradeHeadroom(session_id) => {
                    info!("Downgrading Headroom for session {}", session_id);
                    if let Err(e) = self.downgrade_headroom_session(session_id).await {
                        error!(
                            "Failed to downgrade Headroom for session {}: {}",
                            session_id, e
                        );
                        self.add_error_notification(format!("Failed to downgrade Headroom: {}", e));
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
                action @ AsyncAction::AttachWitr => {
                    debug!("AttachWitr action deferred to main loop");
                    self.pending_async_action = Some(action);
                }
                action @ AsyncAction::AttachAbtop => {
                    debug!("AttachAbtop action deferred to main loop");
                    self.pending_async_action = Some(action);
                }
                action @ AsyncAction::SetupAbtopRateLimits => {
                    debug!("SetupAbtopRateLimits action deferred to main loop");
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
                action @ AsyncAction::KillWorkspaceShell(_) => {
                    debug!("KillWorkspaceShell action deferred to main loop");
                    self.pending_async_action = Some(action);
                }
                action @ AsyncAction::OpenInEditor(_) => {
                    debug!("OpenInEditor action deferred to main loop");
                    self.pending_async_action = Some(action);
                }
                AsyncAction::OnboardingInstallDep(dep_id) => {
                    use crate::components::onboarding::state::DepInstall;
                    use crate::setup::{catalog, install_dep_capture};
                    info!("Installing dependency '{dep_id}' from onboarding");
                    // Own the catalog dep so it can move into spawn_blocking.
                    let dep = catalog().into_iter().flat_map(|t| t.deps).find(|d| d.id == dep_id);
                    let result = match dep {
                        Some(dep) => tokio::task::spawn_blocking(move || install_dep_capture(&dep))
                            .await
                            .unwrap_or_else(|e| Err(e.to_string())),
                        None => Err("unknown dependency".to_string()),
                    };
                    if let Some(os) = &mut self.onboarding_state {
                        match result {
                            Ok(()) => {
                                // Mark done; the row keeps a ✓ marker until the
                                // user presses `r` to re-check (which flips the
                                // real checkbox green).
                                os.install_states.insert(dep_id.clone(), DepInstall::Done);
                                os.error_message = None;
                                os.status_message =
                                    Some(format!("✓ installed {dep_id} — press r to re-check"));
                            }
                            Err(msg) => {
                                os.install_states
                                    .insert(dep_id.clone(), DepInstall::Error(msg.clone()));
                                os.status_message = None;
                                os.error_message = Some(format!("✗ {dep_id}: {msg}"));
                            }
                        }
                    }
                    self.ui_needs_refresh = true;
                }
                AsyncAction::OnboardingCheckDeps => {
                    info!("Running onboarding dependency check");
                    use crate::setup::{RealEnv, detect_all};
                    // Run blocking I/O on dedicated thread pool to avoid blocking async runtime
                    match tokio::task::spawn_blocking(|| detect_all(&RealEnv)).await {
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
                AsyncAction::SkillPreviewFetch(uri) => {
                    info!(uri = %uri, "Fetching skill source for preview");
                    let ainb_home = ainb_skill_core::default_ainb_home();
                    let fetch_uri = uri.clone();
                    // Git clone + adapter scan — blocking I/O off the runtime.
                    let result = tokio::task::spawn_blocking(move || {
                        ainb_cli::source::preview_source(&ainb_home, &fetch_uri)
                    })
                    .await;
                    self.skill_manager_state.preview_loading = None;
                    match result {
                        Ok(Ok(preview)) if preview.units.is_empty() => {
                            self.add_warning_notification(format!(
                                "{uri}: fetched OK but no skills/agents/commands found"
                            ));
                        }
                        Ok(Ok(preview)) => {
                            info!(uri = %uri, units = preview.units.len(),
                                  "SkillManager: source preview open");
                            // Pre-check + badge units already installed: the
                            // manifest-declared URIs of the units we already
                            // track. `declared_uri` is the same
                            // `<source>@<ref>/<path>` shape the picker rebuilds.
                            let installed_uris: std::collections::HashSet<String> = self
                                .skill_manager_state
                                .units
                                .iter()
                                .map(|u| u.declared_uri.clone())
                                .collect();
                            self.skill_manager_state.preview = Some(
                                crate::components::skill_manager_screen::SourcePreviewViewState::new(
                                    preview,
                                    &installed_uris,
                                ),
                            );
                        }
                        Ok(Err(e)) => {
                            self.add_error_notification(format!("preview failed: {e:#}"));
                        }
                        Err(e) => {
                            self.add_error_notification(format!("preview task failed: {e}"));
                        }
                    }
                    self.ui_needs_refresh = true;
                }
            }
        }
        Ok(())
    }

    /// Run OAuth authentication setup
    async fn run_oauth_setup(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        use crossterm::{
            event::{
                DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste,
                EnableMouseCapture,
            },
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

        // Disable raw mode and tear down input modes that match TUI startup
        // (see main.rs: EnterAlternateScreen + EnableMouseCapture + EnableBracketedPaste).
        let _ = disable_raw_mode();
        let _ = execute!(
            std::io::stdout(),
            LeaveAlternateScreen,
            DisableMouseCapture,
            DisableBracketedPaste,
        );

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

        // Re-enable raw mode and the full input mode set established at startup —
        // without re-enabling mouse capture + bracketed paste, mouse events stop
        // arriving after the auth flow returns to the TUI.
        use crossterm::terminal::{EnterAlternateScreen, enable_raw_mode};
        let _ = enable_raw_mode();
        let _ = execute!(
            std::io::stdout(),
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableBracketedPaste,
        );

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

                    // Phase 6 (new-session redesign): restart routes the user
                    // straight to the Configure screen with the source repo
                    // pre-selected. The user can review the preset / branch /
                    // prompt and press Enter to relaunch.
                    use crate::components::new_session::configure::ConfigureState;
                    use crate::config::session_defaults::SessionDefaults;
                    use crate::git::repo_source::RepoSource;

                    let defaults = SessionDefaults::load_from(&SessionDefaults::default_path());
                    let repo_source = RepoSource::LocalPath(workspace.path.clone());
                    let repo_label = workspace
                        .path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .map(str::to_string)
                        .unwrap_or_else(|| workspace.path.display().to_string());
                    let branch_source = crate::git::repo_source::head_branch(&workspace.path);
                    let branch_prefix = self.app_config.workspace_defaults.branch_prefix.clone();
                    // Same complete in-use list the repo-picker path uses. The
                    // legacy `list_worktrees()` here only saw legacy UUID dirs
                    // and missed every by-name worktree, so a re-launch onto an
                    // already-checked-out branch slipped the collision guard and
                    // died at `git worktree add` (Stevie 2026-06-06: feat/ota).
                    let existing_branches = crate::git::branch_list::in_use_branch_names(Some(
                        workspace.path.as_path(),
                    ));
                    // All existing branch names for the base-off "⚠ exists"
                    // guard (restart is always a local repo path).
                    let repo_branch_names =
                        crate::git::branch_list::list_repo_branches(&workspace.path)
                            .into_iter()
                            .map(|e| e.short_name)
                            .collect();
                    let configure_state = ConfigureState::from_pick_repo(
                        repo_source,
                        repo_label,
                        &defaults,
                        branch_source,
                        &branch_prefix,
                        existing_branches,
                        repo_branch_names,
                    );

                    self.current_screen = screen_ids::NEW_SESSION.to_string();
                    self.new_session_state = Some(NewSessionState {
                        step: NewSessionStep::Configure,
                        configure_state: Some(configure_state),
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
                            self.add_error_notification(format!("❌ Failed to restart CLI: {}", e));
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
            // Build the Warp-style Code Review model for the default Review tab.
            git_state.refresh_review();

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

    /// The ainb-hooks `agent` string a session's events are recorded
    /// under, or `None` for session types that don't emit hook events
    /// (plain shell / SSH, and the not-yet-wired Gemini/Kiro).
    const fn agent_hook_name(agent: SessionAgentType) -> Option<&'static str> {
        match agent {
            SessionAgentType::Claude => Some("claude"),
            SessionAgentType::Codex => Some("codex"),
            SessionAgentType::Copilot => Some("copilot"),
            _ => None,
        }
    }

    /// Decide a single session's attention marker from recent hook
    /// events. Pure + deterministic (no clock / IO) so it is directly
    /// unit-testable.
    ///
    /// `recent` MUST be newest-first (as [`Store::recent_since`] returns
    /// it). The marker is the kind implied by the newest user-facing
    /// event for this session's `(cwd, agent)` whose `ts` is strictly
    /// newer than `baseline_ms` — unless the agent is currently
    /// `generating` (suppressed; the `●` busy dot covers it) or the only
    /// match is a `Finished` turn past its short TTL. Returns `None`
    /// (blank — no marker) when nothing qualifies, which is the common
    /// case for an idle session with no pending hook event.
    fn attention_for_session(
        session_cwd: &str,
        agent: Option<&str>,
        generating: bool,
        baseline_ms: i64,
        now_ms: i64,
        recent: &[ainb_plugin_notifyd::NotificationRecord],
    ) -> Option<ainb_plugin_notifyd::AlertKind> {
        use ainb_plugin_notifyd::{AlertKind, classify_attention};
        // A `[✓]` Finished marker is informational; retire it after this.
        const FINISHED_TTL_MS: i64 = 5 * 60 * 1000;

        if generating {
            return None;
        }
        let agent = agent?;
        let cwd = session_cwd.trim_end_matches('/');

        for rec in recent {
            // `recent` is newest-first and `ts`-sorted globally, so the
            // first row at/under the baseline means none remain newer.
            if rec.ts <= baseline_ms {
                break;
            }
            if rec.agent != agent || rec.cwd.trim_end_matches('/') != cwd {
                continue;
            }
            let Some(kind) = classify_attention(&rec.raw_event) else {
                continue;
            };
            // Newest qualifying event wins. A long-finished turn isn't
            // worth a marker — and it supersedes any older question, so
            // we stop rather than fall back to a staler event.
            if kind == AlertKind::Finished && now_ms.saturating_sub(rec.ts) > FINISHED_TTL_MS {
                return None;
            }
            return Some(kind);
        }
        None
    }

    /// Recent user-facing hook events across the fleet, newest-first, or
    /// `None` when the notifications store doesn't exist yet (daemon
    /// never ran) or can't be read. Floored at app start so pre-existing
    /// history never marks, and windowed so a long-lived TUI doesn't
    /// accrue stale markers.
    ///
    /// Opens the store per call rather than holding a handle: the read
    /// is microseconds, runs at most every preview-refresh interval
    /// (~5s), and keeps the daemon as the DB's sole long-lived owner.
    /// The `exists()` guard avoids `Store::open` creating an empty DB on
    /// machines where notifications were never set up.
    ///
    /// The window is purely time-based (`now − LOOKBACK`), **not** floored
    /// at app start — so opening ainb immediately surfaces sessions that
    /// were already waiting before launch. Stale `[✓]` turns don't pile up
    /// because the `Finished` marker self-retires on its own short TTL (see
    /// [`Self::attention_for_session`]); only genuinely-pending `[?]` / `[!]`
    /// from the window survive.
    fn recent_attention_events(
        &self,
        now_ms: i64,
    ) -> Option<Vec<ainb_plugin_notifyd::NotificationRecord>> {
        // Only events within this rolling window can mark a session.
        const LOOKBACK_MS: i64 = 6 * 60 * 60 * 1000;
        // Bounds query cost; ample for any realistic active fleet.
        const QUERY_LIMIT: u32 = 500;

        let db = ainb_plugin_notifyd::Paths::from_home().ok()?.db;
        if !db.exists() {
            return None;
        }
        let store = ainb_plugin_notifyd::Store::open(&db).ok()?;
        let floor = now_ms - LOOKBACK_MS;
        match store.recent_since(floor, QUERY_LIMIT) {
            Ok(rows) => Some(rows),
            Err(e) => {
                debug!("attention: notifications store read failed: {e}");
                None
            }
        }
    }

    /// Recompute every session's attention marker (`[!]`/`[?]`/`[✓]`)
    /// from recent ainb-hooks events. Attached sessions never nag and
    /// have their baseline advanced to "now", so re-marking only happens
    /// for activity that arrives after the user looks away.
    fn refresh_attention_markers(&mut self, now_ms: i64) {
        let Some(recent) = self.recent_attention_events(now_ms) else {
            return;
        };

        // Phase 1 — read-only compute (no mutable borrow of self).
        let mut marks: Vec<(Uuid, Option<ainb_plugin_notifyd::AlertKind>, bool)> = Vec::new();
        for ws in &self.workspaces {
            for s in &ws.sessions {
                if s.is_attached {
                    marks.push((s.id, None, true));
                    continue;
                }
                let generating = matches!(s.status, crate::models::SessionStatus::Running);
                // Default 0: with no per-session clear point yet, any event in
                // the lookback window can mark — so pre-launch waiters show up.
                // Attaching advances this to "now" (see below).
                let baseline = self.attention_baseline.get(&s.id).copied().unwrap_or(0);
                let kind = Self::attention_for_session(
                    &s.workspace_path,
                    Self::agent_hook_name(s.agent_type),
                    generating,
                    baseline,
                    now_ms,
                    &recent,
                );
                marks.push((s.id, kind, false));
            }
        }

        // Phase 2 — apply. Bumping an attached session's baseline and
        // writing its marker are separate self borrows, taken in turn.
        let mut changed = false;
        for (id, kind, attached) in marks {
            if attached {
                self.attention_baseline.insert(id, now_ms);
            }
            if let Some(s) = self.find_session_mut(id) {
                if s.live_attention != kind {
                    s.live_attention = kind;
                    changed = true;
                }
            }
        }
        if changed {
            self.ui_needs_refresh = true;
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

        // Non-selected sessions only need a status (running/idle) refresh, which
        // is not time-critical — sweep them on a longer cadence so we don't
        // spawn one `capture-pane` per non-selected session on every 5s preview
        // refresh. (perf: bead 9pb)
        const STATUS_INTERVAL_SECS: u64 = 20;
        let do_status_check = match self.last_status_check {
            Some(last) => now.duration_since(last).as_secs() >= STATUS_INTERVAL_SECS,
            None => true,
        };
        if do_status_check {
            self.last_status_check = Some(now);
        }

        // updates: (session_id, content, claude_running) for the selected session.
        // Attention markers are derived separately from hook events in
        // `refresh_attention_markers`, not from live pane state.
        let mut updates = Vec::new();
        // status_updates: (session_id, claude_running) for non-selected sessions
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

            // While the interactive embed is live, the selected session's
            // pane renders straight from the embed's vt100 screen — the full
            // capture would be pure subprocess waste. Fall through to the
            // cheap status-dot check instead (the collapsed rail still needs
            // those for every session).
            if is_selected && !self.is_interactive_pane() {
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
            } else if do_status_check {
                // Non-selected sessions: only capture visible area for status
                // detection, and only on the longer status cadence. Much
                // cheaper — just the last screenful (~50 lines). (perf: 9pb)
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
            // Accumulate the change flag inside the session borrow, then
            // touch `self.ui_needs_refresh` only after it ends (avoids a
            // borrow conflict between `find_session_mut` and `self`).
            let mut changed = false;
            if let Some(session) = self.find_session_mut(session_id) {
                use crate::models::SessionStatus;
                let new_status = if claude_running {
                    SessionStatus::Running
                } else {
                    SessionStatus::Idle
                };
                if session.status != new_status {
                    session.set_status(new_status);
                    changed = true;
                }
            }
            if changed {
                self.ui_needs_refresh = true;
            }
        }

        // Apply updates for the selected session (preview always changes,
        // so this loop unconditionally requests a refresh).
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
            }

            self.ui_needs_refresh = true;
        }

        // Now that per-session running/idle status is current, recompute
        // each session's attention marker (`[!]`/`[?]`/`[✓]`) from recent
        // ainb-hooks events. Independent of pane capture, so it also
        // covers sessions with no live pane (stopped / never-captured).
        self.refresh_attention_markers(chrono::Utc::now().timestamp_millis());

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
        let model = session.model;
        let codex_model = session.codex_model;

        let provider = match agent_type {
            SessionAgentType::Claude => CliProvider::Claude,
            SessionAgentType::Codex => CliProvider::Codex,
            SessionAgentType::Gemini => CliProvider::Gemini,
            SessionAgentType::Copilot => CliProvider::Copilot,
            SessionAgentType::Shell | SessionAgentType::Ssh | SessionAgentType::Kiro => {
                anyhow::bail!("Restart unsupported for agent type {:?}", agent_type);
            }
        };

        // Load persisted metadata once — used for both the resume-history probe
        // (Claude, keyed off the worktree cwd) and the Headroom routing flag.
        let store = crate::interactive::SessionStore::load();
        let metadata = store.sessions.get(&tmux_session_name);

        // Restart continues the existing conversation, for parity with the
        // Stopped-session resume path: Claude `--continue`, Codex `resume
        // --last`, Copilot `--continue`. `has_history` gates Claude's
        // `--continue` (no prior transcript → fresh, avoids a dead pane).
        let has_history = agent_type == SessionAgentType::Claude
            && metadata
                .map(|m| Self::find_latest_transcript(&m.worktree_path).is_some())
                .unwrap_or(false);

        let cmd_parts =
            crate::interactive::session_manager::InteractiveSessionManager::build_cli_cmd_parts(
                &provider,
                agent_type,
                skip_permissions,
                model,
                codex_model,
                true, // resume_requested — restart continues the conversation
                has_history,
            );

        // Preserve per-session Headroom routing across restart. `send-keys`
        // bypasses build_env_setup_for_provider, so re-derive the proxy export
        // from the persisted SessionMetadata (keyed by tmux name) and prepend
        // it — otherwise a restarted HR session would silently stop routing
        // through the proxy.
        //
        // Mirror the launch path (`start_cli_in_tmux`): the stored flag is
        // *intent*; only inject the base URL when the proxy is actually
        // healthy. Injecting a dead-port URL would brick the restarted CLI on
        // connection-refused. Ensure the proxy first; degrade to direct on
        // failure rather than pointing the session at a closed port.
        let mut headroom_active = metadata.map(|m| m.headroom_enabled).unwrap_or(false)
            && matches!(
                agent_type,
                SessionAgentType::Claude | SessionAgentType::Codex
            );
        if headroom_active {
            if let Err(e) = crate::headroom::ensure_proxy_running().await {
                warn!(
                    "headroom proxy unavailable on restart — running DIRECT, no compression: {e}"
                );
                headroom_active = false;
            }
        }
        let cli_cmd = format!(
            "{}{}",
            crate::interactive::session_manager::headroom_env_prefix(agent_type, headroom_active),
            cmd_parts.join(" ")
        );

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

    /// Flip headroom off for a running session and replace its CLI process.
    ///
    /// Steps:
    /// 1. Resolve the session and validate it is Claude or Codex.
    /// 2. Load the SessionStore; check that headroom_enabled is true.
    /// 3. Set headroom_enabled = false and save the store.
    /// 4. Build the resume command: `[provider] [--skip-perms] [--continue for Claude]`.
    ///    No env prefix (headroom is now off → `headroom_env_prefix(…, false)` == "").
    /// 5. Replace the running CLI with `tmux respawn-pane -k` using the same
    ///    `sh -c '…exec cli …'` shape as `start_cli_in_tmux`.
    ///    `respawn-pane -k` kills the running process and starts fresh in-place,
    ///    which is the only way to clear env vars from a running process.
    ///    Codex has no `--continue` flag — it restarts fresh (noted in notification).
    async fn downgrade_headroom_session(&mut self, session_id: Uuid) -> anyhow::Result<()> {
        use crate::config::CliProvider;
        use crate::models::session::SessionAgentType;
        use anyhow::Context;
        use tokio::process::Command;

        // --- 1. Resolve session ---
        let session = self
            .find_session(session_id)
            .ok_or_else(|| anyhow::anyhow!("Session not found"))?;

        let tmux_session_name = session
            .tmux_session_name
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No tmux session associated with this session"))?
            .clone();

        let agent_type = session.agent_type;
        let skip_permissions = session.skip_permissions;

        // --- 1a. Only Claude/Codex are Headroom-capable ---
        let provider = match agent_type {
            SessionAgentType::Claude => CliProvider::Claude,
            SessionAgentType::Codex => CliProvider::Codex,
            other => {
                self.add_warning_notification(format!(
                    "Headroom is Claude/Codex only — {:?} does not use the proxy",
                    other
                ));
                return Ok(());
            }
        };

        // --- 2. Check and flip headroom_enabled in SessionStore ---
        let mut store = crate::interactive::SessionStore::load();
        match store.sessions.get(&tmux_session_name) {
            None => {
                self.add_warning_notification(
                    "Session not found in store — Headroom state unknown".to_string(),
                );
                return Ok(());
            }
            Some(meta) if !meta.headroom_enabled => {
                self.add_info_notification("Session is already direct (Headroom off)".to_string());
                return Ok(());
            }
            _ => {}
        }

        // --- 3. Persist headroom_enabled = false ---
        if let Some(meta) = store.sessions.get_mut(&tmux_session_name) {
            meta.headroom_enabled = false;
        }
        if let Err(e) = store.save() {
            // Non-fatal: we still attempt the respawn; the flag will be
            // re-read from a stale store on the next restart, so log clearly.
            warn!(
                "Failed to persist headroom_enabled=false for {}: {}",
                tmux_session_name, e
            );
        }

        // --- 4. Build the resume command (no env prefix — headroom is off) ---
        //
        // env_setup is intentionally empty: `headroom_env_prefix(…, false)` == ""
        // and we are not injecting an API key here (the original launch path
        // already injected it into the pane's environment; `respawn-pane -k`
        // inherits from the ainb-tui process which has the correct key).
        let mut cmd_parts: Vec<String> = vec![provider.command().to_string()];
        if skip_permissions {
            cmd_parts.push(provider.skip_permissions_flag().to_string());
        }
        // Claude: `--continue` (-c) resumes the most recent conversation in the cwd.
        // Codex: no continue/resume flag exists — restarts fresh.
        let codex_fresh_note = if agent_type == SessionAgentType::Claude {
            cmd_parts.push("--continue".to_string());
            ""
        } else {
            " (Codex restarted fresh — no --continue flag)"
        };

        let cli_cmd = cmd_parts.join(" ");

        info!(
            "Downgrading Headroom for {} in '{}': cmd={}",
            provider.display_name(),
            tmux_session_name,
            cli_cmd
        );

        // --- 5. Replace the running CLI via tmux respawn-pane -k ---
        //
        // Mirrors `start_cli_in_tmux` exactly:
        //   - `remain-on-exit on` first so any startup error stays visible.
        //   - `respawn-pane -k -t <name> sh -c 'exec <cmd>'`
        //     The `exec` replaces `sh` itself; the pane ends up running only
        //     the CLI binary (same as the original launch). Because env_setup
        //     is empty we could use the argv path, but wrapping in `sh -c 'exec …'`
        //     is consistent with start_cli_in_tmux and future-proof.
        let target = tmux_session_name.clone();

        // Set remain-on-exit so startup errors stay visible (best-effort).
        let _ = Command::new("tmux")
            .args(["set-option", "-w", "-t", &target, "remain-on-exit", "on"])
            .output()
            .await;

        let full_line = format!("exec {cli_cmd}");
        let output = Command::new("tmux")
            .args(["respawn-pane", "-k", "-t", &target, "sh", "-c", &full_line])
            .output()
            .await
            .context("Failed to invoke tmux respawn-pane for Headroom downgrade")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // Restore the headroom flag in the store so the next manual
            // restart picks it back up (best-effort).
            let mut store2 = crate::interactive::SessionStore::load();
            if let Some(meta) = store2.sessions.get_mut(&tmux_session_name) {
                meta.headroom_enabled = true;
            }
            let _ = store2.save();
            anyhow::bail!(
                "tmux respawn-pane failed for {}: {}",
                tmux_session_name,
                stderr
            );
        }

        // Update in-memory status.
        if let Some(session) = self.find_session_mut(session_id) {
            session.set_status(crate::models::SessionStatus::Running);
        }

        self.add_success_notification(format!(
            "Headroom OFF for this session — resumed direct (no compression){}",
            codex_fresh_note
        ));

        info!(
            "Headroom downgraded for {} in tmux session '{}'",
            provider.display_name(),
            tmux_session_name
        );
        Ok(())
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

    /// Consume pending `ui.close_request` publishes from plugins.
    ///
    /// A plugin publishes this topic when it receives an `Esc` with no
    /// internal state left to pop (its root view — see the burndown
    /// plugin's Esc handler). The snapshot store stamps every publish
    /// with a monotonic version, so polling by version honours each
    /// request at most once; the version is consumed on sight whether
    /// or not it triggers navigation, so a request observed while the
    /// user is on a different screen is absorbed instead of firing
    /// later. A matching request navigates back to the screen the
    /// panel was opened from (same pop as `AppEvent::PanelBack`).
    ///
    /// Called from `App::tick_plugin_renders`, which already holds a
    /// cloned runtime `handle`, so it's passed in rather than re-cloned
    /// per render tick.
    pub fn tick_panel_close_requests(&mut self, handle: &ainb_plugin_runtime::RuntimeHandle) {
        let Some((payload, version, publisher)) =
            handle.snapshot_get_versioned(ainb_plugin_runtime::topics::UI_CLOSE_REQUEST)
        else {
            return;
        };
        if self.last_panel_close_version == Some(version) {
            return;
        }
        self.last_panel_close_version = Some(version);
        if !panel_close_matches(&self.current_screen, &payload, publisher.as_str()) {
            return;
        }
        let target = self
            .previous_screen
            .take()
            .unwrap_or_else(|| crate::app::screens::ids::HOME.to_string());
        tracing::info!(
            from = %self.current_screen,
            to = %target,
            "ui.close_request: closing plugin panel"
        );
        self.current_screen = target;
        self.ui_needs_refresh = true;
    }
}

/// `true` when a `ui.close_request` payload names the currently-focused
/// plugin screen AND was published by the plugin that owns it. Rejects:
/// requests while a non-plugin screen is focused (a plugin can't close
/// the session list out from under the user), publishes from any plugin
/// other than the focused screen's owner (the publisher stamp comes
/// from the wire connection, not the payload, so it can't be forged),
/// and malformed payloads.
fn panel_close_matches(current_screen: &str, payload: &[u8], publisher: &str) -> bool {
    let Some(owner) = crate::app::screens::builtin::plugin_id_for_screen(current_screen) else {
        return false;
    };
    if publisher != owner {
        tracing::warn!(
            publisher,
            owner,
            screen = %current_screen,
            "ui.close_request: publisher does not own the focused screen — ignoring"
        );
        return false;
    }
    match serde_json::from_slice::<ainb_plugin_runtime::topics::UiCloseRequest>(payload) {
        Ok(req) => req.screen_id == current_screen,
        Err(e) => {
            tracing::warn!(error = %e, "ui.close_request: malformed payload — ignoring");
            false
        }
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
    /// Filesystem watcher that keeps the burndown usage snapshot live by
    /// nudging session-reader to rescan on provider-dir changes. Held by
    /// `App` so the watch (and its debounce task) stops when `App` drops.
    /// `None` until [`App::init`] runs, or when no provider dir is
    /// watchable — the burndown then keeps its press-`r` refresh behaviour.
    usage_dir_watcher: Option<crate::models::usage_dir_watcher::UsageDirWatcher>,
}

impl App {
    pub fn new() -> Self {
        Self {
            state: AppState::new(),
            plugin_runtime_owner: None,
            usage_dir_watcher: None,
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
    /// Drive plugin-owned screens. Returns `true` if a fresh plugin frame was
    /// drained into `pending_plugin_renders` this tick, so the render loop can
    /// treat that as a reason to repaint (perf: bead `wai` dirty-gate).
    pub fn tick_plugin_renders(&mut self) -> bool {
        // Clone the cheap Send + Clone handle so we can hold a reference
        // to the runtime while also mutably borrowing the various
        // `state.*` plugin caches below.
        let Some(handle) = self.state.plugin_runtime.clone() else {
            return false;
        };
        let mut drained = false;

        // Honour any pending plugin close request (root-view Esc) before
        // kicking renders — a closed screen shouldn't get another paint.
        self.state.tick_panel_close_requests(&handle);

        // Static plugin-screen routing table. Pairs a stable screen id
        // (consumed by `PluginScreen` and matched against
        // `state.current_screen`) with the plugin id that owns it.
        const PLUGIN_SCREENS: &[(&str, &str)] = &[
            (crate::app::screens::ids::ANALYTICS, "burndown"),
            (crate::app::screens::ids::WITR, "witr"),
            (crate::app::screens::ids::LEARNINGS, "learnings"),
            (crate::app::screens::ids::ABTOP, "abtop"),
            (crate::app::screens::ids::HANGAR, "hangar-tui"),
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
            // hand-off the render thread relies on. Unconditional (no
            // visibility gate): a frame that completed just before the
            // user navigated away must still land in the cache so the
            // screen repaints instantly on return.
            if let Some(buf) = handle.try_recv_render(&pid) {
                self.state.pending_plugin_renders.insert((*screen_id).to_string(), buf);
                drained = true;
            }

            // Visibility gate: only the plugin owning the focused screen
            // gets render kicks. `LayoutComponent::render` dispatches
            // exactly `state.current_screen` through the screen registry,
            // so a hidden screen's buffer is never painted — kicking its
            // renders only burns CPU. Concretely this stops (a) a
            // self-animating plugin (search spinner returning
            // `redraw=true`, which re-marks the dirty flag each frame)
            // from re-rendering an invisible screen at tick cadence, and
            // (b) the startup storm where the registration-seeded dirty
            // flag kicked all five screen plugins on the first tick —
            // `Command::Render` lazy-spawns the subprocess via
            // `ensure_running`, so that defeated `spawn = "lazy"`. Eager
            // plugins are unaffected: registration pokes them with
            // `EnsureSpawned` (see `register_kept` in the runtime), not
            // this loop.
            //
            // MUST stay above `take_render_dirty`: the gate skips the
            // consume, so a hidden plugin's dirty flag survives until the
            // user opens the screen and the first tick after the switch
            // kicks the deferred paint.
            if self.state.current_screen != *screen_id {
                continue;
            }

            // Viewport comes from the previous frame's allocated area
            // (stashed by `PluginScreen::render`). Falls back to (0, 0)
            // before the first paint — the plugin treats that as "use
            // your own fallback size", which keeps the first frame
            // sensible until the area cache fills in.
            let (width, height) =
                self.state.plugin_render_areas.get(*screen_id).copied().unwrap_or((0, 0));

            // Force a render kick whenever the live area differs from
            // the one our last kick used. This is what carries a plugin
            // screen from the seed `(0, 0)` render (which paints into the
            // plugin's fallback size, off-screen for the real layout) to
            // a render at the actual allocated viewport once
            // `PluginScreen::render` has stashed it — and likewise on any
            // resize. Plugins fed by a host-published snapshot get
            // re-marked dirty by that publish, but a screen with no such
            // feed (e.g. `witr` before a scan) would otherwise stay blank
            // forever after its dirty flag was consumed at `(0, 0)`.
            let last_viewport = self.state.plugin_last_render_viewport.get(*screen_id).copied();
            let viewport_changed = last_viewport != Some((width, height));

            // Kick the next render when something has actually changed
            // since the last paint — a keystroke landed (`send_key`), a
            // snapshot event arrived (host or plugin `publish_snapshot`),
            // the screen has never painted yet (registration seeds the
            // flag to `true`), or the allocated viewport changed.
            //
            // The dirty gate turns the loop from a fixed-cadence render
            // storm (~4/s at the 250 ms tick) into an event-driven
            // repaint: before it, the per-tick kick compounded with the
            // `event::poll` idle wait to add ~250-300 ms of perceived lag
            // per keystroke. `take_render_dirty` swaps the flag to false,
            // so evaluate the viewport-change escape hatch first to avoid
            // short-circuiting past it.
            let dirty = handle.take_render_dirty(&pid);
            if !dirty && !viewport_changed {
                continue;
            }

            self.state
                .plugin_last_render_viewport
                .insert((*screen_id).to_string(), (width, height));

            let viewport = ainb_plugin_runtime::Viewport { width, height };
            // Returned oneshot is intentionally dropped — the cache
            // pickup happens via `try_recv_render` next tick.
            let _ = handle.render(&pid, viewport, 0);
        }
        drained
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
                self.state.plugin_runtime = Some(handle.clone());

                // Surface each loaded plugin's `[[config]]` schema in the
                // Settings ▸ Plugins category. `from_app_config` built the
                // config screen before discovery ran (the handle is `None` at
                // `AppState` construction), so we backfill the per-plugin rows
                // here now that the manifests are known. Defaults resolve from
                // the persisted `[plugins.<name>]` table first, else the
                // schema default. Idempotent — only the plugin rows are
                // rebuilt; the static enable/disable rows are kept.
                let manifests: Vec<ainb_plugin_protocol::manifest::Manifest> =
                    handle.registered_plugins().iter().map(|p| p.manifest.clone()).collect();
                self.state
                    .config_screen_state
                    .apply_plugin_manifests(&manifests, &self.state.app_config.plugins);

                // A fresh runtime means a fresh snapshot store whose
                // version counter restarts at 0 — drop any version
                // watermark from a previous runtime so an equal-valued
                // version can't mask a new close request. Init runs once
                // today; this keeps any future runtime-restart path safe.
                self.state.last_panel_close_version = None;
                // Keep the burndown usage snapshot live: watch provider
                // session dirs and nudge session-reader to rescan on
                // change, so "today" appears without the user pressing
                // `r`. Best-effort — `None` when no dir is watchable.
                self.usage_dir_watcher =
                    crate::models::usage_dir_watcher::UsageDirWatcher::start(handle);
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

        // Check for completed background drift scan
        // (skill-manager v1.2 bead v12.E.4).
        if self.state.check_drift_load_complete() {
            self.state.ui_needs_refresh = true;
        }
        // Check for a completed base-branch refresh (Configure picker)
        if self.state.check_branch_refresh_complete() {
            self.state.ui_needs_refresh = true;
        }

        // Drain + lazily refresh the MCP pool overlay (no-op when closed).
        self.state.check_mcp_overlay();
        // Drain completed daemons overlay fetch (no-op when closed).
        self.state.check_daemons_overlay();
        // Re-ensure the Headroom proxy if a Headroom session is live but the
        // proxy died (throttled, async, best-effort).
        self.state.headroom_watchdog();

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

#[cfg(test)]
mod plugin_render_gate_tests {
    //! Visibility gate on the render-tick loop: only the plugin owning
    //! `state.current_screen` gets render kicks. A hidden plugin's dirty
    //! flag must SURVIVE the gate (it is consumed by `take_render_dirty`
    //! only after the screen check) so the deferred first paint happens
    //! on the first tick after the user opens the screen.

    use std::path::PathBuf;

    use ainb_plugin_protocol::manifest::{
        Capabilities, Lifecycle, Manifest, PluginMeta, Provides, SpawnMode, Subscribes,
    };
    use ainb_plugin_runtime::{PluginId, RegisteredPlugin, Runtime};

    use super::App;
    use crate::app::screens::ids;

    fn lazy_manifest(name: &str) -> Manifest {
        Manifest {
            plugin: PluginMeta {
                name: name.into(),
                version: "0.1.0".into(),
                abi_version: 2,
                description: String::new(),
            },
            capabilities: Capabilities::default(),
            provides: Provides::default(),
            subscribes: Subscribes::default(),
            lifecycle: Lifecycle {
                spawn: SpawnMode::Lazy,
                idle_reap_secs: 600,
            },
            config: Vec::new(),
        }
    }

    /// Real runtime + an `App` wired to it, with the named lazy plugins
    /// registered. The binary path is deliberately nonexistent — these
    /// tests assert host-side kick bookkeeping only; an actual spawn
    /// attempt would fail harmlessly on the runtime's executor.
    fn app_with_plugins(names: &[&str]) -> (Runtime, App) {
        let (runtime, handle) = Runtime::new().expect("runtime constructs without plugins");
        for name in names {
            runtime.register(RegisteredPlugin::new(
                lazy_manifest(name),
                PathBuf::from("/nonexistent/plugin-binary"),
                PathBuf::from("/nonexistent/manifest.toml"),
            ));
        }
        let mut app = App::new();
        app.state.plugin_runtime = Some(handle);
        (runtime, app)
    }

    #[test]
    fn hidden_screen_gets_no_render_kick_and_stays_dirty() {
        let (runtime, mut app) = app_with_plugins(&["learnings"]);
        let handle = app.state.plugin_runtime.clone().expect("handle wired");
        let pid = PluginId::from("learnings");

        app.state.current_screen = ids::SESSION_LIST.to_string();
        app.tick_plugin_renders();
        app.tick_plugin_renders();

        // No kick: `plugin_last_render_viewport` is only written when a
        // render is dispatched.
        assert!(
            !app.state.plugin_last_render_viewport.contains_key(ids::LEARNINGS),
            "hidden screen must not receive a render kick"
        );
        // The registration-seeded dirty flag survived both ticks, so the
        // deferred first paint still happens when the screen opens.
        assert!(
            handle.take_render_dirty(&pid),
            "hidden plugin's dirty flag must survive the gated ticks"
        );

        runtime.shutdown();
    }

    #[test]
    fn dirty_plugin_kicks_on_first_tick_after_screen_switch() {
        let (runtime, mut app) = app_with_plugins(&["learnings"]);
        let handle = app.state.plugin_runtime.clone().expect("handle wired");
        let pid = PluginId::from("learnings");

        // Ticks while hidden: gated, dirty preserved (proved above).
        app.state.current_screen = ids::SESSION_LIST.to_string();
        app.tick_plugin_renders();

        // User opens the learnings screen → first tick kicks the render.
        app.state.current_screen = ids::LEARNINGS.to_string();
        app.tick_plugin_renders();

        assert_eq!(
            app.state.plugin_last_render_viewport.get(ids::LEARNINGS),
            Some(&(0, 0)),
            "first tick after the switch must kick a render at the seed viewport"
        );
        assert!(
            !handle.take_render_dirty(&pid),
            "the kick must consume the dirty flag"
        );

        runtime.shutdown();
    }

    #[test]
    fn only_the_focused_plugin_screen_is_kicked() {
        let (runtime, mut app) = app_with_plugins(&["learnings", "burndown"]);
        let handle = app.state.plugin_runtime.clone().expect("handle wired");

        app.state.current_screen = ids::LEARNINGS.to_string();
        app.tick_plugin_renders();

        assert!(
            app.state.plugin_last_render_viewport.contains_key(ids::LEARNINGS),
            "focused plugin screen must be kicked"
        );
        assert!(
            !app.state.plugin_last_render_viewport.contains_key(ids::ANALYTICS),
            "unfocused plugin screen must not be kicked"
        );
        assert!(
            handle.take_render_dirty(&PluginId::from("burndown")),
            "unfocused plugin must stay dirty for its deferred first paint"
        );

        runtime.shutdown();
    }
}

#[cfg(test)]
mod panel_close_tests {
    use super::panel_close_matches;
    use crate::app::screens::ids;

    /// Plugin id owning the analytics screen (see `PLUGIN_SCREENS`).
    const BURNDOWN: &str = "burndown";

    fn payload(screen: &str) -> Vec<u8> {
        serde_json::to_vec(&ainb_plugin_runtime::topics::UiCloseRequest {
            screen_id: screen.to_string(),
        })
        .unwrap()
    }

    #[test]
    fn matches_when_owning_plugin_names_focused_screen() {
        assert!(panel_close_matches(
            ids::ANALYTICS,
            &payload(ids::ANALYTICS),
            BURNDOWN
        ));
    }

    #[test]
    fn rejects_request_for_a_different_screen() {
        // A stale close request for analytics must not close the witr
        // screen the user has since navigated to.
        assert!(!panel_close_matches(
            ids::WITR,
            &payload(ids::ANALYTICS),
            "witr"
        ));
    }

    #[test]
    fn rejects_when_focused_screen_is_not_plugin_owned() {
        // A plugin can't close the session list (or any host screen)
        // out from under the user, even if it names it.
        assert!(!panel_close_matches(
            ids::SESSION_LIST,
            &payload(ids::SESSION_LIST),
            BURNDOWN
        ));
    }

    #[test]
    fn rejects_publish_from_plugin_that_does_not_own_screen() {
        // The payload alone is forgeable — any plugin can serialize
        // {"screen_id":"analytics"}. The publisher stamp (taken from
        // the wire connection, not the payload) is not: a publish from
        // session-reader naming burndown's screen must be ignored.
        assert!(!panel_close_matches(
            ids::ANALYTICS,
            &payload(ids::ANALYTICS),
            "session-reader"
        ));
        // The reserved host id doesn't own plugin screens either.
        assert!(!panel_close_matches(
            ids::ANALYTICS,
            &payload(ids::ANALYTICS),
            "host"
        ));
    }

    #[test]
    fn rejects_malformed_payload() {
        assert!(!panel_close_matches(ids::ANALYTICS, b"not-json", BURNDOWN));
    }
}
