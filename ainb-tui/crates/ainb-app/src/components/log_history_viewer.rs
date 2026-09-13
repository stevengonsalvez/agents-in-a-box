// ABOUTME: Renderer-agnostic half of the `log_history_viewer` component: its
// state types and the logic that does not draw. The renderer lives in
// `ainb-core::components::log_history_viewer`, which re-exports this module.

use super::live_logs_stream::{LogEntry, LogEntryLevel};
use super::log_reader::{AppLogInfo, JsonlLogReader};
use std::path::PathBuf;

/// Convert a character index to a byte index in a UTF-8 string
/// Returns the byte offset of the nth character, or the string length if n exceeds char count
pub fn char_to_byte_index(s: &str, char_idx: usize) -> usize {
    s.char_indices().nth(char_idx).map(|(byte_idx, _)| byte_idx).unwrap_or(s.len())
}

/// Focus area within the log viewer
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogViewerFocus {
    SessionList,
    LogEntries,
}

/// Filter level for log display
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFilterLevel {
    All,
    Info,
    Warn,
    Error,
}

impl LogFilterLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::All => "ALL",
            Self::Info => "INFO+",
            Self::Warn => "WARN+",
            Self::Error => "ERROR",
        }
    }

    pub fn next(&self) -> Self {
        match self {
            Self::All => Self::Info,
            Self::Info => Self::Warn,
            Self::Warn => Self::Error,
            Self::Error => Self::All,
        }
    }

    pub fn matches(&self, level: LogEntryLevel) -> bool {
        match self {
            Self::All => true,
            Self::Info => !matches!(level, LogEntryLevel::Debug),
            Self::Warn => matches!(level, LogEntryLevel::Warn | LogEntryLevel::Error),
            Self::Error => matches!(level, LogEntryLevel::Error),
        }
    }
}

/// Summary of a log file for display
#[derive(Debug, Clone)]
pub struct SessionLogSummary {
    /// Filename (e.g., "agents-in-a-box-20260107-001310.jsonl")
    pub filename: String,
    /// Display name (e.g., "2026-01-07 00:13:10")
    pub display_name: String,
    /// Full path to the log file
    pub log_path: PathBuf,
    /// Number of log entries
    pub log_count: usize,
    /// Count of error-level logs
    pub error_count: usize,
    /// Count of warning-level logs
    pub warn_count: usize,
    /// Whether this is a JSONL file
    pub is_jsonl: bool,
}

impl From<AppLogInfo> for SessionLogSummary {
    fn from(info: AppLogInfo) -> Self {
        Self {
            filename: info.filename,
            display_name: info.display_name,
            log_path: info.log_path,
            log_count: info.log_count,
            error_count: info.error_count,
            warn_count: info.warn_count,
            is_jsonl: info.is_jsonl,
        }
    }
}

/// Text selection state for copy functionality
#[derive(Debug, Clone, Default)]
pub struct TextSelection {
    /// Start position (line index, char offset)
    pub start: Option<(usize, usize)>,
    /// End position (line index, char offset)
    pub end: Option<(usize, usize)>,
    /// Whether a drag is in progress
    pub is_selecting: bool,
    /// Cached selected text
    pub selected_text: Option<String>,
}

impl TextSelection {
    pub fn clear(&mut self) {
        self.start = None;
        self.end = None;
        self.is_selecting = false;
        self.selected_text = None;
    }

    pub fn has_selection(&self) -> bool {
        self.start.is_some() && self.end.is_some()
    }

    /// Get normalized selection range (start <= end)
    pub fn normalized(&self) -> Option<((usize, usize), (usize, usize))> {
        match (self.start, self.end) {
            (Some(start), Some(end)) => {
                if start.0 < end.0 || (start.0 == end.0 && start.1 <= end.1) {
                    Some((start, end))
                } else {
                    Some((end, start))
                }
            }
            _ => None,
        }
    }
}

/// State for the log history viewer
#[derive(Debug)]
pub struct LogHistoryViewerState {
    /// Currently selected log file (filename)
    pub selected_log_file: Option<String>,
    /// List of available log files
    pub sessions: Vec<SessionLogSummary>,
    /// Currently loaded logs for selected file
    pub current_logs: Vec<LogEntry>,
    /// Scroll offset for log entries
    pub scroll_offset: usize,
    /// Session list selection state
    pub selected_session: Option<usize>,
    /// Current filter level
    pub filter_level: LogFilterLevel,
    /// Search query (if any)
    pub search_query: Option<String>,
    /// Which pane is focused
    pub focus: LogViewerFocus,
    /// Whether the viewer is active/visible
    pub is_visible: bool,
    /// Log directory path
    pub log_dir: Option<PathBuf>,
    /// Error message (if any)
    pub error_message: Option<String>,
    /// Text selection state for copy
    pub selection: TextSelection,
    /// Log entries pane area (for mouse coordinate mapping)
    pub log_entries_area: Option<crate::geometry::Area>,
    /// Horizontal scroll offset for log content
    pub horizontal_scroll: usize,
}

