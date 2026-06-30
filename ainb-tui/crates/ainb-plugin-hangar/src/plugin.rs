//! ABI v2 [`Plugin`] implementation for the Hangar control plane TUI.
//!
//! P3.7 wires the daemon connection into the plugin. On `plugin/init` the
//! plugin dials the daemon socket through the host `unix_socket_dial` cap,
//! sends a `workspace/subscribe` request framed over that cap stream, and
//! moves a [`Connection`] state machine `Disconnected → Dialing →
//! Handshake → Connected`. Inbound daemon frames arrive as
//! `socket:<stream_id>` `plugin/handle_event` notifications; the plugin
//! reassembles them with a [`FrameDecoder`] and acks the subscribe to
//! reach [`ConnState::Connected`].
//!
//! `render` paints the shared P4.1 chrome: the [top tab bar](crate::chrome)
//! on row 0 (four primary tabs + workspace slug + an online/offline presence
//! dot derived from the daemon link) and the contextual key-hint footer on the
//! last row. The screen body (rows `1..h-1`) is filled by the per-screen
//! renderers landing in P4.3..P4.7.
//!
//! Everything declared in `manifest.toml` — the four P3 host caps — is
//! requested here; this phase exercises `unix_socket_dial` +
//! `unix_socket_send`. `spawn_managed_subprocess` (auto-starting the
//! daemon) and `secret_store_get` land in later phases.

use ainb_hangar_proto::events::HangarEvent;
use ainb_hangar_proto::{methods as daemon_methods, RpcId, RpcResponse};
use ainb_plugin_sdk::{
    CliOutput, HandleEventParams, HandleKeyParams, HostClient, InitContext, KeyCode, Plugin,
    RenderParams, Result, RpcError, UnixSocketEvent, UnixSocketEventKind, WireBuffer,
};
use async_trait::async_trait;

use crate::chrome::{render_footer, render_top_bar, Presence};
use crate::connection::{ConnState, Connection, DEFAULT_WORKSPACE_ID};
use crate::firstrun::{self, reduce_first_run, FirstRunIntent, FirstRunModal};
use crate::jsonrpc_over_socket::{encode_request, FrameDecoder};
use crate::screen::{
    render_body, route_key, AppEvent, AppState, NavIntent, Screen, ScreenStates, WorkspaceAction,
};
use ainb_hangar_core::ids::WorkspaceId;
use ainb_hangar_proto::settings::WorkspaceRow;

/// Static manifest TOML loaded at compile time. The [`Server`] uses
/// this on `plugin/init` to echo `name`/`version` back to the host so
/// spawn-vs-manifest can be cross-checked.
///
/// [`Server`]: ainb_plugin_sdk::Server
pub const MANIFEST_TOML: &str = include_str!("../manifest.toml");

/// Fallback render viewport used only if a degenerate `0×0` ever
/// arrives in [`RenderParams`]. The host normally sends an explicit
/// viewport.
const FALLBACK_VIEWPORT: (u16, u16) = (1, 1);

/// The daemon socket path the plugin dials. The host `unix_socket_dial`
/// cap expands `~` and canonicalizes before checking the manifest
/// whitelist (which lists this exact path).
const DAEMON_SOCKET_PATH: &str = "~/.agents-in-a-box/hangar.sock";

/// JSON-RPC id of the `auth/hello` frame the plugin sends FIRST on every
/// connection (e38.1): the daemon rejects any other first frame. The token is
/// read from `{hangar_home}/hangar/daemon.token` (written by the daemon at
/// boot, `0600`).
const AUTH_REQ_ID: i64 = 0;

/// JSON-RPC id the plugin assigns to its `workspace/subscribe` request.
/// A single id is enough: the plugin issues exactly one subscribe per
/// connection, and matching the ack by id avoids treating an unrelated
/// reply as the handshake completion.
const SUBSCRIBE_REQ_ID: i64 = 1;

/// JSON-RPC id for the `hangar/issues_list` snapshot request.
const ISSUES_REQ_ID: i64 = 10;
/// JSON-RPC id for the `hangar/agents_list` snapshot request.
const AGENTS_REQ_ID: i64 = 11;
/// JSON-RPC id for the `hangar/skills_list` snapshot request.
const SKILLS_REQ_ID: i64 = 12;
/// JSON-RPC id for the `hangar/health` snapshot request.
const HEALTH_REQ_ID: i64 = 13;
/// JSON-RPC id for a `hangar/skill_get` detail request (P6.5).
const SKILL_GET_REQ_ID: i64 = 14;
/// JSON-RPC id for a `hangar/skills_sync` request (P6.5).
const SKILLS_SYNC_REQ_ID: i64 = 15;
/// JSON-RPC id for a `hangar/skill_attach` request (P6.5).
const SKILL_ATTACH_REQ_ID: i64 = 16;
/// JSON-RPC id for a `hangar/skill_detach` request (P6.5).
const SKILL_DETACH_REQ_ID: i64 = 17;
/// JSON-RPC id for the `hangar/autopilots_list` snapshot request (P7.5).
const AUTOPILOTS_REQ_ID: i64 = 18;
/// JSON-RPC id for a `hangar/autopilot_runs` request (P7.5).
const AUTOPILOT_RUNS_REQ_ID: i64 = 19;
/// JSON-RPC id for a `hangar/autopilot_fire_now` request (P7.5).
const AUTOPILOT_FIRE_REQ_ID: i64 = 20;
/// JSON-RPC id for a `hangar/autopilot_set_enabled` request (P7.5).
const AUTOPILOT_TOGGLE_REQ_ID: i64 = 21;
/// JSON-RPC id for the `hangar/tasks_list` snapshot request (P8.4).
const TASKS_REQ_ID: i64 = 22;
/// JSON-RPC id for a `hangar/task_transition` request (P8.4).
const TASK_TRANSITION_REQ_ID: i64 = 23;
/// JSON-RPC id for the `hangar/daemon_health` snapshot request (P8.5).
const DAEMON_HEALTH_REQ_ID: i64 = 24;
/// JSON-RPC id for a `hangar/issue_update` request raised by the agent picker
/// (e38.8).
const ISSUE_UPDATE_REQ_ID: i64 = 25;
/// JSON-RPC id for a `hangar/comment_add` request raised by the task-detail
/// compose modal (e38.5).
const COMMENT_ADD_REQ_ID: i64 = 26;
/// JSON-RPC id for a `hangar/issue_create` request raised by the issue-list
/// inline create flow (e38.29).
const ISSUE_CREATE_REQ_ID: i64 = 27;
/// JSON-RPC id for the `hangar/members_list` snapshot request feeding the
/// settings Members pane (e38.11).
const MEMBERS_REQ_ID: i64 = 28;
/// JSON-RPC id for the `hangar/inbox_list` snapshot request feeding the Inbox
/// screen (e38.14).
const INBOX_LIST_REQ_ID: i64 = 29;
/// JSON-RPC id for a `hangar/inbox_mark_read` request raised by the Inbox `r`
/// key (e38.14).
const INBOX_MARK_READ_REQ_ID: i64 = 30;
/// JSON-RPC id for a `hangar/search` request raised by the command palette
/// (e38.13).
const SEARCH_REQ_ID: i64 = 31;
/// JSON-RPC id for the `hangar/usage_rollup` snapshot request feeding the usage
/// dashboard (e38.35).
const USAGE_ROLLUP_REQ_ID: i64 = 32;
/// JSON-RPC id for the `hangar/pr_status_refresh` request raised when a
/// task-detail screen with a bound PR opens (e38.34).
const PR_STATUS_REFRESH_REQ_ID: i64 = 33;
/// The actor-ref the plugin authors comments as (e38.5).
///
/// The plugin has no per-user auth/identity layer yet (a later concern), so a
/// comment the local user composes is attributed to this canonical member ref.
/// The daemon only requires a well-formed `member:<id>` / `agent:<id>` token (the
/// `comment.author_type` CHECK), so this is accepted as-is; swapping in the real
/// signed-in member is a drop-in change once identity lands.
const SELF_AUTHOR_REF: &str = "member:me";
/// How many trailing log lines the logs pane reads from the newest `daemon.*`
/// file on each refresh (P8.6). Bounded so a huge log file never blows up the
/// pane; the daily rotation keeps a single day's file the practical ceiling.
const LOGS_TAIL_LINES: usize = 500;

/// Hangar plugin state.
///
/// Holds the daemon [`Connection`] state machine and the inbound socket
/// [`FrameDecoder`]. The SDK serialises handler access behind a mutex, so
/// `&mut self` mutation here is race-free.
#[derive(Debug)]
pub struct HangarPlugin {
    conn: Connection,
    decoder: FrameDecoder,
    app: Option<AppState>,
    /// Per-screen render-state caches filled from the daemon snapshot RPCs.
    screens: ScreenStates,
    /// Set when a subscribe ack just arrived, so `handle_event` knows to fire
    /// the snapshot fetches (it has the `host` the sync decode path lacks).
    fetch_pending: bool,
    /// The first-run danger-full-access modal (P5.6). `Showing` over the landing
    /// screen on a fresh machine until the user accepts (`y`), then `Dismissed`.
    /// Initialised from the recorded `warnings_ack` on `plugin/init`.
    first_run: FirstRunModal,
    /// Set when the user accepted the first-run modal (`y`); the `first_run` ack
    /// is persisted to `state.toml` in `render` (where host IO is safe, unlike
    /// the inline `handle_key`). One write per accept.
    first_run_ack_pending: bool,
    /// The slug of the skill a `hangar/skill_get` is in flight for (P6.5), so the
    /// detail reply can be folded onto the right skill. `None` when no detail
    /// request is pending.
    pending_detail_slug: Option<String>,
    /// The id of the autopilot a `hangar/autopilot_runs` is in flight for (P7.5),
    /// so the reply folds onto the right row. `None` when none pending.
    pending_runs_autopilot: Option<String>,
    /// The host-shell opener used by the `o` (open-PR) action (P9.2). Defaults to
    /// the real [`SystemOpener`] (or, when `$HANGAR_OPENER_PROBE_FILE` is set, a
    /// [`RecordingOpener`] for the tmux tripwire — see [`crate::shell`]). Held as
    /// a trait object so tests can inject a recording opener.
    opener: Box<dyn crate::shell::Opener>,
    /// The host-shell daemon starter used by the offline `[s]` action (e38.36).
    /// Defaults to the real [`SystemDaemonStarter`](crate::shell::SystemDaemonStarter)
    /// (or, when `$HANGAR_DAEMON_START_PROBE_FILE` is set, a
    /// [`RecordingDaemonStarter`](crate::shell::RecordingDaemonStarter)). Held as
    /// a trait object so tests can inject a recording/failing starter.
    daemon_starter: Box<dyn crate::shell::DaemonStarter>,
    /// Set when the user pressed `[s]` while the daemon link was offline (e38.36).
    /// The host-shell start + re-dial can't run inline in `handle_key` (it would
    /// deadlock the reader loop), so it is deferred and drained in `render`.
    start_daemon_pending: bool,
    /// The message from the last failed `[s]` start attempt (e38.36), surfaced in
    /// the offline empty-state so a start failure is visible rather than silent.
    /// `None` once a start succeeds or while none has been attempted.
    daemon_start_error: Option<String>,
    /// The issue id of a task-detail screen with a bound PR that just opened
    /// (e38.34), so `render` can fire `hangar/pr_status_refresh` for it (the
    /// socket send can't run inline in the `apply_nav` key path). `None` when no
    /// refresh is armed; consumed (taken) once fired.
    pending_pr_status_refresh: Option<String>,
    /// The card-board mouse drag FSM (63l.2). `handle_mouse` folds each forwarded
    /// pointer event against the [`hit_map`](Self::hit_map) into this, producing a
    /// [`MouseIntent`](crate::mouse::MouseIntent).
    mouse_fsm: crate::mouse::MouseFsm,
    /// The render-time hit-map (63l.2) the last `render` recorded for the active
    /// board screen — the geometry `handle_mouse` hit-tests against. Rebuilt each
    /// render; empty until the first paint of a board screen.
    hit_map: crate::mouse::HitMap,
    /// Mouse intents `handle_mouse` produced, drained on the next `render`
    /// (63l.2). `handle_mouse` runs INLINE on the reader loop, so it only stashes
    /// intents here; the spawned `render` applies the local board effects (select,
    /// open, move, reorder, scroll, hover) and binds the cross-column move to a
    /// daemon RPC (63l.4). A non-empty queue is itself the [`Plugin::wants_redraw`]
    /// signal — no separate redraw bool.
    pending_mouse_intents: Vec<crate::mouse::MouseIntent>,
    /// A cross-column drag-drop (63l.4) that `drain_mouse_intents` resolved into a
    /// real lifecycle move: the `(issue_id, to_status)` to fire as
    /// `hangar/issue_update{state}` over the daemon socket. The board already moved
    /// the card optimistically; this arms the RPC that makes the move durable.
    /// Drained in `render` (the spawned task where host IO is safe), exactly like
    /// the assign / comment / create deferred RPCs. `None` when no move is armed.
    pending_issue_state_update: Option<(String, ainb_hangar_proto::lifecycle::IssueLifecycle)>,
    /// The right-click context-menu overlay (63l.5), present only while open.
    /// Raised by the `OpenContextMenu` mouse intent over a card; its keyboard /
    /// click navigation produces a [`ContextMenuIntent`](crate::screen::context_menu::ContextMenuIntent)
    /// the `render` drain binds to a real `hangar/issue_update` RPC. `None` when
    /// the menu is closed.
    context_menu: Option<crate::screen::context_menu::ContextMenuState>,
    /// A priority edit (63l.5) raised by the context menu's `Priority ▸` submenu:
    /// the `(issue_id, priority)` to fire as `hangar/issue_update{priority}` over
    /// the daemon socket. Drained in `render` like the state-move RPC. `None` when
    /// no priority edit is armed.
    pending_issue_priority_update: Option<(String, i64)>,
    /// An assignee edit (63l.5) raised by the context menu's `Assign ▸` submenu:
    /// the `(issue_id, actor_ref)` to fire as `hangar/issue_update{assignee}` over
    /// the daemon socket. Drained in `render` like the assign-picker path. `None`
    /// when no assignee edit is armed.
    pending_issue_assignee_update: Option<(String, String)>,
    /// The render-time card-board layout (63l.6) the last `render` recorded for the
    /// active NON-issue list board screen (Kanban / Autopilots / Skills) — the
    /// geometry `handle_mouse` folds via
    /// [`fold_board_mouse`](crate::board_mouse::fold_board_mouse) against. Rebuilt
    /// each render; `None` off those screens (a click there resolves to nothing).
    board_layout: Option<crate::widgets::card_board::BoardLayout>,
    /// List-screen mouse intents `handle_mouse` produced (63l.6), drained on the
    /// next `render`. Like the issue board's queue, this is the inline,
    /// non-blocking stash: the spawned `render` binds each to the active screen's
    /// EXISTING action (open/scroll/hover/context-menu). A non-empty queue is part
    /// of the [`Plugin::wants_redraw`] signal.
    pending_board_mouse_intents: Vec<crate::board_mouse::BoardMouseIntent>,
    /// The generic list-screen right-click context-menu overlay (63l.6), present
    /// only while open. Raised by an `OpenContextMenu` board-mouse intent over a
    /// Kanban / Autopilots / Skills card; its leaf binds to the screen's EXISTING
    /// daemon RPC seam. `None` when closed.
    list_context_menu: Option<crate::screen::list_context_menu::ListContextMenuState>,
}

/// Read the daemon socket-auth token from `{hangar_home}/hangar/daemon.token`.
///
/// The home resolves exactly like [`crate::firstrun::state_path`]
/// (`$AINB_HANGAR_HOME` when set and non-empty, else `$HOME/.agents-in-a-box`) via the
/// shared [`ainb_hangar_proto::auth::default_token_file`] helper, so the file
/// read here is the one the daemon wrote at boot. `None` when the file is
/// missing or empty (the daemon then rejects the connection with a clear
/// UNAUTHORIZED error instead of a hang).
fn read_daemon_token() -> Option<String> {
    let path = ainb_hangar_proto::auth::default_token_file()?;
    let raw = std::fs::read_to_string(path).ok()?;
    let token = raw.trim().to_string();
    (!token.is_empty()).then_some(token)
}

/// The current wall-clock time in epoch milliseconds, for the Kanban card-age
/// derivation when (re)building the hit-map (63l.6). Mirrors the render clock in
/// [`crate::screen::app_screens`]; a clock skew before the epoch saturates to `0`.
fn now_ms_clock() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

impl Default for HangarPlugin {
    fn default() -> Self {
        Self {
            conn: Connection::default(),
            decoder: FrameDecoder::default(),
            app: None,
            screens: ScreenStates::default(),
            fetch_pending: false,
            first_run: FirstRunModal::default(),
            first_run_ack_pending: false,
            pending_detail_slug: None,
            pending_runs_autopilot: None,
            opener: crate::shell::default_opener(),
            daemon_starter: crate::shell::default_daemon_starter(),
            start_daemon_pending: false,
            daemon_start_error: None,
            pending_pr_status_refresh: None,
            mouse_fsm: crate::mouse::MouseFsm::default(),
            hit_map: crate::mouse::HitMap::default(),
            pending_mouse_intents: Vec::new(),
            pending_issue_state_update: None,
            context_menu: None,
            pending_issue_priority_update: None,
            pending_issue_assignee_update: None,
            board_layout: None,
            pending_board_mouse_intents: Vec::new(),
            list_context_menu: None,
        }
    }
}

impl HangarPlugin {
    /// Construct a fresh, disconnected plugin.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct a plugin with a custom [`Opener`](crate::shell::Opener) for the
    /// `o` (open-PR) action. Used by tests to inject a
    /// [`RecordingOpener`](crate::shell::RecordingOpener) so the open is
    /// observable without launching a real browser.
    #[must_use]
    pub fn with_opener(opener: Box<dyn crate::shell::Opener>) -> Self {
        Self {
            opener,
            ..Self::default()
        }
    }

    /// Construct a plugin with a custom
    /// [`DaemonStarter`](crate::shell::DaemonStarter) for the offline `[s]`
    /// action (e38.36). Used by tests to inject a
    /// [`RecordingDaemonStarter`](crate::shell::RecordingDaemonStarter) /
    /// [`FailingDaemonStarter`](crate::shell::FailingDaemonStarter) so the start
    /// is observable without launching a real daemon.
    #[must_use]
    pub fn with_daemon_starter(daemon_starter: Box<dyn crate::shell::DaemonStarter>) -> Self {
        Self {
            daemon_starter,
            ..Self::default()
        }
    }

    /// The routing state, lazily initialised on the `default` workspace.
    ///
    /// P4.1 lands routing state on the plugin so the shared chrome (top tab
    /// bar + footer) can render the active screen. Later P4 sub-beads replace
    /// the fixed `default` workspace with the one the daemon hands back on
    /// `workspace/subscribe`.
    fn app_state(&mut self) -> &AppState {
        self.app.get_or_insert_with(|| {
            let ws = WorkspaceId::from_str(DEFAULT_WORKSPACE_ID)
                .expect("DEFAULT_WORKSPACE_ID is a non-empty literal");
            AppState::new(ws)
        })
    }

    /// Presence dot state derived from the daemon link.
    const fn presence(state: &ConnState) -> Presence {
        match state {
            ConnState::Connected => Presence::Online,
            ConnState::Disconnected
            | ConnState::Dialing
            | ConnState::Handshake
            | ConnState::Error(_) => Presence::Offline,
        }
    }

