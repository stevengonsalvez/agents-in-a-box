//! The plugin's render-state cache + render/key dispatch over the Core 5 screens
//! (P4.10 connecting tissue).
//!
//! P4.1–P4.8 landed five **pure** screen reducers + width-aware renderers, but
//! `plugin::render` painted only the shared chrome and `handle_key` never reached
//! a screen reducer. This module is the glue: [`ScreenStates`] caches each
//! screen's render state (filled from the daemon snapshot RPCs), [`render_body`]
//! dispatches the active [`Screen`] to its renderer over the chrome body rows,
//! and [`route_key`] folds a forwarded key into the active screen's reducer and
//! surfaces any cross-screen [`NavIntent`] back to the plugin glue (open a task,
//! open the picker, switch tabs).
//!
//! The plugin still owns **zero domain data** — every cache here is filled from a
//! daemon snapshot and folded by the daemon's event stream.

use ainb_hangar_proto::events::{ActorRow, AutopilotRow, IssueRow, SkillRow, TaskCardRow};
use ainb_hangar_proto::settings::{
    DaemonHealthSnapshot, HealthSnapshot, KeyRow, ProviderRow, WorkspaceRow,
};
use ainb_hangar_proto::snapshots::{MemberWireRow, RunHistoryResult, UsageRollupResult};
use ainb_plugin_sdk::{KeyCode, KeyEvent, WireBuffer};

use super::agent_picker::{
    AgentPickerEvent, AgentPickerIntent, AgentPickerState, reduce_agent_picker,
};
use super::autopilots::{AutopilotsEvent, AutopilotsIntent, AutopilotsState, reduce_autopilots};
use super::boards::{BoardsEvent, BoardsIntent, BoardsKey, BoardsState, reduce_boards};
use super::command_palette::{
    CommandPaletteEvent, CommandPaletteIntent, CommandPaletteState, reduce_command_palette,
};
use super::control_center::{
    ControlCenterEvent, ControlCenterIntent, ControlCenterState, reduce_control_center,
};
use super::daemon_health::DaemonHealthState;
use super::inbox::InboxState;
use super::issue_list::{IssueListEvent, IssueListIntent, IssueListState, reduce_issue_list};
use super::kanban::{KanbanEvent, KanbanIntent, KanbanState, reduce_kanban};
use super::logs::LogsState;
use super::profiles::{ProfilesEvent, ProfilesIntent, ProfilesState, reduce_profiles};
use super::settings::{SettingsEvent, SettingsIntent, SettingsState, reduce_settings};
use super::skill_manager::{
    SkillManagerEvent, SkillManagerIntent, SkillManagerState, reduce_skill_manager,
};
use super::squads::{SquadsEvent, SquadsIntent, SquadsState, reduce_squads};
use super::task_detail::{TaskDetailEvent, TaskDetailIntent, TaskDetailState, reduce_task_detail};
use super::usage_dashboard::UsageState;
use super::{AppState, Screen};

/// A deferred host-cap action raised by the Settings Workspace pane (P5.5).
///
/// The sync key router can't `await` a host call, so it stashes the action on
/// [`ScreenStates::pending_ws_action`]; the plugin's async key handler drains
/// it and calls the matching `host/workspace_*` cap, then re-fetches the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceAction {
    /// Fetch the host workspace list to seed the pane — raised when the user
    /// navigates into the Workspace section (`host/workspace_list`).
    Refresh,
    /// Set the workspace active (`s`) — `host/workspace_set_active`.
    SetActive(String),
    /// Toggle the workspace default (`d`) — `host/workspace_set_default`.
    SetDefault(String),
}

/// A deferred daemon RPC raised by the skill-manager screen (P6.5).
///
/// Like [`WorkspaceAction`], the sync key router can't `await`; it stashes the
/// action on [`ScreenStates::pending_skill_action`] and the plugin's `render`
/// pass drains it and fires the matching daemon JSON-RPC over the socket cap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillAction {
    /// Run the curated-skills importer (`s`) — `hangar/skills_sync`.
    Sync,
    /// Fetch a skill's detail body + files (Enter) — `hangar/skill_get`.
    LoadDetail(String),
    /// Attach a skill to the selected agent (`i`) — `hangar/skill_attach`.
    Attach(String),
    /// Detach a skill from the selected agent (`d`) — `hangar/skill_detach`.
    Detach(String),
}

/// A deferred daemon RPC raised by the autopilot-manager screen (P7.5).
///
/// Like [`SkillAction`], the sync key router can't `await`; it stashes the action
/// on [`ScreenStates::pending_autopilot_action`] and the plugin's `render` pass
/// drains it and fires the matching daemon JSON-RPC over the socket cap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutopilotAction {
    /// Load an autopilot's run history (selection change / run event) —
    /// `hangar/autopilot_runs`.
    LoadRuns(String),
    /// Fire the selected autopilot now (`r`) — `hangar/autopilot_fire_now`.
    FireNow(String),
    /// Toggle the selected autopilot's enabled flag (`d`) —
    /// `hangar/autopilot_set_enabled`.
    SetEnabled {
        /// The autopilot to toggle.
        autopilot_id: String,
        /// The target enabled state.
        enabled: bool,
    },
}

/// A deferred daemon RPC raised by the Kanban board (P8.4).
///
/// Like [`AutopilotAction`], the sync key router can't `await`; it stashes the
/// action on [`ScreenStates::pending_kanban_action`] and the plugin's `render`
/// pass drains it and fires `hangar/task_transition` over the daemon socket cap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KanbanAction {
    /// Move a card to a new column (`Shift+←` / `Shift+→`) —
    /// `hangar/task_transition`.
    MoveCard {
        /// The task being moved.
        task_id: String,
        /// The target status wire token (the destination column's drop status).
        to_status: String,
    },
}

/// A deferred daemon RPC raised by the user-defined Boards screen (P4 / D8).
///
/// Like [`KanbanAction`], the sync key router can't `await`; it stashes the
/// action on [`ScreenStates::pending_boards_action`] and the plugin's `render`
/// pass drains it and fires the matching `hangar/board_*` RPC over the daemon
/// socket, then re-pulls `hangar/boards_list`. Only the self-contained (no
/// text-input) mutations are lifted here; run-a-card / rename / add-card are
/// raised as intents but need a dispatch/prompt seam wired in a follow-up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoardsAction {
    /// Create a new board (`b`) — `hangar/board_create`. The empty-state
    /// affordance: fires with no board focused so a fresh workspace can bootstrap
    /// its first board from the TUI.
    BoardCreate {
        /// The new board's name.
        name: String,
    },
    /// Reorder a board's columns (`⇧←/→`) — `hangar/board_column_reorder`.
    ColumnReorder {
        /// The board to reorder.
        board_id: String,
        /// The columns in their new left-to-right order.
        column_ids: Vec<String>,
    },
    /// Delete the focused column (`x`) — `hangar/board_column_delete`.
    ColumnDelete {
        /// The board the column belongs to.
        board_id: String,
        /// The column to delete (its cards park unmapped).
        column_id: String,
    },
    /// Append a column with a default name (`n`) — `hangar/board_column_add`.
    ColumnAdd {
        /// The board to append a column to.
        board_id: String,
        /// The new column's name.
        name: String,
    },
    /// Flip the board's auto-move master toggle (`m`) — `hangar/board_update`.
    BoardUpdate {
        /// The board to retune.
        board_id: String,
        /// The new auto-move value.
        auto_move: bool,
    },
    /// Create a card (issue) from the typed title + picked assignee profile and
    /// place it in a column (`c`) — `hangar/board_card_create` (D8, D16).
    CardCreate {
        /// The board to add the card to.
        board_id: String,
        /// The column to place the card in.
        column_id: String,
        /// The new issue's title.
        title: String,
        /// The picked assignee profile slug, or `None` (unassigned).
        assignee_profile: Option<String>,
    },
    /// Launch a card's issue on its assignee profile now (`Enter` → `Run ▾`) —
    /// `hangar/board_card_run` (D6, D16).
    CardRun {
        /// The board the card sits on.
        board_id: String,
        /// The card's issue to launch.
        issue_id: String,
        /// The launch-mode wire token (`headless` / `interactive`).
        mode: String,
    },
    /// Attach to a card's live run session (`a`).
    CardAttach {
        /// The board the card sits on.
        board_id: String,
        /// The card's issue whose run to attach to.
        issue_id: String,
    },
    /// Rename a column to the typed name (`r`) — `hangar/board_column_update`.
    ColumnRename {
        /// The board the column belongs to.
        board_id: String,
        /// The column to rename.
        column_id: String,
        /// The new column name.
        name: String,
    },
}

