// ABOUTME: The 19 sections AppState is grouped into. Each one sits behind a
// `Versioned<T>` on AppState, so any `&mut` access bumps that section alone.
//
// The grouping is the field audit from the plan (2026-09-05-desktop-p0-surface-safety.md
// at Phase 2), refreshed against the struct as it stands: three fields the plan
// named have since moved to `UiState` or gone, and seventeen that did not exist
// when it was written are placed here for the first time.

use crate::app::SessionLoader;
use crate::app::state::*;
use crate::app::versioned::Versioned;
use crate::audit::{self, AuditResult, AuditTrigger};
use crate::claude::client::ClaudeChatManager;
use crate::claude::types::ClaudeStreamingEvent;
use crate::claude::{ClaudeApiClient, ClaudeMessage};
use crate::components::home_screen_v2::HomeScreenV2State;
use crate::components::live_logs_stream::LogEntry;
use crate::config::screen_model::{self, ConfigTreeNode};
use crate::config::{AppConfig, SessionLabelStore, registry};
use crate::credentials;
use crate::docker::LogStreamingCoordinator;
use crate::fleet::attention::{Answerable, AttentionKind, SessionAttention};
use crate::models::{Session, SessionAgentType, Workspace, is_default_model};
use chrono;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tracing::{debug, error, info, trace, warn};
use uuid::Uuid;

#[derive(Debug)]
pub struct McpPoolSection {
    pub mcp_overlay: Option<McpOverlayState>,
}

impl Default for McpPoolSection {
    fn default() -> Self {
        Self { mcp_overlay: None }
    }
}

#[derive(Debug)]
pub struct RecoverySection {
    pub session_recovery_state: crate::components::SessionRecoveryState,
}

impl Default for RecoverySection {
    fn default() -> Self {
        Self {
            session_recovery_state: crate::components::SessionRecoveryState::default(),
        }
    }
}

#[derive(Debug)]
pub struct GitViewSection {
    pub git_view_state: Option<crate::components::GitViewState>,
    // Previous view for navigation (e.g., to return from GitView)
    pub quick_commit_message: Option<String>, // None = not in quick commit mode, Some = message being entered
    pub quick_commit_cursor: usize,           // Cursor position in quick commit message
    pub is_current_dir_git_repo: bool,
    // Track which session logs were last fetched to avoid unnecessary refetches
}

impl Default for GitViewSection {
    fn default() -> Self {
        Self {
            git_view_state: None,
            quick_commit_message: None,
            quick_commit_cursor: 0,

            // Initialize tmux integration
            is_current_dir_git_repo: false,
        }
    }
}

#[derive(Debug)]
pub struct ClaudeChatSection {
    pub claude_chat_visible: bool,

    // Focus management for panes
    pub claude_chat_state: Option<ClaudeChatState>,
    // Live logs from Docker containers
    pub claude_manager: Option<ClaudeChatManager>,
    // Docker log streaming coordinator
}

impl Default for ClaudeChatSection {
    fn default() -> Self {
        Self {
            claude_chat_visible: false,
            claude_chat_state: None,
            claude_manager: None,
        }
    }
}

#[derive(Debug)]
pub struct HangarSection {
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
    /// Daemons screen state (cached runtime-health snapshot + poll tick).
    pub daemons_state: crate::components::daemons::DaemonsState,
}

impl Default for HangarSection {
    fn default() -> Self {
        Self {
            pending_daemon_config_edits: Vec::new(),
            hangar_daemon_config_loaded: false,
            daemons_state: crate::components::daemons::DaemonsState::default(),
        }
    }
}

#[derive(Debug)]
pub struct PluginsHostSection {
    /// WireBuffers freshly drained from plugins, keyed by screen id.
    /// `App::tick_plugin_renders` populates this before each frame so
    /// `PluginScreen::render` can paint without needing access to the
    /// plugin runtime (which lives on `App`, not `AppState`).
    pub pending_plugin_renders:
        std::collections::HashMap<crate::app::screens::ScreenId, ainb_plugin_runtime::WireBuffer>,
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
}

