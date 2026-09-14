#![allow(missing_docs)]

// ABOUTME: A second host implements the preview pane's terminal contract from
// the effect and report docs alone: it keeps its own client, reports by
// session name, and closes whatever the reducer stops naming. No PTY, and no
// handle that only one process can redeem.

use std::collections::VecDeque;
use std::time::Duration;

use ainb_app::app::NoRenderer;
use ainb_app::app::TerminalTarget;
use ainb_app::app::reports;
use ainb_app::app::screens::ids;
use ainb_app::models::other_tmux::OtherTmuxSession;
use ainb_app::{AppState, CommandId, Effect, Intent, Keymap, dispatch};

/// A host with no terminal of its own: a "client" is the session name it holds.
#[derive(Default)]
struct HeadlessHost {
    held: Option<String>,
}

impl HeadlessHost {
    /// Run `effects` and dispatch their reports, then close the client the
    /// state no longer names, as the contract says a host does.
    fn run(&mut self, state: &mut AppState, keymap: &Keymap, effects: Vec<Effect>) {
        let mut queue = VecDeque::from(effects);
        while let Some(effect) = queue.pop_front() {
            let report = match effect {
                Effect::AttachTerminal(TerminalTarget::InPlace { tmux_session, .. }) => {
                    self.held = Some(tmux_session.as_str().to_string());
                    reports::in_place_opened(tmux_session.as_str())
                }
                Effect::AttachTerminal(TerminalTarget::Observe { tmux_session, .. }) => {
                    self.held = Some(tmux_session.as_str().to_string());
                    reports::observer_opened(tmux_session.as_str())
                }
                Effect::Detach => reports::detached(),
                other => panic!("the preview contract does not use {other:?}"),
            };
            queue.extend(dispatch(state, keymap, &mut NoRenderer, report));
        }
        if self.held.as_deref() != state.embed_session_name() {
            self.held = None;
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
    static HOME: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    HOME.get_or_init(|| {
        let home = tempfile::tempdir().expect("scratch home");
        std::env::set_var("HOME", home.path());
        home
    });
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