impl LogHistoryViewerState {
    pub fn new() -> Self {
        Self {
            selected_log_file: None,
            sessions: Vec::new(),
            current_logs: Vec::new(),
            scroll_offset: 0,
            selected_session: None,
            filter_level: LogFilterLevel::All,
            search_query: None,
            focus: LogViewerFocus::SessionList,
            is_visible: false,
            log_dir: None,
            error_message: None,
            selection: TextSelection::default(),
            log_entries_area: None,
            horizontal_scroll: 0,
        }
    }

    /// Set the log directory and refresh log files
    pub fn set_log_dir(&mut self, log_dir: PathBuf) {
        self.log_dir = Some(log_dir);

        // Auto-prune old logs (>24h) on startup
        if let Ok(count) = self.prune_old_logs() {
            if count > 0 {
                tracing::info!("Pruned {} old log files (>24h)", count);
            }
        }

        self.refresh_sessions();
    }

    /// Refresh the list of available log files
    pub fn refresh_sessions(&mut self) {
        let Some(log_dir) = &self.log_dir else {
            self.error_message = Some("Log directory not configured".to_string());
            return;
        };

        match JsonlLogReader::list_app_logs(log_dir) {
            Ok(infos) => {
                self.sessions = infos.into_iter().map(SessionLogSummary::from).collect();
                self.error_message = None;

                // Select first log file if none selected
                if self.selected_log_file.is_none() && !self.sessions.is_empty() {
                    self.selected_session = Some(0);
                }
            }
            Err(e) => {
                self.error_message = Some(format!("Failed to list log files: {}", e));
            }
        }
    }

    /// Load logs for the selected log file
    pub fn load_selected_session(&mut self) {
        let Some(idx) = self.selected_session else {
            return;
        };

        let Some(session) = self.sessions.get(idx) else {
            return;
        };

        // Use the log_path directly from the session summary
        let log_path = session.log_path.clone();

        match JsonlLogReader::read_tracing_logs(&log_path) {
            Ok(logs) => {
                self.current_logs = logs;
                self.selected_log_file = Some(session.filename.clone());
                self.scroll_offset = 0;
                self.error_message = None;
            }
            Err(e) => {
                self.error_message = Some(format!("Failed to load logs: {}", e));
            }
        }
    }

    /// Get filtered logs based on current filter level
    pub fn filtered_logs(&self) -> Vec<&LogEntry> {
        self.current_logs
            .iter()
            .filter(|log| self.filter_level.matches(log.level))
            .filter(|log| {
                if let Some(query) = &self.search_query {
                    let query_lower = query.to_lowercase();
                    log.message.to_lowercase().contains(&query_lower)
                        || log.source.to_lowercase().contains(&query_lower)
                } else {
                    true
                }
            })
            .collect()
    }

    /// Move session selection up
    pub fn select_prev_session(&mut self) {
        if self.sessions.is_empty() {
            return;
        }

        let current = self.selected_session.unwrap_or(0);
        let new_idx = if current > 0 { current - 1 } else { 0 };
        self.selected_session = Some(new_idx);
    }

    /// Move session selection down
    pub fn select_next_session(&mut self) {
        if self.sessions.is_empty() {
            return;
        }

        let current = self.selected_session.unwrap_or(0);
        let max_idx = self.sessions.len().saturating_sub(1);
        let new_idx = if current < max_idx {
            current + 1
        } else {
            max_idx
        };
        self.selected_session = Some(new_idx);
    }

    /// Select a session by index (for mouse click)
    pub fn select_session_by_index(&mut self, index: usize) {
        if index < self.sessions.len() {
            self.selected_session = Some(index);
            self.focus = LogViewerFocus::SessionList;
            self.load_selected_session();
        }
    }

    /// Handle mouse click at coordinates (relative to log history area)
    /// Returns true if click was handled
    pub fn handle_click(&mut self, x: u16, y: u16, area_x: u16, area_y: u16) -> bool {
        // Account for outer border (1 pixel) and title
        let inner_x = x.saturating_sub(area_x + 1);
        let inner_y = y.saturating_sub(area_y + 1);

        // Left pane is 25 chars wide, account for its border too
        let left_pane_width = 25u16;

        if inner_x < left_pane_width {
            // Clicked in session list pane - clear any selection
            self.selection.clear();
            // Account for panel border and title (2 lines: border + title line)
            let list_y = inner_y.saturating_sub(1);
            let clicked_index = list_y as usize;

            if clicked_index < self.sessions.len() {
                self.select_session_by_index(clicked_index);
                return true;
            }
        } else {
            // Clicked in log entries pane - start text selection
            self.focus = LogViewerFocus::LogEntries;
            self.start_selection(x, y);
            return true;
        }

        false
    }

