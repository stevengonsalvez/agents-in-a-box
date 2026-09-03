// ABOUTME: Fleet control panel — the interactive "who-needs-you" looking-glass.
//
// A two-pane (list + detail) screen backed by Hangar's authoritative Fleet
// snapshot RPC. Socket reads run on a worker thread, cached rows survive daemon
// outages, and selection is restored by stable session key after each refresh.
// Lifecycle and attention stay independent in the wire model, then map onto the
// compact legacy badges rendered here.
//
// Actions use versioned `fleet/action` RPC receipts. Structured answers never
// become generic text, and stale request fingerprints are rejected by Hangar.
//
// Keys:
//   - ↑ / ↓ / k / j     move the row selection
//   - Tab / Shift+Tab    move the ASK option cursor (when the row is an ASK)
//   - Enter / a          answer the selected ASK with the highlighted option
//   - B                  broadcast a ping prompt to the selected session
//   - y                  approve the selected APPROVE permission request
//   - n                  deny (on an APPROVE row) / open the new-ATC prompt (elsewhere)
//   - F5                 force-refresh from the daemon snapshot
//   - q / Esc            back to the previous screen
//
// Style follows the ainb-tui guide (rounded borders, gold titles,
// cornflower-blue panels, selection-green indicator), matching Inbox/Daemons.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ainb_hangar_proto::fleet::FleetSession;
use ainb_plugin_hangar::screen::fleet::{
    FleetCapabilities, FleetEvent, FleetIntent, FleetPaneState, FleetSessionRow, reduce_fleet,
};
use ainb_plugin_notifyd::StateRow;
use ratatui::{
    prelude::*,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, List, ListItem, ListState, Paragraph, Wrap},
};

pub use crate::fleet::control::ActionFeedback;
use crate::fleet::control::{
    FleetDaemonHealth, FleetHostUpdate, FleetHostUpdateSink, FleetSnapshotUpdate,
};
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
    pub store: Option<()>,
    /// Cached store path for diagnostics in the empty state.
    pub db_path: std::path::PathBuf,
    /// Latest authoritative session metadata keyed by stable session key.
    pub session_meta: HashMap<String, FleetSession>,
    /// Canonical Fleet reducer shared with Hangar plugin pane.
    pub canonical: FleetPaneState,
    /// Ordered snapshots and connection health from the persistent stream.
    stream_updates: FleetHostUpdateSink,
    /// Persistent stream starts lazily when the operator opens Fleet.
    stream_started: bool,
    /// Current authoritative transport health for action gating and rendering.
    daemon_health: FleetDaemonHealth,
    /// Worker-produced reducer events awaiting UI-thread application.
    canonical_updates: Arc<Mutex<Vec<FleetEvent>>>,
    /// Render-tick counter retained for deterministic UI tests and animation.
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
        let db_path = crate::fleet::bridge::daemon::socket_path().unwrap_or_default();
        Self {
            selected: 0,
            option_cursor: 0,
            rows: Vec::new(),
            store: None,
            db_path,
            session_meta: HashMap::new(),
            canonical: FleetPaneState::default(),
            stream_updates: Arc::new(Mutex::new(Vec::new())),
            stream_started: false,
            daemon_health: FleetDaemonHealth::Offline("Fleet stream not started".into()),
            canonical_updates: Arc::new(Mutex::new(Vec::new())),
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
            .field("fleet_revision", &self.canonical.head_revision())
            .field("daemon_health", &self.daemon_health)
            .finish()
    }
}

impl FleetPanelState {
    /// Drain persistent stream and action updates without performing socket IO.
    pub fn refresh(&mut self) {
        self.drain_stream_updates();
        self.drain_canonical_updates();
    }

    /// Request a fresh daemon snapshot on a worker thread, then apply any
    /// completed stream/action updates immediately.
    pub fn force_refresh(&mut self) {
        crate::fleet::control::request_fleet_snapshot(Arc::clone(&self.stream_updates));
        self.refresh();
    }

    fn start_subscription(&mut self) {
        if self.stream_started {
            return;
        }
        self.stream_started = true;
        self.daemon_health = FleetDaemonHealth::Connecting;
        if let Err(error) = crate::fleet::control::spawn_fleet_subscription(
            Arc::clone(&self.stream_updates),
            self.canonical.head_revision(),
        ) {
            self.daemon_health = FleetDaemonHealth::Offline(error.clone());
            self.set_feedback(format!("Fleet subscription failed: {error}"));
        }
    }

    fn apply_snapshot(&mut self, update: FleetSnapshotUpdate) {
        let FleetSnapshotUpdate {
            snapshot,
            git_contexts,
        } = update;
        let sessions = snapshot.sessions;
        let selected_key = self.selected_row().map(|row| row.session_id.clone());
        self.session_meta = sessions
            .iter()
            .cloned()
            .map(|session| (session.session_key.clone(), session))
            .collect();
        self.rows = sessions.iter().map(state_row_from_fleet).collect();
        self.rows.sort_by_key(|row| std::cmp::Reverse(row.last_event_ts));
        self.canonical.apply_snapshot(
            snapshot.head_revision,
            sessions
                .into_iter()
                .map(|session| {
                    let context = git_contexts.get(&session.cwd).cloned().unwrap_or_default();
                    let mut row = FleetSessionRow::from(session);
                    row.repository_name = context.repository_name;
                    row.branch_name = context.branch_name;
                    row
                })
                .collect(),
        );
        self.selected = selected_key
            .as_ref()
            .and_then(|key| self.rows.iter().position(|row| &row.session_id == key))
            .unwrap_or_else(|| self.selected.min(self.rows.len().saturating_sub(1)));
        self.clamp_option_cursor();
    }

