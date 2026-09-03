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

pub mod activity;
pub mod agent_picker;
pub mod agents;
pub mod app_screens;
pub mod autopilots;
pub mod banner_state;
pub mod boards;
pub mod command_palette;
pub mod context_menu;
pub mod control_center;
pub mod daemon_health;
pub mod fleet;
pub mod fleet_chat;
pub mod inbox;
pub mod issue_list;
pub mod kanban;
pub mod list_context_menu;
pub mod logs;
pub mod profiles;
pub mod router;
pub mod settings;
pub mod skill_manager;
pub mod squads;
pub mod task_detail;
pub mod usage_dashboard;

pub use app_screens::{
    AgentsAction, AttentionAnswerAction, AutopilotAction, BoardsAction, IssueAssignAction,
    IssueCommentAction, IssueCreateAction, IssueCriterionAction, KanbanAction, NavIntent,
    PaletteAction, ScreenStates, SkillAction, SquadAction, WorkspaceAction, render_body, route_key,
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
    /// Activity-timeline modal overlay opened for a specific issue (hotkey `y`,
    /// multica parity #13) — the card's merged activity + comment narrative.
    ActivityTimeline(IssueId),
    /// Skill manager (hotkey `3`).
    SkillManager,
    /// Autopilot manager (hotkey `4`).
    Autopilots,
    /// Kanban board (hotkey `K`) — the task queue laid out as four columns.
    Kanban,
    /// User-defined boards (hotkey `B`) — custom columns with FSM-state mapping +
    /// auto-move (P4 / D8). Additive alongside the fixed [`Screen::Kanban`].
    Boards,
    /// Daemon health (hotkey `D`) — runtimes, claim cache, concurrency, and the
    /// dual-dim throughput sparkline (P8.5).
    DaemonHealth,
    /// Usage dashboard (hotkey `U`) — total token/cost + a per-agent rollup,
    /// backed by `hangar/usage_rollup` (e38.35).
    Usage,
    /// Logs tail (hotkey `L`) — a read-only, level-filterable view over the
    /// daemon's structured JSON log file (P8.6).
    Logs,
    /// Notification inbox (hotkey `I`) — the aggregated issue/comment/task
    /// events with an unread badge, backed by `hangar/inbox_list` (e38.14).
    Inbox,
    /// Control center (hotkey `C`) — the fleet-wide agentpeek "who-needs-you"
    /// board, backed by the P2 attention feed (`attention/list` +
    /// `attention/subscribe`) with inline answering via `attention/answer`.
    ControlCenter,
    /// Fleet super-control pane (hotkey `F`), backed by authoritative registry
    /// snapshots and revisioned live updates.
    Fleet,
    /// Squads (hotkey `S`) — the daemon-native team primitive (D17): squads with
    /// a leader + members, live per-member status, and issue-assign leader-routing
    /// dispatch, backed by `hangar/squads_list` + the squad mutation RPCs (P7).
    Squads,
    /// Profile editor (hotkey `P`) — the agent-profile roster + a live preview of
    /// BOTH compile targets (lossless Claude `.md`, lossy Codex fragment/prompt),
    /// backed by `profile/list` + `profile/get`, with tier editing via
    /// `profile/upsert` (P5).
    Profiles,
    /// Agents roster (hotkey `A`, slice 2) — the first-class list of the
    /// workspace's named agents with inline create (`n` → `hangar/agent_create`) +
    /// delete (`x` → `hangar/agent_delete`), backed by the cached `hangar/agents_list`
    /// snapshot (no new list RPC).
    Agents,
    /// Settings (hotkey `,`).
    Settings,
    /// Help overlay (hotkey `?`) — a modal listing global + screen-local
    /// hotkeys, drawn over whatever screen opened it; Esc restores that screen.
    Help,
    /// Command palette (hotkey `Ctrl+P`) — a modal global cross-entity search
    /// drawn over whatever screen opened it; Enter jumps to the selected entity,
    /// Esc restores the prior screen (e38.13).
    CommandPalette,
}