impl Default for PluginsHostSection {
    fn default() -> Self {
        Self {
            pending_plugin_renders: std::collections::HashMap::new(),
            plugin_captures_text: std::collections::HashMap::new(),
            plugin_render_errors: std::collections::HashMap::new(),
            plugin_runtime: None,
        }
    }
}

#[derive(Debug)]
pub struct SkillsSection {
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
}

impl Default for SkillsSection {
    fn default() -> Self {
        Self {
            skills_state: crate::components::skills::SkillsViewState::default(),
            skills_load_receiver: None,
            skill_manager_state: crate::components::skill_manager_screen::SkillsScreenData::default(
            ),
            drift_load_receiver: None,
        }
    }
}

#[derive(Debug)]
pub struct OnboardingSection {
    // Onboarding wizard state
    pub onboarding_state: Option<crate::components::onboarding::OnboardingState>,
    // Setup menu state
    pub setup_menu_state: crate::components::setup_menu::SetupMenuState,
    // Auth setup state
    pub auth_setup_state: Option<AuthSetupState>,
    pub auth_provider_popup_state: AuthProviderPopupState,
}

impl Default for OnboardingSection {
    fn default() -> Self {
        Self {
            onboarding_state: None,
            setup_menu_state: crate::components::setup_menu::SetupMenuState::new(),
            auth_setup_state: None,
            // AppState::default overwrites this from the config it loads. The
            // neutral baseline is here so the section still stands alone, which
            // the section tests need and a partial `..Default::default()` uses.
            auth_provider_popup_state: AuthProviderPopupState::from_app_config(
                &crate::config::AppConfig::default(),
            ),
        }
    }
}

#[derive(Debug)]
pub struct SshSection {
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
}

impl Default for SshSection {
    fn default() -> Self {
        Self {
            ssh_sessions: Vec::new(),
            ssh_sessions_expanded: true, // Default to expanded
            selected_ssh_session_index: None,
            ssh_session_rename_mode: false,
            ssh_session_rename_buffer: String::new(),
        }
    }
}

#[derive(Debug)]
pub struct SessionLabelsSection {
    /// Persistent store for durable session labels.
    pub session_label_store: SessionLabelStore,
    /// Durable-label text popup state for managed and SSH sessions.
    pub session_label_rename_mode: bool,
    pub session_label_rename_buffer: String,
    pub session_label_rename_target: Option<AttachableRef>,
    pub session_context_menu: Option<SessionContextMenu>,
}

impl Default for SessionLabelsSection {
    fn default() -> Self {
        Self {
            session_label_store: SessionLabelStore::load(),
            session_label_rename_mode: false,
            session_label_rename_buffer: String::new(),
            session_label_rename_target: None,
            session_context_menu: None,
        }
    }
}

#[derive(Debug)]
pub struct ConfigSection {
    // Persistent configuration (saved to ~/.agents-in-a-box/config/config.toml)
    pub app_config: AppConfig,
    pub config_screen_state: ConfigScreenState,
    /// Config popup state for choice/text input popups in config screen
    pub config_popup_state: crate::components::config_popup::ConfigPopupState,
    // Changelog viewer state
    pub changelog_state: crate::components::ChangelogState,
}

impl Default for ConfigSection {
    fn default() -> Self {
        // AppState::default replaces both of these with the config it loads.
        // The neutral baseline keeps the section standing alone.
        let app_config = AppConfig::default();
        Self {
            config_screen_state: ConfigScreenState::from_app_config(&app_config),
            app_config,
            config_popup_state: crate::components::config_popup::ConfigPopupState::default(),
            changelog_state: crate::components::ChangelogState::new(),
        }
    }
}

#[derive(Debug)]
pub struct WorkspaceLoadSection {
    // Background workspace loading state
    pub is_loading_workspaces: bool,
    pub workspace_load_error: Option<String>,
    pub workspace_load_started: Option<Instant>,
    /// Channel receiver for background workspace loading results
    pub workspace_load_receiver: Option<mpsc::UnboundedReceiver<WorkspaceLoadResult>>,
    // Periodic session snapshot tracking
    pub last_snapshot_time: Option<Instant>,
    // Throttled tmux preview updates (avoid spawning subprocesses every 250ms tick)
    pub last_preview_update: Option<Instant>,
    // Throttle for the cheaper non-selected-session status sweep. Status
    // (running/idle) is not time-critical, so it polls on a longer cadence than
    // the selected session's live preview: one `capture-pane` subprocess per
    // non-selected session is only spawned every `STATUS_INTERVAL_SECS`, not on
    // every 5s preview refresh. (perf: bead 9pb)
    pub last_status_check: Option<Instant>,
}