    fn drain_stream_updates(&mut self) {
        let updates = self
            .stream_updates
            .lock()
            .ok()
            .map(|mut updates| std::mem::take(&mut *updates))
            .unwrap_or_default();
        let (health_updates, newest_snapshot) = coalesce_host_updates(updates);
        for health in health_updates {
            self.store = health.is_online().then_some(());
            if let FleetDaemonHealth::Offline(detail) = &health {
                self.set_feedback(format!("Fleet daemon unavailable: {detail}"));
            }
            self.daemon_health = health;
        }
        if let Some(snapshot) = newest_snapshot {
            self.apply_snapshot(snapshot);
        }
    }

    fn drain_canonical_updates(&mut self) {
        let updates = self
            .canonical_updates
            .lock()
            .ok()
            .map(|mut updates| std::mem::take(&mut *updates))
            .unwrap_or_default();
        for event in updates {
            let reduction = reduce_fleet(&self.canonical, event);
            self.canonical = reduction.state;
        }
    }

    /// Fold one canonical event and return host side effect intent.
    pub fn reduce_canonical(&mut self, event: FleetEvent) -> Option<FleetIntent> {
        let reduction = reduce_fleet(&self.canonical, event);
        self.canonical = reduction.state;
        reduction.intent
    }

    /// Whether the copilot chat is the open mode, which makes the panel a
    /// repaint source: the chat polls on the frame tick, and the paint loop is
    /// dirty-gated, so an open chat with no repaint reason loads once and then
    /// never sees another message.
    pub const fn chat_open(&self) -> bool {
        self.canonical.is_chat_open()
    }

    /// Whether canonical pane currently captures modal input.
    pub fn canonical_modal_open(&self) -> bool {
        self.canonical.is_modal_open()
    }

    /// Whether live authoritative Fleet control transport is available.
    #[must_use]
    pub const fn daemon_online(&self) -> bool {
        self.daemon_health.is_online()
    }

    /// Whether the daemon has been probed and found unreachable.
    ///
    /// The gate for degraded UI and high-risk refusals. `!daemon_online()` is
    /// the wrong test: it is also true while the subscription is merely
    /// dialing, which is every screen's first frame.
    #[must_use]
    pub const fn daemon_offline(&self) -> bool {
        self.daemon_health.is_offline()
    }

    /// Whether the subscription is dialing and has reported nothing yet.
    #[must_use]
    pub const fn daemon_connecting(&self) -> bool {
        self.daemon_health.is_connecting()
    }

    /// Human-readable connection health for degraded UI and tests.
    #[must_use]
    pub fn daemon_health(&self) -> &FleetDaemonHealth {
        &self.daemon_health
    }

    /// Queue reducer update from a detached action worker.
    pub fn canonical_update_sink(&self) -> Arc<Mutex<Vec<FleetEvent>>> {
        Arc::clone(&self.canonical_updates)
    }

