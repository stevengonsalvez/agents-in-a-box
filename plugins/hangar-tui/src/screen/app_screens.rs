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
use ainb_hangar_proto::settings::{HealthSnapshot, KeyRow, ProviderRow, WorkspaceRow};
use ainb_plugin_sdk::{KeyCode, KeyEvent, WireBuffer};

use super::agent_picker::{reduce_agent_picker, AgentPickerEvent, AgentPickerState};
use super::autopilots::{reduce_autopilots, AutopilotsEvent, AutopilotsIntent, AutopilotsState};
use super::issue_list::{reduce_issue_list, IssueListEvent, IssueListIntent, IssueListState};
use super::kanban::{reduce_kanban, KanbanEvent, KanbanIntent, KanbanState};
use super::settings::{reduce_settings, SettingsEvent, SettingsIntent, SettingsState};
use super::skill_manager::{
    reduce_skill_manager, SkillManagerEvent, SkillManagerIntent, SkillManagerState,
};
use super::task_detail::{reduce_task_detail, TaskDetailEvent, TaskDetailState};
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
    /// Settings screen cache (built once the four snapshots arrive).
    pub settings: Option<SettingsState>,
    /// Task-detail screen cache (present only while a task is open).
    pub task_detail: Option<TaskDetailState>,
    /// Agent-picker modal cache (present only while the modal is open).
    pub agent_picker: Option<AgentPickerState>,
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
    /// Cached workspace catalogue from `host/workspace_list` (P5.5). Seeds the
    /// Settings Workspace pane regardless of which snapshot arrives first.
    pub workspace_rows: Vec<WorkspaceRow>,
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
        self.settings = Some(SettingsState::new(health, providers, keys, workspaces));
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
}

/// The cached actor snapshot, stashed on [`ScreenStates`] so the picker can be
/// rebuilt for whichever issue it is opened on.
impl ScreenStates {
    /// Open the agent picker for `issue` over the cached actor snapshot.
    pub fn open_picker(&mut self, issue: ainb_hangar_core::ids::IssueId) {
        self.agent_picker = Some(AgentPickerState::new(issue, self.actors.clone()));
    }

    /// Open task detail for `issue`'s task, seeding from the issue's row.
    pub fn open_task_detail(&mut self, task_id: ainb_hangar_core::ids::TaskId, issue: IssueRow) {
        self.task_detail = Some(TaskDetailState::new(task_id, issue));
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
    /// Close the active modal back to its prior screen (raised by Esc on a modal).
    CloseModal,
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
        "1 issues  2 task  4 skills  , settings",
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
/// Tab-switch keys (`1`/`2`/`4`/`,`) and `?`/Esc are routing-layer concerns
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
        Screen::TaskDetail(_) => {
            if let Some(td) = states.task_detail.take() {
                let ev = match key.code {
                    KeyCode::Esc => TaskDetailEvent::Esc,
                    _ => TaskDetailEvent::Key(key_char(key)?),
                };
                let out = reduce_task_detail(&td, ev);
                states.task_detail = Some(out.state);
            }
            None
        }
        Screen::AgentPicker(_) => route_agent_picker(states, key),
        Screen::Help => None,
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
        // CreateIssue is a P5 flow; ignored at P4.
        _ => None,
    }
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

/// Agent-picker key routing: Esc (or a reducer-closed state) raises
/// [`NavIntent::CloseModal`] so the plugin pops the modal back to its prior
/// screen.
fn route_agent_picker(states: &mut ScreenStates, key: &KeyEvent) -> Option<NavIntent> {
    let picker = states.agent_picker.take()?;
    let ev = match key.code {
        KeyCode::Esc => AgentPickerEvent::Esc,
        _ => AgentPickerEvent::Key(key_char(key)?),
    };
    let out = reduce_agent_picker(&picker, ev);
    let closed = out.state.is_closed();
    states.agent_picker = Some(out.state);
    if closed {
        states.agent_picker = None;
        Some(NavIntent::CloseModal)
    } else {
        None
    }
}