impl Default for WorkspaceLoadSection {
    fn default() -> Self {
        Self {
            is_loading_workspaces: false,
            workspace_load_error: None,
            workspace_load_started: None,
            workspace_load_receiver: None,
            last_snapshot_time: None,
            last_preview_update: None,
            last_status_check: None,
        }
    }
}

#[derive(Debug)]
pub struct NewSessionSection {
    // New session creation state
    pub new_session_state: Option<NewSessionState>,
    // Usage analytics state: removed. Burndown plugin owns usage state
    // (provider, period, filters, zoom). Host no longer reads or writes
    // `usage_state` / `usage_load_receiver`. Statusline-related state
    // (live_window_watcher, statusline_status_cache) stays in core
    // because that's a host CLI install concern, not a plugin one.
    /// Background base-branch refresh for the Configure picker. The fetch +
    /// re-list runs on `spawn_blocking`; the result lands here and is applied
    /// by `check_branch_refresh_complete` on the next tick. The `u64` is a
    /// generation guard, so results from a closed or reopened picker are dropped.
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
}

impl Default for NewSessionSection {
    fn default() -> Self {
        Self {
            new_session_state: None,
            branch_refresh_receiver: None,
            branch_refresh_seq: 0,
            repo_check_receiver: None,
            repo_check_seq: 0,
            repo_init_receiver: None,
            repo_init_seq: 0,
        }
    }
}

#[derive(Debug)]
pub struct SessionsSection {
    pub workspaces: Vec<Workspace>,
    pub selected_workspace_index: Option<usize>,
    pub selected_session_index: Option<usize>,
    pub shell_selected: bool, // Whether the workspace shell is currently selected
    pub selected_sessions: HashSet<Uuid>, // Multi-selected session IDs for bulk operations
    pub expand_all_workspaces: bool, // When true, show all sessions across all workspaces
    pub session_filter: SessionFilter, // View filter for Interactive sessions (Shift+F to cycle)
    // Track attached terminal state
    pub attached_session_id: Option<Uuid>,
    /// Cache of workspace paths that are currently favorited (starred).
    /// Computed by `recompute_favorite_workspaces()` whenever the workspace
    /// list or the favorites store changes, NOT in the render path. The
    /// session-list render reads this set with an O(1) lookup, so it never
    /// re-parses `favorites.yaml` or opens a git repo per frame.
    pub favorite_workspace_paths: HashSet<PathBuf>,
}

impl Default for SessionsSection {
    fn default() -> Self {
        Self {
            workspaces: Vec::new(),
            selected_workspace_index: None,
            selected_session_index: None,
            shell_selected: false,
            selected_sessions: HashSet::new(),
            expand_all_workspaces: true, // Default to expanded view
            // AppState::default overwrites this from the loaded config.
            session_filter: crate::app::state::SessionFilter::default(),
            attached_session_id: None,
            favorite_workspace_paths: HashSet::new(),
        }
    }
}

#[derive(Debug)]
pub struct LogsSection {
    pub logs: HashMap<Uuid, Vec<String>>,
    // Claude chat integration
    pub live_logs: HashMap<Uuid, Vec<LogEntry>>,
    // Track when logs were last updated for each session
    pub log_last_updated: HashMap<Uuid, std::time::Instant>,
    // Track the last time we checked for log updates globally
    pub last_log_check: Option<std::time::Instant>,
    // Track if current directory is a git repository
    pub last_logs_session_id: Option<Uuid>,
    // Claude API client manager (when initialized)
    pub log_streaming_coordinator: Option<LogStreamingCoordinator>,
    // Channel sender for log streaming
    pub log_sender: Option<mpsc::UnboundedSender<(Uuid, LogEntry)>>,
    // Log history viewer state
    pub log_history_state: crate::components::LogHistoryViewerState,
    /// The `log` tab's history, filled by [`crate::fleet::session_log`] on its
    /// own thread.
    ///
    /// Read on the render path, never QUERIED there: the store read used to
    /// live inside `terminal.draw` and cost a real store up to 948 ms a frame.
    pub session_log: Arc<crate::fleet::session_log::Shared>,
    /// Whether the session-log worker is alive. Same idempotence flag, and the
    /// same reason, as [`Self::attention_poll_running`].
    pub session_log_running: Arc<std::sync::atomic::AtomicBool>,
}