    /// Perform one copilot-chat effect on a detached worker.
    ///
    /// The single executor for the chat surface, called from BOTH doors: the
    /// key path (`app::events`) and the frame tick below, which is what makes
    /// a reply arrive without the operator pressing anything. The chat reducer
    /// latches an in-flight flag before emitting, so the per-frame tick cannot
    /// spawn a worker per repaint.
    pub fn dispatch_chat_intent(
        &mut self,
        intent: ainb_plugin_hangar::screen::fleet_chat::ChatIntent,
    ) {
        use ainb_plugin_hangar::screen::fleet_chat::{ChatIntent, ChatTopic};

        let sink = self.canonical_update_sink();
        // Which conversation the page at the end of this worker reads. Carried
        // on the intent rather than sniffed from the scope string: the surface
        // already knows which conversation it is, and re-deriving that here
        // would be the same fact written in two places.
        let topic = match &intent {
            ChatIntent::Refresh { topic, .. } | ChatIntent::Send { topic, .. } => topic.clone(),
            // Guardrail cards exist only on the copilot channel.
            ChatIntent::ConfirmAnswer(_) => ChatTopic::Copilot,
            // A create has no conversation to page yet: the channel it mints IS
            // the topic, and the surface opens on it when the daemon answers.
            // A list has none either; both are handled on their own workers
            // below and never reach the page.
            ChatIntent::CreateChannel { .. } | ChatIntent::ListChannels => ChatTopic::Copilot,
        };
        // The picker's read. Its own worker for the same reason a create is:
        // falling into the "write then page" path below would page the COPILOT
        // channel behind a picker the operator opened to find a broadcast one.
        if matches!(intent, ChatIntent::ListChannels) {
            let spawned =
                std::thread::Builder::new().name("ainb-fleet-channels".into()).spawn(move || {
                    let event = match crate::fleet::control::channel_list_blocking() {
                        Ok(channels) => FleetEvent::ChannelsListed(channels),
                        // The picker's failure is the create form's failure:
                        // one feedback line, and the form stays open so the
                        // operator can still mint one.
                        Err(detail) => FleetEvent::ChannelCreateFailed {
                            detail: format!("channel list failed: {detail}"),
                        },
                    };
                    if let Ok(mut updates) = sink.lock() {
                        updates.push(event);
                    }
                });
            if let Err(error) = spawned {
                if let Ok(mut updates) = self.canonical_updates.lock() {
                    updates.push(FleetEvent::ChannelCreateFailed {
                        detail: format!("channel list worker did not start: {error}"),
                    });
                }
            }
            return;
        }
        // A create is its own worker with its own event: it must NOT fall into
        // the "write then page" path below, which would page the COPILOT
        // channel behind a broadcast channel the operator just made.
        if let ChatIntent::CreateChannel { name, recipients } = intent {
            let spawned =
                std::thread::Builder::new().name("ainb-fleet-channel".into()).spawn(move || {
                    let event = match crate::fleet::control::channel_create_blocking(
                        name.clone(),
                        recipients,
                    ) {
                        // The MINTED scope and the membership the daemon
                        // actually recorded, never the ones we asked for: the
                        // daemon deduplicates the list and owns the id.
                        Ok(channel) => FleetEvent::ChannelCreated {
                            scope_key: channel.scope_key,
                            name: channel.name,
                            recipients: channel.recipients,
                        },
                        Err(detail) => FleetEvent::ChannelCreateFailed { detail },
                    };
                    if let Ok(mut updates) = sink.lock() {
                        updates.push(event);
                    }
                });
            if let Err(error) = spawned {
                if let Ok(mut updates) = self.canonical_updates.lock() {
                    updates.push(FleetEvent::ChannelCreateFailed {
                        detail: format!("channel worker did not start: {error}"),
                    });
                }
            }
            return;
        }
        let spawned = std::thread::Builder::new().name("ainb-fleet-chat".into()).spawn(move || {
            // A WRITE always ends by paging: the surface's own in-flight latch
            // is cleared by the page, and the operator sees the durable row the
            // daemon actually stored rather than an optimistic local echo.
            // The per-recipient legs of a send, kept so the surface can paint a
            // receipt per recipient instead of one aggregate tick. A fan-out
            // where one of four is refused is the case an operator has to read.
            let mut receipts = None;
            let (write_failure, scope_key) = match intent {
                ChatIntent::Refresh { scope_key, .. } => (None, scope_key),
                ChatIntent::Send {
                    scope_key,
                    targets,
                    text,
                    request_id,
                    ..
                } => {
                    let params = ainb_hangar_proto::fleet::FleetMessageSendParams {
                        scope_key: Some(scope_key.clone()),
                        // Absent, so the daemon records `operator`. A copilot
                        // write never originates at a human's keyboard.
                        actor: None,
                        targets,
                        // A composed message OPENS a thread rather than
                        // answering one: the agent's reply is the row that
                        // carries the origin, and the daemon sets that at turn
                        // end. An operator reply to a specific message is a
                        // `--origin` send, and has no key on this surface yet.
                        origin_message_id: None,
                        text,
                        request_id,
                    };
                    let failure = match crate::fleet::control::chat_send_blocking(params) {
                        Ok(result) => {
                            receipts = Some(result.deliveries);
                            None
                        }
                        Err(detail) => Some(detail),
                    };
                    (failure, Some(scope_key))
                }
                ChatIntent::ConfirmAnswer(params) => {
                    let confirm_id = params.confirm_id.clone();
                    let failure = crate::fleet::control::chat_confirm_answer_blocking(params)
                        .err()
                        .map(|detail| format!("answering {confirm_id}: {detail}"));
                    (failure, None)
                }
                // Handled above, each on its own worker.
                ChatIntent::CreateChannel { .. } | ChatIntent::ListChannels => (None, None),
            };
            let mut events = Vec::new();
            if let Some(detail) = write_failure {
                events.push(FleetEvent::ChatSendFailed { detail });
            }
            // BEFORE the page, so the receipts are on screen with the message
            // they belong to rather than one poll interval behind it.
            if let Some(deliveries) = receipts {
                events.push(FleetEvent::ChatReceipts(deliveries));
            }
            // The copilot channel needs its channel, session, card and activity
            // calls; a session thread needs one `fleet/message_list`. Branching
            // on the TOPIC rather than on the scope string is what keeps a
            // thread from paying for four round trips a second, and what keeps
            // it from creating a copilot channel it has no business creating.
            let page = match &topic {
                ChatTopic::Copilot => crate::fleet::control::chat_page_blocking(scope_key),
                // A channel pages exactly like a session thread: one
                // `fleet/message_list` on the scope the TOPIC carries. It has
                // no copilot machinery either, and its scope is minted, so
                // asking the topic is the only correct way to get it.
                ChatTopic::Session { .. } | ChatTopic::Channel { .. } => {
                    crate::fleet::control::chat_thread_page_blocking(&topic)
                }
            };
            events.push(match page {
                Ok(snapshot) => FleetEvent::ChatSnapshot(snapshot),
                Err(detail) => FleetEvent::ChatFailed { detail },
            });
            if let Ok(mut updates) = sink.lock() {
                updates.extend(events);
            }
        });
        if let Err(error) = spawned {
            if let Ok(mut updates) = self.canonical_updates.lock() {
                updates.push(FleetEvent::ChatFailed {
                    detail: format!("chat worker did not start: {error}"),
                });
            }
        }
    }

