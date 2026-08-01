// ABOUTME: Session discovery sources for fleet orchestration.

pub mod ainb;
pub mod jobs;
pub mod merge;
pub mod peers;
pub mod process_tree;
pub mod tmux;

pub use ainb::discover_from_ainb;
pub use jobs::discover_from_jobs;
pub use merge::{merge_fleet_sessions, merge_sessions};
pub use peers::{discover_from_peers, list_broker_peers};
pub use process_tree::ProcessTable;
pub use tmux::{discover_all_tmux_panes, discover_from_tmux};
