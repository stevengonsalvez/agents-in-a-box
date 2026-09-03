//! The pure screen-routing reducer.
//!
//! [`reduce`] is the single entry point through which every key press and host
//! event flows. It owns the tab-switch / modal / quit semantics of P4.1:
//!
//! - `1` / `,` switch to the issue-list / settings tabs, and `K` / `B` / `I` /
//!   `A` to the four lettered ones (crisp B5 §2.5 cut the strip from sixteen
//!   tabs to seven; the nine demoted screens keep their reducers and are reached
//!   through the `Go:` palette family instead — [`super::command_palette`]).
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

/// Chars the ROUTING layer claims on EVERY hangar screen (tab switches, help,
/// settings, quit) before the active screen's reducer is consulted.
///
/// Kept in lock-step with [`reduce_key`]: a char listed here MUST have a
/// `reduce_key` arm, and every `reduce_key` arm MUST be listed here
/// (`router_keys_all_have_a_reduce_key_arm`).
///
/// Nine of eighteen went out with the crisp B5 tab shrink (`3` `4` `D` `U` `L`
/// `C` `F` `S` `P`). Their screens did not: they are reached with `^P` and the
/// screen's word, and every one of them is pinned reachable by
/// `palette_reaches_every_demoted_screen`. A char dropped here also stops being
/// reserved, so a screen-local binding on it comes ALIVE — `3`/`4` on the fleet
/// filter row had been dead since the router claimed them.
pub const ROUTER_KEYS: [char; 9] = ['1', '2', 'B', 'K', 'I', 'A', ',', '?', 'q'];

/// Chars the HOST swallows before the plugin ever sees them.
///
/// Empty since `ainb-core` lists `hangar-tui` in `PLUGINS_WITH_OWN_HELP`: the
/// host forwards `?` and `H` to this plugin whatever the capture flag says (the
/// `?` overlay is ours, and `H` is a screen key: fleet history, board hint).
/// `Ctrl+C` is a chord, not a bare char, so it is not listed. Kept as the seam
/// the reserved-key invariant test unions with [`ROUTER_KEYS`], so a host that
/// starts claiming a char again has one place to declare it.
pub const HOST_RESERVED_KEYS: [char; 0] = [];

/// Whether the routing layer claims `ch` (drives `routing_event` in the plugin).
///
/// READS [`ROUTER_KEYS`] rather than restating it: the two were a hand-kept
/// `matches!` mirror of the array, so shrinking the strip was three edits that
/// had to stay in step. Now it is two, and the one that remains ([`reduce_key`])
/// is covered by `router_keys_all_have_a_reduce_key_arm`.
#[must_use]
pub const fn is_router_key(ch: char) -> bool {
    let mut i = 0;
    while i < ROUTER_KEYS.len() {
        if ROUTER_KEYS[i] == ch {
            return true;
        }
        i += 1;
    }
    false
}

/// Every char a hangar SCREEN must not bind while it is not capturing text.
///
/// The union of [`ROUTER_KEYS`] and [`HOST_RESERVED_KEYS`]. A screen-local
/// binding on one of these is DEAD — the key never reaches the screen reducer —
/// so an advertised hint on it lies to the user (issue #450).
#[must_use]
pub const fn is_reserved_key(ch: char) -> bool {
    is_router_key(ch)
}

/// Fold one [`AppEvent`] into `state`, returning the next state and any
/// [`Intent`] the IO layer must perform. Pure: no IO, no mutation of `state`.
#[must_use]
pub fn reduce(state: &AppState, ev: AppEvent) -> Reduction {
    match ev {
        AppEvent::Key(c) => reduce_key(state, c),
        AppEvent::Esc => reduce_esc(state),
        AppEvent::OpenAgentPicker(issue) => open_modal(state, Screen::AgentPicker(issue)),
        AppEvent::OpenActivityTimeline(issue) => open_modal(state, Screen::ActivityTimeline(issue)),
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
        // `K` (capital) opens the Kanban board from anywhere (P8.4).
        'K' => switch_tab(state, Screen::Kanban),
        // `B` (capital) opens the user-defined Boards screen from anywhere (P4).
        'B' => switch_tab(state, Screen::Boards),
        // `I` (capital) opens the notification inbox from anywhere (e38.14) — the
        // one attention surface since crisp B3 §2.4.
        'I' => switch_tab(state, Screen::Inbox),
        // `A` (capital) opens the Agents roster from anywhere (slice 2). `A` was
        // free — the old `[3]Agents` tab folded into the issue-list filter chip
        // (e38.38) and never reclaimed a letter, so this is the first `A` binding.
        'A' => switch_tab(state, Screen::Agents),
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

#[cfg(test)]
mod agents_nav_tests {
    use super::*;
    use ainb_hangar_core::ids::WorkspaceId;

    fn state() -> AppState {
        AppState::new(WorkspaceId::from_str("default").unwrap())
    }

    /// `A` routes to the Agents roster from any screen, clearing modal bookkeeping.
    #[test]
    fn a_routes_to_agents() {
        let out = reduce(&state(), AppEvent::Key('A'));
        assert_eq!(out.state.screen, Screen::Agents);
        assert!(out.state.prior_screen.is_none());
        assert!(out.intent.is_none());
    }

    /// LOCK-STEP (#450): every char in [`ROUTER_KEYS`] must actually DO something
    /// in [`reduce_key`] — change the screen or raise an intent. This stops the
    /// reserved set (which screens are forbidden from binding, and which
    /// `routing_event` consults) from drifting away from the real router.
    ///
    /// `'2'` is only live with a task selected, so the seed state carries one.
    #[test]
    fn router_keys_all_have_a_reduce_key_arm() {
        let mut seed = state();
        seed.selected_task =
            Some(ainb_hangar_core::ids::TaskId::from_str("01HANGARTASK000000000001").unwrap());
        // Two seed screens: a key whose target IS the seed screen is a legitimate
        // no-op there, so it only has to bite on one of them.
        let seeds = [Screen::IssueList, Screen::Boards].map(|screen| {
            let mut s = seed.clone();
            s.screen = screen;
            s
        });
        for ch in ROUTER_KEYS {
            assert!(is_router_key(ch), "`{ch}` must be in `is_router_key`");
            assert!(is_reserved_key(ch), "`{ch}` must be reserved for screens");
            let bites = seeds.iter().any(|s| {
                let out = reduce(s, AppEvent::Key(ch));
                out.state.screen != s.screen || out.intent.is_some()
            });
            assert!(
                bites,
                "ROUTER_KEYS lists `{ch}` but `reduce_key` has no arm for it"
            );
        }
        for ch in HOST_RESERVED_KEYS {
            assert!(
                is_reserved_key(ch),
                "host-reserved `{ch}` must be reserved for screens"
            );
        }
    }

    /// The Agents tab is not a modal, so a bare Esc on it is a no-op at the routing
    /// layer (its create/delete overlays own Esc-to-cancel); the user leaves via a
    /// tab hotkey or `q`, never trapped.
    #[test]
    fn esc_on_agents_is_a_routing_noop() {
        let on_agents = reduce(&state(), AppEvent::Key('A')).state;
        let out = reduce(&on_agents, AppEvent::Esc);
        assert_eq!(out.state.screen, Screen::Agents);
        assert!(out.intent.is_none());
    }
}
