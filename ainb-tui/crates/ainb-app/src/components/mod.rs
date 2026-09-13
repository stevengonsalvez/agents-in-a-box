// ABOUTME: Renderer-agnostic halves of the screen components: the state types,
// their impls and the reducers that do not draw. The draw functions stay in
// `ainb-core::components`, which re-exports this module.

pub mod config_popup;
pub mod daemons;
pub mod live_logs_stream;
pub mod log_parser;
pub mod log_reader;
pub mod log_writer;
pub mod onboarding;
pub mod session_tabs;
pub mod setup_menu;
pub mod skill_manager_screen;
pub mod skills;
