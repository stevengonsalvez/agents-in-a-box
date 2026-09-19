#![allow(missing_docs)]

// ABOUTME: A second host implements the preview pane's terminal contract from
// the effect and report docs alone: it keeps its own client, reports by
// session name, and closes whatever the reducer stops naming. No PTY, and no
// handle that only one process can redeem.

#[path = "support/home.rs"]
mod home;

use std::collections::VecDeque;
use std::time::Duration;

use ainb_app::app::NoRenderer;
use ainb_app::app::TerminalTarget;
use ainb_app::app::keymap::Chord;
use ainb_app::app::reports::{self, AttachOutcome, AttachedTo};
use ainb_app::app::screens::ids;
use ainb_app::models::other_tmux::OtherTmuxSession;
use ainb_app::{AppState, CommandId, Effect, Intent, Keymap, dispatch};

/// A host with no terminal of its own: a "client" is the session name it holds.
#[derive(Default)]
struct HeadlessHost {
    held: Option<String>,
    /// What this host still held when it ran a full-screen attach, per attach.
    held_during_full_screen: Vec<Option<String>>,
    /// A host with no writable terminal answers in-place with `unsupported`.
    no_in_place: bool,
    /// In-place attaches this host was asked for.
    in_place_requests: usize,
    /// Keys this host sent to its client.
    forwarded: Vec<Chord>,
}

impl HeadlessHost {
    /// Close the client the state no longer names, as the contract says a
    /// host does before each effect and after the last.
    fn reconcile(&mut self, state: &AppState) {
        if self.held.as_deref() != state.embed_session_name() {
            self.held = None;
        }
    }

    /// Run `effects` and dispatch their reports.
    fn run(&mut self, state: &mut AppState, keymap: &Keymap, effects: Vec<Effect>) {
        let mut queue = VecDeque::from(effects);
        while let Some(effect) = queue.pop_front() {
            self.reconcile(state);
            let report = match effect {
                Effect::AttachTerminal(TerminalTarget::InPlace { tmux_session, .. }) => {
                    self.in_place_requests += 1;
                    if self.no_in_place {
                        reports::in_place_failed(tmux_session.as_str(), "no terminal here", true)
                    } else {
                        self.held = Some(tmux_session.as_str().to_string());
                        reports::in_place_opened(tmux_session.as_str())
                    }
                }
                Effect::AttachTerminal(TerminalTarget::Observe { tmux_session, .. }) => {
                    self.held = Some(tmux_session.as_str().to_string());
                    reports::observer_opened(tmux_session.as_str())
                }
                Effect::AttachTerminal(TerminalTarget::Tmux(tmux_session)) => {
                    self.held_during_full_screen.push(self.held.clone());
                    reports::attach_finished(
                        &AttachedTo::Tmux(tmux_session.as_str().to_string()),
                        &AttachOutcome::Detached,
                    )
                }
                Effect::Detach => reports::detached(),
                other => panic!("the preview contract does not use {other:?}"),
            };
            queue.extend(dispatch(state, keymap, &mut NoRenderer, report));
        }
        self.reconcile(state);
    }

    /// Route a key the way a host must while the in-place pane is live: every
    /// key to the client except the chord that releases it.
    fn key(&mut self, state: &mut AppState, keymap: &Keymap, chord: &str) {
        let chord = Chord::parse(chord).expect("chord");
        if state.is_interactive_pane() && keymap.releases_in_place_pane(&chord) {
            self.command(state, keymap, "embed_interactive.detach");
        } else if state.is_interactive_pane() {
            self.forwarded.push(chord);
        } else {
            let effects = dispatch(state, keymap, &mut NoRenderer, Intent::Key(chord));
            self.run(state, keymap, effects);
        }
    }

    fn command(&mut self, state: &mut AppState, keymap: &Keymap, id: &str) {
        let effects = dispatch(
            state,
            keymap,
            &mut NoRenderer,
            Intent::Command(CommandId::new(id), serde_json::Value::Null),
        );
        self.run(state, keymap, effects);
    }
}

fn isolated_home() {
    home::shared();
}

fn session_list_with(rows: &[&str]) -> AppState {
    let mut state = AppState::new();
    state.shell.current_screen = ids::SESSION_LIST.to_string();
    state.sessions.selected_workspace_index = None;
    state.sessions.selected_session_index = None;
    state.tmux.other_tmux_sessions = rows
        .iter()
        .map(|name| OtherTmuxSession::new((*name).to_string(), false, 1))
        .collect();
    state.tmux.selected_other_tmux_index = Some(0);
    state
}

#[test]
fn a_headless_host_attaches_in_place_and_releases_on_detach() {
    isolated_home();
    let keymap = Keymap::defaults();
    let mut state = session_list_with(&["ainb-contract-a"]);
    let mut host = HeadlessHost::default();

    host.command(&mut state, &keymap, "session_list.attach_interactive");
    assert!(state.is_interactive_pane());
    assert_eq!(host.held.as_deref(), Some("ainb-contract-a"));

    host.command(&mut state, &keymap, "embed_interactive.detach");
    assert!(!state.is_interactive_pane());
    assert_eq!(host.held, None, "the host closed what the reducer released");
}

#[test]
fn leaving_the_session_list_closes_the_hosts_client() {
    isolated_home();
    let keymap = Keymap::defaults();
    let mut state = session_list_with(&["ainb-contract-b"]);
    let mut host = HeadlessHost::default();
    host.command(&mut state, &keymap, "session_list.attach_interactive");
    assert!(state.is_interactive_pane());

    state.shell.current_screen = ids::GIT_VIEW.to_string();
    assert!(state.tick_terminal_pane());
    host.run(&mut state, &keymap, Vec::new());

    assert_eq!(host.held, None);
}