    /// Whether the offline empty-state + `[s]` start action apply (e38.36).
    ///
    /// True only for the genuinely-down link states (`Disconnected` / `Error`),
    /// NOT the transient `Dialing` / `Handshake` mid-connect states: showing the
    /// "daemon offline, press [s]" panel while a dial is in flight would flicker
    /// the guidance over a link that is about to come up. The presence DOT still
    /// reads offline during those transients (via [`Self::presence`]); only the
    /// full-screen guidance panel + the `[s]` binding are gated on this stricter
    /// "really down" predicate.
    const fn is_offline(state: &ConnState) -> bool {
        matches!(state, ConnState::Disconnected | ConnState::Error(_))
    }

    /// Shell `ainb hangar daemon start` via the
    /// [`DaemonStarter`](crate::shell::DaemonStarter) seam (e38.36).
    ///
    /// Returns `true` when the start succeeded (the caller should then re-dial),
    /// `false` on a spawn failure — in which case the error is recorded in
    /// `daemon_start_error` (surfaced in the empty-state) so it is visible rather
    /// than silent. Host-free so the start dispatch is unit-testable; the re-dial
    /// (which needs the [`HostClient`]) is the caller's concern.
    fn try_start_daemon(&mut self) -> bool {
        match self.daemon_starter.start() {
            Ok(()) => {
                self.daemon_start_error = None;
                true
            }
            Err(e) => {
                self.daemon_start_error = Some(format!("start failed: {e}"));
                false
            }
        }
    }

    /// Run the deferred offline `[s]` start: shell `ainb hangar daemon start`,
    /// then re-dial so the link flips online once the socket appears (e38.36).
    ///
    /// On a spawn failure the re-dial is skipped (the error is already recorded by
    /// [`Self::try_start_daemon`]) — best-effort, never a panic. On success the
    /// prior error is cleared and `connect` re-runs the dial/auth/subscribe
    /// handshake.
    async fn start_daemon_and_redial(&mut self, host: &HostClient) {
        if self.try_start_daemon() {
            let _ = host
                .log_info("hangar: [s] started daemon, re-dialing")
                .await;
            self.connect(host).await;
        } else if let Some(msg) = &self.daemon_start_error {
            let _ = host.log_info(format!("hangar: {msg}")).await;
        }
    }

    /// Dial the daemon and send the workspace subscribe. Records the
    /// resulting [`ConnState`] on `self`; transport failures land the
    /// machine in [`ConnState::Error`] (rendered Red) rather than
    /// propagating, so a downed daemon shows a clean footer instead of
    /// crashing the plugin.
    async fn connect(&mut self, host: &HostClient) {
        self.decoder = FrameDecoder::new();
        self.conn.dialing();

        let dial = match host.unix_socket_dial(DAEMON_SOCKET_PATH).await {
            Ok(r) => r,
            Err(e) => {
                self.conn.on_error(format!("dial failed: {e}"));
                let _ = host.log_info(format!("hangar: dial failed: {e}")).await;
                return;
            }
        };
        self.conn.on_dialed(dial.stream_id.clone());

        // First frame (e38.1): authenticate with the daemon token read from
        // `{hangar_home}/hangar/daemon.token`. A missing/unreadable file sends
        // an empty token — the daemon answers UNAUTHORIZED, which surfaces as
        // a clean connection error rather than a silent hang.
        let token = read_daemon_token().unwrap_or_default();
        let auth_body = match encode_request(
            AUTH_REQ_ID,
            daemon_methods::AUTH_HELLO,
            serde_json::json!({ "token": token }),
        ) {
            Ok(b) => b,
            Err(e) => {
                self.conn.on_error(format!("encode auth failed: {e}"));
                return;
            }
        };
        if let Err(e) = host
            .unix_socket_send(dial.stream_id.clone(), auth_body)
            .await
        {
            self.conn.on_error(format!("send auth failed: {e}"));
            return;
        }

        // Frame the workspace/subscribe request and write it to the cap stream.
        let body = match encode_request(
            SUBSCRIBE_REQ_ID,
            daemon_methods::WORKSPACE_SUBSCRIBE,
            serde_json::json!({ "workspace_id": DEFAULT_WORKSPACE_ID }),
        ) {
            Ok(b) => b,
            Err(e) => {
                self.conn.on_error(format!("encode subscribe failed: {e}"));
                return;
            }
        };
        if let Err(e) = host.unix_socket_send(dial.stream_id, body).await {
            self.conn.on_error(format!("send subscribe failed: {e}"));
            return;
        }
        let _ = host
            .log_info("hangar: dialed daemon, auth + subscribe sent")
            .await;
    }

    /// Feed an inbound `socket:<stream_id>` event into the connection.
    ///
    /// Decodes the [`UnixSocketEvent`] envelope: `Data` bytes are framed by the
    /// [`FrameDecoder`], then each whole frame is classified by shape — a
    /// JSON-RPC *response* (carries an `id`) drives [`Self::on_daemon_response`]
    /// (subscribe ack + snapshot results), while a pushed `hangar/event`
    /// *notification* (carries a `method`, no `id`) folds into the screen caches
    /// via [`Self::on_daemon_event`]. `Eof` drops to `Disconnected`; `Error`
    /// lands in `Error`.
    ///
    /// e38.29: the daemon multiplexes responses AND `hangar/event` notifications
    /// over one connection. Decoding every frame as an `RpcResponse` tore the
    /// link down on the first event push (`missing field id`), so a mutation that
    /// emitted an event (`IssueCreated`, `IssueUpdated`, …) silently knocked the
    /// plugin offline. Splitting at the body level keeps the link healthy and
    /// lets pushed events re-render without a full re-pull.
    fn on_socket_event(&mut self, event: &UnixSocketEvent) {
        match event.kind {
            UnixSocketEventKind::Data => {
                let Some(bytes) = event.bytes.as_ref() else {
                    return;
                };
                match self.decoder.push_frames(bytes) {
                    Ok(frames) => {
                        for body in frames {
                            self.route_daemon_frame(&body);
                        }
                    }
                    Err(e) => self.conn.on_error(format!("frame decode: {e}")),
                }
            }
            UnixSocketEventKind::Eof => self.conn.on_eof(),
            UnixSocketEventKind::Error => {
                let msg = event.error.clone().unwrap_or_else(|| "socket error".into());
                self.conn.on_error(msg);
            }
        }
    }

    /// Classify one whole daemon frame body and route it: a response (`id`
    /// present) to [`Self::on_daemon_response`]; a `hangar/event` notification
    /// (`method` present) to [`Self::on_daemon_event`]; anything else is ignored
    /// (a forward-compatible notification the plugin doesn't model — never an
    /// error, so an unknown push can't knock the link offline). A body that
    /// parses as neither is a genuine protocol fault and errors the link.
    fn route_daemon_frame(&mut self, body: &[u8]) {
        let value: serde_json::Value = match serde_json::from_slice(body) {
            Ok(v) => v,
            Err(e) => {
                self.conn.on_error(format!("frame decode: {e}"));
                return;
            }
        };
        if value.get("id").is_some() {
            match serde_json::from_value::<RpcResponse>(value) {
                Ok(resp) => self.on_daemon_response(&resp),
                Err(e) => self.conn.on_error(format!("response decode: {e}")),
            }
        } else if value.get("method").is_some() {
            self.on_daemon_event(&value);
        }
        // Neither id nor method: ignore (defensive — never error the link).
    }

    /// Fold one pushed `hangar/event` notification into the screen caches
    /// (e38.29). A non-`hangar/event` method is ignored; an undecodable event
    /// payload is logged-by-silence (skipped) rather than erroring the link — the
    /// next snapshot re-pull reconciles. Keeps the link `Connected`.
    fn on_daemon_event(&mut self, value: &serde_json::Value) {
        use ainb_hangar_proto::events::EVENT_METHOD;
        if value.get("method").and_then(serde_json::Value::as_str) != Some(EVENT_METHOD) {
            return;
        }
        let Some(params) = value.get("params") else {
            return;
        };
        let Ok(event) = serde_json::from_value::<HangarEvent>(params.clone()) else {
            return;
        };
        self.apply_hangar_event(event);
        // A pushed event is the steady state — keep the link Connected and ask
        // for a re-pull so every screen's derived columns reconcile.
        self.conn.on_event();
        self.fetch_pending = true;
    }

    /// Fold a typed [`HangarEvent`] into the issue-list + Kanban caches so a
    /// pushed mutation (create / update / task lifecycle) re-renders within a
    /// tick, ahead of the reconciling snapshot re-pull (e38.29).
    fn apply_hangar_event(&mut self, event: HangarEvent) {
        use crate::screen::issue_list::{reduce_issue_list, IssueListEvent};
        use crate::screen::kanban::{reduce_kanban, KanbanEvent};
        self.screens.issue_list = reduce_issue_list(
            &self.screens.issue_list,
            IssueListEvent::Event(event.clone()),
        )
        .state;
        self.screens.kanban = reduce_kanban(&self.screens.kanban, KanbanEvent::Event(event)).state;
    }

    /// React to one fully-decoded daemon response.
    fn on_daemon_response(&mut self, resp: &RpcResponse) {
        match resp.id {
            // The auth ack precedes the subscribe ack (e38.1). Success is
            // silent (the subscribe ack drives the state machine forward); a
            // rejection surfaces as a connection error so the chrome shows why
            // the daemon is unreachable.
            RpcId::Number(AUTH_REQ_ID) => {
                if let Some(err) = &resp.error {
                    self.conn
                        .on_error(format!("daemon auth rejected: {}", err.message));
                }
            }
            // The subscribe ack completes the handshake and arms the snapshot
            // fetch (issued by `handle_event`, which holds the `host`).
            RpcId::Number(SUBSCRIBE_REQ_ID) => {
                if resp.error.is_some() {
                    self.conn
                        .on_error("daemon rejected workspace/subscribe".to_string());
                } else {
                    self.conn.on_subscribe_ack();
                    self.fetch_pending = true;
                }
            }
            RpcId::Number(ISSUES_REQ_ID) => self.apply_issues(resp),
            RpcId::Number(AGENTS_REQ_ID) => self.apply_agents(resp),
            RpcId::Number(SKILLS_REQ_ID) => self.apply_skills(resp),
            RpcId::Number(HEALTH_REQ_ID) => self.apply_health(resp),
            RpcId::Number(SKILL_GET_REQ_ID) => self.apply_skill_detail(resp),
            RpcId::Number(AUTOPILOTS_REQ_ID) => self.apply_autopilots(resp),
            RpcId::Number(AUTOPILOT_RUNS_REQ_ID) => self.apply_autopilot_runs(resp),
            RpcId::Number(TASKS_REQ_ID) => self.apply_tasks(resp),
            RpcId::Number(DAEMON_HEALTH_REQ_ID) => self.apply_daemon_health(resp),
            RpcId::Number(USAGE_ROLLUP_REQ_ID) => self.apply_usage(resp),
            RpcId::Number(PR_STATUS_REFRESH_REQ_ID) => self.apply_pr_status(resp),
            RpcId::Number(MEMBERS_REQ_ID) => self.apply_members(resp),
            RpcId::Number(INBOX_LIST_REQ_ID) => self.apply_inbox(resp),
            RpcId::Number(SEARCH_REQ_ID) => self.apply_search(resp),
            // Mutating RPCs (skill sync/attach/detach, autopilot fire/toggle,
            // kanban task transition, issue assign, inbox mark-read) answer with
            // the changed row or `{}`; we re-fetch the relevant lists to refresh
            // derived columns (`used`, next-tick, enabled, last-run, task status
            // buckets, issue assignee, inbox unread count).
            RpcId::Number(
                SKILLS_SYNC_REQ_ID
                | SKILL_ATTACH_REQ_ID
                | SKILL_DETACH_REQ_ID
                | AUTOPILOT_FIRE_REQ_ID
                | AUTOPILOT_TOGGLE_REQ_ID
                | TASK_TRANSITION_REQ_ID
                | ISSUE_UPDATE_REQ_ID
                | ISSUE_CREATE_REQ_ID
                | INBOX_MARK_READ_REQ_ID,
            ) => {
                self.fetch_pending = true;
                self.conn.on_event();
            }
            // Any other response/event keeps the link alive.
            _ => self.conn.on_event(),
        }
    }

    /// Populate the issue-list cache from an `hangar/issues_list` result.
    fn apply_issues(&mut self, resp: &RpcResponse) {
        if let Some(result) = &resp.result {
            if let Ok(r) = serde_json::from_value::<ainb_hangar_proto::snapshots::IssuesListResult>(
                result.clone(),
            ) {
                self.screens.set_issues(r.issues);
            }
        }
    }

    /// Populate the actor cache from an `hangar/agents_list` result.
    fn apply_agents(&mut self, resp: &RpcResponse) {
        if let Some(result) = &resp.result {
            if let Ok(r) = serde_json::from_value::<ainb_hangar_proto::snapshots::AgentsListResult>(
                result.clone(),
            ) {
                self.screens.set_actors(r.actors);
            }
        }
    }

    /// Populate the settings Members pane from a `hangar/members_list` result
    /// (e38.11). The pane is render-only, so the rows are simply cached.
    fn apply_members(&mut self, resp: &RpcResponse) {
        if let Some(result) = &resp.result {
            if let Ok(r) = serde_json::from_value::<ainb_hangar_proto::snapshots::MembersListResult>(
                result.clone(),
            ) {
                self.screens.set_members(r.members);
            }
        }
    }

    /// Populate the Inbox screen from a `hangar/inbox_list` result (e38.14): the
    /// aggregated issue/comment/task entries + the unread count for the badge.
    fn apply_inbox(&mut self, resp: &RpcResponse) {
        if let Some(result) = &resp.result {
            if let Ok(r) = serde_json::from_value::<ainb_hangar_proto::snapshots::InboxListResult>(
                result.clone(),
            ) {
                self.screens.set_inbox(r.entries, r.unread);
            }
        }
    }

    /// Fold a `hangar/search` result into the open command palette (e38.13): the
    /// ranked cross-entity entries the palette renders + jumps from. A no-op when
    /// the palette has since closed (a stale reply for a dismissed modal).
    fn apply_search(&mut self, resp: &RpcResponse) {
        if let Some(result) = &resp.result {
            if let Ok(r) =
                serde_json::from_value::<ainb_hangar_proto::snapshots::SearchResult>(result.clone())
            {
                self.screens.set_palette_results(r.entries);
            }
        }
    }

    /// Populate the skill-manager cache from an `hangar/skills_list` result.
    fn apply_skills(&mut self, resp: &RpcResponse) {
        if let Some(result) = &resp.result {
            if let Ok(r) = serde_json::from_value::<ainb_hangar_proto::snapshots::SkillsListResult>(
                result.clone(),
            ) {
                self.screens.set_skills(r.skills);
            }
        }
    }

    /// Populate the autopilot-manager cache from an `hangar/autopilots_list`
    /// result (P7.5).
    fn apply_autopilots(&mut self, resp: &RpcResponse) {
        if let Some(result) = &resp.result {
            if let Ok(r) = serde_json::from_value::<
                ainb_hangar_proto::snapshots::AutopilotsListResult,
            >(result.clone())
            {
                self.screens.set_autopilots(r.autopilots);
            }
        }
    }

    /// Populate the Kanban board cache from a `hangar/tasks_list` result (P8.4).
    fn apply_tasks(&mut self, resp: &RpcResponse) {
        if let Some(result) = &resp.result {
            if let Ok(r) = serde_json::from_value::<ainb_hangar_proto::snapshots::TasksListResult>(
                result.clone(),
            ) {
                self.screens.set_tasks(&r.tasks);
            }
        }
    }

    /// Populate the daemon-health pane from a `hangar/daemon_health` result
    /// (P8.5).
    fn apply_daemon_health(&mut self, resp: &RpcResponse) {
        if let Some(result) = &resp.result {
            if let Ok(snap) = serde_json::from_value::<
                ainb_hangar_proto::settings::DaemonHealthSnapshot,
            >(result.clone())
            {
                self.screens.set_daemon_health(snap);
            }
        }
    }

    /// Populate the usage dashboard from a `hangar/usage_rollup` result (e38.35):
    /// the grand token/cost totals + the per-agent breakdown.
    fn apply_usage(&mut self, resp: &RpcResponse) {
        if let Some(result) = &resp.result {
            if let Ok(rollup) = serde_json::from_value::<
                ainb_hangar_proto::snapshots::UsageRollupResult,
            >(result.clone())
            {
                self.screens.set_usage(rollup);
            }
        }
    }

    /// Re-read the daemon's structured-log file into the logs pane (P8.6).
    ///
    /// Resolves the log dir the same way the daemon writes it
    /// ([`ainb_hangar_core::logs::default_log_dir`]) and reads the newest
    /// `daemon.*` file's last `LOGS_TAIL_LINES` lines under the screen's active
    /// `--level` floor. A missing dir / file yields an empty pane (no panic).
    fn refresh_logs(&mut self) {
        let filter = self.screens.logs.filter();
        let lines = ainb_hangar_core::logs::default_log_dir().map_or_else(Vec::new, |dir| {
            ainb_hangar_core::logs::read_tail(&dir, LOGS_TAIL_LINES, filter)
        });
        self.screens.set_logs(lines);
    }

    /// Fire a deferred `hangar/inbox_mark_read` raised by the inbox `r` key
    /// (e38.14): mark every unread entry read, framed over the socket cap. The
    /// mutating-RPC reply re-fetches the snapshots so the unread badge drops to
    /// zero. A send failure is logged but non-fatal — the badge simply stays.
    async fn mark_inbox_read(&mut self, host: &HostClient) {
        let Some(stream_id) = self.conn.stream_id().map(ToString::to_string) else {
            return;
        };
        let ws = self.app_state().ws_id.as_str().to_string();
        let params = serde_json::json!({ "workspace_id": ws });
        let Ok(body) = encode_request(
            INBOX_MARK_READ_REQ_ID,
            daemon_methods::HANGAR_INBOX_MARK_READ,
            params,
        ) else {
            return;
        };
        if let Err(e) = host.unix_socket_send(stream_id, body).await {
            let _ = host
                .log_info(format!("hangar: inbox mark-read send failed: {e}"))
                .await;
        }
    }

    /// Fire a deferred `hangar/search` raised by the command palette (e38.13):
    /// the ranked cross-entity search for the typed `query`, framed over the
    /// socket cap. The read reply (`apply_search`) folds the entries back into the
    /// palette. A send failure is logged but non-fatal — the results simply don't
    /// update.
    async fn run_palette_search(&mut self, host: &HostClient, query: String) {
        let Some(stream_id) = self.conn.stream_id().map(ToString::to_string) else {
            return;
        };
        let ws = self.app_state().ws_id.as_str().to_string();
        let params = serde_json::json!({ "workspace_id": ws, "query": query });
        let Ok(body) = encode_request(SEARCH_REQ_ID, daemon_methods::HANGAR_SEARCH, params) else {
            return;
        };
        if let Err(e) = host.unix_socket_send(stream_id, body).await {
            let _ = host
                .log_info(format!("hangar: search send failed: {e}"))
                .await;
        }
    }

