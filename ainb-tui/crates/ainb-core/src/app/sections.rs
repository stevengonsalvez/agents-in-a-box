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
