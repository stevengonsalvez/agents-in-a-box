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
use ainb_hangar_proto::{RpcId, RpcResponse, methods as daemon_methods};
use ainb_plugin_sdk::{
    CliOutput, HandleEventParams, HandleKeyParams, HostClient, InitContext, KeyCode, Plugin,
    RenderParams, Result, RpcError, UnixSocketEvent, UnixSocketEventKind, WireBuffer,
};
use async_trait::async_trait;

use crate::chrome::{Presence, render_footer, render_top_bar};
use crate::connection::{ConnState, Connection, DEFAULT_WORKSPACE_ID};
use crate::firstrun::{self, FirstRunIntent, FirstRunModal, reduce_first_run};
use crate::jsonrpc_over_socket::{FrameDecoder, encode_request};
use crate::screen::{
    AppEvent, AppState, NavIntent, Screen, ScreenStates, WorkspaceAction, render_body, route_key,
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

/// The daemon socket path the plugin dials when `$AINB_HANGAR_HOME` is NOT set.
/// The host `unix_socket_dial` cap expands `~` and canonicalizes before
/// checking the manifest whitelist (which lists this exact unexpanded string).
const DEFAULT_DAEMON_SOCKET_PATH: &str = "~/.agents-in-a-box/hangar.sock";

/// The daemon socket path the plugin dials when `$AINB_HANGAR_HOME` IS set.
/// Sent UNEXPANDED so the host (which owns env/`~` expansion for BOTH the dial
/// request and the allow-list) resolves it the same way on both sides — the
/// plugin must never expand it itself, or the canonicalized strings would
/// diverge and the cap gate would deny the dial (-32001).
const OVERRIDE_DAEMON_SOCKET_PATH: &str = "${AINB_HANGAR_HOME}/hangar.sock";

/// Resolve the unexpanded socket-path string the plugin sends to
/// `host/unix_socket_dial`.
///
/// The daemon binds its control socket under the resolved Hangar home
/// (`{hangar_home}/hangar.sock`), so when `$AINB_HANGAR_HOME` is set the socket
/// MOVES with it. This returns the matching allow-listed form: the
/// `${AINB_HANGAR_HOME}/hangar.sock` template when the env var is set and
/// non-empty (the host expands `${VAR}`), else the `~/.agents-in-a-box`
/// default. Both forms are on the manifest `unix_socket_dial` allow-list, so
/// the host's `path_allowed` gate permits whichever one we return. Returning
/// the UNEXPANDED string (not a pre-expanded absolute path) is essential: the
/// host expands the dial request and the allow-list entry with the same code,
/// so a non-default home still passes the exact-match cap check.
fn daemon_socket_path() -> &'static str {
    match std::env::var_os(ainb_hangar_core::paths::HANGAR_HOME_ENV) {
        Some(v) if !v.is_empty() => OVERRIDE_DAEMON_SOCKET_PATH,
        _ => DEFAULT_DAEMON_SOCKET_PATH,
    }
}

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
/// create wizard (Phase 5; formerly the e38.29 inline title-only flow). Its
/// reply carries the new `IssueRow`, whose id arms the follow-up
/// `issue_update` + `issue_run` dispatch chain.
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
/// JSON-RPC id for the fleet-wide `attention/subscribe` request that registers
/// the control-center's live attention stream and seeds + refreshes the board
/// (P2). Rides the snapshot fetch; its ack carries the current open snapshot.
const ATTENTION_SUBSCRIBE_REQ_ID: i64 = 34;
/// JSON-RPC id for an `attention/answer` request raised by the control-center's
/// inline ASK answering (P2).
const ATTENTION_ANSWER_REQ_ID: i64 = 35;
/// JSON-RPC id for the `hangar/boards_list` snapshot request (P4 / D8). Every
/// board read AND mutation reply carries this id — the daemon answers each
/// `hangar/board_*` with the refreshed `BoardsListResult`, so one `apply_boards`
/// handler folds them all.
const BOARDS_REQ_ID: i64 = 36;
/// JSON-RPC id for the `hangar/squads_list` snapshot request (P7 / D17). Every
/// squad read AND the `squad_create` / `squad_member_add` / `squad_member_remove`
/// mutation replies carry this id — the daemon answers each with the refreshed
/// `SquadsListResult`, so one `apply_squads` handler folds them all.
const SQUADS_LIST_REQ_ID: i64 = 37;
/// JSON-RPC id for a `hangar/squad_fanout` request raised by the Squads screen's
/// assign key (P7). Its reply is a `SquadFanoutResult` (not a squads list), so it
/// is dispatched separately to surface the "briefed the leader + N members" note.
const SQUAD_FANOUT_REQ_ID: i64 = 38;
/// JSON-RPC id for the `hangar/run_history` snapshot request feeding the usage
/// dashboard's recent-runs timeline (P10 / D19). Renumbered to 39 on the wave-C
/// merge — P7's squad ids (37/38) landed first, so run-history takes the next id.
const RUN_HISTORY_REQ_ID: i64 = 39;
/// JSON-RPC id for the `profile/list` snapshot request feeding the profile-editor
/// roster (P5). Renumbered to 40 on the wave-C merge — boards/squads/run-history
/// (36-39) landed first, so the profile ids take the next contiguous block.
const PROFILE_LIST_REQ_ID: i64 = 40;
/// JSON-RPC id for a `profile/get` request raised when the profile-editor
/// selection moves to a row whose detail is not loaded (P5).
const PROFILE_GET_REQ_ID: i64 = 41;
/// JSON-RPC id for a `profile/upsert` request raised by the profile-editor `t`
/// tier cycle (P5).
const PROFILE_UPSERT_REQ_ID: i64 = 42;
/// JSON-RPC id for a `hangar/board_card_run` request raised by the Boards `Run ▾`
/// (ccc / D6). The reply carries the routed agent/runtime, surfaced as a note.
const BOARD_CARD_RUN_REQ_ID: i64 = 43;
/// JSON-RPC id for the `hangar/repo_list` snapshot request feeding the Boards
/// card-create `@` autocomplete roster (spec F3). Host-scoped (a repo picker is
/// not workspace-partitioned), fetched once alongside the other snapshots.
const REPO_LIST_REQ_ID: i64 = 44;
/// JSON-RPC id for a `hangar/board_card_cancel` mutation (tcp T3 / F6). The reply
/// is a `BoardCardCancelResult` surfaced as a transient board note; the card
/// leaves the running state via the daemon's pushed `TaskFinished(Cancelled)`.
const BOARD_CARD_CANCEL_REQ_ID: i64 = 45;
/// JSON-RPC id for a `hangar/board_card_timeline` fetch (tcp T3 / F6). The reply
/// carries a card's newest run transcript (raw stream-json); the plugin parses it
/// into the prettied timeline overlay.
const BOARD_CARD_TIMELINE_REQ_ID: i64 = 46;
/// JSON-RPC id for the `hangar/notify_rules_list` fetch feeding the Settings
/// Notifications routing grid (tcp T5). Workspace-scoped; fetched on first entry
/// to the section.
const NOTIFY_RULES_REQ_ID: i64 = 47;
/// JSON-RPC id for a `hangar/notify_rule_set` upsert raised by a toggled routing
/// cell (tcp T5). Its reply re-fetches the grid so the pane reflects the write.
const NOTIFY_RULE_SET_REQ_ID: i64 = 48;
/// `hangar/daemon_config_list` for the Settings Daemon-section config rows: reads
/// every daemon-config knob's live value in one round trip.
const DAEMON_CONFIG_LIST_REQ_ID: i64 = 49;
/// `hangar/daemon_config_set` write from a Daemon-section knob edit; its reply
/// re-fetches the whole config so the pane reflects the persisted value.
const DAEMON_CONFIG_SET_REQ_ID: i64 = 50;
/// JSON-RPC id for a `hangar/agent_create` raised by the Squads screen `n`
/// create-agent prompt. The reply carries the refreshed `AgentsListResult`, so
/// it folds through [`Self::apply_agents`] into the cached actors that drive
/// `first_agent_ref` — clearing the "no agent available to lead a squad" gate live.
///
/// 49/50 are the daemon-config get/set pair, which landed on main while this
/// branch was in review — this id must stay clear of them.
const AGENT_CREATE_REQ_ID: i64 = 51;
/// JSON-RPC id for a `hangar/issue_run` request raised by the Issues create
/// wizard's dispatch (Phase 5). The reply is a `BoardCardRunResult` (same shape
/// as `board_card_run`), surfaced as a transient issue-list note; an error is
/// surfaced the same way — never silent.
const ISSUE_RUN_REQ_ID: i64 = 52;
/// Request id for the issue-list `x` delete (63d). The reply is a bare `{}` ack
/// (the row is dropped by the daemon's `IssueDeleted` push, not this reply); an
/// error (e.g. an active task) is surfaced as a transient issue-list note — never
/// silent.
const ISSUE_DELETE_REQ_ID: i64 = 53;
/// JSON-RPC id for the `hangar/issue_cancel_active` mutation (board-less "cancel
/// run(s) & delete"). On success the plugin retries the `issue_delete`; an error
/// surfaces as a transient issue-list note.
const ISSUE_CANCEL_ACTIVE_REQ_ID: i64 = 54;
/// JSON-RPC id for the `hangar/task_retry` force-requeue raised by the Task Kanban
/// failed-column / task-detail `R` (a human override of the auto-retry gate).
const TASK_RETRY_REQ_ID: i64 = 55;
/// JSON-RPC id for a `hangar/agent_delete` raised by the Agents roster `x` confirm
/// (slice 2). The reply carries the refreshed `AgentsListResult`, so it folds
/// through [`Self::apply_agents`] into the cached actors — the same seam
/// [`AGENT_CREATE_REQ_ID`] uses, so the deleted row drops from the roster live.
const AGENT_DELETE_REQ_ID: i64 = 56;
/// Authoritative Fleet registry snapshot.
const FLEET_SNAPSHOT_REQ_ID: i64 = 57;
/// Gapless Fleet revision subscription.
const FLEET_SUBSCRIBE_REQ_ID: i64 = 58;
/// One versioned Fleet control action.
const FLEET_ACTION_REQ_ID: i64 = 59;
/// Explicit-recipient Fleet broadcast.
const FLEET_BROADCAST_REQ_ID: i64 = 60;
/// Daemon-owned new Fleet session start.
const FLEET_START_REQ_ID: i64 = 64;
/// JSON-RPC id for a `hangar/skill_set_enabled` request (parity #24).
const SKILL_SET_ENABLED_REQ_ID: i64 = 61;
/// JSON-RPC id for a `hangar/agent_skills_list` request (parity #24). Fired
/// after every attach / detach / toggle so the ` (disabled)` marker is fresh.
const AGENT_SKILLS_LIST_REQ_ID: i64 = 62;
/// JSON-RPC id for a `hangar/issue_criterion_set` request (multica parity
/// #11-rest). Fired by the task-detail `t` key; the reply is an `IssueRow` and
/// the daemon's `IssueUpdated` push re-renders the card.
const ISSUE_CRITERION_SET_REQ_ID: i64 = 63;

/// `hangar/issue_timeline` — the per-issue activity + comment narrative behind
/// the `y` modal (multica parity #13).
const ISSUE_TIMELINE_REQ_ID: i64 = 64;
/// The actor-ref the plugin authors comments as (e38.5).
///
/// The plugin has no per-user auth/identity layer yet (a later concern), so a
/// comment the local user composes is attributed to this canonical member ref.
/// The daemon only requires a well-formed `member:<id>` / `agent:<id>` token (the
/// `comment.author_type` CHECK), so this is accepted as-is; swapping in the real
/// signed-in member is a drop-in change once identity lands.
const SELF_AUTHOR_REF: &str = "member:me";

/// Params for the two inbox RPCs: the workspace plus WHOSE inbox this is.
///
/// Every inbox entry is addressed to exactly one actor (store migration 0060),
/// so the Inbox screen is THE LOCAL HUMAN's inbox, not a workspace-wide feed:
/// the request names [`SELF_AUTHOR_REF`] as the recipient, and the daemon
/// returns / sweeps only that actor's rows. When a real signed-in identity
/// lands, only that constant changes.
/// Render the `@`-mention routing outcomes of one `comment_add` reply as a
/// single transcript line, or `None` when there is nothing to say
/// (multica parity #2-rest).
///
/// Shape: `↪ @alice notified · @builder queued · @bot blocked (invocation not allowed)`.
/// A `blocked` / `deferred` row carries the DispatchReason's human label in
/// parentheses — that parenthetical IS the feature: before this item a refused
/// mention was indistinguishable from one that ran.
///
/// An EMPTY outcome set renders nothing, so a comment with no mentions looks
/// exactly as it did before.
fn render_mention_outcomes(
    rows: &[ainb_hangar_proto::snapshots::MentionOutcomeRow],
) -> Option<String> {
    use ainb_hangar_core::dispatch_reason::DispatchReason;

    if rows.is_empty() {
        return None;
    }
    let parts: Vec<String> = rows
        .iter()
        .map(|r| {
            let who = if r.handle.is_empty() {
                "(unknown)".to_string()
            } else {
                format!("@{}", r.handle)
            };
            // Only a refusal / deferral needs the WHY; `queued` and `notified`
            // already say everything.
            let why = match r.outcome.as_str() {
                "blocked" | "deferred" => DispatchReason::parse(&r.reason)
                    .map(|d| format!(" ({})", d.label()))
                    .unwrap_or_default(),
                _ => String::new(),
            };
            format!("{who} {}{why}", r.outcome)
        })
        .collect();
    Some(format!("↪ {}", parts.join(" · ")))
}

fn inbox_params(ws: &str) -> serde_json::Value {
    serde_json::json!({ "workspace_id": ws, "recipient": SELF_AUTHOR_REF })
}
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
    /// A Fleet event or lag notification requested a focused snapshot refresh.
    fleet_fetch_pending: bool,
    /// The workspace handshake or a lag notification requested a new gapless
    /// Fleet subscription.
    fleet_subscribe_pending: bool,
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
    /// Armed after any attach / detach / toggle reply: the next `render` pass
    /// fires `hangar/agent_skills_list` so the skill rows' ` (disabled)` markers
    /// never go stale (parity #24).
    refresh_agent_skill_links: bool,
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
    /// Set when the user pressed the quit key (`q` → [`Intent::Quit`]) on a
    /// hangar screen. The plugin can't quit the host itself; it publishes a
    /// `ui.close_request` so the host pops this panel back to wherever it was
    /// opened. Like `start_daemon_pending`, the publish awaits a host cap and so
    /// can't run inline in `handle_key` (reader-loop deadlock) — it is drained in
    /// `render`. Without this the router computed `Intent::Quit` and dropped it,
    /// so `q` was dead on every hangar screen (only `Ctrl+C` escaped).
    close_request_pending: bool,
    /// The host's screen-id string for the surface the plugin is currently
    /// focused on, captured from each `plugin/handle_key`. `ui.close_request`
    /// must name the screen to pop, and the SDK `RenderParams` doesn't carry it —
    /// so we stash it here from the key event that armed `close_request_pending`.
    current_screen_id: String,
    /// The message from the last failed `[s]` start attempt (e38.36), surfaced in
    /// the offline empty-state so a start failure is visible rather than silent.
    /// `None` once a start succeeds or while none has been attempted.
    daemon_start_error: Option<String>,
    /// After a successful offline `[s]` spawn: keep re-dialing the daemon socket
    /// until this deadline so the link flips online as soon as the daemon binds,
    /// without another keypress. The daemon needs a moment to boot + bind; the
    /// old single immediate re-dial always lost that race and the offline panel
    /// sat there forever. `None` = no start attempt in flight.
    daemon_start_redial_until: Option<std::time::Instant>,
    /// Last redial attempt inside the window — throttles dials to ~1/s while
    /// `wants_redraw` keeps frames coming.
    daemon_start_last_redial: Option<std::time::Instant>,
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
    /// The Issues create-wizard payload whose `hangar/issue_create` is in flight
    /// (Phase 5): the repo / agent / branches to persist + dispatch once the
    /// create reply hands back the new issue id. `None` when no wizard create is
    /// pending. Cleared on the create's error reply (nothing to dispatch).
    wizard_dispatch_in_flight: Option<WizardDispatch>,
    /// The dispatch armed by a successful wizard `issue_create` reply: the new
    /// `issue_id` plus the stashed payload, awaiting the next `render` pass (host
    /// IO is safe there) to fire `issue_update` + `issue_run`. `None` when idle.
    pending_issue_dispatch: Option<(String, WizardDispatch)>,
    /// The issue whose `hangar/issue_delete` is in flight, stashed so a delete
    /// REFUSED for active tasks can re-target the "cancel run(s) & delete" overlay
    /// at the right issue (the reply carries no issue id). `None` when idle.
    delete_in_flight: Option<ainb_hangar_core::ids::IssueId>,
    /// The issue whose `hangar/issue_cancel_active` is in flight, awaiting its
    /// reply to arm the follow-up `hangar/issue_delete` retry (cancel commits
    /// before the delete). `None` when idle.
    cancel_delete_in_flight: Option<ainb_hangar_core::ids::IssueId>,
}

/// The Issues create-wizard fields that ride ALONGSIDE the `issue_create` call
/// (Phase 5): persisted onto the new issue via `hangar/issue_update` and carried
/// as explicit `hangar/issue_run` overrides, so the dispatch never depends on
/// the persist landing first.
#[derive(Debug, Clone)]
struct WizardDispatch {
    /// The picked repo (REQUIRED): absolute path, `scratch`, or a remote ref.
    repo_ref: String,
    /// The provider agent wire token (`claude` / `codex` / `copilot`) when the
    /// Agent row fell back to provider chips; `None` when a NAMED agent was
    /// targeted (its own provider drives the run — see [`Self::assignee`]).
    agent: Option<String>,
    /// The NAMED workspace agent targeted by the Agent row as its `agent:<id>`
    /// ref (V3-F3): set on the `issue_update` assignee AND carried as the
    /// `issue_run` assignee override, so the dispatch routes to it regardless of
    /// which leg lands first. `None` when a provider chip was chosen instead.
    assignee: Option<String>,
    /// The source branch the run branches FROM; `None` = repo default.
    source_branch: Option<String>,
    /// The target branch a future PR lands INTO; `None` = unset.
    target_branch: Option<String>,
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

/// Open an exact tmux target in a popup owned by the current tmux client.
/// Closing the nested client restores the unchanged Fleet pane state.
fn launch_fleet_tmux_popup(target: &str, fullscreen: bool) -> std::io::Result<()> {
    if std::env::var_os("TMUX").is_none() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotConnected,
            "Fleet attach requires tmux host",
        ));
    }
    let width = if fullscreen { "100%" } else { "90%" };
    let height = if fullscreen { "100%" } else { "90%" };
    let command = fleet_tmux_attach_command(target);
    std::process::Command::new("tmux")
        .args(["display-popup", "-E", "-w", width, "-h", height, &command])
        .spawn()
        .map(|_| ())
}