    /// Start text selection at given screen coordinates
    pub fn start_selection(&mut self, x: u16, y: u16) {
        if let Some(area) = self.log_entries_area {
            // Convert screen coordinates to log line and char offset
            if let Some((line_idx, char_offset)) = self.screen_to_log_position(x, y, area) {
                self.selection.clear();
                self.selection.start = Some((line_idx, char_offset));
                self.selection.end = Some((line_idx, char_offset));
                self.selection.is_selecting = true;
            }
        }
    }

    /// Update text selection during drag
    pub fn update_selection(&mut self, x: u16, y: u16) {
        if !self.selection.is_selecting {
            return;
        }
        if let Some(area) = self.log_entries_area {
            if let Some((line_idx, char_offset)) = self.screen_to_log_position(x, y, area) {
                self.selection.end = Some((line_idx, char_offset));
                // Update cached selected text
                self.selection.selected_text = self.get_selected_text();
            }
        }
    }

    /// End text selection
    pub fn end_selection(&mut self) {
        self.selection.is_selecting = false;
        self.selection.selected_text = self.get_selected_text();
    }

    /// Convert screen coordinates to log line index and character offset
    fn screen_to_log_position(
        &self,
        x: u16,
        y: u16,
        area: crate::geometry::Area,
    ) -> Option<(usize, usize)> {
        // Check if coordinates are within the log entries area
        if x < area.x || x >= area.x + area.width || y < area.y || y >= area.y + area.height {
            return None;
        }

        // Account for border (1 char on each side)
        let content_x = x.saturating_sub(area.x + 1);
        let content_y = y.saturating_sub(area.y + 1);

        // Calculate log line index (accounting for scroll offset)
        let line_idx = self.scroll_offset + content_y as usize;
        let char_offset = content_x as usize;

        let filtered = self.filtered_logs();
        if line_idx < filtered.len() {
            Some((line_idx, char_offset))
        } else {
            // Clamp to last line
            if !filtered.is_empty() {
                Some((filtered.len() - 1, char_offset))
            } else {
                None
            }
        }
    }

    /// Get the currently selected text
    pub fn get_selected_text(&self) -> Option<String> {
        let ((start_line, start_char), (end_line, end_char)) = self.selection.normalized()?;
        let filtered = self.filtered_logs();

        if filtered.is_empty() || start_line >= filtered.len() {
            return None;
        }

        let mut result = String::new();

        for line_idx in start_line..=end_line.min(filtered.len() - 1) {
            let log = &filtered[line_idx];
            let line_text = format!(
                "{} [{}] {}",
                log.timestamp.format("%H:%M:%S"),
                log.source,
                log.message
            );

            let char_count = line_text.chars().count();

            if start_line == end_line {
                // Single line selection - convert char indices to byte indices
                let byte_start = char_to_byte_index(&line_text, start_char.min(char_count));
                let byte_end = char_to_byte_index(&line_text, end_char.min(char_count));
                if byte_start < byte_end {
                    result.push_str(&line_text[byte_start..byte_end]);
                }
            } else if line_idx == start_line {
                // First line - from start_char to end
                let byte_start = char_to_byte_index(&line_text, start_char.min(char_count));
                result.push_str(&line_text[byte_start..]);
                result.push('\n');
            } else if line_idx == end_line {
                // Last line - from beginning to end_char
                let byte_end = char_to_byte_index(&line_text, end_char.min(char_count));
                result.push_str(&line_text[..byte_end]);
            } else {
                // Middle line - entire line
                result.push_str(&line_text);
                result.push('\n');
            }
        }

        if result.is_empty() {
            None
        } else {
            Some(result)
        }
    }

    /// Copy selected text to clipboard
    pub fn copy_selection_to_clipboard(&self) -> Result<(), String> {
        // Get text from cached selection or compute it fresh
        let text = self.selection.selected_text.clone().or_else(|| self.get_selected_text());

        if let Some(text) = text {
            use arboard::Clipboard;
            let mut clipboard = Clipboard::new().map_err(|e| e.to_string())?;
            clipboard.set_text(text).map_err(|e| e.to_string())?;
            Ok(())
        } else {
            Err("No text selected".to_string())
        }
    }

    /// Check if a log line is within the current selection
    pub fn is_line_selected(&self, line_idx: usize) -> bool {
        if let Some(((start_line, _), (end_line, _))) = self.selection.normalized() {
            line_idx >= start_line && line_idx <= end_line
        } else {
            false
        }
    }