    /// Fold a `hangar/autopilot_runs` result onto the autopilot-manager screen
    /// (P7.5), keyed by the autopilot id the request was issued for so a stale
    /// reply for a since-changed selection is ignored by the reducer.
    fn apply_autopilot_runs(&mut self, resp: &RpcResponse) {
        let Some(autopilot_id) = self.pending_runs_autopilot.take() else {
            return;
        };
        let Some(result) = &resp.result else {
            return;
        };
        if let Ok(r) = serde_json::from_value::<ainb_hangar_proto::snapshots::AutopilotRunsResult>(
            result.clone(),
        ) {
            let event = crate::screen::autopilots::AutopilotsEvent::RunsLoaded {
                autopilot_id,
                runs: r.runs,
            };
            let out = crate::screen::autopilots::reduce_autopilots(&self.screens.autopilots, event);
            self.screens.autopilots = out.state;
        }
    }

    /// Fold a `hangar/skill_get` detail result onto the skill-manager screen
    /// (P6.5): the SKILL.md body + file list, keyed by the slug the request was
    /// issued for so a stale reply for a since-changed selection is ignored.
    fn apply_skill_detail(&mut self, resp: &RpcResponse) {
        let Some(slug) = self.pending_detail_slug.take() else {
            return;
        };
        // A `null` result (skill vanished / foreign workspace) leaves the pane
        // empty rather than erroring.
        let Some(result) = &resp.result else {
            return;
        };
        if let Ok(Some(detail)) = serde_json::from_value::<
            Option<ainb_hangar_proto::snapshots::SkillDetail>,
        >(result.clone())
        {
            let event = crate::screen::skill_manager::SkillManagerEvent::DetailLoaded {
                slug,
                body: detail.body.unwrap_or_default(),
                files: detail.files,
            };
            let out = crate::screen::skill_manager::reduce_skill_manager(
                &self.screens.skill_manager,
                event,
            );
            self.screens.skill_manager = out.state;
        }
    }

    /// Build the settings cache from an `hangar/health` result.
    fn apply_health(&mut self, resp: &RpcResponse) {
        if let Some(result) = &resp.result {
            if let Ok(h) = serde_json::from_value::<ainb_hangar_proto::settings::HealthSnapshot>(
                result.clone(),
            ) {
                let ws = self.app_state().ws_id.as_str().to_string();
                self.screens.set_health(h, &ws);
            }
        }
    }

    /// Fire every `hangar/*` snapshot request over the daemon stream, framed for
    /// the cap (one per landing screen — issues, agents, skills, autopilots,
    /// tasks, daemon-health, usage, members, health). A send failure is logged but
    /// non-fatal — the screens simply stay empty until the next subscribe.
    async fn fetch_snapshots(&mut self, host: &HostClient) {
        let Some(stream_id) = self.conn.stream_id().map(ToString::to_string) else {
            return;
        };
        let ws = self.app_state().ws_id.as_str().to_string();
        let scoped = serde_json::json!({ "workspace_id": ws });
        let requests = [
            (
                ISSUES_REQ_ID,
                daemon_methods::HANGAR_ISSUES_LIST,
                scoped.clone(),
            ),
            (
                AGENTS_REQ_ID,
                daemon_methods::HANGAR_AGENTS_LIST,
                scoped.clone(),
            ),
            (
                SKILLS_REQ_ID,
                daemon_methods::HANGAR_SKILLS_LIST,
                scoped.clone(),
            ),
            (
                AUTOPILOTS_REQ_ID,
                daemon_methods::HANGAR_AUTOPILOTS_LIST,
                scoped.clone(),
            ),
            (
                TASKS_REQ_ID,
                daemon_methods::HANGAR_TASKS_LIST,
                scoped.clone(),
            ),
            (
                DAEMON_HEALTH_REQ_ID,
                daemon_methods::HANGAR_DAEMON_HEALTH,
                scoped.clone(),
            ),
            (
                USAGE_ROLLUP_REQ_ID,
                daemon_methods::HANGAR_USAGE_ROLLUP,
                scoped.clone(),
            ),
            (
                MEMBERS_REQ_ID,
                daemon_methods::HANGAR_MEMBERS_LIST,
                scoped.clone(),
            ),
            (
                INBOX_LIST_REQ_ID,
                daemon_methods::HANGAR_INBOX_LIST,
                scoped.clone(),
            ),
            (
                HEALTH_REQ_ID,
                daemon_methods::HANGAR_HEALTH,
                serde_json::json!({}),
            ),
        ];
        for (id, method, params) in requests {
            let Ok(body) = encode_request(id, method, params) else {
                continue;
            };
            if let Err(e) = host.unix_socket_send(stream_id.clone(), body).await {
                let _ = host
                    .log_info(format!("hangar: snapshot send failed: {e}"))
                    .await;
            }
        }
    }

    /// Fire a deferred skill RPC raised by the skill-manager screen (P6.5).
    ///
    /// Maps each [`SkillAction`] to its daemon JSON-RPC, framed over the socket
    /// cap. `Attach`/`Detach` need a target agent: the skill screen has no agent
    /// selector yet, so the first cached agent actor is used (v1 is
    /// single-agent-per-workspace in the seed; a richer selector lands later). A
    /// send failure is logged but non-fatal — the screen simply doesn't update.
    async fn apply_skill_action(&mut self, host: &HostClient, action: crate::screen::SkillAction) {
        use crate::screen::SkillAction;
        let Some(stream_id) = self.conn.stream_id().map(ToString::to_string) else {
            return;
        };
        let ws = self.app_state().ws_id.as_str().to_string();
        let (id, method, params) = match action {
            SkillAction::Sync => (
                SKILLS_SYNC_REQ_ID,
                daemon_methods::HANGAR_SKILLS_SYNC,
                serde_json::json!({ "workspace_id": ws }),
            ),
            SkillAction::LoadDetail(slug) => {
                self.pending_detail_slug = Some(slug.clone());
                (
                    SKILL_GET_REQ_ID,
                    daemon_methods::HANGAR_SKILL_GET,
                    serde_json::json!({ "workspace_id": ws, "skill_id": slug }),
                )
            }
            SkillAction::Attach(slug) => {
                let Some(agent) = self.first_agent_ref() else {
                    let _ = host.log_info("hangar: no agent to attach skill to").await;
                    return;
                };
                (
                    SKILL_ATTACH_REQ_ID,
                    daemon_methods::HANGAR_SKILL_ATTACH,
                    serde_json::json!({ "workspace_id": ws, "agent_id": agent, "skill_id": slug }),
                )
            }
            SkillAction::Detach(slug) => {
                let Some(agent) = self.first_agent_ref() else {
                    let _ = host.log_info("hangar: no agent to detach skill from").await;
                    return;
                };
                (
                    SKILL_DETACH_REQ_ID,
                    daemon_methods::HANGAR_SKILL_DETACH,
                    serde_json::json!({ "workspace_id": ws, "agent_id": agent, "skill_id": slug }),
                )
            }
        };
        let Ok(body) = encode_request(id, method, params) else {
            return;
        };
        if let Err(e) = host.unix_socket_send(stream_id, body).await {
            let _ = host
                .log_info(format!("hangar: skill rpc send failed: {e}"))
                .await;
        }
    }

    /// Fire a deferred Kanban card-move RPC raised by the board (P8.4).
    ///
    /// Maps the [`KanbanAction::MoveCard`] to `hangar/task_transition`, framed over
    /// the socket cap. A send failure is logged but non-fatal — the board simply
    /// doesn't move (the next snapshot reconciles).
    async fn apply_kanban_action(
        &mut self,
        host: &HostClient,
        action: crate::screen::KanbanAction,
    ) {
        use crate::screen::KanbanAction;
        let Some(stream_id) = self.conn.stream_id().map(ToString::to_string) else {
            return;
        };
        let ws = self.app_state().ws_id.as_str().to_string();
        let KanbanAction::MoveCard { task_id, to_status } = action;
        let params = serde_json::json!({
            "workspace_id": ws, "task_id": task_id, "to_status": to_status
        });
        let Ok(body) = encode_request(
            TASK_TRANSITION_REQ_ID,
            daemon_methods::HANGAR_TASK_TRANSITION,
            params,
        ) else {
            return;
        };
        if let Err(e) = host.unix_socket_send(stream_id, body).await {
            let _ = host
                .log_info(format!("hangar: task transition send failed: {e}"))
                .await;
        }
    }

    /// Fire a deferred issue-assign RPC raised by the agent-picker modal (e38.8).
    ///
    /// Maps the [`IssueAssignAction::Assign`] to `hangar/issue_update`, setting
    /// the issue's `assignee` to the picked actor-ref, framed over the socket cap.
    /// A send failure is logged but non-fatal — the assignee simply doesn't change
    /// (the next snapshot reconciles).
    async fn apply_assign_action(
        &mut self,
        host: &HostClient,
        action: crate::screen::IssueAssignAction,
    ) {
        use crate::screen::IssueAssignAction;
        let Some(stream_id) = self.conn.stream_id().map(ToString::to_string) else {
            return;
        };
        let ws = self.app_state().ws_id.as_str().to_string();
        let IssueAssignAction::Assign {
            issue_id,
            actor_ref,
        } = action;
        let params = serde_json::json!({
            "workspace_id": ws, "issue_id": issue_id, "assignee": actor_ref
        });
        let Ok(body) = encode_request(
            ISSUE_UPDATE_REQ_ID,
            daemon_methods::HANGAR_ISSUE_UPDATE,
            params,
        ) else {
            return;
        };
        if let Err(e) = host.unix_socket_send(stream_id, body).await {
            let _ = host
                .log_info(format!("hangar: issue update send failed: {e}"))
                .await;
        }
    }

    /// Fire the deferred cross-column issue-move RPC raised by a board drag-drop
    /// (63l.4).
    ///
    /// Maps the `(issue_id, to_status)` to `hangar/issue_update`, setting the
    /// issue's lifecycle `state` to the destination column's canonical wire token
    /// (`IssueLifecycle::as_str`), framed over the socket cap — the SAME
    /// `ISSUE_UPDATE_REQ_ID` seam the agent picker uses for assignee edits. The
    /// daemon's `IssueUpdated` push reconciles the optimistic local move (the board
    /// already shows the card in the new column). A send failure is logged but
    /// non-fatal — the next snapshot reconciles the (now-stale) optimistic move.
    async fn apply_issue_state_update(
        &mut self,
        host: &HostClient,
        issue_id: String,
        to_status: ainb_hangar_proto::lifecycle::IssueLifecycle,
    ) {
        let Some(stream_id) = self.conn.stream_id().map(ToString::to_string) else {
            return;
        };
        let ws = self.app_state().ws_id.as_str().to_string();
        let params = serde_json::json!({
            "workspace_id": ws, "issue_id": issue_id, "state": to_status.as_str()
        });
        let Ok(body) = encode_request(
            ISSUE_UPDATE_REQ_ID,
            daemon_methods::HANGAR_ISSUE_UPDATE,
            params,
        ) else {
            return;
        };
        if let Err(e) = host.unix_socket_send(stream_id, body).await {
            let _ = host
                .log_info(format!("hangar: issue state move send failed: {e}"))
                .await;
        }
    }

    /// Fire the deferred issue-priority RPC raised by the context menu's
    /// `Priority ▸` submenu (63l.5).
    ///
    /// Maps the `(issue_id, priority)` to `hangar/issue_update`, setting the
    /// issue's `priority` scalar (`0..3`) over the SAME `ISSUE_UPDATE_REQ_ID` seam
    /// the assignee / state edits use. The daemon's `IssueUpdated` push re-renders
    /// the new chip. A send failure is logged but non-fatal — the next snapshot
    /// reconciles.
    async fn apply_issue_priority_update(
        &mut self,
        host: &HostClient,
        issue_id: String,
        priority: i64,
    ) {
        let Some(stream_id) = self.conn.stream_id().map(ToString::to_string) else {
            return;
        };
        let ws = self.app_state().ws_id.as_str().to_string();
        let params = serde_json::json!({
            "workspace_id": ws, "issue_id": issue_id, "priority": priority
        });
        let Ok(body) = encode_request(
            ISSUE_UPDATE_REQ_ID,
            daemon_methods::HANGAR_ISSUE_UPDATE,
            params,
        ) else {
            return;
        };
        if let Err(e) = host.unix_socket_send(stream_id, body).await {
            let _ = host
                .log_info(format!("hangar: issue priority send failed: {e}"))
                .await;
        }
    }

    /// Fire the deferred issue-assignee RPC raised by the context menu's
    /// `Assign ▸` submenu (63l.5).
    ///
    /// Maps the `(issue_id, actor_ref)` to `hangar/issue_update`, setting the
    /// issue's `assignee` to the picked canonical `member:<id>` / `agent:<id>` ref
    /// over the SAME `ISSUE_UPDATE_REQ_ID` seam the agent-picker assign uses. The
    /// daemon's `IssueUpdated` push re-renders the assignee. A send failure is
    /// logged but non-fatal — the next snapshot reconciles.
    async fn apply_issue_assignee_update(
        &mut self,
        host: &HostClient,
        issue_id: String,
        actor_ref: String,
    ) {
        let Some(stream_id) = self.conn.stream_id().map(ToString::to_string) else {
            return;
        };
        let ws = self.app_state().ws_id.as_str().to_string();
        let params = serde_json::json!({
            "workspace_id": ws, "issue_id": issue_id, "assignee": actor_ref
        });
        let Ok(body) = encode_request(
            ISSUE_UPDATE_REQ_ID,
            daemon_methods::HANGAR_ISSUE_UPDATE,
            params,
        ) else {
            return;
        };
        if let Err(e) = host.unix_socket_send(stream_id, body).await {
            let _ = host
                .log_info(format!("hangar: issue assignee send failed: {e}"))
                .await;
        }
    }

    /// Fire a deferred issue-comment RPC raised by the task-detail compose modal
    /// (e38.5).
    ///
    /// Maps the [`IssueCommentAction::Add`] to `hangar/comment_add`, posting the
    /// typed body on the issue authored by the current member, framed over the
    /// socket cap. The daemon's `CommentAdded` push re-renders the new comment
    /// (mirroring `apply_assign_action` — this fires the RPC only, no separate
    /// re-pull). A send failure is logged but non-fatal — the comment simply
    /// isn't posted.
    async fn apply_comment_action(
        &mut self,
        host: &HostClient,
        action: crate::screen::IssueCommentAction,
    ) {
        use crate::screen::IssueCommentAction;
        let Some(stream_id) = self.conn.stream_id().map(ToString::to_string) else {
            return;
        };
        let ws = self.app_state().ws_id.as_str().to_string();
        let IssueCommentAction::Add { issue_id, body } = action;
        let params = serde_json::json!({
            "workspace_id": ws, "issue_id": issue_id, "author": SELF_AUTHOR_REF, "body": body
        });
        let Ok(body) = encode_request(
            COMMENT_ADD_REQ_ID,
            daemon_methods::HANGAR_COMMENT_ADD,
            params,
        ) else {
            return;
        };
        if let Err(e) = host.unix_socket_send(stream_id, body).await {
            let _ = host
                .log_info(format!("hangar: comment add send failed: {e}"))
                .await;
        }
    }

    /// Fire the deferred `hangar/pr_status_refresh` for the just-opened
    /// task-detail's issue (e38.34).
    ///
    /// Requests the issue's bound PR status, framed over the socket cap. The reply
    /// ([`Self::apply_pr_status`]) folds the CI + merge status onto the badge; when
    /// the PR is merged the daemon also auto-moves the issue to Done and pushes
    /// `IssueUpdated`, so a subscribed board reflects the column move without a
    /// separate re-pull. A send failure is logged but non-fatal — the badge simply
    /// keeps its prior (unknown) status.
    async fn fire_pr_status_refresh(&mut self, host: &HostClient, issue_id: String) {
        let Some(stream_id) = self.conn.stream_id().map(ToString::to_string) else {
            return;
        };
        let ws = self.app_state().ws_id.as_str().to_string();
        let params = serde_json::json!({ "workspace_id": ws, "issue_id": issue_id });
        let Ok(body) = encode_request(
            PR_STATUS_REFRESH_REQ_ID,
            daemon_methods::HANGAR_PR_STATUS_REFRESH,
            params,
        ) else {
            return;
        };
        if let Err(e) = host.unix_socket_send(stream_id, body).await {
            let _ = host
                .log_info(format!("hangar: pr status refresh send failed: {e}"))
                .await;
        }
    }

    /// Fold a `hangar/pr_status_refresh` reply onto the open task-detail badge
    /// (e38.34): apply the fetched CI + merge status. A merged-PR transition was
    /// already performed daemon-side (and announced via `IssueUpdated`), so the
    /// plugin only mirrors the status here. A malformed / error reply is ignored
    /// (the badge keeps its prior status).
    fn apply_pr_status(&mut self, resp: &RpcResponse) {
        if let Some(result) = &resp.result {
            if let Ok(reply) = serde_json::from_value::<
                ainb_hangar_proto::snapshots::PrStatusRefreshResult,
            >(result.clone())
            {
                self.screens.set_task_detail_pr_status(reply.status);
            }
        }
    }

    /// Fire a deferred issue-create RPC raised by the issue-list inline create
    /// flow (e38.29).
    ///
    /// Maps the [`IssueCreateAction::Create`] to `hangar/issue_create`, creating a
    /// new issue with the typed title authored by the current member, framed over
    /// the socket cap. The daemon's `IssueCreated` push re-renders the new row
    /// (mirroring `apply_comment_action` — this fires the RPC only, no separate
    /// re-pull). A send failure is logged but non-fatal — the issue simply isn't
    /// created.
    async fn apply_create_action(
        &mut self,
        host: &HostClient,
        action: crate::screen::IssueCreateAction,
    ) {
        use crate::screen::IssueCreateAction;
        let Some(stream_id) = self.conn.stream_id().map(ToString::to_string) else {
            return;
        };
        let ws = self.app_state().ws_id.as_str().to_string();
        let IssueCreateAction::Create { title } = action;
        let params = serde_json::json!({
            "workspace_id": ws, "title": title, "creator": SELF_AUTHOR_REF
        });
        let Ok(body) = encode_request(
            ISSUE_CREATE_REQ_ID,
            daemon_methods::HANGAR_ISSUE_CREATE,
            params,
        ) else {
            return;
        };
        if let Err(e) = host.unix_socket_send(stream_id, body).await {
            let _ = host
                .log_info(format!("hangar: issue create send failed: {e}"))
                .await;
        }
    }

    /// Fire a deferred autopilot RPC raised by the autopilot-manager screen
    /// (P7.5).
    ///
    /// Maps each [`AutopilotAction`] to its daemon JSON-RPC, framed over the
    /// socket cap: `LoadRuns` → `hangar/autopilot_runs`, `FireNow` →
    /// `hangar/autopilot_fire_now`, `SetEnabled` →
    /// `hangar/autopilot_set_enabled`. A send failure is logged but non-fatal —
    /// the screen simply doesn't update.
    async fn apply_autopilot_action(
        &mut self,
        host: &HostClient,
        action: crate::screen::AutopilotAction,
    ) {
        use crate::screen::AutopilotAction;
        let Some(stream_id) = self.conn.stream_id().map(ToString::to_string) else {
            return;
        };
        let ws = self.app_state().ws_id.as_str().to_string();
        let (id, method, params) = match action {
            AutopilotAction::LoadRuns(ap) => {
                self.pending_runs_autopilot = Some(ap.clone());
                (
                    AUTOPILOT_RUNS_REQ_ID,
                    daemon_methods::HANGAR_AUTOPILOT_RUNS,
                    serde_json::json!({ "workspace_id": ws, "autopilot_id": ap, "limit": 10 }),
                )
            }
            AutopilotAction::FireNow(ap) => (
                AUTOPILOT_FIRE_REQ_ID,
                daemon_methods::HANGAR_AUTOPILOT_FIRE_NOW,
                serde_json::json!({ "workspace_id": ws, "autopilot_id": ap }),
            ),
            AutopilotAction::SetEnabled {
                autopilot_id,
                enabled,
            } => (
                AUTOPILOT_TOGGLE_REQ_ID,
                daemon_methods::HANGAR_AUTOPILOT_SET_ENABLED,
                serde_json::json!({ "workspace_id": ws, "autopilot_id": autopilot_id, "enabled": enabled }),
            ),
        };
        let Ok(body) = encode_request(id, method, params) else {
            return;
        };
        if let Err(e) = host.unix_socket_send(stream_id, body).await {
            let _ = host
                .log_info(format!("hangar: autopilot rpc send failed: {e}"))
                .await;
        }
    }

