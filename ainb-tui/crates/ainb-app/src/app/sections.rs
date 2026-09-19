// ABOUTME: The 20 sections AppState is grouped into. Each one sits behind a
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
    /// What the host's plugin runtime knows about the plugin behind each
    /// plugin-owned screen, keyed by screen id like its neighbours, as
    /// `App::tick_plugin_renders` last read it.
    /// Empty until the runtime is up. The reducer decides from this whether a
    /// key goes to the plugin or back to the host, and a renderer whether the
    /// screen is loading or its plugin is absent; neither asks the runtime.
    pub plugin_presence: std::collections::BTreeMap<crate::app::screens::ScreenId, PluginPresence>,
    /// Each plugin's last `ui.state` view, keyed by plugin id, for a renderer
    /// that draws the plugin's screen itself. Refreshed by
    /// `tick_plugin_renders`; the host stores the JSON and never reads into
    /// it.
    pub plugin_ui_states: std::collections::HashMap<String, PluginUiState>,
    /// The newest `ui.state` version per plugin that must not be shown again:
    /// the last one seen before the plugin stopped, or one that was refused.
    /// A restarted plugin's stale view, or one bad publish read again every
    /// tick, stops here.
    pub plugin_ui_state_spent: std::collections::HashMap<String, u64>,
    /// Plugin screens a host other than the terminal wants kept live, so
    /// their plugins keep rendering and publishing `ui.state` while the
    /// terminal shows something else, each with when its watch was last
    /// renewed. A watch lapses unless renewed within
    /// `AppState::PLUGIN_SCREEN_WATCH_LEASE`, and goes when its plugin does.
    pub watched_plugin_screens: std::collections::BTreeMap<String, ScreenWatch>,
}

/// One plugin as the host's runtime knows it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
pub struct PluginPresence {
    /// The runtime has the plugin registered, running or not.
    pub registered: bool,
    /// Its render has blown its budget, so input sent to it sits unserviced.
    pub wedged: bool,
    /// The ABI revision its manifest declares, 0 while it is not registered.
    /// The router sends a key only to a plugin at or past that key's
    /// `min_abi` (#1171).
    pub abi: u32,
}

/// The requests keeping one plugin screen rendering for hosts that are not
/// showing it here, one per watching host.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScreenWatch {
    /// Each watching host's request still inside its lease: when it last
    /// arrived, and the width and height that host draws the screen at. Keyed
    /// by host, so a stop or a new size from one host replaces only its own.
    pub requests:
        std::collections::BTreeMap<crate::wire::frame::HostId, (std::time::Instant, u16, u16)>,
}

impl ScreenWatch {
    /// Hosts kept per screen; past this the least recently renewed is dropped.
    const MAX_REQUESTS: usize = 16;

    /// The largest viewport a watch may ask a plugin to render. A larger
    /// request is clamped to it, so a host cannot make a plugin allocate an
    /// arbitrarily large frame.
    pub const MAX_VIEWPORT: (u16, u16) = (1024, 512);

    /// Record `host`'s request for `width` by `height` at `now`, clamped to
    /// [`Self::MAX_VIEWPORT`]. It replaces that host's earlier request, size and
    /// lease both. Requests older than `lease` go.
    pub fn renew(
        &mut self,
        host: crate::wire::frame::HostId,
        now: std::time::Instant,
        width: u16,
        height: u16,
        lease: std::time::Duration,
    ) {
        let (width, height) = (
            width.min(Self::MAX_VIEWPORT.0),
            height.min(Self::MAX_VIEWPORT.1),
        );
        self.lapse(now, lease);
        if !self.requests.contains_key(&host) && self.requests.len() >= Self::MAX_REQUESTS {
            let stalest = self
                .requests
                .iter()
                .min_by_key(|(_, (at, _, _))| *at)
                .map(|(stalest, _)| stalest.clone());
            if let Some(stalest) = stalest {
                self.requests.remove(&stalest);
            }
        }
        self.requests.insert(host, (now, width, height));
    }

    /// End `host`'s request. Returns whether it had one.
    pub fn stop(&mut self, host: &crate::wire::frame::HostId) -> bool {
        self.requests.remove(host).is_some()
    }

    /// Drop requests older than `lease` at `now`. Returns whether any went.
    pub fn lapse(&mut self, now: std::time::Instant, lease: std::time::Duration) -> bool {
        let before = self.requests.len();
        self.requests
            .retain(|_, (at, _, _)| now.saturating_duration_since(*at) <= lease);
        self.requests.len() != before
    }

    /// The size to render at: the largest width and the largest height any
    /// live request asked for, so no watching host gets a clipped view.
    #[must_use]
    pub fn viewport(&self) -> Option<(u16, u16)> {
        self.requests.values().fold(None, |size, (_, width, height)| {
            let (w, h) = size.unwrap_or((0, 0));
            Some(((*width).max(w), (*height).max(h)))
        })
    }
}

/// One plugin's `ui.state` view as the snapshot bus last delivered it. Never
/// in a frame: the view is JSON its plugin wrote, with keys no redaction check
/// knows in advance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginUiState {
    /// Snapshot bus version of the publish, increasing per topic.
    pub version: u64,
    /// The plugin's view, in the shape the plugin documents.
    pub view: serde_json::Value,
}

impl Default for PluginsHostSection {
    fn default() -> Self {
        Self {
            pending_plugin_renders: std::collections::HashMap::new(),
            plugin_captures_text: std::collections::HashMap::new(),
            plugin_render_errors: std::collections::HashMap::new(),
            plugin_presence: std::collections::BTreeMap::new(),
            plugin_ui_states: std::collections::HashMap::new(),
            plugin_ui_state_spent: std::collections::HashMap::new(),
            watched_plugin_screens: std::collections::BTreeMap::new(),
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
    /// Whether the Claude statusline is wired, from the shared probe, copied
    /// in on the tick when it changes. Renderers draw the statusline CTA from
    /// this rather than asking the probe, so a host that only receives
    /// sections draws it too.
    pub statusline_status: Option<crate::cli::statusline_install::StatuslineStatus>,
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
            statusline_status: None,
        }
    }
}