/// A deferred daemon RPC raised by the agent-picker modal (e38.8).
///
/// Like [`KanbanAction`], the sync key router can't `await`; the picker stashes
/// the action on [`ScreenStates::pending_assign_action`] and the plugin's
/// `render` pass drains it and fires `hangar/issue_update` over the daemon socket
/// cap, setting the issue's assignee to the picked actor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IssueAssignAction {
    /// Assign an actor to an issue (Enter on a picked actor) —
    /// `hangar/issue_update` with `assignee` set.
    Assign {
        /// The issue the picker was opened for (`issue.id`).
        issue_id: String,
        /// The picked actor in canonical `member:<id>` / `agent:<id>` form.
        actor_ref: String,
    },
}

/// A deferred daemon RPC raised by the task-detail compose modal (e38.5).
///
/// Like [`IssueAssignAction`], the sync key router can't `await`; the compose
/// modal stashes the action on [`ScreenStates::pending_comment_action`] and the
/// plugin's `render` pass drains it and fires `hangar/comment_add` over the
/// daemon socket cap, then re-pulls the issue's comments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IssueCommentAction {
    /// Post a comment on an issue (Enter on a non-empty compose buffer) —
    /// `hangar/comment_add` with the typed body.
    Add {
        /// The issue the comment is for (`issue.id`).
        issue_id: String,
        /// The typed comment body (non-empty).
        body: String,
    },
}

/// A deferred daemon RPC raised by the issue-list inline create flow (e38.29).
///
/// Like [`IssueCommentAction`], the sync key router can't `await`; the create
/// input stashes the action on [`ScreenStates::pending_create_action`] and the
/// plugin's `render` pass drains it and fires `hangar/issue_create` over the
/// daemon socket cap. The daemon's `IssueCreated` push re-renders the new row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IssueCreateAction {
    /// Create a new issue with the typed title (Enter on a non-blank create
    /// input) — `hangar/issue_create`.
    Create {
        /// The new issue's title (non-blank).
        title: String,
    },
}

/// A deferred daemon squad RPC raised by the Squads screen (P7 / D17).
///
/// Like [`BoardsAction`], the sync key router can't `await`; it stashes the action
/// on [`ScreenStates::pending_squads_action`] and the plugin's `render` pass
/// drains it and fires the matching `hangar/squad_*` RPC over the daemon socket,
/// then re-pulls `hangar/squads_list`. The leader (create), member (add), and
/// issue (assign) *selection* is resolved by the glue from its cached
/// agents/issues — the screen intent carries only ids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SquadAction {
    /// Create a squad named `name` (`c` + Enter) — `hangar/squad_create`; the glue
    /// picks the leader from the cached agents.
    Create {
        /// The new squad's name.
        name: String,
    },
    /// Add a member to `squad_id` (`a`) — `hangar/squad_member_add`; the glue picks
    /// the next cached agent not already on the squad.
    AddMember {
        /// The squad to add a member to.
        squad_id: String,
    },
    /// Remove `member_ref` from `squad_id` (`d` on a member row) —
    /// `hangar/squad_member_remove`.
    RemoveMember {
        /// The squad to remove the member from.
        squad_id: String,
        /// The member actor-ref to remove.
        member_ref: String,
    },
    /// Assign the current issue to `squad_id` (`x`) — `hangar/squad_fanout`; the
    /// glue picks the issue and fans the brief to the leader + agent members.
    Assign {
        /// The squad the issue is assigned to.
        squad_id: String,
    },
}

/// A deferred `attention/answer` RPC raised by the control-center screen (P2).
///
/// Like the other deferred actions, the sync key router can't `await`; the
/// control center stashes the answer on [`ScreenStates::pending_answer_action`]
/// and the plugin's `render` pass drains it and fires `attention/answer` over the
/// daemon socket. The daemon runs the first-answer-wins + C1 misroute guards and
/// delivers the pick into the raising session via the one verified send path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttentionAnswerAction {
    /// The attention row to answer.
    pub attention_id: String,
    /// The picked option label delivered into the session.
    pub answer: String,
}

/// A deferred daemon RPC raised by the command-palette modal (e38.13).
///
/// Like [`IssueCreateAction`], the sync key router can't `await`; the palette
/// stashes the action on [`ScreenStates::pending_palette_action`] and the
/// plugin's `render` pass drains it and fires `hangar/search` over the daemon
/// socket cap, then feeds the ranked result back into the palette reducer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteAction {
    /// Run a cross-entity search for the typed query (every keystroke in the
    /// palette) — `hangar/search`.
    Search(String),
}