#[test]
fn a_client_that_ends_on_its_own_releases_the_pane_with_a_notice() {
    isolated_home();
    let keymap = Keymap::defaults();
    let mut state = session_list_with(&["ainb-contract-c"]);
    let mut host = HeadlessHost::default();
    host.command(&mut state, &keymap, "session_list.attach_interactive");

    let effects = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        reports::terminal_exited("ainb-contract-c"),
    );
    host.run(&mut state, &keymap, effects);

    assert!(!state.is_interactive_pane());
    assert_eq!(host.held, None);
    assert!(
        state
            .shell
            .notifications
            .iter()
            .any(|note| note.message.contains("Live session ended"))
    );
}

#[test]
fn the_read_only_preview_follows_the_selection_through_the_host() {
    isolated_home();
    let keymap = Keymap::defaults();
    let mut state = session_list_with(&["ainb-contract-first", "ainb-contract-second"]);
    let mut host = HeadlessHost::default();
    let tick = |state: &mut AppState, host: &mut HeadlessHost| {
        let effects = state.request_terminal_observer().into_iter().collect();
        host.run(state, &keymap, effects);
    };

    tick(&mut state, &mut host);
    assert_eq!(host.held, None, "a new selection settles first");
    std::thread::sleep(Duration::from_millis(300));
    tick(&mut state, &mut host);
    assert_eq!(host.held.as_deref(), Some("ainb-contract-first"));
    assert!(state.is_observing_selected_terminal());

    state.tmux.selected_other_tmux_index = Some(1);
    tick(&mut state, &mut host);
    assert_eq!(host.held, None, "moving off the row closes its mirror");
    std::thread::sleep(Duration::from_millis(300));
    tick(&mut state, &mut host);
    assert_eq!(host.held.as_deref(), Some("ainb-contract-second"));
}

#[test]
fn a_full_screen_attach_runs_with_the_released_preview_client_closed() {
    isolated_home();
    let keymap = Keymap::defaults();
    let mut state = session_list_with(&["ainb-contract-d"]);
    let mut host = HeadlessHost::default();
    // The read-only preview: a live pane takes every command, so the attach
    // below comes from the list while its row is only mirrored.
    let tick = |state: &mut AppState, host: &mut HeadlessHost| {
        let effects = state.request_terminal_observer().into_iter().collect();
        host.run(state, &keymap, effects);
    };
    tick(&mut state, &mut host);
    std::thread::sleep(Duration::from_millis(300));
    tick(&mut state, &mut host);
    assert_eq!(host.held.as_deref(), Some("ainb-contract-d"));

    host.command(&mut state, &keymap, "session_list.attach_tmux");

    assert_eq!(
        host.held_during_full_screen,
        vec![None],
        "the full-screen attach ran with the preview client already closed"
    );
}

#[test]
fn a_closed_input_channel_releases_the_pane_closes_the_client_and_says_so() {
    isolated_home();
    let keymap = Keymap::defaults();
    let mut state = session_list_with(&["ainb-contract-e"]);
    let mut host = HeadlessHost::default();
    host.command(&mut state, &keymap, "session_list.attach_interactive");
    assert!(state.is_interactive_pane());

    let effects = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        reports::terminal_input_closed("ainb-contract-e"),
    );
    host.run(&mut state, &keymap, effects);

    assert!(!state.is_interactive_pane(), "the pane is released");
    assert_eq!(host.held, None, "the host closed the client");
    assert!(
        state
            .shell
            .notifications
            .iter()
            .any(|note| note.message.contains("input channel closed")),
        "{:?}",
        state.shell.notifications
    );
}

#[test]
fn a_host_that_cannot_attach_in_place_is_not_asked_again() {
    isolated_home();
    let keymap = Keymap::defaults();
    let mut state = session_list_with(&["ainb-contract-f"]);
    let mut host = HeadlessHost {
        no_in_place: true,
        ..HeadlessHost::default()
    };

    host.command(&mut state, &keymap, "session_list.attach_interactive");
    assert_eq!(host.in_place_requests, 1);
    assert!(!state.is_interactive_pane());

    host.command(&mut state, &keymap, "session_list.attach_interactive");
    assert_eq!(host.in_place_requests, 1, "the reducer stopped asking");
    assert!(
        state
            .shell
            .notifications
            .iter()
            .any(|note| note.message.contains("full screen")),
        "{:?}",
        state.shell.notifications
    );
}

#[test]
fn a_live_in_place_pane_takes_every_key_but_the_one_that_releases_it() {
    isolated_home();
    let keymap = Keymap::defaults();
    let mut state = session_list_with(&["ainb-contract-g"]);
    let mut host = HeadlessHost::default();
    host.command(&mut state, &keymap, "session_list.attach_interactive");
    let before = state.versions();

    for chord in [":", "q", "ctrl+c", "?", "esc"] {
        host.key(&mut state, &keymap, chord);
    }
    assert_eq!(host.forwarded.len(), 5, "every key went to the client");
    assert_eq!(state.versions(), before, "none reached the reducer");
    assert!(state.is_interactive_pane());

    host.key(&mut state, &keymap, "ctrl+q");
    assert_eq!(host.forwarded.len(), 5, "the release chord is not typed");
    assert!(!state.is_interactive_pane());
    assert_eq!(host.held, None);
}
