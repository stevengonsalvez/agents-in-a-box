//! ainb Hangar plugin — TUI-first managed-agents control plane.
//!
//! P3.6 scaffold: a subprocess plugin built against
//! `ainb-plugin-sdk-rust`. `src/main.rs` runs
//! `Server::new(HangarPlugin::default()).run_stdio()`; the [`plugin`]
//! module wires the (currently stub) [`HangarPlugin`] onto the SDK's
//! `Plugin` trait. The connection state machine + daemon JSON-RPC
//! client land in P3.7.

pub mod board_mouse;
pub mod chrome;
pub mod connection;
pub mod firstrun;
pub mod jsonrpc_over_socket;
pub mod mouse;
pub mod plugin;
pub mod screen;
pub mod shell;
pub mod stream;
pub mod widgets;

pub use board_mouse::{fold_board_mouse, BoardMouseIntent};
pub use chrome::{render_footer, render_top_bar, Presence};
pub use connection::{ConnState, Connection};
pub use firstrun::{reduce_first_run, FirstRunIntent, FirstRunModal, FirstRunReduction};
pub use mouse::{HitMap, MouseFsm, MouseIntent, MouseState, Rect, Target};
pub use plugin::{HangarPlugin, MANIFEST_TOML};
pub use screen::autopilots::{
    reduce_autopilots, AutopilotsEvent, AutopilotsIntent, AutopilotsReduction, AutopilotsState,
};
pub use screen::issue_list::{
    reduce_issue_list, FilterChip, IssueColumn, IssueListEvent, IssueListIntent, IssueListMode,
    IssueListReduction, IssueListState,
};
pub use screen::boards::{
    reduce_boards, render_boards, AgentChip, BoardView, BoardsEvent, BoardsIntent, BoardsKey,
    BoardsOverlay, BoardsReduction, BoardsState, BoardsStatus, CardView, ColumnView, RepoOption,
};
pub use screen::kanban::{
    reduce_kanban, BoardColumn, CardSummary, Column, KanbanEvent, KanbanIntent, KanbanReduction,
    KanbanState,
};
pub use screen::{reduce, ActiveTaskBanner, AppEvent, AppState, Intent, Reduction, Screen};
pub use shell::{
    default_daemon_starter, default_opener, DaemonStarter, FailingDaemonStarter, Opener,
    RecordingDaemonStarter, RecordingOpener, SystemDaemonStarter, SystemOpener,
};
pub use stream::{Backoff, StreamClient, StreamError, SubscribeReplay};
