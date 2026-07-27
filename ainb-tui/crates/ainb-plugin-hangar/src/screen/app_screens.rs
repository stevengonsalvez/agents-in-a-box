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
use super::agents::{AgentsEvent, AgentsIntent, AgentsState, reduce_agents};
use super::autopilots::{AutopilotsEvent, AutopilotsIntent, AutopilotsState, reduce_autopilots};
use super::boards::{BoardsEvent, BoardsIntent, BoardsKey, BoardsState, reduce_boards};
use super::command_palette::{
    CommandPaletteEvent, CommandPaletteIntent, CommandPaletteState, reduce_command_palette,
};
use super::control_center::{
    ControlCenterEvent, ControlCenterIntent, ControlCenterState, reduce_control_center,
};
use super::daemon_health::DaemonHealthState;
use super::fleet::{
    FleetAction, FleetEvent, FleetFilter, FleetIntent, FleetKey, FleetPaneState, reduce_fleet,
    selected_approval_action,
};
use super::inbox::InboxState;
use super::issue_list::{
    IssueListEvent, IssueListIntent, IssueListMode, IssueListState, reduce_issue_list,
};
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
    /// Create a workspace (`n` → name modal → Enter) — `host/workspace_create`,
    /// then auto-switch into it (P-multica#4).
    Create {
        /// The slug derived from the typed name.
        slug: String,
        /// The human-readable workspace name.
        name: String,
    },
    /// Delete a workspace (`x`) — `host/workspace_delete` (P-multica#4).
    Delete(String),
}