impl Default for LogsSection {
    fn default() -> Self {
        Self {
            logs: HashMap::new(),
            live_logs: HashMap::new(),
            log_last_updated: HashMap::new(),
            last_log_check: None,
            last_logs_session_id: None,
            log_streaming_coordinator: None,
            log_sender: None,
            log_history_state: crate::components::LogHistoryViewerState::new(),
            session_log: Arc::new(crate::fleet::session_log::Shared::default()),
            session_log_running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }
}

#[derive(Debug)]
pub struct TmuxSection {
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
    // A changed selection must settle before starting a read-only client.
    pub(crate) observer_pending: Option<(String, Instant)>,
    // A read-only observer that dies waits before the next retry.
    pub(crate) observer_failed_target: Option<(String, Instant, u8)>,
    // A spawned observer must survive briefly before it clears a prior retry
    // count. `tmux attach-session` reports some startup failures asynchronously.
    pub(crate) observer_started_at: Option<Instant>,
}

impl Default for TmuxSection {
    fn default() -> Self {
        Self {
            embed: None,
            embed_session: None,
            tmux_sessions: HashMap::new(),
            preview_update_task: None,
            other_tmux_sessions: Vec::new(),
            other_tmux_expanded: true, // Default to expanded
            selected_other_tmux_index: None,
            selected_other_tmux_sessions: HashSet::new(),
            other_tmux_rename_mode: false,
            other_tmux_rename_buffer: String::new(),
            observer_pending: None,
            observer_failed_target: None,
            observer_started_at: None,
        }
    }
}

#[derive(Debug)]
pub struct FleetSection {
    /// Per-session "cleared up to" timestamp (epoch ms). A hook event
    /// only marks a session if its `ts` is newer than this. Defaults to
    /// `0` (any event in the lookback window can mark); bumped to "now"
    /// while the user is attached, so re-marking only happens for
    /// activity that arrives after they look away.
    pub attention_baseline: HashMap<Uuid, i64>,
    /// Background poller for the live OAuth-window snapshot. The render
    /// path reads via `snapshot()` (cheap RwLock read + clone) instead of
    /// calling `live_window::current()` directly, because Tier 2's JSONL walk
    /// would otherwise stall input handling on every frame.
    pub live_window_watcher: crate::models::live_window_watcher::LiveWindowWatcher,
    // Track the last Headroom proxy watchdog tick (re-ensure if a Headroom
    // session is live but the proxy died).
    pub last_headroom_watchdog: Option<std::time::Instant>,
    // Track the last time we checked for OAuth token refresh
    pub last_token_refresh_check: Option<std::time::Instant>,
    /// The `ask` pane's own state: which option is selected, what has been
    /// typed, and what the last send did.
    pub ask_state: crate::fleet::answer::AskState,
    /// The Pal conversation, opened lazily the first time the tab is.
    ///
    /// Lazy because opening it dials the daemon to resolve the minted channel
    /// scope, and an operator who never opens the tab should never pay for it.
    pub pal_chat: Option<crate::fleet::chat_host::ChatHost>,
    /// The Pal pane's engine / model / guardrail header.
    ///
    /// NOT lazy like the conversation: the header is how an operator recovers
    /// from an adapter that will not spawn, so it reads the registry the first
    /// time the tab is rendered rather than waiting for a chat that may never
    /// open. It costs one `fleet/adapter_list` per session.
    pub pal_dial: crate::fleet::pal_dial::PalDial,
    /// The Pal pane's offer to start the hangar daemon it needs.
    ///
    /// One per process, not one per pane: the offer starts the daemon the whole
    /// TUI talks to, and a second copy would let two panes each shell a start
    /// into the same home.
    pub daemon_start_cta: crate::fleet::daemon_cta::DaemonStartCta,
    /// The broadcast composer, shown on `thread` while rows are checked.
    ///
    /// Survives a change of checkbox set on purpose: an operator who ticks a
    /// fifth session halfway through typing must not lose what they typed.
    pub broadcast: crate::fleet::broadcast::Broadcast,
    /// The selected session's own thread, rebuilt when the selection moves to a
    /// different session.
    ///
    /// One host, not one per session: a thread the operator has navigated away
    /// from is not being read, and keeping N of them alive means N poll loops
    /// against the daemon for conversations nobody is looking at.
    pub session_chat: Option<(String, crate::fleet::chat_host::ChatHost)>,
    /// The daemon's half of the attention picture, refreshed by
    /// [`crate::fleet::attention_poll`] on its own thread.
    ///
    /// Read on the render path, never dialled there: a wedged daemon socket
    /// must cost a frame nothing.
    pub daemon_attention: crate::fleet::attention_poll::Shared,
    /// Last Hangar Fleet snapshot, refreshed beside daemon attention off the
    /// render path.
    pub fleet_snapshot: crate::fleet::attention_poll::SnapshotShared,
    /// Snapshot metadata matched to local session identities. This avoids
    /// assigning a child sharing a cwd to its parent by accident.
    pub fleet_metadata: HashMap<Uuid, SessionFleetMetadata>,
    /// Whether the attention poller thread is alive, so the render loop can
    /// start one without having to remember whether it already did.
    pub attention_poll_running: Arc<std::sync::atomic::AtomicBool>,