/// The render-state cache for every Core 5 screen.
///
/// Each field is the daemon's read model for one screen, pulled over the
/// `hangar/*` snapshot RPCs and kept current by folding the event stream. Modal
/// screens (`task_detail`, `agent_picker`) are `Option` — present only while
/// open.
#[derive(Debug, Default)]
pub struct ScreenStates {
    /// Issue-list landing screen cache.
    pub issue_list: IssueListState,
    /// Skill-manager screen cache.
    pub skill_manager: SkillManagerState,
    /// Autopilot-manager screen cache.
    pub autopilots: AutopilotsState,
    /// Kanban board screen cache (P8.4).
    pub kanban: KanbanState,
    /// User-defined Boards screen cache (P4 / D8), built from `hangar/boards_list`.
    pub boards: BoardsState,
    /// Daemon-health screen cache (P8.5), built from `hangar/daemon_health`.
    pub daemon_health: DaemonHealthState,
    /// Usage-dashboard screen cache (e38.35), built from `hangar/usage_rollup`.
    pub usage: UsageState,
    /// Logs-tail screen cache (P8.6), filled by reading the newest `daemon.*`
    /// structured-log file directly from disk (no daemon RPC).
    pub logs: LogsState,
    /// Inbox screen cache (e38.14), filled from the `hangar/inbox_list` snapshot
    /// (the aggregated issue/comment/task entries + the unread count).
    pub inbox: InboxState,
    /// Control-center screen cache (P2), filled from the fleet-wide `attention/list`
    /// snapshot and refreshed on every `AttentionRaised` / `AttentionAnswered` push.
    pub control_center: ControlCenterState,
    /// Squads screen cache (P7 / D17), built from `hangar/squads_list` with each
    /// leader/member resolved against the cached actor snapshot for live status.
    pub squads: SquadsState,
    /// Profile-editor screen cache (P5), filled from `profile/list` (roster) +
    /// `profile/get` (the selected profile's detail + both compile previews).
    pub profiles: ProfilesState,
    /// Settings screen cache (built once the four snapshots arrive).
    pub settings: Option<SettingsState>,
    /// Task-detail screen cache (present only while a task is open).
    pub task_detail: Option<TaskDetailState>,
    /// Agent-picker modal cache (present only while the modal is open).
    pub agent_picker: Option<AgentPickerState>,
    /// Command-palette modal cache (present only while the palette is open,
    /// e38.13).
    pub command_palette: Option<CommandPaletteState>,
    /// Cached actor snapshot (`hangar/agents_list`); the picker is rebuilt from
    /// it whenever the modal opens for an issue.
    pub actors: Vec<ActorRow>,
    /// Number of agents currently working (drives the working-agents chip).
    pub working_count: usize,
    /// A workspace switch/default action raised by the Settings pane, awaiting
    /// the async key handler to call the host cap (P5.5). `None` when idle.
    pub pending_ws_action: Option<WorkspaceAction>,
    /// A skill RPC (sync / detail / attach / detach) raised by the skill-manager
    /// screen, awaiting the `render` pass to fire it over the daemon socket
    /// (P6.5). `None` when idle.
    pub pending_skill_action: Option<SkillAction>,
    /// An autopilot RPC (load-runs / fire-now / set-enabled) raised by the
    /// autopilot-manager screen, awaiting the `render` pass to fire it over the
    /// daemon socket (P7.5). `None` when idle.
    pub pending_autopilot_action: Option<AutopilotAction>,
    /// A Kanban card-move RPC raised by the board (`Shift+←/→`), awaiting the
    /// `render` pass to fire `hangar/task_transition` over the daemon socket
    /// (P8.4). `None` when idle.
    pub pending_kanban_action: Option<KanbanAction>,
    /// An issue-assign RPC raised by the agent-picker modal (Enter on a picked
    /// actor), awaiting the `render` pass to fire `hangar/issue_update` over the
    /// daemon socket (e38.8). `None` when idle.
    pub pending_assign_action: Option<IssueAssignAction>,
    /// An issue-comment RPC raised by the task-detail compose modal (Enter on a
    /// non-empty buffer), awaiting the `render` pass to fire `hangar/comment_add`
    /// over the daemon socket (e38.5). `None` when idle.
    pub pending_comment_action: Option<IssueCommentAction>,
    /// An issue-create RPC raised by the issue-list inline create flow (Enter on
    /// a non-blank title), awaiting the `render` pass to fire `hangar/issue_create`
    /// over the daemon socket (e38.29). `None` when idle.
    pub pending_create_action: Option<IssueCreateAction>,
    /// A search RPC raised by the command-palette modal (each keystroke),
    /// awaiting the `render` pass to fire `hangar/search` over the daemon socket
    /// (e38.13). `None` when idle.
    pub pending_palette_action: Option<PaletteAction>,
    /// Cached workspace catalogue from `host/workspace_list` (P5.5). Seeds the
    /// Settings Workspace pane regardless of which snapshot arrives first.
    pub workspace_rows: Vec<WorkspaceRow>,
    /// Cached member roster from `hangar/members_list` (e38.11). Seeds the
    /// Settings Members pane regardless of which snapshot arrives first.
    pub member_rows: Vec<MemberWireRow>,
    /// Set when the logs screen's level filter changed (P8.6), asking the glue
    /// to re-read the structured-log file under the new `--level` floor. Drained
    /// by the `render` pass. `false` when idle.
    pub pending_logs_refresh: bool,
    /// Set when the inbox screen's `r` key asked to mark all read (e38.14),
    /// awaiting the `render` pass to fire `hangar/inbox_mark_read` over the daemon
    /// socket. Drained by the `render` pass. `false` when idle.
    pub pending_inbox_mark_read: bool,
    /// An `attention/answer` raised by the control-center screen (Enter / a number
    /// key on an ASK), awaiting the `render` pass to fire it over the daemon socket
    /// (P2). `None` when idle.
    pub pending_answer_action: Option<AttentionAnswerAction>,
    /// A board mutation RPC raised by the Boards screen (`⇧←/→`, `x`, `n`, `m`),
    /// awaiting the `render` pass to fire the matching `hangar/board_*` over the
    /// daemon socket (P4). `None` when idle.
    pub pending_boards_action: Option<BoardsAction>,
    /// A squad mutation RPC raised by the Squads screen (`c`, `a`, `d`, `x`),
    /// awaiting the `render` pass to fire the matching `hangar/squad_*` over the
    /// daemon socket (P7 / D17). `None` when idle.
    pub pending_squads_action: Option<SquadAction>,
    /// A `profile/get` raised by the profile editor (selection moved to a row
    /// whose detail is not loaded), awaiting the `render` pass to fire it. Carries
    /// the slug to fetch. `None` when idle (P5).
    pub pending_profile_detail: Option<String>,
    /// A `profile/upsert` raised by the profile editor (`t` cycled the tier),
    /// awaiting the `render` pass to fire it. Carries `(slug, tier)`. `None` when
    /// idle (P5).
    pub pending_profile_upsert: Option<(String, String)>,
}

impl Default for SkillManagerState {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl ScreenStates {
    /// Replace the issue-list rows from an `hangar/issues_list` snapshot.
    pub fn set_issues(&mut self, issues: Vec<IssueRow>) {
        self.issue_list = IssueListState::with_rows(issues);
    }

    /// Replace the skill-manager rows from an `hangar/skills_list` snapshot.
    pub fn set_skills(&mut self, skills: Vec<SkillRow>) {
        self.skill_manager = SkillManagerState::new(skills);
    }

    /// Replace the autopilot-manager rows from an `hangar/autopilots_list`
    /// snapshot (P7.5).
    pub fn set_autopilots(&mut self, autopilots: Vec<AutopilotRow>) {
        self.autopilots = AutopilotsState::new(autopilots);
    }

    /// Rebuild the Kanban board from a `hangar/tasks_list` snapshot (P8.4). Ages
    /// are recomputed at render time, so a placeholder `now` is fine here — the
    /// renderer is passed the live clock.
    pub fn set_tasks(&mut self, tasks: &[TaskCardRow]) {
        self.kanban = KanbanState::from_tasks(tasks, 0);
    }

    /// Rebuild the user-defined Boards screen from a `hangar/boards_list`
    /// snapshot (P4 / D8), preserving the injected profile roster + any open
    /// overlay / transient note across the refresh (ccc): a background refresh
    /// while the user is typing a card title must not drop the input.
    pub fn set_boards(&mut self, snapshot: &ainb_hangar_proto::snapshots::BoardsListResult) {
        let mut next = BoardsState::from_snapshot(snapshot);
        next.adopt_context(&self.boards);
        self.boards = next;
    }

    /// Inject the assignee-profile roster (slugs) the Boards card-create picker
    /// offers, from the cached `profile/list` (ccc / D16).
    pub fn set_boards_profiles(&mut self, profiles: Vec<String>) {
        self.boards.set_profiles(profiles);
    }

    /// Mark the Boards fetch as failed so the render shows the error rather than
    /// the "no boards yet" create prompt (P4 / D8). Preserves any board already
    /// loaded — a transient mutation error never blanks a live board.
    pub fn set_boards_error(&mut self, message: impl Into<String>) {
        self.boards.set_error(message);
    }

    /// Take the pending board mutation RPC raised by the Boards screen, if any
    /// (P4).
    pub const fn take_pending_boards_action(&mut self) -> Option<BoardsAction> {
        self.pending_boards_action.take()
    }

    /// Rebuild the daemon-health pane from a `hangar/daemon_health` snapshot
    /// (P8.5).
    pub fn set_daemon_health(&mut self, snap: DaemonHealthSnapshot) {
        self.daemon_health = DaemonHealthState::from_snapshot(snap);
    }

    /// Rebuild the usage dashboard's rollup fields from a `hangar/usage_rollup`
    /// snapshot (e38.35) for workspace `ws`, preserving the run-history timeline
    /// when it belongs to the same workspace (they arrive on separate replies,
    /// P10 / D19). A reply for a different workspace resets the state first so a
    /// stale prior-tenant timeline can never sit beside fresh totals.
    pub fn set_usage(&mut self, ws: &str, rollup: UsageRollupResult) {
        self.usage.apply_rollup(ws, rollup);
    }

    /// Update the usage dashboard's recent-runs timeline from a
    /// `hangar/run_history` snapshot (P10 / D19) for workspace `ws`, preserving the
    /// rollup totals when they belong to the same workspace. A reply for a
    /// different workspace resets the state first.
    pub fn set_run_history(&mut self, ws: &str, history: RunHistoryResult) {
        self.usage.apply_run_history(ws, history);
    }

    /// Replace the logs-tail rows from a fresh read of the `daemon.*` file
    /// (P8.6), preserving the active level filter so a re-read under the same
    /// chip keeps the chip lit.
    pub fn set_logs(&mut self, lines: Vec<ainb_hangar_core::logs::LogLine>) {
        let filter = self.logs.filter();
        let mut state = LogsState::from_lines(lines);
        state.set_filter(filter);
        self.logs = state;
    }