    /// Get selection range for a specific line (returns char start/end or None)
    pub fn get_line_selection_range(
        &self,
        line_idx: usize,
        line_len: usize,
    ) -> Option<(usize, usize)> {
        let ((start_line, start_char), (end_line, end_char)) = self.selection.normalized()?;

        if line_idx < start_line || line_idx > end_line {
            return None;
        }

        let sel_start = if line_idx == start_line {
            start_char
        } else {
            0
        };
        let sel_end = if line_idx == end_line {
            end_char.min(line_len)
        } else {
            line_len
        };

        if sel_start < sel_end {
            Some((sel_start, sel_end))
        } else {
            None
        }
    }

    /// Scroll log entries up
    pub fn scroll_up(&mut self) {
        if self.scroll_offset > 0 {
            self.scroll_offset -= 1;
        }
    }

    /// Scroll log entries up by N lines (for mouse scroll)
    pub fn scroll_up_by(&mut self, lines: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(lines);
    }

    /// Scroll log entries down
    pub fn scroll_down(&mut self) {
        let filtered_count = self.filtered_logs().len();
        if self.scroll_offset < filtered_count.saturating_sub(1) {
            self.scroll_offset += 1;
        }
    }

    /// Scroll log entries down by N lines (for mouse scroll)
    pub fn scroll_down_by(&mut self, lines: usize) {
        let filtered_count = self.filtered_logs().len();
        let max_offset = filtered_count.saturating_sub(1);
        self.scroll_offset = (self.scroll_offset + lines).min(max_offset);
    }

    /// Scroll log content left (horizontal)
    pub fn scroll_left(&mut self, amount: usize) {
        self.horizontal_scroll = self.horizontal_scroll.saturating_sub(amount);
    }

    /// Scroll log content right (horizontal)
    pub fn scroll_right(&mut self, amount: usize) {
        self.horizontal_scroll += amount;
    }

    /// Reset horizontal scroll to start
    pub fn scroll_home(&mut self) {
        self.horizontal_scroll = 0;
    }

    /// Page up in log entries
    pub fn page_up(&mut self, page_size: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(page_size);
    }

    /// Page down in log entries
    pub fn page_down(&mut self, page_size: usize) {
        let filtered_count = self.filtered_logs().len();
        let max_offset = filtered_count.saturating_sub(1);
        self.scroll_offset = (self.scroll_offset + page_size).min(max_offset);
    }

    /// Toggle focus between session list and log entries
    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            LogViewerFocus::SessionList => LogViewerFocus::LogEntries,
            LogViewerFocus::LogEntries => LogViewerFocus::SessionList,
        };
    }

    /// Cycle filter level
    pub fn cycle_filter(&mut self) {
        self.filter_level = self.filter_level.next();
        self.scroll_offset = 0; // Reset scroll when filter changes
    }

    /// Set search query
    pub fn set_search(&mut self, query: Option<String>) {
        self.search_query = query;
        self.scroll_offset = 0;
    }

    /// Show the viewer
    pub fn show(&mut self) {
        self.is_visible = true;
        self.refresh_sessions();
    }

    /// Hide the viewer
    pub fn hide(&mut self) {
        self.is_visible = false;
    }

    /// Prune log files older than 24 hours
    /// Returns the number of files deleted
    pub fn prune_old_logs(&self) -> std::io::Result<usize> {
        let Some(log_dir) = &self.log_dir else {
            return Ok(0);
        };

        let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(24 * 60 * 60);
        let mut deleted = 0;

        for entry in std::fs::read_dir(log_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().map_or(false, |ext| ext == "jsonl" || ext == "log") {
                if let Ok(metadata) = path.metadata() {
                    if let Ok(modified) = metadata.modified() {
                        if modified < cutoff {
                            if std::fs::remove_file(&path).is_ok() {
                                deleted += 1;
                            }
                        }
                    }
                }
            }
        }
        Ok(deleted)
    }

    /// Delete all log files (manual cleanup)
    /// Returns the number of files deleted
    pub fn delete_all_logs(&mut self) -> std::io::Result<usize> {
        let Some(log_dir) = &self.log_dir else {
            return Ok(0);
        };

        let mut deleted = 0;
        for entry in std::fs::read_dir(log_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().map_or(false, |ext| ext == "jsonl" || ext == "log") {
                if std::fs::remove_file(&path).is_ok() {
                    deleted += 1;
                }
            }
        }

        // Clear current state
        self.sessions.clear();
        self.current_logs.clear();
        self.selected_log_file = None;
        self.selected_session = None;

        Ok(deleted)
    }
}

impl Default for LogHistoryViewerState {
    fn default() -> Self {
        Self::new()
    }
}
