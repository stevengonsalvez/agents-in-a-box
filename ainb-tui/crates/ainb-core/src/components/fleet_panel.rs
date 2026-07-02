// ABOUTME: Fleet control panel — the interactive "who-needs-you" looking-glass.
//
// A two-pane (list + detail) screen that reads the SAME event-sourced
// `current_state` table the fleet readers use (Wave 4), opening the
// `ainb-plugin-notifyd` SQLite store strictly READ-ONLY (`Store::open_readonly`
// — SQLITE_OPEN_READ_ONLY, no migrate/create/WAL-write) on the render tick so
// the panel can never mutate the daemon's DB. The list shows one row per
// session that has a
// materialized state (ASK/ERR/WAIT/IDLE/RUNNING/DONE); the detail pane expands
// the selected row — for ASK it renders the full question + every option so the
// human can pick one.
//
// Unlike the read-only Daemons screen, this panel ACTS: answering an interview
// (ASK) and broadcasting a prompt both go through the EXISTING fleet send path
// (`fleet::send::send`, the same tmux send-keys mechanism `fleet broadcast`
// uses). The send is async and the TUI key path is sync, so the action is
// dispatched onto a detached worker thread that owns a tiny current-thread tokio
// runtime — the UI render loop never blocks on tmux I/O (mirrors the
// off-the-render-thread discipline in `components/daemons.rs`).
//
// Keys:
//   - ↑ / ↓ / k / j     move the row selection
//   - Tab / Shift+Tab    move the ASK option cursor (when the row is an ASK)
//   - Enter / a          answer the selected ASK with the highlighted option
//   - B                  broadcast a ping prompt to the selected session
//   - y                  approve the selected APPROVE permission request
//   - n                  deny (on an APPROVE row) / open the new-ATC prompt (elsewhere)
//   - r                  force-refresh from the store
//   - q / Esc            back to the previous screen
//
// Style follows the ainb-tui guide (rounded borders, gold titles,
// cornflower-blue panels, selection-green indicator), matching Inbox/Daemons.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ainb_plugin_notifyd::{Paths, StateRow, Store};
use ratatui::{
    prelude::*,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, List, ListItem, ListState, Paragraph, Wrap},
};

pub use crate::fleet::control::ActionFeedback;
use crate::fleet::read::jsonl_tail::AskUserQuestionData;

// Palette shared with the rest of ainb-tui (see components/layout.rs).
const CORNFLOWER_BLUE: Color = Color::Rgb(100, 149, 237);
const GOLD: Color = Color::Rgb(255, 215, 0);
const SELECTION_GREEN: Color = Color::Rgb(100, 200, 100);
const SOFT_WHITE: Color = Color::Rgb(220, 220, 230);
const MUTED_GRAY: Color = Color::Rgb(120, 120, 140);
const PANEL_BG: Color = Color::Rgb(30, 30, 40);
const SUBDUED_BORDER: Color = Color::Rgb(60, 60, 80);
const ALERT_RED: Color = Color::Rgb(220, 100, 100);
const WAIT_AMBER: Color = Color::Rgb(220, 180, 90);

/// Re-query the store at most this often (in render ticks). The query is a
/// single indexed `SELECT` over a small table behind WAL, so every-tick is
/// cheap, but keeping a counter mirrors the Inbox screen's discipline.
const POLL_TICKS: u64 = 1;
/// Window in which an identical dispatch is treated as an accidental double tap.
const DUPLICATE_DISPATCH_WINDOW: Duration = Duration::from_secs(2);

/// Last dispatch memo used for short-window duplicate suppression.
#[derive(Debug, Clone)]
pub struct DispatchMemo {
    session_id: String,
    text: String,
    row_ts: Option<i64>,
    at: Instant,
}

impl DispatchMemo {
    fn matches(&self, session_id: &str, text: &str, row_ts: Option<i64>) -> bool {
        self.session_id == session_id && self.text == text && self.row_ts == row_ts
    }
}

/// All state owned by the Fleet panel. Stored at app-level (like
/// `InboxState`/`DaemonsState`) so the selection + cached rows survive
/// cross-screen navigation.
///
/// `Debug` is hand-rolled to skip the `store` field (rusqlite `Connection`
/// isn't `Debug`) and the shared-feedback `Arc<Mutex<_>>`.
pub struct FleetPanelState {
    /// Currently selected row index into `rows`.
    pub selected: usize,
    /// Within an ASK row, which option is highlighted for answering.
    pub option_cursor: usize,
    /// Most-recently-fetched `current_state` rows (newest first, as the store
    /// returns them ordered by `last_event_ts DESC`).
    pub rows: Vec<StateRow>,
    /// Read-only handle to the notifyd store. `None` until first opened or when
    /// the DB is absent/unreadable (the screen then shows a friendly empty
    /// state, never crashes).
    pub store: Option<Store>,
    /// Cached store path for diagnostics in the empty state.
    pub db_path: std::path::PathBuf,
    /// Render-tick counter gating the re-query cadence.
    pub tick: u64,
    /// Transient feedback published by the async action worker. Cloned cheaply;
    /// the lock is held only for the microseconds it takes to read/replace the
    /// line — never across the actual send I/O.
    pub feedback: Arc<Mutex<ActionFeedback>>,
    /// In-flight guard shared with the action worker: set while a send is
    /// pending so a second Enter/`a`/`B` (key-repeat) is refused rather than
    /// spawning another worker / double-delivering (C3). Cleared by the worker
    /// on completion.
    pub in_flight: Arc<AtomicBool>,
    /// Last dispatch, used to debounce only rapid identical re-sends. The row
    /// timestamp distinguishes later prompts with the same label, and the time
    /// window prevents a stale memo from rejecting legitimate future sends.
    pub last_dispatch: Option<DispatchMemo>,
    /// When `Some`, the "new ATC" name prompt is active and captures keystrokes
    /// (the buffer being typed). `None` = normal browse mode. Submitting shells
    /// out to `ainb fleet atc setup <name>` to bootstrap a fleet member.
    pub new_atc_input: Option<String>,
}

