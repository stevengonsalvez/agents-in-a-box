// ABOUTME: The per-section wire seam. `section_json` is the ONLY way a section
// becomes JSON, so every mirror host, test and doctor check sees the same frame.
//
// No section and not `AppState` derives `Serialize` (issue #983). Instead each
// section has a borrowed view here that names the fields a frame carries and
// leaves out the live handles (channels, `Instant`s, tmux clients, chat hosts).
// Everything BELOW a section serialises through its own derive, which is where
// the redaction lives: `#[serde(skip)]` on credential buffers, `SecretInput`
// split off the generic popup, redacting serializers on env maps and the
// `[fleet.bridge]` table, and `redact::scrub` on captured terminal and diff
// text. The views are what a later `#[derive(Serialize)]` on the sections turns
// into `#[serde(skip)]` lists; `tests/state_serde.rs` locks the shape so that
// swap cannot widen the frame without a failing fixture.
//
// The four leak checks and the key-path fixture all read the frame through
// [`serialize_section`], so they judge exactly what a host receives.

pub mod fields;
pub mod shape;
pub mod trace;

use crate::app::AppState;
use crate::app::sections::{
    ClaudeChatSection, ConfigSection, FleetSection, GitViewSection, HangarSection, LogsSection,
    McpPoolSection, NewSessionSection, OnboardingSection, PluginUiState, PluginsHostSection,
    RecoverySection, SessionLabelsSection, SessionsSection, ShellSection, SkillsSection,
    SshSection, TmuxSection, WorkspaceLoadSection,
};
use crate::app::versioned::SectionId;
use serde::{Serialize, Serializer};
use std::sync::Mutex;

/// The JSON a mirror host receives for one section.
///
/// Reads the poller-published cells (`FleetSection.daemon_attention`,
/// `fleet_snapshot`, the Daemons snapshot) under their locks, so a caller must
/// not hold any of those locks across this call.
///
/// # Panics
///
/// Never in practice: every type reachable from a view serialises to JSON
/// without a non-string map key, which `state_serde.rs` proves per section.
#[must_use]
pub fn section_json(state: &AppState, id: SectionId) -> serde_json::Value {
    serialize_section(state, id, serde_json::value::Serializer)
        .expect("a section view always serialises to JSON")
}

/// Stable wire name of a section, used as the frame key and the fixture root.
#[must_use]
pub const fn section_name(id: SectionId) -> &'static str {
    match id {
        SectionId::Sessions => "sessions",
        SectionId::SessionLabels => "session_labels",
        SectionId::Tmux => "tmux",
        SectionId::Ssh => "ssh",
        SectionId::GitView => "git_view",
        SectionId::WorkspaceLoad => "workspace_load",
        SectionId::NewSession => "new_session",
        SectionId::Logs => "logs",
        SectionId::ClaudeChat => "claude_chat",
        SectionId::Fleet => "fleet",
        SectionId::Hangar => "hangar",
        SectionId::McpPool => "mcp_pool",
        SectionId::Inbox => "inbox",
        SectionId::PluginsHost => "plugins_host",
        SectionId::Config => "config",
        SectionId::Skills => "skills",
        SectionId::Recovery => "recovery",
        SectionId::Onboarding => "onboarding",
        SectionId::Shell => "shell",
    }
}

