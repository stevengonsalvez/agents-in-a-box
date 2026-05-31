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

use ainb_hangar_proto::{methods as daemon_methods, RpcId, RpcResponse};
use ainb_plugin_sdk::{
    CliOutput, HandleEventParams, HandleKeyParams, HostClient, KeyCode, Plugin, RenderParams,
    Result, RpcError, UnixSocketEvent, UnixSocketEventKind, WireBuffer,
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
const DAEMON_SOCKET_PATH: &str = "~/.ainb/hangar.sock";

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
        let _ = host.log_info("hangar: dialed daemon, subscribe sent").await;
    }

    /// Feed an inbound `socket:<stream_id>` event into the connection.
    ///
    /// Decodes the [`UnixSocketEvent`] envelope: `Data` bytes are pushed
    /// to the [`FrameDecoder`] and each complete daemon response is
    /// matched against the subscribe id to ack the handshake; `Eof`
    /// drops to `Disconnected`; `Error` lands in `Error`.
    fn on_socket_event(&mut self, event: &UnixSocketEvent) {
        match event.kind {
            UnixSocketEventKind::Data => {
                let Some(bytes) = event.bytes.as_ref() else {
                    return;
                };
                match self.decoder.push(bytes) {
                    Ok(responses) => {
                        for resp in responses {
                            self.on_daemon_response(&resp);
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

    /// React to one fully-decoded daemon response.
    fn on_daemon_response(&mut self, resp: &RpcResponse) {
        match resp.id {
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
            // Mutating RPCs (skill sync/attach/detach, autopilot fire/toggle,
            // kanban task transition) answer `{}`; we re-fetch the relevant lists
            // to refresh derived columns (`used`, next-tick, enabled, last-run,
            // task status buckets).
            RpcId::Number(
                SKILLS_SYNC_REQ_ID
                | SKILL_ATTACH_REQ_ID
                | SKILL_DETACH_REQ_ID
                | AUTOPILOT_FIRE_REQ_ID
                | AUTOPILOT_TOGGLE_REQ_ID
                | TASK_TRANSITION_REQ_ID,
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
        if let Ok(r) =
            serde_json::from_value::<ainb_hangar_proto::snapshots::AutopilotRunsResult>(
                result.clone(),
            )
        {
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

    /// Fire the four `hangar/*` snapshot requests over the daemon stream, framed
    /// for the cap. A send failure is logged but non-fatal — the screens simply
    /// stay empty until the next subscribe.
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

        // Snapshot the routing state so the borrow on `self.app` is released
        // before we mutate `self.screens` and `self.app` below.
        let app = self.app_state().clone();

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
        }
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
            if matches!(*ch, '1' | '2' | '4' | '5' | 'K' | 'D' | 'L' | ',' | '?' | 'q') =>
        {
            Some(AppEvent::Key(*ch))
        }
        // Esc only routes through the router when a modal is open (close it);
        // otherwise it is a per-screen concern handled by `route_key`.
        KeyCode::Esc if app.screen.is_modal() => Some(AppEvent::Esc),
        _ => None,
    }
}

#[async_trait]
impl Plugin for HangarPlugin {
    fn manifest(&self) -> &'static str {
        MANIFEST_TOML
    }

    async fn on_init(&mut self, host: &HostClient, _granted: &[String]) -> Result<()> {
        // P5.6: decide whether to show the first-run danger-full-access modal
        // from the recorded acks in `~/.ainb/hangar/state.toml`. A missing file
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
        // P8.6: the logs pane reads the daemon's structured-log file directly
        // (not a daemon RPC). Re-read on every render while it is the active
        // screen so live events surface, and on a pending level-filter change.
        let on_logs = matches!(self.app_state().screen, Screen::Logs);
        if on_logs || self.screens.take_pending_logs_refresh() {
            self.refresh_logs();
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
        // P5.6: the first-run danger-full-access modal overlays everything (last
        // write wins on the sparse buffer) until the user accepts.
        if self.first_run.is_showing() {
            crate::widgets::danger_access_modal::render_danger_access_modal(&mut buf, w, h);
        }
        Ok(buf)
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
            ["~/.ainb/hangar.sock", "${XDG_RUNTIME_DIR}/ainb-hangar.sock"]
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
}
