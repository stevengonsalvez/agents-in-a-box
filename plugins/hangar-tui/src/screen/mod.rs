//! P4.1 — Screen routing core: the pure render-state reducer.
//!
//! Hangar's TUI is a small state machine. [`AppState`] holds *which* of the
//! Core 5 screens is active, the optional active-task banner, and the
//! workspace it is bound to. [`reduce`] maps a `(&AppState, AppEvent)` pair to
//! a new [`AppState`] plus an optional [`Intent`] — it is **pure**: no IO, no
//! `tokio`, no socket. The plugin glue ([`crate::plugin`]) feeds host key /
//! event deliveries in and acts on the emitted intents (quit, open a task,
//! issue an RPC). Keeping the routing logic IO-free makes every transition
//! exhaustively unit-testable, which is exactly what the P4.1 RED tests in
//! `tests/screen_router_test.rs` exercise.
//!
//! The shared chrome (top tab bar + footer) that wraps every screen lives in
//! [`crate::chrome`]; this module owns only the routing state it renders from.

pub mod agent_picker;
pub mod app_screens;
pub mod autopilots;
pub mod banner_state;
pub mod issue_list;
pub mod kanban;
mod router;
pub mod settings;
pub mod skill_manager;
pub mod task_detail;

pub use app_screens::{
    render_body, route_key, AutopilotAction, KanbanAction, NavIntent, ScreenStates, SkillAction,
    WorkspaceAction,
};
pub use router::reduce;

use ainb_hangar_core::ids::{IssueId, TaskId, WorkspaceId};

/// One of the Core 5 keyboard-driven screens.
///
/// `TaskDetail` and `AgentPicker` carry the entity they were opened for so the
/// router never has to reach back into a separate "selection" field to render
/// them. `AgentPicker` is a modal overlay drawn on top of whatever screen
/// opened it (tracked by [`AppState::prior_screen`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Screen {
    /// Issue list (hotkey `1`) — the default landing screen.
    IssueList,
    /// Task detail + transcript for a specific task (hotkey `2`, or `enter`
    /// on a list row).
    TaskDetail(TaskId),
    /// Agent-picker modal overlay opened for a specific issue (hotkey `a`).
    AgentPicker(IssueId),
    /// Skill manager (hotkey `4`).
    SkillManager,
    /// Autopilot manager (hotkey `5`).
    Autopilots,
    /// Kanban board (hotkey `K`) — the task queue laid out as four columns.
    Kanban,
    /// Settings (hotkey `,`).
    Settings,
    /// Help overlay (hotkey `?`) — a modal listing global + screen-local
    /// hotkeys, drawn over whatever screen opened it; Esc restores that screen.
    Help,
}

impl Screen {
    /// The tab-bar label rendered for this screen's tab.
    #[must_use]
    pub const fn tab_label(&self) -> &'static str {
        match self {
            Self::IssueList => "Issues",
            Self::TaskDetail(_) => "Task",
            Self::AgentPicker(_) => "Agent",
            Self::SkillManager => "Skills",
            Self::Autopilots => "Autopilots",
            Self::Kanban => "Kanban",
            Self::Settings => "Settings",
            Self::Help => "Help",
        }
    }

    /// `true` when this screen is a modal overlay drawn over a prior screen
    /// (only the agent picker, for now). Esc closes a modal back to its
    /// prior screen rather than quitting.
    #[must_use]
    pub const fn is_modal(&self) -> bool {
        matches!(self, Self::AgentPicker(_) | Self::Help)
    }
}

/// Snapshot describing the active task for the frosted banner.
///
/// P4.1 only routes this through state; the banner *widget* (and its 1Hz
/// elapsed tick) lands in P4.8. Holding the type here lets the chrome reserve
/// the banner region and lets cross-screen tests assert the banner survives a
/// tab switch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveTaskBanner {
    /// The running task this banner reflects.
    pub task_id: TaskId,
    /// Display name of the agent doing the work (e.g. `claude-agent`).
    pub agent_label: String,
    /// Elapsed wall-time in whole seconds since the task started.
    pub elapsed_secs: u64,
    /// Count of tool calls observed so far.
    pub tool_calls: u32,
}

/// The full render state the chrome + active screen paint from.
///
/// Pure data — cloned on every [`reduce`] so transitions stay value-oriented
/// and trivially comparable in tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppState {
    /// The currently active (or overlaid) screen.
    pub screen: Screen,
    /// When a modal is open, the screen to restore on Esc.
    pub prior_screen: Option<Screen>,
    /// The active-task banner, when a task is running for this workspace.
    pub banner: Option<ActiveTaskBanner>,
    /// The workspace this state is bound to (drives the chrome slug).
    pub ws_id: WorkspaceId,
    /// The task currently selected on the issue list, if any. Hotkey `2`
    /// only routes to [`Screen::TaskDetail`] when this is `Some`.
    pub selected_task: Option<TaskId>,
}

impl AppState {
    /// A fresh state landing on the issue list for `ws_id`, no banner, no
    /// selection.
    #[must_use]
    pub const fn new(ws_id: WorkspaceId) -> Self {
        Self {
            screen: Screen::IssueList,
            prior_screen: None,
            banner: None,
            ws_id,
            selected_task: None,
        }
    }
}

/// An input or host event the [`reduce`] function folds into [`AppState`].
///
/// Key presses arrive as single `char`s ([`AppEvent::Key`]); `Esc` is modelled
/// separately because it is not a printable char. Domain triggers that aren't a
/// bare keystroke (opening the agent picker for a specific issue) get their own
/// variants so the reducer stays total over its inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppEvent {
    /// A printable key was pressed (`'1'`, `'q'`, `','`, …).
    Key(char),
    /// The Escape key was pressed (closes modals, aborts input modes).
    Esc,
    /// Open the agent-picker modal for a specific issue (raised by `a` on a
    /// selected issue-list row; carries the issue id the row addresses).
    OpenAgentPicker(IssueId),
}

/// A side-effect the plugin glue must perform after a [`reduce`].
///
/// The reducer is pure, so it can't quit the process or fire an RPC itself; it
/// surfaces the *desire* to do so as an [`Intent`] and lets the IO layer carry
/// it out. P4.1 only needs [`Intent::Quit`]; later screens add open-task,
/// assign, and cancel intents alongside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    /// The user asked to quit the TUI (`q`).
    Quit,
}

/// The result of folding one [`AppEvent`] into an [`AppState`].
///
/// Carries the next state plus an optional [`Intent`] for the IO layer. A
/// no-op event (an invalid hotkey for the current screen) yields the input
/// state unchanged and `intent: None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reduction {
    /// The next application state.
    pub state: AppState,
    /// A side-effect for the plugin glue to perform, if any.
    pub intent: Option<Intent>,
}