/// Serialise one section's view into any serde `Serializer`.
///
/// Generic so the type tracer in [`trace`] walks the same values `section_json`
/// emits, with the declared Rust type of every field in hand.
///
/// # Errors
///
/// Whatever `serializer` reports.
pub fn serialize_section<S: Serializer>(
    state: &AppState,
    id: SectionId,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    // Frame-only redaction on persisted types (see `fields`) is live for
    // exactly this call.
    let _frame = fields::FrameScope::enter();
    match id {
        SectionId::Sessions => SessionsView::from(&*state.sessions).serialize(serializer),
        SectionId::SessionLabels => {
            SessionLabelsView::from(&*state.session_labels).serialize(serializer)
        }
        SectionId::Tmux => TmuxView::from(&*state.tmux).serialize(serializer),
        SectionId::Ssh => SshView::from(&*state.ssh).serialize(serializer),
        SectionId::GitView => GitViewView::from(&*state.git_view).serialize(serializer),
        SectionId::WorkspaceLoad => {
            WorkspaceLoadView::from(&*state.workspace_load).serialize(serializer)
        }
        SectionId::NewSession => NewSessionView::from(&*state.new_session).serialize(serializer),
        SectionId::Logs => LogsView::from(&*state.log_streams).serialize(serializer),
        SectionId::ClaudeChat => ClaudeChatView::from(&*state.claude_chat).serialize(serializer),
        SectionId::Fleet => FleetView::from(&*state.fleet).serialize(serializer),
        SectionId::Hangar => HangarView::from(&*state.hangar).serialize(serializer),
        SectionId::McpPool => McpPoolView::from(&*state.mcp_pool).serialize(serializer),
        SectionId::Inbox => InboxView {}.serialize(serializer),
        SectionId::PluginsHost => PluginsHostView::from(&*state.plugins_host).serialize(serializer),
        SectionId::Config => ConfigView::from(&*state.config).serialize(serializer),
        SectionId::Skills => SkillsView::from(&*state.skills).serialize(serializer),
        SectionId::Recovery => RecoveryView::from(&*state.recovery).serialize(serializer),
        SectionId::Onboarding => OnboardingView::from(&*state.onboarding).serialize(serializer),
        SectionId::Shell => ShellView::from(&*state.shell).serialize(serializer),
    }
}

/// Serialise the value behind a shared cell the poller threads write through.
/// A poisoned lock still holds the last complete value, so it is read anyway.
// serde's `serialize_with` hands the view's `&&T`, so the double reference is its signature.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn locked<T: Serialize, S: Serializer>(cell: &&Mutex<T>, serializer: S) -> Result<S::Ok, S::Error> {
    let guard = cell.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    guard.serialize(serializer)
}

/// The Hangar fleet snapshot without `current_request`.
///
/// `FleetSession` is the daemon's protocol row, so its own `Serialize` must
/// keep the request for the RPC it comes from. That field is the complete tool
/// input of a pending approval (the command about to run, the file about to be
/// written), unbounded and shaped by the agent, so a frame carries the
/// fingerprint and never the request.
// serde's `serialize_with` hands the view's `&&T`, so the double reference is its signature.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn fleet_rows<S: Serializer>(
    cell: &&Mutex<Vec<ainb_hangar_proto::fleet::FleetSession>>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let rows: Vec<_> = cell
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .map(|row| ainb_hangar_proto::fleet::FleetSession {
            current_request: None,
            ..row.clone()
        })
        .collect();
    rows.serialize(serializer)
}

/// Plugin render failures, scrubbed: an error string can carry a URL or token.
// serde's `serialize_with` hands the view's `&&T`, so the double reference is its signature.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn scrubbed_values<K: Serialize + std::hash::Hash + Eq, S: Serializer>(
    map: &&std::collections::HashMap<K, String>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer
        .collect_map(map.iter().map(|(key, text)| (key, crate::fleet::bridge::redact::scrub(text))))
}

/// Subprocess error text, scrubbed.
// serde's `serialize_with` hands the view's `&&T`, so the double reference is its signature.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn scrubbed_opt<S: Serializer>(value: &&Option<String>, serializer: S) -> Result<S::Ok, S::Error> {
    fields::scrub_opt(value, serializer)
}

/// The quick-commit composer as its length: operator prose mid-typing.
// serde's `serialize_with` hands the view's `&&T`, so the double reference is its signature.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn opt_len<S: Serializer>(value: &&Option<String>, serializer: S) -> Result<S::Ok, S::Error> {
    fields::opt_char_count(value, serializer)
}

