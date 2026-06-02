// ABOUTME: Inbox screen component for ainb-hooks notifications.
//
// Renders a two-pane (list + detail) view backed by the
// `ainb-plugin-notifyd` SQLite store at
// `~/.agents-in-a-box/notifications.db`. Polls the store on every
// render tick (we hold a long-lived `Store` handle; rusqlite WAL means
// the daemon's writes are visible without re-opening). Keys:
//
//   - ↑ / ↓ / k / j   move selection
//   - PageUp / PageDown   jump 10 rows
//   - Enter           open detail + auto-mark-read
//   - d               dismiss selected row
//   - Shift+C         dismiss every currently-visible row
//   - a               toggle archived (dismissed) filter
//   - p               cycle agent filter (all / claude / codex)
//   - r               force refresh from store
//   - q / Esc         return to previous screen
//
// All rendering follows the ainb-tui style guide (rounded borders,
// gold titles, cornflower-blue panels, selection-green indicator).

use ainb_plugin_notifyd::{NotificationRecord, Paths, Store};
use chrono::{DateTime, Local, TimeZone};
use ratatui::{
    prelude::*,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, List, ListItem, ListState, Paragraph, Wrap},
};

// Match the rest of ainb-tui's palette (see components/layout.rs).
const CORNFLOWER_BLUE: Color = Color::Rgb(100, 149, 237);
const GOLD: Color = Color::Rgb(255, 215, 0);
const SELECTION_GREEN: Color = Color::Rgb(100, 200, 100);
const SOFT_WHITE: Color = Color::Rgb(220, 220, 230);
const MUTED_GRAY: Color = Color::Rgb(120, 120, 140);
const PANEL_BG: Color = Color::Rgb(30, 30, 40);
const SUBDUED_BORDER: Color = Color::Rgb(60, 60, 80);

/// Agent filter cycled by the `p` key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentFilter {
    /// Show both agents.
    #[default]
    All,
    /// Only Claude rows.
    Claude,
    /// Only Codex rows.
    Codex,
}

impl AgentFilter {
    /// Cycle to the next filter (All → Claude → Codex → All).
    pub fn next(self) -> Self {
        match self {
            AgentFilter::All => AgentFilter::Claude,
            AgentFilter::Claude => AgentFilter::Codex,
            AgentFilter::Codex => AgentFilter::All,
        }
    }

    /// Lowercase label for display.
    pub fn label(self) -> &'static str {
        match self {
            AgentFilter::All => "all",
            AgentFilter::Claude => "claude",
            AgentFilter::Codex => "codex",
        }
    }

    /// Returns the `agent` SQL filter, or None when no filter applies.
    pub fn as_sql(self) -> Option<&'static str> {
        match self {
            AgentFilter::All => None,
            AgentFilter::Claude => Some("claude"),
            AgentFilter::Codex => Some("codex"),
        }
    }
}

/// All state owned by the Inbox screen. Stored at app-level so it
/// survives screen transitions without forgetting selection/filters.
///
/// `Debug` is hand-rolled below to skip the `store` field, which
/// wraps a rusqlite connection that isn't `Debug`.
pub struct InboxState {
    /// Currently selected row index (0-based) into `rows`.
    pub selected: usize,
    /// Most-recently-fetched rows, ordered ts-descending.
    pub rows: Vec<NotificationRecord>,
    /// Include dismissed rows in the visible set.
    pub show_archived: bool,
    /// Agent filter (cycled by `p`).
    pub agent_filter: AgentFilter,
    /// Connection handle. `None` until the screen is first opened or
    /// the DB cannot be opened (the screen surfaces a friendly empty
    /// state in that case).
    pub store: Option<Store>,
    /// Cached path for diagnostics (shown in the empty state when the
    /// store hasn't been initialised).
    pub db_path: std::path::PathBuf,
    /// Render-tick counter used to decide whether to re-query SQLite.
    /// We re-query at most every `POLL_TICKS` renders to keep CPU low
    /// on busy screens.
    pub tick: u64,
}

impl Default for InboxState {
    fn default() -> Self {
        let paths = Paths::from_home().ok();
        let db_path = paths.as_ref().map(|p| p.db.clone()).unwrap_or_default();
        // Eagerly open the store when the DB already exists so the
        // global unread badge (rendered by the bottom menu bar before
        // the Inbox screen is opened) sees a live count from the
        // first frame.
        let store = if db_path.exists() {
            Store::open(&db_path)
                .map_err(|e| {
                    tracing::warn!(
                        error = ?e,
                        path = %db_path.display(),
                        "inbox: eager store open failed"
                    );
                })
                .ok()
        } else {
            None
        };
        Self {
            selected: 0,
            rows: Vec::new(),
            show_archived: false,
            agent_filter: AgentFilter::default(),
            store,
            db_path,
            tick: 0,
        }
    }
}