    /// Take the pending logs-refresh request (filter changed), if any (P8.6).
    pub const fn take_pending_logs_refresh(&mut self) -> bool {
        let pending = self.pending_logs_refresh;
        self.pending_logs_refresh = false;
        pending
    }

    /// Replace the inbox cache from a `hangar/inbox_list` snapshot (e38.14).
    pub fn set_inbox(
        &mut self,
        entries: Vec<ainb_hangar_proto::events::InboxEntryRow>,
        unread: i64,
    ) {
        self.inbox = InboxState::from_snapshot(entries, unread);
    }

    /// Take the pending mark-all-read request (`r` pressed), if any (e38.14).
    pub const fn take_pending_inbox_mark_read(&mut self) -> bool {
        let pending = self.pending_inbox_mark_read;
        self.pending_inbox_mark_read = false;
        pending
    }

    /// Rebuild the control-center board from an `attention/list` /
    /// `attention/subscribe` snapshot (P2), preserving the human's focus + option
    /// cursor across the auto-shuffle.
    pub fn set_attention(&mut self, rows: &[ainb_hangar_proto::events::AttentionRow]) {
        self.control_center.set_attention(rows);
    }

    /// Take the pending `attention/answer` raised by the control center, if any (P2).
    pub const fn take_pending_answer_action(&mut self) -> Option<AttentionAnswerAction> {
        self.pending_answer_action.take()
    }

    /// Rebuild the Squads screen from a `hangar/squads_list` snapshot (or any
    /// `hangar/squad_*` mutation reply, which returns the same refreshed envelope)
    /// (P7 / D17), resolving actors against the cached agent snapshot and
    /// preserving the selection, open create input, and transient note across the
    /// refresh.
    pub fn set_squads(&mut self, snapshot: &ainb_hangar_proto::snapshots::SquadsListResult) {
        let selected = self.squads.selected_index();
        let creating = self.squads.create_buffer().map(str::to_string);
        let note = self.squads.note().cloned();
        let mut next = SquadsState::from_snapshot(snapshot, &self.actors);
        next.set_selected(selected);
        next.set_create_buffer(creating);
        next.set_note(note);
        self.squads = next;
    }

    /// Take the pending squad mutation RPC raised by the Squads screen, if any
    /// (P7 / D17).
    pub const fn take_pending_squads_action(&mut self) -> Option<SquadAction> {
        self.pending_squads_action.take()
    }

    /// Replace the profile-editor roster from a `profile/list` result (P5),
    /// preserving the selection where possible. Arms a `profile/get` for the
    /// selected profile when its detail is not yet loaded, so the preview pane
    /// fills right after the first roster load (no manual navigation needed).
    pub fn set_profiles(&mut self, rows: Vec<super::profiles::ProfileRosterEntry>) {
        self.profiles.set_roster(rows);
        if self.pending_profile_detail.is_none() {
            self.pending_profile_detail = self.profiles.needs_detail();
        }
    }

    /// Fold a `profile/get` result into the selected profile's detail (P5).
    pub fn set_profile_detail(&mut self, detail: super::profiles::ProfileDetailView) {
        self.profiles.set_detail(detail);
    }

    /// Take the pending `profile/get` slug raised by the profile editor, if any (P5).
    pub fn take_pending_profile_detail(&mut self) -> Option<String> {
        self.pending_profile_detail.take()
    }

    /// Take the pending `profile/upsert` `(slug, tier)` raised by the profile
    /// editor, if any (P5).
    pub fn take_pending_profile_upsert(&mut self) -> Option<(String, String)> {
        self.pending_profile_upsert.take()
    }

    /// Cache the agent snapshot rows; the picker is rebuilt from them on open.
    pub fn set_actors(&mut self, actors: Vec<ActorRow>) {
        self.actors = actors;
    }

    /// Build the settings cache from the four daemon snapshots.
    ///
    /// Providers / keys / workspaces are derived minimally at P4: the providers
    /// come from the health socket being up (a single `claude` entry until the
    /// provider snapshot RPC lands), the keys are empty (the keychain read is a
    /// later cap), and the workspaces carry the current id.
    pub fn set_health(&mut self, health: HealthSnapshot, ws_id: &str) {
        let providers = vec![ProviderRow {
            name: "claude".into(),
            online: health.connected,
        }];
        let keys: Vec<KeyRow> = Vec::new();
        // Prefer the cached host workspace catalogue (P5.5); fall back to the
        // single current workspace until the list lands.
        let workspaces = if self.workspace_rows.is_empty() {
            vec![WorkspaceRow {
                id: ws_id.to_string(),
                slug: ws_id.to_string(),
                name: ws_id.to_string(),
                current: true,
                default: true,
            }]
        } else {
            self.workspace_rows.clone()
        };
        let mut state = SettingsState::new(health, providers, keys, workspaces);
        // Carry any cached member roster into the rebuilt state so the Members
        // pane survives a `set_health` rebuild (mirrors workspace_rows).
        state.set_members(self.member_rows.clone());
        self.settings = Some(state);
    }

    /// Refresh the Settings Workspace pane from a `host/workspace_list` result
    /// (P5.5). Caches the rows so a later `set_health` rebuild keeps them, and
    /// overlays the live settings state when it already exists.
    pub fn set_workspaces(&mut self, workspaces: Vec<WorkspaceRow>) {
        self.workspace_rows.clone_from(&workspaces);
        if let Some(s) = self.settings.as_mut() {
            s.set_workspaces(workspaces);
        }
    }

    /// Refresh the Settings Members pane from a `hangar/members_list` result
    /// (e38.11). Caches the rows so a later `set_health` rebuild keeps them, and
    /// overlays the live settings state when it already exists.
    pub fn set_members(&mut self, members: Vec<MemberWireRow>) {
        self.member_rows.clone_from(&members);
        if let Some(s) = self.settings.as_mut() {
            s.set_members(members);
        }
    }

    /// Take the pending workspace action raised by the Settings pane, if any.
    pub const fn take_pending_ws_action(&mut self) -> Option<WorkspaceAction> {
        self.pending_ws_action.take()
    }

    /// Take the pending skill RPC raised by the skill-manager screen, if any.
    pub const fn take_pending_skill_action(&mut self) -> Option<SkillAction> {
        self.pending_skill_action.take()
    }

    /// Take the pending autopilot RPC raised by the autopilot-manager screen,
    /// if any.
    pub const fn take_pending_autopilot_action(&mut self) -> Option<AutopilotAction> {
        self.pending_autopilot_action.take()
    }

    /// Take the pending Kanban card-move RPC raised by the board, if any (P8.4).
    pub const fn take_pending_kanban_action(&mut self) -> Option<KanbanAction> {
        self.pending_kanban_action.take()
    }

    /// Take the pending issue-assign RPC raised by the agent picker, if any
    /// (e38.8).
    pub const fn take_pending_assign_action(&mut self) -> Option<IssueAssignAction> {
        self.pending_assign_action.take()
    }

    /// Take the pending issue-comment RPC raised by the task-detail compose
    /// modal, if any (e38.5).
    pub const fn take_pending_comment_action(&mut self) -> Option<IssueCommentAction> {
        self.pending_comment_action.take()
    }

    /// Take the pending issue-create RPC raised by the issue-list inline create
    /// flow, if any (e38.29).
    pub const fn take_pending_create_action(&mut self) -> Option<IssueCreateAction> {
        self.pending_create_action.take()
    }

    /// Take the pending search RPC raised by the command palette, if any
    /// (e38.13).
    pub const fn take_pending_palette_action(&mut self) -> Option<PaletteAction> {
        self.pending_palette_action.take()
    }
}

/// The cached actor snapshot, stashed on [`ScreenStates`] so the picker can be
/// rebuilt for whichever issue it is opened on.
impl ScreenStates {
    /// Open the agent picker for `issue` over the cached actor snapshot.
    pub fn open_picker(&mut self, issue: ainb_hangar_core::ids::IssueId) {
        self.agent_picker = Some(AgentPickerState::new(issue, self.actors.clone()));
    }

