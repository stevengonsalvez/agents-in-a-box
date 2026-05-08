// ABOUTME: Log history viewer component for browsing historical application logs
// Displays log files in a list and shows color-coded log entries with filtering
// Supports both JSONL (new) and plain text (legacy) log formats

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, List, ListItem, ListState, Paragraph},
    Frame,
};
use std::path::PathBuf;

use super::live_logs_stream::{LogEntry, LogEntryLevel};
use super::log_reader::{AppLogInfo, JsonlLogReader};

// Color palette from TUI style guide
const CORNFLOWER_BLUE: Color = Color::Rgb(100, 149, 237);
const GOLD: Color = Color::Rgb(255, 215, 0);
const SELECTION_GREEN: Color = Color::Rgb(100, 200, 100);
const DARK_BG: Color = Color::Rgb(25, 25, 35);
const PANEL_BG: Color = Color::Rgb(30, 30, 40);
const LIST_HIGHLIGHT_BG: Color = Color::Rgb(40, 40, 60);
const SOFT_WHITE: Color = Color::Rgb(220, 220, 230);
const MUTED_GRAY: Color = Color::Rgb(120, 120, 140);
const SUBDUED_BORDER: Color = Color::Rgb(60, 60, 80);
const SELECTION_BG: Color = Color::Rgb(70, 130, 180); // Steel blue for text selection

// Log level colors
const ERROR_RED: Color = Color::Rgb(255, 100, 100);
const WARN_YELLOW: Color = Color::Rgb(255, 200, 100);