impl std::fmt::Debug for InboxState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InboxState")
            .field("selected", &self.selected)
            .field("rows.len", &self.rows.len())
            .field("show_archived", &self.show_archived)
            .field("agent_filter", &self.agent_filter)
            .field("store_open", &self.store.is_some())
            .field("db_path", &self.db_path)
            .field("tick", &self.tick)
            .finish()
    }
}

const POLL_TICKS: u64 = 1; // every render — cheap with WAL + LIMIT 200
const LIST_LIMIT: u32 = 200;

impl InboxState {
    /// Open the SQLite store lazily and refresh the visible rows.
    /// Idempotent: re-running while open is a noop.
    pub fn ensure_open(&mut self) {
        if self.store.is_none() && self.db_path.exists() {
            match Store::open(&self.db_path) {
                Ok(s) => self.store = Some(s),
                Err(e) => {
                    tracing::warn!(error = ?e, path = %self.db_path.display(), "inbox: store open failed");
                }
            }
        }
    }

    /// Re-fetch rows from the store. Resets selection if it lands
    /// past the new bounds.
    pub fn refresh(&mut self) {
        self.ensure_open();
        if let Some(store) = self.store.as_ref() {
            match store.list(
                self.show_archived,
                self.agent_filter.as_sql(),
                None,
                LIST_LIMIT,
            ) {
                Ok(rows) => {
                    self.rows = rows;
                    if self.selected >= self.rows.len() {
                        self.selected = self.rows.len().saturating_sub(1);
                    }
                }
                Err(e) => {
                    tracing::warn!(error = ?e, "inbox: list failed");
                }
            }
        }
    }

    /// Currently-selected row, if any.
    pub fn selected_row(&self) -> Option<&NotificationRecord> {
        self.rows.get(self.selected)
    }

    /// Move selection up by `n` rows, saturating at 0.
    pub fn move_up(&mut self, n: usize) {
        self.selected = self.selected.saturating_sub(n);
    }

    /// Move selection down by `n` rows, saturating at the last row.
    pub fn move_down(&mut self, n: usize) {
        if self.rows.is_empty() {
            self.selected = 0;
        } else {
            self.selected = (self.selected + n).min(self.rows.len() - 1);
        }
    }

    /// Mark the currently-selected row read. Returns true if a row
    /// was selected (and a write was attempted).
    pub fn mark_selected_read(&mut self) -> bool {
        let Some(row_id) = self.selected_row().map(|r| r.id.clone()) else {
            return false;
        };
        if let Some(store) = self.store.as_ref() {
            let _ = store.mark_read(&row_id);
            self.refresh();
            true
        } else {
            false
        }
    }

    /// Dismiss the currently-selected row.
    pub fn dismiss_selected(&mut self) -> bool {
        let Some(row_id) = self.selected_row().map(|r| r.id.clone()) else {
            return false;
        };
        if let Some(store) = self.store.as_ref() {
            let _ = store.dismiss(&row_id);
            self.refresh();
            true
        } else {
            false
        }
    }

    /// Dismiss every currently-visible row.
    pub fn dismiss_visible(&mut self) -> u64 {
        let Some(store) = self.store.as_ref() else {
            return 0;
        };
        let n = store.dismiss_visible().unwrap_or(0);
        self.refresh();
        n
    }

    /// Toggle the archived (dismissed) filter.
    pub fn toggle_archived(&mut self) {
        self.show_archived = !self.show_archived;
        self.refresh();
    }

    /// Cycle the agent filter.
    pub fn cycle_agent_filter(&mut self) {
        self.agent_filter = self.agent_filter.next();
        self.refresh();
    }
}

/// Convert an epoch-ms timestamp to a local `HH:MM:SS` clock label,
/// or a 5-digit relative count if the timestamp is degenerate.
fn fmt_ts(ts: i64) -> String {
    let datetime: DateTime<Local> = match Local.timestamp_millis_opt(ts).single() {
        Some(dt) => dt,
        None => return format!("{ts:05}"),
    };
    datetime.format("%H:%M:%S").to_string()
}

/// Color the row's leading event glyph per agent.
fn agent_color(agent: &str) -> Color {
    match agent {
        "claude" => Color::Rgb(204, 153, 102), // amber
        "codex" => Color::Rgb(102, 204, 153),  // teal-green
        _ => SOFT_WHITE,
    }
}

/// Short event label rendered in the row. Truncated to a maximum of
/// 24 display chars (we count chars, not bytes, so emoji / non-ASCII
/// don't panic). A trailing ellipsis indicates truncation.
fn fmt_event(raw_event: &str) -> String {
    let char_count = raw_event.chars().count();
    if char_count <= 24 {
        raw_event.to_string()
    } else {
        let prefix: String = raw_event.chars().take(23).collect();
        format!("{prefix}…")
    }
}