/// Declares a borrowed view over a section: the listed fields, by reference,
/// with optional per-field serde attributes. Anything not listed is not on the
/// wire.
macro_rules! view {
    ($view:ident<$lt:lifetime> for $section:ty { $( $(#[$attr:meta])* $field:ident : $ty:ty ),* $(,)? }) => {
        #[derive(Serialize)]
        // Field names mirror the section's, prefixes and all, so the frame
        // keys match the Rust fields a later derive would emit.
        #[allow(clippy::struct_field_names)]
        struct $view<$lt> {
            $( $(#[$attr])* $field: &$lt $ty, )*
        }

        impl<$lt> From<&$lt $section> for $view<$lt> {
            fn from(section: &$lt $section) -> Self {
                Self { $( $field: &section.$field, )* }
            }
        }
    };
}

view!(SessionsView<'a> for SessionsSection {
    workspaces: Vec<crate::models::Workspace>,
    selected_workspace_index: Option<usize>,
    selected_session_index: Option<usize>,
    shell_selected: bool,
    selected_sessions: std::collections::HashSet<uuid::Uuid>,
    expand_all_workspaces: bool,
    session_filter: crate::app::state::SessionFilter,
    attached_session_id: Option<uuid::Uuid>,
    favorite_workspace_paths: std::collections::HashSet<std::path::PathBuf>,
});

view!(SessionLabelsView<'a> for SessionLabelsSection {
    session_label_store: crate::config::SessionLabelStore,
    session_label_rename_mode: bool,
    session_label_rename_buffer: String,
    session_label_rename_target: Option<crate::app::state::AttachableRef>,
    session_context_menu: Option<crate::app::state::SessionContextMenu>,
});

view!(TmuxView<'a> for TmuxSection {
    embed_session: Option<String>,
    other_tmux_sessions: Vec<crate::models::OtherTmuxSession>,
    other_tmux_expanded: bool,
    selected_other_tmux_index: Option<usize>,
    selected_other_tmux_sessions: std::collections::HashSet<String>,
    other_tmux_rename_mode: bool,
    other_tmux_rename_buffer: String,
});

view!(SshView<'a> for SshSection {
    ssh_sessions: Vec<crate::models::Session>,
    ssh_sessions_expanded: bool,
    selected_ssh_session_index: Option<usize>,
    ssh_session_rename_mode: bool,
    ssh_session_rename_buffer: String,
});

view!(GitViewView<'a> for GitViewSection {
    git_view_state: Option<crate::components::GitViewState>,
    #[serde(rename = "quick_commit_message_len", serialize_with = "opt_len")]
    quick_commit_message: Option<String>,
    quick_commit_cursor: usize,
    is_current_dir_git_repo: bool,
});

view!(WorkspaceLoadView<'a> for WorkspaceLoadSection {
    is_loading_workspaces: bool,
    #[serde(serialize_with = "scrubbed_opt")]
    workspace_load_error: Option<String>,
});

view!(NewSessionView<'a> for NewSessionSection {
    new_session_state: Option<crate::app::state::NewSessionState>,
    branch_refresh_seq: u64,
    repo_check_seq: u64,
    repo_init_seq: u64,
});

view!(LogsView<'a> for LogsSection {
    live_logs: std::collections::HashMap<uuid::Uuid, Vec<crate::components::live_logs_stream::LogEntry>>,
    last_logs_session_id: Option<uuid::Uuid>,
    log_history_state: crate::components::LogHistoryViewerState,
});

view!(ClaudeChatView<'a> for ClaudeChatSection {
    claude_chat_visible: bool,
    claude_chat_state: Option<crate::app::state::ClaudeChatState>,
});

view!(FleetView<'a> for FleetSection {
    attention_baseline: std::collections::HashMap<uuid::Uuid, i64>,
    live_window: crate::models::live_window::LiveWindow,
    ask_state: crate::fleet::answer::AskState,
    broadcast: crate::fleet::broadcast::Broadcast,
    #[serde(serialize_with = "locked")]
    daemon_attention: Mutex<crate::fleet::attention::DaemonAttention>,
    #[serde(serialize_with = "fleet_rows")]
    fleet_snapshot: Mutex<Vec<ainb_hangar_proto::fleet::FleetSession>>,
    fleet_metadata: std::collections::HashMap<uuid::Uuid, crate::app::state::SessionFleetMetadata>,
    daemon_attention_seen: u64,
    attention_elsewhere: usize,
    attention_error_since: std::collections::HashMap<uuid::Uuid, i64>,
});

// `pending_daemon_config_edits` stays out: raw `(key, value)` edits queued by
// a keystroke and drained on the same app tick by `process_async_action`, so a
// host has nothing to draw from them.
view!(HangarView<'a> for HangarSection {
    hangar_daemon_config_loaded: bool,
    daemons_state: crate::components::daemons::DaemonsState,
});

view!(McpPoolView<'a> for McpPoolSection {
    mcp_overlay: Option<crate::app::state::McpOverlayState>,
});

#[derive(Serialize)]
struct InboxView {}