/// Convert a character index to a byte index in a UTF-8 string
/// Returns the byte offset of the nth character, or the string length if n exceeds char count
fn char_to_byte_index(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(byte_idx, _)| byte_idx)
        .unwrap_or(s.len())
}
const INFO_BLUE: Color = Color::Rgb(100, 149, 237);
const DEBUG_GRAY: Color = Color::Rgb(120, 120, 140);

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
    pub session_list_state: ListState,
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
    pub log_entries_area: Option<ratatui::layout::Rect>,
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
            session_list_state: ListState::default(),
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
                    self.session_list_state.select(Some(0));
                }
            }
            Err(e) => {
                self.error_message = Some(format!("Failed to list log files: {}", e));
            }
        }
    }

    /// Load logs for the selected log file
    pub fn load_selected_session(&mut self) {
        let Some(idx) = self.session_list_state.selected() else {
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

        let current = self.session_list_state.selected().unwrap_or(0);
        let new_idx = if current > 0 { current - 1 } else { 0 };
        self.session_list_state.select(Some(new_idx));
    }

    /// Move session selection down
    pub fn select_next_session(&mut self) {
        if self.sessions.is_empty() {
            return;
        }

        let current = self.session_list_state.selected().unwrap_or(0);
        let max_idx = self.sessions.len().saturating_sub(1);
        let new_idx = if current < max_idx { current + 1 } else { max_idx };
        self.session_list_state.select(Some(new_idx));
    }

    /// Select a session by index (for mouse click)
    pub fn select_session_by_index(&mut self, index: usize) {
        if index < self.sessions.len() {
            self.session_list_state.select(Some(index));
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
    fn screen_to_log_position(&self, x: u16, y: u16, area: ratatui::layout::Rect) -> Option<(usize, usize)> {
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
        let text = self.selection.selected_text.clone()
            .or_else(|| self.get_selected_text());

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
    pub fn get_line_selection_range(&self, line_idx: usize, line_len: usize) -> Option<(usize, usize)> {
        let ((start_line, start_char), (end_line, end_char)) = self.selection.normalized()?;

        if line_idx < start_line || line_idx > end_line {
            return None;
        }

        let sel_start = if line_idx == start_line { start_char } else { 0 };
        let sel_end = if line_idx == end_line { end_char.min(line_len) } else { line_len };

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
        self.session_list_state.select(None);

        Ok(deleted)
    }
}

impl Default for LogHistoryViewerState {
    fn default() -> Self {
        Self::new()
    }
}

/// Log history viewer component
pub struct LogHistoryViewerComponent;

impl LogHistoryViewerComponent {
    pub fn new() -> Self {
        Self
    }

    /// Render the log history viewer
    pub fn render(&self, frame: &mut Frame, area: Rect, state: &mut LogHistoryViewerState) {
        // Main container
        let container = Block::default()
            .title(Line::from(vec![
                Span::styled("📋 ", Style::default().fg(GOLD)),
                Span::styled(
                    "Log History",
                    Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                ),
            ]))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(CORNFLOWER_BLUE))
            .style(Style::default().bg(DARK_BG));

        let inner = container.inner(area);
        frame.render_widget(container, area);

        // Check for error message
        if let Some(error) = &state.error_message {
            let error_text = Paragraph::new(error.as_str())
                .style(Style::default().fg(ERROR_RED))
                .alignment(Alignment::Center);
            frame.render_widget(error_text, inner);
            return;
        }

        // Layout: session list | log entries
        let layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(25), // Session list
                Constraint::Min(40),    // Log entries
            ])
            .split(inner);

        // Store log entries area for mouse coordinate mapping
        state.log_entries_area = Some(layout[1]);

        self.render_session_list(frame, layout[0], state);
        self.render_log_entries(frame, layout[1], state);
    }

    /// Render the session list
    fn render_session_list(&self, frame: &mut Frame, area: Rect, state: &mut LogHistoryViewerState) {
        let is_focused = state.focus == LogViewerFocus::SessionList;
        let border_color = if is_focused { CORNFLOWER_BLUE } else { SUBDUED_BORDER };

        // Build title with directory path (replace $HOME with ~)
        let dir_display = state
            .log_dir
            .as_ref()
            .map(|p| {
                let path_str = p.display().to_string();
                if let Ok(home) = std::env::var("HOME") {
                    path_str.replace(&home, "~")
                } else {
                    path_str
                }
            })
            .unwrap_or_else(|| "Not configured".to_string());

        let block = Block::default()
            .title(Line::from(vec![
                Span::styled("📁 ", Style::default().fg(GOLD)),
                Span::styled(dir_display, Style::default().fg(MUTED_GRAY)),
            ]))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(border_color))
            .style(Style::default().bg(PANEL_BG));

        if state.sessions.is_empty() {
            let empty_msg = Paragraph::new("No log files found")
                .block(block)
                .style(Style::default().fg(MUTED_GRAY))
                .alignment(Alignment::Center);
            frame.render_widget(empty_msg, area);
            return;
        }

        let items: Vec<ListItem> = state
            .sessions
            .iter()
            .map(|session| {
                let status_indicator = if session.error_count > 0 {
                    Span::styled("● ", Style::default().fg(ERROR_RED))
                } else if session.warn_count > 0 {
                    Span::styled("● ", Style::default().fg(WARN_YELLOW))
                } else {
                    Span::styled("● ", Style::default().fg(SELECTION_GREEN))
                };

                let name = Span::styled(
                    &session.display_name,
                    Style::default().fg(SOFT_WHITE),
                );

                let count = Span::styled(
                    format!(" ({})", session.log_count),
                    Style::default().fg(MUTED_GRAY),
                );

                ListItem::new(Line::from(vec![status_indicator, name, count]))
            })
            .collect();

        let list = List::new(items)
            .block(block)
            .highlight_style(
                Style::default()
                    .bg(LIST_HIGHLIGHT_BG)
                    .fg(SOFT_WHITE)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▶ ");

        frame.render_stateful_widget(list, area, &mut state.session_list_state);
    }

    /// Render the log entries
    fn render_log_entries(&self, frame: &mut Frame, area: Rect, state: &LogHistoryViewerState) {
        let is_focused = state.focus == LogViewerFocus::LogEntries;
        let border_color = if is_focused { CORNFLOWER_BLUE } else { SUBDUED_BORDER };

        let filtered_logs = state.filtered_logs();
        let total_count = state.current_logs.len();
        let filtered_count = filtered_logs.len();

        let title = if filtered_count != total_count {
            format!(
                "Logs [{}/{}] [{}]",
                filtered_count,
                total_count,
                state.filter_level.as_str()
            )
        } else {
            format!("Logs [{}] [{}]", total_count, state.filter_level.as_str())
        };

        let block = Block::default()
            .title(Line::from(vec![
                Span::styled(
                    title,
                    Style::default().fg(if is_focused { GOLD } else { MUTED_GRAY }),
                ),
            ]))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(border_color))
            .style(Style::default().bg(PANEL_BG));

        if state.selected_log_file.is_none() {
            let msg = Paragraph::new("Select a log file to view logs")
                .block(block)
                .style(Style::default().fg(MUTED_GRAY))
                .alignment(Alignment::Center);
            frame.render_widget(msg, area);
            return;
        }

        if filtered_logs.is_empty() {
            let msg = if state.search_query.is_some() {
                "No logs match the search query"
            } else {
                "No logs at this filter level"
            };
            let empty = Paragraph::new(msg)
                .block(block)
                .style(Style::default().fg(MUTED_GRAY))
                .alignment(Alignment::Center);
            frame.render_widget(empty, area);
            return;
        }

        // Create log lines with selection highlighting
        let log_lines: Vec<Line> = filtered_logs
            .iter()
            .enumerate()
            .map(|(idx, log)| self.format_log_line_with_selection(log, idx, state))
            .collect();

        let paragraph = Paragraph::new(log_lines)
            .block(block)
            .scroll((state.scroll_offset as u16, state.horizontal_scroll as u16)); // Enable vertical and horizontal scrolling

        frame.render_widget(paragraph, area);
    }

    /// Format a single log line with color coding and selection highlighting
    fn format_log_line_with_selection<'a>(
        &self,
        entry: &'a LogEntry,
        line_idx: usize,
        state: &LogHistoryViewerState,
    ) -> Line<'a> {
        let (icon, color) = match entry.level {
            LogEntryLevel::Error => ("❌", ERROR_RED),
            LogEntryLevel::Warn => ("⚠️", WARN_YELLOW),
            LogEntryLevel::Info => ("ℹ️", INFO_BLUE),
            LogEntryLevel::Debug => ("🔍", DEBUG_GRAY),
        };

        let timestamp = entry.timestamp.format("%H:%M:%S").to_string();
        let line_text = format!("{} {} {}", timestamp, icon, entry.message);

        // Check if this line has selection
        // Note: sel_start/sel_end are character (grapheme) offsets, need to convert to byte offsets
        let char_count = line_text.chars().count();
        if let Some((sel_start, sel_end)) = state.get_line_selection_range(line_idx, char_count) {
            // Convert character offsets to byte offsets safely
            let byte_start = char_to_byte_index(&line_text, sel_start);
            let byte_end = char_to_byte_index(&line_text, sel_end);

            // Build spans with selection highlighting
            let mut spans = Vec::new();

            // Before selection
            if byte_start > 0 {
                let before = &line_text[..byte_start];
                spans.push(Span::styled(before.to_string(), self.get_base_style_for_segment(before, &timestamp, icon, color)));
            }

            // Selected portion
            if byte_start < byte_end && byte_end <= line_text.len() {
                let selected = &line_text[byte_start..byte_end];
                spans.push(Span::styled(
                    selected.to_string(),
                    Style::default().fg(Color::Black).bg(SELECTION_BG),
                ));
            }

            // After selection
            if byte_end < line_text.len() {
                let after = &line_text[byte_end..];
                spans.push(Span::styled(after.to_string(), self.get_base_style_for_segment(after, &timestamp, icon, color)));
            }

            Line::from(spans)
        } else {
            // No selection - use original formatting
            Line::from(vec![
                Span::styled(
                    format!("{} ", timestamp),
                    Style::default().fg(MUTED_GRAY),
                ),
                Span::styled(format!("{} ", icon), Style::default().fg(color)),
                Span::styled(entry.message.clone(), Style::default().fg(SOFT_WHITE)),
            ])
        }
    }

    /// Get base style for a text segment (simplified - just returns white for now)
    fn get_base_style_for_segment(&self, _segment: &str, _timestamp: &str, _icon: &str, _color: Color) -> Style {
        // Simplified: just return soft white for selected portions
        Style::default().fg(SOFT_WHITE)
    }

    /// Render the help bar at the bottom
    pub fn render_help_bar(&self, frame: &mut Frame, area: Rect, state: &LogHistoryViewerState) {
        let help_items = match state.focus {
            LogViewerFocus::SessionList => vec![
                ("↑↓", "navigate"),
                ("Enter", "load"),
                ("Tab", "logs"),
                ("f", "filter"),
                ("r", "refresh"),
                ("C", "clean"),
                ("Esc", "back"),
            ],
            LogViewerFocus::LogEntries => vec![
                ("↑↓", "scroll"),
                ("←→/⇧🖱", "pan"),
                ("drag", "select"),
                ("y/^C", "copy"),
                ("Tab", "files"),
                ("f", "filter"),
                ("C", "clean"),
                ("Esc", "back"),
            ],
        };

        let mut spans = Vec::new();
        spans.push(Span::styled("  ", Style::default()));

        for (i, (key, desc)) in help_items.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(" | ", Style::default().fg(SUBDUED_BORDER)));
            }
            spans.push(Span::styled(
                *key,
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(" ", Style::default()));
            spans.push(Span::styled(*desc, Style::default().fg(MUTED_GRAY)));
        }

        let help_bar = Paragraph::new(Line::from(spans)).style(Style::default().bg(DARK_BG));

        frame.render_widget(help_bar, area);
    }
}