fn fleet_tmux_attach_command(target: &str) -> String {
    let quoted_target = shell_quote(target);
    let session = target.split_once(':').map_or(target, |(session, _)| session);
    let quoted_session = shell_quote(session);
    if target.contains(':') {
        format!(
            "tmux select-window -t {quoted_target} && tmux select-pane -t {quoted_target} && exec env -u TMUX tmux attach-session -t {quoted_session}"
        )
    } else {
        format!("exec env -u TMUX tmux attach-session -t {quoted_session}")
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn fleet_request_id(kind: &str) -> String {
    format!("fleet-ui-{kind}-{}", uuid::Uuid::new_v4())
}

fn fleet_start_params(
    provider: ainb_hangar_proto::fleet::FleetProvider,
    cwd: String,
    prompt: Option<String>,
) -> ainb_hangar_proto::fleet::FleetStartParams {
    ainb_hangar_proto::fleet::FleetStartParams {
        request_id: fleet_request_id("start"),
        provider,
        cwd,
        prompt,
    }
}

impl Default for HangarPlugin {
    fn default() -> Self {
        Self {
            conn: Connection::default(),
            decoder: FrameDecoder::default(),
            app: None,
            screens: ScreenStates::default(),
            fetch_pending: false,
            fleet_fetch_pending: false,
            fleet_subscribe_pending: false,
            first_run: FirstRunModal::default(),
            first_run_ack_pending: false,
            pending_detail_slug: None,
            refresh_agent_skill_links: false,
            pending_runs_autopilot: None,
            opener: crate::shell::default_opener(),
            daemon_starter: crate::shell::default_daemon_starter(),
            start_daemon_pending: false,
            close_request_pending: false,
            current_screen_id: String::new(),
            daemon_start_error: None,
            daemon_start_redial_until: None,
            daemon_start_last_redial: None,
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
            wizard_dispatch_in_flight: None,
            pending_issue_dispatch: None,
            delete_in_flight: None,
            cancel_delete_in_flight: None,
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
            let _ = host.log_info("hangar: [s] started daemon, re-dialing").await;
            // The daemon needs a beat to boot + bind its socket, so one
            // immediate dial is not enough: arm a bounded redial window the
            // render loop pumps (via `wants_redraw`) until the link flips
            // online or the window expires.
            self.daemon_start_redial_until =
                Some(std::time::Instant::now() + Self::START_REDIAL_WINDOW);
            self.daemon_start_last_redial = Some(std::time::Instant::now());
            self.connect(host).await;
        } else if let Some(msg) = &self.daemon_start_error {
            let _ = host.log_info(format!("hangar: {msg}")).await;
        }
    }

    /// How long `[s]` keeps re-dialing before declaring the start failed.
    const START_REDIAL_WINDOW: std::time::Duration = std::time::Duration::from_secs(15);
    /// Minimum gap between two redial attempts inside the window.
    const START_REDIAL_GAP: std::time::Duration = std::time::Duration::from_secs(1);

    /// Pump the post-`[s]` redial window (called from `render`, where host IO
    /// is safe): while the link is still down and the deadline hasn't passed,
    /// re-dial at most once per [`Self::START_REDIAL_GAP`]. On expiry, surface
    /// a hard error in the offline panel instead of staying silent forever.
    async fn pump_start_redial(&mut self, host: &HostClient) {
        let Some(until) = self.daemon_start_redial_until else {
            return;
        };
        match self.conn.state() {
            // Fully up: the start worked, stand down.
            ConnState::Connected => {
                self.daemon_start_redial_until = None;
                self.daemon_start_last_redial = None;
                return;
            }
            // Mid-dial / mid-handshake: a link attempt is live — keep the
            // window ARMED (so a handshake that dies still gets retried /
            // expires with a visible error) but don't tear the attempt down
            // by dialing over it. Standing down here was the wedge: a
            // handshake that then hung left no panel, no retry, no error.
            ConnState::Dialing | ConnState::Handshake => {
                if std::time::Instant::now() >= until {
                    self.daemon_start_redial_until = None;
                    self.daemon_start_last_redial = None;
                    self.daemon_start_error = Some(
                        "daemon started but the link did not come up — run `ainb hangar daemon status`"
                            .to_string(),
                    );
                }
                return;
            }
            ConnState::Disconnected | ConnState::Error(_) => {}
        }
        if std::time::Instant::now() >= until {
            self.daemon_start_redial_until = None;
            self.daemon_start_last_redial = None;
            self.daemon_start_error = Some(
                "daemon did not come up — run `ainb hangar daemon run` in a terminal to see why"
                    .to_string(),
            );
            let _ = host.log_info("hangar: [s] daemon did not come up within window").await;
            return;
        }
        let due = self
            .daemon_start_last_redial
            .is_none_or(|last| last.elapsed() >= Self::START_REDIAL_GAP);
        if due {
            self.daemon_start_last_redial = Some(std::time::Instant::now());
            self.connect(host).await;
        }
    }

    /// Dial the daemon and send the workspace subscribe. Records the
    /// resulting [`ConnState`] on `self`; transport failures land the
    /// machine in [`ConnState::Error`] (rendered Red) rather than
    /// propagating, so a downed daemon shows a clean footer instead of
    /// crashing the plugin.
    async fn connect(&mut self, host: &HostClient) {
        self.decoder = FrameDecoder::new();
        self.fleet_subscribe_pending = false;
        self.fleet_fetch_pending = false;
        self.conn.dialing();

        let dial = match host.unix_socket_dial(daemon_socket_path()).await {
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
        if let Err(e) = host.unix_socket_send(dial.stream_id.clone(), auth_body).await {
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
        let _ = host.log_info("hangar: dialed daemon, auth + subscribe sent").await;
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
        let method = value.get("method").and_then(serde_json::Value::as_str);
        if method == Some("fleet/event") {
            if let Some(params) = value.get("params") {
                if let Ok(event) =
                    serde_json::from_value::<ainb_hangar_proto::fleet::FleetEvent>(params.clone())
                {
                    self.screens.fleet.observe_revision(event.revision);
                    self.fleet_fetch_pending = true;
                    self.conn.on_event();
                }
            }
            return;
        }
        if method == Some("fleet/resync_required") {
            self.fleet_fetch_pending = true;
            self.fleet_subscribe_pending = true;
            self.conn.on_event();
            return;
        }
        if method != Some(EVENT_METHOD) {
            return;
        }
        let Some(params) = value.get("params") else {
            return;
        };
        let Ok(event) = serde_json::from_value::<HangarEvent>(params.clone()) else {
            return;
        };
        // A transcript line (`TaskMessage`) changes NO snapshot-derived state — the
        // timeline overlay live-appends it locally (F6 logs-tail), so a full
        // `fetch_snapshots` fanout would be pure waste: a chatty streaming run
        // (many lines/sec) would hammer the daemon with a whole snapshot bundle per
        // line. Every OTHER event may move a derived column (task status buckets,
        // issue fields, …), so it still arms the reconciling re-pull.
        let needs_refetch = !matches!(event, HangarEvent::TaskMessage { .. });
        self.apply_hangar_event(event);
        // A pushed event is the steady state — keep the link Connected. Non-
        // transcript events also arm a re-pull so every screen's derived columns
        // reconcile.
        self.conn.on_event();
        if needs_refetch {
            self.fetch_pending = true;
        }
    }

    /// Fold a typed [`HangarEvent`] into the issue-list + Kanban caches so a
    /// pushed mutation (create / update / task lifecycle) re-renders within a
    /// tick, ahead of the reconciling snapshot re-pull (e38.29).
    fn apply_hangar_event(&mut self, event: HangarEvent) {
        use crate::screen::issue_list::{IssueListEvent, reduce_issue_list};
        use crate::screen::kanban::{KanbanEvent, reduce_kanban};
        // F6 logs-tail: a live transcript line for the task whose timeline overlay
        // is open auto-appends to it, so the shown run streams in place (no re-
        // fetch). Events for any other task — or with no timeline open — are ignored.
        if let HangarEvent::TaskMessage {
            task_id,
            kind,
            body,
        } = &event
        {
            self.screens.boards.fold_timeline_message(task_id.as_str(), *kind, body.clone());
        }
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
                    self.conn.on_error(format!("daemon auth rejected: {}", err.message));
                }
            }
            // The subscribe ack completes the handshake and arms the snapshot
            // fetch (issued by `handle_event`, which holds the `host`).
            RpcId::Number(SUBSCRIBE_REQ_ID) => {
                if resp.error.is_some() {
                    self.conn.on_error("daemon rejected workspace/subscribe".to_string());
                } else {
                    self.conn.on_subscribe_ack();
                    self.fetch_pending = true;
                    self.fleet_fetch_pending = true;
                    self.fleet_subscribe_pending = true;
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
            RpcId::Number(BOARDS_REQ_ID) => self.apply_boards(resp),
            RpcId::Number(BOARD_CARD_RUN_REQ_ID) => self.apply_board_card_run(resp),
            RpcId::Number(BOARD_CARD_CANCEL_REQ_ID) => self.apply_board_card_cancel(resp),
            RpcId::Number(BOARD_CARD_TIMELINE_REQ_ID) => self.apply_board_card_timeline(resp),
            RpcId::Number(SQUADS_LIST_REQ_ID) => self.apply_squads(resp),
            // The `n` create-agent reply folds the refreshed roster into the cached
            // actors (clearing the squad gate live) and surfaces a note on the pane.
            RpcId::Number(AGENT_CREATE_REQ_ID) => {
                self.apply_agents(resp);
                if let Some(e) = &resp.error {
                    self.screens.squads.note_err(format!("agent create failed: {}", e.message));
                    // The create can be raised from EITHER pane, so the refusal
                    // (e.g. migration 0050's duplicate name) must also land on the
                    // Agents screen — otherwise the wizard closes and nothing at
                    // all is said about why no agent appeared.
                    self.screens.agents.note_err(e.message.clone());
                } else {
                    self.screens.squads.note_ok("agent created");
                }
                self.conn.on_event();
            }
            // The Agents roster `x` delete reply folds the shrunk roster back
            // through the same actor cache; an error (active tasks / FK-pinned
            // history) surfaces on the Agents pane so the refusal is never silent.
            RpcId::Number(AGENT_DELETE_REQ_ID) => {
                if let Some(e) = &resp.error {
                    self.screens
                        .agents
                        .note_err(format!("delete failed: {}", e.message));
                } else {
                    self.apply_agents(resp);
                }
                self.conn.on_event();
            }
            // multica parity #2-rest: the `comment_add` reply now carries one
            // outcome row per `@`-mention. Before this the reply was dropped on
            // the floor, so a refused or coalesced mention looked exactly like
            // one that ran.
            RpcId::Number(COMMENT_ADD_REQ_ID) => self.apply_comment_mention_outcomes(resp),
            RpcId::Number(SQUAD_FANOUT_REQ_ID) => self.apply_squad_fanout(resp),
            RpcId::Number(DAEMON_HEALTH_REQ_ID) => self.apply_daemon_health(resp),
            RpcId::Number(USAGE_ROLLUP_REQ_ID) => self.apply_usage(resp),
            RpcId::Number(RUN_HISTORY_REQ_ID) => self.apply_run_history(resp),
            RpcId::Number(PR_STATUS_REFRESH_REQ_ID) => self.apply_pr_status(resp),
            RpcId::Number(ISSUE_TIMELINE_REQ_ID) => self.apply_issue_timeline(resp),
            RpcId::Number(MEMBERS_REQ_ID) => self.apply_members(resp),
            // tcp T5: the notification routing grid snapshot.
            RpcId::Number(NOTIFY_RULES_REQ_ID) => self.apply_notify_rules(resp),
            // A rule set reply re-fetches the grid so the pane reflects the write,
            // in the SAME scope the write targeted (agents-in-a-box-cqh).
            RpcId::Number(NOTIFY_RULE_SET_REQ_ID) => {
                let scope = self.screens.notify_scope();
                self.screens.pending_notify_action =
                    Some(crate::screen::app_screens::NotifyAction::Refresh { scope });
                self.conn.on_event();
            }
            // Every daemon-config knob's live value for the Settings pane.
            RpcId::Number(DAEMON_CONFIG_LIST_REQ_ID) => self.apply_daemon_config_list(resp),
            RpcId::Number(DAEMON_CONFIG_SET_REQ_ID) => {
                // Re-read the whole config after the write so the pane reflects the
                // persisted value (reconciling the optimistic edit).
                self.fetch_pending = true;
                self.conn.on_event();
            }
            // Phase 5: the wizard's `issue_create` reply hands back the new
            // issue's id, arming the `issue_update` + `issue_run` follow-ups.
            RpcId::Number(ISSUE_CREATE_REQ_ID) => self.apply_wizard_issue_created(resp),
            // Phase 5: the wizard's `issue_run` reply — surfaced as a note either
            // way (launch feedback or the daemon's rejection), never silent.
            RpcId::Number(ISSUE_RUN_REQ_ID) => self.apply_issue_run(resp),
            // 63d: the `x` delete reply. On success the daemon's IssueDeleted push
            // already dropped the row, so nothing to fold; an error (e.g. an active
            // task) surfaces as an issue-list note, never silent.
            RpcId::Number(ISSUE_DELETE_REQ_ID) => {
                let target = self.delete_in_flight.take();
                if let Some(e) = &resp.error {
                    // A delete refused because the issue still has active run(s)
                    // carries `data.reason = "active_tasks"`: offer the inline
                    // "cancel run(s) & delete" instead of dead-ending on the text.
                    let is_active_tasks = e
                        .data
                        .as_ref()
                        .and_then(|d| d.get("reason"))
                        .and_then(serde_json::Value::as_str)
                        == Some("active_tasks");
                    match (is_active_tasks, target) {
                        (true, Some(id)) => self
                            .screens
                            .issue_list
                            .open_confirm_cancel_delete_for(id.as_str()),
                        _ => self.screens.issue_list.set_note(format!("delete failed: {}", e.message)),
                    }
                }
                self.conn.on_event();
            }
            // The board-less cancel-active reply: on success retry the delete
            // (cancel has committed server-side); on error surface a note and do
            // NOT delete.
            RpcId::Number(ISSUE_CANCEL_ACTIVE_REQ_ID) => {
                let target = self.cancel_delete_in_flight.take();
                if let Some(e) = &resp.error {
                    self.screens.issue_list.set_note(format!("cancel failed: {}", e.message));
                } else if let Some(id) = target {
                    // Retry the delete now the run(s) are cancelled — armed as a
                    // pending action the render pass drains + fires.
                    self.screens.pending_delete_action = Some(id);
                }
                self.conn.on_event();
            }
            RpcId::Number(INBOX_LIST_REQ_ID) => self.apply_inbox(resp),
            // The attention/subscribe ack carries the open-attention snapshot that
            // seeds the control-center board.
            RpcId::Number(ATTENTION_SUBSCRIBE_REQ_ID) => self.apply_attention(resp),
            RpcId::Number(FLEET_SNAPSHOT_REQ_ID) => self.apply_fleet_snapshot(resp),
            RpcId::Number(FLEET_SUBSCRIBE_REQ_ID) => self.apply_fleet_subscription(resp),
            RpcId::Number(FLEET_ACTION_REQ_ID) => self.apply_fleet_action_result(resp),
            RpcId::Number(FLEET_BROADCAST_REQ_ID) => self.apply_fleet_broadcast_result(resp),
            RpcId::Number(FLEET_START_REQ_ID) => self.apply_fleet_start_result(resp),
            RpcId::Number(SEARCH_REQ_ID) => self.apply_search(resp),
            // P5: the profile-editor roster + the per-selection detail/previews.
            RpcId::Number(PROFILE_LIST_REQ_ID) => self.apply_profiles(resp),
            RpcId::Number(PROFILE_GET_REQ_ID) => self.apply_profile_detail(resp),
            // F3: the card-create `@` autocomplete repo roster.
            RpcId::Number(REPO_LIST_REQ_ID) => self.apply_repos(resp),
            // Mutating RPCs (skill sync/attach/detach, autopilot fire/toggle,
            // kanban task transition, issue assign, inbox mark-read) answer with
            // the changed row or `{}`; we re-fetch the relevant lists to refresh
            // derived columns (`used`, next-tick, enabled, last-run, task status
            // buckets, issue assignee, inbox unread count).
            // Parity #24: the link listing feeds the ` (disabled)` marker.
            RpcId::Number(AGENT_SKILLS_LIST_REQ_ID) => self.apply_agent_skill_links(resp),
            // Parity #24: a toggle changes only the per-agent link, so refresh
            // the link map rather than the whole snapshot batch.
            RpcId::Number(SKILL_SET_ENABLED_REQ_ID) => self.refresh_agent_skill_links = true,
            RpcId::Number(
                SKILLS_SYNC_REQ_ID
                | SKILL_ATTACH_REQ_ID
                | SKILL_DETACH_REQ_ID
                | AUTOPILOT_FIRE_REQ_ID
                | AUTOPILOT_TOGGLE_REQ_ID
                | TASK_TRANSITION_REQ_ID
                | ISSUE_UPDATE_REQ_ID
                | INBOX_MARK_READ_REQ_ID
                | ATTENTION_ANSWER_REQ_ID
                // P5: a profile/upsert reply re-fetches the snapshot batch so the
                // roster row reflects the new tier; the detail re-fetch is armed
                // separately in the render drain (both previews re-resolve).
                | PROFILE_UPSERT_REQ_ID,
            ) => {
                self.fetch_pending = true;
                // Attach / detach change the link SET for the selected agent, so
                // the enablement map must be re-read too (parity #24).
                self.refresh_agent_skill_links = true;
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
    /// (e38.11) — members AND the live pending invitations that ride on the same
    /// envelope (parity #18). The pane is render-only, so the rows are simply
    /// cached.
    fn apply_members(&mut self, resp: &RpcResponse) {
        if let Some(result) = &resp.result {
            if let Ok(r) = serde_json::from_value::<ainb_hangar_proto::snapshots::MembersListResult>(
                result.clone(),
            ) {
                self.screens.set_members(r.members);
                self.screens.set_pending_invites(r.pending_invites);
            }
        }
    }

    /// Populate the Settings Notifications grid from a `hangar/notify_rules_list`
    /// result (tcp T5): one routing row per attention kind for the active
    /// workspace (override where set, global otherwise).
    fn apply_notify_rules(&mut self, resp: &RpcResponse) {
        if let Some(result) = &resp.result {
            if let Ok(r) = serde_json::from_value::<
                ainb_hangar_proto::snapshots::NotifyRulesListResult,
            >(result.clone())
            {
                // Drop a STALE-SCOPE reply (agents-in-a-box-cqh): if `g` flipped the
                // grid's edit scope while this list was in flight, the reply answers
                // the scope we just LEFT — applying it would briefly repopulate the
                // grid with the wrong scope's rows before the in-scope reply lands.
                // The result echoes the scope it answered; keep it only when that
                // matches the grid's CURRENT scope.
                let ws_id = self.app_state().ws_id.as_str().to_string();
                if crate::screen::settings::notify_reply_matches_scope(
                    self.screens.notify_scope(),
                    &ws_id,
                    r.workspace_id.as_deref(),
                ) {
                    self.screens.set_notify_rules(r.rules);
                }
            }
        }
        self.conn.on_event();
    }

    /// Populate the Settings Daemon-section config rows from a
    /// `hangar/daemon_config_list` result: every knob's persisted value (or `None`
    /// for an unset knob, where the pane shows the descriptor's coded default).
    fn apply_daemon_config_list(&mut self, resp: &RpcResponse) {
        if let Some(result) = &resp.result {
            if let Ok(r) = serde_json::from_value::<
                ainb_hangar_proto::snapshots::DaemonConfigListResult,
            >(result.clone())
            {
                let entries = r.entries.into_iter().map(|e| (e.key, e.value)).collect::<Vec<_>>();
                self.screens.set_daemon_config_entries(entries);
            }
        }
        self.conn.on_event();
    }

    /// Fire a deferred notify-rule RPC (tcp T5) over the daemon socket, scoped to
    /// the grid's current GLOBAL/WORKSPACE scope (agents-in-a-box-cqh): a `Refresh`
    /// fetches the grid, a `Set` upserts one rule (whose reply re-fetches the
    /// grid). In GLOBAL scope the RPC omits `workspace_id` so it targets the
    /// host-wide default rule — the one HOOK-raised attentions resolve; in
    /// WORKSPACE scope it sends the active workspace so it writes that workspace's
    /// override. Best-effort — a send failure is logged, never fatal (the grid
    /// simply keeps its prior rows).
    async fn apply_notify_action(
        &mut self,
        host: &HostClient,
        action: crate::screen::app_screens::NotifyAction,
    ) {
        use crate::screen::app_screens::NotifyAction;
        use crate::screen::settings::NotifyScope;
        let Some(stream_id) = self.conn.stream_id().map(ToString::to_string) else {
            return;
        };
        let ws = self.app_state().ws_id.as_str().to_string();
        // Only the WORKSPACE scope threads a `workspace_id`; GLOBAL omits it so the
        // daemon resolves the host-wide rule (`workspace_id IS NULL`).
        let scope_of = |scope: NotifyScope| -> Option<&str> {
            match scope {
                NotifyScope::Global => None,
                NotifyScope::Workspace => Some(ws.as_str()),
            }
        };
        let (id, method, params) = match action {
            NotifyAction::Refresh { scope } => {
                let mut params = serde_json::Map::new();
                if let Some(w) = scope_of(scope) {
                    params.insert("workspace_id".into(), serde_json::json!(w));
                }
                (
                    NOTIFY_RULES_REQ_ID,
                    daemon_methods::HANGAR_NOTIFY_RULES_LIST,
                    serde_json::Value::Object(params),
                )
            }
            NotifyAction::Set {
                scope,
                kind,
                channels,
            } => {
                let mut params = serde_json::Map::new();
                if let Some(w) = scope_of(scope) {
                    params.insert("workspace_id".into(), serde_json::json!(w));
                }
                params.insert("kind".into(), serde_json::json!(kind));
                params.insert("channels".into(), serde_json::json!(channels));
                (
                    NOTIFY_RULE_SET_REQ_ID,
                    daemon_methods::HANGAR_NOTIFY_RULE_SET,
                    serde_json::Value::Object(params),
                )
            }
        };
        let Ok(body) = encode_request(id, method, params) else {
            return;
        };
        if let Err(e) = host.unix_socket_send(stream_id, body).await {
            let _ = host.log_info(format!("hangar: notify rpc send failed: {e}")).await;
        }
    }

    /// Populate the Inbox screen from a `hangar/inbox_list` result (e38.14): the
    /// aggregated issue/comment/task entries + the unread count for the badge.
    fn apply_inbox(&mut self, resp: &RpcResponse) {
        if let Some(result) = &resp.result {
            if let Ok(r) = serde_json::from_value::<ainb_hangar_proto::snapshots::InboxListResult>(
                result.clone(),
            ) {
                // The snapshot was requested for the local human, so the screen
                // is tagged with the actor it belongs to.
                self.screens.set_inbox(r.entries, r.unread, SELF_AUTHOR_REF.to_string());
            }
        }
    }

    /// Populate the control-center board from an `attention/subscribe` ack (P2):
    /// the open [`AttentionRow`]s the board renders. `set_attention` preserves the
    /// human's focus + option cursor across the auto-shuffle, so a fresh push
    /// never yanks the selection off the card they were reading.
    fn apply_attention(&mut self, resp: &RpcResponse) {
        if let Some(result) = &resp.result {
            if let Ok(r) = serde_json::from_value::<
                ainb_hangar_proto::snapshots::AttentionSubscribeResult,
            >(result.clone())
            {
                self.screens.set_attention(&r.attention);
            }
        }
    }

    /// Populate the profile-editor roster from a `profile/list` result (P5): the
    /// indexed profiles (slug + tier), slug-ordered.
    fn apply_profiles(&mut self, resp: &RpcResponse) {
        if let Some(result) = &resp.result {
            if let Ok(r) = serde_json::from_value::<ainb_hangar_proto::snapshots::ProfileListResult>(
                result.clone(),
            ) {
                // Mirror the slugs into the Boards card-create picker roster
                // (ccc / D16) before moving `r.profiles` into the editor rows.
                let slugs = r.profiles.iter().map(|p| p.slug.clone()).collect();
                self.screens.set_boards_profiles(slugs);
                let rows = r
                    .profiles
                    .into_iter()
                    .map(|p| crate::screen::profiles::ProfileRosterEntry {
                        slug: p.slug,
                        tier: p.tier,
                    })
                    .collect();
                self.screens.set_profiles(rows);
            }
        }
    }

    /// Populate the Boards card-create `@` autocomplete roster from a
    /// `hangar/repo_list` result (spec F3).
    ///
    /// The daemon returns favorites-first + recency order already; the plugin
    /// preserves it and maps each row to a pickable [`RepoOption`]. A row with a
    /// local checkout `path` persists that path as its `repo_ref`. A remote-only
    /// favorite (bead pv8 — no local path but a `remote` indicator) is NO LONGER
    /// dropped: it persists its `remote` as the `repo_ref` and is flagged
    /// `is_remote_only`, so the picker renders ★☁ and the daemon clones it on
    /// card-create. A row with neither a path nor a remote is unprovisionable and
    /// skipped (`scratch` is prepended by the reducer regardless).
    fn apply_repos(&mut self, resp: &RpcResponse) {
        if let Some(result) = &resp.result {
            if let Ok(r) = serde_json::from_value::<ainb_hangar_proto::snapshots::RepoListResult>(
                result.clone(),
            ) {
                let repos = r
                    .repos
                    .into_iter()
                    .filter_map(|row| match (row.path, row.remote) {
                        (Some(path), _) => Some(crate::screen::boards::RepoOption {
                            label: row.name,
                            repo_ref: path,
                            is_favorite: row.is_favorite,
                            is_remote_only: false,
                        }),
                        (None, Some(remote)) => Some(crate::screen::boards::RepoOption {
                            label: row.name,
                            repo_ref: remote,
                            is_favorite: row.is_favorite,
                            is_remote_only: true,
                        }),
                        // No path and no remote: nothing to provision from.
                        (None, None) => None,
                    })
                    .collect();
                self.screens.set_boards_repos(repos);
            }
        }
    }

    /// Surface a `hangar/board_card_run` reply (ccc / D6): a transient note naming
    /// the agent + mode the card launched on, or the daemon's rejection. The
    /// enqueued task runs headless via the claim loop; the D8 auto-move hook then
    /// slides the card as its FSM transitions (no board refresh needed here).
    fn apply_board_card_run(&mut self, resp: &RpcResponse) {
        if let Some(err) = &resp.error {
            self.screens.boards.set_note(format!("run failed: {}", err.message));
            return;
        }
        if let Some(result) = &resp.result {
            if let Ok(r) = serde_json::from_value::<ainb_hangar_proto::snapshots::BoardCardRunResult>(
                result.clone(),
            ) {
                self.screens.boards.set_note(format!("launched {} on {}", r.mode, r.agent_id));
            }
        }
    }

    /// Fold the wizard's `hangar/issue_create` reply (Phase 5): a success carries
    /// the new `IssueRow`, whose id + the stashed
    /// [`Self::wizard_dispatch_in_flight`] arm the `issue_update` + `issue_run`
    /// follow-ups (fired on the next `render`, where host IO is safe). An error —
    /// or an unparseable reply — surfaces as an issue-list note and DROPS the
    /// stash (nothing was created, nothing to dispatch). Either way the snapshot
    /// re-pull is armed so the board reflects the daemon's truth.
    fn apply_wizard_issue_created(&mut self, resp: &RpcResponse) {
        let dispatch = self.wizard_dispatch_in_flight.take();
        if let Some(err) = &resp.error {
            self.screens.issue_list.set_note(format!("create failed: {}", err.message));
        } else if let Some(row) = resp.result.as_ref().and_then(|result| {
            serde_json::from_value::<ainb_hangar_proto::events::IssueRow>(result.clone()).ok()
        }) {
            if let Some(dispatch) = dispatch {
                self.pending_issue_dispatch = Some((row.id.as_str().to_string(), dispatch));
            }
        } else {
            // A result that isn't an IssueRow: the issue may exist but its id is
            // unknown, so the dispatch cannot fire. Say so rather than sit quiet.
            self.screens
                .issue_list
                .set_note("create reply unreadable — issue not dispatched");
        }
        self.fetch_pending = true;
        self.conn.on_event();
    }

    /// Surface the wizard's `hangar/issue_run` reply (Phase 5): a transient
    /// issue-list note naming the agent + mode the task launched on, or the
    /// daemon's rejection (e.g. the copilot F8 dispatch gate) — mirroring
    /// [`Self::apply_board_card_run`], never silent. The card slides Todo → In
    /// Progress via the daemon's pushed task lifecycle events.
    fn apply_issue_run(&mut self, resp: &RpcResponse) {
        if let Some(err) = &resp.error {
            self.screens.issue_list.set_note(format!("run failed: {}", err.message));
        } else if let Some(r) = resp.result.as_ref().and_then(|result| {
            serde_json::from_value::<ainb_hangar_proto::snapshots::BoardCardRunResult>(
                result.clone(),
            )
            .ok()
        }) {
            self.screens
                .issue_list
                .set_note(format!("launched {} on {}", r.mode, r.agent_id));
        }
        self.fetch_pending = true;
        self.conn.on_event();
    }

    /// Fold the `@`-mention routing outcomes off a `comment_add` reply into the
    /// open task-detail transcript (multica parity #2-rest).
    ///
    /// Renders ONE line, e.g.
    /// `↪ @alice notified · @builder queued · @secret-bot blocked (invocation not allowed)`.
    /// A comment that mentioned nobody produces an empty vector and therefore
    /// renders NOTHING, so a plain comment looks exactly as it did before.
    fn apply_comment_mention_outcomes(&mut self, resp: &RpcResponse) {
        let Some(result) = resp.result.as_ref() else {
            return;
        };
        let Ok(parsed) = serde_json::from_value::<ainb_hangar_proto::snapshots::CommentAddResult>(
            result.clone(),
        ) else {
            return;
        };
        if let Some(line) = render_mention_outcomes(&parsed.mention_outcomes) {
            self.screens.push_task_detail_system_line(line);
            self.conn.on_event();
        }
    }

    /// Surface a `hangar/board_card_cancel` reply (tcp T3 / F6): a transient note
    /// confirming the cancel, or reporting that the card had no active run to
    /// cancel. The card leaves the running state via the daemon's pushed
    /// `TaskFinished(Cancelled)` (no board refresh needed here).
    fn apply_board_card_cancel(&mut self, resp: &RpcResponse) {
        if let Some(err) = &resp.error {
            self.screens.boards.set_note(format!("cancel failed: {}", err.message));
            return;
        }
        if let Some(result) = &resp.result {
            if let Ok(r) = serde_json::from_value::<
                ainb_hangar_proto::snapshots::BoardCardCancelResult,
            >(result.clone())
            {
                let note = if r.cancelled {
                    "cancelled the running task".to_string()
                } else {
                    "no active run to cancel (already finished?)".to_string()
                };
                self.screens.boards.set_note(note);
            }
        }
    }

    /// Surface a `hangar/board_card_timeline` reply (tcp T3 / F6, P10 §4.9): parse
    /// the returned raw stream-json into the prettied transcript taxonomy and open
    /// the scrollable timeline overlay over the card. A daemon rejection, or a card
    /// that never ran (empty transcript), surfaces a note instead of an overlay so
    /// the key never dead-ends.
    fn apply_board_card_timeline(&mut self, resp: &RpcResponse) {
        if let Some(err) = &resp.error {
            self.screens.boards.set_note(format!("timeline failed: {}", err.message));
            return;
        }
        let Some(result) = &resp.result else {
            return;
        };
        let Ok(r) = serde_json::from_value::<ainb_hangar_proto::snapshots::BoardCardTimelineResult>(
            result.clone(),
        ) else {
            return;
        };
        let entries = crate::widgets::jsonl_timeline::parse_timeline(&r.jsonl);
        // A card that NEVER ran (no task) with no transcript: a note, not an empty
        // overlay. But a run that HAS started (task present) yet not emitted its
        // first JSONL line still opens the overlay with zero entries, so the F6
        // live-tail can begin appending as lines stream in — refusing here would
        // strand a just-launched run's timeline permanently empty.
        if entries.is_empty() && r.task_id.is_none() {
            self.screens.boards.set_note("no run transcript yet — launch this card first");
            return;
        }
        let provider = r.provider.as_deref().unwrap_or("run");
        let title = format!("Timeline · {provider}");
        // Carry the run's task id so live `TaskMessage` events for THIS task
        // auto-append to the overlay while the run is in flight (F6 logs-tail).
        self.screens.boards.set_timeline(crate::screen::boards::TimelineView::new(
            title, r.task_id, entries,
        ));
    }

    /// Fold a `profile/get` result into the selected profile's detail (P5): the
    /// parsed fields + BOTH compile previews (Claude lossless, Codex lossy +
    /// dropped-field warnings). A not-found result (`found = false`) is ignored.
    fn apply_profile_detail(&mut self, resp: &RpcResponse) {
        if let Some(result) = &resp.result {
            if let Ok(r) = serde_json::from_value::<ainb_hangar_proto::snapshots::ProfileGetResult>(
                result.clone(),
            ) {
                if !r.found {
                    return;
                }
                self.screens.set_profile_detail(crate::screen::profiles::ProfileDetailView {
                    slug: r.slug,
                    description: r.description,
                    tier: r.tier,
                    tools: r.tools,
                    color: r.color,
                    body: r.body,
                    claude_preview: r.claude_preview,
                    codex_fragment: r.codex_preview.config_fragment,
                    codex_prompt: r.codex_preview.prompt,
                    codex_warnings: r.codex_preview.warnings,
                });
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

    /// Populate the user-defined Boards screen from a `hangar/boards_list` result
    /// (or any `hangar/board_*` mutation reply, which returns the same refreshed
    /// envelope) (P4 / D8).
    fn apply_boards(&mut self, resp: &RpcResponse) {
        // A rejected fetch/mutation is an error state, not "no boards" — surface it
        // so the render never invites a create over a daemon failure (P4 / D8).
        if let Some(err) = &resp.error {
            self.screens.set_boards_error(format!("daemon error: {}", err.message));
            return;
        }
        match resp.result.as_ref() {
            Some(result) => {
                match serde_json::from_value::<ainb_hangar_proto::snapshots::BoardsListResult>(
                    result.clone(),
                ) {
                    Ok(r) => self.screens.set_boards(&r),
                    Err(e) => {
                        self.screens.set_boards_error(format!("malformed boards payload: {e}"))
                    }
                }
            }
            None => self.screens.set_boards_error("empty boards reply".to_string()),
        }
    }

    /// Populate the Squads screen from a `hangar/squads_list` result (or any
    /// `hangar/squad_create` / `squad_member_add` / `squad_member_remove` mutation
    /// reply, which returns the same refreshed envelope) (P7 / D17). A malformed /
    /// error reply is logged but non-fatal — the screen keeps its last-good rows.
    fn apply_squads(&mut self, resp: &RpcResponse) {
        // A rejected mutation (e.g. a duplicate squad name) surfaces as a note
        // above the list rather than blanking the last-good rows.
        if let Some(err) = &resp.error {
            self.screens.squads.note_err(format!("squad error: {}", err.message));
            return;
        }
        if let Some(result) = resp.result.as_ref() {
            if let Ok(r) = serde_json::from_value::<ainb_hangar_proto::snapshots::SquadsListResult>(
                result.clone(),
            ) {
                self.screens.set_squads(&r);
            }
        }
    }

    /// Fold a `hangar/squad_fanout` reply into the Squads screen's transient note
    /// (P7): a success surfaces "briefed <leader> + N member(s)" above the list; an
    /// error surfaces the rejection reason. Non-fatal either way.
    fn apply_squad_fanout(&mut self, resp: &RpcResponse) {
        if let Some(err) = &resp.error {
            self.screens.squads.note_err(format!("assign failed: {}", err.message));
            return;
        }
        if let Some(result) = resp.result.as_ref() {
            if let Ok(r) = serde_json::from_value::<ainb_hangar_proto::snapshots::SquadFanoutResult>(
                result.clone(),
            ) {
                let n = r.members.len();
                self.screens.squads.note_ok(format!(
                    "briefed {} + {n} member{}",
                    r.leader.leader_agent_id,
                    if n == 1 { "" } else { "s" }
                ));
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
                // Scope by the CURRENT active workspace so a rollup landing after a
                // `SetActive` switch resets a stale prior-tenant run-history timeline
                // rather than rendering beside it (cross-workspace stale-data leak).
                let ws = self.app_state().ws_id.as_str().to_string();
                self.screens.set_usage(&ws, rollup);
            }
        }
    }

    /// Populate the usage dashboard's recent-runs timeline from a
    /// `hangar/run_history` result (P10 / D19): the newest-first run rows.
    fn apply_run_history(&mut self, resp: &RpcResponse) {
        if let Some(result) = &resp.result {
            if let Ok(history) = serde_json::from_value::<
                ainb_hangar_proto::snapshots::RunHistoryResult,
            >(result.clone())
            {
                // Scope by the CURRENT active workspace so a run-history reply
                // landing after a `SetActive` switch resets stale prior-tenant
                // totals rather than rendering beside them.
                let ws = self.app_state().ws_id.as_str().to_string();
                self.screens.set_run_history(&ws, history);
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
        // The sweep names the local human: it must never clear an agent's rows.
        let params = inbox_params(&ws);
        let Ok(body) = encode_request(
            INBOX_MARK_READ_REQ_ID,
            daemon_methods::HANGAR_INBOX_MARK_READ,
            params,
        ) else {
            return;
        };
        if let Err(e) = host.unix_socket_send(stream_id, body).await {
            let _ = host.log_info(format!("hangar: inbox mark-read send failed: {e}")).await;
        }
    }

    /// Fire a deferred `attention/answer` raised by the control-center screen
    /// (P2): deliver the picked option label to the raising session of
    /// `attention_id`, framed over the socket cap. `answered_by = "tui"` tags the
    /// surface; `is_answer = true` marks it a safety-critical interview answer so
    /// the daemon refuses (rather than mis-routes) on an ambiguous target (C1). The
    /// mutating reply re-fetches the fleet-wide attention list so the answered card
    /// drops off the board. A send failure is logged but non-fatal.
    async fn answer_attention(
        &self,
        host: &HostClient,
        action: crate::screen::AttentionAnswerAction,
    ) {
        let Some(stream_id) = self.conn.stream_id().map(ToString::to_string) else {
            return;
        };
        let params = serde_json::json!({
            "attention_id": action.attention_id,
            "answer": action.answer,
            "answered_by": "tui",
            "is_answer": true,
        });
        let Ok(body) = encode_request(
            ATTENTION_ANSWER_REQ_ID,
            daemon_methods::ATTENTION_ANSWER,
            params,
        ) else {
            return;
        };
        if let Err(e) = host.unix_socket_send(stream_id, body).await {
            let _ = host.log_info(format!("hangar: attention answer send failed: {e}")).await;
        }
    }

    /// Fire a `profile/get` for `slug` (P5) — the profile-editor selection moved to
    /// a row whose detail + previews are not yet loaded.
    async fn fetch_profile_detail(&self, host: &HostClient, slug: String) {
        let Some(stream_id) = self.conn.stream_id().map(ToString::to_string) else {
            return;
        };
        let Ok(body) = encode_request(
            PROFILE_GET_REQ_ID,
            daemon_methods::PROFILE_GET,
            serde_json::json!({ "slug": slug }),
        ) else {
            return;
        };
        if let Err(e) = host.unix_socket_send(stream_id, body).await {
            let _ = host.log_info(format!("hangar: profile get send failed: {e}")).await;
        }
    }

    /// Fire a `profile/upsert` persisting the selected profile's new `tier` (P5) —
    /// raised by the editor `t` cycle. The daemon's `profile/upsert` OVERWRITES
    /// the master from the params, so the upsert MUST carry every field: the
    /// editor cycles tier against the ALREADY-LOADED detail and re-sends the full
    /// master, a lossless round-trip of the current profile.
    ///
    /// Safety: if the detail has not loaded (`detail_for_upsert` is `None`), this
    /// is a NO-OP send — a slug+tier-only upsert would let the daemon default the
    /// other fields to empty and wipe the master's body. `cycle_tier` already
    /// refuses to raise the intent in that window; this guard is defence in depth.
    async fn upsert_profile_tier(&self, host: &HostClient, slug: String, tier: String) {
        let Some(stream_id) = self.conn.stream_id().map(ToString::to_string) else {
            return;
        };
        // Only persist a full-master round-trip. Without loaded detail we refuse
        // to send rather than risk overwriting the master with empty fields.
        let Some(d) = self.screens.profiles.detail_for_upsert(&slug) else {
            let _ = host
                .log_info(format!(
                    "hangar: profile upsert skipped for {slug:?} — detail not loaded (would drop fields)"
                ))
                .await;
            return;
        };
        let params = serde_json::json!({
            "slug": d.slug,
            "description": d.description,
            "tier": tier,
            "tools": d.tools,
            "color": d.color,
            "body": d.body,
        });
        let Ok(body) = encode_request(
            PROFILE_UPSERT_REQ_ID,
            daemon_methods::PROFILE_UPSERT,
            params,
        ) else {
            return;
        };
        if let Err(e) = host.unix_socket_send(stream_id, body).await {
            let _ = host.log_info(format!("hangar: profile upsert send failed: {e}")).await;
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
            let _ = host.log_info(format!("hangar: search send failed: {e}")).await;
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

    /// Fold a `hangar/agent_skills_list` result onto the skill-manager screen
    /// (parity #24): the selected agent's links plus their per-agent enablement.
    fn apply_agent_skill_links(&mut self, resp: &RpcResponse) {
        let Some(result) = &resp.result else {
            return;
        };
        let Ok(listing) = serde_json::from_value::<
            ainb_hangar_proto::snapshots::AgentSkillsListResult,
        >(result.clone()) else {
            return;
        };
        let out = crate::screen::skill_manager::reduce_skill_manager(
            &self.screens.skill_manager,
            crate::screen::skill_manager::SkillManagerEvent::LinksLoaded(listing.links),
        );
        self.screens.skill_manager = out.state;
    }

    /// Fire `hangar/agent_skills_list` for the currently-targeted agent when a
    /// preceding attach / detach / toggle armed the refresh (parity #24).
    ///
    /// A no-op when nothing armed it, when the socket is down, or when the
    /// workspace has no agent — exactly the guards the mutations themselves use.
    async fn drain_agent_skill_links_refresh(&mut self, host: &HostClient) {
        if !self.refresh_agent_skill_links {
            return;
        }
        self.refresh_agent_skill_links = false;
        let Some(stream_id) = self.conn.stream_id().map(ToString::to_string) else {
            return;
        };
        let Some(agent) = self.first_agent_ref() else {
            return;
        };
        let ws = self.app_state().ws_id.as_str().to_string();
        let Ok(body) = encode_request(
            AGENT_SKILLS_LIST_REQ_ID,
            daemon_methods::HANGAR_AGENT_SKILLS_LIST,
            serde_json::json!({ "workspace_id": ws, "agent_id": agent }),
        ) else {
            return;
        };
        if let Err(e) = host.unix_socket_send(stream_id, body).await {
            let _ = host.log_info(format!("hangar: agent_skills_list send failed: {e}")).await;
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

    /// Replace Fleet pane rows from one authoritative snapshot while preserving
    /// selection by stable session key.
    fn apply_fleet_snapshot(&mut self, resp: &RpcResponse) {
        let Some(result) = &resp.result else {
            return;
        };
        let Ok(snapshot) =
            serde_json::from_value::<ainb_hangar_proto::fleet::FleetSnapshot>(result.clone())
        else {
            return;
        };
        let rows = snapshot.sessions.into_iter().map(Into::into).collect();
        self.screens.fleet.apply_snapshot(snapshot.head_revision, rows);
        self.conn.on_event();
    }

    /// Seed Fleet from the race-free subscribe acknowledgement, then record all
    /// replay revisions. Live events trigger focused snapshot reconciliation.
    fn apply_fleet_subscription(&mut self, resp: &RpcResponse) {
        if let Some(error) = &resp.error {
            let out = crate::screen::fleet::reduce_fleet(
                &self.screens.fleet,
                crate::screen::fleet::FleetEvent::ActionFailed {
                    session_key: "fleet".into(),
                    detail: format!("subscribe failed: {}", error.message),
                },
            );
            self.screens.fleet = out.state;
            return;
        }
        let Some(result) = &resp.result else {
            return;
        };
        let Ok(subscription) = serde_json::from_value::<
            ainb_hangar_proto::fleet::FleetSubscribeResult,
        >(result.clone()) else {
            return;
        };
        let rows = subscription.snapshot.sessions.into_iter().map(Into::into).collect();
        self.screens.fleet.apply_snapshot(subscription.snapshot.head_revision, rows);
        for event in subscription.replay {
            self.screens.fleet.observe_revision(event.revision);
        }
        self.conn.on_event();
    }

    fn apply_fleet_action_result(&mut self, resp: &RpcResponse) {
        use ainb_hangar_proto::fleet::{ActionReceiptStatus, FleetActionResult};
        let result = resp
            .result
            .as_ref()
            .and_then(|value| serde_json::from_value::<FleetActionResult>(value.clone()).ok());
        let event = match (result, &resp.error) {
            (Some(result), _) if result.receipt.status == ActionReceiptStatus::Delivered => {
                crate::screen::fleet::FleetEvent::ActionSucceeded {
                    session_key: result.receipt.session_key,
                }
            }
            (Some(result), _) => crate::screen::fleet::FleetEvent::ActionFailed {
                session_key: result.receipt.session_key,
                detail: result
                    .receipt
                    .detail
                    .unwrap_or_else(|| format!("{:?}", result.receipt.status)),
            },
            (None, Some(error)) => crate::screen::fleet::FleetEvent::ActionFailed {
                session_key: "fleet".into(),
                detail: error.message.clone(),
            },
            (None, None) => return,
        };
        let out = crate::screen::fleet::reduce_fleet(&self.screens.fleet, event);
        self.screens.fleet = out.state;
        self.fleet_fetch_pending = true;
        self.conn.on_event();
    }

    fn apply_fleet_start_result(&mut self, resp: &RpcResponse) {
        use ainb_hangar_proto::fleet::{ActionReceiptStatus, FleetStartResult};
        let result = resp
            .result
            .as_ref()
            .and_then(|value| serde_json::from_value::<FleetStartResult>(value.clone()).ok());
        let event = match (result, &resp.error) {
            (Some(result), _) if result.receipt.status == ActionReceiptStatus::Delivered => {
                crate::screen::fleet::FleetEvent::ActionSucceeded {
                    session_key: result.prospective_session_key,
                }
            }
            (Some(result), _) => crate::screen::fleet::FleetEvent::ActionFailed {
                session_key: result.prospective_session_key,
                detail: result
                    .receipt
                    .detail
                    .unwrap_or_else(|| format!("{:?}", result.receipt.status)),
            },
            (None, Some(error)) => crate::screen::fleet::FleetEvent::ActionFailed {
                session_key: "start".into(),
                detail: error.message.clone(),
            },
            (None, None) => return,
        };
        let out = crate::screen::fleet::reduce_fleet(&self.screens.fleet, event);
        self.screens.fleet = out.state;
        self.fleet_fetch_pending = true;
        self.conn.on_event();
    }

    fn apply_fleet_broadcast_result(&mut self, resp: &RpcResponse) {
        use ainb_hangar_proto::fleet::{ActionReceiptStatus, FleetBroadcastResult};
        if let Some(error) = &resp.error {
            let out = crate::screen::fleet::reduce_fleet(
                &self.screens.fleet,
                crate::screen::fleet::FleetEvent::BroadcastFailed {
                    detail: error.message.clone(),
                },
            );
            self.screens.fleet = out.state;
            self.conn.on_event();
            return;
        }
        let Some(result) = resp
            .result
            .as_ref()
            .and_then(|value| serde_json::from_value::<FleetBroadcastResult>(value.clone()).ok())
        else {
            let out = crate::screen::fleet::reduce_fleet(
                &self.screens.fleet,
                crate::screen::fleet::FleetEvent::BroadcastFailed {
                    detail: "invalid fleet/broadcast response".into(),
                },
            );
            self.screens.fleet = out.state;
            self.conn.on_event();
            return;
        };
        let receipts = result
            .receipts
            .into_iter()
            .map(|receipt| crate::screen::fleet::BroadcastReceipt {
                session_key: receipt.session_key,
                status: match receipt.status {
                    ActionReceiptStatus::Delivered => {
                        crate::screen::fleet::ReceiptStatus::Delivered
                    }
                    ActionReceiptStatus::Failed | ActionReceiptStatus::Rejected => {
                        crate::screen::fleet::ReceiptStatus::Failed
                    }
                    ActionReceiptStatus::Pending | ActionReceiptStatus::Unknown => {
                        crate::screen::fleet::ReceiptStatus::Unknown
                    }
                },
                detail: receipt.detail,
            })
            .collect();
        let out = crate::screen::fleet::reduce_fleet(
            &self.screens.fleet,
            crate::screen::fleet::FleetEvent::BroadcastReceipts(receipts),
        );
        self.screens.fleet = out.state;
        self.conn.on_event();
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
        let scoped = serde_json::json!({ "workspace_id": ws.clone() });
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
                BOARDS_REQ_ID,
                daemon_methods::HANGAR_BOARDS_LIST,
                scoped.clone(),
            ),
            (
                SQUADS_LIST_REQ_ID,
                daemon_methods::HANGAR_SQUADS_LIST,
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
                RUN_HISTORY_REQ_ID,
                daemon_methods::HANGAR_RUN_HISTORY,
                scoped.clone(),
            ),
            (
                MEMBERS_REQ_ID,
                daemon_methods::HANGAR_MEMBERS_LIST,
                scoped.clone(),
            ),
            // Every daemon-config knob's live value for the Settings Daemon section.
            (
                DAEMON_CONFIG_LIST_REQ_ID,
                daemon_methods::HANGAR_DAEMON_CONFIG_LIST,
                serde_json::json!({}),
            ),
            // The Inbox screen is THE LOCAL HUMAN's inbox, not the workspace's:
            // every entry is addressed to one actor (store migration 0060), so
            // the request names whose inbox this is. When a real signed-in
            // identity lands, only `SELF_AUTHOR_REF` changes.
            (
                INBOX_LIST_REQ_ID,
                daemon_methods::HANGAR_INBOX_LIST,
                inbox_params(&ws),
            ),
            // The control-center board is FLEET-WIDE (every workspace + the
            // no-workspace host sessions), so its feed is unscoped by design.
            // `attention/subscribe` (no workspace_id) does double duty: it (re)arms
            // the connection's live AttentionRaised / AttentionAnswered forwarder
            // AND its ack carries the current open snapshot the board renders. It
            // rides the snapshot fetch rather than the connect handshake so the
            // handshake ack sequence — and the tests that pin it — stay unchanged;
            // a re-fetch simply re-registers the forwarder (last-subscribe-wins)
            // and reconciles from the fresh ack snapshot.
            (
                ATTENTION_SUBSCRIBE_REQ_ID,
                daemon_methods::ATTENTION_SUBSCRIBE,
                serde_json::json!({}),
            ),
            (
                HEALTH_REQ_ID,
                daemon_methods::HANGAR_HEALTH,
                serde_json::json!({}),
            ),
            // P5: the profile-editor roster is host-scoped (a profile drives runs
            // in any workspace), so its list is unscoped. The per-selection
            // `profile/get` fetches detail + previews lazily.
            (
                PROFILE_LIST_REQ_ID,
                daemon_methods::PROFILE_LIST,
                serde_json::json!({}),
            ),
            // F3: the card-create `@` autocomplete repo roster is host-scoped (a
            // repo picker spans workspaces), so it is fetched unscoped once.
            (
                REPO_LIST_REQ_ID,
                daemon_methods::HANGAR_REPO_LIST,
                serde_json::json!({}),
            ),
        ];
        for (id, method, params) in requests {
            let Ok(body) = encode_request(id, method, params) else {
                continue;
            };
            if let Err(e) = host.unix_socket_send(stream_id.clone(), body).await {
                let _ = host.log_info(format!("hangar: snapshot send failed: {e}")).await;
            }
        }
    }

    async fn fetch_fleet_snapshot(&mut self, host: &HostClient) {
        let Some(stream_id) = self.conn.stream_id().map(ToString::to_string) else {
            return;
        };
        let Ok(body) = encode_request(
            FLEET_SNAPSHOT_REQ_ID,
            daemon_methods::FLEET_SNAPSHOT,
            serde_json::json!({}),
        ) else {
            return;
        };
        if let Err(error) = host.unix_socket_send(stream_id, body).await {
            let _ = host.log_info(format!("hangar: fleet snapshot send failed: {error}")).await;
        }
    }

    async fn subscribe_fleet(&mut self, host: &HostClient) {
        let Some(stream_id) = self.conn.stream_id().map(ToString::to_string) else {
            return;
        };
        let Ok(body) = encode_request(
            FLEET_SUBSCRIBE_REQ_ID,
            daemon_methods::FLEET_SUBSCRIBE,
            serde_json::json!({ "after_revision": self.screens.fleet.head_revision() }),
        ) else {
            return;
        };
        if let Err(error) = host.unix_socket_send(stream_id, body).await {
            let _ = host.log_info(format!("hangar: fleet subscribe send failed: {error}")).await;
        }
    }

    async fn apply_fleet_intent(
        &mut self,
        host: &HostClient,
        intent: crate::screen::fleet::FleetIntent,
    ) {
        use crate::screen::fleet::{FleetEvent, FleetIntent, reduce_fleet};
        match intent {
            FleetIntent::Execute {
                session_key,
                expected_version,
                action,
            } => {
                if Self::is_offline(self.conn.state()) && action.is_high_risk() {
                    let out = reduce_fleet(
                        &self.screens.fleet,
                        FleetEvent::ActionFailed {
                            session_key,
                            detail: "daemon unavailable; high-risk action disabled".into(),
                        },
                    );
                    self.screens.fleet = out.state;
                    return;
                }
                let action = match self.fleet_control_action(&session_key, action) {
                    Ok(action) => action,
                    Err(detail) => {
                        let out = reduce_fleet(
                            &self.screens.fleet,
                            FleetEvent::ActionFailed {
                                session_key,
                                detail,
                            },
                        );
                        self.screens.fleet = out.state;
                        return;
                    }
                };
                let request_id = fleet_request_id("action");
                let params = ainb_hangar_proto::fleet::FleetActionParams {
                    session_key: session_key.clone(),
                    expected_version,
                    request_id,
                    action,
                };
                self.send_fleet_rpc(
                    host,
                    FLEET_ACTION_REQ_ID,
                    daemon_methods::FLEET_ACTION,
                    params,
                    &session_key,
                )
                .await;
            }
            FleetIntent::Broadcast {
                text,
                recipient_keys,
                idempotency_key,
                max_parallel: _,
                retry_failures_only: _,
            } => {
                let params = ainb_hangar_proto::fleet::FleetBroadcastParams {
                    target_keys: recipient_keys,
                    text,
                    idempotency_key,
                };
                self.send_fleet_rpc(
                    host,
                    FLEET_BROADCAST_REQ_ID,
                    daemon_methods::FLEET_BROADCAST,
                    params,
                    "broadcast",
                )
                .await;
            }
            FleetIntent::Start {
                provider,
                cwd,
                prompt,
            } => {
                let params = fleet_start_params(provider, cwd, prompt);
                self.send_fleet_rpc(
                    host,
                    FLEET_START_REQ_ID,
                    daemon_methods::FLEET_START,
                    params,
                    "start",
                )
                .await;
            }
            FleetIntent::AttachEmbedded {
                session_key,
                tmux_target,
            } => self.apply_fleet_attach(session_key, tmux_target, false),
            FleetIntent::AttachFullscreen {
                session_key,
                tmux_target,
            } => self.apply_fleet_attach(session_key, tmux_target, true),
        }
    }

    fn fleet_control_action(
        &self,
        _session_key: &str,
        action: crate::screen::fleet::FleetAction,
    ) -> std::result::Result<ainb_hangar_proto::fleet::ControlAction, String> {
        action.into_control_action()
    }

    async fn send_fleet_rpc<T: serde::Serialize>(
        &mut self,
        host: &HostClient,
        id: i64,
        method: &str,
        params: T,
        target: &str,
    ) {
        use crate::screen::fleet::{FleetEvent, reduce_fleet};
        let send_result = async {
            let stream_id = self
                .conn
                .stream_id()
                .map(ToString::to_string)
                .ok_or_else(|| "daemon unavailable".to_string())?;
            let params = serde_json::to_value(params).map_err(|error| error.to_string())?;
            let body = encode_request(id, method, params).map_err(|error| error.to_string())?;
            host.unix_socket_send(stream_id, body).await.map_err(|error| error.to_string())
        }
        .await;
        if let Err(detail) = send_result {
            let event = if method == daemon_methods::FLEET_BROADCAST {
                FleetEvent::BroadcastFailed { detail }
            } else {
                FleetEvent::ActionFailed {
                    session_key: target.to_string(),
                    detail,
                }
            };
            let out = reduce_fleet(&self.screens.fleet, event);
            self.screens.fleet = out.state;
        }
    }

    fn apply_fleet_attach(&mut self, session_key: String, tmux_target: String, fullscreen: bool) {
        use crate::screen::fleet::{FleetEvent, reduce_fleet};
        let result = launch_fleet_tmux_popup(&tmux_target, fullscreen);
        let message = match result {
            Ok(()) => format!("attached {session_key}; exit returns to Fleet"),
            Err(error) => format!("attach failed: {error}"),
        };
        let out = reduce_fleet(&self.screens.fleet, FleetEvent::Feedback(message));
        self.screens.fleet = out.state;
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
            SkillAction::ToggleEnabled(slug) => {
                let Some(agent) = self.first_agent_ref() else {
                    let _ = host.log_info("hangar: no agent to toggle skill for").await;
                    return;
                };
                // A link the daemon has not reported on is treated as ENABLED
                // (the column default), so the first `t` on it disables it —
                // matching what the row renders.
                let enabled = !self.screens.skill_manager.link_disabled(&slug);
                (
                    SKILL_SET_ENABLED_REQ_ID,
                    daemon_methods::HANGAR_SKILL_SET_ENABLED,
                    serde_json::json!({
                        "workspace_id": ws,
                        "agent_id": agent,
                        "skill_id": slug,
                        "enabled": !enabled,
                    }),
                )
            }
        };
        let Ok(body) = encode_request(id, method, params) else {
            return;
        };
        if let Err(e) = host.unix_socket_send(stream_id, body).await {
            let _ = host.log_info(format!("hangar: skill rpc send failed: {e}")).await;
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
            let _ = host.log_info(format!("hangar: task transition send failed: {e}")).await;
        }
    }

    /// Fire the deferred `hangar/task_retry` raised by the Task Kanban failed-column
    /// `R` / task-detail `R` — the operator's manual force-requeue of a terminal
    /// task.
    ///
    /// Unlike the automatic retry seam (which correctly refuses `agent_error` and a
    /// capped chain), this is a HUMAN override: the daemon requeues ANY terminal
    /// reason. On success the daemon emits `TaskQueued`, which drives the plugin's
    /// snapshot re-fetch so the fresh attempt card appears in the queued column —
    /// the visible confirmation. A send failure is logged but non-fatal.
    async fn apply_task_retry_action(&mut self, host: &HostClient, task_id: String) {
        let Some(stream_id) = self.conn.stream_id().map(ToString::to_string) else {
            return;
        };
        let ws = self.app_state().ws_id.as_str().to_string();
        let params = serde_json::json!({ "workspace_id": ws, "task_id": task_id });
        let Ok(body) = encode_request(TASK_RETRY_REQ_ID, daemon_methods::HANGAR_TASK_RETRY, params)
        else {
            return;
        };
        if let Err(e) = host.unix_socket_send(stream_id, body).await {
            let _ = host.log_info(format!("hangar: task retry send failed: {e}")).await;
        }
    }

    /// Fire a deferred board mutation RPC raised by the Boards screen (P4 / D8,
    /// ccc / D6, D16).
    ///
    /// Maps the [`BoardsAction`] onto its `hangar/board_*` RPC. The CRUD +
    /// card-create + column-rename mutations answer with the refreshed
    /// `BoardsListResult` under [`BOARDS_REQ_ID`] ([`Self::apply_boards`]); the
    /// card RUN answers with a `BoardCardRunResult` under [`BOARD_CARD_RUN_REQ_ID`]
    /// ([`Self::apply_board_card_run`]). `CardAttach` fires no RPC — the current
    /// runner exposes no live pane per task, so it surfaces the card's run state as
    /// a note. A send failure is logged but non-fatal (the next pull reconciles).
    async fn apply_boards_action(
        &mut self,
        host: &HostClient,
        action: crate::screen::BoardsAction,
    ) {
        use crate::screen::BoardsAction;
        let ws = self.app_state().ws_id.as_str().to_string();

        // Attach is a local affordance: with no interactive runner yet (D6), a
        // headless run has no attachable pane. Report the card's run state rather
        // than faking an attach or dead-ending the key.
        if let BoardsAction::CardAttach { issue_id, .. } = &action {
            let note = self.card_attach_note(issue_id);
            self.screens.boards.set_note(note);
            return;
        }

        let Some(stream_id) = self.conn.stream_id().map(ToString::to_string) else {
            return;
        };
        let (req_id, method, params) = match action {
            BoardsAction::BoardCreate { name } => (
                BOARDS_REQ_ID,
                daemon_methods::HANGAR_BOARD_CREATE,
                serde_json::json!({ "workspace_id": ws, "name": name }),
            ),
            BoardsAction::ColumnReorder {
                board_id,
                column_ids,
            } => (
                BOARDS_REQ_ID,
                daemon_methods::HANGAR_BOARD_COLUMN_REORDER,
                serde_json::json!({ "workspace_id": ws, "board_id": board_id, "column_ids": column_ids }),
            ),
            BoardsAction::ColumnDelete {
                board_id,
                column_id,
            } => (
                BOARDS_REQ_ID,
                daemon_methods::HANGAR_BOARD_COLUMN_DELETE,
                serde_json::json!({ "workspace_id": ws, "board_id": board_id, "column_id": column_id }),
            ),
            BoardsAction::ColumnAdd { board_id, name } => (
                BOARDS_REQ_ID,
                daemon_methods::HANGAR_BOARD_COLUMN_ADD,
                serde_json::json!({ "workspace_id": ws, "board_id": board_id, "name": name }),
            ),
            BoardsAction::BoardUpdate {
                board_id,
                auto_move,
            } => (
                BOARDS_REQ_ID,
                daemon_methods::HANGAR_BOARD_UPDATE,
                serde_json::json!({ "workspace_id": ws, "board_id": board_id, "auto_move": auto_move }),
            ),
            BoardsAction::CardCreate {
                board_id,
                column_id,
                title,
                repo_ref,
                agent,
                assignee_profile,
            } => (
                BOARDS_REQ_ID,
                daemon_methods::HANGAR_BOARD_CARD_CREATE,
                serde_json::json!({
                    "workspace_id": ws,
                    "board_id": board_id,
                    "column_id": column_id,
                    "title": title,
                    "assignee_profile": assignee_profile,
                    // F1-F4: the card carries the repo + agent the overlay picked
                    // (repo is REQUIRED, scratch always offered first). The daemon
                    // persists both on the issue so a run / rerun provisions the
                    // right worktree and routes to the chosen provider.
                    "repo_ref": repo_ref,
                    "agent": agent,
                }),
            ),
            BoardsAction::ColumnRename {
                board_id,
                column_id,
                name,
            } => (
                BOARDS_REQ_ID,
                daemon_methods::HANGAR_BOARD_COLUMN_UPDATE,
                serde_json::json!({ "workspace_id": ws, "board_id": board_id, "column_id": column_id, "name": name }),
            ),
            // F6 card edit: rewrite the issue title + persist repo/agent via
            // `issue_update`. It answers with an IssueRow (not a BoardsListResult),
            // so it rides ISSUE_UPDATE_REQ_ID — whose reply handler arms a snapshot
            // re-pull, re-rendering the board with the new title.
            BoardsAction::CardEdit {
                issue_id,
                title,
                repo_ref,
                agent,
            } => (
                ISSUE_UPDATE_REQ_ID,
                daemon_methods::HANGAR_ISSUE_UPDATE,
                serde_json::json!({
                    "workspace_id": ws,
                    "issue_id": issue_id,
                    "title": title,
                    "repo_ref": repo_ref,
                    "agent": agent,
                }),
            ),
            BoardsAction::CardRun {
                board_id,
                issue_id,
                mode,
            } => (
                BOARD_CARD_RUN_REQ_ID,
                daemon_methods::HANGAR_BOARD_CARD_RUN,
                serde_json::json!({ "workspace_id": ws, "board_id": board_id, "issue_id": issue_id, "mode": mode }),
            ),
            BoardsAction::CardCancel { board_id, issue_id } => (
                BOARD_CARD_CANCEL_REQ_ID,
                daemon_methods::HANGAR_BOARD_CARD_CANCEL,
                serde_json::json!({ "workspace_id": ws, "board_id": board_id, "issue_id": issue_id }),
            ),
            // Remove + reorder both answer with the refreshed BoardsListResult under
            // BOARDS_REQ_ID (the board re-renders from the reply); no bespoke note.
            BoardsAction::CardRemove { board_id, issue_id } => (
                BOARDS_REQ_ID,
                daemon_methods::HANGAR_BOARD_CARD_REMOVE,
                serde_json::json!({ "workspace_id": ws, "board_id": board_id, "issue_id": issue_id }),
            ),
            BoardsAction::CardReorder {
                board_id,
                column_id,
                issue_ids,
            } => (
                BOARDS_REQ_ID,
                daemon_methods::HANGAR_BOARD_CARD_REORDER,
                serde_json::json!({ "workspace_id": ws, "board_id": board_id, "column_id": column_id, "issue_ids": issue_ids }),
            ),
            // Timeline fetch answers under its own req id with the raw transcript;
            // the reply handler parses it into the overlay.
            BoardsAction::CardTimeline { board_id, issue_id } => (
                BOARD_CARD_TIMELINE_REQ_ID,
                daemon_methods::HANGAR_BOARD_CARD_TIMELINE,
                serde_json::json!({ "workspace_id": ws, "board_id": board_id, "issue_id": issue_id }),
            ),
            // T4 / F7: assign-squad, add-dependency, and set-auto-run all answer with
            // the refreshed BoardsListResult under BOARDS_REQ_ID, so the board
            // re-renders (member chips / 🔒 blocked-state / auto-run marker) from the
            // reply without a second round-trip (mirrors CardRemove).
            BoardsAction::CardAssignSquad {
                board_id,
                issue_id,
                squad_id,
            } => (
                BOARDS_REQ_ID,
                daemon_methods::HANGAR_BOARD_CARD_ASSIGN_SQUAD,
                serde_json::json!({ "workspace_id": ws, "board_id": board_id, "issue_id": issue_id, "squad_id": squad_id }),
            ),
            BoardsAction::CardDepAdd {
                board_id,
                dependent_issue_id,
                blocker_issue_id,
                link_type,
            } => (
                BOARDS_REQ_ID,
                daemon_methods::HANGAR_BOARD_CARD_DEP_ADD,
                serde_json::json!({ "workspace_id": ws, "board_id": board_id, "dependent_issue_id": dependent_issue_id, "blocker_issue_id": blocker_issue_id, "link_type": link_type }),
            ),
            BoardsAction::CardSetAutoRun {
                board_id,
                issue_id,
                auto_run,
            } => (
                BOARDS_REQ_ID,
                daemon_methods::HANGAR_BOARD_CARD_SET_AUTO_RUN,
                serde_json::json!({ "workspace_id": ws, "board_id": board_id, "issue_id": issue_id, "auto_run": auto_run }),
            ),
            // A local-overlay repaint round-trip: re-fetch the board list; the
            // reply re-renders with the open overlay preserved.
            BoardsAction::Refresh => (
                BOARDS_REQ_ID,
                daemon_methods::HANGAR_BOARDS_LIST,
                serde_json::json!({ "workspace_id": ws }),
            ),
            // Handled above as a local note; never reached here.
            BoardsAction::CardAttach { .. } => return,
        };
        let Ok(body) = encode_request(req_id, method, params) else {
            return;
        };
        if let Err(e) = host.unix_socket_send(stream_id, body).await {
            let _ = host.log_info(format!("hangar: board rpc send failed: {e}")).await;
        }
    }

    /// Build the attach note for `issue_id` (ccc / D6).
    ///
    /// When the card's latest task ran `interactive`, the daemon recorded the exact
    /// tmux session name on it — surface a copyable `tmux attach -t <name>` so the
    /// user can attach to the live agent. This is the HONEST contract: a plugin
    /// subprocess has no controlling terminal and the `host/*` protocol exposes no
    /// terminal-attach method, so the plugin cannot drive the attach itself — it
    /// hands over the exact command instead of faking a takeover. A headless run
    /// has no attachable pane, so it keeps reporting the card's run state.
    fn card_attach_note(&self, issue_id: &str) -> String {
        let card = self
            .screens
            .boards
            .boards()
            .iter()
            .flat_map(|b| b.columns.iter().flat_map(|c| c.cards.iter()).chain(b.unmapped.iter()))
            .find(|c| c.issue_id == issue_id);
        if let Some(name) = card.and_then(|c| c.session_name.as_deref()) {
            return format!("attach: #{issue_id} — tmux attach -t {name}");
        }
        match card.and_then(|c| c.state.as_deref()) {
            Some("running") => {
                format!("attach: #{issue_id} is running (headless — no live pane yet)")
            }
            Some(other) => format!("attach: #{issue_id} is {other} — press Enter to launch"),
            None => format!("attach: #{issue_id} has no run yet — press Enter to launch"),
        }
    }

    /// Fire a deferred squad RPC raised by the Squads screen (P7 / D17).
    ///
    /// Resolves the create/add/assign *selection* from the plugin's cached
    /// agents/issues (the screen intent carries only ids), maps each
    /// [`SquadAction`] to its `hangar/squad_*` RPC, and frames it over the socket
    /// cap. The create / add / remove mutations reply with the refreshed
    /// `SquadsListResult` under [`SQUADS_LIST_REQ_ID`] (folded by
    /// [`Self::apply_squads`]); the assign fans out via `hangar/squad_fanout` under
    /// [`SQUAD_FANOUT_REQ_ID`]. A selection that cannot be resolved (no agent, no
    /// issue) surfaces a note rather than firing an empty RPC.
    async fn apply_squad_action(&mut self, host: &HostClient, action: crate::screen::SquadAction) {
        use crate::screen::SquadAction;
        let Some(stream_id) = self.conn.stream_id().map(ToString::to_string) else {
            return;
        };
        let ws = self.app_state().ws_id.as_str().to_string();
        // A fresh action clears any stale transient note; the reply sets a new one.
        self.screens.squads.clear_note();

        let (id, method, params) = match action {
            SquadAction::CreateAgent { name } => {
                // Fire agent_create with NO ids — the daemon fills workspace/runtime/
                // owner. The reply's refreshed roster clears the squad gate live.
                (
                    AGENT_CREATE_REQ_ID,
                    daemon_methods::HANGAR_AGENT_CREATE,
                    serde_json::json!({ "name": name }),
                )
            }
            SquadAction::Create { name } => {
                let Some(agent_id) = self.first_agent_ref() else {
                    self.screens.squads.note_err("no agent available to lead a squad");
                    return;
                };
                (
                    SQUADS_LIST_REQ_ID,
                    daemon_methods::HANGAR_SQUAD_CREATE,
                    serde_json::json!({
                        "workspace_id": ws,
                        "name": name,
                        "leader": format!("agent:{agent_id}"),
                    }),
                )
            }
            SquadAction::AddMember { squad_id } => {
                let Some(member) = self.next_squad_member_ref(&squad_id) else {
                    self.screens.squads.note_err("no more agents to add");
                    return;
                };
                (
                    SQUADS_LIST_REQ_ID,
                    daemon_methods::HANGAR_SQUAD_MEMBER_ADD,
                    serde_json::json!({ "workspace_id": ws, "squad_id": squad_id, "member": member }),
                )
            }
            SquadAction::RemoveMember {
                squad_id,
                member_ref,
            } => (
                SQUADS_LIST_REQ_ID,
                daemon_methods::HANGAR_SQUAD_MEMBER_REMOVE,
                serde_json::json!({ "workspace_id": ws, "squad_id": squad_id, "member": member_ref }),
            ),
            SquadAction::SetMemberRole {
                squad_id,
                member_ref,
                role,
            } => (
                SQUADS_LIST_REQ_ID,
                daemon_methods::HANGAR_SQUAD_MEMBER_ROLE_SET,
                serde_json::json!({
                    "workspace_id": ws,
                    "squad_id": squad_id,
                    "member": member_ref,
                    "role": role,
                }),
            ),
            SquadAction::SetInstructions {
                squad_id,
                instructions,
            } => (
                SQUADS_LIST_REQ_ID,
                daemon_methods::HANGAR_SQUAD_INSTRUCTIONS_SET,
                serde_json::json!({
                    "workspace_id": ws,
                    "squad_id": squad_id,
                    "instructions": instructions,
                }),
            ),
            SquadAction::Assign { squad_id } => {
                let Some(issue_id) = self.first_assignable_issue() else {
                    self.screens.squads.note_err("no issue available to assign");
                    return;
                };
                (
                    SQUAD_FANOUT_REQ_ID,
                    daemon_methods::HANGAR_SQUAD_FANOUT,
                    serde_json::json!({ "workspace_id": ws, "squad_id": squad_id, "issue_id": issue_id }),
                )
            }
        };
        let Ok(body) = encode_request(id, method, params) else {
            return;
        };
        if let Err(e) = host.unix_socket_send(stream_id, body).await {
            let _ = host.log_info(format!("hangar: squad rpc send failed: {e}")).await;
        }
    }

    /// Fire a deferred agent RPC raised by the Agents roster screen (slice 2).
    ///
    /// Maps [`AgentsAction::Create`] to `hangar/agent_create` (fired with no ids —
    /// the daemon fills workspace / runtime / owner) and [`AgentsAction::Delete`] to
    /// `hangar/agent_delete` (the agent's id extracted from its `agent:<id>` ref,
    /// scoped to the workspace). The create reply folds through [`AGENT_CREATE_REQ_ID`]
    /// and the delete through [`AGENT_DELETE_REQ_ID`] — both refresh the roster via
    /// the shared actor cache.
    async fn apply_agents_action(
        &mut self,
        host: &HostClient,
        action: crate::screen::AgentsAction,
    ) {
        use crate::screen::AgentsAction;
        let Some(stream_id) = self.conn.stream_id().map(ToString::to_string) else {
            return;
        };
        let ws = self.app_state().ws_id.as_str().to_string();
        let (id, method, params) = match action {
            AgentsAction::Create {
                name,
                description,
                provider,
                model,
                instructions,
            } => (
                AGENT_CREATE_REQ_ID,
                daemon_methods::HANGAR_AGENT_CREATE,
                // `model` / `instructions` / `description` are `Option`s; the
                // proto's `skip_serializing_if` drops them when absent so the wire
                // stays clean and the daemon leaves those columns at their defaults.
                serde_json::json!({
                    "workspace_id": ws,
                    "name": name,
                    "provider": provider,
                    "model": model,
                    "instructions": instructions,
                    "description": description,
                }),
            ),
            AgentsAction::Delete { actor_ref } => {
                // Extract the bare agent id from the canonical `agent:<id>` ref; a
                // malformed ref (no prefix) is a no-op rather than a bad RPC.
                let Some(agent_id) = actor_ref.strip_prefix("agent:") else {
                    return;
                };
                (
                    AGENT_DELETE_REQ_ID,
                    daemon_methods::HANGAR_AGENT_DELETE,
                    serde_json::json!({ "workspace_id": ws, "agent_id": agent_id }),
                )
            }
        };
        let Ok(body) = encode_request(id, method, params) else {
            return;
        };
        if let Err(e) = host.unix_socket_send(stream_id, body).await {
            let _ = host.log_info(format!("hangar: agent rpc send failed: {e}")).await;
        }
    }

    /// The first cached AGENT actor-ref not already the leader or a member of
    /// `squad_id` — the glue's add-member selection policy (P7). `None` when every
    /// cached agent is already on the squad.
    fn next_squad_member_ref(&self, squad_id: &str) -> Option<String> {
        let squad = self.screens.squads.squads().iter().find(|s| s.id == squad_id)?;
        let mut taken: std::collections::HashSet<&str> =
            squad.members.iter().map(|m| m.actor_ref.as_str()).collect();
        taken.insert(squad.leader.actor_ref.as_str());
        self.screens
            .actors
            .iter()
            .find(|a| a.is_agent && !taken.contains(a.actor_ref.as_str()))
            .map(|a| a.actor_ref.clone())
    }

    /// The issue the Squads `x` assign fans out — the selected issue-list row, else
    /// the first visible issue (P7). `None` when the workspace has no issues.
    fn first_assignable_issue(&self) -> Option<String> {
        self.screens
            .issue_list
            .selected_row()
            .or_else(|| self.screens.issue_list.visible_rows().next())
            .map(|row| row.id.as_str().to_string())
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
            let _ = host.log_info(format!("hangar: issue update send failed: {e}")).await;
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
            let _ = host.log_info(format!("hangar: issue state move send failed: {e}")).await;
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
            let _ = host.log_info(format!("hangar: issue priority send failed: {e}")).await;
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
            let _ = host.log_info(format!("hangar: issue assignee send failed: {e}")).await;
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
            let _ = host.log_info(format!("hangar: comment add send failed: {e}")).await;
        }
    }

    /// Fire a deferred acceptance-criterion tick raised by the task-detail `t`
    /// key (multica parity #11-rest).
    ///
    /// Maps the [`IssueCriterionAction`] to `hangar/issue_criterion_set`,
    /// addressing the criterion by its STABLE id and attributing the tick to the
    /// current member. The daemon's `IssueUpdated` push re-renders the card
    /// (mirroring [`Self::apply_comment_action`] — this fires the RPC only, no
    /// separate re-pull). A send failure is logged but non-fatal — the criterion
    /// simply stays as it was.
    ///
    /// [`IssueCriterionAction`]: crate::screen::IssueCriterionAction
    async fn apply_criterion_action(
        &mut self,
        host: &HostClient,
        action: crate::screen::IssueCriterionAction,
    ) {
        let Some(stream_id) = self.conn.stream_id().map(ToString::to_string) else {
            return;
        };
        let ws = self.app_state().ws_id.as_str().to_string();
        let params = serde_json::json!({
            "workspace_id": ws,
            "issue_id": action.issue_id,
            "criterion": action.criterion_id,
            "checked": action.checked,
            "actor": SELF_AUTHOR_REF,
        });
        let Ok(body) = encode_request(
            ISSUE_CRITERION_SET_REQ_ID,
            daemon_methods::HANGAR_ISSUE_CRITERION_SET,
            params,
        ) else {
            return;
        };
        if let Err(e) = host.unix_socket_send(stream_id, body).await {
            let _ = host.log_info(format!("hangar: criterion set send failed: {e}")).await;
        }
    }

    /// Fold a `hangar/issue_timeline` reply into the open activity modal
    /// (multica parity #13).
    ///
    /// A no-op when the modal has since closed (a stale reply for a dismissed
    /// overlay), and an error leaves the modal in its loading state rather than
    /// showing a fabricated empty narrative.
    fn apply_issue_timeline(&mut self, resp: &ainb_hangar_proto::RpcResponse) {
        let Some(result) = resp.result.as_ref() else {
            return;
        };
        let Ok(parsed) = serde_json::from_value::<ainb_hangar_proto::snapshots::IssueTimelineResult>(
            result.clone(),
        ) else {
            return;
        };
        if let Some(activity) = self.screens.activity.as_mut() {
            activity.apply_entries(parsed.entries);
        }
        self.conn.on_event();
    }

    /// Fire a deferred `hangar/issue_timeline` fetch for the open activity modal
    /// (multica parity #13). A send failure is logged but non-fatal — the modal
    /// keeps its loading state and `r` retries.
    async fn fire_issue_timeline(&mut self, host: &HostClient, issue_id: String) {
        let Some(stream_id) = self.conn.stream_id().map(ToString::to_string) else {
            return;
        };
        let ws = self.app_state().ws_id.as_str().to_string();
        let params = serde_json::json!({ "workspace_id": ws, "issue_id": issue_id });
        let Ok(body) = encode_request(
            ISSUE_TIMELINE_REQ_ID,
            daemon_methods::HANGAR_ISSUE_TIMELINE,
            params,
        ) else {
            return;
        };
        if let Err(e) = host.unix_socket_send(stream_id, body).await {
            let _ = host.log_info(format!("hangar: issue timeline send failed: {e}")).await;
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
            let _ = host.log_info(format!("hangar: pr status refresh send failed: {e}")).await;
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

    /// Fire the first leg of the Issues create-wizard chain (Phase 5):
    /// `hangar/issue_create` with the typed title, stashing the repo / agent /
    /// branches on [`Self::wizard_dispatch_in_flight`] so the create reply (which
    /// hands back the new issue id) can arm the `issue_update` + `issue_run`
    /// follow-ups. Any failure to even SEND is surfaced as an issue-list note —
    /// never a silent dead end.
    async fn apply_create_action(
        &mut self,
        host: &HostClient,
        action: crate::screen::IssueCreateAction,
    ) {
        use crate::screen::IssueCreateAction;
        let IssueCreateAction::CreateAndRun {
            title,
            description,
            external_ref,
            acceptance_criteria,
            context_refs,
            priority,
            due_date,
            labels,
            repo_ref,
            source_branch,
            target_branch,
            agent,
            assignee,
            parent_issue_id,
        } = action;
        let Some(stream_id) = self.conn.stream_id().map(ToString::to_string) else {
            self.screens.issue_list.set_note("create failed: daemon link is down");
            return;
        };
        let ws = self.app_state().ws_id.as_str().to_string();
        // `description` is the wizard Brief: it lands on `issue.description`, which
        // `build_prompt` turns into the `claude -p` prompt. Omitted when blank so a
        // title-only stub creates unchanged.
        let mut params = serde_json::json!({
            "workspace_id": ws, "title": title, "creator": SELF_AUTHOR_REF
        });
        if let Some(brief) = description {
            params["description"] = serde_json::Value::String(brief);
        }
        // The linked-issue ref (0043) lands on `issue.external_ref` for traceability
        // and is appended to the dispatched brief; omitted when blank.
        if let Some(link) = external_ref {
            params["external_ref"] = serde_json::Value::String(link);
        }
        // 0046 sub-issues: when the wizard was opened as an "add sub-issue" (`s`),
        // thread the pre-bound parent so the daemon links the new issue as a child
        // (`issue.parent_issue_id`). Omitted for a top-level `c` create so the wire
        // shape only grows when a sub-issue is actually created.
        if let Some(parent) = parent_issue_id {
            params["parent_issue_id"] = serde_json::Value::String(parent);
        }
        // 0048: the wizard's Acceptance / Context lists land on
        // `issue.acceptance_criteria` / `issue.context_refs` and render on the
        // detail card. Omitted when empty so the wire shape only grows when the
        // author supplied them (append-only).
        if !acceptance_criteria.is_empty() {
            params["acceptance_criteria"] = serde_json::json!(acceptance_criteria);
        }
        if !context_refs.is_empty() {
            params["context_refs"] = serde_json::json!(context_refs);
        }
        // 0014/0016: the wizard's Priority / Due / Labels rows. Each key is added
        // ONLY when the author moved it off its default, so an unadorned create's
        // wire shape stays byte-identical to pre-parity-28 (append-only).
        if priority != 0 {
            params["priority"] = serde_json::json!(priority);
        }
        if let Some(due) = due_date {
            params["due_date"] = serde_json::json!(due);
        }
        if !labels.is_empty() {
            params["labels"] = serde_json::json!(labels);
        }
        let Ok(body) = encode_request(
            ISSUE_CREATE_REQ_ID,
            daemon_methods::HANGAR_ISSUE_CREATE,
            params,
        ) else {
            self.screens.issue_list.set_note("create failed: could not encode request");
            return;
        };
        // Stash the dispatch payload BEFORE the send so the reply handler finds
        // it; a send failure clears it again (nothing will answer).
        self.wizard_dispatch_in_flight = Some(WizardDispatch {
            repo_ref,
            agent,
            assignee,
            source_branch,
            target_branch,
        });
        if let Err(e) = host.unix_socket_send(stream_id, body).await {
            self.wizard_dispatch_in_flight = None;
            self.screens.issue_list.set_note(format!("create failed: {e}"));
            let _ = host.log_info(format!("hangar: issue create send failed: {e}")).await;
        }
    }

    /// Fire the issue-list `x` delete (63d): encode + send one
    /// `hangar/issue_delete` over the daemon socket. The row is dropped by the
    /// daemon's `IssueDeleted` push on success; a send failure — or a daemon
    /// rejection on the reply — surfaces as an issue-list note, never silent.
    async fn apply_delete_action(
        &mut self,
        host: &HostClient,
        issue_id: ainb_hangar_core::ids::IssueId,
    ) {
        // Only one delete may be in flight: the fixed ISSUE_DELETE_REQ_ID is not a
        // per-call correlation token, so a second delete before the first reply
        // lands would overwrite `delete_in_flight` and misattribute the reply to
        // the wrong issue. Refuse the overlap instead.
        if self.delete_in_flight.is_some() {
            self.screens.issue_list.set_note("delete already in flight — please wait");
            return;
        }
        let Some(stream_id) = self.conn.stream_id().map(ToString::to_string) else {
            self.screens.issue_list.set_note("delete failed: daemon link is down");
            return;
        };
        let ws = self.app_state().ws_id.as_str().to_string();
        let params = serde_json::json!({
            "workspace_id": ws, "issue_id": issue_id.as_str()
        });
        let Ok(body) = encode_request(
            ISSUE_DELETE_REQ_ID,
            daemon_methods::HANGAR_ISSUE_DELETE,
            params,
        ) else {
            self.screens.issue_list.set_note("delete failed: could not encode request");
            return;
        };
        // Stash the target BEFORE the send so a delete refused for active tasks can
        // re-target the "cancel run(s) & delete" overlay (the reply carries no id);
        // a send failure clears it (nothing will answer).
        self.delete_in_flight = Some(issue_id);
        if let Err(e) = host.unix_socket_send(stream_id, body).await {
            self.delete_in_flight = None;
            self.screens.issue_list.set_note(format!("delete failed: {e}"));
            let _ = host.log_info(format!("hangar: issue delete send failed: {e}")).await;
        }
    }

    /// Fire the board-less "cancel run(s) & delete" first leg: encode + send one
    /// `hangar/issue_cancel_active` over the daemon socket. On its success reply the
    /// plugin retries the `issue_delete` (cancel commits before delete); an error —
    /// or a send failure — surfaces as an issue-list note, never silent.
    async fn apply_cancel_delete_action(
        &mut self,
        host: &HostClient,
        issue_id: ainb_hangar_core::ids::IssueId,
    ) {
        // Only one cancel-and-delete may be in flight: ISSUE_CANCEL_ACTIVE_REQ_ID is
        // a fixed per-method id, so an overlapping flow would overwrite
        // `cancel_delete_in_flight` and could fire the delete retry against the
        // wrong issue. Refuse the overlap.
        if self.cancel_delete_in_flight.is_some() {
            self.screens.issue_list.set_note("cancel already in flight — please wait");
            return;
        }
        let Some(stream_id) = self.conn.stream_id().map(ToString::to_string) else {
            self.screens.issue_list.set_note("cancel failed: daemon link is down");
            return;
        };
        let ws = self.app_state().ws_id.as_str().to_string();
        let params = serde_json::json!({
            "workspace_id": ws, "issue_id": issue_id.as_str()
        });
        let Ok(body) = encode_request(
            ISSUE_CANCEL_ACTIVE_REQ_ID,
            daemon_methods::HANGAR_ISSUE_CANCEL_ACTIVE,
            params,
        ) else {
            self.screens.issue_list.set_note("cancel failed: could not encode request");
            return;
        };
        // Stash the target so the reply can arm the follow-up delete retry.
        self.cancel_delete_in_flight = Some(issue_id);
        if let Err(e) = host.unix_socket_send(stream_id, body).await {
            self.cancel_delete_in_flight = None;
            self.screens.issue_list.set_note(format!("cancel failed: {e}"));
            let _ = host.log_info(format!("hangar: issue cancel-active send failed: {e}")).await;
        }
    }

    /// Fire the second leg of the Issues create-wizard chain (Phase 5), armed by
    /// a successful `issue_create` reply: ONE `hangar/issue_update` persisting
    /// repo / agent / source / target on the new issue (the append-only F6
    /// pattern), then `hangar/issue_run` with the SAME values as explicit
    /// overrides — so the run is correct even if the persist is still in flight.
    /// A send failure on either leg surfaces as an issue-list note.
    async fn fire_issue_dispatch(
        &mut self,
        host: &HostClient,
        issue_id: String,
        dispatch: WizardDispatch,
    ) {
        let Some(stream_id) = self.conn.stream_id().map(ToString::to_string) else {
            self.screens.issue_list.set_note("run failed: daemon link is down");
            return;
        };
        let ws = self.app_state().ws_id.as_str().to_string();
        // V3-F3: a NAMED-agent target persists as the issue's assignee (so the card
        // shows the right owner and a later manual re-run resolves it) via
        // `issue_update`, AND rides the `issue_run` as an explicit override, so the
        // dispatch routes to it even though the daemon processes the two legs in
        // order (same belt-and-suspenders discipline as the repo/branch overrides).
        let assignee_update = dispatch.assignee.as_ref().map_or(
            ainb_hangar_proto::snapshots::FieldUpdate::Keep,
            |actor_ref| ainb_hangar_proto::snapshots::FieldUpdate::Set(actor_ref.clone()),
        );
        let update = ainb_hangar_proto::snapshots::IssueUpdateParams {
            workspace_id: ws.clone(),
            issue_id: issue_id.clone(),
            repo_ref: Some(dispatch.repo_ref.clone()),
            agent: dispatch.agent.clone(),
            assignee: assignee_update,
            source_branch: dispatch.source_branch.clone(),
            target_branch: dispatch.target_branch.clone(),
            ..Default::default()
        };
        let run = ainb_hangar_proto::snapshots::IssueRunParams {
            workspace_id: ws,
            issue_id,
            mode: "headless".to_string(),
            repo_ref: Some(dispatch.repo_ref),
            agent: dispatch.agent,
            source_branch: dispatch.source_branch,
            assignee: dispatch.assignee,
            // gap #8: the TUI operator is the workspace owner; None lets the daemon
            // default the invoker to the owner, whom the gate always admits.
            invoker_user_id: None,
        };
        for (id, method, params) in [
            (
                ISSUE_UPDATE_REQ_ID,
                daemon_methods::HANGAR_ISSUE_UPDATE,
                serde_json::to_value(&update).unwrap_or_default(),
            ),
            (
                ISSUE_RUN_REQ_ID,
                daemon_methods::HANGAR_ISSUE_RUN,
                serde_json::to_value(&run).unwrap_or_default(),
            ),
        ] {
            let Ok(body) = encode_request(id, method, params) else {
                self.screens
                    .issue_list
                    .set_note(format!("dispatch failed: could not encode {method}"));
                return;
            };
            if let Err(e) = host.unix_socket_send(stream_id.clone(), body).await {
                self.screens.issue_list.set_note(format!("dispatch failed: {e}"));
                let _ = host.log_info(format!("hangar: {method} send failed: {e}")).await;
                return;
            }
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
            let _ = host.log_info(format!("hangar: autopilot rpc send failed: {e}")).await;
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
        // Create is special: on success it also auto-switches into the new
        // tenant, so track whether a data-plane re-scope is required.
        let mut rescope = matches!(action, WorkspaceAction::SetActive(_));
        let result = match &action {
            // A bare refresh just pulls the list (no mutating cap call).
            WorkspaceAction::Refresh => Ok(()),
            WorkspaceAction::SetActive(id) => {
                host.workspace_set_active(id.clone()).await.map(|_| ())
            }
            WorkspaceAction::SetDefault(id) => {
                host.workspace_set_default(id.clone()).await.map(|_| ())
            }
            WorkspaceAction::Create { slug, name } => {
                match host.workspace_create(slug.clone(), name.clone()).await {
                    // Auto-switch to the new tenant so the operator lands in it,
                    // then re-scope the data plane below.
                    Ok(r) => {
                        host.workspace_set_active(r.workspace.id.clone()).await.ok();
                        rescope = true;
                        Ok(())
                    }
                    Err(e) => Err(e),
                }
            }
            WorkspaceAction::Delete(id) => {
                // A delete re-scopes too: the active workspace may have moved to a
                // surviving tenant (the host resolves effective-active).
                rescope = true;
                host.workspace_delete(id.clone()).await.map(|_| ())
            }
        };
        if let Err(e) = result {
            let _ = host.log_info(format!("hangar: workspace action failed: {e}")).await;
            return;
        }
        self.refresh_workspaces(host).await;
        // After an active-workspace switch/create/delete, re-pull every
        // workspace-scoped snapshot so the screens reflect the NEW tenant's data
        // (not the prior one's stale cache). `refresh_workspaces` already moved
        // `ws_id`, and `fetch_snapshots` reads it, so the re-fetch is scoped to
        // the effective-active target.
        if rescope {
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
        self.screens.set_workspaces(rows, list.creation_disabled);
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
        // While a `[s]` start is in flight the conn state flaps through
        // Dialing on every redial attempt; keep the panel painted for the
        // whole window (with a "starting…" status) so it doesn't flicker
        // against the empty board.
        let starting = self.daemon_start_redial_until.is_some();
        if Self::is_offline(self.conn.state()) || starting {
            if matches!(app.screen, Screen::Fleet) && !starting {
                crate::screen::fleet::render_degraded_banner(&mut buf, w, 1);
            } else {
                crate::widgets::offline_empty_state::render_offline_empty_state(
                    &mut buf,
                    w,
                    h,
                    self.daemon_start_error.as_deref(),
                    starting.then_some("⟳ starting daemon…"),
                );
            }
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
                // Esc drops whichever capture surface is open in one press — the
                // create wizard, the `x` delete-confirm overlay, OR the `f` facet
                // panel (multica-gap #10; all no-op when not in their mode), never
                // trapping the user (63d).
                self.screens.issue_list.abort_create();
                self.screens.issue_list.abort_confirm_delete();
                self.screens.issue_list.abort_filter_panel();
                return;
            }
            if let Some(nav) = route_key(&app, &mut self.screens, key) {
                self.apply_nav(&app, nav);
            }
            return;
        }

        // The other two text-capture surfaces are just as key-hungry: the
        // task-detail comment-compose modal (e38.5) and the settings key-entry
        // (API-key) modal each treat every printable key as typed text. Without
        // this guard the routing layer would swallow the global tab-switch chars
        // (`C`/`U`/`I`/`K`/`d`/`L`/`,`) first — so typing an uppercase `C` in an
        // API key or a comment draft would switch tabs and drop the character
        // instead of inserting it. Route straight to the screen reducer (which
        // owns Esc-to-close for both), mirroring the issue-list capture guard.
        if matches!(app.screen, Screen::TaskDetail(_))
            && self
                .screens
                .task_detail
                .as_ref()
                .is_some_and(|td| td.compose_buffer().is_some())
        {
            if let Some(nav) = route_key(&app, &mut self.screens, key) {
                self.apply_nav(&app, nav);
            }
            return;
        }
        // The Settings screen has THREE text-capture surfaces: the key-entry
        // (API-key) modal, the Daemon-section numeric-config overlay, and the
        // new-workspace name modal (P-multica#4). All must be listed here — a
        // workspace name like `Beta` / `QA` / `Data` contains an uppercase letter
        // the routing layer claims as a tab switch (`B`→Boards, `q`→quit, …), so
        // omitting the name modal makes typing such a name teleport the user to
        // another tab and drop the keystroke (exactly as the config overlay's
        // digits did). Keep this in sync with `is_capturing_text`.
        if matches!(app.screen, Screen::Settings)
            && self.screens.settings.as_ref().is_some_and(|s| {
                s.key_entry_open()
                    || s.config_input_buffer().is_some()
                    || s.workspace_name_input().is_some()
            })
        {
            if let Some(nav) = route_key(&app, &mut self.screens, key) {
                self.apply_nav(&app, nav);
            }
            return;
        }
        // The Squads screen's create-name input is a text-capture surface too (P7):
        // while it is open, every key — including the tab-switch chars and `q` — is
        // typed text, not a nav key. Route straight to the screen reducer (which
        // owns Esc-to-cancel), mirroring the issue-list capture guard so typing a
        // squad name like `qa` inserts instead of quitting / switching tabs.
        if matches!(app.screen, Screen::Squads) && self.screens.squads.is_capturing() {
            if let Some(nav) = route_key(&app, &mut self.screens, key) {
                self.apply_nav(&app, nav);
            }
            return;
        }
        // The Agents roster's create-name input and delete-confirm overlay are
        // text/decision capture surfaces too (slice 2): while one is open every key
        // — including the tab-switch chars and `q` — belongs to the overlay, not to
        // nav. Route straight to the reducer (which owns Esc-to-cancel) so typing an
        // agent name like `qa` inserts instead of quitting / switching tabs, and so
        // Enter confirms the delete rather than being eaten by the routing layer.
        if matches!(app.screen, Screen::Agents) && self.screens.agents.is_capturing() {
            if let Some(nav) = route_key(&app, &mut self.screens, key) {
                self.apply_nav(&app, nav);
            }
            return;
        }
        // ccc (lu5): the Boards card overlays (create title / profile pick / column
        // rename / `Run ▾`) are text/pick capture surfaces too — while one is open
        // every key, including the tab-switch chars (`B`/`C`/`,`/…) and `q`, is
        // input, not a nav key (a card titled `Cardrun` must not switch to Control
        // on its `C`). Route straight to the boards reducer, which owns Esc-cancel.
        if matches!(app.screen, Screen::Boards) && self.screens.boards.overlay().is_some() {
            let _ = route_key(&app, &mut self.screens, key);
            return;
        }
        // Fleet broadcast and confirmation modes own every key. This keeps text
        // and typed confirmations from leaking into global tab or quit routing.
        if matches!(app.screen, Screen::Fleet) && self.screens.fleet.is_modal_open() {
            let _ = route_key(&app, &mut self.screens, key);
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

        // P2: on the control center, the digit keys answer the selected ASK's
        // options inline (①②③) and MUST NOT be swallowed by the numbered
        // tab-switch (`1`..`4`). They only intercept when the selected card is an
        // answerable ASK; on an idle board a digit still falls through to the tab
        // router, so number-key tab navigation off the control center keeps working
        // (mirrors the issue-list free-text-capture guard above).
        if matches!(app.screen, Screen::ControlCenter) {
            if let KeyCode::Char { ch } = key.code {
                if ch.is_ascii_digit()
                    && ch != '0'
                    && self
                        .screens
                        .control_center
                        .selected_card()
                        .is_some_and(|c| c.kind.is_answerable())
                {
                    let _ = route_key(&app, &mut self.screens, key);
                    return;
                }
            }
        }

        // Routing-layer keys: tab switches, `?` help, Esc-close-modal, `q` quit.
        if let Some(ev) = routing_event(key, &app) {
            let reduction = crate::screen::reduce(&app, ev);
            // `q` reduces to `Intent::Quit`. The plugin can't quit the host, so
            // arm a `ui.close_request` (drained in `render`) instead of dropping
            // the intent — otherwise `q` is dead on every hangar screen.
            if matches!(reduction.intent, Some(crate::screen::Intent::Quit)) {
                self.close_request_pending = true;
            }
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
                // The filter-chip bar sits on the row above the board (chip row
                // = body top, i.e. board `top - 1`). Record one `Target::Tab` per
                // chip so a click selects that filter — the chip bar is chrome,
                // not part of the board layout, so it is pushed here. Geometry
                // mirrors `render_chip_bar`: each chip is `[Label] ` starting at
                // x=0, the clickable span being `[Label]` (bracket + label +
                // bracket) with a one-column trailing gap.
                let chip_row = top.saturating_sub(1);
                let mut chip_x = 0u16;
                for (idx, chip) in
                    crate::screen::issue_list::FilterChip::all().into_iter().enumerate()
                {
                    if chip_x >= w {
                        break;
                    }
                    let span =
                        u16::try_from(chip.label().chars().count()).unwrap_or(0).saturating_add(2);
                    self.hit_map.push(
                        crate::mouse::Rect::new(chip_x, chip_row, span, 1),
                        crate::mouse::Target::Tab(idx),
                    );
                    chip_x = chip_x.saturating_add(span).saturating_add(1);
                }
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
    /// `SwitchTab` selects the clicked filter chip (via the issue-list
    /// `SetFilter` reducer). The remaining intents (`NewIssue`, `FocusColumn`,
    /// `PanColumns`) land in later board sub-beads (the seeded create flow); they
    /// are consumed here so they never accumulate.
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
                    self.screens.issue_list.reorder_within_column(&issue_id, to_index);
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
                // A chip-bar click selects that filter chip (All / Members /
                // Agents / Mine) — fold it through the issue-list reducer so the
                // keyboard `Tab` path and the mouse path converge on `SetFilter`.
                MouseIntent::SwitchTab(idx) => {
                    use crate::screen::issue_list::{
                        FilterChip, IssueListEvent, reduce_issue_list,
                    };
                    if let Some(chip) = FilterChip::all().get(idx).copied() {
                        let out = reduce_issue_list(
                            &self.screens.issue_list,
                            IssueListEvent::SetFilter(chip),
                        );
                        self.screens.issue_list = out.state;
                    }
                }
                // The remaining intents land in later board sub-beads.
                MouseIntent::NewIssue(_)
                | MouseIntent::FocusColumn(_)
                | MouseIntent::PanColumns { .. } => {}
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
            subscriber_count: 0,
            subscribed: false,
            reactions: Vec::new(),
            properties: Vec::new(),
            metadata: Vec::new(),
            last_dispatch_reason: None,
            last_dispatch_detail: None,
            last_dispatch_at: None,
            origin_type: None,
            origin_id: None,
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
            // Carry the card's captured PR so the detail shows the SAME gold PR
            // badge the card does — the branch line then reads UNDER the badge
            // (agents-in-a-box-ch3), never a lone branch with a dropped PR.
            pr_url: card.pr_url.clone(),
            // The card's run branch (ch3) — mirrored onto the row so the issue
            // carries it too; the detail below is seeded from the same value.
            branch: card.branch.clone(),
            // 63d: the Kanban-synthesized header carries no card-parity / run
            // summary of its own (the daemon owns those on the real issue row).
            repo_ref: None,
            agent: None,
            source_branch: None,
            target_branch: None,
            external_ref: None,
            run_count: 0,
            last_run_status: None,
            last_run_at: None,
            parent_id: None,
            child_total: 0,
            child_done: 0,
            // The Kanban-synthesized header carries no acceptance / context lists of
            // its own (the daemon owns those on the real issue row).
            acceptance_criteria: Vec::new(),
            acceptance: Vec::new(),
            context_refs: Vec::new(),
            dependencies: Vec::new(),
        };
        // Seed the run's branch (tcp T2, agents-in-a-box-ch3) from the clicked
        // card so the detail view surfaces `ainb/<slug>` exactly as the card does.
        self.screens.open_task_detail(tid.clone(), issue, card.branch.clone());
        // Seed the card's already-fetched PR CI/merge status so the badge renders
        // the real state (not a muted unknown) with no extra round-trip.
        if let Some(status) = card.pr_status {
            self.screens.set_task_detail_pr_status(status);
        }
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
        let Some(skill) =
            self.screens.skill_manager.visible_skills().into_iter().find(|s| s.slug == slug)
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
        let display_id = row.display_id.clone().unwrap_or_else(|| row.id.as_str().to_string());
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
            // Delete routes into the issue list's `x` RED confirm overlay (the SAME
            // `hangar/issue_delete` path the keyboard `x` uses) — never an inline
            // delete. The overlay's Enter arms `pending_delete_action`, drained +
            // fired in `render`.
            ContextMenuIntent::Delete { issue_id } => {
                self.screens.issue_list.open_confirm_delete_for(&issue_id);
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
            // multica parity #13: open the card's activity timeline AND arm the
            // fetch — the modal opens immediately in its loading state and the
            // `render` pass fires `hangar/issue_timeline`.
            NavIntent::OpenActivityTimeline(issue_id) => {
                let title = self
                    .screens
                    .issue_list
                    .visible_rows()
                    .find(|r| r.id == issue_id)
                    .map_or_else(|| issue_id.as_str().to_string(), |r| r.title.clone());
                self.screens.activity = Some(crate::screen::activity::ActivityState::loading(
                    &issue_id, title,
                ));
                self.screens.pending_activity_fetch = Some(issue_id.as_str().to_string());
                let reduction =
                    crate::screen::reduce(app, AppEvent::OpenActivityTimeline(issue_id));
                self.app = Some(reduction.state);
            }
            NavIntent::OpenTaskForIssue(issue_id) => {
                // Open task detail bound to the issue's row + the running task.
                let issue =
                    self.screens.issue_list.visible_rows().find(|r| r.id == issue_id).cloned();
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
                    // ch3: the issue-list open is a synthetic task with no per-run
                    // branch of its own, so seed the detail from the issue row's
                    // `branch` — the daemon derives it from the issue's latest
                    // completed task (mirroring `pr_url`), so the branch line reads
                    // on the issue-list-opened detail exactly as on the Kanban path.
                    let branch = issue.branch.clone();
                    self.screens.open_task_detail(task_id.clone(), issue, branch);
                    let mut next = app.clone();
                    next.screen = Screen::TaskDetail(task_id.clone());
                    next.selected_task = Some(task_id);
                    next.prior_screen = None;
                    self.app = Some(next);
                }
            }
            NavIntent::MarkIssueDone(issue_id) => {
                // 0046: `d` marks the highlighted issue Done through the SAME seam
                // the context-menu `Move to ▸ Done` uses: move the card
                // optimistically (so the board reflects it at once) AND arm the
                // durable `hangar/issue_update{state:"done"}` RPC (drained + fired
                // in `render`). On the daemon that terminal transition fires the
                // child-done → parent cascade for a sub-issue.
                let to_status = ainb_hangar_proto::lifecycle::IssueLifecycle::Done;
                if let Some(moved) =
                    self.screens.issue_list.move_issue_to(issue_id.as_str(), to_status)
                {
                    self.pending_issue_state_update = Some((moved, to_status));
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
            NavIntent::BackToIssueList => {
                let mut next = app.clone();
                next.screen = Screen::IssueList;
                next.prior_screen = None;
                self.app = Some(next);
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
    // A CTRL/ALT chord is never a tab switch: `Ctrl+K` / `Alt+D` belong to the
    // active screen (or to the host), so the routing layer must not steal them.
    // SHIFT is untouched — the claimed chars are already uppercase.
    let chorded = key.mods & (ainb_plugin_sdk::KEY_MOD_CTRL | ainb_plugin_sdk::KEY_MOD_ALT) != 0;
    match &key.code {
        // `3`/`4` are the renumbered Skills/Autopilots tab keys after the old
        // `[3]Agents` tab folded into the issue-list filter chip (e38.38); the
        // numbered tabs are now contiguous `1`→`4`. The claimed set lives once,
        // in `screen::router::ROUTER_KEYS`, so screens can assert against it
        // (`no_screen_binds_a_reserved_key`, #450).
        KeyCode::Char { ch } if !chorded && crate::screen::router::is_router_key(*ch) => {
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
        if std::mem::take(&mut self.fleet_fetch_pending) {
            self.fetch_fleet_snapshot(host).await;
        }
        if std::mem::take(&mut self.fleet_subscribe_pending) {
            self.subscribe_fleet(host).await;
        }
        Ok(())
    }

    async fn handle_key(&mut self, _host: &HostClient, params: HandleKeyParams) -> Result<()> {
        // Only initial presses drive the reducers; ignore auto-repeat/release so a
        // held key doesn't multi-fire a tab switch.
        if matches!(params.key.kind, ainb_plugin_sdk::KeyKind::Release) {
            return Ok(());
        }
        // Remember the focused screen-id so a `q`-armed `ui.close_request` (drained
        // in `render`, which lacks the screen-id) can name the surface to pop.
        self.current_screen_id.clone_from(&params.screen_id);
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
        //
        // A Boards card overlay repaints via a one-shot boards_list refresh
        // round-trip (ccc / lu5) — the reply's dirty-kick drives the next frame —
        // so it is deliberately NOT a level-triggered redraw here (that would
        // self-render every frame for the overlay's whole lifetime).
        !self.pending_mouse_intents.is_empty()
            || self.context_menu.is_some()
            || self.pending_issue_priority_update.is_some()
            || self.pending_issue_assignee_update.is_some()
            || !self.pending_board_mouse_intents.is_empty()
            || self.list_context_menu.is_some()
            // Post-`[s]` redial window: level-triggered ON PURPOSE (bounded to
            // START_REDIAL_WINDOW) so `pump_start_redial` runs without further
            // input — renders were the only place host IO is safe, and with no
            // frames the daemon coming up was never noticed.
            || self.daemon_start_redial_until.is_some()
            // Phase 5: a wizard dispatch armed by the `issue_create` reply needs a
            // render to fire its `issue_update` + `issue_run` (host IO is only
            // safe there). Consumed (taken) by that render, so not level-held.
            || self.pending_issue_dispatch.is_some()
            || self.screens.pending_fleet_intent.is_some()
    }

    fn captures_text(&self) -> bool {
        // 8hx: declare to the host when a focused surface is capturing free text
        // (a title / filter / compose / API-key / search input) so it suppresses
        // its own global single-character shortcuts (`H`/`?`/`W`) and forwards
        // those keys into the input instead of eating them — e.g. a card titled
        // `Help?` must type verbatim, not toggle the host help overlay.
        //
        // This MIRRORS the text-capture routing guards in `handle_key` 1:1:
        // every surface there that routes keys straight to its reducer as typed
        // content (bypassing the tab-switch / help / quit nav layer) reports
        // here. Keep the two lists in sync — a new capture surface must be added
        // to both, or the host will swallow keystrokes bound for it.
        //
        // The command palette (`Ctrl+P`) is a modal text filter layered over any
        // screen, so it short-circuits before the per-screen match.
        if self.screens.command_palette.is_some() {
            return true;
        }
        let Some(app) = self.app.as_ref() else {
            return false;
        };
        match &app.screen {
            Screen::IssueList => self.screens.issue_list.is_capturing_text(),
            Screen::TaskDetail(_) => self
                .screens
                .task_detail
                .as_ref()
                .is_some_and(|td| td.compose_buffer().is_some()),
            // All THREE Settings capture surfaces: the key-entry modal, the
            // Daemon-section numeric-config overlay, AND the new-workspace name
            // modal (kept in sync with the routing guard in `on_key`).
            Screen::Settings => self.screens.settings.as_ref().is_some_and(|s| {
                s.key_entry_open()
                    || s.config_input_buffer().is_some()
                    || s.workspace_name_input().is_some()
            }),
            Screen::Squads => self.screens.squads.is_capturing(),
            // Every open Boards overlay (create-title / profile-pick / column
            // rename / `Run ▾`) consumes all keys as input, per its routing guard.
            Screen::Boards => self.screens.boards.overlay().is_some(),
            Screen::Fleet => self.screens.fleet.is_capturing_text(),
            _ => false,
        }
    }

    async fn render(&mut self, host: &HostClient, params: RenderParams) -> Result<WireBuffer> {
        if let Some(intent) = self.screens.take_pending_fleet_intent() {
            self.apply_fleet_intent(host, intent).await;
        }
        // Drain any deferred Workspace-pane action here: `plugin/render` is
        // dispatched on a SPAWNED task (unlike the inline `handle_key`/
        // `handle_event`), so the SDK reader loop stays free to deliver the
        // host-cap response — awaiting a host request here can't deadlock.
        if let Some(action) = self.screens.take_pending_ws_action() {
            self.apply_workspace_action(host, action).await;
        }
        // tcp T5: drain any deferred notify-rule RPC (grid fetch on section entry
        // or a rule set from a toggled cell) and fire it over the daemon socket.
        if let Some(action) = self.screens.take_pending_notify_action() {
            self.apply_notify_action(host, action).await;
        }
        // Drain a deferred daemon-config write (bool/enum/int edit) and fire it over
        // the daemon socket; the reply re-fetches the whole config so the pane
        // reflects the persisted value.
        for (key, value) in self.screens.take_pending_daemon_config_sets() {
            let mut sent = false;
            if let Some(stream_id) = self.conn.stream_id().map(ToString::to_string) {
                let body = encode_request(
                    DAEMON_CONFIG_SET_REQ_ID,
                    daemon_methods::HANGAR_DAEMON_CONFIG_SET,
                    serde_json::json!({ "key": key, "value": value }),
                );
                if let Ok(body) = body {
                    match host.unix_socket_send(stream_id, body).await {
                        Ok(()) => sent = true,
                        Err(e) => {
                            let _ = host
                                .log_info(format!("hangar: daemon_config set failed: {e}"))
                                .await;
                        }
                    }
                }
            }
            // On a successful send the SET reply re-fetches (via `fetch_pending`).
            // If the write never left the plugin (disconnected / encode / send
            // error), re-fetch here so the pane reconciles to the persisted value
            // instead of showing the optimistic edit forever.
            if !sent {
                self.fetch_pending = true;
                self.conn.on_event();
            }
        }
        // P6.5: drain any deferred skill RPC (sync / detail / attach / detach)
        // raised by the skill-manager screen and fire it over the daemon socket.
        if let Some(action) = self.screens.take_pending_skill_action() {
            self.apply_skill_action(host, action).await;
        }
        // Parity #24: after any attach / detach / toggle reply, re-read the
        // selected agent's links so the ` (disabled)` markers stay honest.
        self.drain_agent_skill_links_refresh(host).await;
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
        // Drain any deferred manual task-retry (`R` on a terminal card in the Task
        // Kanban failed column / task-detail) and fire `hangar/task_retry` over the
        // daemon socket; the daemon's TaskQueued push re-fetches the board so the
        // fresh queued attempt appears.
        if let Some(task_id) = self.screens.take_pending_task_retry_action() {
            self.apply_task_retry_action(host, task_id).await;
        }
        // P4 / D8: drain any deferred board mutation (`⇧←/→`, `x`, `n`, `m`) raised
        // by the Boards screen and fire the matching `hangar/board_*` over the
        // daemon socket; the refreshed BoardsListResult reply re-renders the board.
        if let Some(action) = self.screens.take_pending_boards_action() {
            self.apply_boards_action(host, action).await;
        }
        // P7 / D17: drain any deferred squad mutation (`c`, `a`, `d`, `x`) raised by
        // the Squads screen and fire the matching `hangar/squad_*` over the daemon
        // socket; the create/add/remove reply re-renders the list, the assign
        // (`squad_fanout`) surfaces the leader+members brief note.
        if let Some(action) = self.screens.take_pending_squads_action() {
            self.apply_squad_action(host, action).await;
        }
        // Slice 2: drain any deferred agent mutation (`n` create / `x` delete) raised
        // by the Agents roster and fire `hangar/agent_create` / `hangar/agent_delete`
        // over the daemon socket; both replies fold the refreshed roster back.
        if let Some(action) = self.screens.take_pending_agents_action() {
            self.apply_agents_action(host, action).await;
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
        // #11-rest: drain a deferred acceptance-criterion tick (`t` on the
        // task-detail card) and fire `hangar/issue_criterion_set`.
        if let Some(action) = self.screens.take_pending_criterion_action() {
            self.apply_criterion_action(host, action).await;
        }
        // Phase 5: drain a deferred wizard create (Enter on the Agent stage) and
        // fire `hangar/issue_create` over the daemon socket; the reply arms the
        // follow-up dispatch below.
        if let Some(action) = self.screens.take_pending_create_action() {
            self.apply_create_action(host, action).await;
        }
        // 63d: drain a deferred issue delete (Enter on the `x` confirm overlay) and
        // fire `hangar/issue_delete` over the daemon socket; the reply's
        // `IssueDeleted` push drops the row, and an error surfaces as a note.
        if let Some(issue_id) = self.screens.take_pending_delete_action() {
            self.apply_delete_action(host, issue_id).await;
        }
        // Drain a deferred "cancel run(s) & delete" (confirm on the active-tasks
        // overlay) and fire `hangar/issue_cancel_active`; its reply retries the
        // delete once the run(s) are cancelled.
        if let Some(issue_id) = self.screens.take_pending_cancel_delete_action() {
            self.apply_cancel_delete_action(host, issue_id).await;
        }
        // Phase 5: drain a dispatch armed by a successful wizard `issue_create`
        // reply — fire `hangar/issue_update` (persist repo / agent / branches on
        // the new issue) then `hangar/issue_run` (the actual launch).
        if let Some((issue_id, dispatch)) = self.pending_issue_dispatch.take() {
            self.fire_issue_dispatch(host, issue_id, dispatch).await;
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
        // multica parity #13: drain a deferred activity-timeline fetch (armed by
        // `y` opening the modal, or `r` inside it) and fire
        // `hangar/issue_timeline`. The reply folds the merged narrative in.
        if let Some(issue_id) = self.screens.pending_activity_fetch.take() {
            self.fire_issue_timeline(host, issue_id).await;
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
        // P2: drain a deferred `attention/answer` (Enter / a number key on an ASK
        // in the control center) and fire it over the daemon socket. The daemon
        // runs the first-answer-wins + C1 guards and delivers the pick into the
        // raising session; its reply re-fetches the fleet-wide attention list so
        // the answered card drops off the board.
        if let Some(action) = self.screens.take_pending_answer_action() {
            self.answer_attention(host, action).await;
        }
        // P5: drain a deferred `profile/get` (selection moved to an un-loaded row
        // in the profile editor) and fire it so the detail + both compile previews
        // fetch.
        if let Some(slug) = self.screens.take_pending_profile_detail() {
            self.fetch_profile_detail(host, slug).await;
        }
        // P5: drain a deferred `profile/upsert` (`t` cycled the selected tier) and
        // fire it, then re-arm a `profile/get` for the same slug so both previews
        // re-resolve their concrete model from the persisted tier.
        if let Some((slug, tier)) = self.screens.take_pending_profile_upsert() {
            self.upsert_profile_tier(host, slug.clone(), tier).await;
            self.fetch_profile_detail(host, slug).await;
        }
        // e38.36: drain a deferred offline `[s]` daemon-start (armed in
        // `handle_key`). `render` runs on a spawned task, so the host-shell start
        // + re-dial handshake (awaiting host caps) can't deadlock the reader loop.
        if std::mem::take(&mut self.start_daemon_pending) {
            self.start_daemon_and_redial(host).await;
        }
        // Drain a deferred quit (`q` → `Intent::Quit`, armed in `handle_key`). The
        // plugin can't quit the host; it asks the host to pop this panel back to
        // wherever it was opened. Best-effort: if the publish is lost the user
        // just presses `q` again.
        if std::mem::take(&mut self.close_request_pending) {
            let req = ainb_plugin_sdk::topics::UiCloseRequest {
                screen_id: self.current_screen_id.clone(),
            };
            let payload = serde_json::to_vec(&req).unwrap_or_default();
            let _ = host.snapshot_publish(ainb_plugin_sdk::topics::UI_CLOSE_REQUEST, payload).await;
        }
        // Keep re-dialing after a `[s]` start until the daemon binds or the
        // window expires (`wants_redraw` keeps frames coming meanwhile).
        self.pump_start_redial(host).await;
        // P5.6: persist the first-run ack here (deferred from `handle_key`). The
        // modal is already `Dismissed` in state; this records it so a relaunch
        // skips the warning. An IO fault is logged, not fatal.
        if std::mem::take(&mut self.first_run_ack_pending) {
            if let Some(path) = firstrun::state_path() {
                if let Err(e) = firstrun::ack_first_run(&path) {
                    let _ =
                        host.log_info(format!("hangar: first-run ack persist failed: {e}")).await;
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
            self.apply_issue_state_update(host, issue_id, to_status).await;
        }
        // 63l.5: fire any context-menu priority / assignee edit armed by a leaf.
        // Both reuse the `hangar/issue_update` seam (the daemon's `IssueUpdated`
        // push reconciles the optimistic local state), mirroring the state-move RPC.
        if let Some((issue_id, priority)) = self.pending_issue_priority_update.take() {
            self.apply_issue_priority_update(host, issue_id, priority).await;
        }
        if let Some((issue_id, actor_ref)) = self.pending_issue_assignee_update.take() {
            self.apply_issue_assignee_update(host, issue_id, actor_ref).await;
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

    /// Both inbox RPCs must carry the local human as the `recipient`, or the
    /// daemon silently answers with `member:me`'s inbox by default while the
    /// screen claims to be someone else's.
    ///
    /// MUTATION GUARD: dropping `recipient` from [`inbox_params`] fails this,
    /// and both `hangar/inbox_list` and `hangar/inbox_mark_read` are encoded
    /// from it, so the wire shape is pinned end to end.
    #[test]
    fn inbox_requests_name_the_local_human_as_recipient() {
        let params = inbox_params("default");
        assert_eq!(params["workspace_id"], "default");
        assert_eq!(
            params["recipient"], SELF_AUTHOR_REF,
            "the inbox request must name whose inbox it reads: {params}"
        );

        for method in [
            daemon_methods::HANGAR_INBOX_LIST,
            daemon_methods::HANGAR_INBOX_MARK_READ,
        ] {
            let frame = encode_request(29, method, inbox_params("default")).expect("encode");
            let body = String::from_utf8(frame).expect("utf8 frame");
            assert!(
                body.contains("\"recipient\":\"member:me\""),
                "{method} must carry the recipient on the wire: {body}"
            );
        }
    }

    #[test]
    fn manifest_returns_canonical_toml() {
        let p = HangarPlugin::new();
        assert_eq!(p.manifest(), MANIFEST_TOML);
        assert!(p.manifest().contains("name = \"hangar-tui\""));
    }

    /// Process-wide mutex serialising `$AINB_HANGAR_HOME` mutations the dial-path
    /// test performs; cargo runs tests in-process + parallel and
    /// `daemon_socket_path` reads the live env, so an unguarded `set_var` races.
    static DIAL_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Finding #1: the unexpanded dial-path string the plugin sends to
    /// `host/unix_socket_dial` must track `$AINB_HANGAR_HOME`: the
    /// `${AINB_HANGAR_HOME}/hangar.sock` template when it is set (so the host
    /// expands it to the moved socket), else the `~/.agents-in-a-box` default.
    /// Both forms are on the manifest allow-list, so whichever is returned
    /// passes the host's cap gate.
    ///
    /// Mutation-check: collapsing `daemon_socket_path` to always return
    /// `DEFAULT_DAEMON_SOCKET_PATH` (deleting the env branch) makes the
    /// set-branch assertion fail — the override case would wrongly dial `~`.
    #[test]
    fn daemon_socket_path_tracks_hangar_home_env() {
        let _guard = DIAL_ENV_LOCK.lock().unwrap();
        let prior = std::env::var_os(ainb_hangar_core::paths::HANGAR_HOME_ENV);

        // Override set + non-empty → the host-expanded template (NOT pre-expanded
        // by the plugin, and NOT the `~` default).
        std::env::set_var(ainb_hangar_core::paths::HANGAR_HOME_ENV, "/tmp/custom-home");
        assert_eq!(
            daemon_socket_path(),
            OVERRIDE_DAEMON_SOCKET_PATH,
            "a set $AINB_HANGAR_HOME must dial the ${{AINB_HANGAR_HOME}} template"
        );
        assert_eq!(daemon_socket_path(), "${AINB_HANGAR_HOME}/hangar.sock");

        // Empty override is ignored → falls back to the `~` default.
        std::env::set_var(ainb_hangar_core::paths::HANGAR_HOME_ENV, "");
        assert_eq!(
            daemon_socket_path(),
            DEFAULT_DAEMON_SOCKET_PATH,
            "an empty $AINB_HANGAR_HOME must fall back to the ~ default"
        );

        // Unset → the `~` default.
        std::env::remove_var(ainb_hangar_core::paths::HANGAR_HOME_ENV);
        assert_eq!(
            daemon_socket_path(),
            DEFAULT_DAEMON_SOCKET_PATH,
            "an unset $AINB_HANGAR_HOME must dial the ~/.agents-in-a-box default"
        );

        match prior {
            Some(v) => std::env::set_var(ainb_hangar_core::paths::HANGAR_HOME_ENV, v),
            None => std::env::remove_var(ainb_hangar_core::paths::HANGAR_HOME_ENV),
        }
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
            m.capabilities.spawn_managed_subprocess.allow_list().unwrap(),
            ["ainb-hangar-daemon"]
        );
        assert_eq!(
            m.capabilities.unix_socket_dial.allow_list().unwrap(),
            [
                "~/.agents-in-a-box/hangar.sock",
                "${AINB_HANGAR_HOME}/hangar.sock",
                "${XDG_RUNTIME_DIR}/ainb-hangar.sock"
            ]
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

    /// A `TaskMessage` (transcript line) must NOT arm a snapshot re-pull — the
    /// timeline live-appends it locally (F6), so a fanout per streamed line would
    /// hammer the daemon. Every OTHER event still arms the reconciling re-fetch.
    #[test]
    fn task_message_event_skips_the_snapshot_refetch() {
        use ainb_hangar_core::ids::{IssueId, TaskId};
        use ainb_hangar_proto::events::{EVENT_METHOD, MessageKind};

        let frame = |ev: &HangarEvent| serde_json::json!({ "method": EVENT_METHOD, "params": serde_json::to_value(ev).unwrap() });

        // A transcript line does not arm a re-fetch.
        let mut p = HangarPlugin::new();
        p.on_daemon_event(&frame(&HangarEvent::TaskMessage {
            task_id: TaskId::from_str("t1").unwrap(),
            kind: MessageKind::Agent,
            body: "streaming line".into(),
        }));
        assert!(
            !p.fetch_pending,
            "a TaskMessage transcript line must not re-pull snapshots"
        );

        // A status-relevant event (issue deleted) DOES arm the reconciling re-fetch.
        let mut p = HangarPlugin::new();
        p.on_daemon_event(&frame(&HangarEvent::IssueDeleted {
            issue_id: IssueId::from_str("i1").unwrap(),
        }));
        assert!(
            p.fetch_pending,
            "a non-transcript event must arm the reconciling re-fetch"
        );
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

    /// USER-VISIBLE PROOF (key dispatch): `q` arms a `ui.close_request` so the
    /// host pops the hangar panel back. Regression guard — the router computed
    /// `Intent::Quit` but the routing branch dropped it, so `q` was dead on every
    /// hangar screen (e.g. Daemon-health) and only `Ctrl+C` escaped.
    #[test]
    fn q_key_arms_close_request() {
        let mut p = HangarPlugin::new();
        assert!(!p.close_request_pending);
        p.on_key(&char_press('q'));
        assert!(
            p.close_request_pending,
            "`q` must arm a ui.close_request instead of being swallowed"
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
            p.daemon_start_error.as_deref().is_some_and(|m| m.contains("start failed")),
            "failure must be recorded for the empty-state, got {:?}",
            p.daemon_start_error
        );
    }

    /// A successful `[s]` start arms the bounded redial window (and the
    /// level-triggered `wants_redraw` that pumps it), so the plugin keeps
    /// re-dialing while the daemon boots instead of dialing exactly once and
    /// sitting on the offline panel forever.
    #[test]
    fn successful_start_arms_redial_window() {
        let dir = tempfile::tempdir().unwrap();
        let probe = dir.path().join("started.txt");
        let mut p = HangarPlugin::with_daemon_starter(Box::new(
            crate::shell::RecordingDaemonStarter::new(&probe),
        ));
        assert!(p.daemon_start_redial_until.is_none());
        // Drive the start dispatch directly (the host round-trip of
        // `start_daemon_and_redial` is covered by the socket tests).
        assert!(p.try_start_daemon());
        p.daemon_start_redial_until =
            Some(std::time::Instant::now() + HangarPlugin::START_REDIAL_WINDOW);
        assert!(
            p.wants_redraw(),
            "the armed redial window must keep frames coming"
        );
    }

    /// A failed `[s]` start must NOT arm the redial window — there is nothing
    /// to wait for, and the error line already tells the user what happened.
    #[test]
    fn failed_start_leaves_redial_window_unarmed() {
        let mut p = HangarPlugin::with_daemon_starter(Box::new(crate::shell::FailingDaemonStarter));
        assert!(!p.try_start_daemon());
        assert!(p.daemon_start_redial_until.is_none());
        assert!(!p.wants_redraw());
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
            subscriber_count: 0,
            subscribed: false,
            reactions: Vec::new(),
            properties: Vec::new(),
            metadata: Vec::new(),
            last_dispatch_reason: None,
            last_dispatch_detail: None,
            last_dispatch_at: None,
            origin_type: None,
            origin_id: None,
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
            branch: None,
            repo_ref: None,
            agent: None,
            source_branch: None,
            target_branch: None,
            external_ref: None,
            run_count: 0,
            last_run_status: None,
            last_run_at: None,
            parent_id: None,
            child_total: 0,
            child_done: 0,
            acceptance_criteria: Vec::new(),
            acceptance: Vec::new(),
            context_refs: Vec::new(),
            dependencies: Vec::new(),
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
                subscriber_count: 0,
                subscribed: false,
                reactions: Vec::new(),
                properties: Vec::new(),
                metadata: Vec::new(),
                last_dispatch_reason: None,
                last_dispatch_detail: None,
                last_dispatch_at: None,
                origin_type: None,
                origin_id: None,
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
                branch: None,
                repo_ref: None,
                agent: None,
                source_branch: None,
                target_branch: None,
                external_ref: None,
                run_count: 0,
                last_run_status: None,
                last_run_at: None,
                parent_id: None,
                child_total: 0,
                child_done: 0,
                acceptance_criteria: Vec::new(),
                acceptance: Vec::new(),
                context_refs: Vec::new(),
                dependencies: Vec::new(),
            },
            IssueRow {
                subscriber_count: 0,
                subscribed: false,
                reactions: Vec::new(),
                properties: Vec::new(),
                metadata: Vec::new(),
                last_dispatch_reason: None,
                last_dispatch_detail: None,
                last_dispatch_at: None,
                origin_type: None,
                origin_id: None,
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
                branch: None,
                repo_ref: None,
                agent: None,
                source_branch: None,
                target_branch: None,
                external_ref: None,
                run_count: 0,
                last_run_status: None,
                last_run_at: None,
                parent_id: None,
                child_total: 0,
                child_done: 0,
                acceptance_criteria: Vec::new(),
                acceptance: Vec::new(),
                context_refs: Vec::new(),
                dependencies: Vec::new(),
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
        let before = p.screens.issue_list.selected_row().map(|r| r.id.as_str().to_string());

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
        let after = p.screens.issue_list.selected_row().map(|r| r.id.as_str().to_string());
        assert_eq!(
            before, after,
            "empty-space press leaves the selection alone"
        );
    }

    /// USER-VISIBLE PROOF (issue-list-filter-chip-unreachable): a left-click on a
    /// filter chip in the chip bar selects that filter. Before this fix the chip
    /// row had no mouse hit-test target (`Target::Tab` was never pushed and
    /// `SwitchTab` was a no-op), so a click fell through and the filter stayed on
    /// `All`. Drives the REAL plugin mouse path (`on_mouse` → FSM → intent stash
    /// → `drain_mouse_intents`).
    #[test]
    fn clicking_a_filter_chip_selects_it() {
        use crate::screen::issue_list::FilterChip;
        use ainb_plugin_sdk::{MouseButton, MouseKind};

        let mut p = connected_plugin_with_issue();
        p.rebuild_hit_map(120, 24);
        assert_eq!(p.screens.issue_list.filter(), FilterChip::All);

        // The chip bar renders on row 1 (board top - 1). `render_chip_bar` paints
        // "[All] [Members] …": the `[All] ` chip occupies cols 0..=5, so the
        // `[Members]` chip starts at col 6 — a click at col 7 lands inside it.
        p.on_mouse(mouse_at(
            MouseKind::Down {
                button: MouseButton::Left,
            },
            7,
            1,
        ));
        p.drain_mouse_intents();
        assert_eq!(
            p.screens.issue_list.filter(),
            FilterChip::Members,
            "a click on the Members chip must select the Members filter"
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
            subscriber_count: 0,
            subscribed: false,
            reactions: Vec::new(),
            properties: Vec::new(),
            metadata: Vec::new(),
            last_dispatch_reason: None,
            last_dispatch_detail: None,
            last_dispatch_at: None,
            origin_type: None,
            origin_id: None,
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
            branch: None,
            repo_ref: None,
            agent: None,
            source_branch: None,
            target_branch: None,
            external_ref: None,
            run_count: 0,
            last_run_status: None,
            last_run_at: None,
            parent_id: None,
            child_total: 0,
            child_done: 0,
            acceptance_criteria: Vec::new(),
            acceptance: Vec::new(),
            context_refs: Vec::new(),
            dependencies: Vec::new(),
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
            subscriber_count: 0,
            subscribed: false,
            reactions: Vec::new(),
            properties: Vec::new(),
            metadata: Vec::new(),
            last_dispatch_reason: None,
            last_dispatch_detail: None,
            last_dispatch_at: None,
            origin_type: None,
            origin_id: None,
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
            branch: None,
            repo_ref: None,
            agent: None,
            source_branch: None,
            target_branch: None,
            external_ref: None,
            run_count: 0,
            last_run_status: None,
            last_run_at: None,
            parent_id: None,
            child_total: 0,
            child_done: 0,
            acceptance_criteria: Vec::new(),
            acceptance: Vec::new(),
            context_refs: Vec::new(),
            dependencies: Vec::new(),
        }]);
        // The card starts in Backlog.
        assert_eq!(p.screens.issue_list.column_count(IssueColumn::Backlog), 1);
        assert_eq!(
            p.screens.issue_list.column_count(IssueColumn::InProgress),
            0
        );

        // Stash the MoveCard intent directly (the FSM origin is exercised by the
        // sibling drag test) and drain it.
        p.pending_mouse_intents.push(crate::mouse::MouseIntent::MoveCard {
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
                subscriber_count: 0,
                subscribed: false,
                reactions: Vec::new(),
                properties: Vec::new(),
                metadata: Vec::new(),
                last_dispatch_reason: None,
                last_dispatch_detail: None,
                last_dispatch_at: None,
                origin_type: None,
                origin_id: None,
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
                branch: None,
                repo_ref: None,
                agent: None,
                source_branch: None,
                target_branch: None,
                external_ref: None,
                run_count: 0,
                last_run_status: None,
                last_run_at: None,
                parent_id: None,
                child_total: 0,
                child_done: 0,
                acceptance_criteria: Vec::new(),
                acceptance: Vec::new(),
                context_refs: Vec::new(),
                dependencies: Vec::new(),
            })
            .collect();
        p.screens.set_issues(rows);

        // A wheel-scroll-down over the Todo column nudges its offset to 1.
        p.pending_mouse_intents.push(crate::mouse::MouseIntent::ScrollColumn {
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
                subscriber_count: 0,
                subscribed: false,
                reactions: Vec::new(),
                properties: Vec::new(),
                metadata: Vec::new(),
                last_dispatch_reason: None,
                last_dispatch_detail: None,
                last_dispatch_at: None,
                origin_type: None,
                origin_id: None,
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
                branch: None,
                repo_ref: None,
                agent: None,
                source_branch: None,
                target_branch: None,
                external_ref: None,
                run_count: 0,
                last_run_status: None,
                last_run_at: None,
                parent_id: None,
                child_total: 0,
                child_done: 0,
                acceptance_criteria: Vec::new(),
                acceptance: Vec::new(),
                context_refs: Vec::new(),
                dependencies: Vec::new(),
            },
            IssueRow {
                subscriber_count: 0,
                subscribed: false,
                reactions: Vec::new(),
                properties: Vec::new(),
                metadata: Vec::new(),
                last_dispatch_reason: None,
                last_dispatch_detail: None,
                last_dispatch_at: None,
                origin_type: None,
                origin_id: None,
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
                branch: None,
                repo_ref: None,
                agent: None,
                source_branch: None,
                target_branch: None,
                external_ref: None,
                run_count: 0,
                last_run_status: None,
                last_run_at: None,
                parent_id: None,
                child_total: 0,
                child_done: 0,
                acceptance_criteria: Vec::new(),
                acceptance: Vec::new(),
                context_refs: Vec::new(),
                dependencies: Vec::new(),
            },
        ]);
        p.screens.set_actors(vec![ActorRow {
            actor_ref: "member:alice".into(),
            display_name: "alice".into(),
            subtitle: "dev".into(),
            presence: PresenceState::Online,
            workload: ainb_hangar_proto::events::Workload::Idle,
            is_agent: false,
            recent_rank: Some(0),
            ..ActorRow::default()
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
        p.pending_mouse_intents.push(crate::mouse::MouseIntent::OpenContextMenu {
            issue_id: "card-b".into(),
            at: (40, 6),
        });
        p.drain_mouse_intents();
        assert!(
            p.context_menu.as_ref().is_some_and(|m| m.issue_id() == "card-b"),
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

    /// USER-VISIBLE PROOF (63l.5): `Delete` closes the menu and opens the issue
    /// list's `x` RED confirm overlay for THAT card — NOT an inline delete. A
    /// second Enter on the overlay then arms the SAME `pending_delete_action` the
    /// keyboard `x` path uses (the one deferred `hangar/issue_delete` seam).
    #[test]
    fn context_menu_delete_opens_confirm_overlay_then_arms_delete() {
        let mut p = connected_plugin_with_two_cards();
        p.open_context_menu("card-b", (40, 6));

        // Down x5 to `Delete` (Open→Move to→Priority→Assign→Copy id→Delete), Enter.
        for _ in 0..5 {
            p.on_key(&key_press(KeyCode::Down));
        }
        p.on_key(&key_press(KeyCode::Enter)); // fire Delete

        // The menu closed and NO delete fired yet — only the confirm overlay opened.
        assert!(p.context_menu.is_none(), "Delete closes the menu");
        assert!(
            p.screens
                .issue_list
                .confirm_delete()
                .is_some_and(|pd| pd.id.as_str() == "card-b"),
            "Delete opens the issue-list confirm overlay for card-b"
        );
        assert!(
            p.screens.pending_delete_action.is_none(),
            "no delete is armed until the overlay is confirmed"
        );

        // Enter on the open overlay confirms → arms the deferred issue_delete.
        p.on_key(&key_press(KeyCode::Enter));
        assert_eq!(
            p.screens.take_pending_delete_action().map(|id| id.as_str().to_string()),
            Some("card-b".to_string()),
            "confirming the overlay arms hangar/issue_delete for card-b"
        );
    }

    /// A `hangar/issue_delete` refused with the `active_tasks` marker arms the
    /// issue-list "cancel run(s) & delete" overlay for the in-flight issue —
    /// instead of dead-ending on a note.
    #[test]
    fn delete_refused_for_active_tasks_arms_cancel_delete_overlay() {
        let mut p = connected_plugin_with_two_cards();
        // Simulate a delete of card-b in flight (as apply_delete_action would stash).
        p.delete_in_flight = Some(ainb_hangar_core::ids::IssueId::from_str("card-b").unwrap());

        let resp = ainb_hangar_proto::RpcResponse {
            jsonrpc: "2.0".into(),
            id: RpcId::Number(ISSUE_DELETE_REQ_ID),
            result: None,
            error: Some(ainb_hangar_proto::RpcError {
                code: -32602,
                message: "1 active task(s) on this issue — cancel the run first, then delete"
                    .into(),
                data: Some(serde_json::json!({ "reason": "active_tasks", "active": 1 })),
            }),
        };
        p.on_daemon_response(&resp);

        assert!(
            p.screens
                .issue_list
                .confirm_cancel_delete()
                .is_some_and(|pd| pd.id.as_str() == "card-b"),
            "an active-tasks refusal arms the cancel-delete overlay for card-b"
        );
        assert!(
            p.delete_in_flight.is_none(),
            "the in-flight marker is consumed"
        );
    }

    /// A plain (non-active-tasks) delete error just surfaces a note — no overlay.
    #[test]
    fn delete_error_without_marker_only_notes() {
        let mut p = connected_plugin_with_two_cards();
        p.delete_in_flight = Some(ainb_hangar_core::ids::IssueId::from_str("card-b").unwrap());
        let resp = ainb_hangar_proto::RpcResponse {
            jsonrpc: "2.0".into(),
            id: RpcId::Number(ISSUE_DELETE_REQ_ID),
            result: None,
            error: Some(ainb_hangar_proto::RpcError {
                code: -32603,
                message: "store error: disk full".into(),
                data: None,
            }),
        };
        p.on_daemon_response(&resp);
        assert!(
            p.screens.issue_list.confirm_cancel_delete().is_none(),
            "a non-active-tasks error opens no overlay"
        );
    }

    /// A successful `hangar/issue_cancel_active` reply retries the delete: it arms
    /// `pending_delete_action` for the in-flight issue (cancel committed → delete).
    #[test]
    fn cancel_active_success_retries_the_delete() {
        let mut p = connected_plugin_with_two_cards();
        p.cancel_delete_in_flight =
            Some(ainb_hangar_core::ids::IssueId::from_str("card-b").unwrap());

        let resp = ainb_hangar_proto::RpcResponse {
            jsonrpc: "2.0".into(),
            id: RpcId::Number(ISSUE_CANCEL_ACTIVE_REQ_ID),
            result: Some(serde_json::json!({ "cancelled": 1 })),
            error: None,
        };
        p.on_daemon_response(&resp);

        assert_eq!(
            p.screens.take_pending_delete_action().map(|id| id.as_str().to_string()),
            Some("card-b".to_string()),
            "cancel success arms the delete retry for card-b (cancel before delete)"
        );
        assert!(
            p.cancel_delete_in_flight.is_none(),
            "the in-flight marker is consumed"
        );
    }

    /// A failed `hangar/issue_cancel_active` reply does NOT delete — it surfaces a
    /// note and leaves the issue intact.
    #[test]
    fn cancel_active_failure_does_not_delete() {
        let mut p = connected_plugin_with_two_cards();
        p.cancel_delete_in_flight =
            Some(ainb_hangar_core::ids::IssueId::from_str("card-b").unwrap());

        let resp = ainb_hangar_proto::RpcResponse {
            jsonrpc: "2.0".into(),
            id: RpcId::Number(ISSUE_CANCEL_ACTIVE_REQ_ID),
            result: None,
            error: Some(ainb_hangar_proto::RpcError {
                code: -32603,
                message: "cancel partially failed: 1 task(s) still active".into(),
                data: None,
            }),
        };
        p.on_daemon_response(&resp);

        assert!(
            p.screens.pending_delete_action.is_none(),
            "a failed cancel must NOT arm a delete"
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

    /// ccc (lu5): on the Boards screen, `c` opens the card-title input, and while
    /// it is open an uppercase `C` (my card title starts with one) is TYPED, not
    /// routed to the control-center tab-switch — proving the card interaction layer
    /// is wired end to end through the real key path (not just the pure reducer).
    #[test]
    fn boards_c_opens_card_title_and_captures_tab_chars() {
        use crate::screen::boards::BoardsOverlay;
        use ainb_hangar_proto::snapshots::{BoardColumnWireRow, BoardWireRow, BoardsListResult};
        let mut p = connected_plugin_with_issue();

        // Load a board with one column, then land on the Boards screen.
        p.screens.set_boards(&BoardsListResult {
            boards: vec![BoardWireRow {
                id: "b1".into(),
                name: "Delivery".into(),
                auto_move: true,
                columns: vec![BoardColumnWireRow {
                    id: "c-todo".into(),
                    name: "Todo".into(),
                    ord: 0,
                    fsm_state: None,
                    auto_move: false,
                    cards: Vec::new(),
                }],
                unmapped: Vec::new(),
            }],
        });
        let mut app = p.app_state().clone();
        app.screen = Screen::Boards;
        p.app = Some(app);

        // 8hx: on the Boards screen with NO overlay open, the plugin is navigable
        // — it does NOT declare text-capture, so the host keeps its globals.
        assert!(
            !p.captures_text(),
            "Boards without an open overlay must not declare text-capture"
        );

        // `c` opens the card-title input on the focused Todo column.
        p.on_key(&char_press('c'));
        assert!(
            matches!(
                p.screens.boards.overlay(),
                Some(crate::screen::boards::BoardsOverlay::CardTitle { column_id, .. })
                    if column_id == "c-todo"
            ),
            "`c` must open the card-title input, got {:?}",
            p.screens.boards.overlay()
        );

        // 8hx: the open card-title input IS a text-capture surface — the plugin
        // must now report capture so the HOST suppresses its global `H`/`?`/`W`
        // shortcuts and forwards those keys into this input instead.
        assert!(
            p.captures_text(),
            "an open card-title overlay must declare text-capture (8hx)"
        );

        // Typing the title's leading `C` must be captured, not switch to Control.
        p.on_key(&char_press('C'));
        assert!(
            matches!(p.app_state().screen, Screen::Boards),
            "typing `C` into the title must NOT switch to the control center, got {:?}",
            p.app_state().screen
        );

        // 8hx: the host-shortcut characters `H` and `?` — which the host would
        // otherwise eat as the help toggle — must land in the title VERBATIM once
        // the host forwards them (it does, because `captures_text()` is true).
        p.on_key(&char_press('H'));
        p.on_key(&char_press('?'));
        assert!(
            matches!(
                p.screens.boards.overlay(),
                Some(BoardsOverlay::CardTitle { title, .. }) if title == "CH?"
            ),
            "`H`/`?` must be typed into the card title verbatim, got {:?}",
            p.screens.boards.overlay()
        );
        assert!(
            matches!(p.app_state().screen, Screen::Boards),
            "typing `H`/`?` must not toggle help or leave the Boards screen, got {:?}",
            p.app_state().screen
        );
    }

    // ----- #450: advertised screen keys must survive the global router -----

    /// Seed a connected plugin parked on Boards with one board, one `Todo`
    /// column, and one focused card, plus a one-squad roster. Drives the REAL
    /// production key seam (`on_key`) in the tests below.
    fn plugin_on_seeded_board() -> HangarPlugin {
        use ainb_hangar_proto::snapshots::{
            BoardCardWireRow, BoardColumnWireRow, BoardWireRow, BoardsListResult, SquadWireRow,
            SquadsListResult,
        };
        let mut p = connected_plugin_with_issue();
        p.screens.set_boards(&BoardsListResult {
            boards: vec![BoardWireRow {
                id: "b1".into(),
                name: "Delivery".into(),
                auto_move: true,
                columns: vec![
                    BoardColumnWireRow {
                        id: "c-todo".into(),
                        name: "Todo".into(),
                        ord: 0,
                        fsm_state: None,
                        auto_move: false,
                        cards: vec![
                            BoardCardWireRow {
                                issue_id: "issue-1".into(),
                                title: "Refactor API".into(),
                                display_id: "1".into(),
                                state: None,
                                session_name: None,
                                repo_ref: None,
                                agent: None,
                                squad_id: None,
                                member_states: Vec::new(),
                                blocked_by: Vec::new(),
                                auto_run: false,
                                blocks: Vec::new(),
                                related: Vec::new(),
                            },
                            BoardCardWireRow {
                                issue_id: "issue-2".into(),
                                title: "Ship docs".into(),
                                display_id: "2".into(),
                                state: None,
                                session_name: None,
                                repo_ref: None,
                                agent: None,
                                squad_id: None,
                                member_states: Vec::new(),
                                blocked_by: Vec::new(),
                                auto_run: false,
                                blocks: Vec::new(),
                                related: Vec::new(),
                            },
                        ],
                    },
                    BoardColumnWireRow {
                        id: "c-done".into(),
                        name: "Done".into(),
                        ord: 1,
                        fsm_state: None,
                        auto_move: false,
                        cards: Vec::new(),
                    },
                ],
                unmapped: Vec::new(),
            }],
        });
        p.screens.set_squads(&SquadsListResult {
            squads: vec![SquadWireRow {
                id: "squad-1".into(),
                name: "Platform".into(),
                leader: "agent:a1".into(),
                members: vec!["agent:a1".into()],
                ..SquadWireRow::default()
            }],
        });
        let mut app = p.app_state().clone();
        app.screen = Screen::Boards;
        p.app = Some(app);
        p
    }

    /// THE #450 ACCEPTANCE PROOF: the advertised squad key opens the assign-squad
    /// picker on the focused card and commits an assignment — driven through the
    /// production `on_key` seam, not the pure reducer.
    ///
    /// Fails on `main`: the key was `q`, which the global router claims as quit,
    /// so the press armed `close_request_pending` and popped the whole panel
    /// instead. `BoardsEvent::AssignSquad` was unreachable from a real keypress.
    #[test]
    fn boards_squad_key_opens_the_picker_instead_of_quitting() {
        use crate::screen::boards::BoardsOverlay;
        let mut p = plugin_on_seeded_board();

        p.on_key(&char_press('s'));

        assert!(
            matches!(
                p.screens.boards.overlay(),
                Some(BoardsOverlay::SquadPick { issue_id, .. }) if issue_id == "issue-1"
            ),
            "the squad key must open the SquadPick picker on the focused card, got {:?}",
            p.screens.boards.overlay()
        );
        assert!(
            matches!(p.app_state().screen, Screen::Boards),
            "the squad key must not leave the Boards screen, got {:?}",
            p.app_state().screen
        );
        assert!(
            !p.close_request_pending,
            "the squad key must NOT arm a ui.close_request (that is the #450 bug)"
        );

        // Down onto the roster's first squad (row 0 is the "clear" row), Enter commits.
        p.on_key(&key_press(KeyCode::Down));
        p.on_key(&key_press(KeyCode::Enter));
        assert_eq!(
            p.screens.take_pending_boards_action(),
            Some(crate::screen::app_screens::BoardsAction::CardAssignSquad {
                board_id: "b1".into(),
                issue_id: "issue-1".into(),
                squad_id: Some("squad-1".into()),
            }),
            "Enter on a roster row must commit the squad assignment RPC"
        );
    }

    /// The escape hatch survives the rebind: bare `q` on Boards (no overlay) is
    /// still the global quit, arming a `ui.close_request` so the user is never
    /// trapped on a screen whose Esc is a no-op.
    #[test]
    fn boards_q_still_quits() {
        let mut p = plugin_on_seeded_board();
        p.on_key(&char_press('q'));
        assert!(
            p.close_request_pending,
            "`q` on Boards must still arm the ui.close_request escape hatch"
        );
        assert!(
            p.screens.boards.overlay().is_none(),
            "`q` must not open a boards overlay, got {:?}",
            p.screens.boards.overlay()
        );
    }

    /// The other two rebound Boards verbs are live through the real key seam:
    /// `w` opens the depends-on picker, `>` / `<` reorder the focused column.
    ///
    /// Fails on `main`: `D` switched to the daemon-health tab and `L`/`H` to the
    /// logs tab / host help toggle.
    #[test]
    fn boards_depends_on_and_reorder_keys_are_live() {
        use crate::screen::boards::BoardsOverlay;
        let mut p = plugin_on_seeded_board();

        p.on_key(&char_press('w'));
        assert!(
            matches!(
                p.screens.boards.overlay(),
                Some(BoardsOverlay::DepPick { dependent_issue_id, .. })
                    if dependent_issue_id == "issue-1"
            ),
            "`w` must open the depends-on picker, got {:?}",
            p.screens.boards.overlay()
        );
        assert!(matches!(p.app_state().screen, Screen::Boards));
        p.on_key(&key_press(KeyCode::Esc));
        let _ = p.screens.take_pending_boards_action();

        p.on_key(&char_press('>'));
        assert_eq!(
            p.screens.take_pending_boards_action(),
            Some(crate::screen::app_screens::BoardsAction::ColumnReorder {
                board_id: "b1".into(),
                column_ids: vec!["c-done".into(), "c-todo".into()],
            }),
            "`>` must lift a column-reorder RPC"
        );
        p.on_key(&char_press('<'));
        // Focus followed the dragged column to index 1, so `<` drags it back.
        // Assert the SHAPE (a reorder for this board over both columns), not a
        // frozen id order — the local board is not re-sorted until the daemon
        // answers, so the emitted order is the same swap either way.
        assert!(
            matches!(
                p.screens.take_pending_boards_action(),
                Some(crate::screen::app_screens::BoardsAction::ColumnReorder {
                    board_id,
                    column_ids,
                }) if board_id == "b1" && column_ids.len() == 2
            ),
            "`<` must lift a column-reorder RPC"
        );
        assert!(
            matches!(p.app_state().screen, Screen::Boards) && !p.close_request_pending,
            "the reorder keys must not navigate away or close the panel"
        );
    }

    /// THE GENERAL GUARD: every single-char key the Boards hint band ADVERTISES
    /// must actually reach the boards screen — pressing it may never switch tabs
    /// or close the panel.
    ///
    /// Fails on `main` for `q` (quit) and `D` (daemon health): the band was
    /// advertising keys the router ate first.
    #[test]
    fn every_boards_hint_band_key_is_reachable() {
        for (key, desc) in crate::screen::boards::BOARDS_HINTS {
            let mut chars = key.chars();
            let (Some(ch), None) = (chars.next(), chars.next()) else {
                continue; // compound / glyph hint (`↵`, `⇧←→`) — not a bare char
            };
            if !ch.is_ascii() {
                continue;
            }
            let mut p = plugin_on_seeded_board();
            p.on_key(&char_press(ch));
            assert!(
                matches!(p.app_state().screen, Screen::Boards),
                "Boards advertises `{ch}:{desc}` but pressing it left the screen \
                 (went to {:?}) — the router stole it",
                p.app_state().screen
            );
            assert!(
                !p.close_request_pending,
                "Boards advertises `{ch}:{desc}` but pressing it closed the panel — \
                 the router stole it as quit"
            );
        }
    }

    /// #450 (Fleet row): `a` on the Fleet pane raises the takeover-attach intent
    /// and stays on Fleet. It was `A` — which the router claims as the Agents tab,
    /// so the advertised `→/A:attach` navigated away instead of attaching.
    #[test]
    fn fleet_lowercase_a_attaches() {
        use crate::screen::fleet::{FleetCapabilities, FleetIntent, FleetSessionRow};
        let mut p = connected_plugin_with_issue();
        p.screens.fleet.set_sessions(vec![FleetSessionRow {
            session_key: "claude:ask".into(),
            provider: "claude".into(),
            provider_session_id: Some("provider-claude:ask".into()),
            current_request_fingerprint: None,
            current_request: None,
            lifecycle_state: "IDLE".into(),
            attention_state: "ASK".into(),
            management_state: "managed".into(),
            provenance: "hangar-authoritative".into(),
            confidence: "authoritative".into(),
            transport_health: "healthy".into(),
            capabilities: FleetCapabilities::List(vec!["tmux_attach".to_string()]),
            version: 7,
            cwd: "/work/claude".into(),
            tmux_target: Some("claude:ask:0.0".into()),
            display_name: Some("claude:ask".into()),
            repository_name: Some("claude".into()),
            branch_name: Some("main".into()),
            discovered_at: 1_000,
            last_observed_at: 9_000,
            metadata_updated_at: 9_000,
            lifecycle_updated_at: 9_000,
            attention_updated_at: 9_000,
            transport_updated_at: 9_000,
        }]);
        let mut app = p.app_state().clone();
        app.screen = Screen::Fleet;
        p.app = Some(app);

        p.on_key(&char_press('a'));

        assert!(
            matches!(p.app_state().screen, Screen::Fleet),
            "`a` must stay on Fleet, got {:?}",
            p.app_state().screen
        );
        assert_eq!(
            p.screens.take_pending_fleet_intent(),
            Some(FleetIntent::AttachFullscreen {
                session_key: "claude:ask".into(),
                tmux_target: "claude:ask:0.0".into(),
            }),
            "`a` on Fleet must raise the takeover-attach intent"
        );
    }

    /// #450 (Settings row): `]` / `[` move the in-section list selection. They
    /// were `J` / `K`, and bare `K` is the router's Kanban tab key — so the
    /// advertised in-section navigation half-worked at best.
    #[test]
    fn settings_bracket_keys_move_the_in_section_list() {
        let mut p = plugin_on_workspaces_settings();
        let start = p.screens.settings.as_ref().expect("settings seeded").list_selected();

        p.on_key(&char_press(']'));
        let moved = p.screens.settings.as_ref().expect("settings seeded").list_selected();
        assert_ne!(moved, start, "`]` must move the workspace-row selection");
        assert!(
            matches!(p.app_state().screen, Screen::Settings),
            "`]` must not leave the Settings screen, got {:?}",
            p.app_state().screen
        );

        p.on_key(&char_press('['));
        assert_eq!(
            p.screens.settings.as_ref().expect("settings seeded").list_selected(),
            start,
            "`[` must move the workspace-row selection back"
        );
        assert!(
            matches!(p.app_state().screen, Screen::Settings) && !p.close_request_pending,
            "`[` must not navigate away or close the panel"
        );
    }

    /// REGRESSION (P2 tab hotkeys): while the task-detail comment-compose modal
    /// is open, an uppercase `C` (common in prose / identifiers) must be TYPED
    /// into the draft, not routed to the control-center tab-switch. Before the
    /// text-capture guard the routing layer swallowed `C`/`U`/`I` first,
    /// navigating away and abandoning the compose draft.
    /// **2-rest** — a comment that mentioned NOBODY renders nothing, so a plain
    /// comment's transcript is byte-identical to what it was before this item.
    #[test]
    fn no_mentions_renders_no_transcript_line() {
        assert_eq!(super::render_mention_outcomes(&[]), None);
    }

    /// **2-rest** — every outcome is named, and a REFUSAL carries the reason's
    /// human label in parentheses. Asserted on the exact phrase, not a
    /// substring-OR: the parenthetical is the whole point of the item.
    #[test]
    fn outcomes_render_with_the_refusal_reason_spelled_out() {
        use ainb_hangar_proto::snapshots::MentionOutcomeRow;

        let row = |handle: &str, outcome: &str, reason: &str| MentionOutcomeRow {
            target_type: "agent".into(),
            target_id: "x".into(),
            handle: handle.into(),
            outcome: outcome.into(),
            reason: reason.into(),
            task_id: None,
            detail: String::new(),
            source: "explicit".into(),
        };
        let line = super::render_mention_outcomes(&[
            row("alice", "notified", ""),
            row("builder", "queued", "queued"),
            row("secret-bot", "blocked", "invocation_not_allowed"),
        ])
        .expect("a non-empty outcome set renders a line");
        assert_eq!(
            line,
            "↪ @alice notified · @builder queued · @secret-bot blocked (invocation not allowed)"
        );
    }

    /// **2-rest** — a `coalesced` row says so without a parenthetical (the
    /// bucket already carries the meaning), and a `deferred` row explains why.
    #[test]
    fn coalesced_is_bare_and_deferred_explains_itself() {
        use ainb_hangar_proto::snapshots::MentionOutcomeRow;

        let row = |outcome: &str, reason: &str| MentionOutcomeRow {
            target_type: "agent".into(),
            target_id: "x".into(),
            handle: "bot".into(),
            outcome: outcome.into(),
            reason: reason.into(),
            task_id: None,
            detail: String::new(),
            source: "explicit".into(),
        };
        assert_eq!(
            super::render_mention_outcomes(&[row("coalesced", "coalesced")]).unwrap(),
            "↪ @bot coalesced"
        );
        assert_eq!(
            super::render_mention_outcomes(&[row("deferred", "deferred")]).unwrap(),
            "↪ @bot deferred (waiting on blockers)"
        );
    }

    #[test]
    fn uppercase_c_types_into_task_detail_compose_not_tab_switch() {
        use ainb_hangar_proto::events::IssueRow;
        let mut p = connected_plugin_with_issue();

        // Open the task detail for a fresh issue.
        let issue = IssueRow {
            subscriber_count: 0,
            subscribed: false,
            reactions: Vec::new(),
            properties: Vec::new(),
            metadata: Vec::new(),
            last_dispatch_reason: None,
            last_dispatch_detail: None,
            last_dispatch_at: None,
            origin_type: None,
            origin_id: None,
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
            branch: None,
            repo_ref: None,
            agent: None,
            source_branch: None,
            target_branch: None,
            external_ref: None,
            run_count: 0,
            last_run_status: None,
            last_run_at: None,
            parent_id: None,
            child_total: 0,
            child_done: 0,
            acceptance_criteria: Vec::new(),
            acceptance: Vec::new(),
            context_refs: Vec::new(),
            dependencies: Vec::new(),
        };
        let tid = ainb_hangar_core::ids::TaskId::from_str("task-1").unwrap();
        p.screens.open_task_detail(tid.clone(), issue, None);
        let mut app = p.app_state().clone();
        app.screen = Screen::TaskDetail(tid);
        p.app = Some(app);

        // `c` (not a routing key) opens an empty compose modal.
        p.on_key(&char_press('c'));
        assert_eq!(
            p.screens.task_detail.as_ref().and_then(|td| td.compose_buffer()),
            Some(""),
            "`c` opens an empty compose modal"
        );

        // The regression key: uppercase `C` must insert, not navigate.
        p.on_key(&char_press('C'));
        assert!(
            matches!(p.app_state().screen, Screen::TaskDetail(_)),
            "typing `C` in the compose draft must NOT switch to the control center, got {:?}",
            p.app_state().screen
        );
        assert_eq!(
            p.screens.task_detail.as_ref().and_then(|td| td.compose_buffer()),
            Some("C"),
            "`C` must land in the draft, not be swallowed as a tab switch"
        );
    }

    /// REGRESSION (P2 tab hotkeys): while the settings key-entry (API-key) modal
    /// is open, an uppercase `C` (common in API keys / tokens) must extend the
    /// in-flight key value, not switch tabs — the same swallow the compose modal
    /// hit, on the second existing text-capture surface.
    #[test]
    fn uppercase_c_stays_in_settings_key_entry_not_tab_switch() {
        use crate::screen::settings::SettingsState;
        use ainb_hangar_proto::settings::HealthSnapshot;
        let mut p = connected_plugin_with_issue();

        // Seed a connected settings screen and land on it.
        let health = HealthSnapshot {
            socket_path: "/tmp/x.sock".into(),
            pid: 1,
            uptime_secs: 0,
            version: "test".into(),
            connected: true,
        };
        p.screens.settings = Some(SettingsState::new(
            health,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        ));
        let mut app = p.app_state().clone();
        app.screen = Screen::Settings;
        p.app = Some(app);

        // Navigate Daemon → Providers → Keys, then open the key-entry modal (`n`).
        p.on_key(&char_press('j'));
        p.on_key(&char_press('j'));
        p.on_key(&char_press('n'));
        assert!(
            p.screens.settings.as_ref().is_some_and(|s| s.key_entry_open()),
            "`n` on the Keys section opens the key-entry modal"
        );

        // The regression key: uppercase `C` must stay in the modal, not navigate.
        p.on_key(&char_press('C'));
        assert!(
            matches!(p.app_state().screen, Screen::Settings),
            "typing `C` in the key-entry modal must NOT switch to the control center, got {:?}",
            p.app_state().screen
        );
        assert!(
            p.screens.settings.as_ref().is_some_and(|s| s.key_entry_open()),
            "the key-entry modal stays open while typing the key"
        );
    }

    /// Seed a plugin sitting on the Settings screen's Daemon section.
    fn plugin_on_daemon_settings() -> HangarPlugin {
        use crate::screen::settings::SettingsState;
        use ainb_hangar_proto::settings::HealthSnapshot;
        let mut p = connected_plugin_with_issue();
        let health = HealthSnapshot {
            socket_path: "/tmp/x.sock".into(),
            pid: 1,
            uptime_secs: 0,
            version: "test".into(),
            connected: true,
        };
        p.screens.settings = Some(SettingsState::new(
            health,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        ));
        let mut app = p.app_state().clone();
        app.screen = Screen::Settings;
        p.app = Some(app);
        p
    }

    /// REGRESSION (routing level, not the pure reducer): the Daemon-section
    /// numeric-config overlay is a text-capture surface. Every realistic value for
    /// an int knob (30, 120, 240, 1440) contains a digit `routing_event` claims as
    /// a tab switch, so without the capture guard typing `3` teleported the user to
    /// the Skill Manager and dropped the keystroke — the headline editing path did
    /// not work at all. Drive the real `on_key` (which consults `routing_event`
    /// BEFORE `route_key`), not `reduce_settings`, or the bug is invisible.
    #[test]
    fn digits_type_into_the_daemon_config_overlay_not_tab_switch() {
        use ainb_hangar_core::daemon_config::{
            DAEMON_CONFIG_REGISTRY, KEY_AUTOSTANDUP_STAGNANT_MIN,
        };
        let mut p = plugin_on_daemon_settings();

        // Move the cursor onto `autostandup.stagnant_min` (an Int knob) and open
        // the numeric overlay with Enter.
        let target = DAEMON_CONFIG_REGISTRY
            .iter()
            .position(|d| d.key == KEY_AUTOSTANDUP_STAGNANT_MIN)
            .expect("stagnant_min is a registry knob");
        for _ in 0..target {
            p.on_key(&key_press(KeyCode::Down));
        }
        p.on_key(&key_press(KeyCode::Enter));
        assert_eq!(
            p.screens.settings.as_ref().and_then(|s| s.config_input_buffer()),
            Some(""),
            "Enter on an int knob opens an empty numeric overlay"
        );

        // THE REGRESSION: `3` must extend the buffer, not switch to the Skill
        // Manager tab (`routing_event` maps '3' → Screen::SkillManager).
        p.on_key(&key_press(KeyCode::Char { ch: '3' }));
        assert!(
            matches!(p.app_state().screen, Screen::Settings),
            "typing `3` in the config overlay must NOT switch tabs, got {:?}",
            p.app_state().screen
        );
        assert_eq!(
            p.screens.settings.as_ref().and_then(|s| s.config_input_buffer()),
            Some("3"),
            "`3` must land in the overlay buffer"
        );

        // `0` completes `30`; the overlay is still open on Settings.
        p.on_key(&key_press(KeyCode::Char { ch: '0' }));
        assert_eq!(
            p.screens.settings.as_ref().and_then(|s| s.config_input_buffer()),
            Some("30"),
            "digits accumulate in the overlay"
        );
        assert!(matches!(p.app_state().screen, Screen::Settings));
    }

    /// Seed a plugin on the Settings screen's Workspaces section with two
    /// workspaces (`default` active + `acme`), so the new-workspace name modal
    /// (P-multica#4) can be driven through the REAL `on_key` routing.
    fn plugin_on_workspaces_settings() -> HangarPlugin {
        use crate::screen::settings::{SettingsSection, SettingsState};
        use ainb_hangar_proto::settings::{HealthSnapshot, WorkspaceRow};
        let mut p = connected_plugin_with_issue();
        let health = HealthSnapshot {
            socket_path: "/tmp/x.sock".into(),
            pid: 1,
            uptime_secs: 0,
            version: "test".into(),
            connected: true,
        };
        let workspaces = vec![
            WorkspaceRow {
                id: "01WSDEFAULT0000000000000000".into(),
                slug: "default".into(),
                name: "Default Workspace".into(),
                current: true,
                default: true,
            },
            WorkspaceRow {
                id: "01WSACME000000000000000000".into(),
                slug: "acme".into(),
                name: "acme".into(),
                current: false,
                default: false,
            },
        ];
        p.screens.settings = Some(SettingsState::new(
            health,
            Vec::new(),
            Vec::new(),
            workspaces,
        ));
        let mut app = p.app_state().clone();
        app.screen = Screen::Settings;
        p.app = Some(app);
        // Navigate Daemon -> Workspaces (j x3) through the real routing.
        for _ in 0..3 {
            p.on_key(&key_press(KeyCode::Char { ch: 'j' }));
        }
        assert_eq!(
            p.screens.settings.as_ref().map(SettingsState::section),
            Some(SettingsSection::Workspaces),
            "j x3 lands on the Workspaces section"
        );
        p
    }

    /// REGRESSION (routing level, not the pure reducer, P-multica#4): the
    /// new-workspace name modal is a text-capture surface. A realistic workspace
    /// name (`Beta`, `QA`, `Data`) begins with an uppercase letter that
    /// `routing_event` claims as a tab switch (`B`→Boards, `q`→quit, …), so
    /// without registering the modal in BOTH capture guards (`on_key` +
    /// `captures_text`) the first such keystroke teleported the user to another
    /// tab and dropped the modal — the headline create path was unusable for any
    /// name starting with a claimed char. The pure `reduce_settings` tests never
    /// see this because they bypass `routing_event`. Drive the real `on_key`.
    #[test]
    fn typing_a_workspace_name_with_a_tab_char_stays_in_the_modal() {
        let mut p = plugin_on_workspaces_settings();

        // `n` opens the name modal with an empty buffer.
        p.on_key(&key_press(KeyCode::Char { ch: 'n' }));
        assert_eq!(
            p.screens.settings.as_ref().and_then(|s| s.workspace_name_input()),
            Some(""),
            "n opens the new-workspace name modal"
        );

        // THE REGRESSION: `B` (routing maps 'B' → Boards) must extend the name
        // buffer, NOT switch tabs. Type "Beta" — every char stays in the modal.
        for ch in ['B', 'e', 't', 'a'] {
            p.on_key(&key_press(KeyCode::Char { ch }));
        }
        assert!(
            matches!(p.app_state().screen, Screen::Settings),
            "typing a workspace name must NOT switch screens, got {:?}",
            p.app_state().screen
        );
        assert_eq!(
            p.screens.settings.as_ref().and_then(|s| s.workspace_name_input()),
            Some("Beta"),
            "the full name (incl. the tab-switch char) lands in the modal"
        );

        // Enter derives the slug and arms the create action carrying the FULL name.
        p.on_key(&key_press(KeyCode::Enter));
        match p.screens.take_pending_ws_action() {
            Some(WorkspaceAction::Create { slug, name }) => {
                assert_eq!(name, "Beta", "the create carries the full typed name");
                assert_eq!(slug, "beta", "slug is derived lower-case from the name");
            }
            other => panic!("expected a pending Create action, got {other:?}"),
        }
    }

    /// The plugin must declare `captures_text` while the new-workspace name modal
    /// is open (P-multica#4) so the HOST suppresses its own global single-char
    /// shortcuts (`H`/`?`/`W`) and forwards them into the name instead of eating
    /// them — the second half of the capture-surface contract (the first is the
    /// `on_key` routing guard proven above). Kept in lock-step with that guard.
    #[test]
    fn name_modal_declares_text_capture() {
        let mut p = plugin_on_workspaces_settings();
        assert!(
            !p.captures_text(),
            "no capture surface is open before the modal"
        );
        p.on_key(&key_press(KeyCode::Char { ch: 'n' }));
        assert!(
            p.captures_text(),
            "an open new-workspace name modal must declare text-capture"
        );
    }

    /// REGRESSION: two config edits landing before a render pass must BOTH be
    /// written. The pending write used to be a single slot, so the second edit
    /// overwrote the first — the first key was silently never persisted while the
    /// pane happily showed it applied (the optimistic edit stays either way).
    /// Keys arrive far faster than render passes, and the registry generalised
    /// this surface from one knob to five, so this is reachable by simply typing.
    #[test]
    fn two_config_edits_before_a_render_both_queue() {
        use ainb_hangar_core::daemon_config::{
            DAEMON_CONFIG_REGISTRY, KEY_AUTOSTANDUP_ENABLED, KEY_CARD_AGENT_DEFAULT,
        };
        let mut p = plugin_on_daemon_settings();

        // Edit 1: `a` toggles auto-standup from anywhere in the section.
        p.on_key(&char_press('a'));
        // Edit 2: cycle the enum knob, with NO render pass in between.
        let enum_idx = DAEMON_CONFIG_REGISTRY
            .iter()
            .position(|d| d.key == KEY_CARD_AGENT_DEFAULT)
            .expect("enum knob present");
        for _ in 0..enum_idx {
            p.on_key(&key_press(KeyCode::Down));
        }
        p.on_key(&key_press(KeyCode::Enter));

        let queued = &p.screens.pending_daemon_config_set;
        assert_eq!(
            queued.len(),
            2,
            "both edits must be queued, got {queued:?} — a dropped write is invisible"
        );
        assert_eq!(
            queued[0],
            (KEY_AUTOSTANDUP_ENABLED.to_string(), "true".to_string())
        );
        assert_eq!(
            queued[1],
            (KEY_CARD_AGENT_DEFAULT.to_string(), "codex".to_string())
        );

        // Draining hands them over in edit order and leaves the queue empty.
        let drained = p.screens.take_pending_daemon_config_sets();
        assert_eq!(drained.len(), 2, "the drain yields every queued write");
        assert!(
            p.screens.take_pending_daemon_config_sets().is_empty(),
            "a drained queue is empty — no write fires twice"
        );
    }

    /// REGRESSION: while the numeric overlay is open the plugin must DECLARE text
    /// capture to the host, which is what stops the host eating a bare `q` (quit)
    /// or `?` as its own global shortcut instead of forwarding it.
    ///
    /// Asserting on screen state alone would be VACUOUS here: `on_key` discards
    /// the routing layer's `Intent::Quit`, so a `q` that leaked to the nav layer
    /// leaves `app.screen` on Settings either way. `captures_text` is the seam the
    /// host actually reads, so that is what this pins.
    #[test]
    fn the_daemon_config_overlay_declares_text_capture_to_the_host() {
        use ainb_plugin_sdk::Plugin;
        let mut p = plugin_on_daemon_settings();
        assert!(
            !p.captures_text(),
            "no capture surface open on the bare Daemon section"
        );

        p.on_key(&key_press(KeyCode::Down));
        p.on_key(&key_press(KeyCode::Enter));
        assert!(
            p.screens.settings.as_ref().and_then(|s| s.config_input_buffer()).is_some(),
            "the numeric overlay is open"
        );
        assert!(
            p.captures_text(),
            "an open config overlay must declare capture, else the host eats `q`/`?`"
        );

        // Esc closes it and capture is released.
        p.on_key(&key_press(KeyCode::Esc));
        assert!(
            p.screens.settings.as_ref().and_then(|s| s.config_input_buffer()).is_none(),
            "Esc cancels the overlay in a single press"
        );
        assert!(!p.captures_text(), "capture is released with the overlay");
    }

    #[test]
    fn fleet_hotkey_routes_to_dedicated_pane() {
        let app =
            AppState::new(WorkspaceId::from_str("default").expect("valid default workspace id"));
        let event = routing_event(&char_press('F'), &app).expect("Fleet route event");
        let out = crate::screen::reduce(&app, event);
        assert_eq!(out.state.screen, Screen::Fleet);
    }

    #[test]
    fn fleet_live_event_advances_cursor_and_arms_focused_reconcile() {
        let mut plugin = HangarPlugin::new();
        let event = ainb_hangar_proto::fleet::FleetEvent {
            revision: 12,
            event_id: "evt-12".into(),
            session_key: "codex:thread-1".into(),
            observed_at: 100,
            provenance: ainb_hangar_proto::fleet::FleetProvenance::Authoritative,
            event_type: "turn_started".into(),
            payload: serde_json::json!({}),
            session_version: 3,
            applied: true,
        };
        plugin.on_daemon_event(&serde_json::json!({
            "method": "fleet/event",
            "params": event,
        }));
        assert_eq!(plugin.screens.fleet.head_revision(), 12);
        assert!(plugin.fleet_fetch_pending);
    }

    #[test]
    fn fleet_subscribe_ack_seeds_complete_snapshot_and_replay_cursor() {
        let mut plugin = HangarPlugin::new();
        let session = serde_json::json!({
            "session_key": "codex:thread-1",
            "provider": "codex",
            "provider_session_id": "thread-1",
            "tmux_target": "codex-1:0.0",
            "process_start_fingerprint": null,
            "cwd": "/work/shared",
            "display_name": "codex-1",
            "lifecycle": "IDLE",
            "attention": "ASK",
            "current_request_fingerprint": "fingerprint",
            "current_request": {
                "questions": [{
                    "id": "q1",
                    "header": "Tool",
                    "question": "Pick tools",
                    "options": [
                        {"label": "rg", "description": "Text"},
                        {"label": "ast-grep", "description": "Syntax"}
                    ],
                    "multiSelect": true
                }]
            },
            "management": "MANAGED",
            "transport_health": "HEALTHY",
            "capabilities": {"structured_answer": true, "tmux_attach": true},
            "provenance": "authoritative",
            "confidence": "HIGH",
            "discovered_at": 1,
            "last_observed_at": 2,
            "lifecycle_updated_at": 2,
            "attention_updated_at": 2,
            "version": 4,
            "updated_revision": 7
        });
        let response = RpcResponse {
            jsonrpc: "2.0".into(),
            id: RpcId::Number(FLEET_SUBSCRIBE_REQ_ID),
            result: Some(serde_json::json!({
                "snapshot": {"head_revision": 7, "sessions": [session]},
                "replay": [{
                    "revision": 8,
                    "event_id": "evt-8",
                    "session_key": "codex:thread-1",
                    "observed_at": 3,
                    "provenance": "authoritative",
                    "event_type": "AskUserQuestion",
                    "payload": {},
                    "session_version": 4,
                    "applied": false
                }],
                "replay_state": {"state": "complete"}
            })),
            error: None,
        };
        plugin.on_daemon_response(&response);
        assert_eq!(plugin.screens.fleet.head_revision(), 8);
        let selected = plugin.screens.fleet.selected_session().expect("Fleet row");
        assert_eq!(selected.session_key, "codex:thread-1");
        let questions = selected
            .current_request
            .as_ref()
            .and_then(|request| request.get("questions"))
            .and_then(serde_json::Value::as_array)
            .expect("complete questions");
        assert_eq!(questions[0]["options"].as_array().unwrap().len(), 2);
        assert_eq!(questions[0]["multiSelect"], true);
    }

    #[test]
    fn fleet_subscribe_decoder_accepts_every_tagged_replay_state() {
        let states = [
            serde_json::json!({"state": "complete"}),
            serde_json::json!({"state": "snapshot_reset", "reason": "bootstrap"}),
            serde_json::json!({"state": "snapshot_reset", "reason": "cursor_ahead"}),
            serde_json::json!({"state": "snapshot_reset", "reason": "replay_limit_exceeded"}),
        ];
        for replay_state in states {
            let mut plugin = HangarPlugin::new();
            plugin.on_daemon_response(&RpcResponse {
                jsonrpc: "2.0".into(),
                id: RpcId::Number(FLEET_SUBSCRIBE_REQ_ID),
                result: Some(serde_json::json!({
                    "snapshot": {"head_revision": 7, "sessions": []},
                    "replay": [],
                    "replay_state": replay_state,
                })),
                error: None,
            });
            assert_eq!(plugin.screens.fleet.head_revision(), 7);
        }
    }

    #[test]
    fn fleet_structured_action_preserves_exact_request_identity() {
        use ainb_hangar_proto::fleet::{ControlAction, FleetQuestionAnswer, FleetRequestIdentity};
        let plugin = HangarPlugin::new();
        let identity = FleetRequestIdentity {
            request_id: serde_json::json!(42),
            thread_id: "thread-1".into(),
            turn_id: "turn-2".into(),
            item_id: "item-3".into(),
        };
        let answers = vec![FleetQuestionAnswer {
            question_id: "question-1".into(),
            selected_options: vec!["yes".into()],
            text: None,
        }];
        let action = plugin
            .fleet_control_action(
                "codex:thread-1",
                crate::screen::fleet::FleetAction::StructuredAnswer {
                    request_fingerprint: "fingerprint".into(),
                    request_identity: Some(identity.clone()),
                    answers: answers.clone(),
                },
            )
            .expect("structured action maps");
        assert_eq!(
            action,
            ControlAction::StructuredAnswer {
                request_fingerprint: "fingerprint".into(),
                request_identity: Some(identity),
                answers,
            }
        );
    }

    #[test]
    fn fleet_verified_picker_preserves_exact_request_identity_and_key() {
        use ainb_hangar_proto::fleet::ControlAction;

        let plugin = HangarPlugin::new();
        let action = plugin
            .fleet_control_action(
                "legacy:tmux",
                crate::screen::fleet::FleetAction::VerifiedPicker {
                    request_fingerprint: "request-fingerprint".into(),
                    key: "1".into(),
                },
            )
            .expect("picker maps to typed daemon action");
        assert_eq!(
            action,
            ControlAction::VerifiedPicker {
                request_fingerprint: "request-fingerprint".into(),
                key: "1".into(),
            }
        );
    }

    #[test]
    fn fleet_request_ids_do_not_collide_across_plugin_boots() {
        let first = fleet_request_id("action");
        let second = fleet_request_id("action");
        assert_ne!(first, second);
        assert!(first.starts_with("fleet-ui-action-"));
    }

    #[test]
    fn fleet_broadcast_rpc_error_restores_confirmation_for_retry() {
        use crate::screen::fleet::{
            FleetEvent, FleetFilter, FleetIntent, FleetKey, FleetSessionRow, reduce_fleet,
        };

        let mut plugin = HangarPlugin::new();
        let row: FleetSessionRow = serde_json::from_value(serde_json::json!({
            "session_key": "codex:thread-1",
            "provider": "codex",
            "lifecycle": "IDLE",
            "attention": "NONE",
            "management": "MANAGED",
            "provenance": "authoritative",
            "confidence": "HIGH",
            "transport_health": "HEALTHY",
            "version": 1
        }))
        .expect("Fleet row");
        plugin.screens.fleet =
            reduce_fleet(&plugin.screens.fleet, FleetEvent::Snapshot(vec![row])).state;
        plugin.screens.fleet = reduce_fleet(
            &plugin.screens.fleet,
            FleetEvent::SetFilter(FleetFilter::All),
        )
        .state;
        for event in [
            FleetEvent::Key(FleetKey::Char('b')),
            FleetEvent::Key(FleetKey::Char('x')),
            FleetEvent::Key(FleetKey::Enter),
            FleetEvent::Key(FleetKey::Space),
            FleetEvent::Key(FleetKey::Enter),
        ] {
            plugin.screens.fleet = reduce_fleet(&plugin.screens.fleet, event).state;
        }
        let sent = reduce_fleet(&plugin.screens.fleet, FleetEvent::Key(FleetKey::Enter));
        assert!(matches!(sent.intent, Some(FleetIntent::Broadcast { .. })));
        plugin.screens.fleet = sent.state;

        plugin.apply_fleet_broadcast_result(&RpcResponse {
            jsonrpc: "2.0".into(),
            id: RpcId::Number(FLEET_BROADCAST_REQ_ID),
            result: None,
            error: Some(ainb_hangar_proto::RpcError {
                code: -32000,
                message: "transport closed".into(),
                data: None,
            }),
        });

        assert_eq!(
            plugin.screens.fleet.feedback(),
            Some("broadcast failed: transport closed")
        );
        let retried = reduce_fleet(&plugin.screens.fleet, FleetEvent::Key(FleetKey::Enter));
        assert!(matches!(
            retried.intent,
            Some(FleetIntent::Broadcast { .. })
        ));
    }

    #[test]
    fn fleet_start_mapping_preserves_exact_global_params() {
        use ainb_hangar_proto::fleet::FleetProvider;

        let params = fleet_start_params(
            FleetProvider::Codex,
            "/work/new".into(),
            Some("inspect failures".into()),
        );
        assert!(params.request_id.starts_with("fleet-ui-start-"));
        assert_eq!(params.provider, FleetProvider::Codex);
        assert_eq!(params.cwd, "/work/new");
        assert_eq!(params.prompt.as_deref(), Some("inspect failures"));
    }

    #[test]
    fn fleet_attach_selects_exact_window_and_pane_before_attach() {
        let command = fleet_tmux_attach_command("fleet-alpha:3.7");
        assert!(command.contains("tmux select-window -t 'fleet-alpha:3.7'"));
        assert!(command.contains("tmux select-pane -t 'fleet-alpha:3.7'"));
        assert!(command.ends_with("tmux attach-session -t 'fleet-alpha'"));
    }
}