    /// Open a fresh, empty command palette (e38.13).
    pub fn open_palette(&mut self) {
        self.command_palette = Some(CommandPaletteState::new());
    }

    /// Fold a `hangar/search` result into the open palette (e38.13). A no-op when
    /// the palette has since closed (a stale reply for a dismissed modal).
    pub fn set_palette_results(&mut self, results: Vec<ainb_hangar_proto::snapshots::SearchEntry>) {
        if let Some(palette) = self.command_palette.take() {
            let out = reduce_command_palette(&palette, CommandPaletteEvent::ResultsLoaded(results));
            self.command_palette = Some(out.state);
        }
    }

    /// Open task detail for `issue`'s task, seeding from the issue's row.
    pub fn open_task_detail(&mut self, task_id: ainb_hangar_core::ids::TaskId, issue: IssueRow) {
        self.task_detail = Some(TaskDetailState::new(task_id, issue));
    }

    /// Apply a freshly-fetched PR status to the open task-detail screen (e38.34)
    /// so the badge re-renders the CI + merge state. A no-op when no task-detail
    /// is open (the reply outlived the screen).
    pub const fn set_task_detail_pr_status(
        &mut self,
        status: ainb_hangar_proto::pr_status::PrStatus,
    ) {
        if let Some(td) = self.task_detail.as_mut() {
            td.set_pr_status(status);
        }
    }
}

/// A cross-screen navigation the key router surfaces to the plugin glue, which
/// owns the routing-state transition (it can't be done inside a screen reducer).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NavIntent {
    /// Open the agent-picker modal for an issue (raised by `a`).
    OpenAgentPicker(ainb_hangar_core::ids::IssueId),
    /// Open task detail for the issue under the selection (raised by Enter).
    OpenTaskForIssue(ainb_hangar_core::ids::IssueId),
    /// Open the task's captured PR URL in the host browser (raised by `o` on the
    /// task-detail screen, P9.2). Only surfaced when the task has a `pr_url` — `o`
    /// is a no-op (no intent) when none, so there is never a silent open of
    /// nothing.
    OpenPrUrl(String),
    /// Close the active modal back to its prior screen (raised by Esc on a modal).
    CloseModal,
    /// Jump to an entity's screen from the command palette (raised by Enter on a
    /// palette result, e38.13). Carries the jump-target screen token, the entity
    /// id, and the kind tag so the glue can switch the routing screen and select
    /// the row where possible.
    NavigateToEntity {
        /// The screen-routing token to open (e.g. `"issue_list"`).
        screen: String,
        /// The selected entity's id.
        id: String,
        /// The selected entity's kind tag (e.g. `"issue"`).
        kind: String,
    },
}

/// Render the active screen's body between the chrome top bar (row 0) and footer
/// (last row). Modal screens overlay the whole area.
pub fn render_body(buf: &mut WireBuffer, w: u16, h: u16, app: &AppState, states: &ScreenStates) {
    let top = 1u16;
    let bottom = h.saturating_sub(1);
    match &app.screen {
        Screen::IssueList => {
            super::issue_list::render_issue_list(
                buf,
                w,
                top,
                bottom,
                &states.issue_list,
                states.working_count,
            );
        }
        Screen::SkillManager => {
            super::skill_manager::render_skill_manager(buf, w, top, bottom, &states.skill_manager);
        }
        Screen::Autopilots => {
            super::autopilots::render_autopilots(buf, w, top, bottom, &states.autopilots);
        }
        Screen::Kanban => {
            super::kanban::render_kanban(buf, w, top, bottom, &states.kanban, now_ms());
        }
        Screen::Boards => {
            super::boards::render_boards(buf, w, top, bottom, &states.boards);
        }
        Screen::DaemonHealth => {
            super::daemon_health::render_daemon_health(buf, w, top, bottom, &states.daemon_health);
        }
        Screen::Usage => {
            super::usage_dashboard::render_usage(buf, w, top, bottom, &states.usage);
        }
        Screen::Logs => {
            super::logs::render_logs(buf, w, top, bottom, &states.logs);
        }
        Screen::Inbox => {
            super::inbox::render_inbox(buf, w, top, bottom, &states.inbox);
        }
        Screen::ControlCenter => {
            super::control_center::render_control_center(
                buf,
                w,
                top,
                bottom,
                &states.control_center,
                now_ms(),
            );
        }
        Screen::Squads => {
            super::squads::render_squads(buf, w, top, bottom, &states.squads);
        }
        Screen::Profiles => {
            super::profiles::render_profiles(buf, w, top, bottom, &states.profiles);
        }
        Screen::Settings => {
            if let Some(s) = &states.settings {
                super::settings::render_settings(buf, w, h, top, bottom, s);
            }
        }
        Screen::TaskDetail(_) => {
            if let Some(td) = &states.task_detail {
                super::task_detail::render_task_detail(buf, w, top, bottom, td);
            }
        }
        Screen::AgentPicker(_) => {
            // The picker is a modal: paint the screen it overlays first, then the
            // modal centred over the whole area.
            if let Some(prior) = &app.prior_screen {
                render_prior(buf, w, h, prior, states);
            }
            if let Some(picker) = &states.agent_picker {
                super::agent_picker::render_agent_picker(buf, w, h, picker);
            }
        }
        Screen::CommandPalette => {
            // The palette is a modal: paint the screen it overlays first, then
            // the palette centred over the whole area (e38.13).
            if let Some(prior) = &app.prior_screen {
                render_prior(buf, w, h, prior, states);
            }
            if let Some(palette) = &states.command_palette {
                super::command_palette::render_command_palette(buf, w, h, palette);
            }
        }
        Screen::Help => render_help(buf, w, h),
    }
}

/// The current wall-clock time in epoch milliseconds, for card-age derivation.
/// A clock skew before the epoch saturates to `0` (ages read `0m`).
fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// Render the screen a modal overlays (so the modal floats over real content).
fn render_prior(buf: &mut WireBuffer, w: u16, h: u16, prior: &Screen, states: &ScreenStates) {
    let top = 1u16;
    let bottom = h.saturating_sub(1);
    match prior {
        Screen::IssueList => super::issue_list::render_issue_list(
            buf,
            w,
            top,
            bottom,
            &states.issue_list,
            states.working_count,
        ),
        Screen::SkillManager => {
            super::skill_manager::render_skill_manager(buf, w, top, bottom, &states.skill_manager);
        }
        _ => {}
    }
}

/// Render the help overlay (a simple centred hint list).
fn render_help(buf: &mut WireBuffer, w: u16, h: u16) {
    use ainb_plugin_sdk::{Cell, Color, Coord};
    const GOLD: Color = Color::rgb(255, 215, 0);
    let lines = [
        "Hangar — keys",
        "1 issues  2 task  3 skills  , settings",
        "a assign  c create  / filter",
        "esc close  q quit",
    ];
    let y0 = h / 2 - 2;
    for (i, line) in lines.iter().enumerate() {
        let y = y0 + u16::try_from(i).unwrap_or(0);
        let line_w = u16::try_from(line.chars().count()).unwrap_or(u16::MAX);
        let x0 = w.saturating_sub(line_w) / 2;
        for (ch, cx) in line.chars().zip(x0..w) {
            let mut cell = Cell::new(ch.to_string());
            cell.fg = Some(GOLD);
            buf.push(Coord::new(cx, y), cell);
        }
    }
}

/// Translate a wire [`KeyEvent`] into the `char` the pure screen reducers expect.
///
/// The reducers model navigation as printable chars (`'j'`, `'/'`, …) plus `'\n'`
/// for Enter and `'\u{8}'` for Backspace. Esc and the tab-switch keys are handled
/// by the caller before reaching a reducer.
const fn key_char(key: &KeyEvent) -> Option<char> {
    match &key.code {
        KeyCode::Char { ch } => Some(*ch),
        KeyCode::Enter => Some('\n'),
        KeyCode::Backspace => Some('\u{8}'),
        _ => None,
    }
}

