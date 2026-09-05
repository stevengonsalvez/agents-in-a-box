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
use crate::config::screen_model::{self, ConfigTreeNode};
use crate::config::{AppConfig, SessionLabelStore, registry};
use crate::credentials;
use crate::docker::LogStreamingCoordinator;
// Phase 6 (new-session redesign): ParsedRepo / RemoteBranch / legacy
// `RepoSource` import retired with the legacy remote-clone flow.
use crate::models::{Session, SessionAgentType, Workspace, is_default_model};
use std::collections::{BTreeSet, HashMap, HashSet};
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

/// Actions available from a session row's context menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionContextAction {
    Attach,
    Restart,
    EditLabel,
    OpenEditor,
    OpenShell,
    OpenGit,
    QuickCommit,
    Delete,
}

/// Ephemeral state for the keyboard-accessible right-click context menu.
#[derive(Debug, Clone, Copy)]
pub struct SessionContextMenu {
    pub target: AttachableRef,
    pub selected: usize,
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
const OBSERVER_SETTLE_DELAY: Duration = Duration::from_millis(250);
const OBSERVER_RETRY_DELAY: Duration = Duration::from_secs(2);

impl AppState {
    /// Enter interactive mode by replacing the selected read-only tmux client
    /// with a writable client feeding the same terminal parser path.
    pub fn enter_interactive_pane(&mut self, rows: u16, cols: u16) -> bool {
        let attached_elsewhere = self.selected_session_attached_elsewhere();
        self.observer_pending = None;
        self.observer_failed_target = None;
        if self.embed.is_some() {
            if self.selected_tmux_name() == self.embed_session && self.is_interactive_pane() {
                return true;
            }
            self.release_interactive_pane();
        }
        let Some(name) = self.selected_tmux_name() else {
            self.add_warning_notification("No tmux session on this row".to_string());
            return false;
        };
        // tmux mirrors a session to every attached client, but all clients
        // fight over its size — attaching alongside an existing client is the
        // user's call, so allow it and warn (never block).
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

    /// Keep one read-only tmux client on the selected terminal. The observer
    /// consumes the same PTY byte stream as interactive attach, but input stays
    /// host-owned until [`Self::is_interactive_pane`] becomes true.
    ///
    /// Returns true when the observed target changes.
    pub fn sync_terminal_observer(&mut self, rows: u16, cols: u16) -> bool {
        let target = self.selected_tmux_name();
        let Some(name) = target else {
            self.observer_pending = None;
            self.observer_failed_target = None;
            self.release_interactive_pane();
            return false;
        };
        let now = Instant::now();
        if self.observer_failed_target.as_ref().is_some_and(|(failed, _)| failed != &name) {
            self.observer_failed_target = None;
        }
        if self.embed_session.as_deref() == Some(name.as_str()) && self.embed.is_some() {
            self.observer_pending = None;
            return false;
        }
        if let Some((failed, retry_at)) = &self.observer_failed_target {
            if failed == &name && now < *retry_at {
                return false;
            }
            self.observer_failed_target = None;
        }
        if !self.observer_target_settled(&name, now) {
            self.release_interactive_pane();
            return false;
        }

        self.release_interactive_pane();
        match crate::tmux::EmbedClient::observe(&name, rows, cols) {
            Ok(client) => {
                self.embed = Some(client);
                self.embed_session = Some(name);
                true
            }
            Err(e) => {
                tracing::debug!("failed to observe terminal {name}: {e}");
                self.observer_failed_target = Some((name, now + OBSERVER_RETRY_DELAY));
                false
            }
        }
    }

    fn observer_target_settled(&mut self, target: &str, now: Instant) -> bool {
        match self.observer_pending.as_ref() {
            Some((pending, ready_at)) if pending == target && now >= *ready_at => {
                self.observer_pending = None;
                true
            }
            Some((pending, _)) if pending == target => false,
            _ => {
                self.observer_pending = Some((target.to_string(), now + OBSERVER_SETTLE_DELAY));
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

    /// Release the ephemeral client. Read-only preview reconnects next loop.
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

    /// True when the selected terminal has a read-only observer client.
    pub fn is_observing_selected_terminal(&self) -> bool {
        self.selected_tmux_name()
            .is_some_and(|name| self.is_observing_tmux_session(&name))
    }

    fn is_observing_tmux_session(&self, session: &str) -> bool {
        !self.is_interactive_pane()
            && self.embed.is_some()
            && self.embed_session.as_deref() == Some(session)
    }

    /// If the observer has ended or become invisible, stop it. Keys can never
    /// be forwarded to an invisible PTY.
    ///
    /// Returns true when it released (the layout changed → repaint needed).
    pub fn poll_embed_exit(&mut self) -> bool {
        if self.embed.is_none() {
            return false;
        }
        let exited = self.embed.as_ref().is_some_and(|e| e.has_exited());
        let invisible = self.current_screen != screen_ids::SESSION_LIST;
        let interactive = self.is_interactive_pane();
        let session = self.embed_session.clone();
        if exited || invisible {
            self.release_interactive_pane();
            if exited && interactive {
                self.add_info_notification("Live session ended, released".to_string());
            } else if exited {
                if let Some(session) = session {
                    self.observer_failed_target =
                        Some((session, Instant::now() + OBSERVER_RETRY_DELAY));
                }
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
    filter_toggle_area: Option<Rect>,
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
            filter_toggle_area: None,
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

    pub fn set_filter_toggle_area(&mut self, area: Rect) {
        self.filter_toggle_area = Some(area);
    }

    pub fn is_on_filter_toggle(&self, x: u16, y: u16) -> bool {
        self.filter_toggle_area.is_some_and(|area| {
            x >= area.x
                && x < area.x.saturating_add(area.width)
                && y >= area.y
                && y < area.y.saturating_add(area.height)
        })
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
    BulkDeleteSessions(Vec<Uuid>), // Delete every multi-selected session (removes worktrees)
    BulkStopSessions(Vec<Uuid>), // Soft-stop every multi-selected session (preserves worktrees)
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

/// Assemble the shared Stop / Delete / Cancel dialog.
///
/// One builder for the single-row and the bulk path, so a future safety change
/// (a new warning, a different default) lands on both at once. Stop is always
/// `selected_index: 0`: the safe option is the one an accidental Enter picks.
fn stop_or_delete_dialog(
    title: String,
    message: String,
    warning: Option<String>,
    stop: (&str, ConfirmAction),
    delete: (&str, ConfirmAction),
) -> ConfirmationDialog {
    let (stop_label, stop_action) = stop;
    let (delete_label, delete_action) = delete;
    ConfirmationDialog {
        title,
        message,
        // confirm_action mirrors the default (Stop) so the legacy binary
        // ConfirmationConfirm handler still does the safe thing if it ever runs
        // without options.
        confirm_action: stop_action.clone(),
        selected_option: true,
        warning,
        options: Some(vec![
            DialogOption {
                label: stop_label.to_string(),
                action: stop_action,
            },
            DialogOption {
                label: delete_label.to_string(),
                action: delete_action,
            },
            DialogOption {
                label: "Cancel".to_string(),
                action: ConfirmAction::Cancel,
            },
        ]),
        selected_index: 0, // Default = Stop (safe option)
    }
}

/// `true` for the sessions Stop actually applies to: interactive agent sessions,
/// where killing tmux leaves a worktree that resumes later. Boss (Docker) and
/// Shell sessions have no soft-stop, so they only ever get a delete
/// confirmation.
pub(crate) const fn is_stoppable_interactive(session: &crate::models::session::Session) -> bool {
    use crate::models::{SessionAgentType, SessionMode};
    matches!(session.mode, SessionMode::Interactive)
        && matches!(
            session.agent_type,
            SessionAgentType::Claude
                | SessionAgentType::Codex
                | SessionAgentType::Gemini
                | SessionAgentType::Copilot
        )
}

/// What one selection has to lose, as the bulk dialog reports it.
#[derive(Debug, Clone)]
pub(crate) struct BulkWorktreeStatus {
    /// `(session name, uncommitted file count)` per dirty tree.
    pub dirty: Vec<(String, usize)>,
    /// Trees that are there but could not be read. Never folded into "clean".
    pub unchecked: usize,
    /// Distinct trees on disk, which is what a delete actually removes.
    pub with_worktree: usize,
    /// False when nothing could be resolved, so `with_worktree` is a guess and
    /// the dialog must not print it as a fact.
    pub worktree_count_known: bool,
}

impl Default for BulkWorktreeStatus {
    fn default() -> Self {
        Self {
            dirty: Vec::new(),
            unchecked: 0,
            with_worktree: 0,
            worktree_count_known: true,
        }
    }
}

/// One tree's uncommitted count, with the reason logged when it cannot be read:
/// "could not check N session(s)" is undiagnosable otherwise.
fn probe_tree(path: &std::path::Path) -> Result<usize, ()> {
    crate::git::WorktreeManager::uncommitted_file_count_at(path).map_err(|e| {
        warn!(
            "Could not check {} for uncommitted work: {}",
            path.display(),
            e
        );
    })
}

/// Shown when a bulk key is pressed with nothing checked.
pub(crate) const NOTHING_SELECTED_WARNING: &str =
    "No sessions selected. Use Space to select sessions first.";

/// Render at most three items, then "and N more". Shared by the bulk dialog's
/// message and its warning so the two cannot truncate at different lengths or
/// word it differently.
fn truncate_list(items: impl Iterator<Item = String>) -> String {
    const MAX_NAMED: usize = 3;
    let items: Vec<String> = items.collect();
    let shown = items.len().min(MAX_NAMED);
    let listed = items[..shown].join(", ");
    if items.len() > shown {
        format!("{listed}, and {} more", items.len() - shown)
    } else {
        listed
    }
}

/// Label for a selected id that no longer resolves to a session. The id is kept
/// in the bulk action (so nothing silently drops out of a delete) but a full
/// 36-character uuid would eat three rows of the dialog on its own.
fn unknown_session_label(id: Uuid) -> String {
    let id = id.to_string();
    format!("unknown ({})", &id[..8])
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

/// A section of the settings screen.
///
/// Every [`ConfigRow`](crate::config::ConfigRow) files under one of these, so
/// the list has to cover the whole TOML schema, not just the sections the
/// hand-written rows below happen to reach. The screen renders the subset that
/// actually has rows today; `CONFIG_REGISTRY` is the source of truth for the
/// rest, and wiring it in is what removes that gap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConfigCategory {
    Authentication,
    Workspace,
    Docker,
    AgentDefaults,
    Editor,
    Plugins,
    McpPool,
    Appearance,
    General,
    ContainerTemplates,
    McpServers,
    Fleet,
    Usage,
    Skills,
    SessionReader,
    Presets,
    Daemons,
    Web,
    Acp,
    HangarDaemon,
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
            ConfigCategory::Appearance,
            ConfigCategory::General,
            ConfigCategory::ContainerTemplates,
            ConfigCategory::McpServers,
            ConfigCategory::Fleet,
            ConfigCategory::Usage,
            ConfigCategory::Skills,
            ConfigCategory::SessionReader,
            ConfigCategory::Presets,
            ConfigCategory::Daemons,
            ConfigCategory::Web,
            ConfigCategory::Acp,
            ConfigCategory::HangarDaemon,
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
            ConfigCategory::Appearance => "Appearance",
            ConfigCategory::General => "General",
            ConfigCategory::ContainerTemplates => "Container Templates",
            ConfigCategory::McpServers => "MCP Servers",
            ConfigCategory::Fleet => "Fleet",
            ConfigCategory::Usage => "Usage",
            ConfigCategory::Skills => "Skills",
            ConfigCategory::SessionReader => "Session Reader",
            ConfigCategory::Presets => "Presets",
            ConfigCategory::Daemons => "Daemons",
            ConfigCategory::Web => "Web Dashboard",
            ConfigCategory::Acp => "ACP Adapters",
            ConfigCategory::HangarDaemon => "Hangar Daemon",
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
            ConfigCategory::Appearance => "🎨",
            ConfigCategory::General => "⚙️",
            ConfigCategory::ContainerTemplates => "📦",
            ConfigCategory::McpServers => "🛰️",
            ConfigCategory::Fleet => "🚁",
            ConfigCategory::Usage => "💰",
            ConfigCategory::Skills => "🎓",
            ConfigCategory::SessionReader => "📖",
            ConfigCategory::Presets => "🗂️",
            ConfigCategory::Daemons => "🛎️",
            ConfigCategory::Web => "🌐",
            ConfigCategory::Acp => "🔗",
            ConfigCategory::HangarDaemon => "🏗️",
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
            ConfigCategory::Appearance => "Theme, colors, status indicators",
            ConfigCategory::General => "Default template, presets file",
            ConfigCategory::ContainerTemplates => "Per-template image, resources, mounts",
            ConfigCategory::McpServers => "Per-server install and launch definitions",
            ConfigCategory::Fleet => "Cost caps, interview surface, phone bridge",
            ConfigCategory::Usage => "Plan, currency, model aliases",
            ConfigCategory::Skills => "Catalog release, API key",
            ConfigCategory::SessionReader => "Incremental scan window",
            ConfigCategory::Presets => "Where presets.toml lives",
            ConfigCategory::Daemons => "Staleness windows, notification debounce, approvals",
            ConfigCategory::Web => "`ainb web` bind address and read-only mode",
            ConfigCategory::Acp => "Per-adapter command and pinned permission mode",
            ConfigCategory::HangarDaemon => "Auto-standup and lockdown (stored in the daemon DB)",
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

/// A credential row: the *reference* config.toml stores, plus whether that
/// reference currently resolves to a non-empty secret.
///
/// The screen never renders the plaintext of a credential, and never renders a
/// literal's characters either — only a status and the source it came from. The
/// resolved value is deliberately not kept: nothing on this screen needs it, and
/// not holding it is the cheapest way to guarantee it cannot be painted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SecretValue {
    /// Exactly what config.toml holds: empty, a literal, `$ENV_VAR`, or
    /// `keychain:<service>`.
    pub reference: String,
    /// Whether `reference` resolved when the row was built. Resolving a
    /// `keychain:` reference shells out to `/usr/bin/security`, so this is
    /// evaluated once at build time and never per frame.
    pub resolved: bool,
}

impl SecretValue {
    /// True when the reference points somewhere else (env var / keychain)
    /// rather than being the secret itself.
    #[must_use]
    pub fn is_reference(&self) -> bool {
        self.reference.starts_with('$') || self.reference.starts_with("keychain:")
    }

    /// `unset` / `resolved (source)` / `unresolved (source)` / `literal …`.
    #[must_use]
    pub fn status_line(&self) -> String {
        if self.reference.trim().is_empty() {
            return "unset".to_string();
        }
        if self.is_reference() {
            let status = if self.resolved {
                "resolved"
            } else {
                "unresolved"
            };
            return format!("{status}  ({})", self.reference);
        }
        // A literal already in the user's config keeps working; we say so
        // rather than rewriting it behind their back.
        "literal  (in config.toml)".to_string()
    }
}

#[derive(Debug, Clone)]
pub enum ConfigValue {
    Text(String),
    /// A credential. Rendered as status + source, never as the value.
    Secret(SecretValue),
    Bool(bool),
    Choice(Vec<String>, usize), // Options and selected index
    Number(i64),
}

impl ConfigValue {
    pub fn display(&self) -> String {
        match self {
            ConfigValue::Text(s) => s.clone(),
            ConfigValue::Secret(secret) => secret.status_line(),
            ConfigValue::Bool(b) => if *b { "✓ Enabled" } else { "✗ Disabled" }.to_string(),
            ConfigValue::Choice(options, idx) => options.get(*idx).cloned().unwrap_or_default(),
            ConfigValue::Number(n) => n.to_string(),
        }
    }

    /// The raw string this widget would persist: the inverse of
    /// [`ConfigRow::to_value`](crate::config::ConfigRow::to_value), and what
    /// [`registry::set_validated`](crate::config::registry::set_validated)
    /// parses back into a typed TOML value.
    pub fn raw(&self) -> String {
        match self {
            ConfigValue::Text(s) => s.clone(),
            ConfigValue::Secret(secret) => secret.reference.clone(),
            ConfigValue::Bool(b) => b.to_string(),
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
        ConfigValue::Text(s) => toml::Value::String(s.clone()),
        ConfigValue::Secret(secret) => toml::Value::String(secret.reference.clone()),
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

/// What one pass of [`ConfigScreenState::apply_to_app_config`] produced.
#[derive(Debug, Clone, Default)]
pub struct AppliedEdits {
    /// Edits whose section `AppConfig` does not model, for
    /// [`AppConfig::save_external_keys`](crate::config::AppConfig::save_external_keys).
    pub external: Vec<(String, String)>,
    /// `(key, why)` for edits the registry refused. Reported to the user and
    /// then dropped, so a value that can never be written cannot wedge every
    /// subsequent save.
    pub rejected: Vec<(String, String)>,
    /// `(daemon_config key, raw value)` for the `hangar_daemon.*` rows, whose
    /// backend is the Hangar SQLite `daemon_config` table rather than
    /// config.toml. Dispatched through
    /// `pending_daemon_config_edits`,
    /// because that store is async and this pass is not.
    pub daemon: Vec<(String, String)>,
}

/// The settings screen: a section tree on the left, the rows of the selected
/// section on the right.
///
/// Every row comes from [`CONFIG_REGISTRY`](crate::config::CONFIG_REGISTRY),
/// seeded from the loaded config, so "a TOML key exists" and "a menu row
/// exists" are the same statement. The screen used to carry its own list of
/// ~24 hand-written rows with a hand-written persist branch per row; ten of
/// those rows accepted an edit and dropped it, and every new schema field
/// needed both lists touched to become reachable. Now the registry is the only
/// list, and a save routes every edit through
/// [`registry::set_validated`](crate::config::registry::set_validated).
#[derive(Debug, Clone)]
pub struct ConfigScreenState {
    /// Index into [`visible_nodes`](Self::visible_nodes) — the tree row the
    /// left pane has selected.
    pub selected_node: usize,
    /// Index into [`visible_rows`](Self::visible_rows).
    pub selected_setting: usize,
    /// Categories that have at least one row, in [`ConfigCategory::all`] order.
    pub categories: Vec<ConfigCategory>,
    /// Rows per category, in registry order. Plugin rows are appended to
    /// [`ConfigCategory::Plugins`] by [`Self::apply_plugin_manifests`].
    pub settings: HashMap<ConfigCategory, Vec<ConfigSetting>>,
    /// The whole tree in pre-order: category roots plus a node per TOML
    /// sub-table under them.
    pub tree: Vec<ConfigTreeNode>,
    /// Indices into [`tree`](Self::tree) that are currently on screen, i.e.
    /// with every collapsed subtree skipped.
    pub visible_nodes: Vec<usize>,
    /// `ConfigTreeNode::id`s of the expanded nodes, persisted in
    /// `ui_preferences.config_tree_expanded`.
    pub expanded: BTreeSet<String>,
    /// `(category, row index)` of every row the right pane shows: the selected
    /// node's subtree, or the `/` filter's matches.
    pub visible_rows: Vec<(ConfigCategory, usize)>,
    /// The active `/` filter. `Some("")` is an open, empty filter box.
    pub search: Option<String>,
    /// Keys the user has actually edited this session, and therefore the only
    /// keys a save writes.
    ///
    /// Deliberately not "every row that differs from the file": a row for an
    /// absent optional leaf seeds as an empty/false widget, so a diff against
    /// the file would materialise `require_mention_in_groups = false` into a
    /// `[fleet.bridge.telegram]` section the user never created. Tracking the
    /// edits themselves cannot invent a value.
    pub dirty: BTreeSet<String>,
    /// Whether the tree expansion changed since the screen opened.
    ///
    /// Expanding a section is a navigation keystroke; writing config.toml on
    /// each one puts a read-parse-serialize-write in the event loop and gives a
    /// user who is just looking around a modified file. The write happens once,
    /// on the way out.
    pub expansion_dirty: bool,
    /// The secret row a Ctrl+K prompt is collecting a literal for.
    ///
    /// Set for exactly one popup round-trip: the popup's confirm writes the
    /// literal to the OS keychain and stores only `keychain:<service>` in the
    /// row, so the plaintext never reaches `config.toml` or the row's value.
    pub keychain_target: Option<String>,
    pub editing: bool,
    pub edit_buffer: String,
    /// True when entering API key (special handling - saves to keychain)
    pub api_key_input_mode: bool,
    /// Which pane currently has focus (Categories or Settings)
    pub focused_pane: ConfigPane,
}

impl Default for ConfigScreenState {
    fn default() -> Self {
        Self::from_app_config(&AppConfig::default())
    }
}

impl ConfigScreenState {
    pub fn new() -> Self {
        Self::default()
    }

    /// The registry row key whose edit opens the auth-provider popup instead of
    /// the generic choice popup.
    ///
    /// Choosing "API key" there also prompts for the key and stores it in the
    /// OS keychain, which a plain choice widget cannot do. Matched by KEY, not
    /// by pane index — the old index match broke the moment the row order
    /// changed.
    pub const CLAUDE_PROVIDER_KEY: &'static str = "authentication.claude_provider";

    /// Build the screen from a loaded config.
    ///
    /// The seed is the serialized `config` plus the top-level sections
    /// `AppConfig` does not model (`[skills]`, `[session_reader]`), read off the
    /// user config file so their rows show what is actually on disk rather than
    /// blank. A missing or unparseable file just means those rows seed empty.
    pub fn from_app_config(config: &AppConfig) -> Self {
        let seed = seed_value(config);
        let settings = screen_model::build_rows(&seed);
        let categories: Vec<ConfigCategory> = ConfigCategory::all()
            .into_iter()
            .filter(|category| settings.get(category).is_some_and(|rows| !rows.is_empty()))
            .collect();
        let tree = screen_model::build_tree(&categories, &settings);
        let expanded: BTreeSet<String> =
            config.ui_preferences.config_tree_expanded.iter().cloned().collect();

        let mut state = Self {
            selected_node: 0,
            selected_setting: 0,
            categories,
            settings,
            tree,
            visible_nodes: Vec::new(),
            expanded,
            visible_rows: Vec::new(),
            search: None,
            dirty: BTreeSet::new(),
            expansion_dirty: false,
            keychain_target: None,
            editing: false,
            edit_buffer: String::new(),
            api_key_input_mode: false,
            focused_pane: ConfigPane::Categories,
        };
        state.refresh();
        state
    }

    // --- derived view state -------------------------------------------------

    /// Recompute the two flattened lists the panes render. Cheap (a walk over
    /// ~40 tree nodes and ~250 rows) and called only when something that
    /// changes them changes, never per frame.
    pub fn refresh(&mut self) {
        self.refresh_visible_nodes();
        self.refresh_visible_rows();
    }

    fn refresh_visible_nodes(&mut self) {
        let mut visible = Vec::new();
        let mut skip_deeper_than: Option<usize> = None;
        for (index, node) in self.tree.iter().enumerate() {
            if let Some(depth) = skip_deeper_than {
                if node.depth > depth {
                    continue;
                }
                skip_deeper_than = None;
            }
            visible.push(index);
            if node.has_children && !self.expanded.contains(&node.id()) {
                skip_deeper_than = Some(node.depth);
            }
        }
        self.visible_nodes = visible;
        if self.selected_node >= self.visible_nodes.len() {
            self.selected_node = self.visible_nodes.len().saturating_sub(1);
        }
    }

    fn refresh_visible_rows(&mut self) {
        self.visible_rows = match self.search.as_deref() {
            Some(query) if !query.trim().is_empty() => self.search_matches(query.trim()),
            _ => self
                .current_node()
                .and_then(|node| {
                    let category = node.category;
                    let rows = node.rows.clone();
                    self.settings
                        .get(&category)
                        .map(|_| rows.into_iter().map(|index| (category, index)).collect())
                })
                .unwrap_or_default(),
        };
        if self.selected_setting >= self.visible_rows.len() {
            self.selected_setting = self.visible_rows.len().saturating_sub(1);
        }
    }

    /// Rows matching the `/` filter, across every category.
    ///
    /// Ranked so a row whose key or label CONTAINS the query comes before one
    /// that merely has it as a subsequence: typing `theme` should land on
    /// `ui_preferences.theme`, not on the first row whose help text happens to
    /// spell t-h-e-m-e.
    fn search_matches(&self, query: &str) -> Vec<(ConfigCategory, usize)> {
        let lowered = query.to_lowercase();
        let mut exact = Vec::new();
        let mut loose = Vec::new();
        for category in &self.categories {
            let Some(rows) = self.settings.get(category) else {
                continue;
            };
            for (index, row) in rows.iter().enumerate() {
                let key = row.key.to_lowercase();
                let label = row.label.to_lowercase();
                if key.contains(&lowered) || label.contains(&lowered) {
                    exact.push((*category, index));
                } else if screen_model::fuzzy_matches(&row.key, query)
                    || screen_model::fuzzy_matches(&row.label, query)
                    || screen_model::fuzzy_matches(&row.description, query)
                {
                    loose.push((*category, index));
                }
            }
        }
        exact.extend(loose);
        exact
    }

    /// The tree node the left pane has selected.
    #[must_use]
    pub fn current_node(&self) -> Option<&ConfigTreeNode> {
        self.visible_nodes
            .get(self.selected_node)
            .and_then(|index| self.tree.get(*index))
    }

    /// The category the right pane's title names.
    #[must_use]
    pub fn current_category(&self) -> Option<ConfigCategory> {
        if self.is_searching() {
            return None;
        }
        self.current_node().map(|node| node.category)
    }

    /// The rows the right pane shows, in display order.
    #[must_use]
    pub fn current_settings(&self) -> Vec<&ConfigSetting> {
        self.visible_rows
            .iter()
            .filter_map(|(category, index)| self.settings.get(category)?.get(*index))
            .collect()
    }

    #[must_use]
    pub fn current_setting(&self) -> Option<&ConfigSetting> {
        let (category, index) = *self.visible_rows.get(self.selected_setting)?;
        self.settings.get(&category)?.get(index)
    }

    /// Why the selected row cannot be edited, or `None`.
    #[must_use]
    pub fn current_read_only_reason(&self) -> Option<&'static str> {
        screen_model::read_only_reason(&self.current_setting()?.key)
    }

    /// True while the `/` filter box is open.
    #[must_use]
    pub fn is_searching(&self) -> bool {
        self.search.is_some()
    }

    // --- navigation ---------------------------------------------------------

    pub fn select_next_category(&mut self) {
        if self.visible_nodes.is_empty() {
            return;
        }
        self.selected_node = (self.selected_node + 1) % self.visible_nodes.len();
        self.selected_setting = 0;
        self.refresh_visible_rows();
    }

    pub fn select_prev_category(&mut self) {
        if self.visible_nodes.is_empty() {
            return;
        }
        self.selected_node =
            self.selected_node.checked_sub(1).unwrap_or(self.visible_nodes.len() - 1);
        self.selected_setting = 0;
        self.refresh_visible_rows();
    }

    pub fn select_next_setting(&mut self) {
        if !self.visible_rows.is_empty() {
            self.selected_setting = (self.selected_setting + 1) % self.visible_rows.len();
        }
    }

    pub fn select_prev_setting(&mut self) {
        if !self.visible_rows.is_empty() {
            self.selected_setting =
                self.selected_setting.checked_sub(1).unwrap_or(self.visible_rows.len() - 1);
        }
    }

    /// Open or close the selected tree node.
    ///
    /// Records that the expansion changed; the write itself waits for
    /// [`take_expansion_to_persist`](Self::take_expansion_to_persist) on screen
    /// exit. Returns whether anything toggled.
    pub fn toggle_expanded(&mut self) -> bool {
        let Some(node) = self.current_node() else {
            return false;
        };
        if !node.has_children {
            return false;
        }
        let id = node.id();
        if !self.expanded.remove(&id) {
            self.expanded.insert(id);
        }
        self.expansion_dirty = true;
        self.refresh();
        true
    }

    /// The expansion ids to write, once, if any node was toggled since the last
    /// call. `None` means the file must be left alone.
    pub fn take_expansion_to_persist(&mut self) -> Option<Vec<String>> {
        if !std::mem::take(&mut self.expansion_dirty) {
            return None;
        }
        Some(self.expanded.iter().cloned().collect())
    }

    // --- search -------------------------------------------------------------

    /// Open the `/` filter box.
    pub fn start_search(&mut self) {
        self.search = Some(String::new());
        self.selected_setting = 0;
        self.focused_pane = ConfigPane::Settings;
        self.refresh_visible_rows();
    }

    pub fn push_search_char(&mut self, c: char) {
        if let Some(query) = self.search.as_mut() {
            query.push(c);
            self.selected_setting = 0;
            self.refresh_visible_rows();
        }
    }

    pub fn pop_search_char(&mut self) {
        if let Some(query) = self.search.as_mut() {
            query.pop();
            self.selected_setting = 0;
            self.refresh_visible_rows();
        }
    }

    /// Close the filter box and go back to the selected section.
    pub fn clear_search(&mut self) {
        self.search = None;
        self.selected_setting = 0;
        self.focused_pane = ConfigPane::Categories;
        self.refresh_visible_rows();
    }

    // --- editing ------------------------------------------------------------

    /// Overwrite a row's widget value and mark it for the next save.
    ///
    /// Keyed by the row's dotted path, which is unique across the whole screen,
    /// so a popup confirm does not have to know which category or node it came
    /// from — the filter pane shows rows from categories other than the
    /// selected one, and the old category-scoped lookup silently missed them.
    /// Reseed one row's widget from the live config, WITHOUT marking it dirty.
    ///
    /// For a value that changed outside the settings screen — the auth flow
    /// writes `authentication.claude_provider` through its own popup and
    /// keychain path, then the row has to catch up. Those call sites used to
    /// look the row up by the hand-written key `"claude_auth"`, which the
    /// registry rewrite deleted, so the `find` silently never matched and the
    /// row kept showing the old provider until restart. They also wrote a
    /// `Text` status into what the registry declares a `Choice`.
    pub fn reseed_row(&mut self, key: &str, config: &AppConfig) {
        let Some(row) = crate::config::registry::row(key) else {
            return;
        };
        let Ok(as_toml) = toml::Value::try_from(config) else {
            return;
        };
        let current = crate::config::registry::navigate_toml(&as_toml, key).ok();
        let value = row.to_value(current);
        for rows in self.settings.values_mut() {
            if let Some(existing) = rows.iter_mut().find(|r| r.key == key) {
                existing.value = value;
                // Deliberately not dirty: the value is already on disk, and
                // marking it would write it back from this snapshot.
                self.dirty.remove(key);
                return;
            }
        }
    }

    /// Overwrite the `hangar_daemon.*` rows from the daemon's stored values,
    /// WITHOUT marking them dirty.
    ///
    /// `stored` is `(daemon_config key, value)`; a key absent from it keeps the
    /// coded default the row was seeded with, which is what the daemon applies
    /// for a key with no row. Deliberately not `set_row_value`: that marks the
    /// row dirty, and the next save would write these values straight back into
    /// a database that already holds them.
    pub fn seed_hangar_daemon_rows(&mut self, stored: &[(String, String)]) {
        for (daemon_key, value) in stored {
            let key = format!("{}{daemon_key}", registry::HANGAR_DAEMON_PREFIX);
            let Some(row) = registry::row(&key) else {
                continue;
            };
            let seeded = row.to_value(Some(&registry::parse_toml_scalar(value)));
            for rows in self.settings.values_mut() {
                if let Some(existing) = rows.iter_mut().find(|r| r.key == key) {
                    existing.value = seeded;
                    self.dirty.remove(&key);
                    break;
                }
            }
        }
        self.refresh_visible_rows();
    }

    pub fn set_row_value(&mut self, key: &str, value: ConfigValue) {
        for rows in self.settings.values_mut() {
            if let Some(row) = rows.iter_mut().find(|row| row.key == key) {
                row.value = value;
                self.dirty.insert(key.to_string());
                return;
            }
        }
    }

    /// Cycle the selected row's value in place (booleans and choices only).
    pub fn toggle_current_setting(&mut self) {
        let Some(key) = self.current_setting().map(|row| row.key.clone()) else {
            return;
        };
        if screen_model::read_only_reason(&key).is_some() {
            return;
        }
        let Some(current) = self.current_setting().map(|row| row.value.clone()) else {
            return;
        };
        let next = match current {
            ConfigValue::Bool(b) => ConfigValue::Bool(!b),
            ConfigValue::Choice(options, idx) if !options.is_empty() => {
                let next = (idx + 1) % options.len();
                ConfigValue::Choice(options, next)
            }
            _ => return,
        };
        self.set_row_value(&key, next);
    }

    // --- persistence --------------------------------------------------------

    /// Every edit the user has made, as `(dotted key, raw value)`.
    ///
    /// Read-only rows are filtered out rather than trusted not to be dirty: the
    /// screen refuses those edits at the keypress, and this is the second lock
    /// on the same door.
    #[must_use]
    pub fn pending_edits(&self) -> Vec<(String, String)> {
        let mut edits = Vec::new();
        for category in &self.categories {
            let Some(rows) = self.settings.get(category) else {
                continue;
            };
            for row in rows {
                if !self.dirty.contains(&row.key) {
                    continue;
                }
                if Self::parse_plugin_row_key(&row.key).is_some()
                    || Self::parse_plugin_toggle_key(&row.key).is_some()
                    || screen_model::read_only_reason(&row.key).is_some()
                {
                    continue;
                }
                edits.push((row.key.clone(), row.value.raw()));
            }
        }
        edits
    }

    /// Fold every edit into `config`, returning the ones whose section
    /// `AppConfig` does not model so the caller can hand them to
    /// [`AppConfig::save_external_keys`].
    ///
    /// The whole persist path is: serialize `config` to TOML, write each edit
    /// through the registry's validator, deserialize back. That replaced ~200
    /// lines of per-field match arms whose only job was to name, for a second
    /// time, where each row lived — and which was missing an arm for every row
    /// that silently discarded its edit.
    pub fn apply_to_app_config(&self, config: &mut AppConfig) -> anyhow::Result<AppliedEdits> {
        let mut root = toml::Value::try_from(&*config)
            .map_err(|e| anyhow::anyhow!("config does not serialize to TOML: {e}"))?;

        let mut applied = AppliedEdits::default();
        for (key, raw) in self.pending_edits() {
            if let Some(daemon_key) = registry::hangar_daemon_key(&key) {
                // Not config.toml at all: `save_external_keys` would happily
                // write a `[hangar_daemon]` section that nothing ever reads,
                // which is the silent no-op this category exists to avoid.
                applied.daemon.push((daemon_key.to_string(), raw));
                continue;
            }
            if registry::is_external(&key) {
                // These go through the key-level writer rather than the struct.
                // `[skills]` and `[session_reader]` are parsed off this file by
                // other crates and have no `AppConfig` field to land in at all.
                // `[fleet.bridge]` does round-trip as an opaque passthrough, but
                // routing it through the struct made the edit a no-op: `save()`
                // preserves `fleet.bridge` from disk to stop a stale startup
                // snapshot clobbering a hand edit, so the value the user just
                // typed was overwritten by the old one and the screen still
                // reported "saved". Writing the single key they touched is both
                // explicit and safe.
                applied.external.push((key, raw));
                continue;
            }
            // Collected, not propagated. Failing the whole save on the first
            // bad row left that row in `dirty`, so every later auto-persist hit
            // the same error — one out-of-range number blocked every unrelated
            // edit for the rest of the session.
            if let Err(e) = registry::set_validated(&mut root, &key, &raw) {
                applied.rejected.push((key, e.to_string()));
            }
        }

        *config = root
            .try_into()
            .map_err(|e| anyhow::anyhow!("edited config does not deserialize: {e}"))?;

        // Plugin rows keep their own path: their schema lives in the plugin
        // manifest, not the registry, and `plugins.*` is an opaque leaf the
        // registry validator deliberately refuses.
        self.apply_plugin_rows(config);
        self.apply_plugin_toggle_rows(config);

        Ok(applied)
    }

    /// Forget the pending edits after they have been written, so a later save
    /// does not rewrite values another process may have changed since.
    pub fn mark_saved(&mut self) {
        self.dirty.clear();
    }

    /// Prefix that marks a [`ConfigCategory::Plugins`] row as a per-plugin
    /// `[[config]]` field (vs. a registry row). The row key is
    /// `plugin:<plugin_name>:<field_key>` — unique across plugins that share a
    /// field name, and reversible in
    /// [`apply_plugin_rows`](Self::apply_plugin_rows) so the edit lands under
    /// `plugins.values[plugin_name][field_key]`.
    const PLUGIN_ROW_PREFIX: &'static str = "plugin:";

    /// Prefix for the per-plugin enable/disable toggle rows.
    const PLUGIN_TOGGLE_PREFIX: &'static str = "plugin-enabled:";

    /// Compose the Plugins-category row key for a plugin's config field.
    fn plugin_row_key(plugin: &str, field_key: &str) -> String {
        format!("{}{plugin}:{field_key}", Self::PLUGIN_ROW_PREFIX)
    }

    /// Split a Plugins-category row key back into `(plugin_name, field_key)`,
    /// or `None` for any other row. The plugin name and the field key are
    /// joined by the *first* `:` after the prefix, so plugin names never
    /// contain `:` but field keys may.
    fn parse_plugin_row_key(key: &str) -> Option<(&str, &str)> {
        let rest = key.strip_prefix(Self::PLUGIN_ROW_PREFIX)?;
        rest.split_once(':')
    }

    /// The plugin name behind an enable/disable toggle row, or `None`.
    fn parse_plugin_toggle_key(key: &str) -> Option<&str> {
        key.strip_prefix(Self::PLUGIN_TOGGLE_PREFIX)
    }

    /// Rebuild the Plugins category from the loaded plugin manifests: one
    /// enable/disable toggle per plugin, then one row per `[[config]]` field.
    ///
    /// The toggles are the "real plugin list" that replaces the old static
    /// "Installed Plugins: None installed" placeholder — a row that reported
    /// nothing and edited nothing. They write `plugins.disabled`, so while they
    /// are shown the raw `plugins.enabled` / `plugins.disabled` list rows are
    /// dropped: two rows writing one key is how they end up disagreeing.
    ///
    /// An allowlist (`plugins.enabled` non-empty) takes precedence over the
    /// denylist in discovery, so a toggle there would be a lie. In that mode the
    /// raw list rows stay and no toggles are added.
    ///
    /// One [`ConfigSetting`] is produced per [`ConfigField`], mapping the field
    /// `kind` to the matching [`ConfigValue`] widget (`path`/`string` → `Text`,
    /// `bool` → `Bool`, `enum` → `Choice`, `int` → `Number`). The displayed
    /// value defaults from `plugins.values[plugin][key]` when present, else the
    /// schema `default`.
    ///
    /// Idempotent: re-invoking it (e.g. after the plugin runtime finishes
    /// discovery) rebuilds the plugin rows from scratch rather than duplicating
    /// them.
    pub fn apply_plugin_manifests(
        &mut self,
        manifests: &[ainb_plugin_protocol::manifest::Manifest],
        plugins_cfg: &crate::config::PluginsConfig,
    ) {
        // Toggles only replace the raw list rows when there is something to
        // toggle. With plugins disabled (`AINB_DISABLE_PLUGINS=1`) or discovery
        // not yet run, dropping the lists would leave the category empty and
        // the section would vanish from the tree entirely.
        let show_toggles = !manifests.is_empty() && plugins_cfg.enabled.is_empty();
        let dirty = self.dirty.clone();
        let rows = self.settings.entry(ConfigCategory::Plugins).or_default();

        // Discovery finishes asynchronously, so this can land between an edit
        // and its save. Anything the user has already touched keeps the value
        // they typed; everything else is rebuilt from the manifests.
        let edited: HashMap<String, ConfigValue> = rows
            .iter()
            .filter(|row| dirty.contains(&row.key))
            .map(|row| (row.key.clone(), row.value.clone()))
            .collect();

        // Drop everything this method owns so repeated calls are idempotent.
        rows.retain(|row| {
            Self::parse_plugin_row_key(&row.key).is_none()
                && Self::parse_plugin_toggle_key(&row.key).is_none()
        });
        if show_toggles {
            rows.retain(|row| {
                // `plugins.disabled` always stays. Discovery filters denied
                // plugins out entirely, so after a restart a plugin disabled
                // from this screen has no manifest and therefore no toggle row
                // — dropping the raw list too left no way to re-enable it.
                // Disabling would be a one-way door.
                row.key == "plugins.disabled"
                    || (row.key != "plugins.enabled" && row.key != "plugins.disabled")
                    || dirty.contains(&row.key)
            });
        }

        for manifest in manifests {
            let plugin = manifest.plugin.name.as_str();

            if show_toggles {
                let enabled = !plugins_cfg.disabled.iter().any(|name| name == plugin);
                rows.push(ConfigSetting {
                    key: format!("{}{plugin}", Self::PLUGIN_TOGGLE_PREFIX),
                    label: format!("Plugin: {plugin}"),
                    value: ConfigValue::Bool(enabled),
                    description: format!(
                        "{} · load {plugin} at startup",
                        manifest.plugin.description
                    ),
                });
            }

            // The resolved [plugins.<name>] value table, if the user has set
            // any keys — drives the displayed default ahead of the schema's.
            let saved = plugins_cfg.values.get(plugin).and_then(toml::Value::as_table);

            for field in &manifest.config {
                // Saved string value (config.toml only stores TOML scalars; we
                // render every kind from its string form for the widget).
                let saved_str = saved.and_then(|t| t.get(&field.key)).map(toml_scalar_to_string);
                let value = config_value_for_field(field, saved_str.as_deref());

                let key = Self::plugin_row_key(plugin, &field.key);
                rows.push(ConfigSetting {
                    label: field.label.clone(),
                    value: edited.get(&key).cloned().unwrap_or(value),
                    description: format!("{} · plugin: {}", field.label, plugin),
                    key,
                });
            }
        }

        // Same for the enable toggles.
        for row in rows.iter_mut() {
            if let Some(value) = edited.get(&row.key) {
                row.value = value.clone();
            }
        }

        // The Plugins category may have gone from empty to populated (or back),
        // which changes both panes.
        self.rebuild_tree();
    }

    /// Recompute `categories` + `tree` after the row set changed.
    fn rebuild_tree(&mut self) {
        self.categories = ConfigCategory::all()
            .into_iter()
            .filter(|category| self.settings.get(category).is_some_and(|rows| !rows.is_empty()))
            .collect();
        self.tree = screen_model::build_tree(&self.categories, &self.settings);
        self.refresh();
    }

    /// Route every Plugins-category row whose key is `plugin:<name>:<field_key>`
    /// (see [`Self::plugin_row_key`]) into `config.plugins.values[<name>]
    /// [<field_key>]` — NOT a top-level field. The serialized `[plugins.<name>]`
    /// table round-trips through the existing `AppConfig::save()` pipeline.
    fn apply_plugin_rows(&self, config: &mut AppConfig) {
        let Some(settings) = self.settings.get(&ConfigCategory::Plugins) else {
            return;
        };
        for setting in settings {
            // Only rows the user actually edited. Writing every row would
            // materialise each discovered plugin's schema defaults into
            // config.toml the first time any unrelated setting was saved,
            // pinning today's values so a later default change in the plugin's
            // manifest could never take effect. This is the same reason
            // `dirty` exists for the non-plugin rows.
            if !self.dirty.contains(&setting.key) {
                continue;
            }
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

    /// Turn the per-plugin enable toggles into `plugins.disabled`.
    ///
    /// Rebuilt from the rows rather than diffed, so re-enabling a plugin removes
    /// its name; sorted so config.toml diffs stay stable across saves. No-op
    /// when there are no toggle rows (allowlist mode, or discovery has not run).
    fn apply_plugin_toggle_rows(&self, config: &mut AppConfig) {
        let Some(settings) = self.settings.get(&ConfigCategory::Plugins) else {
            return;
        };
        let mut seen_a_toggle = false;
        let mut disabled: Vec<String> = Vec::new();
        for setting in settings {
            let Some(plugin) = Self::parse_plugin_toggle_key(&setting.key) else {
                continue;
            };
            seen_a_toggle = true;
            if matches!(setting.value, ConfigValue::Bool(false)) {
                disabled.push(plugin.to_string());
            }
        }
        if !seen_a_toggle {
            return;
        }
        // Keep names for plugins that aren't installed here: a shared config
        // that disables a plugin this machine never discovered must not have
        // that line dropped on the next save.
        for name in &config.plugins.disabled {
            let known = settings.iter().any(|row| {
                Self::parse_plugin_toggle_key(&row.key).is_some_and(|plugin| plugin == name)
            });
            if !known && !disabled.contains(name) {
                disabled.push(name.clone());
            }
        }
        disabled.sort();
        disabled.dedup();
        config.plugins.disabled = disabled;
    }
}

/// The TOML tree the settings rows read from.
///
/// `AppConfig` does not model the whole file: `[skills]` and
/// `[session_reader]` are parsed straight off it by `ainb-cli` and the
/// session-reader plugin. Their registry rows would otherwise always render
/// blank, so they are merged in from disk here. Best effort — an unreadable or
/// unparseable file just leaves those rows empty, exactly as if the sections
/// were absent.
fn seed_value(config: &AppConfig) -> toml::Value {
    let mut seed =
        toml::Value::try_from(config).unwrap_or_else(|_| toml::Value::Table(toml::Table::new()));

    let on_disk = AppConfig::get_user_config_dir()
        .ok()
        .map(|dir| dir.join("config.toml"))
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| text.parse::<toml::Table>().ok());
    if let Some(on_disk) = on_disk {
        merge_external_sections(&mut seed, &toml::Value::Table(on_disk));
    }
    seed_hangar_daemon_defaults(&mut seed);
    seed_builtin_acp_adapters(&mut seed);
    seed
}

/// Plant the built-in ACP adapters into the seed so their rows exist.
///
/// `acp.adapters.*` rows are wildcards, and `AcpConfig::adapters` defaults empty
/// with `skip_serializing_if`, so the key was absent from the seed, `expand_key`
/// returned nothing, and the whole "ACP Adapters" category was filtered out for
/// having zero rows. It could never render, and there was no way to reach the
/// adapters from the screen at all.
///
/// The built-ins live in `PoolConfig::default()`, not in config.toml, so they
/// have to be named here for the same reason the Hangar daemon knobs do: the
/// row has to exist before a user can be the first person to configure it. Only
/// planted where the user has not already declared the adapter, so a configured
/// `command` is never overwritten by a default.
fn seed_builtin_acp_adapters(seed: &mut toml::Value) {
    for name in ainb_hangar_daemon::acp_pool::PoolConfig::default().adapters.keys() {
        let base = format!("acp.adapters.{}", registry::quote_key_segment(name));
        for (field, value) in [
            ("command", toml::Value::String(String::new())),
            (
                "permission_mode",
                toml::Value::String("default".to_string()),
            ),
        ] {
            let key = format!("{base}.{field}");
            if registry::navigate_toml(seed, &key).is_err() {
                let _ = registry::insert_at(seed, &key, value);
            }
        }
    }
}

/// Plant every Hangar daemon knob's coded default under `hangar_daemon.` in the
/// seed.
///
/// Without this the rows have no value to render at all and every one of them
/// would seed as an empty widget, which claims the daemon is unconfigured. The
/// coded default is the honest placeholder: it IS what the daemon runs when a
/// key has no stored row. `load_hangar_daemon_config` replaces it with
/// the stored value shortly after startup.
/// The Hangar SQLite database, when one exists.
///
/// `Store::open_default` CREATES the database and runs migrations, which is the
/// right behaviour for the daemon and the wrong one for a settings screen
/// painting itself: opening the TUI must not conjure a hangar.db on a machine
/// that has never run the daemon. So the file is probed first.
fn hangar_db_path() -> Option<std::path::PathBuf> {
    let path = ainb_hangar_core::hangar_home()?.join("hangar.db");
    path.exists().then_some(path)
}

/// Every stored `daemon_config` value, or `None` when there is no database yet.
///
/// Only the keys the registry knows: an internal-state row (the daemon's own
/// bookkeeping) is not config and must never reach a settings row.
async fn read_daemon_config() -> anyhow::Result<Option<Vec<(String, String)>>> {
    use ainb_hangar_core::daemon_config::DAEMON_CONFIG_REGISTRY;

    if hangar_db_path().is_none() {
        return Ok(None);
    }
    // Read-only. `Store::open_default` takes a whole-database backup and
    // applies pending migrations, and this runs on the first app tick of every
    // launch — so after an upgrade it migrated a running daemon's live schema
    // out from under it, and blocked the event loop for the length of the copy.
    // The existence guard above stops creation, not migration.
    let Some(rows) = ainb_hangar_store::Store::read_daemon_config_read_only().await? else {
        return Ok(None);
    };
    let known: std::collections::HashMap<&str, String> =
        rows.into_iter()
            .fold(std::collections::HashMap::new(), |mut acc, (key, value)| {
                if let Some(descriptor) = DAEMON_CONFIG_REGISTRY.iter().find(|d| d.key == key) {
                    acc.insert(descriptor.key, value);
                }
                acc
            });
    let mut stored: Vec<(String, String)> =
        known.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    stored.sort();
    Ok(Some(stored))
}

/// Validate one `daemon_config` edit and write it.
///
/// Validation goes through the descriptor, not the core registry: the daemon's
/// own registry is the authority on what its table accepts, and it is the gate
/// the RPC handler and the CLI both use.
async fn write_daemon_config_batch(edits: &[(String, String)]) -> Vec<(String, anyhow::Error)> {
    use ainb_hangar_store::repo::daemon_config::DaemonConfigRepo;

    let mut failures = Vec::new();
    // Validate before opening anything: a batch of only-invalid values should
    // not open the store at all.
    let mut valid = Vec::with_capacity(edits.len());
    for (key, raw) in edits {
        match ainb_hangar_core::daemon_config::descriptor(key) {
            Some(descriptor) => match descriptor.validate(raw) {
                Ok(normalized) => valid.push((key.clone(), normalized)),
                Err(why) => failures.push((key.clone(), anyhow::anyhow!(why))),
            },
            None => failures.push((key.clone(), anyhow::anyhow!("not a daemon config key"))),
        }
    }
    if valid.is_empty() {
        return failures;
    }

    // ONE open for the whole batch. `open_default` runs a whole-database
    // VACUUM INTO backup plus migrations, and this runs on the UI task — doing
    // it per key meant saving three rows paid that cost three times.
    let store = match ainb_hangar_store::Store::open_default().await {
        Ok(store) => store,
        Err(error) => {
            let message = error.to_string();
            failures
                .extend(valid.into_iter().map(|(key, _)| (key, anyhow::anyhow!(message.clone()))));
            return failures;
        }
    };
    for (key, normalized) in valid {
        if let Err(error) = DaemonConfigRepo::set(store.pool(), &key, &normalized).await {
            failures.push((key, error.into()));
        }
    }
    failures
}

fn seed_hangar_daemon_defaults(seed: &mut toml::Value) {
    for descriptor in ainb_hangar_core::daemon_config::DAEMON_CONFIG_REGISTRY {
        let key = format!("{}{}", registry::HANGAR_DAEMON_PREFIX, descriptor.key);
        let _ = registry::insert_at(seed, &key, registry::parse_toml_scalar(descriptor.default));
    }
}

/// Copy each [`EXTERNAL_PREFIXES`](registry::EXTERNAL_PREFIXES) section from the
/// on-disk file into the seed, unless the seed already has it.
///
/// The prefixes are DOTTED PATHS (`"fleet.bridge."`), not top-level keys, so
/// they have to be walked rather than looked up flat: `table.get("fleet.bridge")`
/// can never match, because TOML nests. Harmless today only because
/// `fleet.bridge` happens to be a modelled passthrough that the seed already
/// carries; `skills` and `session_reader` are single-segment and worked by
/// accident.
pub(crate) fn merge_external_sections(seed: &mut toml::Value, on_disk: &toml::Value) {
    for prefix in registry::EXTERNAL_PREFIXES {
        let path = prefix.trim_end_matches('.');
        if registry::navigate_toml(seed, path).is_ok() {
            continue;
        }
        let Ok(value) = registry::navigate_toml(on_disk, path) else {
            continue;
        };
        // Best effort: an on-disk shape that will not graft is simply skipped,
        // leaving those rows blank rather than failing the whole screen.
        let _ = registry::insert_at(seed, path, value.clone());
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

/// View filter for the session tree, cycled by `Shift+F` or its clickable title chip.
///
/// Phase 2 of `load_interactive_mode_sessions` started surfacing Stopped sessions
/// (tmux-dead but worktree-alive) alongside Running ones. With many worktrees
/// the tree gets crowded; this filter lets the user hide stopped rows or focus
/// on stopped-only without losing access. Persisted in UI preferences.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
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

    pub const fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::ActiveOnly => "active",
            Self::StoppedOnly => "stopped",
        }
    }
}

/// Payload of the Configure remote-repo pre-flight: generation guard + the
/// `ls-remote` branch listing (or the error string to show on the form).
type RepoCheckPayload = (u64, Result<Vec<crate::git::RemoteBranch>, String>);

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
    /// Hangar daemon `(daemon_config key, raw value)` edits waiting to be
    /// written to the daemon's SQLite table.
    ///
    /// A queue of its own rather than an `AsyncAction`: that slot holds exactly
    /// one action and is drained once per app tick, so two settings edits
    /// confirmed inside the same 250 ms tick would silently lose the first
    /// while toasting success for both. Appended to, drained in
    /// `process_async_action`.
    pub pending_daemon_config_edits: Vec<(String, String)>,
    /// Whether the Hangar daemon's stored `daemon_config` values have been read
    /// into the settings rows yet.
    ///
    /// A one-shot of its own rather than a seeded `pending_async_action`: that
    /// slot holds ONE keystroke-driven action, so pre-filling it both races the
    /// first keystroke and makes "no action is pending" untestable.
    pub hangar_daemon_config_loaded: bool,
    // Flag to track if user cancelled during async operation
    pub async_operation_cancelled: bool,
    // Confirmation dialog state
    pub confirmation_dialog: Option<ConfirmationDialog>,
    // Shared MCP pool observability overlay (None = closed; no refresh runs).
    pub mcp_overlay: Option<McpOverlayState>,
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
    //  - Ctrl+Q releases only while interactive focus owns the terminal.
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
    // A changed selection must settle before starting a read-only client.
    observer_pending: Option<(String, Instant)>,
    // A read-only observer that dies waits before the next retry.
    observer_failed_target: Option<(String, Instant)>,
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
    /// Persistent store for durable session labels.
    pub session_label_store: SessionLabelStore,
    /// Durable-label text popup state for managed and SSH sessions.
    pub session_label_rename_mode: bool,
    pub session_label_rename_buffer: String,
    pub session_label_rename_target: Option<AttachableRef>,
    pub session_context_menu: Option<SessionContextMenu>,

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

    /// Whether each plugin-owned screen's focused surface is currently capturing
    /// free text (a title/filter/compose/search/API-key input), as reported by
    /// its last frame's `RenderResult.captures_text`. Refreshed every tick by
    /// `tick_plugin_renders` from `RuntimeHandle::captures_text`.
    ///
    /// While the entry for `current_screen` is `true`, the host key dispatch
    /// (`is_text_input_context` + the plugin key-forwarder) suppresses its own
    /// global single-character shortcuts (`H`/`?`/`W`) and forwards `?`/`H` to
    /// the plugin so keystrokes land in the input verbatim instead of toggling
    /// help / wiring the statusline (8hx). Absent entry (never painted, or not a
    /// plugin screen) reads as `false`.
    pub plugin_captures_text: std::collections::HashMap<crate::app::screens::ScreenId, bool>,

    /// Last `plugin/render` failure per plugin-owned screen id, as reported by
    /// the render oneshot that `tick_plugin_renders` now keeps instead of
    /// dropping. Set on `RenderOutcome::RuntimeError` / `PluginError`, cleared
    /// the moment a frame renders successfully.
    ///
    /// `PluginScreen::render` paints this instead of the "connecting…"
    /// placeholder, which is the difference between a screen that explains it
    /// cannot start the plugin and one that claims to be loading forever.
    pub plugin_render_errors: std::collections::HashMap<crate::app::screens::ScreenId, String>,

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

    /// Background remote-repo pre-flight for the Configure screen (ls-remote
    /// at open: does the repo exist, does it have branches). Applied by
    /// `check_repo_check_complete` on the next tick; the `u64` is a
    /// generation guard so a stale check can't stamp a newer Configure form.
    pub repo_check_receiver: Option<mpsc::UnboundedReceiver<RepoCheckPayload>>,
    /// Current repo-check generation (bumped on every Configure open).
    pub repo_check_seq: u64,

    /// Background empty-remote initialization (`[i]` on Configure: README +
    /// initial commit + push). `Ok(branch)` carries the branch the commit
    /// landed on. Applied by `check_repo_init_complete` on the next tick.
    pub repo_init_receiver: Option<mpsc::UnboundedReceiver<(u64, Result<String, String>)>>,
    /// Current repo-init generation.
    pub repo_init_seq: u64,

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

    let session_label_store = SessionLabelStore::load();
    for interactive_session in interactive_sessions {
        let mut session = interactive_session.to_session_model();
        if let Some(label) = session_label_store.get(&interactive_session.tmux_session_name) {
            session.display_name = Some(label.clone());
        }
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
/// with Interactive results. The Docker probe runs on a blocking thread so a
/// wedged Docker socket can't pin a tokio runtime thread.
async fn fetch_boss_mode_workspaces() -> Vec<Workspace> {
    const BOSS_MODE_TIMEOUT: Duration = Duration::from_secs(5);

    // DISPLAY CLASS: cached. A stale "no" costs this refresh its Boss-mode
    // rows and nothing else; the next refresh inside 30s shows them.
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
    /// Raw provider model ID. AINB passes this through unchanged to Claude or
    /// Codex; provider CLI owns validation and model catalog updates.
    model: Option<String>,
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
    BulkStopSessions(Vec<Uuid>),           // Soft-stop many sessions (tmux only; keeps worktrees)
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
            session_filter: app_config.ui_preferences.session_filter,
            current_screen: screen_ids::HOME.to_string(),
            should_quit: false,
            logs: HashMap::new(),
            help_visible: false,
            new_session_state: None,
            pending_async_action: None,
            pending_daemon_config_edits: Vec::new(),
            hangar_daemon_config_loaded: false,
            async_operation_cancelled: false,
            confirmation_dialog: None,
            mcp_overlay: None,
            ui_needs_refresh: false,
            claude_chat_visible: false,
            focused_pane: FocusedPane::Sessions,
            embed: None,
            embed_session: None,
            observer_pending: None,
            observer_failed_target: None,
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
            session_label_store: SessionLabelStore::load(),
            session_label_rename_mode: false,
            session_label_rename_buffer: String::new(),
            session_label_rename_target: None,
            session_context_menu: None,

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
            plugin_captures_text: std::collections::HashMap::new(),
            plugin_render_errors: std::collections::HashMap::new(),
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
            // Configure remote-repo pre-flight (ls-remote at open)
            repo_check_receiver: None,
            repo_check_seq: 0,
            // Configure empty-remote initialization ([i] → README + push)
            repo_init_receiver: None,
            repo_init_seq: 0,

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

    /// Map a finished wizard `OnboardingState` onto the persisted
    /// `OnboardingConfig`.
    ///
    /// This is the take-effect seam for the first-run questionnaire: it copies
    /// the user's source/role/use-case selections (plus git directories and
    /// skipped dependencies) into the config that `complete_onboarding` writes
    /// to disk. Kept as a pure function so the mapping can be exercised in
    /// isolation without touching the real `~/.agents-in-a-box` directory.
    fn onboarding_config_from_state(
        state: &crate::components::onboarding::OnboardingState,
    ) -> crate::config::OnboardingConfig {
        use crate::config::OnboardingConfig;

        let mut config = OnboardingConfig::default();
        config.mark_completed();
        config.git_directories = state.get_valid_directories();
        config.skipped_dependencies = state.skipped_dependencies.clone();
        config.source = state.selected_source();
        config.role = state.selected_role();
        config.use_case = state.selected_use_case();
        config
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
        if let Some(state) = &self.onboarding_state {
            // Save onboarding config
            let config = Self::onboarding_config_from_state(state);
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
            // DISPLAY CLASS: cached. A stale "no" defers the refresh to the
            // next check (this runs on every workspace load, and a periodic
            // 5-minute check runs behind it). Nothing is left behind by
            // skipping it.
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
        //
        // DISPLAY CLASS: cached. A stale "no" costs this load its Boss-mode
        // rows; the next load inside 30s picks them up, and nothing leaks.
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
                        .args([
                            "has-session",
                            "-t",
                            &format!("={}", shell.tmux_session_name),
                        ])
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

        // Set initial selection from rows visible under the active filter.
        if !self.select_first_visible_workspace_item_from(0) {
            if !self.ssh_sessions.is_empty() {
                // No workspaces but there are SSH sessions - select the first one
                self.selected_ssh_session_index = Some(0);
            } else if !self.other_tmux_sessions.is_empty() {
                // No workspaces or SSH sessions but there are "Other tmux" sessions - select the first one
                self.selected_other_tmux_index = Some(0);
            } else {
                info!("No active sessions found. Use 'n' to create a new session.");
                // Selection indices already reset above
            }
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

                            if !self.select_first_visible_workspace_item_from(0) {
                                if !self.ssh_sessions.is_empty() {
                                    // No workspaces but there are SSH sessions - select the first one
                                    self.selected_ssh_session_index = Some(0);
                                } else if !self.other_tmux_sessions.is_empty() {
                                    // No workspaces or SSH sessions but there are "Other tmux" sessions
                                    self.selected_other_tmux_index = Some(0);
                                }
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

        // Workspace bucket key. Two paths that both FAIL to canonicalize must
        // NOT compare equal (the old `a.canonicalize().ok() == b.canonicalize().ok()`
        // matched `None == None`, so the fabricated `__broken_worktrees__`
        // sentinel absorbed every workspace whose path was also unresolvable).
        let canonical_key = |p: &std::path::Path| -> std::path::PathBuf {
            p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
        };

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
                    let mut session = interactive_session.to_session_model();
                    if let Some(label) =
                        self.session_label_store.get(&interactive_session.tmux_session_name)
                    {
                        session.display_name = Some(label.clone());
                    }

                    // Find or create workspace for this session.
                    // Use source_repository (the original git repo) not worktree_path parent.
                    // When the worktree is broken (inside no git repo at all),
                    // source_repository was collapsed onto worktree_path by the discovery
                    // fallback, bucket every such session under a single sentinel path so
                    // they all collapse into one "(broken)" workspace row instead of
                    // fanning out. A plain checkout is NOT broken: it resolves to itself,
                    // so it buckets under its own repo root and joins any existing row for
                    // that repository (including linked worktrees off the same repo).
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

                    let workspace_key = canonical_key(workspace_path);
                    if let Some(workspace) = self
                        .workspaces
                        .iter_mut()
                        .find(|w| canonical_key(std::path::Path::new(&w.path)) == workspace_key)
                    {
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
        // Skip "dead-but-not-deleted" worktrees (dir exists but sits inside NO
        // git repository, usually a leftover cache like `.vite/` keeping the
        // dir alive). Without this guard the loader fabricates a phantom
        // workspace named after the sanitized worktree-dir basename and bunches
        // every such session into it, because the previous grouping key was
        // `worktree_path.parent()` (a single shared dir for every flat
        // worktree). These entries should be surfaced via /recover-sessions
        // instead.
        //
        // Plain checkouts and subdirectories of a checkout resolve fine (see
        // `get_source_repository`), so a stopped session created with
        // `ainb run --repo <clone>` stays visible here instead of vanishing.
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
                    "Skipping stopped session {}: worktree {:?} is inside no git repository (broken). Use /recover-sessions to clean up.",
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
            let workspace_key = canonical_key(&workspace_path);

            if let Some(workspace) = self
                .workspaces
                .iter_mut()
                .find(|w| canonical_key(std::path::Path::new(&w.path)) == workspace_key)
            {
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
            metadata.launch_model(),
        );
        session.id = metadata.session_id;
        // `Session::new_with_options` fabricates `ainb/<workspace>` for a new
        // session. A stopped session already has a worktree, so recover the
        // branch from that checkout instead. This retains custom prefixes in
        // the list and also repairs metadata written before branch persistence.
        if let Some(branch_name) = crate::git::current_branch_at(&metadata.worktree_path) {
            session.branch_name = branch_name;
        }
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

        let current_tmux_session = if std::env::var_os("TMUX").is_some() {
            Command::new("tmux")
                .args(["display-message", "-p", "#{session_name}"])
                .output()
                .await
                .ok()
                .filter(|output| output.status.success())
                .and_then(|output| String::from_utf8(output.stdout).ok())
                .map(|session| session.trim().to_string())
        } else {
            None
        };

        let sessions_output = String::from_utf8_lossy(&output.stdout);
        let mut other_sessions = Vec::new();
        let mut ssh_sessions = Vec::new();

        for line in sessions_output.lines() {
            let parts: Vec<&str> = line.split(':').collect();
            if parts.len() >= 3 {
                // Session name may contain colons, so reconstruct from all parts except last two
                let name = parts[..parts.len() - 2].join(":");

                // A live observer of ainb's own tmux client would consume each
                // repaint and trigger another repaint forever.
                if current_tmux_session.as_deref() == Some(name.as_str()) {
                    continue;
                }

                // Skip shell sessions (ainb-ws-*, ainb-sh-*, ainb-shell-*)
                if name.starts_with("ainb-ws-")
                    || name.starts_with("ainb-sh-")
                    || name.starts_with("ainb-shell-")
                {
                    continue;
                }

                let attached_clients = parts[parts.len() - 2].parse::<usize>().unwrap_or(0);
                let attached =
                    attached_clients > usize::from(self.is_observing_tmux_session(name.as_str()));
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
                    if let Some(preserved_name) = self.session_label_store.get(&name) {
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

        self.select_first_visible_workspace_item_from(0);
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
    /// live tmux session. Returned in list order, like the delete path.
    pub fn selected_resumable_session_ids(&self) -> Vec<Uuid> {
        use crate::models::SessionStatus;
        // List order, like the delete path: the order sessions are resumed in,
        // and the order they appear in the log and the audit trail, should not
        // be the per-process randomisation of HashSet iteration.
        self.selected_session_ids_in_order()
            .into_iter()
            .filter(|id| {
                self.find_session(*id).is_some_and(|s| {
                    is_stoppable_interactive(s) && matches!(s.status, SessionStatus::Stopped)
                })
            })
            .collect()
    }

    /// Multi-selected managed session ids in list order (workspace, then
    /// session), so a dialog that names them reads the same way the list does
    /// instead of following `HashSet` iteration order. Ids no longer present in
    /// any workspace are kept, at the end, so nothing silently drops out of a
    /// bulk operation.
    pub fn selected_session_ids_in_order(&self) -> Vec<Uuid> {
        let mut seen: HashSet<Uuid> = HashSet::new();
        let mut ordered: Vec<Uuid> = self
            .workspaces
            .iter()
            .flat_map(|w| w.sessions.iter())
            .map(|s| s.id)
            .filter(|id| self.selected_sessions.contains(id) && seen.insert(*id))
            .collect();
        // Ids that resolve to no session have no list position, so they are
        // sorted rather than left in HashSet order, which Rust randomises per
        // process and would make the dialog text differ run to run.
        let mut orphans: Vec<Uuid> =
            self.selected_sessions.iter().copied().filter(|id| !seen.contains(id)).collect();
        orphans.sort();
        ordered.extend(orphans);
        ordered
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

    fn select_workspace_item(&mut self, workspace_idx: usize, session_idx: Option<usize>) {
        self.selected_workspace_index = Some(workspace_idx);
        self.selected_session_index = session_idx;
        self.shell_selected = session_idx.is_none();
        self.selected_ssh_session_index = None;
        self.selected_other_tmux_index = None;
        if session_idx.is_some() {
            self.queue_logs_fetch();
        }
    }

    fn select_first_visible_workspace_item_from(&mut self, start: usize) -> bool {
        let target = self.workspaces.iter().enumerate().skip(start).find_map(
            |(workspace_idx, workspace)| {
                workspace
                    .sessions
                    .iter()
                    .position(|session| self.session_passes_filter(session))
                    .map(|session_idx| (workspace_idx, Some(session_idx)))
                    .or_else(|| workspace.shell_session.as_ref().map(|_| (workspace_idx, None)))
            },
        );
        if let Some((workspace_idx, session_idx)) = target {
            self.select_workspace_item(workspace_idx, session_idx);
            true
        } else {
            false
        }
    }

    fn select_last_visible_workspace_item_before(&mut self, end: usize) -> bool {
        let target = self.workspaces.iter().enumerate().take(end).rev().find_map(
            |(workspace_idx, workspace)| {
                workspace.shell_session.as_ref().map(|_| (workspace_idx, None)).or_else(|| {
                    workspace
                        .sessions
                        .iter()
                        .rposition(|session| self.session_passes_filter(session))
                        .map(|session_idx| (workspace_idx, Some(session_idx)))
                })
            },
        );
        if let Some((workspace_idx, session_idx)) = target {
            self.select_workspace_item(workspace_idx, session_idx);
            true
        } else {
            false
        }
    }

    fn select_first_visible_workspace_item_before(&mut self, end: usize) -> bool {
        let target = self.workspaces.iter().enumerate().take(end).rev().find_map(
            |(workspace_idx, workspace)| {
                workspace
                    .sessions
                    .iter()
                    .position(|session| self.session_passes_filter(session))
                    .map(|session_idx| (workspace_idx, Some(session_idx)))
                    .or_else(|| workspace.shell_session.as_ref().map(|_| (workspace_idx, None)))
            },
        );
        if let Some((workspace_idx, session_idx)) = target {
            self.select_workspace_item(workspace_idx, session_idx);
            true
        } else {
            false
        }
    }

    /// Helper: Move to next workspace's first session/shell, or SSH sessions, or Other tmux
    fn move_to_next_workspace_first_item(&mut self, current_workspace_idx: usize) {
        if self.select_first_visible_workspace_item_from(current_workspace_idx + 1) {
            return;
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
                } else {
                    self.select_last_visible_workspace_item_before(self.workspaces.len());
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
                self.select_last_visible_workspace_item_before(self.workspaces.len());
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
                    if let Some(session_idx) = workspace
                        .sessions
                        .iter()
                        .rposition(|session| self.session_passes_filter(session))
                    {
                        // Go back to last regular session
                        self.shell_selected = false;
                        self.selected_session_index = Some(session_idx);
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
                        self.select_last_visible_workspace_item_before(workspace_idx);
                        // else: at first workspace, first session - stay (no wrap)
                    }
                }
            }
        }
    }

    pub fn next_workspace(&mut self) {
        if !self.workspaces.is_empty() {
            let current = self.selected_workspace_index.unwrap_or(0);
            let start = (current + 1) % self.workspaces.len();
            if !self.select_first_visible_workspace_item_from(start) && start > 0 {
                self.select_first_visible_workspace_item_from(0);
            }
        }
    }

    pub fn previous_workspace(&mut self) {
        if !self.workspaces.is_empty() {
            let current = self.selected_workspace_index.unwrap_or(0);
            if !self.select_first_visible_workspace_item_before(current) {
                self.select_first_visible_workspace_item_before(self.workspaces.len());
            }
        }
    }

    pub fn select_first_visible_session_in_current_workspace(&mut self) {
        let session_idx = self.selected_workspace_index.and_then(|workspace_idx| {
            self.workspaces.get(workspace_idx).and_then(|workspace| {
                workspace
                    .sessions
                    .iter()
                    .position(|session| self.session_passes_filter(session))
            })
        });
        if let Some(session_idx) = session_idx {
            self.selected_session_index = Some(session_idx);
            self.shell_selected = false;
            self.queue_logs_fetch();
        }
    }

    pub fn select_last_visible_session_in_current_workspace(&mut self) {
        let session_idx = self.selected_workspace_index.and_then(|workspace_idx| {
            self.workspaces.get(workspace_idx).and_then(|workspace| {
                workspace
                    .sessions
                    .iter()
                    .rposition(|session| self.session_passes_filter(session))
            })
        });
        if let Some(session_idx) = session_idx {
            self.selected_session_index = Some(session_idx);
            self.shell_selected = false;
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
        self.app_config.ui_preferences.session_filter = self.session_filter;
        if let Err(e) = self.app_config.save() {
            warn!("Failed to persist session filter: {}", e);
        }
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
                    self.session_label_store.set(key, session.display_name.clone());
                    if let Err(e) = self.session_label_store.save() {
                        warn!("Failed to save session labels: {}", e);
                    }
                }
            }
        }

        self.ssh_session_rename_mode = false;
        self.ssh_session_rename_buffer.clear();
    }

    /// Open durable-label editing for the selected managed or SSH session.
    pub fn start_session_label_rename(&mut self) {
        let target = if let (Some(workspace_idx), Some(session_idx)) =
            (self.selected_workspace_index, self.selected_session_index)
        {
            Some(AttachableRef::WorkspaceSession {
                workspace_idx,
                session_idx,
            })
        } else {
            self.selected_ssh_session_index
                .map(|ssh_idx| AttachableRef::SshSession { ssh_idx })
        };
        let Some(target) = target else {
            return;
        };

        let current = match target {
            AttachableRef::WorkspaceSession {
                workspace_idx,
                session_idx,
            } => self
                .workspaces
                .get(workspace_idx)
                .and_then(|workspace| workspace.sessions.get(session_idx))
                .and_then(|session| session.display_name.clone()),
            AttachableRef::SshSession { ssh_idx } => {
                self.ssh_sessions.get(ssh_idx).and_then(|session| session.display_name.clone())
            }
            _ => None,
        };
        self.session_label_rename_target = Some(target);
        self.session_label_rename_buffer = current.unwrap_or_default();
        self.session_label_rename_mode = true;
    }

    pub fn cancel_session_label_rename(&mut self) {
        self.session_label_rename_mode = false;
        self.session_label_rename_buffer.clear();
        self.session_label_rename_target = None;
    }

    pub fn session_label_rename_char(&mut self, c: char) {
        if self.session_label_rename_mode {
            self.session_label_rename_buffer.push(c);
        }
    }

    pub fn session_label_rename_backspace(&mut self) {
        if self.session_label_rename_mode {
            self.session_label_rename_buffer.pop();
        }
    }

    /// Validate, persist, and immediately render a durable session label.
    pub fn confirm_session_label_rename(&mut self) {
        let Some(target) = self.session_label_rename_target else {
            return self.cancel_session_label_rename();
        };
        let label = match crate::config::normalize_session_label(&self.session_label_rename_buffer)
        {
            Ok(label) => label,
            Err(error) => {
                self.add_error_notification(error);
                return;
            }
        };

        let tmux_name = match target {
            AttachableRef::WorkspaceSession {
                workspace_idx,
                session_idx,
            } => self
                .workspaces
                .get_mut(workspace_idx)
                .and_then(|workspace| workspace.sessions.get_mut(session_idx)),
            AttachableRef::SshSession { ssh_idx } => self.ssh_sessions.get_mut(ssh_idx),
            _ => None,
        }
        .and_then(|session| {
            session.display_name = label.clone();
            session.tmux_session_name.clone()
        });

        if let Some(tmux_name) = tmux_name {
            self.session_label_store.set(tmux_name, label);
            if let Err(error) = self.session_label_store.save() {
                self.add_error_notification(format!("Failed to save session label: {error}"));
                return;
            }
        }
        self.cancel_session_label_rename();
    }

    pub fn open_session_context_menu(&mut self, target: AttachableRef) {
        self.select_attachable(target);
        self.session_context_menu = Some(SessionContextMenu {
            target,
            selected: 0,
        });
    }

    pub fn close_session_context_menu(&mut self) {
        self.session_context_menu = None;
    }

    pub fn session_context_actions(&self) -> &'static [SessionContextAction] {
        const MANAGED: &[SessionContextAction] = &[
            SessionContextAction::Attach,
            SessionContextAction::Restart,
            SessionContextAction::EditLabel,
            SessionContextAction::OpenEditor,
            SessionContextAction::OpenShell,
            SessionContextAction::OpenGit,
            SessionContextAction::QuickCommit,
            SessionContextAction::Delete,
        ];
        const SSH: &[SessionContextAction] = &[
            SessionContextAction::Attach,
            SessionContextAction::EditLabel,
            SessionContextAction::Delete,
        ];
        match self.session_context_menu.map(|menu| menu.target) {
            Some(AttachableRef::SshSession { .. }) => SSH,
            _ => MANAGED,
        }
    }

    pub fn session_context_next(&mut self, delta: isize) {
        let len = self.session_context_actions().len();
        if let Some(menu) = self.session_context_menu.as_mut() {
            menu.selected = (menu.selected as isize + delta).rem_euclid(len as isize) as usize;
        }
    }

    pub fn take_session_context_action(&mut self) -> Option<SessionContextAction> {
        let selected = self.session_context_menu?.selected;
        let action = self.session_context_actions().get(selected).copied();
        self.close_session_context_menu();
        action
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

        // Check for uncommitted changes in the session's worktree
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

        self.confirmation_dialog = Some(stop_or_delete_dialog(
            "Stop or Delete Session".to_string(),
            "Stop keeps the worktree and resumes later. Delete removes the worktree.".to_string(),
            warning,
            ("Stop", ConfirmAction::StopSession(session_id)),
            ("Delete", ConfirmAction::DeleteSession(session_id)),
        ));
    }

    /// Uncommitted-work warning for one session, or None when there is nothing
    /// to say.
    ///
    /// Runs the same code as the bulk dialog on a one-element selection, so the
    /// two cannot apply different skip rules or different wording to the same
    /// session.
    fn check_session_uncommitted_warning(&self, session_id: Uuid) -> Option<String> {
        let name = self
            .find_session(session_id)
            .map_or_else(|| unknown_session_label(session_id), |s| s.name.clone());
        let status = Self::bulk_uncommitted_counts(&[(session_id, name)]);
        Self::format_bulk_uncommitted_warning(&status.dirty, status.unchecked, 1)
    }

    /// Show the tri-option Stop all / Delete all / Cancel dialog for every
    /// multi-selected session.
    ///
    /// The single-row flow has always defaulted to Stop so a stray `d` cannot
    /// destroy a worktree. The bulk flow used to skip confirmation entirely and
    /// delete immediately, taking uncommitted work with it; it now gets the same
    /// dialog, the same Stop default, and an aggregate uncommitted-work warning.
    pub fn show_bulk_delete_or_stop_confirmation(&mut self, session_ids: Vec<Uuid>) {
        use crate::models::SessionStatus;

        if session_ids.is_empty() {
            self.add_warning_notification(NOTHING_SELECTED_WARNING.to_string());
            return;
        }
        let count = session_ids.len();
        info!(
            "Showing bulk Stop/Delete/Cancel dialog for {} session(s)",
            count
        );

        // One pass over the selection: every later question (what to name, how
        // many worktrees, what can be stopped) is answered from this, rather
        // than walking the workspace list once per question per id.
        let mut id_names: Vec<(Uuid, String)> = Vec::with_capacity(count);
        let mut stoppable: Vec<Uuid> = Vec::new();
        let mut already_stopped = 0;
        let mut no_stop_path = 0;
        for id in &session_ids {
            let session = self.find_session(*id);
            id_names.push((
                *id,
                session.map_or_else(|| unknown_session_label(*id), |s| s.name.clone()),
            ));
            // Stop only means something for interactive agent sessions: it kills
            // tmux and the session resumes later. Boss (Docker) and Shell
            // sessions have no such path, and an already-Stopped session has
            // nothing to stop. The two exclusions are counted apart because the
            // dialog has to say which applies: "cannot be stopped" and "already
            // stopped" are opposite claims about whether it can come back.
            match session {
                Some(s) if !is_stoppable_interactive(s) => no_stop_path += 1,
                Some(s) if matches!(s.status, SessionStatus::Stopped) => already_stopped += 1,
                Some(_) => stoppable.push(*id),
                None => no_stop_path += 1,
            }
        }

        let status = Self::bulk_uncommitted_counts(&id_names);
        // What Delete actually removes: distinct trees on disk, not selected
        // rows. When nothing could be resolved the number is a guess, so the
        // message says "their worktrees" rather than printing a figure as fact.
        let removes = if status.worktree_count_known {
            format!("{} worktree(s)", status.with_worktree)
        } else {
            "their worktrees".to_string()
        };
        let warning = Self::format_bulk_uncommitted_warning(&status.dirty, status.unchecked, count);
        let summary = Self::format_bulk_session_summary(&id_names);

        self.confirmation_dialog = Some(if stoppable.len() == count {
            stop_or_delete_dialog(
                format!("Stop or Delete {count} Session(s)"),
                format!(
                    "{summary}\nStop keeps every worktree and resumes later. \
                     Delete removes {removes}."
                ),
                warning,
                ("Stop all", ConfirmAction::BulkStopSessions(stoppable)),
                ("Delete all", ConfirmAction::BulkDeleteSessions(session_ids)),
            )
        } else if stoppable.is_empty() {
            let reason = if no_stop_path == 0 {
                "Every one of these is already stopped, so there is nothing to stop"
            } else if already_stopped == 0 {
                "None of these sessions can be stopped and resumed"
            } else {
                "These sessions are either already stopped or have no stop path"
            };
            ConfirmationDialog {
                title: format!("Delete {count} Session(s)"),
                message: format!(
                    "{summary}\n{reason}, so Delete is the only option offered. \
                     It removes {removes}."
                ),
                confirm_action: ConfirmAction::BulkDeleteSessions(session_ids),
                selected_option: false, // Default = No
                warning,
                options: None,
                selected_index: 0,
            }
        } else {
            let stoppable_count = stoppable.len();
            let rest = count - stoppable_count;
            let (subject, verb) = if rest == 1 {
                ("the other one".to_string(), "is")
            } else {
                (format!("the other {rest}"), "are")
            };
            let excluded = if no_stop_path == 0 {
                format!("{subject} {verb} already stopped")
            } else if already_stopped == 0 {
                format!("{subject} cannot be stopped")
            } else {
                format!("{subject} {verb} already stopped or cannot be stopped")
            };
            stop_or_delete_dialog(
                format!("Stop or Delete {count} Session(s)"),
                format!(
                    "{summary}\nStop covers {stoppable_count} and keeps their worktrees, \
                     {excluded}. Delete removes {removes}."
                ),
                warning,
                (
                    &format!("Stop {stoppable_count}"),
                    ConfirmAction::BulkStopSessions(stoppable),
                ),
                ("Delete all", ConfirmAction::BulkDeleteSessions(session_ids)),
            )
        });
    }

    /// One-line "which sessions are affected" summary for the bulk dialog.
    /// Long selections are truncated so the message still fits the dialog.
    fn format_bulk_session_summary(names: &[(Uuid, String)]) -> String {
        debug_assert!(
            !names.is_empty(),
            "the caller returns early on an empty selection"
        );
        format!(
            "{} session(s): {}",
            names.len(),
            truncate_list(names.iter().map(|(_, name)| name.clone()))
        )
    }

    /// What the selection has to lose.
    ///
    /// Probes each distinct worktree once, not each session: two sessions can
    /// resolve to the same tree, and reporting its four modified files twice
    /// would say eight. A tree whose status cannot be read is NOT reported as
    /// clean, because "unknown" and "nothing to lose" must never look the same
    /// on a delete confirmation. Every selected id is resolved, Shell sessions
    /// included: what delete removes is whatever `by-session/<uuid>` points at,
    /// so anything with a directory is counted and probed.
    fn bulk_uncommitted_counts(names: &[(Uuid, String)]) -> BulkWorktreeStatus {
        use crate::git::WorktreeManager;

        /// Probes in flight at once. Each one forks a `git status`, so the fan
        /// out is bounded: a 40-row selection must not spawn 40 threads and 40
        /// child processes off a single keypress.
        const MAX_CONCURRENT_PROBES: usize = 8;

        let Ok(worktree_manager) = WorktreeManager::for_reading() else {
            warn!("Cannot resolve worktrees: uncommitted work is unknown for this selection");
            return BulkWorktreeStatus {
                dirty: Vec::new(),
                unchecked: names.len(),
                with_worktree: names.len(),
                worktree_count_known: false,
            };
        };

        // Resolve directories first: a stat, not a subprocess, and it collapses
        // sessions that share a tree into one probe. Every selected id is
        // resolved, including ones with no session row and Shell sessions: what
        // delete removes is whatever `by-session/<uuid>` points at, so anything
        // with a directory gets probed and counted.
        let mut probes: Vec<(PathBuf, Vec<String>)> = Vec::new();
        let mut seen: HashMap<PathBuf, usize> = HashMap::new();
        let mut status = BulkWorktreeStatus::default();
        for (id, name) in names {
            let dir = match worktree_manager.session_dir(*id) {
                // Nothing on disk: deleting this session destroys no files.
                Ok(None) => continue,
                Ok(Some(dir)) => dir,
                Err(e) => {
                    // The link is there and could not be followed, which is
                    // unknown, not empty.
                    warn!("Could not resolve the worktree for {id}: {e}");
                    status.unchecked += 1;
                    status.with_worktree += 1;
                    continue;
                }
            };
            // Canonicalise before deduping, or /tmp and /private/tmp count twice
            // on macOS. Fall back to the raw path when it cannot be resolved.
            let key = dir.canonicalize().unwrap_or_else(|_| dir.clone());
            // Sessions sharing a tree are named together: naming only the first
            // would tell the user the others have nothing to lose.
            if let Some(&idx) = seen.get(&key) {
                probes[idx].1.push(name.clone());
            } else {
                seen.insert(key, probes.len());
                status.with_worktree += 1;
                probes.push((dir, vec![name.clone()]));
            }
        }

        // `git status` is a subprocess per tree and this runs inline on a
        // keypress. Batching does not reduce the number of calls, one per tree
        // either way; it bounds how many run at once so a large selection cannot
        // spawn a thread and a child process per tree all at the same time.
        let mut counts: Vec<Result<usize, ()>> = Vec::with_capacity(probes.len());
        for batch in probes.chunks(MAX_CONCURRENT_PROBES) {
            let batch_counts: Vec<Result<usize, ()>> = std::thread::scope(|scope| {
                // Spawn every probe in the batch before joining any of them,
                // otherwise they run one at a time.
                let mut handles = Vec::with_capacity(batch.len());
                for (path, _) in batch {
                    handles.push(scope.spawn(move || probe_tree(path)));
                }
                handles.into_iter().map(|h| h.join().unwrap_or(Err(()))).collect()
            });
            counts.extend(batch_counts);
        }

        for ((_, names), count) in probes.into_iter().zip(counts) {
            match count {
                Ok(0) => {}
                Ok(count) => status.dirty.push((names.join(", "), count)),
                Err(()) => status.unchecked += 1,
            }
        }
        status
    }

    /// Aggregate uncommitted-work warning for the bulk dialog: this is exactly
    /// the work "Delete all" would destroy, so it names the dirty sessions
    /// rather than reporting a bare total.
    /// `selected` is how many rows the dialog is about, which decides the
    /// wording: on a one-row dialog naming the session and saying "1 session(s)"
    /// repeats what the user is looking at, but in a bulk selection the name is
    /// the whole point of the warning.
    fn format_bulk_uncommitted_warning(
        dirty: &[(String, usize)],
        unchecked: usize,
        selected: usize,
    ) -> Option<String> {
        if dirty.is_empty() {
            if unchecked == 0 {
                return None;
            }
            return Some(if selected == 1 {
                "⚠️ could not check this worktree for uncommitted work".to_string()
            } else {
                format!("⚠️ could not check {unchecked} session(s) for uncommitted work")
            });
        }
        let total: usize = dirty.iter().map(|(_, count)| *count).sum();
        let listed = truncate_list(dirty.iter().map(|(name, count)| format!("{name} ({count})")));
        let unknown = if unchecked > 0 {
            format!("; {unchecked} more could not be checked")
        } else {
            String::new()
        };
        if selected == 1 && unchecked == 0 {
            return Some(format!("⚠️ {total} uncommitted file(s) in worktree"));
        }
        Some(format!(
            "⚠️ {} uncommitted file(s) in {} session(s): {}{}",
            total,
            dirty.len(),
            listed,
            unknown
        ))
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

    /// Show confirmation dialog for killing multiple "other" tmux sessions.
    ///
    /// Names them, like the managed-session bulk dialog reached by the same key:
    /// a count alone does not tell the user whether the selection is the one
    /// they meant.
    pub fn show_kill_other_tmux_sessions_confirmation(&mut self, session_names: Vec<String>) {
        let count = session_names.len();
        info!("Showing kill confirmation for {count} other tmux sessions");
        let listed = truncate_list(session_names.iter().cloned());
        self.confirmation_dialog = Some(ConfirmationDialog {
            title: "Kill tmux Sessions".to_string(),
            message: format!(
                "Kill {count} tmux session(s): {listed}?\nThese are not managed by ainb, so \
                 only the tmux session is killed."
            ),
            confirm_action: ConfirmAction::KillOtherTmuxSessions(session_names),
            selected_option: false, // Default to "No"
            warning: Some("⚠️ This closes all selected external tmux sessions".to_string()),
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
        // Pre-1.21 hook installs pinned Homebrew's versioned Cellar binary.
        // This migration is safe only for that recognisable legacy shape; it
        // never rewrites an intentional dev target.
        if let Err(error) = ainb_plugin_notifyd::auto_repair_hook_binary(&paths) {
            tracing::warn!(error = %error, "could not migrate legacy hook binary pointer");
        }
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
        use crate::models::{SessionAgentType, SessionMode};

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
            "antigravity" => SessionAgentType::Antigravity,
            _ => SessionAgentType::Claude,
        };
        let model = if matches!(
            agent_type,
            SessionAgentType::Claude | SessionAgentType::Codex | SessionAgentType::Antigravity
        ) {
            let model = preset.agent_model.trim();
            (!is_default_model(model)).then(|| model.to_string())
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
            model,
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
            // Launching is the retry the message below asks for, so the gate
            // must ask Docker rather than replay a "no" cached before the user
            // started it. The gate owns that decision and the message it
            // carries, which is what makes both of them testable.
            if let DockerGate::Blocked(message) =
                boss_mode_docker_gate(&DOCKER_PROBE, DOCKER_PROBE_TTL, Self::probe_docker_async)
                    .await
            {
                error!("Boss mode requires Docker but Docker is not running");
                self.add_error_notification(message);
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
                snapshot.model,
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
                // `{:#}` walks the anyhow context chain. With `{}` the user saw
                // only "Codex failed to start" and never the cause underneath it
                // (e.g. "did not publish a remote thread within 10 seconds").
                error!("Failed to create session via configure flow: {:#}", e);
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
                // Don't hardcode a base name in the message — the base is the
                // remote's default (main/master/develop) or a picked branch;
                // "off main" misled the empty-repo diagnosis (Stevie 2026-07-04).
                self.add_error_notification(format!("Could not prepare worktree: {msg}"));
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

        // Remote pre-flight: one background ls-remote validates the repo
        // exists, is reachable, and has at least one branch — BEFORE Launch.
        // Without it a typo'd repo dies after Launch with "Clone failed" and
        // an empty repo dies even later at `prepare_remote_worktree` with a
        // cryptic origin/HEAD error (Stevie 2026-07-04: mysocialmedia).
        // `ConfigureState::from_pick_repo` already set `repo_check = Checking`
        // for these sources; `check_repo_check_complete` applies the verdict.
        // Same `is_remote()` predicate as `from_pick_repo`'s Checking
        // decision — the two must agree or the form waits on a verdict that
        // never comes.
        if source.is_remote() {
            self.repo_check_seq += 1;
            let seq = self.repo_check_seq;
            let (tx, rx) = mpsc::unbounded_channel();
            self.repo_check_receiver = Some(rx);
            tokio::spawn(async move {
                let join = tokio::task::spawn_blocking(move || {
                    crate::git::RemoteRepoManager::new()
                        .map_err(|e| e.to_string())?
                        .list_remote_branches(&source)
                        .map_err(|e| e.to_string())
                })
                .await;
                let payload = match join {
                    Ok(r) => r,
                    Err(join_err) => Err(format!("repo check task panicked: {join_err}")),
                };
                let _ = tx.send((seq, payload));
            });
        }
        self.ui_needs_refresh = true;
    }

    /// Poll the background remote-repo pre-flight. Applies the verdict to the
    /// Configure screen: `Failed` blocks Launch with an inline message; a
    /// success also stamps the real default-branch name onto the Branch row
    /// (was a hardcoded "main" placeholder — wrong for master-default repos)
    /// and backfills the "⚠ exists" guard for not-yet-cached remotes.
    /// Returns true when state changed this tick.
    pub fn check_repo_check_complete(&mut self) -> bool {
        use crate::components::new_session::configure::RepoCheck;

        let Some(ref mut receiver) = self.repo_check_receiver else {
            return false;
        };
        let (seq, result) = match receiver.try_recv() {
            Ok(payload) => payload,
            Err(mpsc::error::TryRecvError::Empty) => return false,
            Err(mpsc::error::TryRecvError::Disconnected) => {
                self.repo_check_receiver = None;
                return false;
            }
        };
        self.repo_check_receiver = None;
        if seq != self.repo_check_seq {
            // A newer Configure form superseded this check.
            return false;
        }
        let Some(cfg) = self.new_session_state.as_mut().and_then(|ns| ns.configure_state.as_mut())
        else {
            return false;
        };
        // Apply-side state gate, on top of the seq guard: the verdict only
        // lands on a form that is actually waiting for one. The seq guard
        // alone has a hole — it's bumped only when a REMOTE Configure form
        // opens, so a stale check could otherwise stamp a later local-path or
        // restart form (repo_check = NotApplicable) whose open never bumped
        // the seq.
        if cfg.repo_check != RepoCheck::Checking {
            return false;
        }

        if let Ok(branches) = &result {
            if let Some(default) = branches.iter().find(|b| b.is_default) {
                // Only when the user hasn't already picked a base — a pick
                // owns the Branch-row display.
                if cfg.base_selection.is_none() {
                    cfg.branch_source.clone_from(&default.name);
                }
            }
            // Feed the base-off "⚠ exists" guard for not-yet-cached remotes
            // (a cached remote was already seeded from its clone's refs —
            // that list includes local heads, so don't clobber it).
            if cfg.repo_branch_names.is_empty() {
                cfg.repo_branch_names = branches.iter().map(|b| b.name.clone()).collect();
            }
        }
        let mut offline_warn: Option<String> = None;
        cfg.repo_check = match RepoCheck::from_branches(result.map(|branches| branches.len())) {
            RepoCheck::Failed(msg)
                if crate::git::RemoteRepoManager::new()
                    .ok()
                    .and_then(|m| m.cached_source_path(&cfg.repo_source))
                    .is_some() =>
            {
                // Warm clone cache → the launch path works offline by design
                // (its fetch failures are warn-only). A failed validation
                // must not brick that flow; warn and let Launch proceed.
                offline_warn = Some(msg);
                RepoCheck::Ok
            }
            verdict => verdict,
        };
        if let RepoCheck::Failed(msg) = &cfg.repo_check {
            tracing::warn!(error = %msg, "configure repo pre-flight failed");
        }
        if let Some(msg) = offline_warn {
            tracing::warn!(error = %msg, "repo pre-flight failed but clone cache is warm — allowing launch");
            self.add_warning_notification(format!(
                "Could not validate remote — using cached clone ({msg})"
            ));
        }
        true
    }

    /// `[i]` on an `EmptyRemote` verdict: initialize the empty remote in
    /// place — clone (an empty clone succeeds), commit a README, push — so
    /// the user never has to leave ainb to make a fresh repo launchable.
    /// The component already flipped `repo_check` to `Initializing`; the
    /// verdict lands via `check_repo_init_complete`.
    pub fn initialize_remote_repo(&mut self) {
        let Some(source) = self
            .new_session_state
            .as_ref()
            .and_then(|ns| ns.configure_state.as_ref())
            .map(|cfg| cfg.repo_source.clone())
        else {
            return;
        };
        self.repo_init_seq += 1;
        let seq = self.repo_init_seq;
        let (tx, rx) = mpsc::unbounded_channel();
        self.repo_init_receiver = Some(rx);
        tokio::spawn(async move {
            let join = tokio::task::spawn_blocking(move || {
                let manager = crate::git::RemoteRepoManager::new().map_err(|e| e.to_string())?;
                let parsed = source.parse_components().map_err(|e| e.to_string())?;
                manager.initialize_empty_remote(&source, &parsed).map_err(|e| e.to_string())
            })
            .await;
            let payload = match join {
                Ok(r) => r,
                Err(join_err) => Err(format!("repo init task panicked: {join_err}")),
            };
            let _ = tx.send((seq, payload));
        });
    }

    /// Poll the background empty-remote initialization. Success flips the
    /// Configure verdict to Ok, stamps the pushed branch onto the Branch row,
    /// and toasts; failure returns to `EmptyRemote` (so `[i]` can retry) with
    /// the exact git error in a toast. Returns true when state changed.
    pub fn check_repo_init_complete(&mut self) -> bool {
        use crate::components::new_session::configure::RepoCheck;

        let Some(ref mut receiver) = self.repo_init_receiver else {
            return false;
        };
        let (seq, result) = match receiver.try_recv() {
            Ok(payload) => payload,
            Err(mpsc::error::TryRecvError::Empty) => return false,
            Err(mpsc::error::TryRecvError::Disconnected) => {
                self.repo_init_receiver = None;
                return false;
            }
        };
        self.repo_init_receiver = None;
        if seq != self.repo_init_seq {
            return false;
        }
        let mut toast: Option<Result<String, String>> = None;
        if let Some(cfg) =
            self.new_session_state.as_mut().and_then(|ns| ns.configure_state.as_mut())
        {
            // Apply-side state gate (mirrors check_repo_check_complete): only
            // a form that is actually Initializing takes the verdict. Without
            // it, [i] on repo A → Esc → open repo B lets A's init result
            // force B's verdict to Ok while B is still Checking — reopening
            // the fail-after-Launch hole this feature closes.
            if cfg.repo_check != RepoCheck::Initializing {
                return false;
            }
            match result {
                Ok(branch) => {
                    if cfg.base_selection.is_none() {
                        cfg.branch_source.clone_from(&branch);
                    }
                    if cfg.repo_branch_names.is_empty() {
                        cfg.repo_branch_names = vec![branch.clone()];
                    }
                    cfg.repo_check = RepoCheck::Ok;
                    toast = Some(Ok(branch));
                }
                Err(msg) => {
                    // Back to the actionable verdict — `[i]` retries.
                    cfg.repo_check = RepoCheck::EmptyRemote;
                    toast = Some(Err(msg));
                }
            }
        }
        match toast {
            Some(Ok(branch)) => {
                self.add_info_notification(format!(
                    "Initialized repository — pushed README to origin/{branch}"
                ));
            }
            Some(Err(msg)) => {
                tracing::error!(error = %msg, "empty-remote initialization failed");
                self.add_error_notification(format!("Could not initialize repository: {msg}"));
            }
            None => {}
        }
        true
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
        model: Option<String>,
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
                    model.clone(),
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
        model: Option<String>,
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
                    model.clone(),
                    headroom_enabled,
                    rtk_enabled,
                    // `prepare_remote_worktree` cloned this tree into a path derived
                    // from THIS session id moments ago, so a failed launch must remove
                    // it rather than leave the directory, its checked-out cache branch
                    // and its index entry behind.
                    crate::interactive::session_manager::WorktreeOwner::ThisSession,
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
                    model.clone(),
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
                let mut session = interactive_session.to_session_model();
                if let Some(label) =
                    self.session_label_store.get(&interactive_session.tmux_session_name)
                {
                    session.display_name = Some(label.clone());
                }

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
                // See the configure-flow comment: `{:#}` keeps the cause.
                error!("Failed to create Interactive session: {:#}", e);
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
            // `=name` so an orphan named "shell-x" cannot take out a live
            // "shell-x-2" via tmux's prefix matching.
            match Command::new("tmux")
                .args(["kill-session", "-t", &format!("={session_name}")])
                .output()
                .await
            {
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

            // Try Boss cleanup if Docker is available. CLEANUP CLASS: this
            // probes Docker rather than reading the 30s cache, because a stale
            // "no" here leaks the container instead of merely delaying a
            // display. See `boss_cleanup_docker_gate`.
            if boss_cleanup_docker_gate(&DOCKER_PROBE, DOCKER_PROBE_TTL, Self::probe_docker_async)
                .await
            {
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
    async fn stop_interactive_session(
        &mut self,
        session_id: Uuid,
        trigger_key: &str,
    ) -> anyhow::Result<()> {
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
            // Hard constraint: kill only the exact named session, never a prefix
            // match. `-t name` resolves exact, then prefix, then fnmatch, so
            // killing "feat-auth" would take out a live "feat-auth-2"; `=name`
            // forces exact. NEVER kill-server.
            let output = tokio::process::Command::new("tmux")
                .args(["kill-session", "-t", &format!("={name}")])
                .output()
                .await;

            // Every exit path below reaches the audit record: a failed kill
            // still has to leave a trail. Only a successful one flips the row
            // to Stopped, see below.
            match output {
                Err(e) => Err(anyhow::anyhow!("Failed to run tmux kill-session: {e}")),
                Ok(output) if output.status.success() => {
                    info!("Killed tmux session: {name}");
                    Ok(())
                }
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    // tmux returns non-zero when the session is already gone, which
                    // is the post-condition we want, so treat it as success.
                    if stderr.contains("can't find session") || stderr.contains("no server running")
                    {
                        info!("tmux session '{name}' already gone, proceeding");
                        Ok(())
                    } else {
                        Err(anyhow::anyhow!(
                            "Failed to kill tmux session '{name}': {stderr}"
                        ))
                    }
                }
            }
        } else {
            warn!(
                "No tmux_session_name for {} — nothing to kill, just marking Stopped",
                session_id
            );
            Ok(())
        };

        // Only a successful kill flips the row to Stopped. Marking a session
        // Stopped while its agent is still running is worse than reporting the
        // failure: the row invites a resume, which would tear down and replace
        // the live agent.
        if result.is_ok() {
            // Drop the live tmux handle but DO NOT touch SessionStore or worktree.
            self.tmux_sessions.remove(&session_id);

            if let Some(session) = self.find_session_mut(session_id) {
                session.set_status(SessionStatus::Stopped);
                session.is_attached = false;
            }
        }

        let audit_result = match &result {
            Ok(()) => AuditResult::Success,
            Err(e) => AuditResult::Failed(e.to_string()),
        };
        audit::audit_session_stopped(
            session_id,
            tmux_name,
            worktree_path,
            AuditTrigger::UserKeypress(trigger_key.to_string()),
            audit_result,
        );

        // The caller owns the workspace refresh so a bulk stop repaints once
        // instead of rescanning every workspace per session.
        result
    }

    /// Soft-stop every session in `session_ids`.
    ///
    /// Each one goes through `stop_interactive_session`, so tmux is killed and
    /// nothing else is: worktrees, the `sessions.json` entries, and the
    /// `by-session/<uuid>` symlinks all survive and every session stays
    /// resumable. The caller refreshes the workspace view once afterwards.
    async fn bulk_stop_sessions(&mut self, session_ids: Vec<Uuid>) {
        use crate::models::SessionStatus;

        let total = session_ids.len();
        let mut stopped = 0;
        let mut already_stopped = 0;
        let mut failed = 0;
        for id in session_ids {
            // The dialog already filters these out, so this is normally zero.
            // It still has to be here: a session can reach Stopped between the
            // dialog opening and this running, and counting it as a stop would
            // report "Stopped 10" for what stopped 5.
            if self
                .find_session(id)
                .is_some_and(|s| matches!(s.status, SessionStatus::Stopped))
            {
                // The dialog excludes these from the action, so their check was
                // never cleared and there is nothing to restore.
                already_stopped += 1;
                continue;
            }
            if let Err(e) = self.stop_interactive_session(id, "D→Stop (bulk)").await {
                error!("Failed to stop session {}: {}", id, e);
                failed += 1;
                // The row was unchecked optimistically when the user confirmed.
                // It is still running, so put the check back rather than making
                // the user hunt for it.
                self.selected_sessions.insert(id);
            } else {
                stopped += 1;
            }
        }
        if failed > 0 {
            let attempted = total - already_stopped;
            let skipped = if already_stopped > 0 {
                format!(", {already_stopped} already stopped")
            } else {
                String::new()
            };
            self.add_warning_notification(format!(
                "Stopped {stopped}/{attempted} sessions ({failed} failed{skipped})"
            ));
        } else if already_stopped > 0 {
            self.add_success_notification(format!(
                "Stopped {stopped} session(s) ({already_stopped} already stopped)"
            ));
        } else {
            self.add_success_notification(format!("Stopped {stopped} session(s)"));
        }
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
        let (skip_permissions, model) = metadata
            .as_ref()
            .map(|m| (m.skip_permissions.unwrap_or(true), m.launch_model()))
            .unwrap_or((true, None));

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
            // first so we get a clean shell. `=name` forces an exact target:
            // bare `-t name` falls through to prefix matching, so resuming
            // "feat-auth" would kill a live "feat-auth-2".
            let _ = tokio::process::Command::new("tmux")
                .args([
                    "kill-session",
                    "-t",
                    &format!("={}", metadata.tmux_session_name),
                ])
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

            // `None` covers both "not a Codex session" and "shared remote
            // control is unavailable on this hangar home" (already warned
            // about). Both resume the session with plain provider argv.
            let mut codex_remote = if metadata.agent_type == SessionAgentType::Codex {
                crate::interactive::session_manager::ensure_codex_remote_thread(
                    metadata.session_id,
                    &metadata.worktree_path,
                    model.as_deref(),
                    skip_permissions,
                    metadata.headroom_enabled,
                    metadata.codex_thread_id.clone(),
                )
                .await?
            } else {
                None
            };

            let manager = InteractiveSessionManager::new()?;
            manager
                .start_cli_in_tmux(
                    &metadata.tmux_session_name,
                    &metadata.worktree_path,
                    skip_permissions,
                    model.clone(),
                    metadata.agent_type,
                    transcript.clone(),
                    true, // resume_requested — Enter/r on a Stopped session
                    metadata.headroom_enabled,
                    codex_remote.as_ref(),
                )
                .await?;

            if codex_remote.as_ref().is_some_and(|remote| remote.thread_id.is_none()) {
                codex_remote = crate::interactive::session_manager::claim_codex_remote_thread(
                    metadata.session_id,
                    &metadata.worktree_path,
                    model.as_deref(),
                    skip_permissions,
                    metadata.headroom_enabled,
                    &metadata.tmux_session_name,
                )
                .await?;
            }
            if let Some(thread_id) =
                codex_remote.as_ref().and_then(|remote| remote.thread_id.as_deref())
            {
                crate::interactive::session_manager::persist_codex_thread_id(
                    metadata.session_id,
                    thread_id.to_string(),
                )?;
            }

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
                    let encoded = Self::claude_project_dir_name(&metadata.worktree_path);
                    format!(
                        "No transcript found at ~/.claude/projects/{} - starting fresh",
                        encoded
                    )
                }
                // Codex resumes via `codex resume --last`, Copilot and Antigravity via `--continue`:
                // all continue the most recent session in the worktree cwd.
                (SessionAgentType::Codex, _)
                | (SessionAgentType::Copilot, _)
                | (SessionAgentType::Antigravity, _) => "Resuming most recent session".to_string(),
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

    /// Canonicalize-then-encode a worktree path into Claude Code's project
    /// directory NAME under `~/.claude/projects/`. Claude Code keys the dir
    /// off the PHYSICAL cwd, so the symlink-resolved path must be encoded
    /// (falling back to the raw path when it does not exist). Single source
    /// of truth for both the transcript probe and any user-facing display of
    /// the expected directory; if resume ever fails on a very long path,
    /// check for a truncated+hashed dir name on disk (not mirrored here).
    pub(crate) fn claude_project_dir_name(worktree_path: &std::path::Path) -> String {
        let physical =
            std::fs::canonicalize(worktree_path).unwrap_or_else(|_| worktree_path.to_path_buf());
        Self::encode_claude_project_dir(&physical)
    }

    /// Encode an absolute path the way Claude Code names its
    /// `~/.claude/projects/{encoded}/` transcript directory: every UTF-16
    /// code unit that is not ASCII-alphanumeric becomes `-` (Claude Code is
    /// JS, so an astral char like an emoji yields TWO dashes). The leading
    /// `/` yields the leading `-`; a dotted component like `.agents-in-a-box`
    /// yields `--agents-in-a-box`. Callers wanting the on-disk dir for a
    /// worktree should use [`Self::claude_project_dir_name`], which
    /// canonicalizes first.
    pub(crate) fn encode_claude_project_dir(worktree_path: &std::path::Path) -> String {
        worktree_path
            .to_string_lossy()
            .chars()
            .flat_map(|c| {
                if c.is_ascii_alphanumeric() {
                    std::iter::repeat_n(c, 1)
                } else {
                    std::iter::repeat_n('-', c.len_utf16())
                }
            })
            .collect()
    }

    /// Find the most recently modified Claude transcript (`*.jsonl`) for the
    /// given worktree under `~/.claude/projects/{encoded}/`.
    ///
    /// Returns `None` when the project directory is missing or contains no
    /// transcripts.
    pub fn find_latest_transcript(worktree_path: &std::path::Path) -> Option<std::path::PathBuf> {
        let home = dirs::home_dir()?;
        Self::find_latest_transcript_in(&home, worktree_path)
    }

    /// Test-friendly variant: caller supplies the home directory so unit tests
    /// don't have to mutate process-wide environment.
    ///
    /// The worktree path is canonicalized first via
    /// [`Self::claude_project_dir_name`]: a symlinked component (`/tmp` →
    /// `/private/tmp` on macOS) would otherwise encode to a directory that
    /// never exists on disk and silently drop `--continue`.
    pub(crate) fn find_latest_transcript_in(
        home: &std::path::Path,
        worktree_path: &std::path::Path,
    ) -> Option<std::path::PathBuf> {
        let project_dir = home
            .join(".claude")
            .join("projects")
            .join(Self::claude_project_dir_name(worktree_path));

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

    /// Reseed the `Hangar Daemon` settings rows from the `daemon_config` table.
    ///
    /// A missing database is the normal state on a machine that has never run
    /// the daemon, so it is silent: the rows keep their coded defaults, which is
    /// what the daemon would apply anyway. A database that exists but cannot be
    /// read IS reported, because then the rows are showing values that may not
    /// be what is stored.
    async fn load_hangar_daemon_config(&mut self) {
        match read_daemon_config().await {
            Ok(Some(stored)) => self.config_screen_state.seed_hangar_daemon_rows(&stored),
            Ok(None) => {}
            Err(error) => {
                warn!(%error, "hangar daemon config: could not read stored values");
                self.add_warning_notification(format!(
                    "Hangar Daemon settings show defaults: {error}"
                ));
            }
        }
    }

    /// Write edited `hangar_daemon.*` rows back to the `daemon_config` table.
    ///
    /// Every value passes `ConfigDescriptor::validate` first, which is the same
    /// gate the RPC handler and the `ainb hangar daemon config set` CLI use, so
    /// the three surfaces cannot disagree about what is legal. Each failure
    /// raises its own error notification naming the row: a write the user asked
    /// for that quietly did not happen is worse than one that visibly failed.
    async fn set_hangar_daemon_config(&mut self, edits: Vec<(String, String)>) {
        let failures = write_daemon_config_batch(&edits).await;
        for (key, error) in &failures {
            warn!(key, %error, "hangar daemon config write failed");
            self.add_error_notification(format!("hangar_daemon.{key}: {error}"));
        }
        // Put a failed row back to the value the database actually holds, and
        // mark it dirty again so a later `S` retries it. `read_daemon_config`
        // only returns rows that EXIST, so a first-time write that failed left
        // the rejected value on screen with `dirty` already cleared: the row
        // claimed the setting had landed and no later save would ever write it.
        let failed_rows: Vec<String> =
            failures.iter().map(|(key, _)| format!("hangar_daemon.{key}")).collect();
        // Re-read rather than trust the write: the row now shows what the
        // database holds, including for any write that just failed.
        self.load_hangar_daemon_config().await;
        // AFTER the re-seed, not before: `seed_hangar_daemon_rows` clears
        // `dirty` for every key it finds in the store, so marking the row first
        // meant the flag was erased on the next line.
        //
        // What this does and does not buy: the row now shows the value the
        // database actually holds, and stays dirty so it is visibly unsaved.
        // For a key that already had a stored row that means a later `S`
        // rewrites the STORED value — a no-op — rather than the one the user
        // typed, because the re-seed has already replaced it. Preserving the
        // rejected input across a failure needs the row to carry a pending
        // value distinct from its displayed one, which the widget has no room
        // for today. The error notification names the row, so the failure is
        // never silent; re-typing it is the recovery.
        for row_key in failed_rows {
            self.config_screen_state.dirty.insert(row_key);
        }
    }

    pub async fn process_async_action(&mut self) -> anyhow::Result<()> {
        // Once, on the first app tick: the `Hangar Daemon` settings rows are
        // seeded with coded defaults synchronously (the store is async and the
        // screen is not), and this replaces them with what is actually stored.
        if !self.hangar_daemon_config_loaded {
            self.hangar_daemon_config_loaded = true;
            self.load_hangar_daemon_config().await;
        }
        if !self.pending_daemon_config_edits.is_empty() {
            let edits = std::mem::take(&mut self.pending_daemon_config_edits);
            self.set_hangar_daemon_config(edits).await;
        }
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
                    if let Err(e) = self.stop_interactive_session(session_id, "D→Stop").await {
                        error!("Failed to stop session {}: {}", session_id, e);
                        self.add_error_notification(format!("Stop failed: {}", e));
                    }
                    // Refresh so the Stopped indicator is rendered.
                    self.load_real_workspaces().await;
                    self.ui_needs_refresh = true;
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
                AsyncAction::BulkStopSessions(session_ids) => {
                    self.bulk_stop_sessions(session_ids).await;
                    self.load_real_workspaces().await;
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
                            // Still there, so keep it checked: the row was
                            // unchecked optimistically on confirmation.
                            self.selected_sessions.insert(id);
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
                    // A manual refresh is the user saying "look again", which
                    // includes at Docker: without this, starting Docker and
                    // hitting refresh keeps showing no Boss-mode rows until the
                    // 30s cache lapses, and the refresh looks broken.
                    invalidate_docker_probe_cache(&DOCKER_PROBE);
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

        // Re-running setup is the retry the message below asks for, so the
        // gate must ask Docker rather than replay a "no" cached before the
        // user started it.
        if let DockerGate::Blocked(message) =
            auth_setup_docker_gate(&DOCKER_PROBE, DOCKER_PROBE_TTL, Self::probe_docker_async).await
        {
            warn!("Docker is not available or not running");
            if let Some(ref mut auth_state) = self.auth_setup_state {
                auth_state.error_message = Some(message);
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
    ///
    /// Answers from `DOCKER_PROBE` whenever a probe ran inside the TTL, so a
    /// wedged Docker costs one 3s stall per window rather than one per call
    /// site. Asks `docker version` rather than `docker info` so this and the
    /// async twin put the same question to Docker, which is what makes a single
    /// shared cache correct.
    ///
    /// DISPLAY CLASS ONLY. A cached answer can be up to `DOCKER_PROBE_TTL` old,
    /// which is fine where a stale "no" costs a render or defers a retryable
    /// piece of work, and wrong where it would skip a teardown. Cleanup and
    /// teardown paths take `boss_cleanup_docker_gate`; a user pressing a key
    /// after being told to start Docker takes `boss_mode_docker_gate` or
    /// `auth_setup_docker_gate`. All three invalidate and probe.
    pub fn is_docker_available_sync() -> bool {
        docker_answer_or_probe(&DOCKER_PROBE, DOCKER_PROBE_TTL, Self::probe_docker_sync)
    }

    /// Ask Docker itself, ignoring the cache. Reached only through
    /// `is_docker_available_sync`, which owns the caching.
    fn probe_docker_sync() -> bool {
        use std::process::{Command, Stdio};

        // Spawn the process and wait with a timeout to avoid hanging
        // when Docker Desktop is installed but not running
        match Command::new("docker")
            .args(["version", "--format", "{{.Server.Version}}"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(mut child) => match wait_or_reap(&mut child, std::time::Duration::from_secs(3)) {
                Some(succeeded) => succeeded,
                // Timed out, or could not be polled: either way the child has
                // been killed and reaped, and Docker has not answered.
                None => {
                    warn!("docker version did not answer within 3s - Docker not available");
                    false
                }
            },
            Err(_) => false,
        }
    }

    /// Check if Docker is available and running
    ///
    /// Shares `DOCKER_PROBE` with the sync twin: both run the same
    /// `docker version` command, so either one's answer serves the other.
    ///
    /// DISPLAY CLASS ONLY, with the same split as `is_docker_available_sync`:
    /// anything that would leak a container on a stale "no" must go through
    /// `boss_cleanup_docker_gate` instead.
    async fn is_docker_available(&self) -> bool {
        docker_answer_or_probe_async(&DOCKER_PROBE, DOCKER_PROBE_TTL, Self::probe_docker_async)
            .await
    }

    /// Ask Docker itself, ignoring the cache. Reached only through
    /// `is_docker_available`, which owns the caching.
    async fn probe_docker_async() -> bool {
        // `tokio::process` rather than `std::process`: on timeout the future is
        // dropped, and `kill_on_drop` then signals the child and lets tokio's
        // reaper collect it. A `std::process::Child` dropped here would be left
        // running against a wedged Docker socket and orphaned to launchd when
        // the TUI exits, which is how 117 `com.docker.cli` processes piled up.
        let output = tokio::process::Command::new("docker")
            .args(["version", "--format", "{{.Server.Version}}"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .output();

        match tokio::time::timeout(std::time::Duration::from_secs(3), output).await {
            Ok(Ok(output)) => {
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
            Ok(Err(e)) => {
                warn!("Docker not found or not accessible: {}", e);
                false
            }
            Err(_) => {
                warn!("Docker version timed out after 3s - Docker not available");
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
            SessionAgentType::Antigravity => Some("antigravity"),
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
        // Only events within this rolling window can mark a session, and only
        // this many rows are read per refresh. Both bound query cost on a large
        // notifications DB; `[ui]` raises them for a very large fleet.
        let config = crate::config::tunables::snapshot();
        let lookback_ms = i64::from(config.ui.session_lookback_hours) * 60 * 60 * 1000;
        let query_limit = config.ui.session_query_limit;

        let db = ainb_plugin_notifyd::Paths::from_home().ok()?.db;
        if !db.exists() {
            return None;
        }
        let store = ainb_plugin_notifyd::Store::open(&db).ok()?;
        let floor = now_ms - lookback_ms;
        match store.recent_since(floor, query_limit) {
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

            // The selected session renders from the observer's vt100 screen,
            // so a parallel capture would waste work and rebuild terminal
            // text through the lossy legacy path.
            if is_selected && self.embed_session.as_deref() != Some(tmux_session.name()) {
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
        use crate::interactive::InteractiveSessionManager;
        use crate::models::session::SessionAgentType;

        let session = self
            .find_session(session_id)
            .ok_or_else(|| anyhow::anyhow!("Session not found"))?;

        let tmux_session_name = session
            .tmux_session_name
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No tmux session associated with this session"))?
            .clone();

        let workspace_path = session.workspace_path.clone();
        let agent_type = session.agent_type;

        let provider = match agent_type {
            SessionAgentType::Claude => CliProvider::Claude,
            SessionAgentType::Codex => CliProvider::Codex,
            SessionAgentType::Gemini => CliProvider::Gemini,
            SessionAgentType::Copilot => CliProvider::Copilot,
            SessionAgentType::Antigravity => CliProvider::Antigravity,
            SessionAgentType::Shell | SessionAgentType::Ssh | SessionAgentType::Kiro => {
                anyhow::bail!("Restart unsupported for agent type {:?}", agent_type);
            }
        };

        // Load persisted metadata once — used for both the resume-history probe
        // (Claude, keyed off the worktree cwd) and the Headroom routing flag.
        let store = crate::interactive::SessionStore::load();
        let metadata = store.sessions.get(&tmux_session_name);
        let skip_permissions =
            metadata.and_then(|m| m.skip_permissions).unwrap_or(session.skip_permissions);
        let model = metadata
            .and_then(crate::interactive::SessionMetadata::launch_model)
            .or_else(|| session.model.clone());

        // Reuse the launch/resume path: it replaces even a dead
        // remain-on-exit pane with `respawn-pane -k`, restores provider argv,
        // and reapplies Headroom/API-key/OTEL environment consistently.
        let resume_transcript = if agent_type == SessionAgentType::Claude {
            let worktree_path = metadata
                .map(|m| m.worktree_path.as_path())
                .unwrap_or_else(|| std::path::Path::new(&workspace_path));
            Self::find_latest_transcript(worktree_path)
        } else {
            None
        };
        let headroom_enabled = metadata.map(|m| m.headroom_enabled).unwrap_or(false);
        // `None` covers both "not a Codex session" and "shared remote control
        // is unavailable on this hangar home" (already warned about). Both
        // restart the session with plain provider argv.
        let mut codex_remote = if agent_type == SessionAgentType::Codex {
            crate::interactive::session_manager::ensure_codex_remote_thread(
                session_id,
                std::path::Path::new(&workspace_path),
                model.as_deref(),
                skip_permissions,
                headroom_enabled,
                metadata.and_then(|m| m.codex_thread_id.clone()),
            )
            .await?
        } else {
            None
        };

        info!(
            "Restarting {} in tmux session '{}' for workspace '{}'",
            provider.display_name(),
            tmux_session_name,
            workspace_path
        );

        InteractiveSessionManager::new()?
            .start_cli_in_tmux(
                &tmux_session_name,
                std::path::Path::new(&workspace_path),
                skip_permissions,
                model.clone(),
                agent_type,
                resume_transcript,
                true,
                headroom_enabled,
                codex_remote.as_ref(),
            )
            .await?;

        if codex_remote.as_ref().is_some_and(|remote| remote.thread_id.is_none()) {
            codex_remote = crate::interactive::session_manager::claim_codex_remote_thread(
                session_id,
                std::path::Path::new(&workspace_path),
                model.as_deref(),
                skip_permissions,
                headroom_enabled,
                &tmux_session_name,
            )
            .await?;
        }
        if let Some(thread_id) =
            codex_remote.as_ref().and_then(|remote| remote.thread_id.as_deref())
        {
            crate::interactive::session_manager::persist_codex_thread_id(
                session_id,
                thread_id.to_string(),
            )?;
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
    /// 4. Rebuild the provider command from persisted launch settings.
    ///    No env prefix (headroom is now off → `headroom_env_prefix(…, false)` == "").
    /// 5. Replace the running CLI with `tmux respawn-pane -k` using the same
    ///    `sh -c '…exec cli …'` shape as `start_cli_in_tmux`.
    ///    `respawn-pane -k` kills the running process and starts fresh in-place,
    ///    which is the only way to clear env vars from a running process.
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
        let session_skip_permissions = session.skip_permissions;
        let session_model = session.model.clone();

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

        // --- 2 + 3. Check and flip headroom_enabled in SessionStore ---
        //
        // Scoped block so the cross-process lock (pu4) is held ONLY across the
        // load-inspect-mutate-save window and released before the slow tmux
        // respawn below — never hold the sessions.json lock across async IO.
        // The lock is best-effort: on failure we proceed unlocked rather than
        // abort the downgrade. The early-return paths drop the guard (unlock)
        // as they leave the block.
        let (skip_permissions, model, has_history) = {
            let _lock = crate::interactive::SessionStore::lock()
                .map_err(|e| warn!("Failed to lock sessions.json for Headroom flip: {e}"))
                .ok();
            let mut store = crate::interactive::SessionStore::load();
            let launch_settings = match store.sessions.get(&tmux_session_name) {
                None => {
                    self.add_warning_notification(
                        "Session not found in store — Headroom state unknown".to_string(),
                    );
                    return Ok(());
                }
                Some(meta) if !meta.headroom_enabled => {
                    self.add_info_notification(
                        "Session is already direct (Headroom off)".to_string(),
                    );
                    return Ok(());
                }
                Some(meta) => (
                    meta.skip_permissions.unwrap_or(session_skip_permissions),
                    meta.launch_model().or(session_model),
                    agent_type == SessionAgentType::Claude
                        && Self::find_latest_transcript(&meta.worktree_path).is_some(),
                ),
            };

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
            launch_settings
        };

        // --- 4. Build the resume command (no env prefix — headroom is off) ---
        //
        // env_setup is intentionally empty: `headroom_env_prefix(…, false)` == ""
        // and we are not injecting an API key here (the original launch path
        // already injected it into the pane's environment; `respawn-pane -k`
        // inherits from the ainb-tui process which has the correct key).
        let cmd_parts =
            crate::interactive::session_manager::InteractiveSessionManager::build_cli_cmd_parts(
                &provider,
                agent_type,
                skip_permissions,
                model.as_deref(),
                true,
                has_history,
            );
        let cli_cmd = cmd_parts
            .iter()
            .map(|part| shell_escape::escape(part.into()).into_owned())
            .collect::<Vec<_>>()
            .join(" ");

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
            // restart picks it back up (best-effort, locked RMW — pu4).
            let _ = crate::interactive::SessionStore::mutate(|store2| {
                if let Some(meta) = store2.sessions.get_mut(&tmux_session_name) {
                    meta.headroom_enabled = true;
                }
            });
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
            "Headroom OFF for this session — resumed direct (no compression)"
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

/// How long a Docker answer is reused. Long enough that a wedged Docker costs
/// one 3s probe per window across every call site, short enough that starting
/// Docker Desktop is noticed without restarting the TUI.
const DOCKER_PROBE_TTL: Duration = Duration::from_secs(30);

/// The last Docker answer and the instant it was taken, shared by
/// `AppState::is_docker_available_sync` and `AppState::is_docker_available`.
/// One entry serves both because both run the same `docker version` probe.
///
/// The probe runs outside the lock, so two callers that miss together each
/// probe once and the later answer wins. That is the deliberate trade: holding
/// this lock across a 3s wait would stall every other call site behind the one
/// that is already paying for the wedged Docker.
static DOCKER_PROBE: Mutex<Option<(Instant, bool)>> = Mutex::new(None);

/// The cached answer, when one was taken less than `ttl` before `now`.
///
/// `None` means "probe anyway": the cache is empty, the entry has aged out, or
/// the lock is poisoned. A poisoned lock must never take the TUI down over a
/// cached boolean, and re-probing is always correct, only slower.
fn cached_docker_answer(
    now: Instant,
    cache: &Mutex<Option<(Instant, bool)>>,
    ttl: Duration,
) -> Option<bool> {
    let (taken_at, answer) = (*cache.lock().ok()?)?;
    (now.saturating_duration_since(taken_at) < ttl).then_some(answer)
}

/// Record `answer` as the Docker state observed at `now`.
///
/// A poisoned lock is dropped silently, for the same reason as above: losing a
/// cache entry costs one extra probe, panicking costs the session.
fn store_docker_answer(now: Instant, cache: &Mutex<Option<(Instant, bool)>>, answer: bool) {
    if let Ok(mut entry) = cache.lock() {
        *entry = Some((now, answer));
    }
}

/// Drop any cached Docker answer, so the next caller asks Docker again.
///
/// A poisoned lock needs nothing dropped, since `cached_docker_answer` already
/// reads it as "probe anyway".
fn invalidate_docker_probe_cache(cache: &Mutex<Option<(Instant, bool)>>) {
    if let Ok(mut entry) = cache.lock() {
        *entry = None;
    }
}

/// Whether Docker is available: the cached answer when one was taken inside
/// `ttl`, otherwise `probe`'s answer, which is then cached.
///
/// The lookup lives here rather than at each call site so it is exercised in
/// one place. A negative answer is cached too: a Docker that is down is
/// exactly the case that costs 3s a call, and the TTL bounds how long a
/// restarted Docker stays unnoticed (`invalidate_docker_probe_cache` shortens
/// that to nothing where the user asked for a retry).
fn docker_answer_or_probe(
    cache: &Mutex<Option<(Instant, bool)>>,
    ttl: Duration,
    probe: impl FnOnce() -> bool,
) -> bool {
    if let Some(cached) = cached_docker_answer(Instant::now(), cache, ttl) {
        return cached;
    }
    let answer = probe();
    store_docker_answer(Instant::now(), cache, answer);
    answer
}

/// `docker_answer_or_probe` for a probe that has to be awaited.
///
/// Takes a closure rather than a future because `tokio::process::Command`
/// spawns the child while building its future, and a cache hit must not spawn
/// a `docker version` it will then discard.
async fn docker_answer_or_probe_async<F, Fut>(
    cache: &Mutex<Option<(Instant, bool)>>,
    ttl: Duration,
    probe: F,
) -> bool
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    if let Some(cached) = cached_docker_answer(Instant::now(), cache, ttl) {
        return cached;
    }
    let answer = probe().await;
    store_docker_answer(Instant::now(), cache, answer);
    answer
}

/// `docker_answer_or_probe_async` for a check the user just asked for by
/// retrying, which must reach Docker rather than answer from the cache.
///
/// A "no" is cached like any other answer, so a user who read "start Docker
/// and try again", started Docker, and pressed the key again would otherwise
/// be told the same "no" for the rest of the TTL. Dropping the entry first is
/// what makes the retry a retry; the fresh answer is then cached as usual, so
/// the retry costs one probe rather than exempting the path from caching.
async fn docker_answer_for_retry<F, Fut>(
    cache: &Mutex<Option<(Instant, bool)>>,
    ttl: Duration,
    probe: F,
) -> bool
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    invalidate_docker_probe_cache(cache);
    docker_answer_or_probe_async(cache, ttl, probe).await
}

/// What a Docker-gated path does next, and what to tell the user when it can
/// go no further.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DockerGate {
    /// Docker answered "yes" just now, so the path continues.
    Proceed,
    /// Docker is not there now, not merely when it was last asked, and this is
    /// the message that says so.
    Blocked(String),
}

/// The Docker gate on launching a Boss-mode session.
///
/// Takes the cache and the probe rather than an already-computed `bool` so the
/// retry semantics live inside what a test drives: prime the cache with the
/// "no" the user was already shown, and the gate must still reach Docker. A
/// call site that answered the question for itself and handed a `bool` in
/// could quietly go back to replaying that "no", with nothing to notice it.
async fn boss_mode_docker_gate<F, Fut>(
    cache: &Mutex<Option<(Instant, bool)>>,
    ttl: Duration,
    probe: F,
) -> DockerGate
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    if docker_answer_for_retry(cache, ttl, probe).await {
        DockerGate::Proceed
    } else {
        DockerGate::Blocked(
            "Boss mode requires Docker.\n\nPlease start Docker and try again, or use Interactive mode instead."
                .to_string(),
        )
    }
}

/// The Docker gate on tearing a Boss-mode session's container down.
///
/// CLEANUP CLASS, and the reason that class exists. It reaches Docker through
/// `docker_answer_for_retry`, the same invalidate-then-probe seam the retry
/// gates use, instead of through the 30s cache. A "no" cached moments earlier
/// by a display path (a workspace refresh taken while Docker was still coming
/// up) would otherwise skip `delete_boss_session` outright, and nothing revisits
/// it: the session row is being deleted, so the container is leaked for good.
/// One 3s worst case per deletion is the price of not leaking.
///
/// Returns a bare `bool` rather than a `DockerGate`: there is no user to show a
/// message to and nothing to block. A genuine "Docker is down" here means the
/// container is already gone with the engine, so the deletion proceeds.
///
/// Takes the cache and the probe rather than an already-computed `bool` for the
/// same reason `boss_mode_docker_gate` does: prime the cache with the stale "no"
/// and the gate must still reach Docker, which is only checkable at this seam.
async fn boss_cleanup_docker_gate<F, Fut>(
    cache: &Mutex<Option<(Instant, bool)>>,
    ttl: Duration,
    probe: F,
) -> bool
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    docker_answer_for_retry(cache, ttl, probe).await
}

/// The Docker gate on running authentication setup, which needs a container to
/// run the OAuth flow in. Same seam and same reason as `boss_mode_docker_gate`:
/// re-running setup is the retry its message asks for.
async fn auth_setup_docker_gate<F, Fut>(
    cache: &Mutex<Option<(Instant, bool)>>,
    ttl: Duration,
    probe: F,
) -> DockerGate
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    if docker_answer_for_retry(cache, ttl, probe).await {
        DockerGate::Proceed
    } else {
        DockerGate::Blocked(
            "❌ Docker is not available\n\n\
             Please start Docker and try again."
                .to_string(),
        )
    }
}

/// Wait up to `timeout` for `child`, returning whether it exited successfully,
/// or `None` when it had to be killed.
///
/// The kill path also `wait()`s. A signalled child that is never waited on stays
/// a zombie held by this process, and one that outlives the signal (a `docker
/// info` blocked on an unresponsive Docker socket) reparents to launchd when we
/// exit: 117 such `com.docker.cli` processes, aged up to four days, were reaped
/// from one machine. Issue #785.
fn wait_or_reap(child: &mut std::process::Child, timeout: std::time::Duration) -> Option<bool> {
    /// Kill the child and reap it. A signalled child that is never waited on
    /// stays a zombie; one that outlives the signal reparents to launchd when
    /// we exit.
    fn kill_and_reap(child: &mut std::process::Child) {
        if let Err(e) = child.kill() {
            warn!("could not kill probe (pid {}): {}", child.id(), e);
        }
        // SIGKILL cannot be ignored, so this returns as soon as the kernel has
        // torn the child down.
        if let Err(e) = child.wait() {
            warn!("could not reap probe: {}", e);
        }
    }

    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status.success()),
            Ok(None) => {
                if start.elapsed() > timeout {
                    kill_and_reap(child);
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            // A failed `try_wait` says nothing about the child, so it is still
            // running and still ours to clean up. Returning here without the
            // kill leaves exactly the orphan this function exists to prevent.
            Err(e) => {
                warn!("could not poll probe (pid {}): {}", child.id(), e);
                kill_and_reap(child);
                return None;
            }
        }
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
    /// In-flight `plugin/render` outcome receivers, keyed by screen id.
    ///
    /// The frame itself comes back via `RuntimeHandle::try_recv_render`, so
    /// this oneshot used to be dropped on the spot — which threw away the
    /// FAILURE half of the result. A lazy plugin whose subprocess can no
    /// longer be spawned answers every kick with
    /// `RenderOutcome::RuntimeError`, and with the receiver dropped that
    /// error went nowhere: the screen sat on "connecting…" forever while
    /// key and mouse events were silently dropped (`child.is_none()`).
    /// Holding the receiver for one tick and polling it with `try_recv`
    /// keeps `tick_plugin_renders` synchronous while letting the failure
    /// reach `state.plugin_render_errors` and the user.
    plugin_render_outcomes: std::collections::HashMap<
        crate::app::screens::ScreenId,
        tokio::sync::oneshot::Receiver<ainb_plugin_runtime::RenderOutcome>,
    >,
}

impl App {
    pub fn new() -> Self {
        Self {
            state: AppState::new(),
            plugin_runtime_owner: None,
            usage_dir_watcher: None,
            plugin_render_outcomes: std::collections::HashMap::new(),
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

            // Refresh the text-capture flag from the plugin's last frame every
            // tick (one atomic load; `false` for an unregistered plugin). The
            // host key dispatch reads this for the focused screen to suppress
            // its global single-char shortcuts while a plugin input is focused
            // (8hx). Done before the lifecycle skip so an unregistered plugin's
            // stale flag is cleared to false rather than lingering true.
            self.state
                .plugin_captures_text
                .insert((*screen_id).to_string(), handle.captures_text(&pid));

            // Skip plugins the runtime doesn't know about — keeps the
            // loop cheap and resilient when discovery comes up empty.
            if handle.lifecycle_state(&pid).is_none() {
                continue;
            }

            // Collect the previous kick's outcome before issuing another one.
            // Only the failure half matters here (the frame arrives via
            // `try_recv_render` above), and it MUST be collected: a lazy
            // plugin whose binary is gone answers every kick with
            // `RuntimeError`, and dropping that receiver is what left the
            // screen on "connecting…" indefinitely with no log line.
            self.collect_plugin_render_outcome(screen_id);

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
            // (stashed by `PluginScreen::render`); (0, 0) means that render
            // hasn't happened yet.
            let (width, height) =
                self.state.plugin_render_areas.get(*screen_id).copied().unwrap_or((0, 0));

            // No allocated area stashed yet — the very first entry to this
            // screen, before `PluginScreen::render` has run once. Kicking now
            // would render at the plugin's 80×24 fallback and paint that
            // mostly-void frame across the real (larger) area: the "blank
            // screen" flash on hangar entry. Skip WITHOUT consuming the dirty
            // flag; this draw stashes the real area and the next tick kicks
            // at full size, while the loading placeholder covers the gap.
            if width == 0 || height == 0 {
                continue;
            }

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
            // The frame lands in the cache for `try_recv_render`; the
            // returned oneshot carries the outcome, and specifically the
            // failure case that cache can never represent. Park it until
            // the next tick's `collect_plugin_render_outcome`.
            let rx = handle.render(&pid, viewport, 0);
            self.plugin_render_outcomes.insert((*screen_id).to_string(), rx);
        }
        drained
    }

    /// Poll the parked `plugin/render` oneshot for `screen_id`, recording any
    /// failure in `state.plugin_render_errors` (and clearing it on success).
    ///
    /// Non-blocking by construction — `try_recv` never awaits, so
    /// `tick_plugin_renders` stays synchronous per its `build.rs`-enforced
    /// contract. A receiver that is still `Empty` is put back, so a render that
    /// outlives one tick is judged on a later tick rather than abandoned
    /// immediately — but only until the next kick for that screen replaces it.
    /// Superseding is deliberate: the newer kick reports the current state of
    /// the same plugin, so a persistent failure is still caught on the very
    /// next tick, and a failure that has since been fixed is not worth
    /// resurrecting.
    fn collect_plugin_render_outcome(&mut self, screen_id: &str) {
        use ainb_plugin_runtime::RenderOutcome;
        use tokio::sync::oneshot::error::TryRecvError;

        let Some(mut rx) = self.plugin_render_outcomes.remove(screen_id) else {
            return;
        };
        let message = match rx.try_recv() {
            Ok(RenderOutcome::Ok(_)) => {
                self.state.plugin_render_errors.remove(screen_id);
                return;
            }
            Ok(RenderOutcome::RuntimeError(e)) => e,
            Ok(RenderOutcome::PluginError { code, message }) => {
                format!("{message} (code {code})")
            }
            Err(TryRecvError::Empty) => {
                // Still rendering. Keep waiting rather than treating a slow
                // frame as a failure.
                self.plugin_render_outcomes.insert(screen_id.to_string(), rx);
                return;
            }
            // Sender dropped without answering: the plugin task is gone.
            Err(TryRecvError::Closed) => "plugin task stopped without answering".to_string(),
        };

        // Log once per distinct message so a failing screen doesn't spam the
        // log at tick cadence while the user sits on it.
        let is_new = self.state.plugin_render_errors.get(screen_id) != Some(&message);
        if is_new {
            warn!(screen = %screen_id, error = %message, "plugin render failed");
        }
        self.state.plugin_render_errors.insert(screen_id.to_string(), message);
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
        //
        // DISPLAY CLASS: cached. Startup, and the cache is empty here anyway,
        // so this is the probe every other startup call site then reuses.
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
            // DISPLAY CLASS: cached, and deliberately the same answer the log
            // streaming check above just took - two 3s probes in the startup
            // path is the stall the cache exists to remove. A stale "no" defers
            // the refresh to the periodic check.
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
        // Check for a completed remote-repo pre-flight (Configure screen)
        if self.state.check_repo_check_complete() {
            self.state.ui_needs_refresh = true;
        }
        // Check for a completed empty-remote initialization ([i] on Configure)
        if self.state.check_repo_init_complete() {
            self.state.ui_needs_refresh = true;
        }

        // Drain + lazily refresh the MCP pool overlay (no-op when closed).
        self.state.check_mcp_overlay();
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

                    // Only attempt refresh if Docker is available.
                    //
                    // DISPLAY CLASS: cached. This check itself repeats every 5
                    // minutes, which is ten times the cache TTL, so a stale
                    // "no" can only ever cost one cycle.
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
    fn dirty_plugin_kick_deferred_until_viewport_known() {
        let (runtime, mut app) = app_with_plugins(&["learnings"]);
        let handle = app.state.plugin_runtime.clone().expect("handle wired");
        let pid = PluginId::from("learnings");

        // Ticks while hidden: gated, dirty preserved (proved above).
        app.state.current_screen = ids::SESSION_LIST.to_string();
        app.tick_plugin_renders();

        // User opens the learnings screen. No allocated area is stashed yet,
        // so the tick must NOT kick: a (0, 0) seed kick made the plugin paint
        // its 80×24 fallback across the real (larger) area — the blank-flash
        // bug on first entry.
        app.state.current_screen = ids::LEARNINGS.to_string();
        app.tick_plugin_renders();
        assert!(
            !app.state.plugin_last_render_viewport.contains_key(ids::LEARNINGS),
            "no render kick before the real viewport is known"
        );

        // The draw pass stashes the allocated area (what `PluginScreen::render`
        // does) → the next tick kicks at full size and consumes the flag.
        app.state.plugin_render_areas.insert(ids::LEARNINGS.to_string(), (120, 40));
        app.tick_plugin_renders();
        assert_eq!(
            app.state.plugin_last_render_viewport.get(ids::LEARNINGS),
            Some(&(120, 40)),
            "first tick with a known viewport must kick at the real size"
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
        // Focused screen has painted once (area known); the hidden one hasn't.
        app.state.plugin_render_areas.insert(ids::LEARNINGS.to_string(), (100, 30));
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

    /// A lazy plugin only execs its binary on first use, so one that vanished
    /// after discovery (`brew upgrade` deleting the keg a running TUI was
    /// launched from) fails at the render kick. The render oneshot used to be
    /// dropped, so that failure reached nobody: the screen sat on
    /// "connecting…" forever while key and mouse events were dropped. The
    /// outcome must land in `plugin_render_errors` for the placeholder to
    /// paint instead.
    #[test]
    fn unspawnable_plugin_records_a_render_error() {
        // `app_with_plugins` registers against /nonexistent/plugin-binary,
        // which is exactly the post-upgrade state.
        let (runtime, mut app) = app_with_plugins(&["learnings"]);

        app.state.current_screen = ids::LEARNINGS.to_string();
        app.state.plugin_render_areas.insert(ids::LEARNINGS.to_string(), (120, 40));

        // First tick kicks the render; the spawn attempt and its failure
        // happen on the runtime's executor, so poll a bounded number of
        // ticks for the outcome rather than assuming one is enough.
        let mut recorded = None;
        for _ in 0..200 {
            app.tick_plugin_renders();
            if let Some(err) = app.state.plugin_render_errors.get(ids::LEARNINGS) {
                recorded = Some(err.clone());
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        let err = recorded.expect(
            "a plugin whose binary cannot be spawned must record a render error, \
             not leave the screen on the loading placeholder forever",
        );
        assert!(
            !err.is_empty(),
            "the recorded render error must carry a message to show the user"
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

#[cfg(test)]
mod seeded_category_tests {
    use super::*;

    /// Categories that describe something already IN FORCE, and so must render
    /// on a machine that has configured nothing.
    ///
    /// `McpServers` is deliberately absent: its rows describe user-created
    /// servers, and having none until you add one is the honest state. The
    /// three below are not like that. The ACP built-ins are spawning sessions
    /// right now and the Hangar daemon knobs are governing it, but neither
    /// lives in config.toml, so unless the seed plants them `expand_key`
    /// returns nothing and the category is dropped for having zero rows.
    const ALWAYS_REACHABLE: &[ConfigCategory] = &[
        ConfigCategory::Acp,
        ConfigCategory::HangarDaemon,
        ConfigCategory::ContainerTemplates,
    ];

    /// A category whose subject already exists must be openable out of the box.
    ///
    /// `every_category_has_at_least_one_row` in the registry proves each
    /// category has an ENTRY; it cannot prove the entry expands. Before
    /// `seed_builtin_acp_adapters` this fails with
    /// `these categories seed no rows and can never be opened: ["ACP Adapters"]`.
    #[test]
    fn categories_describing_live_things_seed_rows_out_of_the_box() {
        let seed = seed_value(&AppConfig::default());
        let rows = crate::config::screen_model::build_rows(&seed);
        let empty: Vec<&str> = ALWAYS_REACHABLE
            .iter()
            .filter(|category| rows.get(category).is_none_or(|r| r.is_empty()))
            .map(|category| category.label())
            .collect();
        assert!(
            empty.is_empty(),
            "these categories seed no rows and can never be opened: {empty:?}"
        );
    }

    /// The built-in ACP adapters are reachable by name, not just as a count.
    #[test]
    fn the_builtin_acp_adapters_get_rows() {
        let seed = seed_value(&AppConfig::default());
        let rows = crate::config::screen_model::build_rows(&seed);
        let keys: Vec<&str> = rows
            .get(&ConfigCategory::Acp)
            .expect("the ACP category has rows")
            .iter()
            .map(|row| row.key.as_str())
            .collect();
        for adapter in ["claude-agent-acp", "codex-acp"] {
            assert!(
                keys.iter().any(|key| key.contains(adapter)),
                "no row for the built-in adapter {adapter}: {keys:?}"
            );
        }
    }

    /// A user-declared adapter's own values survive the seeding, which only
    /// fills in built-ins the config has not already described.
    #[test]
    fn a_configured_adapter_is_not_overwritten_by_the_builtin_seed() {
        let mut config = AppConfig::default();
        config.acp.adapters.insert(
            "claude-agent-acp".to_string(),
            crate::config::AcpAdapterConfig {
                command: Some("/opt/mine".to_string()),
                permission_mode: "plan".to_string(),
            },
        );
        let seed = seed_value(&config);
        assert_eq!(
            crate::config::registry::navigate_toml(&seed, "acp.adapters.claude-agent-acp.command")
                .unwrap()
                .as_str(),
            Some("/opt/mine")
        );
    }
}

#[cfg(test)]
mod docker_probe_reap_tests {
    //! Issue #785: the probe's timeout path killed its child without reaping
    //! it, leaving a zombie, and a child that outlived the signal orphaned to
    //! launchd. Repeated probes against a Docker that never answers must leave
    //! nothing behind.

    use std::time::Duration;

    use super::wait_or_reap;

    /// The `ps` state letter for `pid`, or `None` when no such process exists.
    /// A survivor reads `S`/`R`; a killed-but-unreaped child reads `Z`.
    fn process_state(pid: u32) -> Option<String> {
        let output = std::process::Command::new("ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .expect("run ps");
        let state = String::from_utf8_lossy(&output.stdout).trim().to_string();
        (!state.is_empty()).then_some(state)
    }

    /// Stands in for `docker info` against a wedged socket: a child that will
    /// not answer inside the probe's budget. Every pid asserted on below is one
    /// this test spawned itself.
    fn unresponsive_probe() -> std::process::Child {
        std::process::Command::new("/bin/sleep")
            .arg("30")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn probe stand-in")
    }

    #[test]
    fn repeated_timed_out_probes_leave_no_child_and_no_zombie() {
        let mut pids = Vec::new();
        for _ in 0..5 {
            let mut child = unresponsive_probe();
            pids.push(child.id());
            assert_eq!(
                wait_or_reap(&mut child, Duration::from_millis(100)),
                None,
                "an unresponsive probe must report a timeout"
            );
        }

        for pid in pids {
            let state = process_state(pid);
            assert!(
                state.is_none(),
                "pid {pid} outlived the probe (ps state {state:?})"
            );
        }
    }

    #[test]
    fn a_probe_that_answers_in_time_reports_its_status() {
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .spawn()
            .expect("spawn probe");
        assert_eq!(wait_or_reap(&mut child, Duration::from_secs(5)), Some(true));

        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "exit 1"])
            .spawn()
            .expect("spawn probe");
        assert_eq!(
            wait_or_reap(&mut child, Duration::from_secs(5)),
            Some(false)
        );
    }
}

#[cfg(test)]
mod docker_probe_cache_tests {
    //! The TTL cache both Docker probes consult, and the seam they consult it
    //! through. Nothing here shells out to docker: the boundary cases run
    //! against explicit `Instant`s so the TTL is exact and the tests cost no
    //! wall-clock time, and the seam runs against a counting stand-in probe so
    //! a lost cache lookup shows up as an extra call rather than as nothing.

    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use super::{
        DOCKER_PROBE_TTL, DockerGate, auth_setup_docker_gate, boss_cleanup_docker_gate,
        boss_mode_docker_gate, cached_docker_answer, docker_answer_for_retry,
        docker_answer_or_probe, docker_answer_or_probe_async, invalidate_docker_probe_cache,
        store_docker_answer,
    };

    const TTL: Duration = Duration::from_secs(30);

    #[test]
    fn an_empty_cache_asks_the_caller_to_probe() {
        let cache = Mutex::new(None);
        assert_eq!(cached_docker_answer(Instant::now(), &cache, TTL), None);
    }

    #[test]
    fn an_entry_inside_the_ttl_is_reused() {
        for answer in [true, false] {
            let taken_at = Instant::now();
            let cache = Mutex::new(Some((taken_at, answer)));
            assert_eq!(
                cached_docker_answer(taken_at, &cache, TTL),
                Some(answer),
                "an entry taken this instant must be reused"
            );
            assert_eq!(
                cached_docker_answer(taken_at + TTL - Duration::from_millis(1), &cache, TTL),
                Some(answer),
                "an entry one millisecond short of the TTL must be reused"
            );
        }
    }

    #[test]
    fn an_entry_at_or_past_the_ttl_is_not_reused() {
        let taken_at = Instant::now();
        let cache = Mutex::new(Some((taken_at, true)));
        assert_eq!(
            cached_docker_answer(taken_at + TTL, &cache, TTL),
            None,
            "the TTL is exclusive: an entry exactly that old has expired"
        );
        assert_eq!(
            cached_docker_answer(taken_at + TTL + Duration::from_secs(1), &cache, TTL),
            None
        );
    }

    #[test]
    fn a_stored_answer_is_what_the_next_reader_sees() {
        let cache = Mutex::new(None);
        let taken_at = Instant::now();

        store_docker_answer(taken_at, &cache, true);
        assert_eq!(cached_docker_answer(taken_at, &cache, TTL), Some(true));

        // A later probe replaces the entry rather than ageing alongside it.
        let later = taken_at + TTL + Duration::from_secs(1);
        store_docker_answer(later, &cache, false);
        assert_eq!(cached_docker_answer(later, &cache, TTL), Some(false));
    }

    #[test]
    fn a_poisoned_lock_degrades_to_probing_instead_of_panicking() {
        let cache = Arc::new(Mutex::new(Some((Instant::now(), true))));

        let poisoner = Arc::clone(&cache);
        let panicked = std::thread::spawn(move || {
            let _held = poisoner.lock().expect("lock is healthy until we panic");
            panic!("poison the docker probe cache");
        })
        .join();
        assert!(panicked.is_err(), "the poisoning thread must have panicked");
        assert!(cache.is_poisoned(), "the lock must now be poisoned");

        assert_eq!(
            cached_docker_answer(Instant::now(), &cache, TTL),
            None,
            "a poisoned lock must read as 'probe anyway', not panic"
        );
        // And the write side must survive it too.
        store_docker_answer(Instant::now(), &cache, false);
    }

    #[test]
    fn the_shipped_ttl_is_the_window_an_answer_is_reused_for() {
        // Driven with the shipped constant rather than the local one, so this
        // pins what `DOCKER_PROBE_TTL` does rather than what it equals.
        let taken_at = Instant::now();
        let cache = Mutex::new(Some((taken_at, true)));

        assert_eq!(
            cached_docker_answer(
                taken_at + DOCKER_PROBE_TTL - Duration::from_millis(1),
                &cache,
                DOCKER_PROBE_TTL,
            ),
            Some(true),
            "an answer taken inside the shipped window must be reused"
        );
        assert_eq!(
            cached_docker_answer(taken_at + DOCKER_PROBE_TTL, &cache, DOCKER_PROBE_TTL),
            None,
            "an answer as old as the shipped window must be re-probed"
        );

        // And the window is short enough that a user who starts Docker Desktop
        // is not stuck with a stale "no" for the rest of the session.
        assert!(
            DOCKER_PROBE_TTL <= Duration::from_secs(60),
            "a longer window would strand a user who just started Docker"
        );
    }

    /// A probe stand-in that records how many times it was asked and answers
    /// differently each time, so a reused answer is distinguishable from a
    /// fresh one.
    fn counted_probe(calls: &AtomicUsize) -> bool {
        calls.fetch_add(1, Ordering::SeqCst) == 0
    }

    #[test]
    fn a_second_call_inside_the_ttl_never_reaches_docker() {
        let cache = Mutex::new(None);
        let calls = AtomicUsize::new(0);

        assert!(docker_answer_or_probe(&cache, TTL, || counted_probe(
            &calls
        )));
        assert_eq!(calls.load(Ordering::SeqCst), 1, "a cold cache must probe");

        assert!(
            docker_answer_or_probe(&cache, TTL, || counted_probe(&calls)),
            "the second call must return the first probe's answer"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "a call inside the TTL must not probe again"
        );
    }

    #[test]
    fn a_call_past_the_ttl_probes_again() {
        // A zero TTL ages an entry out the instant it is stored, which is the
        // expired branch without a test that waits out the shipped 30s.
        let cache = Mutex::new(None);
        let calls = AtomicUsize::new(0);

        assert!(docker_answer_or_probe(&cache, Duration::ZERO, || {
            counted_probe(&calls)
        }));
        assert!(
            !docker_answer_or_probe(&cache, Duration::ZERO, || counted_probe(&calls)),
            "an expired entry must be replaced by the fresh answer, not reused"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn the_async_probe_follows_the_same_policy() {
        let cache = Mutex::new(None);
        let calls = AtomicUsize::new(0);

        assert!(
            docker_answer_or_probe_async(&cache, TTL, || async { counted_probe(&calls) }).await
        );
        assert!(
            docker_answer_or_probe_async(&cache, TTL, || async { counted_probe(&calls) }).await,
            "the async twin must answer from the cache the sync twin filled"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "a cache hit must not spawn a probe"
        );

        assert!(
            !docker_answer_or_probe_async(&cache, Duration::ZERO, || async {
                counted_probe(&calls)
            })
            .await
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn an_answer_from_the_sync_path_serves_the_async_path() {
        // The claim that makes one static correct for both probes: they ask
        // Docker the same question, so either one's answer is the other's.
        let cache = Mutex::new(None);
        let calls = AtomicUsize::new(0);

        assert!(docker_answer_or_probe(&cache, TTL, || counted_probe(
            &calls
        )));
        assert!(
            docker_answer_or_probe_async(&cache, TTL, || async { counted_probe(&calls) }).await,
            "the async path must reuse the answer the sync path stored"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "the async path must not re-probe what the sync path just answered"
        );
    }

    #[tokio::test]
    async fn an_answer_from_the_async_path_serves_the_sync_path() {
        let cache = Mutex::new(None);
        let calls = AtomicUsize::new(0);

        assert!(
            docker_answer_or_probe_async(&cache, TTL, || async { counted_probe(&calls) }).await
        );
        assert!(
            docker_answer_or_probe(&cache, TTL, || counted_probe(&calls)),
            "the sync path must reuse the answer the async path stored"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "the sync path must not re-probe what the async path just answered"
        );
    }

    #[test]
    fn invalidation_drops_a_stored_answer() {
        let cache = Mutex::new(None);
        store_docker_answer(Instant::now(), &cache, false);
        assert_eq!(
            cached_docker_answer(Instant::now(), &cache, TTL),
            Some(false),
            "the answer must be cached before invalidation can drop it"
        );

        invalidate_docker_probe_cache(&cache);

        assert_eq!(
            cached_docker_answer(Instant::now(), &cache, TTL),
            None,
            "after invalidation the next caller must probe Docker again"
        );
    }

    #[tokio::test]
    async fn a_retry_asks_docker_instead_of_replaying_a_cached_no() {
        // The user-visible bug this guards: told to start Docker, the user
        // starts it and retries, and the fresh "yes" must win over the "no"
        // cached moments earlier.
        let cache = Mutex::new(Some((Instant::now(), false)));

        let probed = AtomicUsize::new(0);
        let answer = docker_answer_for_retry(&cache, TTL, || async {
            probed.fetch_add(1, Ordering::SeqCst);
            true
        })
        .await;

        assert!(answer, "a retry must report what Docker says now");
        assert_eq!(
            probed.load(Ordering::SeqCst),
            1,
            "a retry must reach Docker even with an answer cached inside the TTL"
        );
        assert_eq!(
            cached_docker_answer(Instant::now(), &cache, TTL),
            Some(true),
            "the retry's answer must replace the stale one for later callers"
        );
    }

    #[tokio::test]
    async fn a_cleanup_asks_docker_instead_of_replaying_a_cached_no() {
        // The container-leak this guards: a display path caches "no" while
        // Docker is still coming up, the user deletes the session seconds
        // later, and the Boss-mode teardown reads that cached "no" and skips
        // container removal. Unlike a display path there is no next time - the
        // session row is gone, so the container is orphaned for good.
        let cache = Mutex::new(Some((Instant::now(), false)));

        let probed = AtomicUsize::new(0);
        let available = boss_cleanup_docker_gate(&cache, TTL, || async {
            probed.fetch_add(1, Ordering::SeqCst);
            true
        })
        .await;

        assert!(
            available,
            "cleanup must act on the Docker that is there now, not the one cached moments ago"
        );
        assert_eq!(
            probed.load(Ordering::SeqCst),
            1,
            "cleanup must reach Docker even with an answer cached inside the TTL"
        );
        assert_eq!(
            cached_docker_answer(Instant::now(), &cache, TTL),
            Some(true),
            "the cleanup's answer must replace the stale one for later callers"
        );
    }

    #[tokio::test]
    async fn a_cleanup_that_finds_no_docker_still_reports_it() {
        // The gate is not a veto: a genuine "Docker is down" means the
        // container went down with the engine, and the deletion proceeds
        // without a Boss-mode teardown. What must never happen is reporting
        // that from the cache instead of from Docker.
        let cache = Mutex::new(Some((Instant::now(), true)));

        let probed = AtomicUsize::new(0);
        let available = boss_cleanup_docker_gate(&cache, TTL, || async {
            probed.fetch_add(1, Ordering::SeqCst);
            false
        })
        .await;

        assert!(!available, "a cleanup must report what Docker says now");
        assert_eq!(
            probed.load(Ordering::SeqCst),
            1,
            "a cached yes must not stand in for the probe either"
        );
    }

    #[tokio::test]
    async fn a_check_after_a_retry_answers_from_the_retry() {
        // The retry drops the cache once, it does not exempt the path from
        // caching: the next ordinary check inside the TTL still costs nothing.
        let cache = Mutex::new(None);
        let calls = AtomicUsize::new(0);

        assert!(docker_answer_for_retry(&cache, TTL, || async { counted_probe(&calls) }).await);
        assert!(docker_answer_or_probe(&cache, TTL, || counted_probe(
            &calls
        )));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "only the retry itself may probe"
        );
    }

    /// A probe that must not be reached, so a gate answering from the cache
    /// fails loudly instead of quietly.
    async fn never_probed() -> bool {
        panic!("the gate answered from the cache instead of asking Docker");
    }

    #[tokio::test]
    async fn the_boss_mode_gate_asks_docker_rather_than_replaying_the_no_the_user_saw() {
        // The user read "start Docker and try again", started Docker, and
        // pressed launch. The cached "no" is exactly the answer the gate must
        // not give back.
        let cache = Mutex::new(Some((Instant::now(), false)));
        let probed = AtomicUsize::new(0);

        let gate = boss_mode_docker_gate(&cache, TTL, || async {
            probed.fetch_add(1, Ordering::SeqCst);
            true
        })
        .await;

        assert_eq!(
            gate,
            DockerGate::Proceed,
            "an explicit retry must report what Docker says now"
        );
        assert_eq!(
            probed.load(Ordering::SeqCst),
            1,
            "the gate must reach Docker even with an answer cached inside the TTL"
        );
    }

    #[tokio::test]
    async fn the_boss_mode_gate_blocks_with_the_message_that_asks_for_the_retry() {
        let cache = Mutex::new(None);

        let DockerGate::Blocked(message) =
            boss_mode_docker_gate(&cache, TTL, || async { false }).await
        else {
            panic!("a gate with no Docker behind it must block");
        };

        assert!(
            message.contains("Boss mode requires Docker"),
            "the message must name what needs Docker: {message}"
        );
        assert!(
            message.contains("start Docker and try again"),
            "the message must ask for the retry the gate then honours: {message}"
        );
    }

    #[tokio::test]
    async fn the_auth_setup_gate_asks_docker_rather_than_replaying_the_no_the_user_saw() {
        let cache = Mutex::new(Some((Instant::now(), false)));
        let probed = AtomicUsize::new(0);

        let gate = auth_setup_docker_gate(&cache, TTL, || async {
            probed.fetch_add(1, Ordering::SeqCst);
            true
        })
        .await;

        assert_eq!(
            gate,
            DockerGate::Proceed,
            "re-running setup is a retry, so it must report what Docker says now"
        );
        assert_eq!(
            probed.load(Ordering::SeqCst),
            1,
            "the gate must reach Docker even with an answer cached inside the TTL"
        );
    }

    #[tokio::test]
    async fn the_auth_setup_gate_blocks_with_the_message_that_asks_for_the_retry() {
        let cache = Mutex::new(None);

        let DockerGate::Blocked(message) =
            auth_setup_docker_gate(&cache, TTL, || async { false }).await
        else {
            panic!("a gate with no Docker behind it must block");
        };

        assert!(
            message.contains("Docker is not available"),
            "the message must say what is missing: {message}"
        );
        assert!(
            message.contains("start Docker and try again"),
            "the message must ask for the retry the gate then honours: {message}"
        );
    }

    #[tokio::test]
    async fn a_gate_that_passes_leaves_its_answer_for_the_next_ordinary_check() {
        // The gate drops the cache once, it does not exempt the path from
        // caching: the ordinary check that follows costs nothing.
        let cache = Mutex::new(None);

        assert_eq!(
            boss_mode_docker_gate(&cache, TTL, || async { true }).await,
            DockerGate::Proceed
        );
        assert!(
            docker_answer_or_probe_async(&cache, TTL, never_probed).await,
            "the check after the gate must answer from the gate's probe"
        );
    }
}

#[cfg(test)]
mod docker_probe_shared_static_test {
    //! One test, deliberately alone in this module, run against the shipped
    //! `DOCKER_PROBE` static rather than a locally built cache.
    //!
    //! Every other cache test drives a local `Mutex` precisely so it cannot
    //! collide with the rest of this multi-threaded binary, which leaves one
    //! claim unpinned: that `is_docker_available_sync` really consults the
    //! static the async twin consults, and not a cache of its own. That can
    //! only be checked against the static itself, so it is checked once, here,
    //! by the only test that touches it, and the static is emptied on both
    //! sides of the check so nothing else in the binary inherits an answer.
    //!
    //! The check primes the static instead of letting a real probe fill it.
    //! Running `docker version` for real against a wedged Docker is the 3s
    //! stall and the orphaned `com.docker.cli` that issue #785 is about: the
    //! CLI re-spawns itself, so killing the direct child still leaves copies
    //! reparented to launchd. A unit test must not add those to a developer's
    //! machine. What the sync probe writes on a miss is `docker_answer_or_probe`'s
    //! half, pinned on a local cache in `docker_probe_cache_tests`.

    use std::time::Instant;

    use super::{AppState, DOCKER_PROBE, invalidate_docker_probe_cache, store_docker_answer};

    #[test]
    fn the_sync_probe_answers_from_the_shared_static() {
        invalidate_docker_probe_cache(&DOCKER_PROBE);

        // Both answers, so a probe wired to a cache of its own cannot pass by
        // coincidentally agreeing with whatever this machine's Docker says.
        for primed in [true, false] {
            store_docker_answer(Instant::now(), &DOCKER_PROBE, primed);
            assert_eq!(
                AppState::is_docker_available_sync(),
                primed,
                "the sync probe must answer from DOCKER_PROBE, the static the async twin reads"
            );
        }

        invalidate_docker_probe_cache(&DOCKER_PROBE);
    }
}
