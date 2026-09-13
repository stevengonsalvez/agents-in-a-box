// ABOUTME: Terminal-side fleet pieces. The fleet service layer lives in
// `ainb-app`; this module re-exports it and adds the two modules that still
// depend on screen components (the daemon start offer and the session log tab).

pub use ainb_app::fleet::*;

pub mod daemon_cta;
pub mod session_log;
