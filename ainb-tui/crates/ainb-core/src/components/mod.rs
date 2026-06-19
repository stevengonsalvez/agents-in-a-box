// ABOUTME: UI components for the TUI interface including session list, logs viewer, and help

pub mod action_card;
pub mod agent_selection;
pub mod attached_terminal;
pub mod auth_provider_popup;
pub mod auth_setup;
pub mod changelog;
pub mod claude_chat;
pub mod code_review;
pub mod config_popup;
pub mod config_screen;
pub mod confirmation_dialog;
pub mod daemons;
pub mod fleet_panel;
pub mod fuzzy_file_finder;
pub mod git_view;
pub mod help;
pub mod home_screen;
pub mod home_screen_v2;
pub mod inbox;
pub mod layout;
pub mod live_logs_stream;
// pub mod log_formatter;  // Complex version with borrow issues, using simple version instead
pub mod log_formatter_simple;
pub mod log_history_viewer;
pub mod log_parser;
pub mod log_reader;
pub mod log_writer;
pub mod logs_viewer;
pub mod mascot;
pub mod new_session;
pub mod onboarding;
pub mod session_list;
pub mod session_recovery;
pub mod setup_menu;
pub mod sidebar;
pub mod skills;
pub mod slash;
pub mod tmux_preview;
// `usage` removed in Phase 3 cutover — the burndown plugin owns the
// Analytics screen UI now. See crates/ainb-plugin-burndown/src/ui.rs.
pub mod welcome_panel;

pub use action_card::{ActionCard, ActionCardGridState, ActionCardId};
pub use agent_selection::AgentSelectionComponent;
pub use attached_terminal::AttachedTerminalComponent;
pub use auth_provider_popup::AuthProviderPopupComponent;
pub use auth_setup::AuthSetupComponent;
pub use changelog::{ChangelogComponent, ChangelogState};
pub use claude_chat::ClaudeChatComponent;
pub use config_popup::{ConfigPopupComponent, ConfigPopupState, ConfigPopupType, ConfigPopupValue};
pub use config_screen::ConfigScreenComponent;
pub use confirmation_dialog::ConfirmationDialogComponent;
pub use git_view::{GitViewComponent, GitViewState};
pub use help::HelpComponent;
pub use home_screen::HomeScreenComponent;
pub use home_screen_v2::{HomeScreenFocus, HomeScreenV2Component, HomeScreenV2State, LayoutMode};
pub use layout::LayoutComponent;
pub use live_logs_stream::{LiveLogsStreamComponent, LogEntry, LogEntryLevel};
pub use log_history_viewer::{LogHistoryViewerComponent, LogHistoryViewerState, SessionLogSummary};
pub use log_reader::{AppLogInfo, JsonlLogReader};
pub use log_writer::{JsonlLogEntry, JsonlLogWriter};
pub use logs_viewer::LogsViewerComponent;
pub use mascot::{MascotAnimation, render_mascot, render_mascot_centered};
pub use new_session::NewSessionComponent;
pub use onboarding::{OnboardingComponent, OnboardingState, OnboardingStep};
pub use session_list::SessionListComponent;
pub use session_recovery::{
    OrphanType, OrphanedSession, OrphanedWorktree, RecoveryViewMode, SessionRecovery,
    SessionRecoveryState,
};
pub use setup_menu::{SetupMenuComponent, SetupMenuItem, SetupMenuState};
pub use sidebar::{SidebarComponent, SidebarItem, SidebarState};
#[allow(unused_imports)]
pub use tmux_preview::{PreviewMode, TmuxPreviewPane};
pub use welcome_panel::{WelcomePanelComponent, WelcomePanelState};