/// Fold a forwarded key into the active screen's reducer, returning an optional
/// cross-screen [`NavIntent`] the plugin glue must act on.
///
/// Tab-switch keys (`1`/`2`/`3`/`,`) and `?`/Esc are routing-layer concerns
/// handled by the caller via [`super::reduce`]; this function owns the
/// **per-screen** keys (`j`/`k`/`/`/`a`/Enter/…).
pub fn route_key(app: &AppState, states: &mut ScreenStates, key: &KeyEvent) -> Option<NavIntent> {
    match &app.screen {
        Screen::IssueList => route_issue_list(states, key),
        Screen::SkillManager => {
            if let Some(c) = key_char(key) {
                let out = reduce_skill_manager(&states.skill_manager, SkillManagerEvent::Key(c));
                states.skill_manager = out.state;
                // Lift the screen intent into a deferred daemon RPC; the async
                // `render` pass drains `pending_skill_action` and fires it (the
                // sync key router can't `await`). `LoadFiles` is the legacy P4
                // file-only path and has no P6.5 RPC.
                states.pending_skill_action = match out.intent {
                    Some(SkillManagerIntent::Sync) => Some(SkillAction::Sync),
                    Some(SkillManagerIntent::LoadDetail(slug)) => {
                        Some(SkillAction::LoadDetail(slug))
                    }
                    Some(SkillManagerIntent::Attach(slug)) => Some(SkillAction::Attach(slug)),
                    Some(SkillManagerIntent::Detach(slug)) => Some(SkillAction::Detach(slug)),
                    Some(SkillManagerIntent::LoadFiles(_)) | None => None,
                };
            }
            None
        }
        Screen::Autopilots => {
            if let Some(c) = key_char(key) {
                let out = reduce_autopilots(&states.autopilots, AutopilotsEvent::Key(c));
                states.autopilots = out.state;
                // Lift the screen intent into a deferred daemon RPC the `render`
                // pass drains + fires (the sync key router can't `await`). `Add` /
                // `Edit` are create-flow intents with no P7.5 RPC yet.
                states.pending_autopilot_action = match out.intent {
                    Some(AutopilotsIntent::FireNow(id)) => Some(AutopilotAction::FireNow(id)),
                    Some(AutopilotsIntent::SetEnabled {
                        autopilot_id,
                        enabled,
                    }) => Some(AutopilotAction::SetEnabled {
                        autopilot_id,
                        enabled,
                    }),
                    Some(AutopilotsIntent::LoadRuns(id)) => Some(AutopilotAction::LoadRuns(id)),
                    Some(AutopilotsIntent::Add | AutopilotsIntent::Edit(_)) | None => None,
                };
            }
            None
        }
        Screen::Kanban => {
            route_kanban(states, key);
            None
        }
        Screen::Boards => {
            route_boards(states, key);
            None
        }
        Screen::Settings => {
            if let Some(s) = states.settings.take() {
                // Build the settings event; a key the reducer doesn't model
                // (not Char/Enter/Backspace/Esc) is a no-op — but we MUST put
                // the state back before returning, never drop it on the floor
                // (else the settings screen goes dead and stops navigating).
                let ev = if matches!(key.code, KeyCode::Esc) {
                    SettingsEvent::Esc
                } else if let Some(c) = key_char(key) {
                    SettingsEvent::Key(c)
                } else {
                    states.settings = Some(s);
                    return None;
                };
                let out = reduce_settings(&s, ev);
                let now_on_workspaces =
                    out.state.section() == super::settings::SettingsSection::Workspaces;
                states.settings = Some(out.state);
                // Lift the workspace switch/default intents into a deferred host
                // action; the async key handler drains it and calls the cap.
                match out.intent {
                    Some(SettingsIntent::SwitchWorkspace(id)) => {
                        states.pending_ws_action = Some(WorkspaceAction::SetActive(id));
                    }
                    Some(SettingsIntent::ToggleDefault(id)) => {
                        states.pending_ws_action = Some(WorkspaceAction::SetDefault(id));
                    }
                    // KeychainWrite / New / Rename land in their own beads.
                    _ => {
                        // Seed the pane from the live host workspace list the
                        // first time the user lands on the Workspace section.
                        if now_on_workspaces && states.workspace_rows.is_empty() {
                            states.pending_ws_action = Some(WorkspaceAction::Refresh);
                        }
                    }
                }
            }
            None
        }
        Screen::TaskDetail(_) => route_task_detail(states, key),
        Screen::AgentPicker(_) => route_agent_picker(states, key),
        Screen::CommandPalette => route_command_palette(states, key),
        Screen::Logs => {
            // The logs pane owns the level-filter chips (`a`/`i`/`w`/`e`). A
            // filter change flags a deferred re-read of the `daemon.*` file
            // under the new `--level` floor; the `render` pass drains it.
            if let Some(c) = key_char(key) {
                if states.logs.handle_key(c) {
                    states.pending_logs_refresh = true;
                }
            }
            None
        }
        Screen::Inbox => {
            // The inbox owns the mark-all-read key (`r`): it flags a deferred
            // `hangar/inbox_mark_read` request the `render` pass fires, after
            // which the re-pulled snapshot drops the unread badge to zero.
            if key_char(key) == Some('r') {
                states.pending_inbox_mark_read = true;
            }
            None
        }
        Screen::ControlCenter => {
            route_control_center(states, key);
            None
        }
        Screen::Squads => {
            route_squads(states, key);
            None
        }
        Screen::Profiles => {
            route_profiles(states, key);
            None
        }
        // Read-only / overlay screens with no per-screen keys: the daemon-health
        // pane (P8.5), the usage dashboard (e38.35), and the help overlay (the
        // `D`/`U`/`?` tab-switch + global keys are handled by the router before
        // reaching here).
        Screen::DaemonHealth | Screen::Usage | Screen::Help => None,
    }
}

/// Issue-list key routing: fold into the reducer, lifting the open-task /
/// open-picker intents into [`NavIntent`]s (the routing screen lives on the
/// plugin's [`AppState`], not in the issue-list reducer).
fn route_issue_list(states: &mut ScreenStates, key: &KeyEvent) -> Option<NavIntent> {
    let c = key_char(key)?;
    let out = reduce_issue_list(&states.issue_list, IssueListEvent::Key(c));
    states.issue_list = out.state;
    match out.intent {
        Some(IssueListIntent::OpenAgentPicker(id)) => Some(NavIntent::OpenAgentPicker(id)),
        Some(IssueListIntent::OpenTaskDetail(id)) => Some(NavIntent::OpenTaskForIssue(id)),
        // e38.29: a submitted create-title lifts into a deferred
        // `hangar/issue_create` RPC the `render` pass drains + fires (the sync key
        // router can't `await`).
        Some(IssueListIntent::CreateIssue { title }) => {
            states.pending_create_action = Some(IssueCreateAction::Create { title });
            None
        }
        None => None,
    }
}

/// Task-detail key routing (P4.4 + P9.2): fold keys into the pure reducer, but
/// intercept `o` (open PR) at the routing layer first.
///
/// `o` is not a reducer key (the reducer owns scroll / retry / cancel); it is a
/// cross-screen side effect (launch the host browser), so it surfaces a
/// [`NavIntent::OpenPrUrl`] the plugin glue acts on — and **only** when the task
/// carries a `pr_url`. When there is no PR, `o` raises no intent (it folds into
/// the reducer as an unmodelled no-op), so there is never a silent open of
/// nothing. Esc + all other keys fold into the reducer as before.
fn route_task_detail(states: &mut ScreenStates, key: &KeyEvent) -> Option<NavIntent> {
    let td = states.task_detail.take()?;
    // Intercept `o` before the reducer: open the captured PR URL, if any — but
    // NOT while the compose modal is open, where `o` is a typed character (e38.5).
    if td.compose_buffer().is_none() && key.code == (KeyCode::Char { ch: 'o' }) {
        let nav = td.pr_url().map(|url| NavIntent::OpenPrUrl(url.to_string()));
        states.task_detail = Some(td);
        return nav;
    }
    // Esc folds in directly; any other printable char becomes a `Key` event;
    // a non-printable key is an unmodelled no-op (restore state and bail).
    let ev = if key.code == KeyCode::Esc {
        TaskDetailEvent::Esc
    } else if let Some(c) = key_char(key) {
        TaskDetailEvent::Key(c)
    } else {
        states.task_detail = Some(td);
        return None;
    };
    let out = reduce_task_detail(&td, ev);
    // Lift the compose-submit intent into a deferred `hangar/comment_add` RPC the
    // `render` pass drains + fires (the sync key router can't `await`). Retry /
    // cancel intents are not yet wired to an RPC, so they fold as before.
    if let Some(TaskDetailIntent::AddComment { issue_id, body }) = out.intent {
        states.pending_comment_action = Some(IssueCommentAction::Add {
            issue_id: issue_id.as_str().to_string(),
            body,
        });
    }
    states.task_detail = Some(out.state);
    None
}