view!(PluginsHostView<'a> for PluginsHostSection {
    plugin_captures_text: std::collections::HashMap<crate::app::screens::ScreenId, bool>,
    #[serde(serialize_with = "scrubbed_values")]
    plugin_render_errors: std::collections::HashMap<crate::app::screens::ScreenId, String>,
    plugin_ui_states: std::collections::HashMap<String, PluginUiState>,
});

view!(ConfigView<'a> for ConfigSection {
    app_config: crate::config::AppConfig,
    config_screen_state: crate::app::state::ConfigScreenState,
    config_popup_state: crate::components::config_popup::ConfigPopupState,
    changelog_state: crate::components::ChangelogState,
    statusline_status: Option<crate::cli::statusline_install::StatuslineStatus>,
});

view!(SkillsView<'a> for SkillsSection {
    skills_state: crate::components::skills::SkillsViewState,
    skill_manager_state: crate::components::skill_manager_screen::SkillsScreenData,
});

view!(RecoveryView<'a> for RecoverySection {
    session_recovery_state: crate::components::SessionRecoveryState,
});

view!(OnboardingView<'a> for OnboardingSection {
    onboarding_state: Option<crate::components::onboarding::OnboardingState>,
    setup_menu_state: crate::components::setup_menu::SetupMenuState,
    auth_setup_state: Option<crate::app::state::AuthSetupState>,
    auth_provider_popup_state: crate::app::state::AuthProviderPopupState,
});

view!(ShellView<'a> for ShellSection {
    current_screen: crate::app::screens::ScreenId,
    previous_screen: Option<crate::app::screens::ScreenId>,
    should_quit: bool,
    help_visible: bool,
    ui_needs_refresh: bool,
    home_screen_state: crate::app::state::HomeScreenState,
    home_screen_v2_state: crate::components::home_screen_v2::HomeScreenV2State,
    notifications: Vec<crate::app::state::Notification>,
    confirmation_dialog: Option<crate::app::state::ConfirmationDialog>,
    async_operation_cancelled: bool,
    last_panel_close_version: Option<u64>,
    session_tab: crate::components::session_tabs::SessionTab,
    focused_pane: crate::app::state::FocusedPane,
});

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::ConfigValue;
    use crate::components::config_popup::ConfigPopupType;

    /// Build under a scratch `HOME`, holding the crate's env lock and putting
    /// the previous value back, the same way the reducer tests do.
    fn with_scratch_home<T>(body: impl FnOnce() -> T) -> T {
        let _guard = crate::config::tunables::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = tempfile::tempdir().expect("scratch home");
        let previous = std::env::var_os("HOME");
        std::env::set_var("HOME", dir.path());
        let out = body();
        match previous {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        out
    }

    #[test]
    fn enter_on_an_env_row_opens_a_popup_that_withholds_the_value() {
        with_scratch_home(|| {
            let mut state = shape::sample_state(&mut shape::PlainSeed);
            let screen = &mut state.config.get_mut().config_screen_state;
            let (category, index) = screen
                .settings
                .iter()
                .find_map(|(category, rows)| {
                    rows.iter()
                        .position(|row| row.key.ends_with(".environment.ANTHROPIC_API_KEY"))
                        .map(|index| (*category, index))
                })
                .expect("the sample template has an env row");
            if let ConfigValue::Text(text) =
                &mut screen.settings.get_mut(&category).unwrap()[index].value
            {
                *text = "env-value-marker".to_string();
            }
            screen.visible_rows = vec![(category, index)];
            screen.selected_setting = 0;
            crate::app::EventHandler::process_event(
                crate::app::AppEvent::ConfigEditSetting,
                &mut state,
            );

            assert!(matches!(
                state.config.config_popup_state.popup_type,
                ConfigPopupType::SecretInput { .. }
            ));
            let frame = section_json(&state, SectionId::Config).to_string();
            assert!(!frame.contains("env-value-marker"), "{frame}");
        });
    }

    #[test]
    fn a_failed_answer_does_not_carry_the_typed_draft() {
        let frame = with_scratch_home(|| {
            let state = shape::sample_state(&mut shape::PlainSeed);
            section_json(&state, SectionId::Fleet).to_string()
        });
        assert!(frame.contains("draft_len"), "{frame}");
        assert!(!frame.contains("typed answer"), "{frame}");
    }
}