impl Default for LogHistoryViewerComponent {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_filter_level_cycle() {
        let mut level = LogFilterLevel::All;
        level = level.next();
        assert_eq!(level, LogFilterLevel::Info);
        level = level.next();
        assert_eq!(level, LogFilterLevel::Warn);
        level = level.next();
        assert_eq!(level, LogFilterLevel::Error);
        level = level.next();
        assert_eq!(level, LogFilterLevel::All);
    }

    #[test]
    fn test_filter_matches() {
        assert!(LogFilterLevel::All.matches(LogEntryLevel::Debug));
        assert!(LogFilterLevel::All.matches(LogEntryLevel::Error));

        assert!(!LogFilterLevel::Info.matches(LogEntryLevel::Debug));
        assert!(LogFilterLevel::Info.matches(LogEntryLevel::Info));

        assert!(!LogFilterLevel::Warn.matches(LogEntryLevel::Info));
        assert!(LogFilterLevel::Warn.matches(LogEntryLevel::Warn));
        assert!(LogFilterLevel::Warn.matches(LogEntryLevel::Error));

        assert!(!LogFilterLevel::Error.matches(LogEntryLevel::Warn));
        assert!(LogFilterLevel::Error.matches(LogEntryLevel::Error));
    }

    #[test]
    fn test_state_navigation() {
        let mut state = LogHistoryViewerState::new();
        state.sessions = vec![
            SessionLogSummary {
                filename: "agents-in-a-box-20260107-001310.jsonl".to_string(),
                display_name: "2026-01-07 00:13:10".to_string(),
                log_path: PathBuf::from("/tmp/test1.jsonl"),
                log_count: 10,
                error_count: 0,
                warn_count: 0,
                is_jsonl: true,
            },
            SessionLogSummary {
                filename: "agents-in-a-box-20260107-001410.jsonl".to_string(),
                display_name: "2026-01-07 00:14:10".to_string(),
                log_path: PathBuf::from("/tmp/test2.jsonl"),
                log_count: 20,
                error_count: 1,
                warn_count: 2,
                is_jsonl: true,
            },
        ];
        state.session_list_state.select(Some(0));

        state.select_next_session();
        assert_eq!(state.session_list_state.selected(), Some(1));

        state.select_next_session();
        assert_eq!(state.session_list_state.selected(), Some(1)); // Should stay at max

        state.select_prev_session();
        assert_eq!(state.session_list_state.selected(), Some(0));
    }

    #[test]
    fn test_toggle_focus() {
        let mut state = LogHistoryViewerState::new();
        assert_eq!(state.focus, LogViewerFocus::SessionList);

        state.toggle_focus();
        assert_eq!(state.focus, LogViewerFocus::LogEntries);

        state.toggle_focus();
        assert_eq!(state.focus, LogViewerFocus::SessionList);
    }
}