/// Kanban board key routing (P8.4): map the arrow keys (plus Shift) into the
/// board reducer. `←/→/↑/↓` move focus; `Shift+←/→` drag the focused card and
/// lift the resulting [`KanbanIntent::MoveCard`] into a deferred
/// `hangar/task_transition` RPC (the sync key router can't `await`; the `render`
/// pass drains `pending_kanban_action` and fires it). `h/j/k/l` mirror the arrows
/// for vi-style navigation.
fn route_kanban(states: &mut ScreenStates, key: &KeyEvent) {
    let shift = key.mods & ainb_plugin_sdk::KEY_MOD_SHIFT != 0;
    let ev = match &key.code {
        KeyCode::Left => Some(if shift {
            KanbanEvent::MoveCardLeft
        } else {
            KanbanEvent::FocusLeft
        }),
        KeyCode::Right => Some(if shift {
            KanbanEvent::MoveCardRight
        } else {
            KanbanEvent::FocusRight
        }),
        KeyCode::Up => Some(KanbanEvent::FocusUp),
        KeyCode::Down => Some(KanbanEvent::FocusDown),
        // vi-style fallbacks: capital H/L drag a card, lowercase navigate.
        KeyCode::Char { ch } => match ch {
            'h' => Some(KanbanEvent::FocusLeft),
            'l' => Some(KanbanEvent::FocusRight),
            'k' => Some(KanbanEvent::FocusUp),
            'j' => Some(KanbanEvent::FocusDown),
            'H' => Some(KanbanEvent::MoveCardLeft),
            'L' => Some(KanbanEvent::MoveCardRight),
            _ => None,
        },
        _ => None,
    };
    let Some(ev) = ev else {
        return;
    };
    let out = reduce_kanban(&states.kanban, ev);
    states.kanban = out.state;
    if let Some(KanbanIntent::MoveCard { task_id, to_status }) = out.intent {
        states.pending_kanban_action = Some(KanbanAction::MoveCard { task_id, to_status });
    }
}

/// Boards screen key routing (P4 / D8, ccc / D6, D16): fold keys into the boards
/// reducer, lifting each raised intent into a deferred [`BoardsAction`] the
/// `render` pass fires over the daemon socket (the sync key router can't `await`).
///
/// When an interactive overlay is open (card create / column rename / `Run ▾`),
/// EVERY key routes to it as a [`BoardsEvent::Key`] so typed text and picker
/// motion land in the input rather than moving the board. With no overlay open
/// the navigation/verb map applies: `←/→/↑/↓` (and `h/j/k/l`) move focus; `[`/`]`
/// switch boards; `⇧←/→` reorder; `x` deletes a column; `n` appends one; `m`
/// toggles auto-move; `c` opens card-create; `r` opens column-rename; `Enter`
/// opens the card's `Run ▾`; `a` attaches to the card's run.
fn route_boards(states: &mut ScreenStates, key: &KeyEvent) {
    let ev = if states.boards.overlay().is_some() {
        overlay_key_event(key)
    } else {
        board_nav_event(key)
    };
    let Some(ev) = ev else {
        return;
    };
    let out = reduce_boards(&states.boards, ev);
    states.boards = out.state;
    states.pending_boards_action = lift_boards_intent(out.intent);
}

/// Translate a raw key into an overlay [`BoardsEvent::Key`] while an overlay is
/// open. Every printable char / Backspace / Enter / Esc / ↑ / ↓ is folded into
/// the input; unmapped keys are dropped.
fn overlay_key_event(key: &KeyEvent) -> Option<BoardsEvent> {
    let k = match &key.code {
        KeyCode::Enter => BoardsKey::Enter,
        KeyCode::Esc => BoardsKey::Esc,
        KeyCode::Backspace => BoardsKey::Backspace,
        KeyCode::Up => BoardsKey::Up,
        KeyCode::Down => BoardsKey::Down,
        KeyCode::Char { ch } => BoardsKey::Char(*ch),
        _ => return None,
    };
    Some(BoardsEvent::Key(k))
}

/// Map a raw key to a board navigation / verb event when no overlay is open.
fn board_nav_event(key: &KeyEvent) -> Option<BoardsEvent> {
    let shift = key.mods & ainb_plugin_sdk::KEY_MOD_SHIFT != 0;
    match &key.code {
        KeyCode::Left => Some(if shift {
            BoardsEvent::ReorderColumnLeft
        } else {
            BoardsEvent::FocusLeft
        }),
        KeyCode::Right => Some(if shift {
            BoardsEvent::ReorderColumnRight
        } else {
            BoardsEvent::FocusRight
        }),
        KeyCode::Up => Some(BoardsEvent::FocusUp),
        KeyCode::Down => Some(BoardsEvent::FocusDown),
        KeyCode::Enter => Some(BoardsEvent::RunFocusedCard),
        KeyCode::Char { ch } => match ch {
            'h' => Some(BoardsEvent::FocusLeft),
            'l' => Some(BoardsEvent::FocusRight),
            'k' => Some(BoardsEvent::FocusUp),
            'j' => Some(BoardsEvent::FocusDown),
            'H' => Some(BoardsEvent::ReorderColumnLeft),
            'L' => Some(BoardsEvent::ReorderColumnRight),
            '[' => Some(BoardsEvent::PrevBoard),
            ']' => Some(BoardsEvent::NextBoard),
            'b' => Some(BoardsEvent::CreateBoard),
            'a' => Some(BoardsEvent::AttachFocusedCard),
            'n' => Some(BoardsEvent::AddColumn),
            'r' => Some(BoardsEvent::RenameColumn),
            'x' => Some(BoardsEvent::DeleteColumn),
            'c' => Some(BoardsEvent::AddCard),
            'm' => Some(BoardsEvent::ToggleAutoMove),
            _ => None,
        },
        _ => None,
    }
}

/// Lift a raised [`BoardsIntent`] into the deferred [`BoardsAction`] the render
/// pass fires. Board/column CRUD carry their own defaults; the card intents carry
/// the typed title / picked profile / chosen run mode from the committed overlay.
fn lift_boards_intent(intent: Option<BoardsIntent>) -> Option<BoardsAction> {
    match intent? {
        BoardsIntent::CreateBoard => Some(BoardsAction::BoardCreate {
            name: "New Board".to_string(),
        }),
        BoardsIntent::ReorderColumns {
            board_id,
            column_ids,
        } => Some(BoardsAction::ColumnReorder {
            board_id,
            column_ids,
        }),
        BoardsIntent::DeleteColumn {
            board_id,
            column_id,
        } => Some(BoardsAction::ColumnDelete {
            board_id,
            column_id,
        }),
        BoardsIntent::AddColumn { board_id } => Some(BoardsAction::ColumnAdd {
            board_id,
            name: "New Column".to_string(),
        }),
        BoardsIntent::ToggleAutoMove {
            board_id,
            auto_move,
        } => Some(BoardsAction::BoardUpdate {
            board_id,
            auto_move,
        }),
        BoardsIntent::CreateCard {
            board_id,
            column_id,
            title,
            assignee_profile,
        } => Some(BoardsAction::CardCreate {
            board_id,
            column_id,
            title,
            assignee_profile,
        }),
        BoardsIntent::RunCard {
            board_id,
            issue_id,
            mode,
        } => Some(BoardsAction::CardRun {
            board_id,
            issue_id,
            mode: mode.wire().to_string(),
        }),
        BoardsIntent::AttachCard { board_id, issue_id } => Some(BoardsAction::CardAttach {
            board_id,
            issue_id,
        }),
        BoardsIntent::RenameColumn {
            board_id,
            column_id,
            name,
        } => Some(BoardsAction::ColumnRename {
            board_id,
            column_id,
            name,
        }),
    }
}