/// Render the inbox screen into `area`.
pub fn render(frame: &mut Frame, area: Rect, state: &mut InboxState) {
    state.tick = state.tick.wrapping_add(1);
    if state.tick % POLL_TICKS == 0 {
        state.refresh();
    }

    // Three vertical regions: title bar, content (50/50 split), help bar.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(area);

    render_title(frame, chunks[0], state);
    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(chunks[1]);
    render_list(frame, panes[0], state);
    render_detail(frame, panes[1], state);
    render_help(frame, chunks[2], state);
}

fn render_title(frame: &mut Frame, area: Rect, state: &InboxState) {
    let unread = state.store.as_ref().and_then(|s| s.unread_count().ok()).unwrap_or(0);
    let total = state.rows.len();
    let title = Line::from(vec![
        Span::styled(
            "📥 Inbox ",
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("· {total} visible · {unread} unread "),
            Style::default().fg(SOFT_WHITE),
        ),
        Span::styled(
            format!("· filter: agent={} ", state.agent_filter.label()),
            Style::default().fg(MUTED_GRAY),
        ),
        Span::styled(
            format!(
                "· archived={}",
                if state.show_archived { "on" } else { "off" }
            ),
            Style::default().fg(MUTED_GRAY),
        ),
    ]);
    frame.render_widget(Paragraph::new(title), area);
}

fn render_list(frame: &mut Frame, area: Rect, state: &mut InboxState) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CORNFLOWER_BLUE))
        .style(Style::default().bg(PANEL_BG))
        .title(Span::styled(
            " events ",
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ));

    if state.rows.is_empty() {
        let msg = if state.store.is_some() {
            "(no notifications yet — fire a hook to populate)"
        } else if state.db_path.exists() {
            "(could not open notifications.db — check daemon logs)"
        } else {
            "(notifications.db not found — run `ainb-notifyd install --all` then start the daemon)"
        };
        let paragraph = Paragraph::new(msg)
            .style(Style::default().fg(MUTED_GRAY))
            .block(block)
            .wrap(Wrap { trim: false });
        frame.render_widget(paragraph, area);
        return;
    }

    let items: Vec<ListItem> = state
        .rows
        .iter()
        .map(|r| {
            let glyph = if r.read { "  " } else { "● " };
            let glyph_style = if r.read {
                Style::default().fg(MUTED_GRAY)
            } else {
                Style::default().fg(agent_color(&r.agent))
            };
            let project_label = if r.project.is_empty() {
                "(no project)".to_string()
            } else {
                r.project.clone()
            };
            let row = Line::from(vec![
                Span::styled(glyph, glyph_style),
                Span::styled(fmt_ts(r.ts), Style::default().fg(MUTED_GRAY)),
                Span::raw("  "),
                Span::styled(
                    format!("{:<7}", r.agent),
                    Style::default().fg(agent_color(&r.agent)),
                ),
                Span::raw(" "),
                Span::styled(
                    format!("{:<24}", fmt_event(&r.raw_event)),
                    Style::default().fg(SOFT_WHITE),
                ),
                Span::styled(project_label, Style::default().fg(MUTED_GRAY)),
            ]);
            ListItem::new(row)
        })
        .collect();

    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD))
        .highlight_symbol("▶ ");
    let mut list_state = ListState::default();
    list_state.select(Some(state.selected));
    frame.render_stateful_widget(list, area, &mut list_state);
}

