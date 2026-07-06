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

pub use board_mouse::{BoardMouseIntent, fold_board_mouse};
pub use chrome::{Presence, render_footer, render_top_bar};
pub use connection::{ConnState, Connection};
pub use firstrun::{FirstRunIntent, FirstRunModal, FirstRunReduction, reduce_first_run};
pub use mouse::{HitMap, MouseFsm, MouseIntent, MouseState, Rect, Target};
pub use plugin::{HangarPlugin, MANIFEST_TOML};
pub use screen::autopilots::{
    AutopilotsEvent, AutopilotsIntent, AutopilotsReduction, AutopilotsState, reduce_autopilots,
};
pub use screen::boards::{
    AgentChip, BoardView, BoardsEvent, BoardsIntent, BoardsKey, BoardsOverlay, BoardsReduction,
    BoardsState, BoardsStatus, CardView, ColumnView, RepoOption, reduce_boards, render_boards,
};
pub use screen::issue_list::{
    FilterChip, IssueColumn, IssueListEvent, IssueListIntent, IssueListMode, IssueListReduction,
    IssueListState, reduce_issue_list,
};
pub use screen::kanban::{
    BoardColumn, CardSummary, Column, KanbanEvent, KanbanIntent, KanbanReduction, KanbanState,
    reduce_kanban,
};
pub use screen::{ActiveTaskBanner, AppEvent, AppState, Intent, Reduction, Screen, reduce};
pub use shell::{
    DaemonStarter, FailingDaemonStarter, Opener, RecordingDaemonStarter, RecordingOpener,
    SystemDaemonStarter, SystemOpener, default_daemon_starter, default_opener,
};
pub use stream::{Backoff, StreamClient, StreamError, SubscribeReplay};
