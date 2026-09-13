// ABOUTME: Renderer-agnostic halves of the screen components: the state types,
// their impls and the reducers that do not draw. The draw functions stay in
// `ainb-core::components`, which re-exports this module.

pub mod changelog;
pub mod code_review;
pub mod config_popup;
pub mod daemons;
pub mod git_view;
pub mod home_screen_v2;
pub mod live_logs_stream;
pub mod log_history_viewer;
pub mod log_parser;
pub mod log_reader;
pub mod log_writer;
pub mod mascot;
pub mod new_session;
pub mod onboarding;
pub mod session_recovery;
pub mod session_tabs;
pub mod setup_menu;
pub mod sidebar;
pub mod skill_manager_screen;
pub mod skills;
pub mod welcome_panel;

pub use changelog::ChangelogState;
pub use git_view::GitViewState;
pub use log_history_viewer::LogHistoryViewerState;
pub use session_recovery::SessionRecoveryState;