impl Default for FleetPanelState {
    fn default() -> Self {
        let paths = Paths::from_home().ok();
        let db_path = paths.as_ref().map(|p| p.db.clone()).unwrap_or_default();
        let store = if db_path.exists() {
            // READ-ONLY: the panel only reads current_state on the render tick;
            // it must never migrate/create/WAL-write the daemon's DB.
            Store::open_readonly(&db_path)
                .map_err(|e| {
                    tracing::warn!(error = ?e, path = %db_path.display(), "fleet panel: eager store open failed");
                })
                .ok()
        } else {
            None
        };
        Self {
            selected: 0,
            option_cursor: 0,
            rows: Vec::new(),
            store,
            db_path,
            tick: 0,
            feedback: Arc::new(Mutex::new(ActionFeedback::default())),
            in_flight: Arc::new(AtomicBool::new(false)),
            last_dispatch: None,
            new_atc_input: None,
        }
    }
}

impl std::fmt::Debug for FleetPanelState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FleetPanelState")
            .field("selected", &self.selected)
            .field("option_cursor", &self.option_cursor)
            .field("rows.len", &self.rows.len())
            .field("store_open", &self.store.is_some())
            .field("db_path", &self.db_path)
            .field("tick", &self.tick)
            .finish()
    }
}

impl FleetPanelState {
    /// Lazily open the store when the DB file exists. Idempotent.
    pub fn ensure_open(&mut self) {
        if self.store.is_none() && self.db_path.exists() {
            match Store::open_readonly(&self.db_path) {
                Ok(s) => self.store = Some(s),
                Err(e) => {
                    tracing::warn!(error = ?e, path = %self.db_path.display(), "fleet panel: store open failed");
                }
            }
        }
    }

    /// Re-fetch `current_state` rows from the store, clamping selection into the
    /// new bounds. Best-effort: a query error leaves the prior rows in place.
    pub fn refresh(&mut self) {
        self.ensure_open();
        if let Some(store) = self.store.as_ref() {
            match store.list_current_state() {
                Ok(rows) => {
                    self.rows = rows;
                    if self.selected >= self.rows.len() {
                        self.selected = self.rows.len().saturating_sub(1);
                    }
                    self.clamp_option_cursor();
                }
                Err(e) => tracing::warn!(error = ?e, "fleet panel: list_current_state failed"),
            }
        }
    }

    /// Arm the screen on navigation INTO it: open + refresh so the first frame
    /// is populated. Idempotent.
    pub fn arm(&mut self) {
        self.refresh();
    }

    /// Currently-selected row, if any.
    #[must_use]
    pub fn selected_row(&self) -> Option<&StateRow> {
        self.rows.get(self.selected)
    }

    /// Kind of the selected row (`"APPROVE"`, `"ASK"`, …). The key router uses
    /// this to split the overloaded `n`: deny on an APPROVE row, new-ATC prompt
    /// everywhere else.
    pub fn selected_kind(&self) -> Option<&str> {
        self.selected_row().map(|r| r.kind.as_str())
    }

    /// Move the row selection up by `n`, saturating at 0; resets the option
    /// cursor since a different row may have a different option set.
    pub fn move_up(&mut self, n: usize) {
        self.selected = self.selected.saturating_sub(n);
        self.option_cursor = 0;
    }

    /// Move the row selection down by `n`, saturating at the last row.
    pub fn move_down(&mut self, n: usize) {
        if self.rows.is_empty() {
            self.selected = 0;
        } else {
            self.selected = (self.selected + n).min(self.rows.len() - 1);
        }
        self.option_cursor = 0;
    }

    /// The parsed ASK payload for the selected row, if it is an ASK.
    #[must_use]
    pub fn selected_ask(&self) -> Option<AskUserQuestionData> {
        let row = self.selected_row()?;
        if row.kind != "ASK" {
            return None;
        }
        parse_ask(row.context.as_deref())
    }

    /// Move the ASK option cursor, wrapping within the option count.
    pub fn option_next(&mut self) {
        if let Some(ask) = self.selected_ask() {
            let n = ask.options.len().max(1);
            self.option_cursor = (self.option_cursor + 1) % n;
        }
    }

    /// Move the ASK option cursor backwards, wrapping.
    pub fn option_prev(&mut self) {
        if let Some(ask) = self.selected_ask() {
            let n = ask.options.len().max(1);
            self.option_cursor = (self.option_cursor + n - 1) % n;
        }
    }

    /// Keep the option cursor within the selected row's option count.
    fn clamp_option_cursor(&mut self) {
        let opts = self.selected_ask().map(|a| a.options.len()).unwrap_or(0);
        if opts == 0 {
            self.option_cursor = 0;
        } else if self.option_cursor >= opts {
            self.option_cursor = opts - 1;
        }
    }