/// Control-center key routing (P2): map the navigation keys into the board
/// reducer, lifting an [`ControlCenterIntent::Answer`] into a deferred
/// `attention/answer` RPC (the sync key router can't `await`; the `render` pass
/// drains [`ScreenStates::pending_answer_action`] and fires it).
///
/// `↓`/`↑` (and `j`/`k`) move the card selection; `→`/`←` (and `l`/`h`) move the
/// ASK option cursor; `Enter` answers the highlighted option and `1`..`9` answer
/// directly. An unmapped key folds as a no-op.
fn route_control_center(states: &mut ScreenStates, key: &KeyEvent) {
    let c = match &key.code {
        KeyCode::Down => 'j',
        KeyCode::Up => 'k',
        KeyCode::Right => 'l',
        KeyCode::Left => 'h',
        _ => match key_char(key) {
            Some(c) => c,
            None => return,
        },
    };
    let out = reduce_control_center(&states.control_center, ControlCenterEvent::Key(c));
    states.control_center = out.state;
    if let Some(ControlCenterIntent::Answer {
        attention_id,
        answer,
    }) = out.intent
    {
        states.pending_answer_action = Some(AttentionAnswerAction {
            attention_id,
            answer,
        });
    }
}

/// Squads screen key routing (P7 / D17): fold the key into the pure reducer,
/// lifting a [`SquadsIntent`] into a deferred [`SquadAction`] the `render` pass
/// fires over the daemon socket (the sync key router can't `await`).
///
/// Esc cancels an open create input; every other printable key (incl. Enter /
/// Backspace while creating) folds into the reducer. The create/add/assign
/// selection policy (which leader / member / issue) is resolved by the plugin glue
/// from its cached agents/issues, so the lifted action carries only ids.
fn route_squads(states: &mut ScreenStates, key: &KeyEvent) {
    let ev = if matches!(key.code, KeyCode::Esc) {
        SquadsEvent::Esc
    } else if let Some(c) = key_char(key) {
        SquadsEvent::Key(c)
    } else {
        return;
    };
    let out = reduce_squads(&states.squads, ev);
    states.squads = out.state;
    states.pending_squads_action = match out.intent {
        Some(SquadsIntent::CreateSquad { name }) => Some(SquadAction::Create { name }),
        Some(SquadsIntent::AddMember { squad_id }) => Some(SquadAction::AddMember { squad_id }),
        Some(SquadsIntent::RemoveMember {
            squad_id,
            member_ref,
        }) => Some(SquadAction::RemoveMember {
            squad_id,
            member_ref,
        }),
        Some(SquadsIntent::AssignIssue { squad_id }) => Some(SquadAction::Assign { squad_id }),
        None => None,
    };
}

/// Profile-editor key routing (P5): fold the key into the profile reducer,
/// lifting its intents into deferred daemon RPCs the `render` pass drains — a
/// [`ProfilesIntent::LoadDetail`] into a `profile/get`
/// ([`ScreenStates::pending_profile_detail`]) and a
/// [`ProfilesIntent::CycleTier`] into a `profile/upsert`
/// ([`ScreenStates::pending_profile_upsert`]). The sync key router can't `await`.
fn route_profiles(states: &mut ScreenStates, key: &KeyEvent) {
    let c = match &key.code {
        KeyCode::Down => 'j',
        KeyCode::Up => 'k',
        _ => match key_char(key) {
            Some(c) => c,
            None => return,
        },
    };
    let out = reduce_profiles(&states.profiles, ProfilesEvent::Key(c));
    states.profiles = out.state;
    match out.intent {
        Some(ProfilesIntent::LoadDetail(slug)) => {
            states.pending_profile_detail = Some(slug);
        }
        Some(ProfilesIntent::CycleTier { slug, tier }) => {
            states.pending_profile_upsert = Some((slug, tier));
        }
        None => {}
    }
}

/// Agent-picker key routing: fold the key into the pure reducer, then act on the
/// reduction.
///
/// Enter raises [`AgentPickerIntent::Assign`]; the sync router can't `await`, so
/// the action is lifted into a deferred [`IssueAssignAction::Assign`] on
/// [`ScreenStates::pending_assign_action`] (the `render` pass drains it and fires
/// `hangar/issue_update` over the daemon socket) and the modal is dismissed —
/// pressing Enter assigns the issue *and* closes the picker (e38.8). Esc (or any
/// reducer-closed state) raises [`NavIntent::CloseModal`] with no assign, popping
/// the modal back to its prior screen.
fn route_agent_picker(states: &mut ScreenStates, key: &KeyEvent) -> Option<NavIntent> {
    let picker = states.agent_picker.take()?;
    let ev = match key.code {
        KeyCode::Esc => AgentPickerEvent::Esc,
        _ => AgentPickerEvent::Key(key_char(key)?),
    };
    let out = reduce_agent_picker(&picker, ev);

    // Enter on a picked actor: queue the assign RPC and dismiss the modal.
    if let Some(AgentPickerIntent::Assign {
        issue_id,
        actor_ref,
    }) = out.intent
    {
        states.agent_picker = None;
        states.pending_assign_action = Some(IssueAssignAction::Assign {
            issue_id: issue_id.as_str().to_string(),
            actor_ref,
        });
        return Some(NavIntent::CloseModal);
    }

    // No assign: Esc (or a reducer-closed state) dismisses; anything else stays.
    let closed = out.state.is_closed();
    states.agent_picker = Some(out.state);
    if closed {
        states.agent_picker = None;
        Some(NavIntent::CloseModal)
    } else {
        None
    }
}

/// Command-palette key routing (e38.13): fold the key into the pure reducer, then
/// act on the reduction.
///
/// Up/Down (and vi `j`/`k` are NOT used — every printable char is query text)
/// move the selection; Esc closes; Enter raises [`CommandPaletteIntent::Navigate`]
/// which lifts into a [`NavIntent::NavigateToEntity`] (the glue switches the
/// routing screen + selects the row) and dismisses the modal. A query edit raises
/// [`CommandPaletteIntent::Search`], lifted into a deferred
/// [`PaletteAction::Search`] the `render` pass drains + fires (the sync key router
/// can't `await`).
fn route_command_palette(states: &mut ScreenStates, key: &KeyEvent) -> Option<NavIntent> {
    let palette = states.command_palette.take()?;
    let ev = match key.code {
        KeyCode::Esc => CommandPaletteEvent::Esc,
        KeyCode::Down => CommandPaletteEvent::SelectDown,
        KeyCode::Up => CommandPaletteEvent::SelectUp,
        _ => CommandPaletteEvent::Key(key_char(key)?),
    };
    let out = reduce_command_palette(&palette, ev);

    match out.intent {
        // Enter on a result: jump to its screen and dismiss the modal.
        Some(CommandPaletteIntent::Navigate { screen, id, kind }) => {
            states.command_palette = None;
            return Some(NavIntent::NavigateToEntity { screen, id, kind });
        }
        // A query edit: queue the search RPC, keep the modal open.
        Some(CommandPaletteIntent::Search(query)) => {
            states.pending_palette_action = Some(PaletteAction::Search(query));
        }
        None => {}
    }

    // Esc (or a reducer-closed state) dismisses; anything else stays.
    let closed = out.state.is_closed();
    states.command_palette = Some(out.state);
    if closed {
        states.command_palette = None;
        Some(NavIntent::CloseModal)
    } else {
        None
    }
}