    /// The bare id of the first cached agent actor (`agent:<id>` → `<id>`), or
    /// `None` when no agent is cached. The attach/detach target until a proper
    /// agent selector lands.
    fn first_agent_ref(&self) -> Option<String> {
        self.screens
            .actors
            .iter()
            .find(|a| a.is_agent)
            .and_then(|a| a.actor_ref.strip_prefix("agent:").map(str::to_string))
    }

    /// Apply a deferred Settings Workspace action (P5.5): call the matching
    /// `host/workspace_*` cap, then re-fetch the workspace list and fold it into
    /// the Settings pane + the routing state's active workspace.
    ///
    /// A cap error (e.g. `-32001` if the grant were withdrawn) is logged but
    /// non-fatal: the pane simply stays on the prior active workspace.
    ///
    /// # Data-plane re-scope (e38.26)
    ///
    /// A `SetActive` switch must re-scope the DATA plane, not just the active
    /// `▶` marker / top-bar slug. `refresh_workspaces` advances
    /// `app_state().ws_id` to the newly-active workspace, but the cached
    /// snapshots (issues / agents / skills / tasks / autopilots) still hold the
    /// PRIOR workspace's rows until they are re-pulled. Without the re-fetch the
    /// issue list would keep showing the old tenant's issues after the switch —
    /// a cross-tenant data leak. So after a `SetActive`, re-issue the
    /// workspace-scoped snapshot requests (now scoped to the new `ws_id`).
    async fn apply_workspace_action(&mut self, host: &HostClient, action: WorkspaceAction) {
        let result = match &action {
            // A bare refresh just pulls the list (no mutating cap call).
            WorkspaceAction::Refresh => Ok(()),
            WorkspaceAction::SetActive(id) => {
                host.workspace_set_active(id.clone()).await.map(|_| ())
            }
            WorkspaceAction::SetDefault(id) => {
                host.workspace_set_default(id.clone()).await.map(|_| ())
            }
        };
        if let Err(e) = result {
            let _ = host
                .log_info(format!("hangar: workspace action failed: {e}"))
                .await;
            return;
        }
        self.refresh_workspaces(host).await;
        // After an active-workspace switch, re-pull every workspace-scoped
        // snapshot so the screens reflect the NEW tenant's data (not the prior
        // one's stale cache). `refresh_workspaces` already moved `ws_id`, and
        // `fetch_snapshots` reads it, so the re-fetch is scoped to the switch
        // target.
        if matches!(action, WorkspaceAction::SetActive(_)) {
            self.fetch_snapshots(host).await;
        }
    }

    /// Pull the host workspace list and fold it into the Settings pane + the
    /// routing state's active workspace slug.
    async fn refresh_workspaces(&mut self, host: &HostClient) {
        let Ok(list) = host.workspace_list().await else {
            return;
        };
        let rows: Vec<WorkspaceRow> = list
            .workspaces
            .iter()
            .map(|w| WorkspaceRow {
                id: w.id.clone(),
                slug: w.slug.clone(),
                name: w.name.clone(),
                current: w.active,
                default: w.default,
            })
            .collect();
        // Update the routing state's active workspace so the top-bar slug and
        // every workspace-scoped fetch follow the switch.
        if let Some(active) = list.workspaces.iter().find(|w| w.active) {
            if let Ok(ws) = WorkspaceId::from_str(active.id.clone()) {
                let mut next = self.app_state().clone();
                next.ws_id = ws;
                self.app = Some(next);
            }
        }
        self.screens.set_workspaces(rows);
    }

    /// Compose the full render frame into a fresh [`WireBuffer`] (the pure,
    /// host-free tail of [`Plugin::render`]).
    ///
    /// Paints the shared chrome (top bar + body + footer), then overlays the
    /// e38.36 offline empty-state when the daemon link is genuinely down, then the
    /// P5.6 first-run danger modal (top-most). Split out so the offline-overlay
    /// branch is unit-testable without a [`HostClient`] — the async `render` only
    /// drains deferred host IO and then delegates here.
    fn compose_frame(&mut self, w: u16, h: u16) -> WireBuffer {
        let mut buf = WireBuffer::new(w, h);

        // Shared chrome (P4.1): top tab bar on row 0, contextual footer on the
        // last row. The active screen body (rows 1..h-1) is filled by the
        // per-screen renderers (P4.3..P4.7) dispatched via `render_body`.
        let presence = Self::presence(self.conn.state());
        let app = self.app_state().clone();
        // Display the active workspace's slug, not its raw ULID id: after a
        // switch `ws_id` holds the ULID, so resolve it back to the slug via the
        // cached workspace catalogue (falling back to the id when unknown).
        let ws_slug = self
            .screens
            .workspace_rows
            .iter()
            .find(|r| r.id == app.ws_id.as_str())
            .map_or_else(|| app.ws_id.as_str().to_string(), |r| r.slug.clone());
        render_top_bar(&mut buf, w, &app.screen, &ws_slug, presence);
        render_body(&mut buf, w, h, &app, &self.screens);
        render_footer(&mut buf, w, h, &app.screen);
        // e38.36: when the daemon link is genuinely down (Disconnected/Error) the
        // body is an empty void (all-zero counts, no rows). Overlay the offline
        // empty-state panel so the landing explains the daemon is offline and
        // offers BOTH the one-key `[s] start daemon` action AND the literal
        // `ainb hangar daemon start` command, instead of reading as broken. The
        // panel sits ABOVE the body but BELOW the first-run modal (which stays the
        // top-most overlay on a fresh machine).
        if Self::is_offline(self.conn.state()) {
            crate::widgets::offline_empty_state::render_offline_empty_state(
                &mut buf,
                w,
                h,
                self.daemon_start_error.as_deref(),
            );
        }
        // 63l.5: the right-click context menu floats over the board, anchored at
        // the click. It sits ABOVE the body but BELOW the first-run modal (which
        // stays the top-most overlay on a fresh machine). The render records its
        // hit-map onto the state so the next click hit-tests against this paint.
        if let Some(menu) = self.context_menu.as_mut() {
            crate::screen::context_menu::render_context_menu(&mut buf, w, h, menu);
        }
        // 63l.6: the generic list-screen context menu (Kanban / Autopilots /
        // Skills) floats over the board the same way, anchored at the click. It
        // records its hit-map onto the state so the next click hit-tests against
        // this paint.
        if let Some(menu) = self.list_context_menu.as_mut() {
            crate::screen::list_context_menu::render_list_context_menu(&mut buf, w, h, menu);
        }
        // P5.6: the first-run danger-full-access modal overlays everything (last
        // write wins on the sparse buffer) until the user accepts.
        if self.first_run.is_showing() {
            crate::widgets::danger_access_modal::render_danger_access_modal(&mut buf, w, h);
        }
        buf
    }

    /// Fold a forwarded key: tab-switch / modal keys go through the routing
    /// reducer ([`crate::screen::reduce`]); everything else routes to the active
    /// screen's reducer via [`route_key`], whose cross-screen [`NavIntent`]s
    /// drive the routing-state transition + lazy modal construction here.
    fn on_key(&mut self, key: &ainb_plugin_sdk::KeyEvent) {
        // P5.6: while the first-run danger-full-access modal is showing it is
        // *modal* — it captures every key. `y` dismisses + arms the ack persist
        // (drained in `render`, where host IO is safe); `q` falls through to the
        // host's quit; every other key is swallowed so it can't leak to the
        // screen beneath the warning.
        if self.first_run.is_showing() {
            if let KeyCode::Char { ch } = key.code {
                if ch == 'q' {
                    // Let the host handle quit; don't dismiss the warning.
                    return;
                }
                let out = reduce_first_run(self.first_run, ch);
                self.first_run = out.state;
                if matches!(out.intent, Some(FirstRunIntent::AckFirstRun)) {
                    self.first_run_ack_pending = true;
                }
            }
            return;
        }

        // 63l.5: while the context menu is open it is *modal* — it captures every
        // key. Arrows / `hjkl` navigate, Enter / `l` fire-or-open, Esc / `h`
        // collapse-or-close. A fired leaf is applied here (binding its deferred
        // RPC); the menu closes itself when a leaf fires or Esc is pressed at the
        // root. Intercepted ahead of every screen route so it never leaks a key to
        // the board beneath it.
        if self.context_menu.is_some() {
            self.route_context_menu_key(key);
            return;
        }

        // 63l.6: the generic list-screen context menu (Kanban / Autopilots /
        // Skills) is likewise modal while open — it captures every key.
        if self.list_context_menu.is_some() {
            self.route_list_context_menu_key(key);
            return;
        }

        // e38.36: while the daemon link is genuinely down (Disconnected/Error) the
        // body is the offline empty-state, not a real screen. `[s]` arms the
        // deferred daemon-start (drained in `render`, where host IO is safe). This
        // is intercepted BEFORE per-screen routing so it never shadows an online
        // `s` binding (the Settings/Skills `s` keys are only reachable online); a
        // held-key repeat is already filtered out by `handle_key`. Every other key
        // falls through so global nav (tab switches, `q`) still works while offline.
        if Self::is_offline(self.conn.state()) && key.code == (KeyCode::Char { ch: 's' }) {
            self.start_daemon_pending = true;
            return;
        }

        // Snapshot the routing state so the borrow on `self.app` is released
        // before we mutate `self.screens` and `self.app` below.
        let app = self.app_state().clone();

        // e38.29: while the issue list is capturing free text (the `/` filter or
        // the `c` create-title input), every key — including the global
        // tab-switch chars (`1`/`K`/`,`/`q`/…) — is typed text, NOT a nav key.
        // Route it straight to the screen reducer so a title like `q,K` types
        // instead of quitting / switching tabs. Esc aborts the create flow.
        if matches!(app.screen, Screen::IssueList) && self.screens.issue_list.is_capturing_text() {
            if matches!(key.code, KeyCode::Esc) {
                self.screens.issue_list.abort_create();
                return;
            }
            if let Some(nav) = route_key(&app, &mut self.screens, key) {
                self.apply_nav(&app, nav);
            }
            return;
        }

        // e38.13: `Ctrl+P` opens the global command palette from any non-modal
        // screen. It is a modifier chord, so it never shadows a per-screen `p`
        // (bare `p` still reaches the active reducer). When the palette is already
        // open the chord falls through to its reducer (where it is an unmodelled
        // no-op), so it never re-opens a fresh palette over itself.
        if !app.screen.is_modal() && is_ctrl_p(key) {
            let reduction = crate::screen::reduce(&app, AppEvent::OpenCommandPalette);
            self.screens.open_palette();
            self.app = Some(reduction.state);
            return;
        }

        // Routing-layer keys: tab switches, `?` help, Esc-close-modal, `q` quit.
        if let Some(ev) = routing_event(key, &app) {
            let reduction = crate::screen::reduce(&app, ev);
            self.app = Some(reduction.state);
            return;
        }

        // Per-screen keys → the active screen's reducer.
        if let Some(nav) = route_key(&app, &mut self.screens, key) {
            self.apply_nav(&app, nav);
        }
    }

    /// Route a key to the open context menu (63l.5), folding it into the menu
    /// reducer and applying any leaf intent it fires.
    ///
    /// The SDK arrow [`KeyCode`](ainb_plugin_sdk::KeyCode)s and the `h`/`j`/`k`/`l`
    /// vim chars map onto the menu's small [`ContextMenuKey`](crate::screen::context_menu::ContextMenuKey)
    /// alphabet; an unmapped key is swallowed (the menu is modal). When the
    /// reduction closes the menu (a fired leaf, or Esc at the root) the overlay is
    /// dropped so the board regains focus.
    fn route_context_menu_key(&mut self, key: &ainb_plugin_sdk::KeyEvent) {
        let Some(menu_key) = context_menu_key_of(key) else {
            return;
        };
        let Some(menu) = self.context_menu.as_mut() else {
            return;
        };
        if let Some(intent) = menu.handle_key(menu_key) {
            self.apply_context_menu_intent(intent);
        }
        // Drop the overlay once it closes itself (a leaf fired, or Esc at root).
        if self
            .context_menu
            .as_ref()
            .is_some_and(crate::screen::context_menu::ContextMenuState::is_closed)
        {
            self.context_menu = None;
        }
    }

    /// Fold one forwarded mouse event into the card-board mouse framework
    /// (63l.2): hit-test it against the last render's [`hit_map`](Self::hit_map),
    /// fold the [`MouseFsm`](crate::mouse::MouseFsm), and stash any produced
    /// [`MouseIntent`](crate::mouse::MouseIntent) for the next render to drain.
    ///
    /// Host-free + non-blocking (the SDK dispatches `handle_mouse` INLINE on the
    /// reader loop): it never touches a socket, only hit-tests + folds the FSM and
    /// stashes the produced intent. The spawned `render` drains it (a non-empty
    /// queue is the `wants_redraw` signal), applying the local effects and (in
    /// 63l.4) binding the mutating intents to RPCs.
    fn on_mouse(&mut self, ev: ainb_plugin_sdk::MouseEvent) {
        // 63l.5: while the issue context menu is open it owns the pointer.
        if self.context_menu.is_some() {
            self.route_context_menu_mouse(ev);
            return;
        }
        // 63l.6: while the list-screen context menu is open it owns the pointer.
        if self.list_context_menu.is_some() {
            self.route_list_context_menu_mouse(ev);
            return;
        }
        // 63l.6: the Kanban / Autopilots / Skills list screens fold the pointer
        // against the render-time card-board layout (the lifecycle-free
        // `fold_board_mouse` path), NOT the issue board's lifecycle FSM. A click
        // opens, a wheel scrolls, a move hovers, a right-click anchors a context
        // menu — no drag, no fabricated mutation.
        if let Some(layout) = self.board_layout.as_ref() {
            if let Some(intent) = crate::board_mouse::fold_board_mouse(layout, ev) {
                self.pending_board_mouse_intents.push(intent);
            }
            return;
        }
        // Otherwise the issue board's lifecycle-typed drag FSM.
        if let Some(intent) = self.mouse_fsm.handle(&self.hit_map, ev) {
            self.pending_mouse_intents.push(intent);
        }
    }

    /// Route a forwarded pointer event to the open context menu (63l.5).
    ///
    /// Only a `Down{Left}` acts (the conventional menu click): it folds into the
    /// menu's [`handle_click`](crate::screen::context_menu::ContextMenuState::handle_click)
    /// against the last render's hit-map, applying any fired leaf intent and
    /// dropping the overlay once it closes (a leaf fired, or a click-away dismiss).
    /// Other events while the menu is open are swallowed.
    fn route_context_menu_mouse(&mut self, ev: ainb_plugin_sdk::MouseEvent) {
        use ainb_plugin_sdk::{MouseButton, MouseKind};
        if !matches!(
            ev.kind,
            MouseKind::Down {
                button: MouseButton::Left
            }
        ) {
            return;
        }
        let Some(menu) = self.context_menu.as_mut() else {
            return;
        };
        if let Some(intent) = menu.handle_click(ev.col, ev.row) {
            self.apply_context_menu_intent(intent);
        }
        if self
            .context_menu
            .as_ref()
            .is_some_and(crate::screen::context_menu::ContextMenuState::is_closed)
        {
            self.context_menu = None;
        }
    }

    /// Rebuild the render-time hit-map (63l.2) for the active board screen.
    ///
    /// Today the issue list is the only board screen wired to the mouse layer; it
    /// flattens its visible rows into card-board columns
    /// ([`IssueListState::board_columns`](crate::screen::issue_list::IssueListState::board_columns)),
    /// renders them into a scratch [`WireBuffer`] purely to obtain the
    /// [`BoardLayout`](crate::widgets::card_board::BoardLayout) geometry, and
    /// builds the hit-map from it — so `handle_mouse` hit-tests against the SAME
    /// geometry the board paints, never re-derived. Non-board screens clear the
    /// map (a click there resolves to nothing). The visible board swap lands in
    /// 63l.4; this records the geometry so the mouse path is live now.
    fn rebuild_hit_map(&mut self, w: u16, h: u16) {
        use crate::mouse::HitMap;
        // Default: no board geometry recorded (a click resolves to nothing).
        self.hit_map = HitMap::default();
        self.board_layout = None;
        match self.app_state().screen {
            Screen::IssueList => {
                let columns = self.screens.issue_list.board_columns();
                // The issue body runs below the chip row (top + 1) to the footer
                // (h - 1), matching the issue-list body band.
                let mut scratch = WireBuffer::new(w, h);
                let top = 2u16.min(h);
                let bottom = h.saturating_sub(1).max(top);
                let layout = crate::widgets::card_board::render_card_board(
                    &mut scratch,
                    w,
                    top,
                    bottom,
                    &columns,
                    None,
                );
                // The issue board uses the lifecycle-typed FSM hit-map.
                self.hit_map = HitMap::from_board_layout(&layout);
            }
            // 63l.6: the Kanban / Autopilots / Skills list screens record the
            // card-board layout the lifecycle-free `fold_board_mouse` hit-tests
            // against. Each scratch render mirrors that screen's REAL body band so
            // the hit-test geometry is exactly what `render_body` paints.
            Screen::Kanban => {
                let mut scratch = WireBuffer::new(w, h);
                let columns = self.screens.kanban.board_columns(now_ms_clock());
                let (top, bottom) = (1u16, h.saturating_sub(1).max(1));
                self.board_layout = Some(crate::widgets::card_board::render_card_board(
                    &mut scratch,
                    w,
                    top,
                    bottom,
                    &columns,
                    None,
                ));
            }
            Screen::Autopilots => {
                if !self.screens.autopilots.autopilots().is_empty() {
                    let mut scratch = WireBuffer::new(w, h);
                    let columns = self.screens.autopilots.board_columns();
                    // The autopilots board occupies body_top (top+1) .. board_bottom
                    // (reserving 4 rows for the run-history pane), matching
                    // `render_autopilots`.
                    let body_top = 2u16.min(h);
                    let bottom = h.saturating_sub(1).max(body_top);
                    let avail = bottom.saturating_sub(body_top);
                    let board_bottom = body_top.saturating_add(avail.saturating_sub(4).max(1));
                    self.board_layout = Some(crate::widgets::card_board::render_card_board(
                        &mut scratch,
                        w,
                        body_top,
                        board_bottom,
                        &columns,
                        None,
                    ));
                }
            }
            Screen::SkillManager => {
                let mut scratch = WireBuffer::new(w, h);
                let columns = self.screens.skill_manager.board_columns();
                // The skill list pane is the left 28 cols, starting one row below
                // the chip bar (top + 1), matching `render_skill_manager`.
                let list_w = 28u16.min(w);
                let content_top = 2u16.min(h);
                let bottom = h.saturating_sub(1).max(content_top);
                self.board_layout = Some(crate::widgets::card_board::render_card_board(
                    &mut scratch,
                    list_w,
                    content_top,
                    bottom,
                    &columns,
                    None,
                ));
            }
            _ => {}
        }
    }