    /// The answer text the user would send for the selected ASK (the
    /// highlighted option's label), if a row + option are selected.
    #[must_use]
    pub fn pending_answer(&self) -> Option<(StateRow, String)> {
        let row = self.selected_row()?.clone();
        let ask = self.selected_ask()?;
        let label = ask.options.get(self.option_cursor)?.label.clone();
        Some((row, label))
    }

    /// Publish a transient feedback line (called from the UI thread when an
    /// action is fired; the worker overwrites it with the final outcome).
    pub fn set_feedback(&self, message: impl Into<String>) {
        if let Ok(mut g) = self.feedback.lock() {
            g.message = message.into();
        }
    }

    /// Snapshot the current feedback line.
    #[must_use]
    pub fn feedback_line(&self) -> String {
        self.feedback.lock().map(|g| g.message.clone()).unwrap_or_default()
    }

    /// True while the "new ATC" name prompt is capturing keystrokes.
    #[must_use]
    pub fn is_naming_atc(&self) -> bool {
        self.new_atc_input.is_some()
    }

    /// Open the new-ATC name prompt (bound to `n`). Refused while an action is
    /// in flight so the create can't race a pending send.
    pub fn open_new_atc(&mut self) {
        if self.in_flight.load(Ordering::Acquire) {
            self.set_feedback("action already in flight — wait for it to finish");
            return;
        }
        self.new_atc_input = Some(String::new());
    }

    /// Append a typed char to the name buffer (ignored outside prompt mode).
    /// Accepts exactly the chars the CLI keeps unchanged (`sanitize_instance_name`
    /// in `fleet/atc/paths.rs`: ASCII alnum, `-`, `_`) so the visible buffer is
    /// byte-for-byte what the created instance name will be — no silent rewrite.
    pub fn new_atc_type(&mut self, c: char) {
        if let Some(buf) = self.new_atc_input.as_mut() {
            if (c.is_ascii_alphanumeric() || c == '-' || c == '_') && buf.len() < 40 {
                buf.push(c);
            }
        }
    }

    /// Delete the last char of the name buffer.
    pub fn new_atc_backspace(&mut self) {
        if let Some(buf) = self.new_atc_input.as_mut() {
            buf.pop();
        }
    }

    /// Cancel the name prompt without creating anything.
    pub fn new_atc_cancel(&mut self) {
        self.new_atc_input = None;
    }

    /// Submit the name prompt: validate non-empty, then dispatch the CLI setup
    /// off the UI thread. Keeps the prompt open on an empty/invalid name so the
    /// user can correct it.
    pub fn new_atc_submit(&mut self) {
        let Some(raw) = self.new_atc_input.clone() else {
            return;
        };
        let name = raw.trim().to_string();
        if name.is_empty() {
            self.set_feedback("name required — type a name, Enter to create, Esc to cancel");
            return;
        }
        self.new_atc_input = None;
        crate::fleet::control::dispatch_new_atc(
            Arc::clone(&self.feedback),
            Arc::clone(&self.in_flight),
            name,
        );
    }