fn render_detail(frame: &mut Frame, area: Rect, state: &InboxState) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(SUBDUED_BORDER))
        .style(Style::default().bg(PANEL_BG))
        .title(Span::styled(
            " detail ",
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ));

    let Some(row) = state.selected_row() else {
        let paragraph = Paragraph::new("(select a row to view detail)")
            .style(Style::default().fg(MUTED_GRAY))
            .block(block);
        frame.render_widget(paragraph, area);
        return;
    };

    let mut lines: Vec<Line> = Vec::new();
    let label = |k: &str, v: String| {
        Line::from(vec![
            Span::styled(format!("{k:<10}"), Style::default().fg(MUTED_GRAY)),
            Span::styled(v, Style::default().fg(SOFT_WHITE)),
        ])
    };
    lines.push(label("event:", row.raw_event.clone()));
    lines.push(label("agent:", row.agent.clone()));
    lines.push(label("session:", row.session_id.clone()));
    lines.push(label("project:", row.project.clone()));
    lines.push(label("cwd:", row.cwd.clone()));
    lines.push(label("ts:", fmt_ts(row.ts)));
    lines.push(label(
        "state:",
        format!("read={} dismissed={}", row.read as u8, row.dismissed as u8),
    ));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "payload:",
        Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
    )));
    // Pretty-print the payload JSON; fall back to the raw string if
    // it can't be parsed (shouldn't happen — we wrote it).
    let pretty = serde_json::from_str::<serde_json::Value>(&row.payload_json)
        .ok()
        .and_then(|v| serde_json::to_string_pretty(&v).ok())
        .unwrap_or_else(|| row.payload_json.clone());
    for l in pretty.lines() {
        lines.push(Line::from(Span::styled(
            l.to_string(),
            Style::default().fg(SOFT_WHITE),
        )));
    }

    let paragraph = Paragraph::new(lines).block(block).wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn render_help(frame: &mut Frame, area: Rect, _state: &InboxState) {
    let spans = vec![
        Span::styled("↑↓", Style::default().fg(GOLD)),
        Span::styled(" move  ", Style::default().fg(MUTED_GRAY)),
        Span::styled("Enter", Style::default().fg(GOLD)),
        Span::styled(" open+read  ", Style::default().fg(MUTED_GRAY)),
        Span::styled("d", Style::default().fg(GOLD)),
        Span::styled(" dismiss  ", Style::default().fg(MUTED_GRAY)),
        Span::styled("C", Style::default().fg(GOLD)),
        Span::styled(" clear visible  ", Style::default().fg(MUTED_GRAY)),
        Span::styled("a", Style::default().fg(GOLD)),
        Span::styled(" archived  ", Style::default().fg(MUTED_GRAY)),
        Span::styled("p", Style::default().fg(GOLD)),
        Span::styled(" agent  ", Style::default().fg(MUTED_GRAY)),
        Span::styled("r", Style::default().fg(GOLD)),
        Span::styled(" refresh  ", Style::default().fg(MUTED_GRAY)),
        Span::styled("q/Esc", Style::default().fg(GOLD)),
        Span::styled(" back", Style::default().fg(MUTED_GRAY)),
    ];
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_state_with_rows(rows: Vec<NotificationRecord>) -> InboxState {
        let mut s = InboxState::default();
        s.rows = rows;
        s
    }

    fn row(id: &str, agent: &str, event: &str, ts: i64, read: bool) -> NotificationRecord {
        NotificationRecord {
            id: id.into(),
            ts,
            agent: agent.into(),
            session_id: format!("s-{id}"),
            cwd: "/tmp/x".into(),
            project: "x".into(),
            raw_event: event.into(),
            payload_json: json!({"k":"v"}).to_string(),
            read,
            dismissed: false,
        }
    }

    #[test]
    fn agent_filter_cycles_in_order() {
        let mut f = AgentFilter::default();
        assert_eq!(f, AgentFilter::All);
        f = f.next();
        assert_eq!(f, AgentFilter::Claude);
        f = f.next();
        assert_eq!(f, AgentFilter::Codex);
        f = f.next();
        assert_eq!(f, AgentFilter::All);
    }

    #[test]
    fn move_down_caps_at_last_row() {
        let mut s = make_state_with_rows(vec![
            row("a", "claude", "Stop", 1, false),
            row("b", "codex", "Stop", 2, false),
        ]);
        s.move_down(99);
        assert_eq!(s.selected, 1);
    }

    #[test]
    fn move_up_saturates_at_zero() {
        let mut s = make_state_with_rows(vec![row("a", "claude", "Stop", 1, false)]);
        s.selected = 0;
        s.move_up(5);
        assert_eq!(s.selected, 0);
    }

    #[test]
    fn fmt_event_truncates_long_strings_with_ellipsis() {
        assert_eq!(fmt_event("Stop"), "Stop");
        let truncated = fmt_event(&"a".repeat(30));
        assert_eq!(truncated.chars().count(), 24);
        assert!(truncated.ends_with('…'));
    }

    #[test]
    fn fmt_ts_for_zero_returns_a_clock_label() {
        // Local midnight Jan 1 1970 may not be 00:00:00 in every
        // timezone, but the format must be HH:MM:SS.
        let s = fmt_ts(0);
        let parts: Vec<&str> = s.split(':').collect();
        assert_eq!(parts.len(), 3, "expected HH:MM:SS, got {s}");
    }

    #[test]
    fn renders_empty_state_when_no_rows() {
        let backend = ratatui::backend::TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = InboxState::default();
        // Force store to None so we render the "not found / not open" message.
        state.store = None;
        state.db_path = std::path::PathBuf::from("/nonexistent/notifications.db");
        terminal
            .draw(|f| {
                let area = f.size();
                render(f, area, &mut state);
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let text = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer.get(x, y).symbol().chars().next().unwrap_or(' '))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        // Title bar present.
        assert!(text.contains("Inbox"), "missing title in:\n{text}");
        // Empty-state hint present.
        assert!(
            text.contains("notifications.db not found") || text.contains("no notifications yet"),
            "missing empty-state hint in:\n{text}"
        );
        // Help bar keys visible.
        assert!(text.contains("Enter"), "missing help bar in:\n{text}");
    }
}