#[derive(Debug)]
pub struct WorkspaceLoadSection {
    // Background workspace loading state
    pub is_loading_workspaces: bool,
    pub workspace_load_error: Option<String>,
}

impl Default for WorkspaceLoadSection {
    fn default() -> Self {
        Self {
            is_loading_workspaces: false,
            workspace_load_error: None,
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
    // (live_window_watcher, the statusline probe) stays with the host app
    // because that's a host CLI install concern, not a plugin one.
    /// Current branch-refresh generation (bumped on every picker open).
    pub branch_refresh_seq: u64,
    /// Current repo-check generation (bumped on every Configure open).
    pub repo_check_seq: u64,
    /// Current repo-init generation.
    pub repo_init_seq: u64,
}

impl Default for NewSessionSection {
    fn default() -> Self {
        Self {
            new_session_state: None,
            branch_refresh_seq: 0,
            repo_check_seq: 0,
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
    // Track if current directory is a git repository
    pub last_logs_session_id: Option<Uuid>,
    // Log history viewer state
    pub log_history_state: crate::components::LogHistoryViewerState,
}

impl Default for LogsSection {
    fn default() -> Self {
        Self {
            logs: HashMap::new(),
            live_logs: HashMap::new(),
            last_logs_session_id: None,
            log_history_state: crate::components::LogHistoryViewerState::new(),
        }
    }
}

#[derive(Debug)]
pub struct TmuxSection {
    // The tmux session the host's live client for the preview pane is on, as
    // the host reported it: read-only while the sessions pane has focus,
    // interactive once the preview does. The client itself is the host's.
    // Setting this to `None` is how the reducer releases it: the host closes
    // any client whose session this no longer names. Invariants (focus can
    // drift, so none of these are assumed):
    //  - Input forwards only while `is_interactive_pane()` holds (a session
    //    here AND focused_pane == Preview).
    //  - Ctrl+Q releases only while interactive focus owns the terminal.
    //  - `tick_terminal_pane` releases when the session-list screen is no
    //    longer current, so keys are never forwarded to an invisible pane.
    // Re-entering on a DIFFERENT row releases the old client and attaches to
    // the new target instead of silently refocusing the stale one (see
    // `AppState::in_place_target`).
    pub embed_session: Option<crate::app::effect::TmuxSessionName>,
    // Other tmux sessions (not managed by agents-in-a-box)
    pub other_tmux_sessions: Vec<crate::models::OtherTmuxSession>,
    pub other_tmux_expanded: bool,
    pub selected_other_tmux_index: Option<usize>,
    pub selected_other_tmux_sessions: HashSet<String>, // Multi-selected external tmux names
    /// Whether we're in rename mode for the selected "Other tmux" session
    pub other_tmux_rename_mode: bool,
    /// Buffer for the new name being typed during rename
    pub other_tmux_rename_buffer: String,
}

impl Default for TmuxSection {
    fn default() -> Self {
        Self {
            embed_session: None,
            other_tmux_sessions: Vec::new(),
            other_tmux_expanded: true, // Default to expanded
            selected_other_tmux_index: None,
            selected_other_tmux_sessions: HashSet::new(),
            other_tmux_rename_mode: false,
            other_tmux_rename_buffer: String::new(),
        }
    }
}

#[derive(Debug)]
pub struct FleetSection {
    /// Per-session "cleared up to" timestamp (epoch ms). A hook event
    /// only marks a session if its `ts` is newer than this. Defaults to
    /// `0` (any event in the lookback window can mark); folded from
    /// `HostOnlyState::attention_attached_at` once, on the refresh that sees
    /// the session detach; read through `AppState::attention_clear_point`.
    pub attention_baseline: HashMap<Uuid, i64>,
    /// The watcher's latest snapshot, copied in on the tick when it changes.
    /// Renderers draw the status bar's quota widget from this, so a host that
    /// only receives sections draws it too.
    pub live_window: crate::models::live_window::LiveWindow,
    /// The `ask` pane's own state: which option is selected, what has been
    /// typed, and what the last send did.
    pub ask_state: crate::fleet::answer::AskState,
    /// The broadcast composer, shown on `thread` while rows are checked.
    ///
    /// Survives a change of checkbox set on purpose: an operator who ticks a
    /// fifth session halfway through typing must not lose what they typed.
    pub broadcast: crate::fleet::broadcast::Broadcast,
    /// The open conversation, as a frame carries it: bounded and scrubbed,
    /// written by the reducer's tick from the chat host in `HostOnlyState`.
    ///
    /// Here rather than in a section of its own (there is no twenty-first,
    /// #1076) and rather than on `ClaudeChatSection`, which is the Docker
    /// claude-chat pane: an ACP transcript under that name would put two
    /// unrelated things in one place. It belongs with `ask_state` and
    /// `broadcast` because it is the same attention-and-answer family.
    pub conversation: crate::fleet::conversation::Conversation,
    /// The open ACP transcript, bounded and scrubbed, written by the reducer's
    /// tick from the transcript host in `HostOnlyState`. The default whenever
    /// none is open, so the section never carries a closed run's tail.
    pub transcript: crate::fleet::transcript::Transcript,
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
    pub attention_local_since: HashMap<AttentionLocalKey, i64>,
}

/// Which local chip an [`FleetSection::attention_local_since`] clock belongs to.
///
/// Named rather than a tuple: a tuple cannot be a map key on any serialised
/// form (JSON or the TypeScript contract), and three positional fields invite
/// swapping `kind` and `detail` at a call site.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AttentionLocalKey {
    /// The session the chip is on.
    pub session_id: Uuid,
    /// The chip's kind.
    pub kind: AttentionKind,
    /// The chip's detail: two questions of one kind are two different waits.
    pub detail: Option<String>,
}

impl Default for FleetSection {
    fn default() -> Self {
        Self {
            attention_baseline: HashMap::new(),
            live_window: crate::models::live_window::LiveWindow::default(),
            ask_state: crate::fleet::answer::AskState::default(),
            broadcast: crate::fleet::broadcast::Broadcast::default(),
            conversation: crate::fleet::conversation::Conversation::default(),
            transcript: crate::fleet::transcript::Transcript::default(),
            daemon_attention: Arc::new(Mutex::new(
                crate::fleet::attention::DaemonAttention::default(),
            )),
            fleet_snapshot: Arc::new(Mutex::new(Vec::new())),
            fleet_metadata: HashMap::new(),
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
    pub(crate) pending_event: Option<crate::app::events::AppEvent>,
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

/// Rows the inbox section keeps of one `hangar/inbox_list` read. Below the
/// daemon's own cap of 200 (`INBOX_LIST_LIMIT`) on purpose, so the cut path
/// runs against a real daemon rather than only in a test.
pub const MAX_INBOX_ROWS: usize = 100;
/// Characters of `summary` kept per row. The aggregator writes an issue's
/// title into it with no cap of its own, so this is the bound that keeps one
/// row from being the whole frame.
pub const MAX_INBOX_SUMMARY_CHARS: usize = 256;
/// Appended to a summary that was cut, so a short summary and a cut one are
/// not read as the same thing.
pub const INBOX_SUMMARY_CUT_MARKER: &str = " [cut]";
/// Longest id-shaped field (`id`, `subject_id`, `kind`, `event`, `recipient`)
/// a row may carry. A ULID is 26 characters; anything past this is not an id,
/// and the row is dropped and counted rather than trusted. The content is
/// checked too: an id is ASCII letters, digits, `-`, `_`, `:` and `.`, and a
/// row carrying anything else in an id field is dropped the same way.
pub const MAX_INBOX_ID_CHARS: usize = 128;
/// Characters kept of a host's own reason (`absent`, `unreachable`). A daemon
/// error message can be as long as the client accepts, and a reason that
/// blanked the section would be the failure the budget exists to stop.
pub const MAX_INBOX_REASON_CHARS: usize = 512;
/// The section's encoded byte budget, well under `MAX_FRAME_BYTES`, because a
/// section past the ceiling is withheld whole and a withheld inbox is a blank
/// inbox with no counter to explain it. Held by construction: the caps above
/// bound every string, and `tests/inbox_bound.rs` frames the worst case.
pub const MAX_INBOX_BYTES: usize = 1024 * 1024;
/// The actor whose inbox every surface reads: the local human, the same
/// recipient the hangar plugin names. A workspace or actor picker is not this
/// node's.
pub const INBOX_RECIPIENT: &str = "member:me";
/// The workspace the read names: the daemon's default, as the hangar plugin
/// sends it (`DEFAULT_WORKSPACE_ID`).
pub const INBOX_WORKSPACE_ID: &str = "default";

/// Section 16: the daemon's notification inbox (D3-prime).
///
/// One actor's `hangar/inbox_list` read, folded: the rows newest-first, cut
/// to [`MAX_INBOX_ROWS`] with every summary scrubbed then cut to
/// [`MAX_INBOX_SUMMARY_CHARS`], and the unread count the daemon reported.
/// Populated by a host-owned reader (the host owns the socket); this crate
/// only reduces. The section kept its place, empty, from the extraction
/// until the screen came back, so nothing renumbered.
#[derive(Debug, Default)]
pub struct InboxSection {
    /// The rows a surface draws, newest first, already bounded and scrubbed.
    pub entries: Vec<ainb_hangar_proto::events::InboxEntryRow>,
    /// The daemon's unread count for `recipient`, not derived from `entries`,
    /// since the rows kept may be fewer than the rows unread.
    pub unread: i64,
    /// The actor whose inbox this is (`member:me` today).
    pub recipient: String,
    /// Why there are no rows, when there are none and the host knows why.
    pub absent: Option<String>,
    /// The last read failed for this reason; the rows shown are the last
    /// ones that landed. Cleared by the next read.
    pub unreachable: Option<String>,
    /// Rows the daemon sent that the fold did not keep: past the row cap, or
    /// carrying an id-shaped field that is not an id.
    pub rows_cut: usize,
    /// Summaries cut to [`MAX_INBOX_SUMMARY_CHARS`] in the rows kept.
    pub summaries_cut: usize,
    /// The local clock when the last read landed, epoch milliseconds.
    pub received_at_ms: i64,
}

impl InboxSection {
    /// Fold one read: bound it, scrub it, count what went. True when anything
    /// a surface renders changed; the same rows again is not a change.
    pub fn apply_read(
        &mut self,
        read: ainb_hangar_proto::snapshots::InboxListResult,
        recipient: &str,
        received_at_ms: i64,
    ) -> bool {
        let sent = read.entries.len();
        let mut summaries_cut = 0;
        let mut kept: Vec<_> = read
            .entries
            .into_iter()
            .filter(|row| {
                let ids = [
                    &row.id,
                    &row.subject_id,
                    &row.kind,
                    &row.event,
                    &row.recipient,
                ];
                ids.iter().all(|id| id_like(id))
            })
            .collect();
        kept.truncate(MAX_INBOX_ROWS);
        // Every row sent that is not in the kept set was cut, whether for
        // its ids or for the cap.
        let rows_cut = sent - kept.len();
        let entries: Vec<_> = kept
            .into_iter()
            .map(|mut row| {
                // Scrub before the cut, never after: cutting first could keep
                // the head of a credential the scrubber no longer recognises.
                let scrubbed = crate::fleet::bridge::redact::scrub(&row.summary);
                row.summary = match cut_chars(&scrubbed, MAX_INBOX_SUMMARY_CHARS) {
                    Some(head) => {
                        summaries_cut += 1;
                        format!("{head}{INBOX_SUMMARY_CUT_MARKER}")
                    }
                    None => scrubbed,
                };
                row
            })
            .collect();
        let changed = self.entries != entries
            || self.unread != read.unread
            || self.recipient != recipient
            || self.absent.is_some()
            || self.unreachable.is_some()
            || self.rows_cut != rows_cut
            || self.summaries_cut != summaries_cut;
        self.entries = entries;
        self.unread = read.unread;
        self.recipient = recipient.to_string();
        self.absent = None;
        self.unreachable = None;
        self.rows_cut = rows_cut;
        self.summaries_cut = summaries_cut;
        self.received_at_ms = received_at_ms;
        changed
    }

    /// The host's read failed: the rows stay, and the surface says why they
    /// may be stale. Without rows the section is absent for `reason`.
    pub fn mark_read_failed(&mut self, reason: impl Into<String>) -> bool {
        let reason = bound_reason(&reason.into());
        // With no rows to keep there is nothing to be unreachable from: the
        // section is absent, and a repeated failure replaces that one reason
        // rather than adding a second beside it.
        if self.entries.is_empty() {
            return self.mark_absent(reason);
        }
        let changed = self.unreachable.as_deref() != Some(reason.as_str());
        self.unreachable = Some(reason);
        changed
    }

    /// The daemon cannot serve the read at all: no rows, and why.
    pub fn mark_absent(&mut self, reason: impl Into<String>) -> bool {
        let reason = bound_reason(&reason.into());
        let changed = !self.entries.is_empty()
            || self.unread != 0
            || self.absent.as_deref() != Some(reason.as_str());
        self.entries.clear();
        self.unread = 0;
        self.rows_cut = 0;
        self.summaries_cut = 0;
        // No read is on screen once the section is absent, so no stamp either.
        self.received_at_ms = 0;
        self.unreachable = None;
        self.absent = Some(reason);
        changed
    }

    /// The host reconnected: drop everything so the next read builds fresh.
    pub fn reset(&mut self) -> bool {
        let changed =
            !self.entries.is_empty() || self.absent.is_some() || self.unreachable.is_some();
        *self = Self::default();
        changed
    }

    /// The daemon answered a "mark all read" sweep: fold the unread count it
    /// reported. The rows' `read_at` stamps are the daemon's, never a local
    /// clock, so they arrive with the read the host makes after the sweep
    /// (`apply_read`), not here.
    pub fn apply_mark_all_read(&mut self, unread: i64) -> bool {
        let changed = self.unread != unread;
        self.unread = unread;
        changed
    }
}

/// The first `max` characters of `text` when it is longer than `max`, else
/// `None`. Cuts on a character boundary, never inside one.
fn cut_chars(text: &str, max: usize) -> Option<&str> {
    let end = text.char_indices().nth(max).map(|(index, _)| index)?;
    Some(&text[..end])
}

/// A host's reason as the section keeps it: scrubbed, then cut to
/// [`MAX_INBOX_REASON_CHARS`] with the marker, the row summary's recipe.
fn bound_reason(reason: &str) -> String {
    let scrubbed = crate::fleet::bridge::redact::scrub(reason);
    match cut_chars(&scrubbed, MAX_INBOX_REASON_CHARS) {
        Some(head) => format!("{head}{INBOX_SUMMARY_CUT_MARKER}"),
        None => scrubbed,
    }
}

/// Whether `value` is shaped like an id the daemon mints or names: no longer
/// than [`MAX_INBOX_ID_CHARS`], and only the id alphabet. Free text in an id
/// field is not scrubbed into place; the row is dropped and counted.
pub fn id_like(value: &str) -> bool {
    let mut count = 0;
    for c in value.chars() {
        count += 1;
        if count > MAX_INBOX_ID_CHARS {
            return false;
        }
        if !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.')) {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod inbox_section_tests {
    use super::{INBOX_SUMMARY_CUT_MARKER, InboxSection, MAX_INBOX_SUMMARY_CHARS, cut_chars};
    use ainb_hangar_proto::events::InboxEntryRow;
    use ainb_hangar_proto::snapshots::InboxListResult;

    fn row(summary: &str) -> InboxEntryRow {
        InboxEntryRow {
            id: "01J0".into(),
            kind: "issue".into(),
            event: "issue_created".into(),
            subject_id: "issue-1".into(),
            summary: summary.into(),
            recipient: "member:me".into(),
            created_at: 1,
            read_at: None,
        }
    }

    #[test]
    fn cut_chars_is_none_at_or_under_the_limit() {
        assert_eq!(cut_chars("abc", 3), None);
        assert_eq!(cut_chars("abcd", 3), Some("abc"));
        assert_eq!(cut_chars("ééé", 2), Some("éé"));
    }

    #[test]
    fn a_failed_first_read_is_absent_and_a_failed_later_read_is_unreachable() {
        let mut section = InboxSection::default();
        assert!(section.mark_read_failed("connect: refused"));
        assert_eq!(section.absent.as_deref(), Some("connect: refused"));
        assert!(section.apply_read(
            InboxListResult {
                entries: vec![row("a")],
                unread: 1,
            },
            "member:me",
            5,
        ));
        assert!(section.absent.is_none());
        assert!(section.mark_read_failed("io"));
        assert_eq!(section.unreachable.as_deref(), Some("io"));
        assert_eq!(section.entries.len(), 1);
        assert!(
            !section.mark_read_failed("io"),
            "the same reason twice is no change"
        );
    }

    #[test]
    fn a_repeated_failure_before_any_read_carries_one_reason() {
        let mut section = InboxSection::default();
        assert!(section.mark_read_failed("first"));
        assert!(section.mark_read_failed("second"));
        assert_eq!(section.absent.as_deref(), Some("second"));
        assert!(
            section.unreachable.is_none(),
            "absent and unreachable never both"
        );
    }

    #[test]
    fn a_long_summary_is_cut_once_and_marked() {
        let mut section = InboxSection::default();
        let long = "x".repeat(MAX_INBOX_SUMMARY_CHARS * 2);
        section.apply_read(
            InboxListResult {
                entries: vec![row(&long)],
                unread: 1,
            },
            "member:me",
            5,
        );
        assert_eq!(section.summaries_cut, 1);
        assert!(section.entries[0].summary.ends_with(INBOX_SUMMARY_CUT_MARKER));
    }

    #[test]
    fn a_reason_is_scrubbed_then_cut() {
        let mut section = InboxSection::default();
        let long = format!("connect failed sk-{} {}", "k".repeat(48), "z".repeat(2000));
        section.mark_absent(long);
        let reason = section.absent.as_deref().unwrap();
        assert!(!reason.contains(&"k".repeat(48)));
        assert!(reason.ends_with(INBOX_SUMMARY_CUT_MARKER));
        assert!(
            reason.chars().count()
                <= super::MAX_INBOX_REASON_CHARS + INBOX_SUMMARY_CUT_MARKER.len()
        );
    }

    #[test]
    fn id_like_takes_the_id_alphabet_only() {
        assert!(super::id_like("01J0ABCDEFGHJKMNPQRSTVWXYZ"));
        assert!(super::id_like("member:me"));
        assert!(super::id_like("issue_created"));
        assert!(super::id_like("v1.2-rc"));
        assert!(!super::id_like("has space"));
        assert!(!super::id_like("ctl\u{1}"));
        assert!(!super::id_like("é"));
        assert!(!super::id_like(&"a".repeat(super::MAX_INBOX_ID_CHARS + 1)));
    }

    #[test]
    fn absent_after_a_read_then_a_failure_carries_one_reason_and_no_rows() {
        let mut section = InboxSection::default();
        section.apply_read(
            InboxListResult {
                entries: vec![row("a")],
                unread: 1,
            },
            "member:me",
            5,
        );
        assert!(section.mark_absent("daemon has no inbox_list"));
        assert!(section.mark_read_failed("connect: refused"));
        assert_eq!(section.absent.as_deref(), Some("connect: refused"));
        assert!(
            section.unreachable.is_none(),
            "never both reasons with zero rows"
        );
        assert!(section.entries.is_empty());
        assert_eq!(section.received_at_ms, 0);
    }

    #[test]
    fn reset_drops_everything() {
        let mut section = InboxSection::default();
        section.mark_absent("gone");
        assert!(section.reset());
        assert!(section.absent.is_none());
        assert!(!section.reset());
    }
}

/// Section 20: agent status, the D14 one truth for every surface (T0-section,
/// #1015).
///
/// Holds the last `fleet/roster_status` read folded by
/// [`ainb_hangar_proto::status_view::StatusView`], the SAME reducer the TUI
/// Fleet panel folds through, so a surface rendering from this section alone
/// shows the panel's words. Populated by the host (it owns the socket); this
/// crate only reduces.
///
/// Deliberately not `Serialize` (#983): it must not leave the process until
/// the redaction layer exists. `agent_status_section_is_not_serialize` pins it.
#[derive(Debug, Default)]
pub struct AgentStatusSection {
    /// The folded view, `None` until a read lands or while absent.
    pub view: Option<ainb_hangar_proto::status_view::StatusView>,
    /// Why there is no view, when there is none and the host knows why.
    pub absent: Option<String>,
    /// The newest Fleet revision the host has been told about. Retained across
    /// a reset, so the first read after a reconnect that lands below it renders
    /// stale instead of live (#1019 review).
    pub head_revision: i64,
}

impl AgentStatusSection {
    /// Fold one joined read. True when anything a surface renders changed;
    /// a heartbeat-only read (same cards, newer revision) is not a change.
    pub fn apply_read(
        &mut self,
        read: ainb_hangar_proto::agent_status::RosterStatusResult,
        received_at_ms: i64,
    ) -> bool {
        let had_absent = self.absent.take().is_some();
        let changed = if let Some(view) = &mut self.view {
            view.apply(read, received_at_ms)
        } else {
            let mut view =
                ainb_hangar_proto::status_view::StatusView::from_read(read, received_at_ms);
            view.observe_head(self.head_revision);
            self.view = Some(view);
            true
        };
        changed || had_absent
    }

    /// The host's read failed. With a view its rows freeze and the host is
    /// unreachable; without one the section is absent for `reason`.
    pub fn mark_read_failed(&mut self, reason: impl Into<String>, now_ms: i64) -> bool {
        let reason = reason.into();
        if let Some(view) = &mut self.view {
            view.mark_unreachable(reason, now_ms)
        } else {
            let changed = self.absent.as_deref() != Some(reason.as_str());
            self.absent = Some(reason);
            changed
        }
    }

    /// The daemon cannot serve the read at all: no view, and why.
    pub fn mark_absent(&mut self, reason: impl Into<String>) -> bool {
        let reason = reason.into();
        let changed = self.view.is_some() || self.absent.as_deref() != Some(reason.as_str());
        self.view = None;
        self.absent = Some(reason);
        changed
    }

    /// A newer Fleet revision was observed: rows go stale until a read lands.
    pub fn observe_head(&mut self, head_revision: i64) -> bool {
        self.head_revision = self.head_revision.max(head_revision);
        self.view.as_mut().is_some_and(|view| view.observe_head(head_revision))
    }

    /// The host reconnected: drop the view so the next read builds a fresh
    /// one, keeping the head so that read cannot claim to be live below it.
    pub fn reset(&mut self) -> bool {
        let changed = self.view.is_some() || self.absent.is_some();
        self.view = None;
        self.absent = None;
        changed
    }
}

#[cfg(test)]
mod agent_status_section_tests {
    use super::AgentStatusSection;
    use crate::app::versioned::Versioned;
    use ainb_hangar_proto::agent_status::{RosterStatusResult, RosterStatusRow, status_row};
    use ainb_hangar_proto::fleet::{
        AttentionState, FleetCapabilities, FleetConfidence, FleetProvenance, FleetProvider,
        FleetSession, LifecycleState, ManagementState, PaneBinding, TransportHealth,
    };
    use ainb_hangar_proto::status_view::ViewHealth;

    fn session(attention: AttentionState, heartbeat: i64) -> FleetSession {
        FleetSession {
            session_key: "claude:one".into(),
            provider: FleetProvider::Claude,
            provider_session_id: Some("one".into()),
            tmux_target: Some("dev:1.0".into()),
            pane_binding: PaneBinding::Bound,
            process_start_fingerprint: None,
            cwd: "/w/app".into(),
            display_name: None,
            lifecycle: LifecycleState::Running,
            active_work_count: 0,
            attention,
            current_request_fingerprint: None,
            current_request: None,
            management: ManagementState::Managed,
            transport_health: TransportHealth::Healthy,
            capabilities: FleetCapabilities::default(),
            provenance: FleetProvenance::Authoritative,
            confidence: FleetConfidence::High,
            discovered_at: 1,
            last_observed_at: heartbeat,
            lifecycle_updated_at: 5,
            attention_updated_at: 7,
            model: None,
            reasoning_effort: None,
            model_updated_at: 0,
            version: heartbeat,
            updated_revision: heartbeat,
        }
    }

    fn read(revision: i64, session: FleetSession) -> RosterStatusResult {
        let status = status_row(&session, session.attention != AttentionState::None);
        RosterStatusResult {
            rows: vec![RosterStatusRow {
                session,
                status,
                read_revision: revision,
            }],
            read_revision: revision,
            unknown_events: Vec::new(),
            read_at_ms: 0,
        }
    }

    /// #1015: section 20's version bumps on a status-row change and not on a
    /// transport heartbeat that alters no rendered fact.
    #[test]
    fn a_heartbeat_leaves_the_section_version_untouched() {
        let mut section = Versioned::new(AgentStatusSection::default());
        assert!(section.update(|s| s.apply_read(read(3, session(AttentionState::Ask, 10)), 100)));
        let after_first = section.version();

        assert!(!section.update(|s| s.apply_read(read(4, session(AttentionState::Ask, 11)), 200)));
        assert_eq!(
            section.version(),
            after_first,
            "a heartbeat is not a change"
        );

        assert!(section.update(|s| s.apply_read(read(5, session(AttentionState::None, 12)), 300)));
        assert_eq!(
            section.version(),
            after_first + 1,
            "a state change is one bump"
        );
    }

    /// #1015 failure story: absent without rows, unreachable with rows frozen,
    /// stale when a newer revision is seen; each changes the version once.
    #[test]
    fn absent_unreachable_and_stale_are_section_facts() {
        let mut section = Versioned::new(AgentStatusSection::default());
        assert!(section.update(|s| s.mark_read_failed("connection refused", 1)));
        assert_eq!(section.absent.as_deref(), Some("connection refused"));
        assert!(!section.update(|s| s.mark_read_failed("connection refused", 2)));

        section.update(|s| s.apply_read(read(3, session(AttentionState::Ask, 10)), 100));
        assert_eq!(section.absent, None, "a landed read clears absent");
        assert!(section.update(|s| s.observe_head(6)));
        assert!(matches!(
            section.view.as_ref().unwrap().health,
            ViewHealth::Stale {
                read_revision: 3,
                head_revision: 6
            }
        ));
        assert!(section.update(|s| s.mark_read_failed("daemon gone", 500)));
        let view = section.view.as_ref().unwrap();
        assert!(matches!(
            &view.health,
            ViewHealth::Unreachable {
                stale_since_ms: 500,
                ..
            }
        ));
        assert_eq!(view.cards.len(), 1, "rows stay frozen as last read");

        assert!(section.update(|s| s.mark_absent("daemon has no fleet/roster_status")));
        assert!(section.view.is_none());
    }

    /// #1019 review: across a reconnect the section resets, keeps the head it
    /// was told, and a read that lands below that head renders stale, not live.
    #[test]
    fn a_lower_revision_after_a_reconnect_renders_stale_not_live() {
        let mut section = Versioned::new(AgentStatusSection::default());
        section.update(|s| s.apply_read(read(40, session(AttentionState::Ask, 10)), 100));
        section.update(|s| s.observe_head(42));
        assert!(section.update(AgentStatusSection::reset));
        assert!(section.view.is_none());
        assert_eq!(section.head_revision, 42, "the head survives the reset");

        // A rebuilt store answers from a reset revision counter.
        section.update(|s| s.apply_read(read(3, session(AttentionState::Ask, 11)), 200));
        assert!(matches!(
            section.view.as_ref().unwrap().health,
            ViewHealth::Stale {
                read_revision: 3,
                head_revision: 42
            }
        ));
    }

    /// #983: section 20 must not be serialisable until the redaction layer
    /// exists. Autoref probe: the inherent const wins only if `Serialize` holds.
    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn agent_status_section_is_not_serialize() {
        trait NotSerialize {
            const IS_SERIALIZE: bool = false;
        }
        struct Probe<T: ?Sized>(std::marker::PhantomData<T>);
        impl<T: ?Sized> NotSerialize for Probe<T> {}
        #[allow(dead_code)]
        impl<T: ?Sized + serde::Serialize> Probe<T> {
            const IS_SERIALIZE: bool = true;
        }
        assert!(!<Probe<AgentStatusSection>>::IS_SERIALIZE);
        assert!(!<Probe<ainb_hangar_proto::status_view::StatusView>>::IS_SERIALIZE);
        assert!(!<Probe<ainb_hangar_proto::status_view::AgentCard>>::IS_SERIALIZE);
        assert!(
            <Probe<ainb_hangar_proto::agent_status::AgentStatusRow>>::IS_SERIALIZE,
            "probe works"
        );
    }
}

/// A base-branch refresh result: its generation, and the branches or why not.
pub type BranchRefreshPayload = (
    u64,
    Result<Vec<crate::git::branch_list::BranchEntry>, String>,
);

/// What `AppState::project_conversation` last read: the open topic, its
/// composer's length in characters, and its send refusal.
pub(crate) type ConversationMark = (
    ainb_plugin_hangar::screen::fleet_chat::ChatTopic,
    usize,
    Option<String>,
);

/// What only the process running the reducer can use: channels and task
/// handles, worker liveness flags, the handles background workers write
/// through, and the timers that pace the tick.
///
/// Deliberately not a section: none of it crosses to another process, so
/// writing it bumps no version and no frame carries it. The terminal host still
/// draws three of its handles until D1, listed in `HOST_STATE_READS` in
/// `tests/host_side_effects.rs`.
#[derive(Debug)]
pub struct HostOnlyState {
    /// What kind of surface this process is, as it names itself to the daemon.
    ///
    /// The answer path carries it to `attention/answer`, and the daemon
    /// records the winning row under it (base spec `:307`, `answered_by =
    /// "<kind>@<host>"`), so this is how a second surface learns whether the
    /// person who answered sat at a terminal or at the desktop shell.
    ///
    /// `Tui` unless a host says otherwise: the terminal is the reducer's
    /// original surface, so no existing host changes its provenance by the
    /// field arriving, and a new host that forgets is recorded as the one it
    /// was a copy of rather than as `unknown`.
    pub surface: ainb_hangar_proto::connections::SurfaceKind,
    // Tmux integration
    pub tmux_sessions: HashMap<Uuid, crate::tmux::TmuxSession>,
    /// Whether a workspace scan has ever been applied. A later scan that finds
    /// the same list writes nothing, so a host that rescans on a timer does not
    /// reframe the whole Sessions section, reset the selection or raise a
    /// notice every time; the first one always applies.
    pub(crate) workspaces_applied: bool,
    pub preview_update_task: Option<tokio::task::JoinHandle<()>>,
    // A changed selection must settle before starting a read-only client.
    pub(crate) observer_pending: Option<(String, Instant)>,
    // A read-only observer that dies waits before the next retry.
    pub(crate) observer_failed_target: Option<(String, Instant, u8)>,
    // A spawned observer must survive briefly before it clears a prior retry
    // count. `tmux attach-session` reports some startup failures asynchronously.
    pub(crate) observer_started_at: Option<Instant>,
    // The host answered an in-place attach with `unsupported`: it has no
    // writable terminal, so the key stops asking it for one.
    pub(crate) in_place_unsupported: bool,
    // The tmux session this host runs in, as the host reported it, or `None`
    // outside tmux. The own-session rule reads it: that row's preview would
    // mirror the host into itself, and attaching it would nest it. The reducer
    // never looks it up. A raw name, only ever compared: a session tmux accepts
    // but `TmuxSessionName` refuses must still match its own row.
    pub(crate) host_tmux_session: Option<String>,
    pub(crate) workspace_load_started: Option<Instant>,
    /// Channel receiver for background workspace loading results
    pub(crate) workspace_load_receiver: Option<mpsc::UnboundedReceiver<WorkspaceLoadResult>>,
    /// When a host's rescan cadence runs from: the end of the last workspace
    /// scan, or the state's creation before any has finished.
    pub(crate) workspace_rescan_from: Instant,
    /// The daemon's publish counter as it stood when the last workspace scan
    /// started, so news it brings can start the next one early. `None` until
    /// a scan has started: news is a reason to look again.
    pub(crate) workspace_scanned_generation: Option<u64>,
    // Periodic session snapshot tracking
    pub last_snapshot_time: Option<Instant>,
    // Throttled tmux preview updates (avoid spawning subprocesses every 250ms tick)
    pub last_preview_update: Option<Instant>,
    /// When attention was last merged. Every host calls
    /// `AppState::refresh_attention` on its tick and the throttle lives there:
    /// a merge reads the notifications store, so it runs on daemon news at
    /// once and otherwise on this cadence.
    pub last_attention_refresh: Option<Instant>,
    // Throttle for the cheaper non-selected-session status sweep. Status
    // (running/idle) is not time-critical, so it polls on a longer cadence than
    // the selected session's live preview: one `capture-pane` subprocess per
    // non-selected session is only spawned every `STATUS_INTERVAL_SECS`, not on
    // every 5s preview refresh. (perf: bead 9pb)
    pub last_status_check: Option<Instant>,
    /// Background base-branch refresh for the Configure picker. The fetch +
    /// re-list runs on `spawn_blocking`; the result lands here and is applied
    /// by `check_branch_refresh_complete` on the next tick. The `u64` is a
    /// generation guard, so results from a closed or reopened picker are dropped.
    pub branch_refresh_receiver: Option<mpsc::UnboundedReceiver<BranchRefreshPayload>>,
    /// Background remote-repo pre-flight for the Configure screen (ls-remote
    /// at open: does the repo exist, does it have branches). Applied by
    /// `check_repo_check_complete` on the next tick; the `u64` is a
    /// generation guard so a stale check can't stamp a newer Configure form.
    pub repo_check_receiver: Option<mpsc::UnboundedReceiver<RepoCheckPayload>>,
    /// Background empty-remote initialization (`[i]` on Configure: README +
    /// initial commit + push). `Ok(branch)` carries the branch the commit
    /// landed on. Applied by `check_repo_init_complete` on the next tick.
    pub repo_init_receiver: Option<mpsc::UnboundedReceiver<(u64, Result<String, String>)>>,
    // Track when logs were last updated for each session
    pub log_last_updated: HashMap<Uuid, std::time::Instant>,
    // Track the last time we checked for log updates globally
    pub last_log_check: Option<std::time::Instant>,
    // Claude API client manager (when initialized)
    pub log_streaming_coordinator: Option<LogStreamingCoordinator>,
    // Channel sender for log streaming
    pub log_sender: Option<mpsc::UnboundedSender<(Uuid, LogEntry)>>,
    /// The `log` tab's history, filled by [`crate::fleet::session_log`] on its
    /// own thread.
    ///
    /// Read on the render path, never QUERIED there: the store read used to
    /// live inside `terminal.draw` and cost a real store up to 948 ms a frame.
    pub session_log: Arc<crate::fleet::session_log::Shared>,
    /// Whether the session-log worker is alive. Same idempotence flag, and the
    /// same reason, as [`Self::attention_poll_running`].
    pub session_log_running: Arc<std::sync::atomic::AtomicBool>,
    /// Background poller for the live OAuth-window snapshot. The render
    /// path reads via `snapshot()` (cheap `RwLock` read + clone) instead of
    /// calling `live_window::current()` directly, because Tier 2's JSONL walk
    /// would otherwise stall input handling on every frame.
    pub live_window_watcher: crate::models::live_window_watcher::LiveWindowWatcher,
    // Track the last Headroom proxy watchdog tick (re-ensure if a Headroom
    // session is live but the proxy died).
    pub last_headroom_watchdog: Option<std::time::Instant>,
    // Track the last time we checked for OAuth token refresh
    pub last_token_refresh_check: Option<std::time::Instant>,
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
    /// The selected session's own thread, rebuilt when the selection moves to a
    /// different session.
    ///
    /// One host, not one per session: a thread the operator has navigated away
    /// from is not being read, and keeping N of them alive means N poll loops
    /// against the daemon for conversations nobody is looking at.
    pub session_chat: Option<(String, crate::fleet::chat_host::ChatHost)>,
    /// What the framed conversation was last projected from: the open topic,
    /// its composer's length and its send refusal. A tick that reports no news
    /// and finds this unchanged leaves `fleet.conversation` alone rather than
    /// rebuilding fifty rows to compare them.
    pub(crate) conversation_mark: Option<ConversationMark>,
    /// The ACP transcript a person opened from the board, when one is open.
    ///
    /// An ACP session has no tmux pane, so this is what stands in for its
    /// terminal. Opened by `session_list.open_transcript`, ticked by the
    /// reducer's tick while the session list shows, and dropped on close; a
    /// page still in flight then reports into an inbox nobody reads.
    pub transcript: Option<crate::fleet::transcript::TranscriptHost>,
    /// Whether the attention poller thread is alive, so the render loop can
    /// start one without having to remember whether it already did.
    pub attention_poll_running: Arc<std::sync::atomic::AtomicBool>,
    /// The attention poller's publish counter.
    ///
    /// `FleetSection::daemon_attention` and `fleet_snapshot` are shared
    /// handles: the worker writes through them without anything taking `&mut`,
    /// so the section version would never move for daemon-side news. The
    /// counter is read by `&` like the cells, and
    /// `FleetSection::daemon_attention_seen` is the versioned copy that
    /// `refresh_daemon_attention_generation` folds it into once a frame.
    pub daemon_attention_generation: crate::fleet::attention_poll::Generation,
    /// A "mark all read" sweep the host is sending now. One held key is one
    /// sweep: a second press while it is in flight emits nothing, and the
    /// report clears it.
    pub inbox_mark_in_flight: bool,
    /// When each attached session was last seen attached by
    /// `AppState::refresh_attention`.
    ///
    /// An attached session's clear point moves to "now" on every refresh.
    /// Writing that to `FleetSection::attention_baseline` each time would bump
    /// the Fleet section for nothing, so the instant is held here and folded
    /// into the baseline once, on the refresh that sees the session detached.
    pub attention_attached_at: HashMap<Uuid, i64>,
}

impl Default for HostOnlyState {
    fn default() -> Self {
        Self {
            surface: ainb_hangar_proto::connections::SurfaceKind::Tui,
            tmux_sessions: HashMap::new(),
            workspaces_applied: false,
            preview_update_task: None,
            observer_pending: None,
            observer_failed_target: None,
            observer_started_at: None,
            in_place_unsupported: false,
            host_tmux_session: None,
            workspace_load_started: None,
            workspace_load_receiver: None,
            workspace_rescan_from: Instant::now(),
            workspace_scanned_generation: None,
            last_snapshot_time: None,
            last_preview_update: None,
            last_attention_refresh: None,
            last_status_check: None,
            branch_refresh_receiver: None,
            repo_check_receiver: None,
            repo_init_receiver: None,
            log_last_updated: HashMap::new(),
            last_log_check: None,
            log_streaming_coordinator: None,
            log_sender: None,
            session_log: Arc::new(crate::fleet::session_log::Shared::default()),
            session_log_running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            live_window_watcher: crate::models::live_window_watcher::LiveWindowWatcher::default(),
            last_headroom_watchdog: None,
            last_token_refresh_check: None,
            pal_chat: None,
            pal_dial: crate::fleet::pal_dial::PalDial::new(),
            daemon_start_cta: crate::fleet::daemon_cta::DaemonStartCta::default(),
            session_chat: None,
            conversation_mark: None,
            transcript: None,
            attention_poll_running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            daemon_attention_generation: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            inbox_mark_in_flight: false,
            attention_attached_at: HashMap::new(),
        }
    }
}