impl Screen {
    /// The tab-bar label rendered for this screen's tab.
    #[must_use]
    pub const fn tab_label(&self) -> &'static str {
        match self {
            Self::IssueList => "Issues",
            Self::TaskDetail(_) => "Task",
            Self::AgentPicker(_) => "Agent",
            Self::ActivityTimeline(_) => "Activity",
            Self::SkillManager => "Skills",
            Self::Autopilots => "Autopilots",
            Self::Kanban => "Kanban",
            Self::Boards => "Boards",
            Self::DaemonHealth => "Daemon",
            Self::Usage => "Usage",
            Self::Logs => "Logs",
            Self::Inbox => "Inbox",
            Self::ControlCenter => "Control",
            Self::Fleet => "Fleet",
            Self::Squads => "Squads",
            Self::Profiles => "Profiles",
            Self::Agents => "Agents",
            Self::Settings => "Settings",
            Self::Help => "Help",
            Self::CommandPalette => "Search",
        }
    }

    /// `true` when this screen is a modal overlay drawn over a prior screen
    /// (the agent picker, the help overlay, and the command palette). Esc closes
    /// a modal back to its prior screen rather than quitting.
    #[must_use]
    pub const fn is_modal(&self) -> bool {
        matches!(
            self,
            Self::AgentPicker(_) | Self::ActivityTimeline(_) | Self::Help | Self::CommandPalette
        )
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
    /// Open the activity-timeline modal for a specific issue (raised by `y` on a
    /// selected issue-list row, multica parity #13).
    OpenActivityTimeline(IssueId),
    /// Open the global command-palette modal (raised by `Ctrl+P` from any
    /// screen, e38.13). Carries no payload — the palette starts empty.
    OpenCommandPalette,
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

/// The label an assignee slot paints: the roster display name once `resolved`
/// carries one, else the actor ref with a ULID id cut to its short form.
/// `None` when there is no assignee at all (the caller words its own blank).
///
/// Shared by the three surfaces that paint an assignee (board card footer,
/// task-detail header, issue sidebar) so they degrade the same way: two of them
/// fell back to a whole 26-char ULID while the cards fell back to a bare short id
/// (crisp B1 review).
///
/// Only a ULID is unreadable, so only a ULID is cut. A human ref
/// (`member:dana`, `agent:claude-agent`) is already its own label and survives
/// whole, KIND prefix included: that prefix is what tells a human assignee apart
/// from an agent one, and shortening it blindly turned `agent:claude-agent` into
/// `e-agent`.
#[must_use]
pub fn assignee_label(resolved: Option<&str>, actor_ref: Option<&str>) -> Option<String> {
    if let Some(name) = resolved {
        return Some(name.to_string());
    }
    let actor_ref = actor_ref?;
    let Some((kind, id)) = actor_ref.split_once(':') else {
        return Some(shorten_ulid(actor_ref));
    };
    Some(format!("{kind}:{}", shorten_ulid(id)))
}

/// [`assignee_label`] WITHOUT the actor kind, for a surface too narrow to spend
/// the prefix: a board card footer is ~21 cells, where `agent:` pushes the name
/// itself off the tile.
///
/// Same shortening rule, so the callers cannot drift; the kind is dropped here
/// rather than by the caller pre-splitting the ref, which left the caller owning
/// half the rule (crisp B1 round-2 review). A RESOLVED display name is returned
/// verbatim, never split on a colon it happens to contain.
#[must_use]
pub fn assignee_label_bare(resolved: Option<&str>, actor_ref: Option<&str>) -> Option<String> {
    let label = assignee_label(resolved, actor_ref)?;
    if resolved.is_some() {
        return Some(label);
    }
    Some(match label.split_once(':') {
        Some((_, name)) => name.to_string(),
        None => label,
    })
}

/// `id` cut to its last-6 short form when it is a ULID, else `id` unchanged.
fn shorten_ulid(id: &str) -> String {
    if kanban::is_ulid(id) {
        kanban::short_id(id)
    } else {
        id.to_string()
    }
}

/// Whether a run's terminal `outcome` token is a FAILURE, the one row an
/// operator opens a list to find. The daemon writes exactly three tokens
/// (`success` / `failed` / `cancelled`, `run_loop::record_run_history`), so
/// "not success" is NOT the same rule: it floats a user's own cancel, and a
/// running row whose outcome has not landed, up with the real failures.
///
/// Shared so the inbox and the usage dashboard cannot drift apart on what
/// "failed" means (crisp B1 review): both float the same rows first.
#[must_use]
pub fn is_failed_outcome(outcome: &str) -> bool {
    outcome == "failed"
}

/// `ch` as it is safe to hand a terminal, or `·` when it is not.
///
/// Every rendered char becomes a `Cell` symbol the host paints verbatim, so a
/// screen that shows session-originated free text (assistant replies, error
/// snippets, ASK option labels, issue titles) must strip anything that acts on
/// the terminal or on the reader rather than printing:
///
/// - **C0 / DEL / C1** (`char::is_control`): a raw ESC or BEL reassembles on
///   flush into a live control sequence — an OSC 52 clipboard write, a title
///   set, a cursor jump — in the operator's own terminal.
/// - **Bidi overrides and isolates** (`U+202A`-`U+202E`, `U+2066`-`U+2069`) and
///   the invisible formatters around them (`U+200B`-`U+200F`, `U+2060`-`U+2064`,
///   `U+FEFF`): these do not execute, they REORDER. On the one surface whose job
///   is "pick the option you read", a crafted ASK label can render as a
///   different string than the bytes that get delivered as the answer.
///
/// Surfaced as a visible middot, never dropped: a label that silently loses
/// characters is its own kind of lie.
#[must_use]
pub fn display_char(ch: char) -> char {
    if ch.is_control()
        || matches!(ch,
            '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}')
    {
        '·'
    } else {
        ch
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A 26-char ULID, the shape every id on the wire has.
    const ULID: &str = "01M1FHM2YSRSXZQFR29ZAYF56V";

    /// The BARE label (the card-footer variant) over all four assignee shapes.
    ///
    /// Three surfaces share this helper and each one is ~21 cells wide, so every
    /// rule it encodes is load-bearing: cut a ULID because nobody can read one,
    /// keep a human ref whole because it is already its own label, and drop the
    /// actor KIND because `agent:` alone is a third of a card footer.
    ///
    /// Pinned directly rather than through a caller (crisp B1 review): the three
    /// callers each render it through a whole board, so a regression here used to
    /// surface as an unrelated snapshot diff.
    #[test]
    fn assignee_label_bare_over_every_assignee_shape() {
        for (actor_ref, expected) in [
            // An agent ref: kind dropped, ULID cut to its last six.
            (format!("agent:{ULID}"), "AYF56V"),
            // A bare ULID with no kind at all: still cut.
            (ULID.to_string(), "AYF56V"),
            // A human ref: readable already, so it survives whole minus the kind.
            ("member:dana".to_string(), "dana"),
            // A ref whose id itself contains a colon: only the KIND is dropped,
            // never the rest of the ref.
            ("member:dana:x".to_string(), "dana:x"),
        ] {
            assert_eq!(
                assignee_label_bare(None, Some(&actor_ref)).as_deref(),
                Some(expected),
                "bare label for {actor_ref:?}"
            );
        }
        // No assignee at all is `None`, not an empty string the caller would paint
        // a stray glyph beside.
        assert_eq!(assignee_label_bare(None, None), None);
    }

    /// A RESOLVED display name is returned verbatim — including one that happens
    /// to contain a colon, which the kind-stripping rule must not cut.
    ///
    /// `ops: dana` is a legal roster display name; splitting it would render the
    /// agent as ` dana` and lose the team it belongs to.
    #[test]
    fn a_resolved_name_survives_its_own_colon() {
        assert_eq!(
            assignee_label_bare(Some("ops: dana"), Some(&format!("agent:{ULID}"))).as_deref(),
            Some("ops: dana")
        );
        // And the WIDE variant agrees: a resolved name is the label on every
        // surface, kind prefix or not.
        assert_eq!(
            assignee_label(Some("ops: dana"), Some("member:dana")).as_deref(),
            Some("ops: dana")
        );
    }

    /// The WIDE label keeps the actor kind — that prefix is what tells a human
    /// assignee from an agent one on a row with room for it.
    #[test]
    fn assignee_label_keeps_the_kind_it_can_afford() {
        assert_eq!(
            assignee_label(None, Some(&format!("agent:{ULID}"))).as_deref(),
            Some("agent:AYF56V")
        );
        assert_eq!(
            assignee_label(None, Some("agent:claude-agent")).as_deref(),
            Some("agent:claude-agent"),
            "a named agent is not shortened into nonsense"
        );
    }

    /// `failed` is the one outcome the run lists float first — NOT "anything that
    /// is not success", which would float a user's own cancel and a still-running
    /// row in with the real failures.
    #[test]
    fn only_failed_is_a_failed_outcome() {
        assert!(is_failed_outcome("failed"));
        for other in ["success", "cancelled", "running", "", "FAILED"] {
            assert!(!is_failed_outcome(other), "{other:?} is not a failure");
        }
    }
}
