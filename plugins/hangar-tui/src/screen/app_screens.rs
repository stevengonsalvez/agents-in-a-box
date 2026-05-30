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

use ainb_hangar_proto::events::{ActorRow, IssueRow, SkillRow};
use ainb_hangar_proto::settings::{HealthSnapshot, KeyRow, ProviderRow, WorkspaceRow};
use ainb_plugin_sdk::{KeyCode, KeyEvent, WireBuffer};

use super::agent_picker::{reduce_agent_picker, AgentPickerEvent, AgentPickerState};
use super::issue_list::{reduce_issue_list, IssueListEvent, IssueListIntent, IssueListState};
use super::settings::{reduce_settings, SettingsEvent, SettingsState};
use super::skill_manager::{reduce_skill_manager, SkillManagerEvent, SkillManagerState};
use super::task_detail::{reduce_task_detail, TaskDetailEvent, TaskDetailState};
use super::{AppState, Screen};

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
        let workspaces = vec![WorkspaceRow {
            id: ws_id.to_string(),
            name: ws_id.to_string(),
            current: true,
        }];
        self.settings = Some(SettingsState::new(health, providers, keys, workspaces));
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
            }
            None
        }
        Screen::Settings => {
            if let Some(s) = states.settings.take() {
                let ev = match key.code {
                    KeyCode::Esc => SettingsEvent::Esc,
                    _ => SettingsEvent::Key(key_char(key)?),
                };
                let out = reduce_settings(&s, ev);
                states.settings = Some(out.state);
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