/// A deferred daemon RPC raised by the Settings Notifications grid (tcp T5).
///
/// Like [`WorkspaceAction`], the sync key router can't `await`; it stashes the
/// action on [`ScreenStates::pending_notify_action`] and the plugin's `render`
/// pass drains it and fires the matching `hangar/notify_*` RPC over the daemon
/// socket, scoped to the active workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotifyAction {
    /// Fetch the routing grid for `scope` — raised on entry to the Notifications
    /// section, after a rule write, and when `g` flips the scope
    /// (`hangar/notify_rules_list`). `scope` decides whether the RPC carries a
    /// `workspace_id` (agents-in-a-box-cqh).
    Refresh {
        /// The scope to list (global omits `workspace_id`; workspace sends it).
        scope: super::settings::NotifyScope,
    },
    /// Upsert one rule (`space` toggled a cell) — `hangar/notify_rule_set`.
    Set {
        /// The scope to write (global omits `workspace_id`; workspace sends it).
        scope: super::settings::NotifyScope,
        /// The attention kind wire token the rule governs.
        kind: String,
        /// The full new push-channel set for that kind.
        channels: ainb_hangar_proto::ChannelSet,
    },
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
    /// Flip a skill's per-agent enablement (`t`) — `hangar/skill_set_enabled`
    /// (parity #24). The glue derives the target state from the cached link map.
    ToggleEnabled(String),
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
    /// Create a card (issue) from the typed title + picked repo + agent + assignee
    /// profile and place it in a column (`c`) — `hangar/board_card_create`
    /// (spec F1-F4, D16).
    CardCreate {
        /// The board to add the card to.
        board_id: String,
        /// The column to place the card in.
        column_id: String,
        /// The new issue's title.
        title: String,
        /// The picked repo (an absolute checkout path or `scratch`) — REQUIRED (F2).
        repo_ref: String,
        /// The picked provider agent wire token (`claude`/`codex`/`copilot`, F4).
        agent: String,
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
    /// Cancel a card's in-flight run (`X`) — `hangar/board_card_cancel` (tcp T3 /
    /// F6).
    CardCancel {
        /// The board the card sits on.
        board_id: String,
        /// The card's issue whose active run to cancel.
        issue_id: String,
    },
    /// Remove a card from the board (`d`) — `hangar/board_card_remove` (tcp T3 /
    /// F6). Drops the placement; the issue survives.
    CardRemove {
        /// The board the card sits on.
        board_id: String,
        /// The card's issue whose placement to remove.
        issue_id: String,
    },
    /// Reorder a card within its column (`⇧↑/↓`) — `hangar/board_card_reorder`
    /// (tcp T3 / F6).
    CardReorder {
        /// The board the cards sit on.
        board_id: String,
        /// The column being reordered.
        column_id: String,
        /// The cards in their new top-to-bottom order.
        issue_ids: Vec<String>,
    },
    /// Fetch + open a card's prettied JSONL timeline (`t`) —
    /// `hangar/board_card_timeline` (tcp T3 / F6).
    CardTimeline {
        /// The board the card sits on.
        board_id: String,
        /// The card's issue whose run transcript to show.
        issue_id: String,
    },
    /// Edit an existing card's title + repo + agent (`e`) — `hangar/issue_update`
    /// (F6). Rewrites the title and persists repo/agent on the issue so the next
    /// run routes to the chosen provider.
    CardEdit {
        /// The issue whose card to edit.
        issue_id: String,
        /// The new title (non-blank).
        title: String,
        /// The picked repo (an absolute checkout path or `scratch`).
        repo_ref: String,
        /// The picked provider-agent wire token (`claude`/`codex`/`copilot`).
        agent: String,
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
    /// Assign (or clear) a squad as a card's assignee (`s`) —
    /// `hangar/board_card_assign_squad` (tcp T4 / F7).
    CardAssignSquad {
        /// The board the card sits on.
        board_id: String,
        /// The card's issue to (re)assign.
        issue_id: String,
        /// The squad to assign, or `None` to clear.
        squad_id: Option<String>,
    },
    /// Add a depends-on blocker to a card (`d`) — `hangar/board_card_dep_add`
    /// (tcp T4 / F7).
    CardDepAdd {
        /// The board both cards sit on.
        board_id: String,
        /// The FROM card's issue (the DEPENDENT under the default kind).
        dependent_issue_id: String,
        /// The TO card's issue (the BLOCKER under the default kind).
        blocker_issue_id: String,
        /// The link KIND to author (multica parity #20); `BlockedBy` reproduces
        /// the pre-#20 gating edge.
        link_type: ainb_hangar_proto::snapshots::LinkKindWire,
    },
    /// Flip a card's auto-run flag (`R`) — `hangar/board_card_set_auto_run`
    /// (tcp T4 / F7).
    CardSetAutoRun {
        /// The board the card sits on.
        board_id: String,
        /// The card's issue.
        issue_id: String,
        /// The new auto-run value.
        auto_run: bool,
    },
    /// Re-fetch `hangar/boards_list` to force a repaint after a purely-local
    /// overlay change (open / typed keystroke). A local key change arms no daemon
    /// reply of its own, so without this round-trip the overlay never renders
    /// (the host repaints a plugin on a daemon socket event, and a lone key can
    /// race the one dirty-kick before the plugin observes it). The reply preserves
    /// the open overlay via `adopt_context`.
    Refresh,
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

/// A deferred daemon RPC raised by the task-detail acceptance keys (`a` then
/// `t`, multica parity #11-rest).
///
/// Like [`IssueCommentAction`], the sync key router can't `await`; the toggle
/// stashes the action on [`ScreenStates::pending_criterion_action`] and the
/// plugin's `render` pass drains it and fires `hangar/issue_criterion_set` over
/// the daemon socket cap. The daemon's `IssueUpdated` push refreshes the card, so
/// nothing needs re-pulling here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueCriterionAction {
    /// The issue the criterion belongs to (`issue.id`).
    pub issue_id: String,
    /// The STABLE criterion id (`ac-…`).
    pub criterion_id: String,
    /// The checked state to set.
    pub checked: bool,
}

/// A deferred daemon RPC chain raised by the issue-list create WIZARD (Phase 5).
///
/// Like [`IssueCommentAction`], the sync key router can't `await`; the wizard's
/// Agent-stage commit stashes the action on
/// [`ScreenStates::pending_create_action`] and the plugin's `render` pass drains
/// it and fires `hangar/issue_create`; on that reply the plugin fires
/// `hangar/issue_update` (persisting repo / agent / branches on the new issue)
/// and `hangar/issue_run` (the actual dispatch). Every field is collected by the
/// wizard, so a create from this screen ALWAYS carries an agent and a repo — the
/// title-only inert card is unrepresentable here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IssueCreateAction {
    /// Create a new issue AND dispatch it (Enter on the wizard's Agent stage).
    CreateAndRun {
        /// The new issue's title (non-blank).
        title: String,
        /// The multi-line brief (OPTIONAL) persisted as `issue.description` and
        /// turned into the `claude -p` prompt by `build_prompt`. `None` when blank.
        description: Option<String>,
        /// The linked-issue reference (OPTIONAL) persisted as `issue.external_ref`
        /// for traceability and appended to the dispatched brief. `None` when blank.
        external_ref: Option<String>,
        /// The acceptance criteria (OPTIONAL, migration 0048) persisted as
        /// `issue.acceptance_criteria` and rendered on the detail card. Empty when
        /// the wizard's Acceptance row was left blank.
        acceptance_criteria: Vec<String>,
        /// The context references (OPTIONAL, migration 0048) persisted as
        /// `issue.context_refs` and rendered on the detail card. Empty when the
        /// wizard's Context row was left blank.
        context_refs: Vec<String>,
        /// The urgency picked on the wizard's Priority row (migration 0014), on
        /// the wire scale `0..3` (P3..P0, HIGHER = MORE URGENT). `0` (P3) when the
        /// row was left alone.
        priority: i64,
        /// The deadline typed on the wizard's Due row (migration 0014) as epoch ms
        /// at UTC midnight; `None` when the row was blank.
        due_date: Option<i64>,
        /// The label NAMES typed on the wizard's Labels row (migration 0016),
        /// resolve-or-created and joined to the new issue. Empty when blank.
        labels: Vec<String>,
        /// The picked repo (REQUIRED): an absolute path, `scratch`, or a remote
        /// indicator the daemon clones.
        repo_ref: String,
        /// The source branch the run branches FROM; `None` = repo default.
        source_branch: Option<String>,
        /// The target branch a future PR lands INTO; `None` = unset.
        target_branch: Option<String>,
        /// The provider agent wire token (`claude` / `codex` / `copilot`) when the
        /// Agent row fell back to provider chips; `None` when a NAMED agent was
        /// targeted (its own provider drives the run — see [`Self::assignee`]).
        agent: Option<String>,
        /// The NAMED workspace agent targeted by the Agent row as its `agent:<id>`
        /// ref (V3-F3): persisted as the new issue's assignee AND carried as the
        /// run's assignee override. `None` when a provider chip was chosen instead.
        assignee: Option<String>,
        /// 0046 sub-issues: the parent issue's wire id when the wizard was opened
        /// as an "add sub-issue" (`s`), threaded into `hangar/issue_create` so the
        /// daemon links the new issue as a child. `None` for a top-level create.
        parent_issue_id: Option<String>,
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
    /// Create an AGENT named `name` (`n` + Enter) — `hangar/agent_create`; the glue
    /// fires it with no ids and folds the refreshed roster back, clearing the
    /// "no agent available to lead a squad" gate live.
    CreateAgent {
        /// The new agent's name.
        name: String,
    },
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
    /// Set (or clear) `member_ref`'s free-text role on `squad_id` (`r` on a
    /// member row) — `hangar/squad_member_role_set` (parity #25).
    SetMemberRole {
        /// The squad whose membership is edited.
        squad_id: String,
        /// The member actor-ref whose role changes.
        member_ref: String,
        /// The new role; empty clears it.
        role: String,
    },
    /// Set (or clear) `squad_id`'s user-authored instructions (`i`) —
    /// `hangar/squad_instructions_set` (parity #25).
    SetInstructions {
        /// The squad whose instructions change.
        squad_id: String,
        /// The new instructions; empty clears them.
        instructions: String,
    },
    /// Assign the current issue to `squad_id` (`x`) — `hangar/squad_fanout`; the
    /// glue picks the issue and fans the brief to the leader + agent members.
    Assign {
        /// The squad the issue is assigned to.
        squad_id: String,
    },
}

/// A deferred daemon RPC raised by the Agents roster screen (slice 2).
///
/// Like [`SquadAction`], the sync key router can't `await`; it stashes the action
/// on [`ScreenStates::pending_agents_action`] and the plugin's `render` pass drains
/// it and fires the matching agent RPC over the daemon socket. Both replies carry
/// the refreshed `AgentsListResult`, so the roster folds back through the same
/// `set_actors` seam the pickers use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentsAction {
    /// Create an AGENT from the guided wizard (`n` → name → provider → model →
    /// instructions → confirm) — `hangar/agent_create`; the glue fires it with no
    /// ids (the daemon fills workspace / runtime / owner) and carries the collected
    /// provider / model / instructions through so the created row is fully
    /// configured, not just named.
    Create {
        /// The new agent's name.
        name: String,
        /// The optional short blurb (`None` = left blank, migration 0050).
        description: Option<String>,
        /// The chosen provider (`claude`/`codex`/`copilot`).
        provider: String,
        /// The optional per-agent model override (`None` = provider default).
        model: Option<String>,
        /// The optional free-form instructions (`None` = left blank).
        instructions: Option<String>,
    },
    /// Delete `actor_ref` (Enter on the `x` confirm) — `hangar/agent_delete`; the
    /// glue extracts the id and scopes the delete to the workspace.
    Delete {
        /// The agent to delete, in canonical `agent:<id>` form.
        actor_ref: String,
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
    /// Authoritative Fleet registry pane, fed by `fleet/subscribe` and
    /// reconciled from `fleet/snapshot` after stream gaps.
    pub fleet: FleetPaneState,
    /// Squads screen cache (P7 / D17), built from `hangar/squads_list` with each
    /// leader/member resolved against the cached actor snapshot for live status.
    pub squads: SquadsState,
    /// Agents roster screen cache (slice 2), rebuilt from the same cached
    /// `hangar/agents_list` actor snapshot that feeds the pickers (agent actors
    /// only), preserving selection + open create/delete overlays across a refresh.
    pub agents: AgentsState,
    /// Profile-editor screen cache (P5), filled from `profile/list` (roster) +
    /// `profile/get` (the selected profile's detail + both compile previews).
    pub profiles: ProfilesState,
    /// Settings screen cache (built once the four snapshots arrive).
    pub settings: Option<SettingsState>,
    /// Task-detail screen cache (present only while a task is open).
    pub task_detail: Option<TaskDetailState>,
    /// Agent-picker modal cache (present only while the modal is open).
    pub agent_picker: Option<AgentPickerState>,
    /// Activity-timeline modal cache (multica parity #13; present only while the
    /// modal is open).
    pub activity: Option<super::activity::ActivityState>,
    /// An issue id whose `hangar/issue_timeline` fetch is armed, awaiting the
    /// `render` pass to fire it over the daemon socket. `None` when idle.
    pub pending_activity_fetch: Option<String>,
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
    /// A manual task-retry RPC raised by the Task Kanban failed-column `R` or the
    /// task-detail `R`, awaiting the `render` pass to fire `hangar/task_retry` over
    /// the daemon socket. Carries the terminal task id to force-requeue. `None`
    /// when idle. Unlike the automatic retry seam, this is an operator override:
    /// the daemon requeues ANY terminal reason (including `agent_error`, which
    /// never auto-retries).
    pub pending_task_retry_action: Option<String>,
    /// An issue-assign RPC raised by the agent-picker modal (Enter on a picked
    /// actor), awaiting the `render` pass to fire `hangar/issue_update` over the
    /// daemon socket (e38.8). `None` when idle.
    pub pending_assign_action: Option<IssueAssignAction>,
    /// An issue-comment RPC raised by the task-detail compose modal (Enter on a
    /// non-empty buffer), awaiting the `render` pass to fire `hangar/comment_add`
    /// over the daemon socket (e38.5). `None` when idle.
    pub pending_comment_action: Option<IssueCommentAction>,
    /// An acceptance-criterion tick raised by the task-detail `t` key, awaiting
    /// the `render` pass to fire `hangar/issue_criterion_set` over the daemon
    /// socket (multica parity #11-rest). `None` when idle.
    pub pending_criterion_action: Option<IssueCriterionAction>,
    /// A create-and-dispatch chain raised by the issue-list create wizard (Enter
    /// on the Agent stage), awaiting the `render` pass to fire
    /// `hangar/issue_create` (then, on its reply, `issue_update` + `issue_run`)
    /// over the daemon socket (Phase 5). `None` when idle.
    pub pending_create_action: Option<IssueCreateAction>,
    /// A delete raised by the issue-list `x` confirm overlay (Enter on the RED
    /// overlay), awaiting the `render` pass to fire `hangar/issue_delete` over the
    /// daemon socket (63d). Carries the issue to delete. `None` when idle.
    pub pending_delete_action: Option<ainb_hangar_core::ids::IssueId>,
    /// A "cancel run(s) & delete" raised by the issue-list confirm overlay after a
    /// delete was refused for active tasks, awaiting the `render` pass to fire
    /// `hangar/issue_cancel_active` (then, on its reply, retry `hangar/issue_delete`)
    /// over the daemon socket. Carries the issue. `None` when idle.
    pub pending_cancel_delete_action: Option<ainb_hangar_core::ids::IssueId>,
    /// A search RPC raised by the command-palette modal (each keystroke),
    /// awaiting the `render` pass to fire `hangar/search` over the daemon socket
    /// (e38.13). `None` when idle.
    pub pending_palette_action: Option<PaletteAction>,
    /// Cached workspace catalogue from `host/workspace_list` (P5.5). Seeds the
    /// Settings Workspace pane regardless of which snapshot arrives first.
    pub workspace_rows: Vec<WorkspaceRow>,
    /// Cached `creation_disabled` hint from the same `host/workspace_list` reply.
    /// Held beside the rows so a `set_health` rebuild cannot silently un-hide the
    /// new-workspace affordance on a locked-down instance.
    pub workspace_creation_disabled: bool,
    /// Cached member roster from `hangar/members_list` (e38.11). Seeds the
    /// Settings Members pane regardless of which snapshot arrives first.
    pub member_rows: Vec<MemberWireRow>,
    /// Cached notification routing grid from `hangar/notify_rules_list` (tcp T5).
    /// Seeds the Settings Notifications pane regardless of arrival order.
    pub notify_rule_rows: Vec<ainb_hangar_proto::snapshots::NotifyRuleWireRow>,
    /// A deferred notify-rule RPC raised by the Notifications grid (tcp T5),
    /// drained by the `render` pass. `None` when idle.
    pub pending_notify_action: Option<NotifyAction>,
    /// Cached live daemon-config values from `hangar/daemon_config_list` as
    /// `(key, value)` pairs. Seeds the Settings Daemon-section rows regardless of
    /// arrival order and survives a `set_health` rebuild (each is replayed into the
    /// rebuilt state). Empty until the first list reply lands.
    pub daemon_config_cache: Vec<(String, Option<String>)>,
    /// Deferred daemon-config writes raised by Daemon-section edits (bool toggle,
    /// enum cycle, or committed int overlay): the `(key, value)` pairs awaiting the
    /// `render` pass to fire `hangar/daemon_config_set`, in edit order. Empty when
    /// idle.
    ///
    /// A QUEUE, not a slot: the registry generalised this surface from one knob to
    /// N, and keys land faster than render passes. With a single slot, toggling
    /// auto-standup and then cycling the default agent before the next frame
    /// silently dropped the first write while the pane still showed it applied.
    pub pending_daemon_config_set: Vec<(String, String)>,
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
    /// Fleet socket or attach intent raised by the pure Fleet reducer.
    pub pending_fleet_intent: Option<FleetIntent>,
    /// A board mutation RPC raised by the Boards screen (`⇧←/→`, `x`, `n`, `m`),
    /// awaiting the `render` pass to fire the matching `hangar/board_*` over the
    /// daemon socket (P4). `None` when idle.
    pub pending_boards_action: Option<BoardsAction>,
    /// A squad mutation RPC raised by the Squads screen (`c`, `a`, `d`, `x`),
    /// awaiting the `render` pass to fire the matching `hangar/squad_*` over the
    /// daemon socket (P7 / D17). `None` when idle.
    pub pending_squads_action: Option<SquadAction>,
    /// An agent mutation RPC raised by the Agents roster screen (`n` create / `x`
    /// delete), awaiting the `render` pass to fire `hangar/agent_create` or
    /// `hangar/agent_delete` over the daemon socket (slice 2). `None` when idle.
    pub pending_agents_action: Option<AgentsAction>,
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

    /// Inject the `@`-autocomplete repo roster BOTH card-create pickers offer —
    /// the Boards overlay (spec F3) and the Issues create wizard's repo stage
    /// (Phase 5) — from the one cached `hangar/repo_list` (favorites-first +
    /// recency order preserved; the reducers prepend `scratch` always).
    pub fn set_boards_repos(&mut self, repos: Vec<super::boards::RepoOption>) {
        self.issue_list.set_repos(repos.clone());
        self.boards.set_repos(repos);
    }

    /// Inject the agent chip the Boards card-create picker pre-selects (spec F4
    /// cascade).
    pub const fn set_boards_default_agent(&mut self, agent: super::boards::AgentChip) {
        self.boards.set_default_agent(agent);
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
        recipient: String,
    ) {
        self.inbox = InboxState::from_snapshot(entries, unread, recipient);
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
        let creating_agent = self.squads.agent_buffer().map(str::to_string);
        let note = self.squads.note().cloned();
        let mut next = SquadsState::from_snapshot(snapshot, &self.actors);
        next.set_selected(selected);
        next.set_create_buffer(creating);
        next.set_agent_buffer(creating_agent);
        next.set_note(note);
        self.squads = next;
        // tcp T4 / F7: the Boards assign-squad picker draws from the same roster, so
        // feed it the (id, name) options from the fresh snapshot.
        self.boards.set_squads(
            snapshot
                .squads
                .iter()
                .map(|s| super::boards::SquadOption {
                    id: s.id.clone(),
                    name: s.name.clone(),
                })
                .collect(),
        );
    }

    /// Take the pending squad mutation RPC raised by the Squads screen, if any
    /// (P7 / D17).
    pub const fn take_pending_squads_action(&mut self) -> Option<SquadAction> {
        self.pending_squads_action.take()
    }

    /// Take the pending agent mutation RPC raised by the Agents roster screen, if
    /// any (slice 2).
    pub const fn take_pending_agents_action(&mut self) -> Option<AgentsAction> {
        self.pending_agents_action.take()
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
    ///
    /// Also fans the NAMED workspace agents (agent actors, members filtered out)
    /// into the Issues create wizard's Agent-row roster (V3-F3), so the wizard can
    /// TARGET a named agent — the same `hangar/agents_list` snapshot that feeds the
    /// `a` assign picker. An empty roster leaves the wizard on the provider-chip
    /// fallback.
    pub fn set_actors(&mut self, actors: Vec<ActorRow>) {
        let named: Vec<super::issue_list::WizardAgent> = actors
            .iter()
            .filter(|a| a.is_agent)
            .map(|a| super::issue_list::WizardAgent {
                actor_ref: a.actor_ref.clone(),
                label: a.display_name.clone(),
            })
            .collect();
        self.issue_list.set_agents(named);
        // Rebuild the Agents roster screen from the same snapshot (agent actors
        // only), preserving the selection + any open create/delete overlay so a
        // background refresh mid-interaction does not wipe the user's input. A
        // delete-confirm whose agent vanished (this delete landed) is dropped.
        let selected = self.agents.selected_index();
        let creating = self.agents.create_state();
        let confirming = self.agents.confirm_target().map(str::to_string);
        let note = self.agents.note().map(str::to_string);
        let mut next_agents = AgentsState::from_actors(&actors);
        next_agents.set_selected(selected);
        next_agents.set_create_state(creating);
        next_agents.restore_confirm(confirming);
        next_agents.set_note(note);
        self.agents = next_agents;
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
        state.set_workspace_creation_disabled(self.workspace_creation_disabled);
        // Carry any cached member roster into the rebuilt state so the Members
        // pane survives a `set_health` rebuild (mirrors workspace_rows).
        state.set_members(self.member_rows.clone());
        // Carry any cached notification grid too (tcp T5), same rebuild survival.
        state.set_notify_rules(self.notify_rule_rows.clone());
        // Replay the cached daemon-config values so a `set_health` rebuild keeps
        // every live knob rather than snapping the rows back to their defaults.
        for (key, value) in &self.daemon_config_cache {
            state.set_config_value(key, value.clone());
        }
        self.settings = Some(state);
    }

    /// Refresh the Settings Workspace pane from a `host/workspace_list` result
    /// (P5.5). Caches the rows so a later `set_health` rebuild keeps them, and
    /// overlays the live settings state when it already exists.
    pub fn set_workspaces(&mut self, workspaces: Vec<WorkspaceRow>, creation_disabled: bool) {
        self.workspace_rows.clone_from(&workspaces);
        self.workspace_creation_disabled = creation_disabled;
        if let Some(s) = self.settings.as_mut() {
            s.set_workspaces(workspaces);
            s.set_workspace_creation_disabled(creation_disabled);
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

    /// Refresh the Settings Notifications grid from a `hangar/notify_rules_list`
    /// result (tcp T5). Caches the rows so a later `set_health` rebuild keeps
    /// them, and overlays the live settings state when it already exists.
    pub fn set_notify_rules(
        &mut self,
        rules: Vec<ainb_hangar_proto::snapshots::NotifyRuleWireRow>,
    ) {
        self.notify_rule_rows.clone_from(&rules);
        if let Some(s) = self.settings.as_mut() {
            s.set_notify_rules(rules);
        }
    }

    /// Take the pending workspace action raised by the Settings pane, if any.
    pub const fn take_pending_ws_action(&mut self) -> Option<WorkspaceAction> {
        self.pending_ws_action.take()
    }

    /// Take the pending notify-rule RPC raised by the Notifications grid, if any.
    pub const fn take_pending_notify_action(&mut self) -> Option<NotifyAction> {
        self.pending_notify_action.take()
    }

    /// Apply a full `hangar/daemon_config_list` result: cache every `(key, value)`
    /// so a later `set_health` rebuild keeps them, and overlay the live settings
    /// state when it already exists (mirrors [`Self::set_notify_rules`]).
    pub fn set_daemon_config_entries(&mut self, entries: Vec<(String, Option<String>)>) {
        self.daemon_config_cache.clone_from(&entries);
        if let Some(s) = self.settings.as_mut() {
            for (key, value) in entries {
                s.set_config_value(&key, value);
            }
        }
    }

    /// Drain every pending daemon-config write raised by Daemon-section edits, in
    /// edit order. Returns an empty vec when idle.
    pub fn take_pending_daemon_config_sets(&mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.pending_daemon_config_set)
    }

    /// The Notifications grid's current edit scope (global/workspace,
    /// agents-in-a-box-cqh), or `Global` when the settings screen has never been
    /// opened. The post-write re-fetch reads it so the grid re-lists the SAME
    /// scope the user just wrote to.
    #[must_use]
    pub fn notify_scope(&self) -> super::settings::NotifyScope {
        self.settings
            .as_ref()
            .map_or(super::settings::NotifyScope::Global, |s| s.notify_scope())
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

    /// Take the pending manual task-retry RPC raised by the Kanban / task-detail
    /// `R`, if any.
    pub const fn take_pending_task_retry_action(&mut self) -> Option<String> {
        self.pending_task_retry_action.take()
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

    /// Take the deferred acceptance-criterion tick, leaving `None` (#11-rest).
    pub const fn take_pending_criterion_action(&mut self) -> Option<IssueCriterionAction> {
        self.pending_criterion_action.take()
    }

    /// Take the pending issue-create RPC raised by the issue-list inline create
    /// flow, if any (e38.29).
    pub const fn take_pending_create_action(&mut self) -> Option<IssueCreateAction> {
        self.pending_create_action.take()
    }

    /// Take the pending issue delete raised by the `x` confirm overlay, if any
    /// (63d). The `render` pass drains it and fires `hangar/issue_delete`.
    pub const fn take_pending_delete_action(&mut self) -> Option<ainb_hangar_core::ids::IssueId> {
        self.pending_delete_action.take()
    }

    /// Take the pending "cancel run(s) & delete" raised by the issue-list confirm
    /// overlay, if any. The `render` pass drains it and fires
    /// `hangar/issue_cancel_active`.
    pub const fn take_pending_cancel_delete_action(
        &mut self,
    ) -> Option<ainb_hangar_core::ids::IssueId> {
        self.pending_cancel_delete_action.take()
    }

    /// Take the pending search RPC raised by the command palette, if any
    /// (e38.13).
    pub const fn take_pending_palette_action(&mut self) -> Option<PaletteAction> {
        self.pending_palette_action.take()
    }

    /// Take one deferred Fleet action, broadcast, or attach intent.
    pub const fn take_pending_fleet_intent(&mut self) -> Option<FleetIntent> {
        self.pending_fleet_intent.take()
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

    /// Open task detail for `issue`'s task, seeding from the issue's row and the
    /// run's `branch` (tcp T2, agents-in-a-box-ch3) when the opening card carries
    /// one — the detail view renders it as a branch line under the PR badge. Pass
    /// `None` when there is no per-run branch (e.g. opened from the issue list).
    pub fn open_task_detail(
        &mut self,
        task_id: ainb_hangar_core::ids::TaskId,
        issue: IssueRow,
        branch: Option<String>,
    ) {
        let mut td = TaskDetailState::new(task_id, issue);
        td.set_branch(branch);
        self.task_detail = Some(td);
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
    /// Open the activity-timeline modal for an issue (raised by `y`, multica
    /// parity #13).
    OpenActivityTimeline(ainb_hangar_core::ids::IssueId),
    /// Open task detail for the issue under the selection (raised by Enter).
    OpenTaskForIssue(ainb_hangar_core::ids::IssueId),
    /// 0046 sub-issues: mark an issue Done from the keyboard (`d`). The glue moves
    /// the card optimistically and arms the durable `hangar/issue_update{state}`
    /// RPC (the SAME seam as the context-menu `Move to ▸ Done`), so a `d` on a
    /// sub-issue fires the child-done cascade. Carries the target issue id.
    MarkIssueDone(ainb_hangar_core::ids::IssueId),
    /// Open the task's captured PR URL in the host browser (raised by `o` on the
    /// task-detail screen, P9.2). Only surfaced when the task has a `pr_url` — `o`
    /// is a no-op (no intent) when none, so there is never a silent open of
    /// nothing.
    OpenPrUrl(String),
    /// Close the active modal back to its prior screen (raised by Esc on a modal).
    CloseModal,
    /// Switch to the issue-list tab (raised by a confirmed task-detail `x` delete,
    /// 63l.5) so the deleted card's screen doesn't linger after the row is gone.
    BackToIssueList,
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
        Screen::Fleet => {
            super::fleet::render_fleet(buf, w, top, bottom, &states.fleet);
        }
        Screen::Squads => {
            super::squads::render_squads(buf, w, top, bottom, &states.squads);
        }
        Screen::Profiles => {
            super::profiles::render_profiles(buf, w, top, bottom, &states.profiles);
        }
        Screen::Agents => {
            super::agents::render_agents(buf, w, top, bottom, &states.agents);
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
        Screen::ActivityTimeline(_) => {
            // The timeline is a modal: paint the screen it overlays first, then
            // the modal centred over the whole area (multica parity #13).
            if let Some(prior) = &app.prior_screen {
                render_prior(buf, w, h, prior, states);
            }
            if let Some(activity) = &states.activity {
                super::activity::render_activity(buf, w, h, activity);
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
                    Some(SkillManagerIntent::ToggleEnabled(slug)) => {
                        Some(SkillAction::ToggleEnabled(slug))
                    }
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
                } else if matches!(key.code, KeyCode::Up) {
                    SettingsEvent::CursorUp
                } else if matches!(key.code, KeyCode::Down) {
                    SettingsEvent::CursorDown
                } else if let Some(c) = key_char(key) {
                    SettingsEvent::Key(c)
                } else {
                    states.settings = Some(s);
                    return None;
                };
                let out = reduce_settings(&s, ev);
                let section = out.state.section();
                let now_on_workspaces = section == super::settings::SettingsSection::Workspaces;
                let now_on_notifications =
                    section == super::settings::SettingsSection::Notifications;
                // Capture the grid's edit scope before the state is moved back —
                // every notify RPC is scoped to it (agents-in-a-box-cqh).
                let notify_scope = out.state.notify_scope();
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
                    // `n` name modal confirmed → create the workspace + auto-switch.
                    Some(SettingsIntent::CreateWorkspace { slug, name }) => {
                        states.pending_ws_action = Some(WorkspaceAction::Create { slug, name });
                    }
                    // `x` on a non-active row → delete the workspace.
                    Some(SettingsIntent::DeleteWorkspace(id)) => {
                        states.pending_ws_action = Some(WorkspaceAction::Delete(id));
                    }
                    // A toggled routing cell fires a `hangar/notify_rule_set`
                    // scoped to the grid's current global/workspace scope (tcp T5,
                    // agents-in-a-box-cqh).
                    Some(SettingsIntent::SetNotifyRule { kind, channels }) => {
                        states.pending_notify_action = Some(NotifyAction::Set {
                            scope: notify_scope,
                            kind,
                            channels,
                        });
                    }
                    // `g` flipped the scope: re-list the rules for the new scope so
                    // the grid shows what the human is now editing.
                    Some(SettingsIntent::RefreshNotifyRules) => {
                        states.pending_notify_action = Some(NotifyAction::Refresh {
                            scope: notify_scope,
                        });
                    }
                    // A Daemon-section edit (bool/enum/int) asked to persist one
                    // knob: defer the `hangar/daemon_config_set` write for the
                    // render pass, which re-reads the whole config afterwards.
                    Some(SettingsIntent::SetDaemonConfig { key, value }) => {
                        states.pending_daemon_config_set.push((key, value));
                    }
                    // KeychainWrite / Rename land in their own beads.
                    _ => {
                        // Seed the pane from the live host workspace list the
                        // first time the user lands on the Workspace section.
                        if now_on_workspaces && states.workspace_rows.is_empty() {
                            states.pending_ws_action = Some(WorkspaceAction::Refresh);
                        }
                        // Likewise, fetch the routing grid on first entry to the
                        // Notifications section (tcp T5), scoped to the default
                        // (global) scope.
                        if now_on_notifications && states.notify_rule_rows.is_empty() {
                            states.pending_notify_action = Some(NotifyAction::Refresh {
                                scope: notify_scope,
                            });
                        }
                    }
                }
            }
            None
        }
        Screen::TaskDetail(_) => route_task_detail(states, key),
        Screen::AgentPicker(_) => route_agent_picker(states, key),
        Screen::ActivityTimeline(_) => route_activity(states, key),
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
        Screen::Fleet => {
            route_fleet(states, key);
            None
        }
        Screen::Squads => {
            route_squads(states, key);
            None
        }
        Screen::Agents => {
            route_agents(states, key);
            None
        }
        Screen::Profiles => {
            route_profiles(states, key);
            None
        }
        // Read-only / overlay screens with no per-screen keys: the daemon-health
        // pane (P8.5), the usage dashboard (e38.35), and the help overlay (the
        // `d`/`U`/`?` tab-switch + global keys are handled by the router before
        // reaching here).
        Screen::DaemonHealth | Screen::Usage | Screen::Help => None,
    }
}

/// Fleet pane key routing. Filters and lifecycle verbs lift into the same pure
/// reducer as navigation, broadcast, attach, and confirmation modal keys.
fn route_fleet(states: &mut ScreenStates, key: &KeyEvent) {
    let event = if states.fleet.is_modal_open() {
        fleet_key(key).map(FleetEvent::Key)
    } else {
        match &key.code {
            KeyCode::Char { ch: '1' } => Some(FleetEvent::SetFilter(FleetFilter::NeedsInput)),
            KeyCode::Char { ch: '2' } => Some(FleetEvent::SetFilter(FleetFilter::Idle)),
            KeyCode::Char { ch: '3' } => Some(FleetEvent::SetFilter(FleetFilter::Completed)),
            KeyCode::Char { ch: '4' } => Some(FleetEvent::SetFilter(FleetFilter::Running)),
            KeyCode::Char { ch: '5' } => Some(FleetEvent::SetFilter(FleetFilter::All)),
            KeyCode::Char { ch: 's' } => Some(FleetEvent::RequestAction(FleetAction::Stop)),
            KeyCode::Char { ch: 'r' } => Some(FleetEvent::RequestAction(FleetAction::Restart)),
            KeyCode::Char { ch: 'i' } => Some(FleetEvent::RequestAction(FleetAction::Interrupt)),
            KeyCode::Char { ch: 'n' } => {
                Some(approval_event(&states.fleet, false, FleetAction::Continue))
            }
            KeyCode::Char { ch: 'y' } => {
                Some(approval_event(&states.fleet, true, FleetAction::Retry))
            }
            KeyCode::Char { ch: '!' } => Some(FleetEvent::RequestAction(FleetAction::Kill)),
            KeyCode::Char { ch: '#' } => Some(FleetEvent::RequestAction(FleetAction::Archive)),
            _ => fleet_key(key).map(FleetEvent::Key),
        }
    };
    let Some(event) = event else {
        return;
    };
    let out = reduce_fleet(&states.fleet, event);
    states.fleet = out.state;
    if out.intent.is_some() {
        states.pending_fleet_intent = out.intent;
    }
}

fn approval_event(state: &FleetPaneState, approve: bool, fallback: FleetAction) -> FleetEvent {
    let is_approval = state
        .selected_session()
        .is_some_and(|row| row.attention_state.eq_ignore_ascii_case("APPROVAL"));
    if !is_approval {
        return FleetEvent::RequestAction(fallback);
    }
    match selected_approval_action(state, approve) {
        Ok(action) => FleetEvent::RequestAction(action),
        Err(detail) => FleetEvent::Feedback(detail),
    }
}

fn fleet_key(key: &KeyEvent) -> Option<FleetKey> {
    match &key.code {
        KeyCode::Char { ch: ' ' } => Some(FleetKey::Space),
        KeyCode::Char { ch } => Some(FleetKey::Char(*ch)),
        KeyCode::Enter => Some(FleetKey::Enter),
        KeyCode::Esc => Some(FleetKey::Esc),
        KeyCode::Backspace => Some(FleetKey::Backspace),
        KeyCode::Up => Some(FleetKey::Up),
        KeyCode::Down => Some(FleetKey::Down),
        KeyCode::Left => Some(FleetKey::Left),
        KeyCode::Right => Some(FleetKey::Right),
        _ => None,
    }
}

/// Issue-list key routing: fold into the reducer, lifting the open-task /
/// open-picker intents into [`NavIntent`]s (the routing screen lives on the
/// plugin's [`AppState`], not in the issue-list reducer).
fn route_issue_list(states: &mut ScreenStates, key: &KeyEvent) -> Option<NavIntent> {
    // Phase 5: while the create wizard is open, feed it the STRUCTURED key
    // vocabulary — its picker stages (repo dropdown, agent chips) need Up/Down,
    // and Esc must cancel the whole overlay — which the plain-char path can't
    // carry. Any other key is an unmodelled no-op.
    let ev = if states.issue_list.wizard().is_some() {
        let k = match &key.code {
            KeyCode::Char { ch } => super::issue_list::WizardKey::Char(*ch),
            KeyCode::Enter => super::issue_list::WizardKey::Enter,
            KeyCode::Backspace => super::issue_list::WizardKey::Backspace,
            KeyCode::Esc => super::issue_list::WizardKey::Esc,
            KeyCode::Up => super::issue_list::WizardKey::Up,
            KeyCode::Down => super::issue_list::WizardKey::Down,
            KeyCode::Left => super::issue_list::WizardKey::Left,
            KeyCode::Right => super::issue_list::WizardKey::Right,
            KeyCode::Tab => super::issue_list::WizardKey::Tab,
            KeyCode::BackTab => super::issue_list::WizardKey::BackTab,
            _ => return None,
        };
        IssueListEvent::Wizard(k)
    } else if states.issue_list.mode() == IssueListMode::FilterPanel {
        // multica-gap #10: the `f` facet panel needs the STRUCTURED key
        // vocabulary — arrows to navigate, Space/Enter to toggle — which the
        // plain-char path can't carry. Every key is captured (the panel is modal);
        // Esc is intercepted upstream by the capture guard (`abort_filter_panel`).
        let k = match &key.code {
            KeyCode::Up => super::issue_list::PanelKey::Up,
            KeyCode::Down => super::issue_list::PanelKey::Down,
            KeyCode::Left | KeyCode::BackTab => super::issue_list::PanelKey::Left,
            KeyCode::Right | KeyCode::Tab => super::issue_list::PanelKey::Right,
            KeyCode::Enter => super::issue_list::PanelKey::Toggle,
            KeyCode::Esc => super::issue_list::PanelKey::Close,
            KeyCode::Char { ch } => match ch {
                ' ' => super::issue_list::PanelKey::Toggle,
                'k' => super::issue_list::PanelKey::Up,
                'j' => super::issue_list::PanelKey::Down,
                'h' => super::issue_list::PanelKey::Left,
                'l' => super::issue_list::PanelKey::Right,
                'C' => super::issue_list::PanelKey::Clear,
                'f' => super::issue_list::PanelKey::Close,
                // Any other char is swallowed — the panel never leaks a key to the
                // board underneath.
                _ => return None,
            },
            _ => return None,
        };
        IssueListEvent::Panel(k)
    } else {
        // Normal-mode Tab / Shift+Tab cycle the filter-chip bar
        // (All → Members → Agents → Mine → All). Guarded on Normal mode so the
        // binding never hijacks Tab while the `/` filter-query input or a confirm
        // overlay is focused; those still fall through to the plain-char path.
        match &key.code {
            KeyCode::Tab if states.issue_list.mode() == IssueListMode::Normal => {
                IssueListEvent::SetFilter(states.issue_list.filter().next())
            }
            KeyCode::BackTab if states.issue_list.mode() == IssueListMode::Normal => {
                IssueListEvent::SetFilter(states.issue_list.filter().prev())
            }
            _ => IssueListEvent::Key(key_char(key)?),
        }
    };
    let out = reduce_issue_list(&states.issue_list, ev);
    states.issue_list = out.state;
    match out.intent {
        Some(IssueListIntent::OpenAgentPicker(id)) => Some(NavIntent::OpenAgentPicker(id)),
        // multica parity #13: `y` opens the card's activity timeline.
        Some(IssueListIntent::OpenActivityTimeline(id)) => {
            Some(NavIntent::OpenActivityTimeline(id))
        }
        Some(IssueListIntent::OpenTaskDetail(id)) => Some(NavIntent::OpenTaskForIssue(id)),
        // 0046: `d` marks the highlighted issue Done, surfaced as a NavIntent the
        // glue lifts into the SAME optimistic-move + `hangar/issue_update{state}`
        // seam the context-menu `Move to ▸ Done` uses, firing the child cascade.
        Some(IssueListIntent::MarkDone(id)) => Some(NavIntent::MarkIssueDone(id)),
        // Phase 5: the wizard's Agent-stage commit lifts into a deferred
        // create-and-dispatch chain the `render` pass drains + fires (the sync
        // key router can't `await`).
        Some(IssueListIntent::CreateAndRun {
            title,
            brief,
            external_ref,
            acceptance_criteria,
            context_refs,
            priority,
            due_date,
            labels,
            repo_ref,
            source_branch,
            target_branch,
            agent,
            assignee,
            parent_issue_id,
        }) => {
            states.pending_create_action = Some(IssueCreateAction::CreateAndRun {
                title,
                description: brief,
                external_ref,
                acceptance_criteria,
                context_refs,
                priority,
                due_date,
                labels,
                repo_ref,
                source_branch,
                target_branch,
                agent,
                assignee,
                parent_issue_id,
            });
            None
        }
        // 63d: Enter on the `x` RED confirm overlay lifts into a deferred
        // `hangar/issue_delete` the `render` pass drains + fires (the sync key
        // router can't `await`). The daemon's IssueDeleted push then drops the row.
        Some(IssueListIntent::DeleteIssue(id)) => {
            states.pending_delete_action = Some(id);
            None
        }
        // Confirming the "cancel run(s) & delete" overlay lifts into a deferred
        // `hangar/issue_cancel_active` the `render` pass drains + fires; on its
        // reply the plugin retries the delete (cancel commits before delete).
        Some(IssueListIntent::CancelAndDeleteIssue(id)) => {
            states.pending_cancel_delete_action = Some(id);
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
    // `render` pass drains + fires (the sync key router can't `await`). The retry
    // intent lifts into a deferred `hangar/task_retry`; the cancel intent still
    // folds as before.
    let mut nav = None;
    match &out.intent {
        Some(TaskDetailIntent::AddComment { issue_id, body }) => {
            states.pending_comment_action = Some(IssueCommentAction::Add {
                issue_id: issue_id.as_str().to_string(),
                body: body.clone(),
            });
        }
        // #11-rest: `t` on the selected criterion lifts a deferred
        // `hangar/issue_criterion_set` the `render` pass fires. The daemon's
        // IssueUpdated push refreshes the card, so no re-pull is armed here.
        Some(TaskDetailIntent::SetCriterionChecked {
            issue_id,
            criterion_id,
            checked,
        }) => {
            states.pending_criterion_action = Some(IssueCriterionAction {
                issue_id: issue_id.as_str().to_string(),
                criterion_id: criterion_id.clone(),
                checked: *checked,
            });
        }
        // The `R` retry on a terminal task lifts into a deferred `hangar/task_retry`
        // the `render` pass fires — a HUMAN override that force-requeues ANY
        // terminal reason (the daemon bypasses the auto-retry disposition gate). The
        // freshly-queued attempt then surfaces on the board via the TaskQueued push.
        Some(TaskDetailIntent::RetryTask(task_id)) => {
            states.pending_task_retry_action = Some(task_id.as_str().to_string());
        }
        // 63l.5: confirmed `x` delete arms the SAME deferred `hangar/issue_delete`
        // the issue-list `x` uses, then navigates back to the issue list so the
        // card is gone once the daemon's `IssueDeleted` push lands. A daemon
        // rejection (active tasks) surfaces as an issue-list note there.
        Some(TaskDetailIntent::DeleteIssue(issue_id)) => {
            states.pending_delete_action = Some(issue_id.clone());
            nav = Some(NavIntent::BackToIssueList);
        }
        _ => {}
    }
    states.task_detail = Some(out.state);
    nav
}

/// Kanban board key routing (P8.4): map the arrow keys (plus Shift) into the
/// board reducer. `←/→/↑/↓` move focus; `Shift+←/→` drag the focused card and
/// lift the resulting [`KanbanIntent::MoveCard`] into a deferred
/// `hangar/task_transition` RPC (the sync key router can't `await`; the `render`
/// pass drains `pending_kanban_action` and fires it). `h/j/k/l` mirror the arrows
/// for vi-style navigation.
fn route_kanban(states: &mut ScreenStates, key: &KeyEvent) {
    // `R` force-requeues the focused card when it is terminal (the failed /
    // cancelled columns). A HUMAN override: it lifts a deferred `hangar/task_retry`
    // the `render` pass fires, which the daemon force-requeues regardless of the
    // auto-retry disposition (so a terminal `agent_error` — which never
    // auto-retries — still gets a fresh attempt). A non-terminal focused card (or
    // an empty column) is a no-op. Intercepted before the focus/drag reducer so it
    // never folds into navigation.
    if key.code == (KeyCode::Char { ch: 'R' }) {
        if let Some(card) = states.kanban.focused_card() {
            if matches!(card.status.as_str(), "failed" | "cancelled") {
                states.pending_task_retry_action = Some(card.task_id.clone());
            }
        }
        return;
    }
    let Some(ev) = kanban_nav_event(key) else {
        return;
    };
    let out = reduce_kanban(&states.kanban, ev);
    states.kanban = out.state;
    if let Some(KanbanIntent::MoveCard { task_id, to_status }) = out.intent {
        states.pending_kanban_action = Some(KanbanAction::MoveCard { task_id, to_status });
    }
}

/// Map a raw key to a Kanban navigation / drag event.
///
/// Every char bound here must be free of
/// [`is_reserved_key`](crate::screen::router::is_reserved_key) — a reserved char
/// never reaches this mapper (#450).
fn kanban_nav_event(key: &KeyEvent) -> Option<KanbanEvent> {
    let shift = key.mods & ainb_plugin_sdk::KEY_MOD_SHIFT != 0;
    match &key.code {
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
        // vi-style fallbacks: `<`/`>` drag a card, `h`/`j`/`k`/`l` navigate. The
        // old `H`/`L` drag pair was dead — `H` is swallowed by the HOST help
        // toggle and `L` by the plugin router's Logs tab (#450).
        KeyCode::Char { ch } => match ch {
            'h' => Some(KanbanEvent::FocusLeft),
            'l' => Some(KanbanEvent::FocusRight),
            'k' => Some(KanbanEvent::FocusUp),
            'j' => Some(KanbanEvent::FocusDown),
            '<' => Some(KanbanEvent::MoveCardLeft),
            '>' => Some(KanbanEvent::MoveCardRight),
            _ => None,
        },
        _ => None,
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
/// switch boards; `⇧←/→` (and `<`/`>`) reorder; `x` deletes a column; `n` appends
/// one; `m` toggles auto-move; `c` opens card-create; `r` opens column-rename;
/// `Enter` opens the card's `Run ▾`; `a` attaches to the card's run; `s` opens the
/// squad picker; `w` opens the depends-on picker.
///
/// Every char bound here must be free of
/// [`is_reserved_key`](crate::screen::router::is_reserved_key) — a reserved char
/// never reaches this mapper (#450).
fn route_boards(states: &mut ScreenStates, key: &KeyEvent) {
    // The timeline overlay (`t`) captures keys locally — a read-only scroll view
    // that never routes to the reducer (its content is an IO-derived side-cache).
    if states.boards.timeline().is_some() {
        route_timeline_key(states, key);
        return;
    }
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
    // A committed intent lifts to its RPC; otherwise, while an overlay is open,
    // request a boards refresh so the local overlay change (open / typed key)
    // actually repaints — a lone key arms no daemon reply of its own.
    states.pending_boards_action = match lift_boards_intent(out.intent) {
        Some(action) => Some(action),
        None if states.boards.overlay().is_some() => Some(BoardsAction::Refresh),
        None => None,
    };
}

/// Handle a key while the prettied-JSONL timeline overlay is open (tcp T3 / F6):
/// `Esc` closes it, `j`/`↓` and `k`/`↑` scroll. A read-only local view mutation —
/// no reducer, no RPC.
fn route_timeline_key(states: &mut ScreenStates, key: &KeyEvent) {
    match &key.code {
        KeyCode::Esc => states.boards.close_timeline(),
        KeyCode::Up => states.boards.scroll_timeline(-1),
        KeyCode::Down => states.boards.scroll_timeline(1),
        KeyCode::Char { ch: 'k' } => states.boards.scroll_timeline(-1),
        KeyCode::Char { ch: 'j' } => states.boards.scroll_timeline(1),
        _ => {}
    }
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
        // Overlay-local only: the dep picker cycles its link kind (multica parity
        // #20). No global binding is added, so no host-reserved key is touched.
        KeyCode::Tab => BoardsKey::Tab,
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
        KeyCode::Up => Some(if shift {
            BoardsEvent::ReorderCardUp
        } else {
            BoardsEvent::FocusUp
        }),
        KeyCode::Down => Some(if shift {
            BoardsEvent::ReorderCardDown
        } else {
            BoardsEvent::FocusDown
        }),
        KeyCode::Enter => Some(BoardsEvent::RunFocusedCard),
        KeyCode::Char { ch } => match ch {
            'h' => Some(BoardsEvent::FocusLeft),
            'l' => Some(BoardsEvent::FocusRight),
            'k' => Some(BoardsEvent::FocusUp),
            'j' => Some(BoardsEvent::FocusDown),
            // `<`/`>` reorder the focused column (the `⇧←→` chords still work).
            // The old `H`/`L` pair was dead: `H` is swallowed by the HOST help
            // toggle and `L` by the plugin router's Logs tab (#450).
            '<' => Some(BoardsEvent::ReorderColumnLeft),
            '>' => Some(BoardsEvent::ReorderColumnRight),
            '[' => Some(BoardsEvent::PrevBoard),
            ']' => Some(BoardsEvent::NextBoard),
            'b' => Some(BoardsEvent::CreateBoard),
            'a' => Some(BoardsEvent::AttachFocusedCard),
            // `X` cancels a running card — case-paired with task-detail's `X`
            // cancel (uppercase avoids the lowercase `x` = delete-column binding,
            // and unlike `C` it is not a global tab shortcut).
            'X' => Some(BoardsEvent::CancelFocusedCard),
            // `d` removes the focused card from the board (uppercase-free, distinct
            // from lowercase `x` = delete-column); it opens a confirm overlay.
            'd' => Some(BoardsEvent::RemoveFocusedCard),
            // `t` opens the focused card's prettied JSONL run timeline.
            't' => Some(BoardsEvent::ShowTimeline),
            // `e` edits the focused card (title + repo + agent) — reuses the
            // create overlay, prefilled, committing as an `issue_update` (F6).
            'e' => Some(BoardsEvent::EditFocusedCard),
            'n' => Some(BoardsEvent::AddColumn),
            'r' => Some(BoardsEvent::RenameColumn),
            'x' => Some(BoardsEvent::DeleteColumn),
            'c' => Some(BoardsEvent::AddCard),
            'm' => Some(BoardsEvent::ToggleAutoMove),
            // `s` assigns a SQUAD to the focused card (tcp T4 / F7) — opens a
            // picker. It was `q` until #450: bare `q` is the global quit key, so
            // the binding was dead and the advertised `q:squad` hint popped the
            // whole panel instead of opening the picker.
            's' => Some(BoardsEvent::AssignSquad),
            // `w` ("waits-on") adds a depends-on blocker. Was `D` until #450, which
            // the router claims as the daemon-health tab.
            'w' => Some(BoardsEvent::AddDependency),
            // `R` (uppercase, distinct from `r` = rename) toggles the auto-run flag.
            'R' => Some(BoardsEvent::ToggleAutoRun),
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
        BoardsIntent::AssignSquad {
            board_id,
            issue_id,
            squad_id,
        } => Some(BoardsAction::CardAssignSquad {
            board_id,
            issue_id,
            squad_id,
        }),
        BoardsIntent::AddDependency {
            board_id,
            dependent_issue_id,
            blocker_issue_id,
            link_type,
        } => Some(BoardsAction::CardDepAdd {
            board_id,
            dependent_issue_id,
            blocker_issue_id,
            link_type: link_type.to_wire(),
        }),
        BoardsIntent::ToggleAutoRun {
            board_id,
            issue_id,
            auto_run,
        } => Some(BoardsAction::CardSetAutoRun {
            board_id,
            issue_id,
            auto_run,
        }),
        BoardsIntent::CreateCard {
            board_id,
            column_id,
            title,
            repo_ref,
            agent,
            assignee_profile,
        } => Some(BoardsAction::CardCreate {
            board_id,
            column_id,
            title,
            repo_ref,
            agent: agent.wire().to_string(),
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
        BoardsIntent::AttachCard { board_id, issue_id } => {
            Some(BoardsAction::CardAttach { board_id, issue_id })
        }
        BoardsIntent::CancelCard { board_id, issue_id } => {
            Some(BoardsAction::CardCancel { board_id, issue_id })
        }
        BoardsIntent::RemoveCard { board_id, issue_id } => {
            Some(BoardsAction::CardRemove { board_id, issue_id })
        }
        BoardsIntent::ReorderCards {
            board_id,
            column_id,
            issue_ids,
        } => Some(BoardsAction::CardReorder {
            board_id,
            column_id,
            issue_ids,
        }),
        BoardsIntent::ShowTimeline { board_id, issue_id } => {
            Some(BoardsAction::CardTimeline { board_id, issue_id })
        }
        BoardsIntent::EditCard {
            issue_id,
            title,
            repo_ref,
            agent,
        } => Some(BoardsAction::CardEdit {
            issue_id,
            title,
            repo_ref,
            agent: agent.wire().to_string(),
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
        Some(SquadsIntent::CreateAgent { name }) => Some(SquadAction::CreateAgent { name }),
        Some(SquadsIntent::CreateSquad { name }) => Some(SquadAction::Create { name }),
        Some(SquadsIntent::AddMember { squad_id }) => Some(SquadAction::AddMember { squad_id }),
        Some(SquadsIntent::RemoveMember {
            squad_id,
            member_ref,
        }) => Some(SquadAction::RemoveMember {
            squad_id,
            member_ref,
        }),
        Some(SquadsIntent::SetMemberRole {
            squad_id,
            member_ref,
            role,
        }) => Some(SquadAction::SetMemberRole {
            squad_id,
            member_ref,
            role,
        }),
        Some(SquadsIntent::SetInstructions {
            squad_id,
            instructions,
        }) => Some(SquadAction::SetInstructions {
            squad_id,
            instructions,
        }),
        Some(SquadsIntent::AssignIssue { squad_id }) => Some(SquadAction::Assign { squad_id }),
        None => None,
    };
}

/// Agents roster key routing (slice 2): fold the key into the pure reducer,
/// lifting an [`AgentsIntent`] into a deferred [`AgentsAction`] the `render` pass
/// fires over the daemon socket (the sync key router can't `await`).
///
/// Esc cancels an open create/delete overlay; every other printable key (incl.
/// Enter / Backspace while an overlay is open) folds into the reducer. `↑`/`↓`
/// mirror `k`/`j` for roster navigation.
fn route_agents(states: &mut ScreenStates, key: &KeyEvent) {
    let ev = if matches!(key.code, KeyCode::Esc) {
        AgentsEvent::Esc
    } else if matches!(key.code, KeyCode::Up) {
        AgentsEvent::Key('k')
    } else if matches!(key.code, KeyCode::Down) {
        AgentsEvent::Key('j')
    } else if matches!(key.code, KeyCode::Left) {
        // `←`/`→` drive the create wizard's provider picker (mapped to `h`/`l`).
        AgentsEvent::Key('h')
    } else if matches!(key.code, KeyCode::Right) {
        AgentsEvent::Key('l')
    } else if let Some(c) = key_char(key) {
        AgentsEvent::Key(c)
    } else {
        return;
    };
    let out = reduce_agents(&states.agents, ev);
    states.agents = out.state;
    states.pending_agents_action = match out.intent {
        Some(AgentsIntent::CreateAgent {
            name,
            description,
            provider,
            model,
            instructions,
        }) => Some(AgentsAction::Create {
            name,
            description,
            provider,
            model,
            instructions,
        }),
        Some(AgentsIntent::DeleteAgent { actor_ref }) => Some(AgentsAction::Delete { actor_ref }),
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

/// Activity-timeline key routing (multica parity #13): fold the key into the
/// pure reducer, then act on the reduction.
///
/// `j`/`k` scroll, `r` arms a re-fetch the `render` pass fires, Esc dismisses
/// the modal back to the screen that opened it (the host reserves only Ctrl+C,
/// so a modal MUST offer its own way out).
fn route_activity(states: &mut ScreenStates, key: &KeyEvent) -> Option<NavIntent> {
    use super::activity::{ActivityEvent, ActivityIntent, reduce_activity};

    if matches!(key.code, KeyCode::Esc) {
        states.activity = None;
        return Some(NavIntent::CloseModal);
    }
    let state = states.activity.take()?;
    let Some(c) = key_char(key) else {
        states.activity = Some(state);
        return None;
    };
    let out = reduce_activity(&state, ActivityEvent::Key(c));
    if let Some(ActivityIntent::Refresh { issue_id }) = out.intent {
        states.pending_activity_fetch = Some(issue_id);
    }
    states.activity = Some(out.state);
    None
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

#[cfg(test)]
mod filter_chip_route_tests {
    use super::*;
    use crate::screen::issue_list::FilterChip;
    use ainb_plugin_sdk::{KeyCode, KeyEvent, KeyKind};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            mods: 0,
            kind: KeyKind::Press,
        }
    }

    /// Tab cycles the Issues filter chip forward through the whole ring
    /// (All → Members → Agents → Mine → All). Regression guard for
    /// `issue-list-filter-chip-unreachable`: before this binding the chip bar was
    /// rendered but had no keyboard input path, so the filter was permanently
    /// stuck on `All`. RED before the `KeyCode::Tab` arm in `route_issue_list`.
    #[test]
    fn tab_cycles_filter_chip_forward() {
        let mut states = ScreenStates::default();
        assert_eq!(states.issue_list.filter(), FilterChip::All);

        for expected in [
            FilterChip::Members,
            FilterChip::Agents,
            FilterChip::Mine,
            FilterChip::All,
        ] {
            let intent = route_issue_list(&mut states, &key(KeyCode::Tab));
            assert!(intent.is_none(), "a chip cycle raises no cross-screen nav");
            assert_eq!(states.issue_list.filter(), expected);
        }
    }

    /// Shift+Tab (BackTab) cycles the chip backward (All → Mine → Agents → …).
    #[test]
    fn back_tab_cycles_filter_chip_backward() {
        let mut states = ScreenStates::default();
        route_issue_list(&mut states, &key(KeyCode::BackTab));
        assert_eq!(states.issue_list.filter(), FilterChip::Mine);
        route_issue_list(&mut states, &key(KeyCode::BackTab));
        assert_eq!(states.issue_list.filter(), FilterChip::Agents);
    }
}

#[cfg(test)]
mod kanban_retry_route_tests {
    use super::*;
    use ainb_hangar_proto::events::TaskCardRow;
    use ainb_plugin_sdk::{KeyCode, KeyEvent, KeyKind};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            mods: 0,
            kind: KeyKind::Press,
        }
    }

    fn card(id: &str, status: &str) -> TaskCardRow {
        TaskCardRow {
            id: ainb_hangar_core::ids::TaskId::from_str(id).unwrap(),
            workspace_id: "ws".into(),
            agent_id: "agent-1".into(),
            issue_id: Some("issue-1".into()),
            status: status.into(),
            priority: 0,
            created_at: 0,
            branch: None,
            pr_url: None,
            pr_status: None,
        }
    }

    /// Focus the failed column (index 3 of [Queued, Running, Done, Failed]).
    fn focus_failed_column(states: &mut ScreenStates) {
        for _ in 0..3 {
            route_kanban(states, &key(KeyCode::Right));
        }
    }

    /// `R` on a focused FAILED card lifts a `hangar/task_retry` for that task —
    /// the in-product recovery for a terminal `agent_error` that never auto-retries.
    /// Regression guard for `manual-retry-noop-on-agent-error-task`: before the `R`
    /// arm in `route_kanban` the key folded into navigation and no attempt row was
    /// ever requeued.
    #[test]
    fn r_on_failed_card_lifts_task_retry() {
        let mut states = ScreenStates::default();
        states.set_tasks(&[card("01HANGARTASKFAILED0001", "failed")]);
        focus_failed_column(&mut states);

        route_kanban(&mut states, &key(KeyCode::Char { ch: 'R' }));

        assert_eq!(
            states.take_pending_task_retry_action().as_deref(),
            Some("01HANGARTASKFAILED0001"),
            "R on a failed card must lift a task_retry for that task id"
        );
    }

    /// `R` on a non-terminal (queued) card is a no-op: only a terminal card can be
    /// requeued, so a live run is never forked.
    #[test]
    fn r_on_queued_card_is_a_noop() {
        let mut states = ScreenStates::default();
        states.set_tasks(&[card("01HANGARTASKQUEUED0001", "queued")]);
        // Focus stays on the queued column (index 0), where the card lives.

        route_kanban(&mut states, &key(KeyCode::Char { ch: 'R' }));

        assert!(
            states.take_pending_task_retry_action().is_none(),
            "R on a non-terminal card must not lift a retry"
        );
    }
}

/// #450: the general invariant that keeps a screen-local binding from silently
/// rotting. The routing layer (tab switches / `q` quit / `?` help) and the HOST
/// (`?`/`H` help toggle) both consume their chars BEFORE the active screen's
/// reducer is consulted, so any screen that binds one of those chars binds a key
/// the user can never press. Enumerating the reserved set against every pure nav
/// mapper makes the whole class of bug non-recurring.
#[cfg(test)]
mod reserved_key_invariant_tests {
    use super::*;
    use crate::screen::fleet::{FleetKey, FleetPaneState, reduce_browse_key};
    use crate::screen::router::{HOST_RESERVED_KEYS, ROUTER_KEYS, is_reserved_key};
    use crate::screen::settings::{SettingsEvent, SettingsSection, SettingsState};
    use ainb_hangar_proto::settings::HealthSnapshot;
    use ainb_plugin_sdk::{KeyCode, KeyEvent, KeyKind};

    /// Every char no hangar screen may bind while it is not capturing text.
    fn reserved_chars() -> Vec<char> {
        let mut all: Vec<char> = ROUTER_KEYS.to_vec();
        for ch in HOST_RESERVED_KEYS {
            if !all.contains(&ch) {
                all.push(ch);
            }
        }
        all
    }

    fn press(ch: char) -> KeyEvent {
        KeyEvent {
            code: KeyCode::Char { ch },
            mods: 0,
            kind: KeyKind::Press,
        }
    }

    /// A settings pane parked on `section`, navigated there through the REAL
    /// `j` section-walk (the field is private, and `j` is not a reserved char).
    fn settings_state(section: SettingsSection) -> SettingsState {
        let mut state = SettingsState::new(
            HealthSnapshot {
                socket_path: "/tmp/x.sock".into(),
                pid: 1,
                uptime_secs: 0,
                version: "test".into(),
                connected: true,
            },
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        for _ in 0..8 {
            if state.section() == section {
                return state;
            }
            state = crate::screen::settings::reduce_settings(&state, SettingsEvent::Key('j')).state;
        }
        assert_eq!(
            state.section(),
            section,
            "could not reach {section:?} via `j`"
        );
        state
    }

    /// No pure screen-key mapper may claim a reserved char. Exhaustive over the
    /// reserved set × the Boards / Kanban / Fleet / Settings nav mappers.
    ///
    /// Fails on `main`: Boards bound `q` (squad) and `D` (depends-on), Boards and
    /// Kanban bound `H`/`L`, Fleet bound `A`/`B`, Settings bound `K`.
    #[test]
    fn no_screen_binds_a_reserved_key() {
        for ch in reserved_chars() {
            assert!(is_reserved_key(ch), "`{ch}` must report as reserved");

            assert!(
                board_nav_event(&press(ch)).is_none(),
                "Boards binds reserved key `{ch}` — the router/host eats it first"
            );
            assert!(
                kanban_nav_event(&press(ch)).is_none(),
                "Kanban binds reserved key `{ch}` — the router/host eats it first"
            );

            let mut fleet = FleetPaneState::default();
            let intent = reduce_browse_key(&mut fleet, FleetKey::Char(ch));
            assert!(
                intent.is_none() && !fleet.is_modal_open(),
                "Fleet binds reserved key `{ch}` — the router eats it first"
            );

            for section in [
                SettingsSection::Daemon,
                SettingsSection::Providers,
                SettingsSection::Keys,
                SettingsSection::Workspaces,
                SettingsSection::Members,
                SettingsSection::Notifications,
            ] {
                let before = settings_state(section);
                let after =
                    crate::screen::settings::reduce_settings(&before, SettingsEvent::Key(ch));
                assert!(
                    after.state == before && after.intent.is_none(),
                    "Settings/{section:?} binds reserved key `{ch}` — the router eats it first"
                );
            }
        }
    }

    /// The rebound Boards verbs are live on their NEW chars — the other half of
    /// the invariant, so the fix can't be "delete the binding".
    #[test]
    fn rebound_boards_verbs_are_bound() {
        assert!(matches!(
            board_nav_event(&press('s')),
            Some(BoardsEvent::AssignSquad)
        ));
        assert!(matches!(
            board_nav_event(&press('w')),
            Some(BoardsEvent::AddDependency)
        ));
        assert!(matches!(
            board_nav_event(&press('<')),
            Some(BoardsEvent::ReorderColumnLeft)
        ));
        assert!(matches!(
            board_nav_event(&press('>')),
            Some(BoardsEvent::ReorderColumnRight)
        ));
        assert!(matches!(
            kanban_nav_event(&press('<')),
            Some(KanbanEvent::MoveCardLeft)
        ));
        assert!(matches!(
            kanban_nav_event(&press('>')),
            Some(KanbanEvent::MoveCardRight)
        ));
    }
}

#[cfg(test)]
mod fleet_routing_tests {
    use super::*;
    use crate::screen::fleet::{
        FleetAction, FleetCapabilities, FleetFilter, FleetIntent, FleetSessionRow,
    };
    use ainb_plugin_sdk::{KeyCode, KeyEvent, KeyKind};
    use std::collections::BTreeMap;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            mods: 0,
            kind: KeyKind::Press,
        }
    }

    fn row(attention: &str) -> FleetSessionRow {
        FleetSessionRow {
            session_key: "claude:one".into(),
            provider: "claude".into(),
            provider_session_id: Some("one".into()),
            current_request_fingerprint: Some("fingerprint".into()),
            current_request: Some(serde_json::json!({
                "tool_use_id": "request-1",
                "questions": [{
                    "id": "q1",
                    "question": "Proceed?",
                    "options": [{"label": "Yes"}, {"label": "No"}]
                }]
            })),
            lifecycle_state: "IDLE".into(),
            attention_state: attention.into(),
            management_state: "MANAGED".into(),
            provenance: "hangar-authoritative".into(),
            confidence: "HIGH".into(),
            transport_health: "HEALTHY".into(),
            capabilities: FleetCapabilities::Flags(BTreeMap::from([
                ("structured_answer".into(), true),
                ("approvals".into(), true),
                ("send_prompt".into(), true),
                ("start".into(), true),
            ])),
            version: 9,
            cwd: "/work/one".into(),
            tmux_target: Some("one:0.0".into()),
            display_name: Some("one".into()),
            repository_name: Some("one".into()),
            branch_name: Some("main".into()),
            discovered_at: 1,
            last_observed_at: 2,
            metadata_updated_at: 2,
            lifecycle_updated_at: 2,
            attention_updated_at: 2,
            transport_updated_at: 2,
        }
    }

    #[test]
    fn fleet_routes_numeric_lenses_and_preserves_modal_digit_entry() {
        let mut states = ScreenStates::default();
        states.fleet.set_sessions(vec![row("ASK")]);

        for (digit, filter) in [
            ('1', FleetFilter::NeedsInput),
            ('2', FleetFilter::Idle),
            ('3', FleetFilter::Completed),
            ('4', FleetFilter::Running),
            ('5', FleetFilter::All),
        ] {
            route_fleet(&mut states, &key(KeyCode::Char { ch: digit }));
            assert_eq!(states.fleet.filter(), filter, "Fleet lens key {digit}");
        }

        for legacy in ['f', 'o', 'm', 'd', 'c', 'x', 'v'] {
            route_fleet(&mut states, &key(KeyCode::Char { ch: legacy }));
            assert_eq!(
                states.fleet.filter(),
                FleetFilter::All,
                "legacy Fleet filter key {legacy:?} must not change lens"
            );
        }

        route_fleet(&mut states, &key(KeyCode::Char { ch: 'p' }));
        route_fleet(&mut states, &key(KeyCode::Char { ch: '1' }));
        route_fleet(&mut states, &key(KeyCode::Enter));
        assert!(matches!(
            states.take_pending_fleet_intent(),
            Some(FleetIntent::Execute {
                action: FleetAction::SendText { text },
                ..
            }) if text == "1"
        ));
        assert_eq!(states.fleet.filter(), FleetFilter::All);
    }
}