    fn seed_canonical_from_legacy_rows(&mut self) {
        if self.canonical.session_count() != 0 || self.rows.is_empty() {
            return;
        }
        let rows = self.rows.iter().map(canonical_row_from_legacy).collect();
        self.canonical.apply_snapshot(0, rows);
    }

    /// Arm the screen on navigation INTO it: open + refresh so the first frame
    /// is populated. Idempotent.
    pub fn arm(&mut self) {
        self.start_subscription();
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
        for _ in 0..n {
            let reduction = reduce_fleet(
                &self.canonical,
                FleetEvent::Key(ainb_plugin_hangar::screen::fleet::FleetKey::Up),
            );
            self.canonical = reduction.state;
        }
    }

    /// Move the row selection down by `n`, saturating at the last row.
    pub fn move_down(&mut self, n: usize) {
        if self.rows.is_empty() {
            self.selected = 0;
        } else {
            self.selected = (self.selected + n).min(self.rows.len() - 1);
        }
        self.option_cursor = 0;
        for _ in 0..n {
            let reduction = reduce_fleet(
                &self.canonical,
                FleetEvent::Key(ainb_plugin_hangar::screen::fleet::FleetKey::Down),
            );
            self.canonical = reduction.state;
        }
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
        _cwd: String,
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
        let Some(session) = self.session_meta.get(&session_id).cloned() else {
            self.set_feedback("session changed, refresh Fleet before acting".to_string());
            return false;
        };
        let action = if is_answer {
            let Some(request_fingerprint) = session.current_request_fingerprint.clone() else {
                self.set_feedback("structured request identity unavailable".to_string());
                return false;
            };
            ainb_hangar_proto::fleet::ControlAction::StructuredAnswer {
                request_fingerprint,
                request_identity: fleet_request_identity(&session),
                answers: vec![ainb_hangar_proto::fleet::FleetQuestionAnswer {
                    question_id: first_question_id(&session).unwrap_or_else(|| "0".to_string()),
                    selected_options: vec![text],
                    text: None,
                }],
            }
        } else {
            ainb_hangar_proto::fleet::ControlAction::SendPrompt { text }
        };
        dispatch_fleet_action(
            Arc::clone(&self.feedback),
            Arc::clone(&self.in_flight),
            session,
            action,
            kind_label,
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
        let Some(session) = self.session_meta.get(&session_id).cloned() else {
            self.set_feedback("session changed, refresh Fleet before acting".to_string());
            return false;
        };
        let Some(request_fingerprint) = session.current_request_fingerprint.clone() else {
            self.set_feedback("approval request identity unavailable".to_string());
            return false;
        };
        let request_identity = fleet_request_identity(&session);
        let action = match kind {
            ainb_plugin_notifyd::broker::DecisionKind::Approve => {
                ainb_hangar_proto::fleet::ControlAction::Approve {
                    request_fingerprint,
                    request_identity,
                }
            }
            ainb_plugin_notifyd::broker::DecisionKind::Deny => {
                ainb_hangar_proto::fleet::ControlAction::Deny {
                    request_fingerprint,
                    request_identity,
                }
            }
        };
        self.set_feedback(format!("{kind_label} → {session_id}: delivering…"));
        dispatch_fleet_action(
            Arc::clone(&self.feedback),
            Arc::clone(&self.in_flight),
            session,
            action,
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

/// Full snapshots supersede earlier snapshots, while connection-state changes
/// remain ordered so the final terminal health is never lost.
fn coalesce_host_updates(
    updates: Vec<FleetHostUpdate>,
) -> (Vec<FleetDaemonHealth>, Option<FleetSnapshotUpdate>) {
    let mut health_updates = Vec::new();
    let mut newest_snapshot = None;
    for update in updates {
        match update {
            FleetHostUpdate::Snapshot(snapshot) => newest_snapshot = Some(snapshot),
            FleetHostUpdate::Health(health) => health_updates.push(health),
        }
    }
    (health_updates, newest_snapshot)
}

fn fleet_request_identity(
    session: &FleetSession,
) -> Option<ainb_hangar_proto::fleet::FleetRequestIdentity> {
    let request = session.current_request.as_ref()?;
    let payload = request.get("payload").unwrap_or(request);
    let identity = payload.get("identity").unwrap_or(payload);
    let request_id = identity
        .get("requestId")
        .or_else(|| identity.get("request_id"))
        .or_else(|| identity.get("tool_use_id"))
        .or_else(|| identity.get("id"))?
        .clone();
    let string = |camel: &str, snake: &str| {
        identity
            .get(camel)
            .or_else(|| payload.get(snake))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    };
    Some(ainb_hangar_proto::fleet::FleetRequestIdentity {
        request_id,
        thread_id: string("threadId", "thread_id").unwrap_or_default(),
        turn_id: string("turnId", "turn_id").unwrap_or_default(),
        item_id: string("itemId", "item_id").unwrap_or_default(),
    })
}

fn first_question_id(session: &FleetSession) -> Option<String> {
    let request = session.current_request.as_ref()?;
    let payload = request.get("payload").unwrap_or(request);
    let input = payload.get("tool_input").or_else(|| payload.get("input")).unwrap_or(payload);
    let question = input.get("questions")?.as_array()?.first()?;
    Some(
        question
            .get("id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("0")
            .to_string(),
    )
}

fn dispatch_fleet_action(
    feedback: Arc<Mutex<ActionFeedback>>,
    in_flight: Arc<AtomicBool>,
    session: FleetSession,
    action: ainb_hangar_proto::fleet::ControlAction,
    kind_label: &'static str,
) {
    if in_flight
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    std::thread::spawn(move || {
        let result = crate::fleet::control::execute_fleet_action_blocking(
            ainb_hangar_proto::fleet::FleetActionParams {
                session_key: session.session_key.clone(),
                expected_version: session.version,
                request_id: uuid::Uuid::new_v4().to_string(),
                action,
            },
        );
        let message = match result {
            Ok(receipt) => format!(
                "{kind_label}: {:?}{}",
                receipt.status,
                receipt.detail.as_deref().map_or(String::new(), |detail| format!(": {detail}"))
            ),
            Err(error) => format!("{kind_label} failed: {error}"),
        };
        if let Ok(mut slot) = feedback.lock() {
            slot.message = message;
        }
        in_flight.store(false, Ordering::Release);
    });
}

fn state_row_from_fleet(session: &FleetSession) -> StateRow {
    use ainb_hangar_proto::fleet::{AttentionState, FleetProvenance, LifecycleState};

    let kind = match session.attention {
        AttentionState::Ask => "ASK",
        AttentionState::Approval => "APPROVE",
        AttentionState::Waiting => "WAIT",
        AttentionState::Error => "ERR",
        AttentionState::None => match session.lifecycle {
            LifecycleState::Starting => "STARTING",
            LifecycleState::Running => "RUNNING",
            LifecycleState::TurnComplete | LifecycleState::Exited => "DONE",
            LifecycleState::Idle => "IDLE",
            LifecycleState::Unknown => "UNKNOWN",
        },
    }
    .to_string();
    StateRow {
        session_id: session.session_key.clone(),
        cwd: session.cwd.clone(),
        context: request_context(session),
        parent: None,
        last_event_ts: session
            .attention_updated_at
            .max(session.lifecycle_updated_at)
            .max(session.last_observed_at),
        source: match session.provenance {
            FleetProvenance::Authoritative => "hangar-authoritative",
            FleetProvenance::Inferred => "hangar-inferred",
        }
        .to_string(),
        kind,
    }
}

fn request_context(session: &FleetSession) -> Option<String> {
    use ainb_hangar_proto::fleet::AttentionState;
    let request = session.current_request.as_ref()?;
    if session.attention != AttentionState::Ask {
        return serde_json::to_string(request).ok();
    }
    let payload = request.get("payload").unwrap_or(request);
    let input = payload.get("tool_input").or_else(|| payload.get("input")).unwrap_or(payload);
    let question = input.get("questions")?.as_array()?.first()?;
    let normalized = serde_json::json!({
        "question": question.get("question").and_then(serde_json::Value::as_str).unwrap_or(""),
        "header": question.get("header").and_then(serde_json::Value::as_str),
        "options": question.get("options").cloned().unwrap_or_else(|| serde_json::json!([])),
        "multi_select": question
            .get("multiSelect")
            .or_else(|| question.get("multi_select"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
    });
    serde_json::to_string(&normalized).ok()
}

/// Parse the ASK context JSON the materializer stored (the AskUserQuestion
/// payload). Returns a placeholder question with no options if it can't parse,
/// so the detail pane always renders something rather than blanking.
fn parse_ask(context_json: Option<&str>) -> Option<AskUserQuestionData> {
    Some(
        context_json
            .and_then(|c| serde_json::from_str::<AskUserQuestionData>(c).ok())
            .unwrap_or_else(|| AskUserQuestionData {
                question: "(question payload unavailable)".to_string(),
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
    state.refresh();

    state.seed_canonical_from_legacy_rows();
    let now_ms = chrono::Utc::now().timestamp_millis();
    let reduction = reduce_fleet(&state.canonical, FleetEvent::Tick(now_ms));
    state.canonical = reduction.state;
    // The tick's intent used to be dropped here, which was harmless while
    // nothing emitted one. The copilot chat polls on this tick, so dropping it
    // would leave a chat screen that never loads and never says why.
    if let Some(FleetIntent::Chat(intent)) = reduction.intent {
        state.dispatch_chat_intent(intent);
    }

    let mut wire = ainb_plugin_protocol::wire_buffer::WireBuffer::new(area.width, area.height);
    let content_top = 1;
    let content_bottom = area.height.saturating_sub(1);
    // The banner gets its OWN row and the roster starts below it. It used to be
    // painted over the roster's second row, which was free only because that row
    // held a duplicate count strip; crisp B2 (Q15) deleted the duplicate, so the
    // same overlay would now cover the ACTION QUEUE header.
    let banner_row = (state.daemon_offline() || state.daemon_connecting()).then_some(content_top);
    ainb_plugin_hangar::screen::fleet::render_fleet(
        &mut wire,
        area.width,
        banner_row.map_or(content_top, |row| row.saturating_add(1)),
        content_bottom,
        &state.canonical,
    );
    if let Some(row) = banner_row {
        if state.daemon_offline() {
            ainb_plugin_hangar::screen::fleet::render_degraded_banner(&mut wire, area.width, row);
        } else {
            ainb_plugin_hangar::screen::fleet::render_connecting_banner(&mut wire, area.width, row);
        }
    }
    let buf = frame.buffer_mut();
    for (coord, cell) in wire.cells {
        if coord.x >= area.width || coord.y >= area.height {
            continue;
        }
        if let Some(target) = buf.cell_mut((area.x + coord.x, area.y + coord.y)) {
            target.set_symbol(&cell.symbol);
            let mut style = Style::default().fg(wire_color(cell.fg)).bg(wire_color(cell.bg));
            if cell.modifier & 1 != 0 {
                style = style.add_modifier(Modifier::BOLD);
            }
            target.set_style(style);
        }
    }

    let total = state.canonical.session_count();
    let needs = state.canonical.attention_count();
    frame.render_widget(
        Paragraph::new(format!(
            "🛫 Fleet · {total} sessions · {needs} need attention · Hangar"
        ))
        .style(Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
        Rect::new(area.x, area.y, area.width, 1),
    );
    if total == 0 && area.height > 3 {
        let message = if state.daemon_offline() {
            "Hangar daemon not running, cached snapshot retained"
        } else if state.daemon_connecting() {
            "Connecting to the Fleet daemon…"
        } else {
            "No Fleet sessions yet, press t to start managed Codex"
        };
        frame.render_widget(
            Paragraph::new(message).style(Style::default().fg(MUTED_GRAY)),
            Rect::new(area.x + 2, area.y + 3, area.width.saturating_sub(4), 1),
        );
    }
    let attach_help = state
        .canonical
        .selected_session()
        .map(|session| match session.attachment_label() {
            "TMUX" => "→ open  a full screen",
            "REMOTE" => "remote control",
            _ => "no attachment",
        })
        .unwrap_or("no attachment");
    frame.render_widget(
        Paragraph::new(format!(
            // This bar is AT its width limit: on a 130-column pane the tail is
            // the first thing to go, and a help bar that loses the way OUT to
            // advertise one more way in is a bad trade.
            //
            // `M thread` is therefore not here; it is a per-row action and is
            // advertised per row, beside the other session actions
            // (`available_action_labels`). `N channel` IS here, because it is a
            // GLOBAL action with no row to hang off, and an unadvertised global
            // key is simply undiscoverable. That has already cost us once: the
            // chat surface shipped reachable only by a key nothing named.
            //
            // The width for it was reclaimed from `1-5 views`, not appended.
            // That entry was the one genuinely redundant thing here: the lens
            // row rendered directly beneath spells out every number WITH its
            // label, permanently (`1 needs input 0   2 idle 0   ...`), so nine
            // characters advertised what the next row already says in full.
            // `q/Esc back` also lost `/Esc`, since `q` and `Esc` do the same
            // thing and naming both spent characters on nothing, but that
            // reclaim alone was 4 against a cost of 11 and did NOT pay: the
            // first attempt shipped a bar that clipped to `q bac`.
            //
            // THE NEXT ADDITION MUST DISPLACE SOMETHING TOO, and the worked
            // example above is why. The contract is 100 COLUMNS: that is the
            // width `empty_state_renders_without_panic` renders at, and it is
            // the only guard between this bar and a silent clip. Measuring on
            // your own 180-column terminal will show you nothing.
            "↑↓ select  Enter answer  F5 refresh  {attach_help}  m chat  N channel  B broadcast  q back"
        ))
        .style(Style::default().fg(MUTED_GRAY)),
        Rect::new(
            area.x,
            area.y + area.height.saturating_sub(1),
            area.width,
            1,
        ),
    );
}

fn wire_color(color: Option<ainb_plugin_protocol::wire_buffer::Color>) -> Color {
    color.map_or(Color::Reset, |color| Color::Rgb(color.r, color.g, color.b))
}

fn canonical_row_from_legacy(row: &StateRow) -> FleetSessionRow {
    let (lifecycle_state, attention_state) = match row.kind.as_str() {
        "ASK" => ("IDLE", "ASK"),
        "APPROVE" => ("IDLE", "APPROVAL"),
        "ERR" => ("IDLE", "ERROR"),
        "WAIT" => ("IDLE", "WAITING"),
        "IDLE" => ("IDLE", "NONE"),
        "RUNNING" => ("RUNNING", "NONE"),
        "STARTING" => ("STARTING", "NONE"),
        "DONE" => ("TURN_COMPLETE", "NONE"),
        _ => ("UNKNOWN", "NONE"),
    };
    let raw_context = row
        .context
        .as_deref()
        .and_then(|context| serde_json::from_str::<serde_json::Value>(context).ok());
    let current_request = if row.kind == "ASK" {
        raw_context.map(|question| serde_json::json!({
            "questions": [{
                "id": "0",
                "question": question.get("question").cloned().unwrap_or_default(),
                "header": question.get("header").cloned().unwrap_or_default(),
                "options": question.get("options").cloned().unwrap_or_else(|| serde_json::json!([])),
                "multiSelect": question.get("multi_select").cloned().unwrap_or(false.into())
            }]
        }))
    } else {
        raw_context
    };
    FleetSessionRow {
        session_key: row.session_id.clone(),
        provider: "claude".into(),
        provider_session_id: Some(row.session_id.clone()),
        current_request_fingerprint: current_request.as_ref().map(|_| "legacy-fixture".into()),
        current_request,
        lifecycle_state: lifecycle_state.into(),
        attention_state: attention_state.into(),
        management_state: "MANAGED".into(),
        provenance: row.source.clone(),
        confidence: if row.source == "hook" { "HIGH" } else { "LOW" }.into(),
        transport_health: "HEALTHY".into(),
        capabilities: FleetCapabilities::List(
            [
                "structured_answer",
                "approvals",
                "send_prompt",
                "tmux_attach",
                "continue_turn",
                "retry",
                "interrupt",
                "stop",
                "restart",
                "kill",
                "archive",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
        ),
        version: 1,
        active_work_count: 0,
        cwd: row.cwd.clone(),
        repository_name: None,
        branch_name: None,
        tmux_target: None,
        display_name: Some(short_session(row)),
        discovered_at: row.last_event_ts,
        last_observed_at: row.last_event_ts,
        metadata_updated_at: row.last_event_ts,
        lifecycle_updated_at: row.last_event_ts,
        attention_updated_at: row.last_event_ts,
        transport_updated_at: row.last_event_ts,
    }
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
        Span::styled("· Hangar", Style::default().fg(MUTED_GRAY)),
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
            "(no Fleet sessions yet, press n to create an ATC session)"
        } else if state.db_path.exists() {
            "(Hangar socket unavailable, cached snapshot retained)"
        } else {
            "(Hangar daemon not running)"
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
            let src = if r.source == "hangar-authoritative" {
                ""
            } else {
                " ~inferred"
            };
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
    use ainb_plugin_hangar::screen::fleet::FleetFilter;
    use ratatui::backend::TestBackend;

    fn state_with(rows: Vec<StateRow>) -> FleetPanelState {
        let mut s = FleetPanelState::default();
        // Force the daemon disconnected and use a non-existent socket so the
        // render tick preserves seeded rows in its deterministic cache.
        s.store = None;
        s.db_path = std::path::PathBuf::from("/nonexistent/hangar.sock");
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

    /// One lens as the row paints it: `<key> <label> <count>`.
    ///
    /// Composed from the renderer's own accessors rather than spelled out here.
    /// The pane is drawn by `ainb-plugin-hangar`, whose lens words come from its
    /// `vocab.rs`; when that vocabulary went lowercase these assertions were the
    /// only thing still saying `Needs input`, and they broke in a crate the
    /// change never touched. Deriving them means the next wording change cannot
    /// desynchronise the two crates.
    fn lens_text(filter: FleetFilter, count: usize) -> String {
        format!("{} {} {count}", filter.key(), filter.label())
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
    fn renders_attention_first_operator_view() {
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
        assert!(
            out.contains("2 need attention"),
            "needs counter missing: {out}"
        );
        for (filter, count) in [
            (FleetFilter::NeedsInput, 2),
            (FleetFilter::Idle, 1),
            (FleetFilter::Running, 1),
            (FleetFilter::All, 4),
        ] {
            let lens = lens_text(filter, count);
            assert!(out.contains(&lens), "{lens} lens missing: {out}");
        }
        // Default lens shows only actionable rows.
        assert!(out.contains("ACTION QUEUE"), "action queue missing: {out}");
        assert!(out.contains("INPUT"), "operator state missing: {out}");
        assert!(out.contains("deploy"), "ASK session label missing");
        assert!(out.contains("api"), "ERR session label missing");
        assert!(
            !out.contains("sess-idle"),
            "idle row leaked into default lens: {out}"
        );
        assert!(
            !out.contains("sess-run"),
            "running row leaked into default lens: {out}"
        );
        // Browse detail shows one question and action, not raw option payload.
        assert!(
            out.contains("Ship to which env?"),
            "ASK question missing: {out}"
        );
        assert!(out.contains("Enter Answer"), "answer action missing: {out}");
        assert!(!out.contains("staging"), "raw option payload leaked: {out}");
        assert!(!out.contains("prod"), "raw option payload leaked: {out}");
    }

    #[test]
    fn renders_approval_without_raw_payload_or_non_actionable_rows() {
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
        assert!(
            out.contains("1 need attention"),
            "needs counter wrong: {out}"
        );
        assert!(
            out.contains("Approval required for Bash."),
            "APPROVE detail missing: {out}"
        );
        assert!(out.contains("y Approve"), "approve action missing: {out}");
        assert!(
            !out.contains("rm -rf"),
            "raw approval payload leaked into operator view: {out}"
        );
        assert!(
            !out.contains("boot"),
            "starting row leaked into Needs input lens: {out}"
        );
    }

    #[test]
    fn detail_summarizes_error_and_waiting_context() {
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
        assert!(
            out.contains("Error: overloaded"),
            "error summary missing: {out}"
        );

        state.move_down(1);
        let out = render_to_string(&mut state, 130, 20);
        assert!(out.contains("allow Bash?"), "WAIT summary missing: {out}");
        assert!(
            !out.contains("permission_prompt"),
            "raw WAIT payload leaked: {out}"
        );
    }

    #[test]
    fn empty_state_renders_without_panic() {
        let mut state = FleetPanelState::default();
        state.store = None;
        state.db_path = std::path::PathBuf::from("/nonexistent/hangar.sock");
        let out = render_to_string(&mut state, 100, 16);
        assert!(out.contains("Fleet"), "title missing in empty state: {out}");
        assert!(
            out.contains("Hangar daemon not running") || out.contains("no fleet state"),
            "empty-state hint missing: {out}"
        );
        // The lens numbers moved OUT of the help bar, so this guards the row
        // that actually renders them. Deleting it instead would have left
        // the lens row with no guard at all.
        let lens = lens_text(FleetFilter::NeedsInput, 0);
        assert!(out.contains(&lens), "lens row missing: {out}");
        assert!(out.contains("q back"), "back help missing: {out}");
        assert!(out.contains("N channel"), "channel help missing: {out}");
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

    #[test]
    fn stream_updates_apply_in_order_and_retain_latest_revision_offline() {
        use ainb_hangar_proto::fleet::FleetSnapshot;

        let mut state = FleetPanelState::default();
        state.stream_updates.lock().expect("stream update queue").extend([
            FleetHostUpdate::Snapshot(FleetSnapshotUpdate {
                snapshot: FleetSnapshot {
                    head_revision: 7,
                    sessions: Vec::new(),
                },
                git_contexts: HashMap::new(),
            }),
            FleetHostUpdate::Health(FleetDaemonHealth::Online),
            FleetHostUpdate::Snapshot(FleetSnapshotUpdate {
                snapshot: FleetSnapshot {
                    head_revision: 9,
                    sessions: Vec::new(),
                },
                git_contexts: HashMap::new(),
            }),
            FleetHostUpdate::Health(FleetDaemonHealth::Offline("socket closed".into())),
        ]);

        state.refresh();

        assert_eq!(state.canonical.head_revision(), 9);
        assert_eq!(
            state.daemon_health(),
            &FleetDaemonHealth::Offline("socket closed".into())
        );
        assert!(!state.daemon_online());
        assert!(state.feedback_line().contains("socket closed"));
    }

    #[test]
    fn stream_update_coalescing_keeps_terminal_health_and_newest_snapshot() {
        use ainb_hangar_proto::fleet::FleetSnapshot;

        let (health, snapshot) = coalesce_host_updates(vec![
            FleetHostUpdate::Health(FleetDaemonHealth::Connecting),
            FleetHostUpdate::Snapshot(FleetSnapshotUpdate {
                snapshot: FleetSnapshot {
                    head_revision: 7,
                    sessions: Vec::new(),
                },
                git_contexts: HashMap::new(),
            }),
            FleetHostUpdate::Health(FleetDaemonHealth::Online),
            FleetHostUpdate::Snapshot(FleetSnapshotUpdate {
                snapshot: FleetSnapshot {
                    head_revision: 9,
                    sessions: Vec::new(),
                },
                git_contexts: HashMap::new(),
            }),
            FleetHostUpdate::Health(FleetDaemonHealth::Offline("socket closed".into())),
        ]);

        assert_eq!(
            health,
            vec![
                FleetDaemonHealth::Connecting,
                FleetDaemonHealth::Online,
                FleetDaemonHealth::Offline("socket closed".into()),
            ],
            "connection state order must survive snapshot coalescing"
        );
        assert_eq!(
            snapshot.expect("newest snapshot retained").snapshot.head_revision,
            9,
            "replayed complete snapshots must collapse to the newest revision"
        );
    }

    /// The pre-probe frame must not claim the daemon is down. `Connecting` and
    /// `Offline` used to render identically because every consumer tested
    /// `!is_online()`, so opening Fleet always flashed "daemon offline, cached
    /// snapshot, high-risk actions disabled" before anything had been probed.
    #[test]
    fn connecting_renders_distinctly_from_offline_and_does_not_gate_actions() {
        let mut state = state_with(vec![row(
            "cached-session",
            "/work/cached",
            "IDLE",
            None,
            "hook",
        )]);
        state
            .stream_updates
            .lock()
            .expect("stream update queue")
            .push(FleetHostUpdate::Health(FleetDaemonHealth::Connecting));
        state.refresh();

        assert!(state.daemon_connecting());
        assert!(!state.daemon_offline(), "unprobed must not read as offline");
        assert!(!state.daemon_online(), "unprobed must not read as online");

        let connecting = render_to_string(&mut state, 130, 20);
        assert!(
            connecting.contains("Connecting to the Fleet daemon"),
            "connecting banner missing: {connecting}"
        );
        assert!(
            !connecting.contains("Fleet daemon offline"),
            "connecting must not claim offline: {connecting}"
        );
        assert!(
            !connecting.contains("high-risk actions disabled"),
            "connecting must not disable high-risk actions: {connecting}"
        );

        state
            .stream_updates
            .lock()
            .expect("stream update queue")
            .push(FleetHostUpdate::Health(FleetDaemonHealth::Offline(
                "socket closed".into(),
            )));
        state.refresh();

        let offline = render_to_string(&mut state, 130, 20);
        assert!(state.daemon_offline());
        assert!(
            offline.contains("Fleet daemon offline"),
            "offline banner missing: {offline}"
        );
        assert_ne!(
            connecting, offline,
            "connecting and offline must not render identically"
        );
    }

    #[test]
    fn offline_render_keeps_cached_session_and_shows_degraded_banner() {
        let mut state = state_with(vec![row(
            "cached-session",
            "/work/cached",
            "IDLE",
            None,
            "hook",
        )]);

        let out = render_to_string(&mut state, 130, 20);

        assert!(out.contains("cached"), "cached row disappeared: {out}");
        assert!(
            out.contains("Fleet daemon offline"),
            "banner missing: {out}"
        );
        assert!(
            out.contains("high-risk actions disabled"),
            "offline gate missing: {out}"
        );
    }
}
