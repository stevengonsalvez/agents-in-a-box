//! The pure screen-routing reducer.
//!
//! [`reduce`] is the single entry point through which every key press and host
//! event flows. It owns the tab-switch / modal / quit semantics of P4.1:
//!
//! - `1` / `3` / `4` / `,` switch to the issue-list / skill-manager /
//!   autopilot-manager / settings tabs (the numbered tabs are contiguous; the
//!   old `[3]Agents` tab folded into the issue-list filter chip, so Skills/
//!   Autopilots shifted down to `3`/`4` — e38.38).
//! - `2` switches to task detail **only** when a task is selected (otherwise a
//!   no-op, per the RED test `reduce_tab_key_2_only_valid_when_task_selected`).
//! - `?` opens the [help overlay](Screen::Help) modal, remembering the screen
//!   it overlaid so Esc can restore it.
//! - [`AppEvent::OpenAgentPicker`] opens the agent-picker modal, remembering
//!   the screen it overlaid so Esc can restore it.
//! - `Esc` closes a modal back to [`AppState::prior_screen`].
//! - `q` emits [`Intent::Quit`] without changing the screen.
//!
//! Unknown keys and invalid transitions return the input state unchanged with
//! no intent, so the function is total over `(AppState, AppEvent)`.

use super::{AppEvent, AppState, Intent, Reduction, Screen};

/// Fold one [`AppEvent`] into `state`, returning the next state and any
/// [`Intent`] the IO layer must perform. Pure: no IO, no mutation of `state`.
#[must_use]
pub fn reduce(state: &AppState, ev: AppEvent) -> Reduction {
    match ev {
        AppEvent::Key(c) => reduce_key(state, c),
        AppEvent::Esc => reduce_esc(state),
        AppEvent::OpenAgentPicker(issue) => open_modal(state, Screen::AgentPicker(issue)),
        AppEvent::OpenCommandPalette => open_modal(state, Screen::CommandPalette),
    }
}

/// Handle a printable-key press.
fn reduce_key(state: &AppState, c: char) -> Reduction {
    match c {
        '1' => switch_tab(state, Screen::IssueList),
        // Task detail (`2`) is only reachable when a task is selected;
        // otherwise the key is a no-op.
        '2' => state.selected_task.as_ref().map_or_else(
            || unchanged(state),
            |task| switch_tab(state, Screen::TaskDetail(task.clone())),
        ),
        // `3`/`4` are contiguous after the old `[3]Agents` tab folded into the
        // issue-list filter chip (e38.38): Skills/Autopilots shifted down to close
        // the numbering gap.
        '3' => switch_tab(state, Screen::SkillManager),
        '4' => switch_tab(state, Screen::Autopilots),
        // `K` (capital) opens the Kanban board from anywhere (P8.4).
        'K' => switch_tab(state, Screen::Kanban),
        // `B` (capital) opens the user-defined Boards screen from anywhere (P4).
        'B' => switch_tab(state, Screen::Boards),
        // `D` (capital) opens the daemon-health pane from anywhere (P8.5).
        'D' => switch_tab(state, Screen::DaemonHealth),
        // `U` (capital) opens the usage dashboard from anywhere (e38.35).
        'U' => switch_tab(state, Screen::Usage),
        // `L` (capital) opens the logs-tail pane from anywhere (P8.6).
        'L' => switch_tab(state, Screen::Logs),
        // `I` (capital) opens the notification inbox from anywhere (e38.14).
        'I' => switch_tab(state, Screen::Inbox),
        // `C` (capital) opens the fleet-wide control center from anywhere (P2).
        'C' => switch_tab(state, Screen::ControlCenter),
        ',' => switch_tab(state, Screen::Settings),
        // `?` opens the help overlay over the current screen (P4.1 deliverable,
        // P4.md:78). Esc restores the prior screen, like any modal.
        '?' => open_modal(state, Screen::Help),
        'q' => Reduction {
            state: state.clone(),
            intent: Some(Intent::Quit),
        },
        // Any other key is a no-op at the routing layer (screen-local
        // handlers in later phases consume them).
        _ => unchanged(state),
    }
}

/// Handle Esc: close a modal back to its prior screen; otherwise no-op.
fn reduce_esc(state: &AppState) -> Reduction {
    if state.screen.is_modal() {
        let mut next = state.clone();
        next.screen = next.prior_screen.take().unwrap_or(Screen::IssueList);
        no_intent(next)
    } else {
        unchanged(state)
    }
}

/// Open `modal` as an overlay, stashing the current screen as the
/// [`AppState::prior_screen`] so Esc can restore it. Re-opening a modal from
/// *within* a modal does not bury the real prior screen.
fn open_modal(state: &AppState, modal: Screen) -> Reduction {
    debug_assert!(
        modal.is_modal(),
        "open_modal called with a non-modal screen"
    );
    let mut next = state.clone();
    if !state.screen.is_modal() {
        next.prior_screen = Some(state.screen.clone());
    }
    next.screen = modal;
    no_intent(next)
}

/// Switch to a top-level tab, clearing any modal-restore bookkeeping.
fn switch_tab(state: &AppState, screen: Screen) -> Reduction {
    let mut next = state.clone();
    next.screen = screen;
    next.prior_screen = None;
    no_intent(next)
}

/// A reduction that changes state but emits no intent.
const fn no_intent(state: AppState) -> Reduction {
    Reduction {
        state,
        intent: None,
    }
}

/// A no-op reduction: state cloned unchanged, no intent.
fn unchanged(state: &AppState) -> Reduction {
    no_intent(state.clone())
}