    /// The poller's publish counter, and the last value this section folded in.
    ///
    /// `daemon_attention` and `fleet_snapshot` are shared handles: the worker
    /// writes through them without anything here taking `&mut`, so the section
    /// version would never move for daemon-side news. The counter is read by
    /// `&` like the cells, and `daemon_attention_seen` is the versioned copy
    /// that `refresh_daemon_attention_generation` folds it into once a frame.
    pub daemon_attention_generation: crate::fleet::attention_poll::Generation,
    pub daemon_attention_seen: u64,
    /// Daemon attention rows whose cwd matched no row on this screen, counted
    /// for the header so the ONE attention surface never silently swallows a
    /// request it could not place.
    pub attention_elsewhere: usize,
    /// Per-session instant (epoch ms) the ERR chip's failure was FIRST
    /// observed. `SessionStatus::Error` carries no timestamp of its own, so
    /// without this the chip's age would reset to `0s` on every refresh and an
    /// hour-old failure would read as brand new. Cleared the moment the session
    /// recovers or leaves the tree, so a later failure starts its own clock.
    pub attention_error_since: HashMap<Uuid, i64>,
    /// When each LOCAL blocking chip was first observed, keyed by session and
    /// chip kind.
    ///
    /// `attention_for_session` returns the newest QUALIFYING hook row, so a
    /// producer that re-reports an unanswered question (which Claude Code
    /// does, it re-emits `Notification` while a prompt stays open) hands back
    /// a newer `ts` every time. Two things broke on that moving value: the
    /// chip's age reset to `0s` on every repeat, defeating the oldest-wins rule
    /// `attention::normalise` documents; and `request_id` is derived from
    /// `since_ms`, so a landed answer outcome was filed under a key that then
    /// changed underneath it and the `✗ not answered` line vanished from a
    /// question that had genuinely failed.
    ///
    /// Same shape as [`Self::attention_error_since`]: stamped once, reused
    /// while the chip stays that kind, dropped when it does not.
    pub attention_local_since: HashMap<(Uuid, AttentionKind, Option<String>), i64>,
}

impl Default for FleetSection {
    fn default() -> Self {
        Self {
            attention_baseline: HashMap::new(),
            live_window_watcher: crate::models::live_window_watcher::LiveWindowWatcher::default(),
            last_headroom_watchdog: None,
            last_token_refresh_check: None,
            ask_state: crate::fleet::answer::AskState::default(),
            pal_chat: None,
            pal_dial: crate::fleet::pal_dial::PalDial::new(),
            daemon_start_cta: crate::fleet::daemon_cta::DaemonStartCta::default(),
            broadcast: crate::fleet::broadcast::Broadcast::default(),
            session_chat: None,
            daemon_attention: Arc::new(Mutex::new(
                crate::fleet::attention::DaemonAttention::default(),
            )),
            fleet_snapshot: Arc::new(Mutex::new(Vec::new())),
            fleet_metadata: HashMap::new(),
            attention_poll_running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            daemon_attention_generation: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            daemon_attention_seen: 0,
            attention_elsewhere: 0,
            attention_error_since: HashMap::new(),
            attention_local_since: HashMap::new(),
        }
    }
}

#[derive(Debug)]
pub struct ShellSection {
    pub current_screen: ScreenId,
    // Git view state
    pub previous_screen: Option<ScreenId>,
    pub should_quit: bool,
    pub help_visible: bool,
    // Flag to force UI refresh after workspace changes
    pub ui_needs_refresh: bool,
    // AINB 2.0: Home screen and agent selection
    pub home_screen_state: HomeScreenState,
    pub home_screen_v2_state: HomeScreenV2State,
    // Notification system
    pub notifications: Vec<Notification>,
    // Confirmation dialog state
    pub confirmation_dialog: Option<ConfirmationDialog>,
    // Pending event to be processed in next loop iteration
    pub pending_event: Option<crate::app::events::AppEvent>,
    // Async action processing
    pub pending_async_action: Option<AsyncAction>,
    // Flag to track if user cancelled during async operation
    pub async_operation_cancelled: bool,
    /// Last `ui.close_request` snapshot version consumed by
    /// `tick_panel_close_requests`. The poll acts at most once per
    /// plugin publish: a version is consumed (recorded here) on first
    /// sight whether or not it triggered a navigation, so a close
    /// request that arrives while the user is on a different screen is
    /// absorbed instead of firing later.
    pub last_panel_close_version: Option<u64>,
    /// The active right-pane tab. Reconciled every frame against what is
    /// actually available, so a tab cannot stay open on a pane that has gone
    /// dead under the operator.
    pub session_tab: crate::components::session_tabs::SessionTab,
    /// Sessions already told, on their CURRENT launch, that they started
    /// without shared Codex remote control.
    ///
    /// The dedup key for `notify_codex_degraded`, cleared by
    /// `begin_codex_launch` so the scope is one launch and not the session's
    /// whole life. Kept here rather than checked against the live notification
    /// list because notifications EXPIRE: a message-equality check would let
    /// the same fact reappear minutes later.
    pub(crate) codex_degrade_announced: std::collections::HashSet<Uuid>,
    // Claude chat visibility toggle
    pub focused_pane: FocusedPane,
}

impl Default for ShellSection {
    fn default() -> Self {
        Self {
            current_screen: screen_ids::HOME.to_string(),
            previous_screen: None,
            should_quit: false,
            help_visible: false,
            ui_needs_refresh: false,
            home_screen_state: HomeScreenState::default(),
            // AppState::default builds this one and restores its sidebar
            // width from the loaded config before handing it over.
            home_screen_v2_state: HomeScreenV2State::new(),
            notifications: Vec::new(),
            confirmation_dialog: None,
            pending_event: None,
            pending_async_action: None,
            async_operation_cancelled: false,
            last_panel_close_version: None,
            session_tab: crate::components::session_tabs::SessionTab::default(),
            codex_degrade_announced: std::collections::HashSet::new(),
            focused_pane: FocusedPane::Sessions,
        }
    }
}

/// The ainb-hooks inbox.
///
/// Empty on purpose. The inbox screen's state was removed from `AppState`
/// before this refactor, but the section is one of the nineteen the plan
/// fixes as the boundary set, and `SectionId::Inbox` is what a surface
/// subscribes to. Deleting it would renumber every section after it for a
/// screen that is coming back, so it keeps its place and gains fields when
/// the screen does.
#[derive(Debug, Default)]
pub struct InboxSection {}