    /// Drain the mouse intents stashed by `handle_mouse` (63l.2/63l.4), binding
    /// each to its real board action so the Issues board is fully mouse-driven.
    ///
    /// - `Select` highlights the pressed card (local).
    /// - `ClickOpen` opens the clicked issue's task detail (reusing the keyboard
    ///   `enter` glue so the open flow is byte-identical).
    /// - `MoveCard` is a cross-column drag-drop: the card moves optimistically into
    ///   the destination column AND a `hangar/issue_update{state}` RPC is armed (the
    ///   SAME daemon seam the agent picker / keyboard path uses) so the move is
    ///   durable — the daemon's `IssueUpdated` push reconciles the optimistic move.
    /// - `ReorderCard` reseats the card within its column (local display order; see
    ///   [`IssueListState::reorder_within_column`] for why it is not a priority
    ///   rewrite).
    /// - `ScrollColumn` / `Hover` / `DragHover` update the local board state
    ///   (per-column scroll, hover highlight) so the next render reflects them.
    ///
    /// - `OpenContextMenu` raises the right-click context-menu overlay (63l.5)
    ///   anchored at the click, seeded with the issue's current state/priority + the
    ///   cached actor snapshot.
    ///
    /// The remaining intents (`NewIssue`, `FocusColumn`, `PanColumns`, `SwitchTab`)
    /// land in later board sub-beads (the seeded create flow); they are consumed
    /// here so they never accumulate.
    fn drain_mouse_intents(&mut self) {
        use crate::mouse::MouseIntent;
        use crate::screen::issue_list::IssueColumn;
        let app = self.app_state().clone();
        let intents = std::mem::take(&mut self.pending_mouse_intents);
        for intent in intents {
            match intent {
                // A press highlights the hit card immediately (local, no RPC).
                MouseIntent::Select(id) => self.screens.issue_list.select_by_id(&id),
                // A click opens the clicked issue's task detail.
                MouseIntent::ClickOpen(id) => {
                    self.screens.issue_list.select_by_id(&id);
                    if let Ok(issue_id) = ainb_hangar_core::ids::IssueId::from_str(&id) {
                        self.apply_nav(&app, NavIntent::OpenTaskForIssue(issue_id));
                    }
                }
                // A cross-column drag-drop: move the card optimistically and arm
                // the durable `hangar/issue_update{state}` RPC for the render drain.
                MouseIntent::MoveCard {
                    issue_id,
                    to_status,
                } => {
                    if let Some(moved) = self.screens.issue_list.move_issue_to(&issue_id, to_status)
                    {
                        self.pending_issue_state_update = Some((moved, to_status));
                    }
                }
                // A same-column drag: reseat the card locally (display order only).
                MouseIntent::ReorderCard { issue_id, to_index } => {
                    self.screens
                        .issue_list
                        .reorder_within_column(&issue_id, to_index);
                }
                // A wheel-scroll over a column nudges that column's scroll offset.
                MouseIntent::ScrollColumn { status, delta } => {
                    self.screens
                        .issue_list
                        .scroll_column(IssueColumn::from_lifecycle(status), delta);
                }
                // Hover (no button) highlights the card under the pointer; a live
                // drag hover highlights the card being dragged.
                MouseIntent::Hover(id) => self.screens.issue_list.set_hover(id),
                MouseIntent::DragHover { card, .. } => {
                    self.screens.issue_list.set_hover(Some(card));
                }
                // A right-click on a card raises the context-menu overlay (63l.5)
                // anchored at the click, seeded with the issue's current
                // state/priority + the cached actor snapshot for the Assign submenu.
                MouseIntent::OpenContextMenu { issue_id, at } => {
                    self.open_context_menu(&issue_id, at);
                }
                // The remaining intents land in later board sub-beads.
                MouseIntent::NewIssue(_)
                | MouseIntent::FocusColumn(_)
                | MouseIntent::PanColumns { .. }
                | MouseIntent::SwitchTab(_) => {}
            }
        }
    }

    /// Drain the list-screen mouse intents stashed by `handle_mouse` (63l.6),
    /// binding each to the active screen's EXISTING action so the Kanban /
    /// Autopilots / Skills boards are fully mouse-driven.
    ///
    /// - `ClickOpen` opens the clicked card's detail (Kanban task detail) or
    ///   selects the clicked card (Autopilots → loads its run history, Skills →
    ///   loads its detail) — the same effect the keyboard select produces.
    /// - `ScrollColumn` / `Hover` update the local board state (per-column scroll,
    ///   hover highlight).
    /// - `OpenContextMenu` raises the generic list context-menu overlay anchored
    ///   at the click, whose leaf binds to the screen's real RPC.
    ///
    /// Each binding maps onto a pending-action the `render` pass already drains
    /// and fires (no new wire method); a right-click `Run now` on a Kanban card,
    /// for instance, arms the existing `hangar/task_transition` seam.
    fn drain_board_mouse_intents(&mut self) {
        use crate::board_mouse::BoardMouseIntent;
        let screen = self.app_state().screen.clone();
        let intents = std::mem::take(&mut self.pending_board_mouse_intents);
        for intent in intents {
            match (&screen, intent) {
                // ---- Kanban ----
                (Screen::Kanban, BoardMouseIntent::ClickOpen(task_id)) => {
                    self.open_kanban_task(&task_id);
                }
                (Screen::Kanban, BoardMouseIntent::ScrollColumn { column, delta }) => {
                    self.screens.kanban.scroll_column(column, delta);
                }
                (Screen::Kanban, BoardMouseIntent::Hover(id)) => {
                    self.screens.kanban.set_hover(id);
                }
                (Screen::Kanban, BoardMouseIntent::OpenContextMenu { id, at }) => {
                    self.open_kanban_context_menu(&id, at);
                }
                // ---- Autopilots ----
                (Screen::Autopilots, BoardMouseIntent::ClickOpen(id)) => {
                    // A click selects the autopilot AND loads its run history
                    // (`hangar/autopilot_runs`), the same effect a keyboard select
                    // produces — the "open detail" for an autopilot is its history.
                    self.screens.autopilots.select_by_id(&id);
                    self.screens.pending_autopilot_action =
                        Some(crate::screen::AutopilotAction::LoadRuns(id));
                }
                (Screen::Autopilots, BoardMouseIntent::ScrollColumn { delta, .. }) => {
                    self.screens.autopilots.scroll(delta);
                }
                (Screen::Autopilots, BoardMouseIntent::Hover(id)) => {
                    self.screens.autopilots.set_hover(id);
                }
                (Screen::Autopilots, BoardMouseIntent::OpenContextMenu { id, at }) => {
                    self.open_autopilot_context_menu(&id, at);
                }
                // ---- Skills ----
                (Screen::SkillManager, BoardMouseIntent::ClickOpen(slug)) => {
                    // A click selects the skill AND opens its detail
                    // (`hangar/skill_get`), the same effect Enter produces.
                    self.screens.skill_manager.select_by_slug(&slug);
                    self.screens.pending_skill_action =
                        Some(crate::screen::SkillAction::LoadDetail(slug));
                }
                (Screen::SkillManager, BoardMouseIntent::ScrollColumn { delta, .. }) => {
                    self.screens.skill_manager.scroll(delta);
                }
                (Screen::SkillManager, BoardMouseIntent::Hover(slug)) => {
                    self.screens.skill_manager.set_hover(slug);
                }
                (Screen::SkillManager, BoardMouseIntent::OpenContextMenu { id, at }) => {
                    self.open_skill_context_menu(&id, at);
                }
                // A stale intent for a since-changed screen is dropped.
                _ => {}
            }
        }
    }

    /// Open the task detail (63l.6 Kanban click) for the clicked task, focusing
    /// the clicked card first so the selection lands on it. Builds the task-detail
    /// screen from the clicked card's known fields (a synthesized issue header) so
    /// a click opens the task's transcript exactly as the issue board's click-open
    /// path does. A no-op when no card carries the id (a stale hit-map entry).
    fn open_kanban_task(&mut self, task_id: &str) {
        let Some(card) = self.screens.kanban.card_for_task(task_id).cloned() else {
            return;
        };
        self.screens.kanban.focus_task(task_id);
        let Ok(tid) = ainb_hangar_core::ids::TaskId::from_str(task_id) else {
            return;
        };
        // Synthesize a minimal issue header from the card so the task-detail screen
        // renders the task's title + status; the streaming transcript folds events
        // addressed to this task id, exactly as the issue board's open does.
        let issue = ainb_hangar_proto::events::IssueRow {
            id: ainb_hangar_core::ids::IssueId::from_str(format!("task-{task_id}")).unwrap_or_else(
                |_| ainb_hangar_core::ids::IssueId::from_str("task").expect("non-empty"),
            ),
            display_id: Some(format!("#{}", card.short_id)),
            workspace_id: self.app_state().ws_id.as_str().to_string(),
            title: format!("Task {}", card.short_id),
            description: None,
            state: card.status.clone(),
            assignee: Some(format!("agent:{}", card.agent_id)),
            creator: "member:me".to_string(),
            created_at: card.created_at,
            priority: 0,
            due_date: None,
            labels: Vec::new(),
            pr_url: None,
        };
        self.screens.open_task_detail(tid.clone(), issue);
        let mut next = self.app_state().clone();
        next.selected_task = Some(tid.clone());
        next.prior_screen = None;
        next.screen = Screen::TaskDetail(tid);
        self.app = Some(next);
    }

    /// Raise the generic list context-menu for a Kanban task (63l.6): the task's
    /// real actions (Open / Run now / Cancel / Copy id) bound to the EXISTING
    /// `hangar/task_transition` seam. Run-now transitions a queued task to
    /// `running`; Cancel transitions it to `cancelled` — real mutations, no new
    /// wire method. A no-op when no card carries the id (a stale hit-map entry).
    fn open_kanban_context_menu(&mut self, task_id: &str, at: (u16, u16)) {
        use crate::screen::list_context_menu::{
            ListContextMenuState, ListMenuAction, ListMenuItem,
        };
        let Some(card) = self.screens.kanban.card_for_task(task_id).cloned() else {
            return;
        };
        let id = task_id.to_string();
        let items = vec![
            ListMenuItem::new("Open", ListMenuAction::Open(id.clone())),
            ListMenuItem::new("Run now", ListMenuAction::RunNow(id.clone())),
            ListMenuItem::new("Cancel", ListMenuAction::Cancel(id.clone())),
            ListMenuItem::new(
                "Copy id",
                ListMenuAction::CopyId(format!("#{}", card.short_id)),
            ),
        ];
        self.list_context_menu = Some(ListContextMenuState::new(
            format!("#{}", card.short_id),
            at,
            items,
        ));
    }

    /// Raise the generic list context-menu for an autopilot (63l.6): its real
    /// actions (Open / Run now / Enable-or-Disable / Edit / Copy id) bound to the
    /// EXISTING autopilot RPC seam (`hangar/autopilot_fire_now`,
    /// `hangar/autopilot_set_enabled`). A no-op for an unknown id.
    fn open_autopilot_context_menu(&mut self, autopilot_id: &str, at: (u16, u16)) {
        use crate::screen::list_context_menu::{
            ListContextMenuState, ListMenuAction, ListMenuItem,
        };
        let Some(ap) = self
            .screens
            .autopilots
            .autopilots()
            .iter()
            .find(|a| a.id == autopilot_id)
            .cloned()
        else {
            return;
        };
        let id = autopilot_id.to_string();
        let toggle_label = if ap.enabled { "Disable" } else { "Enable" };
        let items = vec![
            ListMenuItem::new("Open", ListMenuAction::Open(id.clone())),
            ListMenuItem::new("Run now", ListMenuAction::RunNow(id.clone())),
            ListMenuItem::new(
                toggle_label,
                ListMenuAction::SetEnabled {
                    id: id.clone(),
                    enabled: !ap.enabled,
                },
            ),
            ListMenuItem::new("Edit", ListMenuAction::Edit(id.clone())),
            ListMenuItem::new("Copy id", ListMenuAction::CopyId(ap.name.clone())),
        ];
        self.list_context_menu = Some(ListContextMenuState::new(ap.name.clone(), at, items));
    }

    /// Raise the generic list context-menu for a skill (63l.6): its read actions
    /// (Open / Copy id). Skills have no per-card mutating action distinct from the
    /// global sync / agent-scoped attach-detach, so the menu offers the read
    /// actions only (the bead's "no invented mutation" path). A no-op for an
    /// unknown slug.
    fn open_skill_context_menu(&mut self, slug: &str, at: (u16, u16)) {
        use crate::screen::list_context_menu::{
            ListContextMenuState, ListMenuAction, ListMenuItem,
        };
        let Some(skill) = self
            .screens
            .skill_manager
            .visible_skills()
            .into_iter()
            .find(|s| s.slug == slug)
        else {
            return;
        };
        let items = vec![
            ListMenuItem::new("Open", ListMenuAction::Open(slug.to_string())),
            ListMenuItem::new("Copy id", ListMenuAction::CopyId(skill.slug.clone())),
        ];
        self.list_context_menu = Some(ListContextMenuState::new(skill.name.clone(), at, items));
    }

    /// Apply a list context-menu leaf action (63l.6): bind it to the active
    /// screen's EXISTING deferred RPC (fired in `render`) or the local open path.
    /// Each variant maps onto a pending-action the render pass already drains and
    /// fires — there is no new wire method.
    fn apply_list_menu_action(&mut self, action: crate::screen::list_context_menu::ListMenuAction) {
        use crate::screen::list_context_menu::ListMenuAction;
        use crate::screen::{AutopilotAction, KanbanAction, SkillAction};
        let screen = self.app_state().screen.clone();
        match (&screen, action) {
            // ---- Kanban: Open / Run now / Cancel / Copy id ----
            (Screen::Kanban, ListMenuAction::Open(task_id)) => self.open_kanban_task(&task_id),
            (Screen::Kanban, ListMenuAction::RunNow(task_id)) => {
                // Run a queued task now = transition it to `running` over the
                // EXISTING `hangar/task_transition` seam.
                self.screens.pending_kanban_action = Some(KanbanAction::MoveCard {
                    task_id,
                    to_status: ainb_hangar_core::task_status::TaskStatus::Running
                        .as_str()
                        .to_string(),
                });
            }
            (Screen::Kanban, ListMenuAction::Cancel(task_id)) => {
                self.screens.pending_kanban_action = Some(KanbanAction::MoveCard {
                    task_id,
                    to_status: ainb_hangar_core::task_status::TaskStatus::Cancelled
                        .as_str()
                        .to_string(),
                });
            }
            // ---- Autopilots: Open / Run now / Enable-Disable / Edit / Copy id ----
            (Screen::Autopilots, ListMenuAction::Open(id)) => {
                self.screens.autopilots.select_by_id(&id);
                self.screens.pending_autopilot_action = Some(AutopilotAction::LoadRuns(id));
            }
            (Screen::Autopilots, ListMenuAction::RunNow(id)) => {
                self.screens.pending_autopilot_action = Some(AutopilotAction::FireNow(id));
            }
            (Screen::Autopilots, ListMenuAction::SetEnabled { id, enabled }) => {
                self.screens.pending_autopilot_action = Some(AutopilotAction::SetEnabled {
                    autopilot_id: id,
                    enabled,
                });
            }
            (Screen::Autopilots, ListMenuAction::Edit(_id)) => {
                // Edit opens the create/edit flow (no RPC at this layer yet);
                // selecting it is the user-visible effect.
            }
            // ---- Skills: Open ----
            (Screen::SkillManager, ListMenuAction::Open(slug)) => {
                self.screens.skill_manager.select_by_slug(&slug);
                self.screens.pending_skill_action = Some(SkillAction::LoadDetail(slug));
            }
            // Copy id has no host clipboard cap yet; the menu's own `copied` note is
            // the user-visible effect. Log the copy seam so it is observable.
            (_, ListMenuAction::CopyId(display_id)) => {
                tracing::info!(%display_id, "hangar: list context-menu copy id");
            }
            // A stale action for a since-changed screen is dropped.
            _ => {}
        }
    }

    /// Route a key to the open list context menu (63l.6): fold it into the menu
    /// reducer and apply any leaf action it fires, dropping the overlay when it
    /// closes (a leaf fired, or Esc).
    fn route_list_context_menu_key(&mut self, key: &ainb_plugin_sdk::KeyEvent) {
        use crate::screen::list_context_menu::ListMenuKey;
        let menu_key = match &key.code {
            KeyCode::Up => ListMenuKey::Up,
            KeyCode::Down => ListMenuKey::Down,
            KeyCode::Right | KeyCode::Enter => ListMenuKey::Enter,
            KeyCode::Left | KeyCode::Esc => ListMenuKey::Esc,
            KeyCode::Char { ch } => match ch {
                'k' => ListMenuKey::Up,
                'j' => ListMenuKey::Down,
                'l' => ListMenuKey::Enter,
                'h' => ListMenuKey::Esc,
                _ => return,
            },
            _ => return,
        };
        let Some(menu) = self.list_context_menu.as_mut() else {
            return;
        };
        if let Some(action) = menu.handle_key(menu_key) {
            self.apply_list_menu_action(action);
        }
        if self
            .list_context_menu
            .as_ref()
            .is_some_and(crate::screen::list_context_menu::ListContextMenuState::is_closed)
        {
            self.list_context_menu = None;
        }
    }

    /// Route a forwarded pointer event to the open list context menu (63l.6): a
    /// `Down{Left}` folds into its `handle_click`, firing a leaf / dismissing on a
    /// click-away; other events are swallowed.
    fn route_list_context_menu_mouse(&mut self, ev: ainb_plugin_sdk::MouseEvent) {
        use ainb_plugin_sdk::{MouseButton, MouseKind};
        if !matches!(
            ev.kind,
            MouseKind::Down {
                button: MouseButton::Left
            }
        ) {
            return;
        }
        let Some(menu) = self.list_context_menu.as_mut() else {
            return;
        };
        if let Some(action) = menu.handle_click(ev.col, ev.row) {
            self.apply_list_menu_action(action);
        }
        if self
            .list_context_menu
            .as_ref()
            .is_some_and(crate::screen::list_context_menu::ListContextMenuState::is_closed)
        {
            self.list_context_menu = None;
        }
    }

    /// Raise the context-menu overlay for `issue_id` anchored at `at` (63l.5).
    ///
    /// Looks the issue up in the cached issue list to seed the menu with its
    /// `HGR-<n>` display id and current lifecycle state + priority (so the
    /// submenus mark what is set), and copies the cached actor snapshot into the
    /// Assign submenu. A right-click on an id with no cached row is a no-op (a
    /// stale hit-map entry) rather than opening an empty menu.
    fn open_context_menu(&mut self, issue_id: &str, at: (u16, u16)) {
        use crate::screen::context_menu::{ContextMenuState, MenuActor};
        let Some(row) = self
            .screens
            .issue_list
            .visible_rows()
            .find(|r| r.id.as_str() == issue_id)
            .cloned()
        else {
            return;
        };
        let display_id = row
            .display_id
            .clone()
            .unwrap_or_else(|| row.id.as_str().to_string());
        let status = ainb_hangar_proto::lifecycle::IssueLifecycle::for_state(&row.state);
        let actors: Vec<MenuActor> = self
            .screens
            .actors
            .iter()
            .map(|a| MenuActor {
                actor_ref: a.actor_ref.clone(),
                display_name: a.display_name.clone(),
            })
            .collect();
        self.context_menu = Some(ContextMenuState::new(
            row.id.as_str().to_string(),
            display_id,
            status,
            row.priority,
            at,
            actors,
        ));
    }