    /// True when a send is currently in flight (a worker is running).
    #[must_use]
    pub fn is_sending(&self) -> bool {
        self.in_flight.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Guarded dispatch shared by Answer + Broadcast: refuses a re-send while one
    /// is in flight (C3) and debounces a rapid IDENTICAL `(session_id, text)`
    /// re-send. Returns `true` if a send was actually dispatched.
    ///
    /// The actual in-flight CAS lives in [`dispatch_send`]; this method also
    /// short-circuits BEFORE spawning so the feedback line and debounce memo are
    /// updated coherently on the UI thread.
    pub fn guarded_dispatch(
        &mut self,
        session_id: String,
        cwd: String,
        text: String,
        row_ts: Option<i64>,
        kind_label: &'static str,
        is_answer: bool,
    ) -> bool {
        if self.is_sending() {
            self.set_feedback("send already in flight — wait for it to finish".to_string());
            return false;
        }
        // Debounce an identical back-to-back re-send for a short window. The row
        // timestamp is part of the identity so a later prompt in the same
        // session with the same option label is a fresh answer.
        let now = Instant::now();
        if self.is_duplicate_dispatch(&session_id, &text, row_ts, now) {
            self.set_feedback("duplicate send ignored (same answer just sent)".to_string());
            return false;
        }
        self.remember_dispatch(session_id.clone(), text.clone(), row_ts, now);
        crate::fleet::control::dispatch_send(
            Arc::clone(&self.feedback),
            Arc::clone(&self.in_flight),
            session_id,
            cwd,
            text,
            kind_label,
            is_answer,
        );
        true
    }

    /// Guarded approve/deny for the selected APPROVE row: refuses while another
    /// action is in flight (shares the send guard) and only fires when the
    /// selected row is actually a waiting permission request (`APPROVE`).
    /// Returns `true` if a decision was dispatched.
    ///
    /// Unlike [`guarded_dispatch`], this delivers a first-class permission
    /// decision to the notifyd approve broker (`dispatch_decide`), which unblocks
    /// the parked `PermissionRequest` hook — it does NOT route text via tmux.
    pub fn guarded_decide(
        &mut self,
        kind: ainb_plugin_notifyd::broker::DecisionKind,
        kind_label: &'static str,
    ) -> bool {
        if self.is_sending() {
            self.set_feedback("action already in flight — wait for it to finish".to_string());
            return false;
        }
        let Some(row) = self.selected_row() else {
            return false;
        };
        if row.kind != "APPROVE" {
            self.set_feedback("no permission request selected".to_string());
            return false;
        }
        let session_id = row.session_id.clone();
        if session_id.is_empty() {
            self.set_feedback("cannot decide: row has no session id".to_string());
            return false;
        }
        self.set_feedback(format!("{kind_label} → {session_id}: delivering…"));
        crate::fleet::control::dispatch_decide(
            Arc::clone(&self.feedback),
            Arc::clone(&self.in_flight),
            session_id,
            kind,
            String::new(),
            kind_label,
        );
        true
    }

    fn is_duplicate_dispatch(
        &self,
        session_id: &str,
        text: &str,
        row_ts: Option<i64>,
        now: Instant,
    ) -> bool {
        self.last_dispatch.as_ref().is_some_and(|last| {
            last.matches(session_id, text, row_ts)
                && now.duration_since(last.at) < DUPLICATE_DISPATCH_WINDOW
        })
    }

    fn remember_dispatch(
        &mut self,
        session_id: String,
        text: String,
        row_ts: Option<i64>,
        at: Instant,
    ) {
        self.last_dispatch = Some(DispatchMemo {
            session_id,
            text,
            row_ts,
            at,
        });
    }
}

/// Parse the ASK context JSON the materializer stored (the AskUserQuestion
/// payload). Returns a placeholder question with no options if it can't parse,
/// so the detail pane always renders something rather than blanking.
fn parse_ask(context_json: Option<&str>) -> Option<AskUserQuestionData> {
    Some(
        context_json
            .and_then(|c| serde_json::from_str::<AskUserQuestionData>(c).ok())
            .unwrap_or_else(|| AskUserQuestionData {
                question: "(no question text in current_state)".to_string(),
                header: None,
                options: Vec::new(),
                multi_select: false,
            }),
    )
}

/// Kind badge text + colour. Mirrors the classifier's ASK>ERR>WAIT>IDLE
/// priority colouring; RUNNING/DONE are healthy/terminal lifecycle states.
fn kind_badge(kind: &str) -> (&'static str, Color) {
    match kind {
        "ASK" => ("ASK ", GOLD),
        "APPROVE" => ("APRV", GOLD),
        "ERR" => ("ERR ", ALERT_RED),
        "WAIT" => ("WAIT", WAIT_AMBER),
        "IDLE" => ("IDLE", MUTED_GRAY),
        "RUNNING" => ("RUN ", SELECTION_GREEN),
        "STARTING" => ("STRT", CORNFLOWER_BLUE),
        "DONE" => ("DONE", CORNFLOWER_BLUE),
        _ => ("????", MUTED_GRAY),
    }
}

/// A short, human label for a session: the cwd's final path component, or the
/// session id when the cwd is empty.
fn short_session(row: &StateRow) -> String {
    let base = row.cwd.rsplit(['/', '\\']).find(|s| !s.is_empty()).unwrap_or("");
    if base.is_empty() {
        let id = &row.session_id;
        let n = id.chars().count();
        if n <= 12 {
            id.clone()
        } else {
            id.chars().take(12).collect()
        }
    } else {
        base.to_string()
    }
}

/// A one-line context summary for the list row.
fn short_context(row: &StateRow) -> String {
    match row.kind.as_str() {
        "ASK" => parse_ask(row.context.as_deref())
            .map(|a| truncate_chars(&a.question, 40))
            .unwrap_or_default(),
        "APPROVE" => "needs approval".to_string(),
        "STARTING" => "starting".to_string(),
        "ERR" => row
            .context
            .as_deref()
            .and_then(|c| serde_json::from_str::<serde_json::Value>(c).ok())
            .and_then(|v| v.get("error_type").and_then(|t| t.as_str()).map(str::to_string))
            .unwrap_or_else(|| "error".to_string()),
        "WAIT" => row
            .context
            .as_deref()
            .and_then(|c| serde_json::from_str::<serde_json::Value>(c).ok())
            .and_then(|v| {
                v.get("message")
                    .and_then(|m| m.as_str())
                    .or_else(|| v.get("reason").and_then(|r| r.as_str()))
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "waiting".to_string()),
        "IDLE" => {
            let mins = row
                .context
                .as_deref()
                .and_then(|c| serde_json::from_str::<serde_json::Value>(c).ok())
                .and_then(|v| v.get("idle_minutes").and_then(serde_json::Value::as_i64))
                .unwrap_or(0);
            format!("idle {mins}m")
        }
        "RUNNING" => "working".to_string(),
        "DONE" => "finished".to_string(),
        _ => String::new(),
    }
}

/// Truncate to `max` display chars with a trailing ellipsis on overflow.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let prefix: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{prefix}…")
    }
}

/// Render the Fleet panel into `area`.
pub fn render(frame: &mut Frame, area: Rect, state: &mut FleetPanelState) {
    state.tick = state.tick.wrapping_add(1);
    if state.tick % POLL_TICKS == 0 {
        state.refresh();
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);

    render_title(frame, chunks[0], state);
    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(chunks[1]);
    render_list(frame, panes[0], state);
    render_detail(frame, panes[1], state);
    render_status(frame, chunks[2], state);
    render_help(frame, chunks[3], state);
}

fn render_title(frame: &mut Frame, area: Rect, state: &FleetPanelState) {
    let total = state.rows.len();
    let needs = state
        .rows
        .iter()
        .filter(|r| matches!(r.kind.as_str(), "ASK" | "ERR" | "WAIT" | "IDLE" | "APPROVE"))
        .count();
    let title = Line::from(vec![
        Span::styled(
            "🛫 Fleet ",
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("· {total} sessions · {needs} need attention "),
            Style::default().fg(SOFT_WHITE),
        ),
        Span::styled("· current_state", Style::default().fg(MUTED_GRAY)),
    ]);
    frame.render_widget(Paragraph::new(title), area);
}