    /// Apply a context-menu leaf intent (63l.5): bind it to the matching deferred
    /// daemon RPC (fired in `render`) or the local task-open path, then close the
    /// menu unless the leaf (Copy id) keeps it open for its confirmation note.
    fn apply_context_menu_intent(
        &mut self,
        intent: crate::screen::context_menu::ContextMenuIntent,
    ) {
        use crate::screen::context_menu::ContextMenuIntent;
        let app = self.app_state().clone();
        match intent {
            ContextMenuIntent::Open { issue_id } => {
                self.screens.issue_list.select_by_id(&issue_id);
                if let Ok(id) = ainb_hangar_core::ids::IssueId::from_str(&issue_id) {
                    self.apply_nav(&app, NavIntent::OpenTaskForIssue(id));
                }
            }
            ContextMenuIntent::MoveTo {
                issue_id,
                to_status,
            } => {
                // Move the card optimistically (so the board reflects it at once)
                // AND arm the durable `issue_update{state}` RPC — the same seam the
                // drag-drop path uses.
                if let Some(moved) = self.screens.issue_list.move_issue_to(&issue_id, to_status) {
                    self.pending_issue_state_update = Some((moved, to_status));
                }
            }
            ContextMenuIntent::SetPriority { issue_id, priority } => {
                self.pending_issue_priority_update = Some((issue_id, priority));
            }
            ContextMenuIntent::Assign {
                issue_id,
                actor_ref,
            } => {
                self.pending_issue_assignee_update = Some((issue_id, actor_ref));
            }
            // Copy id has no host clipboard cap yet; the menu's own `copied` note is
            // the user-visible effect. Log the copy seam so it is observable.
            ContextMenuIntent::CopyId { display_id } => {
                tracing::info!(%display_id, "hangar: context-menu copy id");
            }
        }
    }

    /// Act on a cross-screen [`NavIntent`] surfaced by a screen reducer: open the
    /// modal/screen on the routing state and build its cache.
    fn apply_nav(&mut self, app: &AppState, nav: NavIntent) {
        match nav {
            NavIntent::OpenAgentPicker(issue_id) => {
                self.screens.open_picker(issue_id.clone());
                let reduction = crate::screen::reduce(app, AppEvent::OpenAgentPicker(issue_id));
                self.app = Some(reduction.state);
            }
            NavIntent::OpenTaskForIssue(issue_id) => {
                // Open task detail bound to the issue's row + the running task.
                let issue = self
                    .screens
                    .issue_list
                    .visible_rows()
                    .find(|r| r.id == issue_id)
                    .cloned();
                if let Some(issue) = issue {
                    // A synthetic task id keyed off the issue — the daemon binds
                    // the real running task to the issue, and the task-detail
                    // transcript folds events addressed to it.
                    let task_id = ainb_hangar_core::ids::TaskId::from_str(format!(
                        "task-{}",
                        issue_id.as_str()
                    ))
                    .unwrap_or_else(|_| {
                        ainb_hangar_core::ids::TaskId::from_str("task").expect("non-empty")
                    });
                    // e38.34: a task-detail screen on an issue with a captured PR
                    // arms a `hangar/pr_status_refresh` so the badge surfaces the
                    // CI + merge status (and a merged PR auto-moves to Done). The
                    // socket send can't run inline here, so it is deferred to
                    // `render`. No PR → no refresh (the badge stays absent).
                    if issue.pr_url.is_some() {
                        self.pending_pr_status_refresh = Some(issue.id.as_str().to_string());
                    }
                    self.screens.open_task_detail(task_id.clone(), issue);
                    let mut next = app.clone();
                    next.screen = Screen::TaskDetail(task_id.clone());
                    next.selected_task = Some(task_id);
                    next.prior_screen = None;
                    self.app = Some(next);
                }
            }
            NavIntent::OpenPrUrl(url) => {
                // Open the captured PR URL in the host browser (P9.2). The
                // routing layer only raises this when the task has a `pr_url`, so
                // there is no silent open of nothing. A launch failure is logged
                // to the daemon-link footer log rather than crashing the plugin.
                if let Err(e) = self.opener.open(&url) {
                    tracing::warn!(%url, error = %e, "hangar: failed to open PR url");
                }
            }
            NavIntent::CloseModal => {
                let reduction = crate::screen::reduce(app, AppEvent::Esc);
                self.app = Some(reduction.state);
            }
            NavIntent::NavigateToEntity { screen, id, kind } => {
                self.navigate_to_entity(app, &screen, &id, &kind);
            }
        }
    }

    /// Jump to the entity the command palette selected (e38.13): switch the
    /// routing screen to the entity's target and, where the screen supports it,
    /// select the matching row.
    ///
    /// The `screen` token is the wire value the daemon carried on the
    /// [`SearchEntry`](ainb_hangar_proto::snapshots::SearchEntry) (derived from the
    /// entity kind). An unknown token is ignored (the palette closes without a
    /// jump rather than panicking). Issue + agent entries land on the issue list;
    /// the issue-list cache is asked to select the matching issue id so the jump
    /// lands ON the row, not merely on the screen.
    fn navigate_to_entity(&mut self, app: &AppState, screen: &str, id: &str, kind: &str) {
        let target = match screen {
            "issue_list" => Screen::IssueList,
            "skill_manager" => Screen::SkillManager,
            "autopilots" => Screen::Autopilots,
            // An unrecognised token: close the palette, don't jump anywhere.
            _ => {
                let reduction = crate::screen::reduce(app, AppEvent::Esc);
                self.app = Some(reduction.state);
                return;
            }
        };
        // Select the matching row where the target screen supports it. Issues
        // (and agents, which land on the issue list) select the issue id.
        if matches!(target, Screen::IssueList) && kind == "issue" {
            self.screens.issue_list.select_by_id(id);
        }
        let mut next = app.clone();
        next.screen = target;
        next.prior_screen = None;
        self.app = Some(next);
    }
}

/// Map a wire key to the open context menu's small navigation alphabet (63l.5),
/// or `None` for a key the modal menu swallows.
///
/// Both the SDK arrow [`KeyCode`](ainb_plugin_sdk::KeyCode)s and the vim
/// `h`/`j`/`k`/`l` chars map onto the same directions, so the menu navigates the
/// same way regardless of how the user reaches for it. `Enter` fires/opens and
/// `Esc` collapses-or-closes.
const fn context_menu_key_of(
    key: &ainb_plugin_sdk::KeyEvent,
) -> Option<crate::screen::context_menu::ContextMenuKey> {
    use crate::screen::context_menu::ContextMenuKey;
    match &key.code {
        KeyCode::Up => Some(ContextMenuKey::Up),
        KeyCode::Down => Some(ContextMenuKey::Down),
        KeyCode::Right => Some(ContextMenuKey::Right),
        KeyCode::Left => Some(ContextMenuKey::Left),
        KeyCode::Enter => Some(ContextMenuKey::Enter),
        KeyCode::Esc => Some(ContextMenuKey::Esc),
        KeyCode::Char { ch } => match ch {
            'k' => Some(ContextMenuKey::Up),
            'j' => Some(ContextMenuKey::Down),
            'l' => Some(ContextMenuKey::Right),
            'h' => Some(ContextMenuKey::Left),
            _ => None,
        },
        _ => None,
    }
}

/// Whether `key` is the `Ctrl+P` command-palette chord (e38.13).
///
/// Matches `p`/`P` with the Ctrl modifier set. Some terminals deliver `Ctrl+P` as
/// the control character `\u{10}` (DLE) with no modifier flag, so that codepoint
/// is also accepted — both spellings open the palette.
const fn is_ctrl_p(key: &ainb_plugin_sdk::KeyEvent) -> bool {
    let ctrl = key.mods & ainb_plugin_sdk::KEY_MOD_CTRL != 0;
    match &key.code {
        KeyCode::Char { ch } if ctrl && (*ch == 'p' || *ch == 'P') => true,
        KeyCode::Char { ch } => *ch == '\u{10}',
        _ => false,
    }
}

/// Map a wire key to a routing-layer [`AppEvent`] when it is a tab-switch / help
/// / quit / Esc key, else `None` (the key belongs to the active screen reducer).
///
/// On a modal screen Esc closes the modal (handled by the screen router); on a
/// non-modal screen the per-screen reducer may want Esc, so it falls through.
const fn routing_event(key: &ainb_plugin_sdk::KeyEvent, app: &AppState) -> Option<AppEvent> {
    match &key.code {
        KeyCode::Char { ch }
            // `3`/`4` are the renumbered Skills/Autopilots tab keys after the old
            // `[3]Agents` tab folded into the issue-list filter chip (e38.38); the
            // numbered tabs are now contiguous `1`→`4`.
            if matches!(
                *ch,
                '1' | '2' | '3' | '4' | 'K' | 'D' | 'L' | ',' | '?' | 'q'
            ) =>
        {
            Some(AppEvent::Key(*ch))
        }
        // Esc routes through the router to close most modals (agent picker, help).
        // The command palette is excluded: it owns a per-modal cache that must be
        // cleared on close, so its Esc falls through to `route_command_palette`
        // (which dismisses the cache AND raises `CloseModal`). Letting the router
        // pop the screen here would restore the prior screen but leak the palette
        // cache (a stale modal lingering until the next open).
        KeyCode::Esc if app.screen.is_modal() && !matches!(app.screen, Screen::CommandPalette) => {
            Some(AppEvent::Esc)
        }
        _ => None,
    }
}