fn render_list(frame: &mut Frame, area: Rect, state: &mut FleetPanelState) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CORNFLOWER_BLUE))
        .style(Style::default().bg(PANEL_BG))
        .title(Span::styled(
            " sessions ",
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ));

    if state.rows.is_empty() {
        let msg = if state.store.is_some() {
            "(no fleet state yet — press n to create an ATC session, or fire a hook)"
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
            let (badge, badge_color) = kind_badge(&r.kind);
            let src = if r.source == "hook" { "" } else { " ~tmux" };
            let row = Line::from(vec![
                Span::styled(
                    badge,
                    Style::default().fg(badge_color).add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::styled(
                    format!("{:<16}", truncate_chars(&short_session(r), 16)),
                    Style::default().fg(SOFT_WHITE),
                ),
                Span::raw(" "),
                Span::styled(short_context(r), Style::default().fg(MUTED_GRAY)),
                Span::styled(src, Style::default().fg(SUBDUED_BORDER)),
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

fn render_detail(frame: &mut Frame, area: Rect, state: &FleetPanelState) {
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
        let paragraph = Paragraph::new("(select a session to view detail)")
            .style(Style::default().fg(MUTED_GRAY))
            .block(block);
        frame.render_widget(paragraph, area);
        return;
    };

    let mut lines: Vec<Line> = Vec::new();
    let label = |k: &str, v: String| {
        Line::from(vec![
            Span::styled(format!("{k:<9}"), Style::default().fg(MUTED_GRAY)),
            Span::styled(v, Style::default().fg(SOFT_WHITE)),
        ])
    };
    let (badge, badge_color) = kind_badge(&row.kind);
    lines.push(Line::from(vec![
        Span::styled("kind:    ", Style::default().fg(MUTED_GRAY)),
        Span::styled(
            badge.trim().to_string(),
            Style::default().fg(badge_color).add_modifier(Modifier::BOLD),
        ),
    ]));
    lines.push(label("session:", row.session_id.clone()));
    lines.push(label("cwd:", row.cwd.clone()));
    lines.push(label("source:", row.source.clone()));
    if let Some(p) = &row.parent {
        lines.push(label("parent:", p.clone()));
    }
    lines.push(Line::from(""));

    match row.kind.as_str() {
        "ASK" => render_ask_detail(&mut lines, row, state.option_cursor),
        "APPROVE" => {
            let v = row
                .context
                .as_deref()
                .and_then(|c| serde_json::from_str::<serde_json::Value>(c).ok());
            let tool = v
                .as_ref()
                .and_then(|v| v.get("tool").and_then(|t| t.as_str()))
                .unwrap_or("(unknown)");
            let input = v
                .as_ref()
                .and_then(|v| v.get("tool_input"))
                .map(|i| i.to_string())
                .unwrap_or_else(|| "null".to_string());
            lines.push(Line::from(Span::styled(
                "needs approval:",
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            )));
            lines.push(label("tool:", tool.to_string()));
            lines.push(label("input:", input));
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled("y", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
                Span::styled(" approve   ", Style::default().fg(MUTED_GRAY)),
                Span::styled("n", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
                Span::styled(" deny", Style::default().fg(MUTED_GRAY)),
            ]));
        }
        "ERR" => {
            let et = row
                .context
                .as_deref()
                .and_then(|c| serde_json::from_str::<serde_json::Value>(c).ok())
                .and_then(|v| v.get("error_type").and_then(|t| t.as_str()).map(str::to_string))
                .unwrap_or_else(|| "unknown".to_string());
            lines.push(Line::from(Span::styled(
                "error:",
                Style::default().fg(ALERT_RED).add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(Span::styled(
                et,
                Style::default().fg(SOFT_WHITE),
            )));
        }
        "WAIT" => {
            let v = row
                .context
                .as_deref()
                .and_then(|c| serde_json::from_str::<serde_json::Value>(c).ok());
            let reason = v
                .as_ref()
                .and_then(|v| v.get("reason").and_then(|r| r.as_str()))
                .unwrap_or("notification");
            let message = v
                .as_ref()
                .and_then(|v| {
                    v.get("message")
                        .and_then(|m| m.as_str())
                        .or_else(|| v.get("tool").and_then(|t| t.as_str()))
                })
                .unwrap_or("");
            lines.push(Line::from(Span::styled(
                "waiting on:",
                Style::default().fg(WAIT_AMBER).add_modifier(Modifier::BOLD),
            )));
            lines.push(label("reason:", reason.to_string()));
            if !message.is_empty() {
                lines.push(label("detail:", message.to_string()));
            }
        }
        "IDLE" => {
            let mins = row
                .context
                .as_deref()
                .and_then(|c| serde_json::from_str::<serde_json::Value>(c).ok())
                .and_then(|v| v.get("idle_minutes").and_then(serde_json::Value::as_i64))
                .unwrap_or(0);
            lines.push(Line::from(Span::styled(
                "idle:",
                Style::default().fg(MUTED_GRAY).add_modifier(Modifier::BOLD),
            )));
            lines.push(label("minutes:", mins.to_string()));
        }
        "RUNNING" => lines.push(Line::from(Span::styled(
            "actively working — no action needed",
            Style::default().fg(SELECTION_GREEN),
        ))),
        "STARTING" => lines.push(Line::from(Span::styled(
            "starting — session is booting",
            Style::default().fg(CORNFLOWER_BLUE),
        ))),
        "DONE" => lines.push(Line::from(Span::styled(
            "finished — completion delivered via the inbox",
            Style::default().fg(CORNFLOWER_BLUE),
        ))),
        _ => {}
    }

    let paragraph = Paragraph::new(lines).block(block).wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

/// Render the ASK detail: the question, header, and every option with the
/// answer cursor highlighted so the user can see which one Enter will send.
fn render_ask_detail(lines: &mut Vec<Line<'static>>, row: &StateRow, option_cursor: usize) {
    let ask = parse_ask(row.context.as_deref()).unwrap_or(AskUserQuestionData {
        question: String::new(),
        header: None,
        options: Vec::new(),
        multi_select: false,
    });
    if let Some(h) = &ask.header {
        lines.push(Line::from(Span::styled(
            h.clone(),
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        )));
    }
    lines.push(Line::from(Span::styled(
        ask.question.clone(),
        Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));
    if ask.options.is_empty() {
        lines.push(Line::from(Span::styled(
            "(no options — broadcast a free-text reply with B)",
            Style::default().fg(MUTED_GRAY),
        )));
        return;
    }
    lines.push(Line::from(Span::styled(
        "options (Tab to move, Enter to send):",
        Style::default().fg(MUTED_GRAY),
    )));
    for (i, opt) in ask.options.iter().enumerate() {
        let selected = i == option_cursor;
        let marker = if selected { "▶ " } else { "  " };
        let style = if selected {
            Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(SOFT_WHITE)
        };
        lines.push(Line::from(vec![
            Span::styled(marker, style),
            Span::styled(opt.label.clone(), style),
        ]));
        if let Some(d) = &opt.description {
            lines.push(Line::from(vec![
                Span::raw("    "),
                Span::styled(d.clone(), Style::default().fg(MUTED_GRAY)),
            ]));
        }
    }
}

fn render_status(frame: &mut Frame, area: Rect, state: &FleetPanelState) {
    // The name prompt takes over the status line while active, so the user sees
    // what they're typing (this screen has no separate input widget).
    if let Some(buf) = &state.new_atc_input {
        let prompt = Line::from(vec![
            Span::styled("new ATC name: ", Style::default().fg(GOLD)),
            Span::styled(format!("{buf}_"), Style::default().fg(SOFT_WHITE)),
        ]);
        frame.render_widget(
            Paragraph::new(prompt).style(Style::default().bg(PANEL_BG)),
            area,
        );
        return;
    }
    let line = state.feedback_line();
    let span = if line.is_empty() {
        Span::styled(
            "ready · select a session, press Enter to answer an ASK or B to broadcast",
            Style::default().fg(MUTED_GRAY),
        )
    } else {
        Span::styled(line, Style::default().fg(SELECTION_GREEN))
    };
    frame.render_widget(
        Paragraph::new(Line::from(span)).style(Style::default().bg(PANEL_BG)),
        area,
    );
}

fn render_help(frame: &mut Frame, area: Rect, state: &FleetPanelState) {
    // While naming a new ATC the only valid keys are Enter/Esc — show those
    // instead of the browse legend so the help bar never lies about the mode.
    let spans = if state.new_atc_input.is_some() {
        vec![
            Span::styled("type", Style::default().fg(GOLD)),
            Span::styled(" name  ", Style::default().fg(MUTED_GRAY)),
            Span::styled("Enter", Style::default().fg(GOLD)),
            Span::styled(" create  ", Style::default().fg(MUTED_GRAY)),
            Span::styled("Esc", Style::default().fg(GOLD)),
            Span::styled(" cancel", Style::default().fg(MUTED_GRAY)),
        ]
    } else {
        vec![
            Span::styled("↑↓", Style::default().fg(GOLD)),
            Span::styled(" move  ", Style::default().fg(MUTED_GRAY)),
            Span::styled("Tab", Style::default().fg(GOLD)),
            Span::styled(" option  ", Style::default().fg(MUTED_GRAY)),
            Span::styled("Enter/a", Style::default().fg(GOLD)),
            Span::styled(" answer  ", Style::default().fg(MUTED_GRAY)),
            Span::styled("B", Style::default().fg(GOLD)),
            Span::styled(" broadcast  ", Style::default().fg(MUTED_GRAY)),
            Span::styled("n", Style::default().fg(GOLD)),
            // `n` is context-sensitive: deny on an APPROVE row (paired with the
            // detail pane's y/n hint), the new-ATC prompt everywhere else. The
            // help bar must say which one THIS selection gets.
            Span::styled(
                if state.selected_kind() == Some("APPROVE") {
                    " deny  "
                } else {
                    " new-atc  "
                },
                Style::default().fg(MUTED_GRAY),
            ),
            Span::styled("r", Style::default().fg(GOLD)),
            Span::styled(" refresh  ", Style::default().fg(MUTED_GRAY)),
            Span::styled("q/Esc", Style::default().fg(GOLD)),
            Span::styled(" back", Style::default().fg(MUTED_GRAY)),
        ]
    };
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    fn state_with(rows: Vec<StateRow>) -> FleetPanelState {
        let mut s = FleetPanelState::default();
        // Force the store closed + point the path at a non-existent DB so the
        // render tick's `refresh()` is a no-op and the seeded rows survive —
        // the render is exercised against a deterministic cache, never the
        // host's real ~/.agents-in-a-box/notifications.db (mirrors the Inbox
        // screen's test seam).
        s.store = None;
        s.db_path = std::path::PathBuf::from("/nonexistent/notifications.db");
        s.rows = rows;
        s.clamp_option_cursor();
        s
    }

    fn row(
        session_id: &str,
        cwd: &str,
        kind: &str,
        context: Option<&str>,
        source: &str,
    ) -> StateRow {
        StateRow {
            session_id: session_id.to_string(),
            cwd: cwd.to_string(),
            kind: kind.to_string(),
            context: context.map(str::to_string),
            parent: None,
            last_event_ts: 100,
            source: source.to_string(),
        }
    }

    /// Render the screen against an in-memory TestBackend and return the buffer
    /// as a single string for substring assertions.
    fn render_to_string(state: &mut FleetPanelState, w: u16, h: u16) -> String {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, f.area(), state)).unwrap();
        let buf = terminal.backend().buffer().clone();
        buf.content().iter().map(|c| c.symbol()).collect::<String>()
    }

    #[test]
    fn renders_title_list_rows_and_kind_badges() {
        let mut state = state_with(vec![
            row(
                "sess-ask",
                "/work/deploy",
                "ASK",
                Some(
                    r#"{"question":"Ship to which env?","header":"Deploy","options":[{"label":"staging","description":"safe"},{"label":"prod"}]}"#,
                ),
                "hook",
            ),
            row(
                "sess-err",
                "/work/api",
                "ERR",
                Some(r#"{"error_type":"rate_limit"}"#),
                "hook",
            ),
            row(
                "sess-idle",
                "/work/ui",
                "IDLE",
                Some(r#"{"idle_minutes":7}"#),
                "tmux",
            ),
            row("sess-run", "/work/lib", "RUNNING", None, "hook"),
        ]);
        let out = render_to_string(&mut state, 130, 24);
        // Title + counters.
        assert!(out.contains("Fleet"), "title missing: {out}");
        assert!(out.contains("4 sessions"), "session count missing: {out}");
        assert!(out.contains("need attention"), "needs counter missing");
        // Every kind badge renders in the list.
        assert!(out.contains("ASK"), "ASK badge missing");
        assert!(out.contains("ERR"), "ERR badge missing");
        assert!(out.contains("IDLE"), "IDLE badge missing");
        assert!(out.contains("RUN"), "RUNNING badge missing");
        // Session short-labels (cwd basename) render.
        assert!(out.contains("deploy"), "ASK session label missing");
        assert!(out.contains("api"), "ERR session label missing");
        // The selected (first) row is an ASK → detail shows the question + options.
        assert!(
            out.contains("Ship to which env?"),
            "ASK question missing: {out}"
        );
        assert!(out.contains("staging"), "ASK option 'staging' missing");
        assert!(out.contains("prod"), "ASK option 'prod' missing");
    }

    #[test]
    fn renders_approve_and_starting_badges() {
        let mut state = state_with(vec![
            row(
                "sess-approve",
                "/work/deploy",
                "APPROVE",
                Some(r#"{"tool":"Bash","tool_input":{"command":"rm -rf /tmp/x"}}"#),
                "hook",
            ),
            row("sess-starting", "/work/boot", "STARTING", None, "hook"),
        ]);
        let out = render_to_string(&mut state, 130, 24);
        assert!(out.contains("APRV"), "APPROVE badge missing: {out}");
        assert!(out.contains("STRT"), "STARTING badge missing: {out}");
        // APPROVE needs attention; the counter reflects it.
        assert!(
            out.contains("1 need attention"),
            "needs counter wrong: {out}"
        );
        // The selected (first) row is APPROVE → detail shows the tool.
        assert!(
            out.contains("needs approval"),
            "APPROVE detail missing: {out}"
        );
        assert!(out.contains("Bash"), "APPROVE tool missing: {out}");
    }

    #[test]
    fn detail_shows_err_wait_and_idle_context_for_selected_row() {
        // Select the ERR row.
        let mut state = state_with(vec![
            row(
                "sess-err",
                "/work/api",
                "ERR",
                Some(r#"{"error_type":"overloaded"}"#),
                "hook",
            ),
            row(
                "sess-wait",
                "/work/db",
                "WAIT",
                Some(r#"{"reason":"permission_prompt","message":"allow Bash?"}"#),
                "hook",
            ),
        ]);
        let out = render_to_string(&mut state, 130, 20);
        assert!(out.contains("overloaded"), "ERR error_type missing: {out}");

        state.move_down(1);
        let out = render_to_string(&mut state, 130, 20);
        assert!(
            out.contains("permission_prompt"),
            "WAIT reason missing: {out}"
        );
        assert!(out.contains("allow Bash?"), "WAIT message missing: {out}");
    }

    #[test]
    fn empty_state_renders_without_panic() {
        let mut state = FleetPanelState::default();
        state.store = None;
        state.db_path = std::path::PathBuf::from("/nonexistent/notifications.db");
        let out = render_to_string(&mut state, 100, 16);
        assert!(out.contains("Fleet"), "title missing in empty state: {out}");
        assert!(
            out.contains("notifications.db not found") || out.contains("no fleet state"),
            "empty-state hint missing: {out}"
        );
        assert!(out.contains("Enter/a"), "help bar missing: {out}");
    }

    #[test]
    fn option_cursor_wraps_within_the_selected_ask() {
        let mut state = state_with(vec![row(
            "s",
            "/p",
            "ASK",
            Some(r#"{"question":"q?","options":[{"label":"a"},{"label":"b"},{"label":"c"}]}"#),
            "hook",
        )]);
        assert_eq!(state.option_cursor, 0);
        state.option_next();
        assert_eq!(state.option_cursor, 1);
        state.option_next();
        state.option_next();
        assert_eq!(state.option_cursor, 0, "wraps past last option");
        state.option_prev();
        assert_eq!(state.option_cursor, 2, "wraps backwards from first");
    }

    #[test]
    fn pending_answer_returns_the_highlighted_option_label() {
        let mut state = state_with(vec![row(
            "s",
            "/p",
            "ASK",
            Some(r#"{"question":"q?","options":[{"label":"yes"},{"label":"no"}]}"#),
            "hook",
        )]);
        state.option_next(); // → "no"
        let (target, answer) = state.pending_answer().expect("ask row has an answer");
        assert_eq!(target.session_id, "s");
        assert_eq!(answer, "no");
    }

    #[test]
    fn guarded_decide_refuses_non_approve_row() {
        use ainb_plugin_notifyd::broker::DecisionKind;
        let mut state = state_with(vec![row(
            "s",
            "/p",
            "ASK",
            Some(r#"{"question":"q?","options":[{"label":"a"}]}"#),
            "hook",
        )]);
        // Selected row is ASK, not APPROVE → decide refused, no dispatch.
        assert!(!state.guarded_decide(DecisionKind::Approve, "approved"));
        assert!(!state.is_sending(), "no worker claimed for non-APPROVE row");
    }

    #[test]
    fn guarded_decide_refuses_empty_session_id() {
        use ainb_plugin_notifyd::broker::DecisionKind;
        let mut state = state_with(vec![row("", "/p", "APPROVE", None, "hook")]);
        assert!(!state.guarded_decide(DecisionKind::Deny, "denied"));
        assert!(
            !state.is_sending(),
            "empty session id must not claim a worker"
        );
    }

    #[test]
    fn pending_answer_none_for_non_ask_row() {
        let state = state_with(vec![row(
            "s",
            "/p",
            "IDLE",
            Some(r#"{"idle_minutes":3}"#),
            "hook",
        )]);
        assert!(state.pending_answer().is_none());
    }

    #[test]
    fn move_down_caps_and_resets_option_cursor() {
        let mut state = state_with(vec![
            row(
                "a",
                "/p1",
                "ASK",
                Some(r#"{"question":"q","options":[{"label":"x"},{"label":"y"}]}"#),
                "hook",
            ),
            row("b", "/p2", "IDLE", Some(r#"{"idle_minutes":1}"#), "hook"),
        ]);
        state.option_next();
        assert_eq!(state.option_cursor, 1);
        state.move_down(99);
        assert_eq!(state.selected, 1, "selection caps at last row");
        assert_eq!(state.option_cursor, 0, "option cursor resets on row move");
    }

    #[test]
    fn new_atc_prompt_captures_and_sanitizes_input() {
        let mut s = FleetPanelState::default();
        assert!(!s.is_naming_atc());
        s.open_new_atc();
        assert!(s.is_naming_atc(), "n should open the prompt");
        for c in "my atc!/9".chars() {
            s.new_atc_type(c);
        }
        // Spaces + punctuation are rejected; alnum/-/_ kept.
        assert_eq!(s.new_atc_input.as_deref(), Some("myatc9"));
        s.new_atc_backspace();
        assert_eq!(s.new_atc_input.as_deref(), Some("myatc"));
        s.new_atc_cancel();
        assert!(!s.is_naming_atc(), "Esc should close the prompt");
    }

    #[test]
    fn new_atc_submit_on_empty_keeps_prompt_open() {
        let mut s = FleetPanelState::default();
        s.open_new_atc();
        s.new_atc_submit(); // empty buffer → must not dispatch, must stay open
        assert!(
            s.is_naming_atc(),
            "empty submit should keep the prompt open"
        );
        assert!(s.feedback_line().contains("name required"));
    }

    #[test]
    fn feedback_round_trips_through_the_shared_cell() {
        let state = FleetPanelState::default();
        assert_eq!(state.feedback_line(), "");
        state.set_feedback("answered ask → /work/x: sent via tmux (sess-1)");
        assert!(state.feedback_line().contains("sent via tmux"));
    }

    #[test]
    fn duplicate_guard_is_limited_to_same_row_within_window() {
        let mut state = FleetPanelState::default();
        let now = Instant::now();
        state.remember_dispatch("sess".into(), "Yes".into(), Some(100), now);

        assert!(
            state.is_duplicate_dispatch("sess", "Yes", Some(100), now + Duration::from_millis(500)),
            "same row/text inside debounce window must be rejected"
        );
        assert!(
            !state.is_duplicate_dispatch(
                "sess",
                "Yes",
                Some(101),
                now + Duration::from_millis(500)
            ),
            "new prompt row with same label must be accepted"
        );
        assert!(
            !state.is_duplicate_dispatch(
                "sess",
                "Yes",
                Some(100),
                now + DUPLICATE_DISPATCH_WINDOW + Duration::from_millis(1)
            ),
            "same text after debounce window must be accepted"
        );
    }

    #[test]
    fn short_session_falls_back_to_truncated_id_when_cwd_empty() {
        let r = row("0123456789abcdef-long", "", "RUNNING", None, "hook");
        assert_eq!(short_session(&r), "0123456789ab");
        let r2 = row("short-id", "", "RUNNING", None, "hook");
        assert_eq!(short_session(&r2), "short-id");
    }
}