#[async_trait]
impl Plugin for HangarPlugin {
    fn manifest(&self) -> &'static str {
        MANIFEST_TOML
    }

    async fn on_init(&mut self, host: &HostClient, _ctx: InitContext<'_>) -> Result<()> {
        // P5.6: decide whether to show the first-run danger-full-access modal
        // from the recorded acks in `~/.agents-in-a-box/hangar/state.toml`. A missing file
        // (fresh machine) → no acks → the modal shows once.
        self.first_run = firstrun::state_path().map_or(FirstRunModal::Dismissed, |p| {
            FirstRunModal::from_acks(&firstrun::read_acks(&p))
        });
        self.connect(host).await;
        Ok(())
    }

    async fn handle_event(&mut self, host: &HostClient, params: HandleEventParams) -> Result<()> {
        // Only socket:<stream_id> deliveries for our current stream concern us.
        let want = self.conn.stream_id().map(|id| format!("socket:{id}"));
        if want.as_deref() != Some(params.topic.as_str()) {
            return Ok(());
        }
        match serde_json::from_slice::<UnixSocketEvent>(&params.payload) {
            Ok(event) => self.on_socket_event(&event),
            Err(e) => self.conn.on_error(format!("bad socket event: {e}")),
        }
        // The subscribe ack (decoded above) arms the snapshot fetch; fire it now
        // that we hold the host. One fetch per subscribe.
        if std::mem::take(&mut self.fetch_pending) {
            self.fetch_snapshots(host).await;
        }
        Ok(())
    }

    async fn handle_key(&mut self, _host: &HostClient, params: HandleKeyParams) -> Result<()> {
        // Only initial presses drive the reducers; ignore auto-repeat/release so a
        // held key doesn't multi-fire a tab switch.
        if matches!(params.key.kind, ainb_plugin_sdk::KeyKind::Release) {
            return Ok(());
        }
        self.on_key(&params.key);
        // A Workspace-pane action (s/d/Refresh) may have been raised. We must NOT
        // perform the host-cap call here: `plugin/handle_key` runs INLINE on the
        // SDK reader loop, so awaiting a host request whose response arrives on
        // that same loop would deadlock. The deferred action is drained in
        // `render` instead (a spawned handler where the reader is free).
        Ok(())
    }

    async fn handle_mouse(
        &mut self,
        _host: &HostClient,
        params: ainb_plugin_sdk::HandleMouseParams,
    ) -> Result<()> {
        // 63l.2: fold the forwarded pointer event into the card-board mouse
        // framework. Like `handle_key`, this runs INLINE on the SDK reader loop,
        // so it MUST stay non-blocking: it only hit-tests against the last
        // render's hit-map, folds the FSM, and stashes any intent. The deferred
        // open-click / mutating intents drain in `render` (the spawned task where
        // host IO is safe), exactly as the key-path actions do.
        self.on_mouse(params.mouse);
        Ok(())
    }

    fn wants_redraw(&self) -> bool {
        // 63l.2: a mouse gesture that produced an intent arms a redraw so the next
        // frame reflects it (selection move, drag highlight, an opened task). The
        // pending-intent queue IS the signal; it is emptied once `render` drains
        // the stashed intents.
        //
        // 63l.5: an open context menu also wants every frame so its keyboard /
        // mouse navigation (selection move, submenu open) repaints, and an armed
        // context-menu RPC (priority / assignee edit) needs a render to fire.
        //
        // 63l.6: the list-screen mouse intents + the generic list context menu
        // arm the same redraw so the Kanban / Autopilots / Skills boards repaint
        // after a click / scroll / hover / menu navigation.
        !self.pending_mouse_intents.is_empty()
            || self.context_menu.is_some()
            || self.pending_issue_priority_update.is_some()
            || self.pending_issue_assignee_update.is_some()
            || !self.pending_board_mouse_intents.is_empty()
            || self.list_context_menu.is_some()
    }

    async fn render(&mut self, host: &HostClient, params: RenderParams) -> Result<WireBuffer> {
        // Drain any deferred Workspace-pane action here: `plugin/render` is
        // dispatched on a SPAWNED task (unlike the inline `handle_key`/
        // `handle_event`), so the SDK reader loop stays free to deliver the
        // host-cap response — awaiting a host request here can't deadlock.
        if let Some(action) = self.screens.take_pending_ws_action() {
            self.apply_workspace_action(host, action).await;
        }
        // P6.5: drain any deferred skill RPC (sync / detail / attach / detach)
        // raised by the skill-manager screen and fire it over the daemon socket.
        if let Some(action) = self.screens.take_pending_skill_action() {
            self.apply_skill_action(host, action).await;
        }
        // P7.5: drain any deferred autopilot RPC (load-runs / fire-now /
        // set-enabled) raised by the autopilot-manager screen.
        if let Some(action) = self.screens.take_pending_autopilot_action() {
            self.apply_autopilot_action(host, action).await;
        }
        // P8.4: drain any deferred Kanban card-move (`Shift+←/→`) raised by the
        // board and fire `hangar/task_transition` over the daemon socket.
        if let Some(action) = self.screens.take_pending_kanban_action() {
            self.apply_kanban_action(host, action).await;
        }
        // e38.8: drain any deferred issue-assign (Enter in the agent picker)
        // raised by the modal and fire `hangar/issue_update` over the daemon
        // socket to set the issue's assignee.
        if let Some(action) = self.screens.take_pending_assign_action() {
            self.apply_assign_action(host, action).await;
        }
        // e38.5: drain any deferred issue-comment (Enter in the task-detail
        // compose modal) and fire `hangar/comment_add` over the daemon socket.
        if let Some(action) = self.screens.take_pending_comment_action() {
            self.apply_comment_action(host, action).await;
        }
        // e38.29: drain any deferred issue-create (Enter on a non-blank title in
        // the issue-list inline create flow) and fire `hangar/issue_create` over
        // the daemon socket.
        if let Some(action) = self.screens.take_pending_create_action() {
            self.apply_create_action(host, action).await;
        }
        // e38.13: drain any deferred command-palette search (every keystroke in
        // the palette) and fire `hangar/search` over the daemon socket; the read
        // reply folds the ranked entries back into the open palette.
        if let Some(crate::screen::PaletteAction::Search(query)) =
            self.screens.take_pending_palette_action()
        {
            self.run_palette_search(host, query).await;
        }
        // e38.34: drain a deferred PR-status refresh (armed when a task-detail
        // screen with a bound PR opened) and fire `hangar/pr_status_refresh`. The
        // reply folds the CI + merge status onto the badge; a merged PR is
        // auto-moved to Done daemon-side (announced via `IssueUpdated`).
        if let Some(issue_id) = self.pending_pr_status_refresh.take() {
            self.fire_pr_status_refresh(host, issue_id).await;
        }
        // P8.6: the logs pane reads the daemon's structured-log file directly
        // (not a daemon RPC). Re-read on every render while it is the active
        // screen so live events surface, and on a pending level-filter change.
        let on_logs = matches!(self.app_state().screen, Screen::Logs);
        if on_logs || self.screens.take_pending_logs_refresh() {
            self.refresh_logs();
        }
        // e38.14: drain a deferred inbox mark-all-read (`r` on the inbox) and fire
        // `hangar/inbox_mark_read` over the daemon socket. The mutating-RPC reply
        // re-fetches the snapshots, so the unread badge drops to zero next render.
        if self.screens.take_pending_inbox_mark_read() {
            self.mark_inbox_read(host).await;
        }
        // e38.36: drain a deferred offline `[s]` daemon-start (armed in
        // `handle_key`). `render` runs on a spawned task, so the host-shell start
        // + re-dial handshake (awaiting host caps) can't deadlock the reader loop.
        if std::mem::take(&mut self.start_daemon_pending) {
            self.start_daemon_and_redial(host).await;
        }
        // P5.6: persist the first-run ack here (deferred from `handle_key`). The
        // modal is already `Dismissed` in state; this records it so a relaunch
        // skips the warning. An IO fault is logged, not fatal.
        if std::mem::take(&mut self.first_run_ack_pending) {
            if let Some(path) = firstrun::state_path() {
                if let Err(e) = firstrun::ack_first_run(&path) {
                    let _ = host
                        .log_info(format!("hangar: first-run ack persist failed: {e}"))
                        .await;
                }
            }
        }
        let (w, h) = if params.viewport.width == 0 || params.viewport.height == 0 {
            FALLBACK_VIEWPORT
        } else {
            (params.viewport.width, params.viewport.height)
        };
        // 63l.2/63l.4: drain any mouse intents the inline `handle_mouse` stashed
        // (the open-click reuses the keyboard task-open path; a cross-column drag
        // moves the card optimistically AND arms the durable issue-state RPC). Then
        // fire that armed RPC over the daemon socket, and rebuild the render-time
        // hit-map for THIS frame's board geometry so the next pointer event
        // hit-tests against what we paint.
        self.drain_mouse_intents();
        // 63l.6: drain the Kanban / Autopilots / Skills list-screen mouse intents
        // (click-open / scroll / hover / context-menu) bound to each screen's
        // existing action; the deferred RPCs they arm fire in the drains below.
        self.drain_board_mouse_intents();
        if let Some((issue_id, to_status)) = self.pending_issue_state_update.take() {
            self.apply_issue_state_update(host, issue_id, to_status)
                .await;
        }
        // 63l.5: fire any context-menu priority / assignee edit armed by a leaf.
        // Both reuse the `hangar/issue_update` seam (the daemon's `IssueUpdated`
        // push reconciles the optimistic local state), mirroring the state-move RPC.
        if let Some((issue_id, priority)) = self.pending_issue_priority_update.take() {
            self.apply_issue_priority_update(host, issue_id, priority)
                .await;
        }
        if let Some((issue_id, actor_ref)) = self.pending_issue_assignee_update.take() {
            self.apply_issue_assignee_update(host, issue_id, actor_ref)
                .await;
        }
        self.rebuild_hit_map(w, h);
        Ok(self.compose_frame(w, h))
    }

    async fn cli_dispatch(
        &mut self,
        _host: &HostClient,
        namespace: &str,
        _argv: &[String],
    ) -> Result<CliOutput> {
        Err(RpcError::not_implemented(format!(
            "hangar CLI namespace `{namespace}` not implemented (scaffold; lands in P4)"
        ))
        .into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_plugin_protocol::manifest::{CapabilityGrant, Manifest};

    #[test]
    fn manifest_returns_canonical_toml() {
        let p = HangarPlugin::new();
        assert_eq!(p.manifest(), MANIFEST_TOML);
        assert!(p.manifest().contains("name = \"hangar-tui\""));
    }

    #[test]
    fn manifest_parses_and_round_trips_all_four_caps() {
        let m: Manifest = toml::from_str(MANIFEST_TOML).expect("manifest parses");
        assert_eq!(m.plugin.name, "hangar-tui");
        assert_eq!(m.plugin.abi_version, 2);

        assert_eq!(
            m.capabilities.event_stream_subscribe.allow_list().unwrap(),
            ["workspace:*", "stream:*", "managed:*", "socket:*"]
        );
        assert_eq!(
            m.capabilities
                .spawn_managed_subprocess
                .allow_list()
                .unwrap(),
            ["ainb-hangar-daemon"]
        );
        assert_eq!(
            m.capabilities.unix_socket_dial.allow_list().unwrap(),
            ["~/.agents-in-a-box/hangar.sock", "${XDG_RUNTIME_DIR}/ainb-hangar.sock"]
        );
        assert_eq!(
            m.capabilities.secrets_read.allow_list().unwrap(),
            ["anthropic_api_key", "openai_api_key"]
        );
        assert!(
            m.capabilities.workspace_write.is_granted(),
            "workspace:write must be granted for the Settings switch pane"
        );

        let s = toml::to_string(&m).expect("serialize");
        let back: Manifest = toml::from_str(&s).expect("re-parse");
        assert_eq!(m, back);
    }

    #[test]
    fn manifest_lifecycle_is_lazy_no_reap() {
        let m: Manifest = toml::from_str(MANIFEST_TOML).unwrap();
        assert_eq!(
            m.lifecycle.spawn,
            ainb_plugin_protocol::manifest::SpawnMode::Lazy
        );
        assert_eq!(m.lifecycle.idle_reap_secs, 0);
    }

    #[test]
    fn manifest_provides_hangar_surface() {
        let m: Manifest = toml::from_str(MANIFEST_TOML).unwrap();
        assert_eq!(m.provides.screens, ["hangar"]);
        assert_eq!(m.provides.commands, ["/hangar"]);
        assert_eq!(m.provides.cli_namespaces, ["hangar"]);
    }

    #[test]
    fn manifest_grants_event_bus_and_plugin_data() {
        let m: Manifest = toml::from_str(MANIFEST_TOML).unwrap();
        assert!(matches!(
            m.capabilities.event_bus,
            CapabilityGrant::Bool(true)
        ));
        assert!(matches!(
            m.capabilities.write_plugin_data,
            CapabilityGrant::Bool(true)
        ));
    }

    /// Presence maps to the top-bar dot: Online only when Connected, Offline
    /// for every transient or failed link state.
    #[test]
    fn presence_maps_state() {
        assert_eq!(
            HangarPlugin::presence(&ConnState::Connected),
            Presence::Online
        );
        assert_eq!(
            HangarPlugin::presence(&ConnState::Dialing),
            Presence::Offline
        );
        assert_eq!(
            HangarPlugin::presence(&ConnState::Handshake),
            Presence::Offline
        );
        assert_eq!(
            HangarPlugin::presence(&ConnState::Disconnected),
            Presence::Offline
        );
        assert_eq!(
            HangarPlugin::presence(&ConnState::Error("x".into())),
            Presence::Offline
        );
    }

    /// A decoded subscribe ack drives the in-memory state machine to
    /// Connected without any socket — proves `on_daemon_response` wiring.
    #[test]
    fn subscribe_ack_response_reaches_connected() {
        let mut p = HangarPlugin::new();
        // Simulate the dial path's state advance.
        p.conn.dialing();
        p.conn.on_dialed("s1");
        let resp = ainb_hangar_proto::RpcResponse {
            jsonrpc: "2.0".into(),
            id: RpcId::Number(SUBSCRIBE_REQ_ID),
            result: Some(serde_json::json!({})),
            error: None,
        };
        p.on_daemon_response(&resp);
        assert!(p.conn.is_connected());
    }

    /// An error envelope on the subscribe id fails the handshake.
    #[test]
    fn subscribe_error_response_fails_handshake() {
        let mut p = HangarPlugin::new();
        p.conn.dialing();
        p.conn.on_dialed("s1");
        let resp = ainb_hangar_proto::RpcResponse {
            jsonrpc: "2.0".into(),
            id: RpcId::Number(SUBSCRIBE_REQ_ID),
            result: None,
            error: Some(ainb_hangar_proto::RpcError {
                code: -32601,
                message: "no such method".into(),
                data: None,
            }),
        };
        p.on_daemon_response(&resp);
        assert!(matches!(p.conn.state(), ConnState::Error(_)));
    }

    /// An EOF socket event drops a connected link back to Disconnected.
    #[test]
    fn socket_eof_disconnects() {
        let mut p = HangarPlugin::new();
        p.conn.dialing();
        p.conn.on_dialed("s1");
        p.conn.on_subscribe_ack();
        assert!(p.conn.is_connected());
        let eof = UnixSocketEvent {
            kind: UnixSocketEventKind::Eof,
            bytes: None,
            error: None,
        };
        p.on_socket_event(&eof);
        assert_eq!(*p.conn.state(), ConnState::Disconnected);
    }

    // ----- e38.36: offline empty-state + [s] start-daemon -----

    /// Build a `Press` key event for `ch`.
    fn char_press(ch: char) -> ainb_plugin_sdk::KeyEvent {
        ainb_plugin_sdk::KeyEvent {
            code: KeyCode::Char { ch },
            mods: 0,
            kind: ainb_plugin_sdk::KeyKind::Press,
        }
    }

    /// Read the whole composed buffer back to text for assertions.
    fn buf_text(buf: &WireBuffer, w: u16, h: u16) -> String {
        let mut grid = vec![vec![' '; w as usize]; h as usize];
        for (coord, cell) in &buf.cells {
            if coord.x < w && coord.y < h {
                if let Some(ch) = cell.symbol.chars().next() {
                    grid[coord.y as usize][coord.x as usize] = ch;
                }
            }
        }
        grid.into_iter()
            .map(|r| r.into_iter().collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// USER-VISIBLE PROOF (render): when the daemon link is offline the composed
    /// frame shows the offline empty-state — the explanation, the `[s]` hint, and
    /// the literal `ainb hangar daemon start` command — on the REAL offline
    /// render branch (`compose_frame`, the host-free tail of `render`).
    #[test]
    fn offline_compose_frame_shows_empty_state_panel() {
        let mut p = HangarPlugin::new();
        // Default state is Disconnected → offline.
        assert!(HangarPlugin::is_offline(p.conn.state()));
        let buf = p.compose_frame(100, 30);
        let text = buf_text(&buf, 100, 30);
        assert!(
            text.contains("daemon offline"),
            "missing panel title:\n{text}"
        );
        assert!(text.contains("[s]"), "missing [s] hint:\n{text}");
        assert!(
            text.contains("ainb hangar daemon start"),
            "missing literal command:\n{text}"
        );
        assert!(text.contains("not running"), "missing explanation:\n{text}");
    }

    /// A failed `[s]` start surfaces the error line in the composed offline frame
    /// rather than being silent.
    #[test]
    fn offline_frame_shows_start_error_when_present() {
        let mut p = HangarPlugin::new();
        p.daemon_start_error = Some("start failed: boom".to_string());
        let buf = p.compose_frame(100, 30);
        let text = buf_text(&buf, 100, 30);
        assert!(
            text.contains("start failed: boom"),
            "missing error:\n{text}"
        );
    }

    /// When CONNECTED the composed frame renders the normal body, NOT the offline
    /// panel — the panel is strictly gated on the offline branch.
    #[test]
    fn connected_compose_frame_omits_empty_state_panel() {
        let mut p = HangarPlugin::new();
        p.conn.dialing();
        p.conn.on_dialed("s1");
        p.conn.on_subscribe_ack();
        assert!(p.conn.is_connected());
        let buf = p.compose_frame(100, 30);
        let text = buf_text(&buf, 100, 30);
        assert!(
            !text.contains("daemon offline"),
            "panel must not show when connected:\n{text}"
        );
    }

    /// The transient mid-connect states keep the panel hidden — only genuinely
    /// down links (`Disconnected`/`Error`) show it, so a dial-in-flight doesn't
    /// flicker the guidance.
    #[test]
    fn transient_states_do_not_trigger_panel() {
        assert!(!HangarPlugin::is_offline(&ConnState::Dialing));
        assert!(!HangarPlugin::is_offline(&ConnState::Handshake));
        assert!(HangarPlugin::is_offline(&ConnState::Disconnected));
        assert!(HangarPlugin::is_offline(&ConnState::Error("x".into())));
    }

    /// USER-VISIBLE PROOF (key dispatch): an `[s]` press while OFFLINE arms the
    /// deferred daemon-start (the action seam drained in `render`).
    #[test]
    fn offline_s_key_arms_start_daemon() {
        let mut p = HangarPlugin::new();
        assert!(HangarPlugin::is_offline(p.conn.state()));
        assert!(!p.start_daemon_pending);
        p.on_key(&char_press('s'));
        assert!(
            p.start_daemon_pending,
            "[s] while offline must arm the daemon-start"
        );
    }

    /// When ONLINE, `s` is NOT the start action — it must not arm the daemon-start
    /// (so it can't shadow the online Settings/Skills `s` bindings).
    #[test]
    fn online_s_key_does_not_arm_start_daemon() {
        let mut p = HangarPlugin::new();
        p.conn.dialing();
        p.conn.on_dialed("s1");
        p.conn.on_subscribe_ack();
        assert!(p.conn.is_connected());
        p.on_key(&char_press('s'));
        assert!(
            !p.start_daemon_pending,
            "online `s` must not arm the daemon-start"
        );
    }

    /// The armed start actually dispatches to the `DaemonStarter` seam: with a
    /// recording starter, draining the pending start writes the probe marker
    /// (proving the `[s]` action reaches the real spawn path) and clears any prior
    /// error.
    #[test]
    fn try_start_daemon_dispatches_to_starter() {
        let dir = tempfile::tempdir().unwrap();
        let probe = dir.path().join("started.txt");
        let mut p = HangarPlugin::with_daemon_starter(Box::new(
            crate::shell::RecordingDaemonStarter::new(&probe),
        ));
        let ok = p.try_start_daemon();
        assert!(ok, "recording starter must succeed");
        let written = std::fs::read_to_string(&probe).expect("probe written");
        assert_eq!(written, "hangar daemon start");
        assert!(p.daemon_start_error.is_none());
    }

    /// A start failure is recorded (not panicked) so the empty-state can show it,
    /// and `try_start_daemon` reports `false` so the re-dial is skipped.
    #[test]
    fn try_start_daemon_records_error_on_failure() {
        let mut p = HangarPlugin::with_daemon_starter(Box::new(crate::shell::FailingDaemonStarter));
        let ok = p.try_start_daemon();
        assert!(!ok, "failing starter must report failure");
        assert!(
            p.daemon_start_error
                .as_deref()
                .is_some_and(|m| m.contains("start failed")),
            "failure must be recorded for the empty-state, got {:?}",
            p.daemon_start_error
        );
    }

    // ----- e38.13: command palette / cross-entity search overlay -----

    /// Build a `Ctrl+P` press (the command-palette chord).
    fn ctrl_p_press() -> ainb_plugin_sdk::KeyEvent {
        ainb_plugin_sdk::KeyEvent {
            code: KeyCode::Char { ch: 'p' },
            mods: ainb_plugin_sdk::KEY_MOD_CTRL,
            kind: ainb_plugin_sdk::KeyKind::Press,
        }
    }

    /// Build an Enter press.
    fn enter_press() -> ainb_plugin_sdk::KeyEvent {
        ainb_plugin_sdk::KeyEvent {
            code: KeyCode::Enter,
            mods: 0,
            kind: ainb_plugin_sdk::KeyKind::Press,
        }
    }

    /// Seed a connected plugin whose issue list already holds `Refactor API`
    /// (`issue-1`) so a palette jump to it is observable.
    fn connected_plugin_with_issue() -> HangarPlugin {
        use ainb_hangar_proto::events::IssueRow;
        let mut p = HangarPlugin::new();
        p.conn.dialing();
        p.conn.on_dialed("s1");
        p.conn.on_subscribe_ack();
        p.screens.set_issues(vec![IssueRow {
            id: ainb_hangar_core::ids::IssueId::from_str("issue-1").unwrap(),
            display_id: None,
            workspace_id: "default".into(),
            title: "Refactor API".into(),
            description: None,
            state: "open".into(),
            assignee: None,
            creator: "member:me".into(),
            created_at: 0,
            priority: 0,
            due_date: None,
            labels: Vec::new(),
            pr_url: None,
        }]);
        p
    }

    /// USER-VISIBLE PROOF (key+render): `Ctrl+P` opens the command-palette modal
    /// over any screen, a typed query arms the `hangar/search` RPC, loaded results
    /// render in the overlay, and Enter on a result JUMPS to that entity's screen
    /// (and selects the issue) — the wiring actually takes effect end to end.
    #[test]
    fn ctrl_p_opens_palette_renders_results_and_enter_navigates() {
        use ainb_hangar_proto::snapshots::{SearchEntry, SearchEntryKind};
        let mut p = connected_plugin_with_issue();

        // From the issue list, Ctrl+P opens the palette modal.
        assert!(matches!(p.app_state().screen, Screen::IssueList));
        p.on_key(&ctrl_p_press());
        assert!(
            matches!(p.app_state().screen, Screen::CommandPalette),
            "Ctrl+P must open the command palette"
        );
        assert!(p.screens.command_palette.is_some(), "palette state created");

        // Typing arms the search RPC (drained in `render`).
        p.on_key(&char_press('r'));
        assert!(
            matches!(
                p.screens.pending_palette_action,
                Some(crate::screen::PaletteAction::Search(ref q)) if q == "r"
            ),
            "a keystroke arms hangar/search for the typed query, got {:?}",
            p.screens.pending_palette_action
        );

        // Feed a ranked result back (as the `hangar/search` reply would) and prove
        // it renders inside the overlay.
        let resp = ainb_hangar_proto::RpcResponse {
            jsonrpc: "2.0".into(),
            id: RpcId::Number(SEARCH_REQ_ID),
            result: Some(
                serde_json::to_value(ainb_hangar_proto::snapshots::SearchResult {
                    entries: vec![SearchEntry {
                        kind: SearchEntryKind::Issue,
                        id: "issue-1".into(),
                        label: "Refactor API".into(),
                        screen: SearchEntryKind::Issue.target_screen().into(),
                    }],
                })
                .unwrap(),
            ),
            error: None,
        };
        p.on_daemon_response(&resp);
        let text = buf_text(&p.compose_frame(100, 30), 100, 30);
        assert!(
            text.contains("Refactor API") && text.contains("[issue]"),
            "the palette overlay must render the ranked result:\n{text}"
        );

        // Enter jumps to the selected entity's screen (issue → issue list) and
        // selects the matching issue row.
        p.on_key(&enter_press());
        assert!(
            matches!(p.app_state().screen, Screen::IssueList),
            "Enter on an issue result jumps to the issue list"
        );
        assert!(
            p.screens.command_palette.is_none(),
            "navigating dismisses the palette"
        );
        assert_eq!(
            p.screens.issue_list.selected_row().map(|r| r.id.as_str()),
            Some("issue-1"),
            "the jumped-to issue is selected"
        );
    }

    /// Esc closes the palette back to the screen that opened it, with no jump.
    #[test]
    fn esc_closes_palette_back_to_prior_screen() {
        let mut p = connected_plugin_with_issue();
        // Open from the Autopilots screen so the restore target is non-default.
        p.on_key(&char_press('4'));
        assert!(matches!(p.app_state().screen, Screen::Autopilots));
        p.on_key(&ctrl_p_press());
        assert!(matches!(p.app_state().screen, Screen::CommandPalette));
        p.on_key(&ainb_plugin_sdk::KeyEvent {
            code: KeyCode::Esc,
            mods: 0,
            kind: ainb_plugin_sdk::KeyKind::Press,
        });
        assert!(
            matches!(p.app_state().screen, Screen::Autopilots),
            "Esc restores the screen the palette overlaid"
        );
        assert!(p.screens.command_palette.is_none(), "palette dismissed");
    }

    /// A synthetic [`MouseEvent`](ainb_plugin_sdk::MouseEvent) at `(col, row)`.
    fn mouse_at(
        kind: ainb_plugin_sdk::MouseKind,
        col: u16,
        row: u16,
    ) -> ainb_plugin_sdk::MouseEvent {
        ainb_plugin_sdk::MouseEvent {
            kind,
            col,
            row,
            mods: 0,
        }
    }

    /// USER-VISIBLE PROOF (63l.2 — `handle_mouse` is the real consumed path): on the
    /// issue list, after a render builds the hit-map, a left-press over a painted
    /// card SELECTS it (and arms a redraw), and a press-then-release on that same
    /// card CLICK-OPENS its task detail once the render drains the intent. This
    /// drives the REAL plugin mouse path (`on_mouse` → FSM → intent stash →
    /// `drain_mouse_intents`), not the FSM in isolation.
    #[test]
    fn mouse_press_selects_and_click_opens_the_hit_card() {
        use ainb_hangar_proto::events::IssueRow;
        use ainb_plugin_sdk::{MouseButton, MouseKind};

        let mut p = connected_plugin_with_issue();
        // Add a second issue in a different lifecycle so the board has cards in
        // more than one column (the press must resolve the RIGHT card).
        p.screens.set_issues(vec![
            IssueRow {
                id: ainb_hangar_core::ids::IssueId::from_str("issue-1").unwrap(),
                display_id: Some("HGR-1".into()),
                workspace_id: "default".into(),
                title: "Refactor API".into(),
                description: None,
                state: "todo".into(),
                assignee: None,
                creator: "member:me".into(),
                created_at: 0,
                priority: 0,
                due_date: None,
                labels: Vec::new(),
                pr_url: None,
            },
            IssueRow {
                id: ainb_hangar_core::ids::IssueId::from_str("issue-2").unwrap(),
                display_id: Some("HGR-2".into()),
                workspace_id: "default".into(),
                title: "Wire the mouse".into(),
                description: None,
                state: "in_progress".into(),
                assignee: Some("agent:claude".into()),
                creator: "member:me".into(),
                created_at: 0,
                priority: 2,
                due_date: None,
                labels: Vec::new(),
                pr_url: None,
            },
        ]);

        // A render builds the hit-map for the current board geometry.
        p.rebuild_hit_map(120, 24);
        assert!(
            !p.hit_map.is_empty(),
            "the issue-list render must record a board hit-map"
        );

        // Find the on-screen rect of the issue-2 card (In Progress, column 2) the
        // SAME way the render painted it, so the press lands on it.
        let columns = p.screens.issue_list.board_columns();
        let mut scratch = WireBuffer::new(120, 24);
        let layout =
            crate::widgets::card_board::render_card_board(&mut scratch, 120, 2, 23, &columns, None);
        let card2 = layout
            .columns
            .iter()
            .flat_map(|c| c.cards.iter())
            .find(|c| c.issue_id == "issue-2")
            .expect("issue-2 must be painted as a card")
            .rect;
        let (cx, cy) = (card2.x + 1, card2.y + 1);

        // Left-press the card → the FSM stashes a Select intent and arms a redraw
        // (the queue IS the redraw signal). The selection applies on the drain.
        p.on_mouse(mouse_at(
            MouseKind::Down {
                button: MouseButton::Left,
            },
            cx,
            cy,
        ));
        assert!(p.wants_redraw(), "a mouse gesture arms a redraw");
        assert!(
            p.pending_mouse_intents
                .iter()
                .any(|i| matches!(i, crate::mouse::MouseIntent::Select(id) if id == "issue-2")),
            "the press stashes a Select(issue-2) intent"
        );
        p.drain_mouse_intents();
        assert_eq!(
            p.screens.issue_list.selected_row().map(|r| r.id.as_str()),
            Some("issue-2"),
            "draining the press selects the hit card"
        );
        assert!(!p.wants_redraw(), "the drain empties the redraw queue");

        // Release on the same card with no drag → ClickOpen, stashed for the
        // render drain.
        p.on_mouse(mouse_at(
            MouseKind::Up {
                button: MouseButton::Left,
            },
            cx,
            cy,
        ));
        assert!(
            matches!(p.mouse_fsm.state(), crate::mouse::MouseState::Idle),
            "release returns the FSM to Idle"
        );
        assert!(
            p.pending_mouse_intents
                .iter()
                .any(|i| matches!(i, crate::mouse::MouseIntent::ClickOpen(id) if id == "issue-2")),
            "a click stashes a ClickOpen intent for the render drain"
        );

        // The render drains the click → the task detail for issue-2 opens.
        p.drain_mouse_intents();
        assert!(
            matches!(p.app_state().screen, Screen::TaskDetail(_)),
            "draining the click opens the clicked issue's task detail, got {:?}",
            p.app_state().screen
        );
        assert!(!p.wants_redraw(), "the drain empties the redraw queue");
    }

    /// MUTATION GUARD (63l.2): a press on EMPTY board space (no card) must NOT
    /// select any card nor stash a `ClickOpen` — proving the selection is driven by
    /// the hit-test, not by the press happening at all.
    #[test]
    fn mouse_press_on_empty_space_selects_nothing() {
        use ainb_plugin_sdk::{MouseButton, MouseKind};
        let mut p = connected_plugin_with_issue();
        p.rebuild_hit_map(120, 24);
        let before = p
            .screens
            .issue_list
            .selected_row()
            .map(|r| r.id.as_str().to_string());

        // Press far outside any column (bottom-right corner of an 80-wide layout
        // region that has empty columns there).
        p.on_mouse(mouse_at(
            MouseKind::Down {
                button: MouseButton::Left,
            },
            119,
            23,
        ));
        p.on_mouse(mouse_at(
            MouseKind::Up {
                button: MouseButton::Left,
            },
            119,
            23,
        ));
        // No ClickOpen stashed, and the selection is unchanged.
        assert!(
            !p.pending_mouse_intents
                .iter()
                .any(|i| matches!(i, crate::mouse::MouseIntent::ClickOpen(_))),
            "a press on empty space must not click-open a card"
        );
        let after = p
            .screens
            .issue_list
            .selected_row()
            .map(|r| r.id.as_str().to_string());
        assert_eq!(
            before, after,
            "empty-space press leaves the selection alone"
        );
    }

    /// USER-VISIBLE PROOF (63l.2 — drag→move intent): a left-press on a card then
    /// a drag into another column's drop zone then a release emits a `MoveCard`
    /// intent carrying the destination lifecycle status — the framework produces
    /// the move intent the P2 RPC binding consumes.
    #[test]
    fn mouse_drag_across_columns_stashes_move_card_intent() {
        use ainb_hangar_proto::events::IssueRow;
        use ainb_plugin_sdk::{MouseButton, MouseKind};

        let mut p = connected_plugin_with_issue();
        p.screens.set_issues(vec![IssueRow {
            id: ainb_hangar_core::ids::IssueId::from_str("issue-1").unwrap(),
            display_id: Some("HGR-1".into()),
            workspace_id: "default".into(),
            title: "Refactor API".into(),
            description: None,
            state: "backlog".into(),
            assignee: None,
            creator: "member:me".into(),
            created_at: 0,
            priority: 0,
            due_date: None,
            labels: Vec::new(),
            pr_url: None,
        }]);
        p.rebuild_hit_map(120, 24);

        // Press the backlog card (column 0), then drag into a different column's
        // body and release there.
        let columns = p.screens.issue_list.board_columns();
        let mut scratch = WireBuffer::new(120, 24);
        let layout =
            crate::widgets::card_board::render_card_board(&mut scratch, 120, 2, 23, &columns, None);
        let card = layout.columns[0].cards[0].rect;
        // A point in column 2 (In Progress) body — to the right of column 0/1.
        let other = layout.columns[2].rect;
        let (ox, oy) = (other.x + 2, other.y + 5);

        p.on_mouse(mouse_at(
            MouseKind::Down {
                button: MouseButton::Left,
            },
            card.x + 1,
            card.y + 1,
        ));
        p.on_mouse(mouse_at(
            MouseKind::Drag {
                button: MouseButton::Left,
            },
            ox,
            oy,
        ));
        p.on_mouse(mouse_at(
            MouseKind::Up {
                button: MouseButton::Left,
            },
            ox,
            oy,
        ));

        assert!(
            p.pending_mouse_intents.iter().any(|i| matches!(
                i,
                crate::mouse::MouseIntent::MoveCard { issue_id, to_status }
                    if issue_id == "issue-1"
                        && *to_status == ainb_hangar_proto::lifecycle::IssueLifecycle::InProgress
            )),
            "a cross-column drag-drop must stash a MoveCard{{issue-1, InProgress}} intent, \
             got {:?}",
            p.pending_mouse_intents
        );
    }

    /// USER-VISIBLE PROOF (63l.4 — the drag TAKES EFFECT): draining a cross-column
    /// `MoveCard` intent both (a) MOVES the card optimistically into the
    /// destination column AND (b) arms the durable `hangar/issue_update{state}`
    /// RPC for the next render — so a drag actually moves a real issue, not just a
    /// local highlight. A `ClickOpen` on the same drain path opens the task detail.
    #[test]
    fn draining_move_card_moves_optimistically_and_arms_issue_update() {
        use crate::screen::issue_list::IssueColumn;
        use ainb_hangar_proto::events::IssueRow;
        use ainb_hangar_proto::lifecycle::IssueLifecycle;

        let mut p = connected_plugin_with_issue();
        p.screens.set_issues(vec![IssueRow {
            id: ainb_hangar_core::ids::IssueId::from_str("issue-1").unwrap(),
            display_id: Some("HGR-1".into()),
            workspace_id: "default".into(),
            title: "Refactor API".into(),
            description: None,
            state: "backlog".into(),
            assignee: None,
            creator: "member:me".into(),
            created_at: 0,
            priority: 0,
            due_date: None,
            labels: Vec::new(),
            pr_url: None,
        }]);
        // The card starts in Backlog.
        assert_eq!(p.screens.issue_list.column_count(IssueColumn::Backlog), 1);
        assert_eq!(
            p.screens.issue_list.column_count(IssueColumn::InProgress),
            0
        );

        // Stash the MoveCard intent directly (the FSM origin is exercised by the
        // sibling drag test) and drain it.
        p.pending_mouse_intents
            .push(crate::mouse::MouseIntent::MoveCard {
                issue_id: "issue-1".into(),
                to_status: IssueLifecycle::InProgress,
            });
        p.drain_mouse_intents();

        // (a) The board moved the card optimistically: it now reads in In Progress.
        assert_eq!(p.screens.issue_list.column_count(IssueColumn::Backlog), 0);
        assert_eq!(
            p.screens.issue_list.column_count(IssueColumn::InProgress),
            1,
            "the drag moves the card optimistically into the destination column"
        );
        // (b) The durable issue_update{state:in_progress} RPC is armed for render.
        assert_eq!(
            p.pending_issue_state_update,
            Some(("issue-1".to_string(), IssueLifecycle::InProgress)),
            "a cross-column drag must arm the in_progress issue_update RPC"
        );

        // A click on the moved card opens its task detail (the open intent takes
        // effect on the same drain path).
        p.pending_mouse_intents
            .push(crate::mouse::MouseIntent::ClickOpen("issue-1".into()));
        p.drain_mouse_intents();
        assert!(
            matches!(p.app_state().screen, Screen::TaskDetail(_)),
            "a click opens the issue's task detail, got {:?}",
            p.app_state().screen
        );
    }

    /// 63l.4 — a wheel-scroll over a column drains into a real per-column scroll
    /// offset (the render reflects it), and a hover drains into the hover
    /// highlight. Both are local board state, no RPC.
    #[test]
    fn draining_scroll_and_hover_updates_local_board_state() {
        use ainb_hangar_proto::events::IssueRow;
        use ainb_hangar_proto::lifecycle::IssueLifecycle;

        let mut p = connected_plugin_with_issue();
        // Several Todo cards so a scroll offset is observable.
        let rows: Vec<IssueRow> = (0..4)
            .map(|i| IssueRow {
                id: ainb_hangar_core::ids::IssueId::from_str(format!("t{i}")).unwrap(),
                display_id: Some(format!("HGR-{i}")),
                workspace_id: "default".into(),
                title: format!("Task {i}"),
                description: None,
                state: "todo".into(),
                assignee: None,
                creator: "member:me".into(),
                created_at: 0,
                priority: 0,
                due_date: None,
                labels: Vec::new(),
                pr_url: None,
            })
            .collect();
        p.screens.set_issues(rows);

        // A wheel-scroll-down over the Todo column nudges its offset to 1.
        p.pending_mouse_intents
            .push(crate::mouse::MouseIntent::ScrollColumn {
                status: IssueLifecycle::Todo,
                delta: 1,
            });
        // A hover over t1 sets the hover highlight.
        p.pending_mouse_intents
            .push(crate::mouse::MouseIntent::Hover(Some("t1".into())));
        p.drain_mouse_intents();

        let cols = p.screens.issue_list.board_columns();
        let todo = &cols[IssueLifecycle::Todo.order()];
        assert_eq!(
            todo.scroll_offset, 1,
            "the wheel-scroll offsets the Todo column"
        );
        assert_eq!(
            p.screens.issue_list.hovered_id(),
            Some("t1"),
            "the hover intent sets the hovered card"
        );
    }

    // ----- 63l.5: right-click context-menu overlay -----

    /// Build a bare key-code press (Up/Down/Right/Left/Enter/Esc).
    fn key_press(code: KeyCode) -> ainb_plugin_sdk::KeyEvent {
        ainb_plugin_sdk::KeyEvent {
            code,
            mods: 0,
            kind: ainb_plugin_sdk::KeyKind::Press,
        }
    }

    /// Build a left-button-down mouse event at `(col, row)`.
    fn down_left(col: u16, row: u16) -> ainb_plugin_sdk::MouseEvent {
        ainb_plugin_sdk::MouseEvent {
            kind: ainb_plugin_sdk::MouseKind::Down {
                button: ainb_plugin_sdk::MouseButton::Left,
            },
            col,
            row,
            mods: 0,
        }
    }

    /// Seed a connected plugin whose board holds two cards (`card-a` in Todo,
    /// `card-b` in Backlog) plus a cached actor snapshot, so a right-click menu on
    /// `card-b` can move/prioritise/assign it.
    fn connected_plugin_with_two_cards() -> HangarPlugin {
        use ainb_hangar_proto::events::{ActorRow, IssueRow, PresenceState};
        let mut p = connected_plugin_with_issue();
        p.screens.set_issues(vec![
            IssueRow {
                id: ainb_hangar_core::ids::IssueId::from_str("card-a").unwrap(),
                display_id: Some("HGR-1".into()),
                workspace_id: "default".into(),
                title: "Alpha".into(),
                description: None,
                state: "todo".into(),
                assignee: None,
                creator: "member:me".into(),
                created_at: 0,
                priority: 0,
                due_date: None,
                labels: Vec::new(),
                pr_url: None,
            },
            IssueRow {
                id: ainb_hangar_core::ids::IssueId::from_str("card-b").unwrap(),
                display_id: Some("HGR-2".into()),
                workspace_id: "default".into(),
                title: "Bravo".into(),
                description: None,
                state: "backlog".into(),
                assignee: None,
                creator: "member:me".into(),
                created_at: 0,
                priority: 0,
                due_date: None,
                labels: Vec::new(),
                pr_url: None,
            },
        ]);
        p.screens.set_actors(vec![ActorRow {
            actor_ref: "member:alice".into(),
            display_name: "alice".into(),
            subtitle: "dev".into(),
            presence: PresenceState::Online,
            is_agent: false,
            recent_rank: Some(0),
        }]);
        p
    }

    /// USER-VISIBLE PROOF (63l.5 — the menu TAKES EFFECT): a right-click on a card
    /// raises the context menu, and navigating `Move to ▸ In Progress` then Enter
    /// arms the durable `hangar/issue_update{state:in_progress}` RPC for THAT card
    /// (`card-b`) and closes the menu. The card also moves optimistically.
    #[test]
    fn context_menu_move_to_in_progress_arms_issue_update_for_card_b() {
        use ainb_hangar_proto::lifecycle::IssueLifecycle;

        let mut p = connected_plugin_with_two_cards();
        // A right-click on card-b raises the overlay (drained from the mouse intent).
        p.pending_mouse_intents
            .push(crate::mouse::MouseIntent::OpenContextMenu {
                issue_id: "card-b".into(),
                at: (40, 6),
            });
        p.drain_mouse_intents();
        assert!(
            p.context_menu
                .as_ref()
                .is_some_and(|m| m.issue_id() == "card-b"),
            "a right-click raises the menu for the clicked card"
        );

        // Navigate: Down to `Move to`, Right to open the submenu (pre-selects the
        // current status, Backlog → order 0), then Down to In Progress (order 2).
        p.on_key(&key_press(KeyCode::Down)); // Move to
        p.on_key(&key_press(KeyCode::Right)); // open submenu (Backlog preselected)
        p.on_key(&key_press(KeyCode::Down)); // Todo
        p.on_key(&key_press(KeyCode::Down)); // In Progress
        p.on_key(&key_press(KeyCode::Enter)); // fire

        // The menu closed and armed the durable state-update RPC for card-b.
        assert!(p.context_menu.is_none(), "firing a leaf closes the menu");
        assert_eq!(
            p.pending_issue_state_update,
            Some(("card-b".to_string(), IssueLifecycle::InProgress)),
            "Move to > In Progress arms issue_update with state=in_progress for card-b"
        );
    }

    /// USER-VISIBLE PROOF (63l.5): `Priority ▸ High` arms
    /// `hangar/issue_update{priority:2}` (the wire scalar `High` round-trips to).
    #[test]
    fn context_menu_priority_high_arms_priority_update() {
        let mut p = connected_plugin_with_two_cards();
        p.open_context_menu("card-b", (40, 6));

        // Down x2 to `Priority`, Right to open (current priority 0 → None at idx 3),
        // Up x2 to High (None→Medium→High), Enter to fire.
        p.on_key(&key_press(KeyCode::Down)); // Move to
        p.on_key(&key_press(KeyCode::Down)); // Priority
        p.on_key(&key_press(KeyCode::Right)); // open submenu
        p.on_key(&key_press(KeyCode::Up)); // Medium
        p.on_key(&key_press(KeyCode::Up)); // High
        p.on_key(&key_press(KeyCode::Enter)); // fire

        assert!(p.context_menu.is_none(), "firing closes the menu");
        assert_eq!(
            p.pending_issue_priority_update,
            Some(("card-b".to_string(), 2)),
            "Priority > High arms issue_update with priority=2 for card-b"
        );
    }

    /// USER-VISIBLE PROOF (63l.5): `Assign ▸ alice` arms
    /// `hangar/issue_update{assignee:member:alice}` over the cached actor.
    #[test]
    fn context_menu_assign_arms_assignee_update() {
        let mut p = connected_plugin_with_two_cards();
        p.open_context_menu("card-b", (40, 6));

        // Down x3 to `Assign`, Right to open, Enter on the first actor (alice).
        p.on_key(&key_press(KeyCode::Down)); // Move to
        p.on_key(&key_press(KeyCode::Down)); // Priority
        p.on_key(&key_press(KeyCode::Down)); // Assign
        p.on_key(&key_press(KeyCode::Right)); // open submenu
        p.on_key(&key_press(KeyCode::Enter)); // fire on alice

        assert_eq!(
            p.pending_issue_assignee_update,
            Some(("card-b".to_string(), "member:alice".to_string())),
            "Assign > alice arms issue_update with assignee=member:alice for card-b"
        );
    }

    /// 63l.5: Esc inside an open submenu collapses to the root; Esc at the root
    /// closes the whole menu back to the board (no RPC armed).
    #[test]
    fn context_menu_esc_collapses_then_closes() {
        let mut p = connected_plugin_with_two_cards();
        p.open_context_menu("card-b", (40, 6));
        p.on_key(&key_press(KeyCode::Down)); // Move to
        p.on_key(&key_press(KeyCode::Right)); // open submenu
        p.on_key(&key_press(KeyCode::Esc)); // collapse submenu
        assert!(
            p.context_menu.is_some(),
            "Esc in a submenu collapses, not closes the menu"
        );
        p.on_key(&key_press(KeyCode::Esc)); // close at root
        assert!(p.context_menu.is_none(), "Esc at the root closes the menu");
        assert!(
            p.pending_issue_state_update.is_none()
                && p.pending_issue_priority_update.is_none()
                && p.pending_issue_assignee_update.is_none(),
            "closing the menu without picking a leaf arms no RPC"
        );
    }

    /// USER-VISIBLE PROOF (63l.5 — render + mouse): the menu paints its items and a
    /// left-click on the painted `Open` row opens the clicked card's task detail,
    /// proving the rendered hit-map is the consumed click path.
    #[test]
    fn context_menu_renders_and_click_open_takes_effect() {
        let mut p = connected_plugin_with_two_cards();
        p.open_context_menu("card-b", (10, 5));

        // Render the frame: the menu paints its title + items, and records its
        // hit-map for the next click.
        let frame = p.compose_frame(100, 30);
        let text = buf_text(&frame, 100, 30);
        for needle in ["HGR-2", "Open", "Move to", "Priority", "Assign", "Copy id"] {
            assert!(
                text.contains(needle),
                "the context menu must paint `{needle}`:\n{text}"
            );
        }

        // A left-click on the painted `Open` root row opens the card's task detail.
        // The root box is anchored at (10, 5); `Open` is the first row two below the
        // title (row 5 + 2 = 7), inside the box columns.
        p.on_mouse(down_left(13, 7));
        assert!(
            p.context_menu.is_none(),
            "clicking Open closes the menu, got {:?}",
            p.context_menu.is_some()
        );
        assert!(
            matches!(p.app_state().screen, Screen::TaskDetail(_)),
            "clicking Open opens the card's task detail, got {:?}",
            p.app_state().screen
        );
    }
}
