// ABOUTME: The reducer's own answer tick: the outcome a send worker reports
// lands, the session tab is reconciled against what is available, and the
// composer is pointed at the request being shown — all without a renderer in
// the process, because the desktop shell has none of the terminal's draw loop.

use ainb_app::AppState;
use ainb_app::app::screens::ids as screen_ids;
use ainb_app::components::session_tabs::SessionTab;
use ainb_app::fleet::answer::{AnswerPhase, AskFocus, request_id};
use ainb_app::fleet::attention::{AttentionKind, AttentionOption, SessionAttention};
use ainb_app::models::{Session, Workspace};

/// One workspace, one selected session, blocking on `chip`.
fn waiting_on(chip: SessionAttention) -> AppState {
    let mut state = AppState::new();
    state.shell.current_screen = screen_ids::SESSION_LIST.to_string();
    let mut workspace = Workspace::new("api".to_string(), "/parity/api".into());
    let mut session = Session::new("feat-login".to_string(), "/parity/api/wt".to_string());
    session.live_attention = vec![chip];
    workspace.add_session(session);
    state.sessions.workspaces = vec![workspace];
    state.sessions.selected_workspace_index = Some(0);
    state.sessions.selected_session_index = Some(0);
    state
}

fn ask(options: &[&str]) -> SessionAttention {
    SessionAttention::daemon(AttentionKind::Ask, 1_000, "att-1".into()).with_options(
        options
            .iter()
            .map(|label| AttentionOption {
                label: (*label).to_string(),
                description: String::new(),
            })
            .collect(),
    )
}

#[test]
fn the_outcome_a_send_worker_reports_lands_with_no_renderer() {
    // The worker reports into the state, not into a frame. A host that folded
    // it only while drawing would leave an answered row reading SENT for as
    // long as its window is open.
    let chip = ask(&["Focused"]);
    let mut state = waiting_on(chip.clone());
    state.tick_answers();

    state.fleet.ask_state.reports().lock().expect("inbox").push((
        request_id(&chip),
        AnswerPhase::Delivered {
            via: "tmux (feat-login)".to_string(),
        },
    ));
    let before = state.versions();
    state.tick_answers();

    assert!(
        matches!(state.fleet.ask_state.phase(), Some(AnswerPhase::Delivered { via }) if via.contains("feat-login")),
        "the worker's outcome is folded in"
    );
    assert_ne!(
        before,
        state.versions(),
        "and what moved is framed for whatever is drawing"
    );
}

#[test]
fn a_tab_that_goes_dead_under_the_operator_is_reconciled() {
    let chip = ask(&["Focused"]);
    let mut state = waiting_on(chip);
    state.shell.session_tab = SessionTab::Ask;
    state.tick_answers();
    assert_eq!(
        state.shell.session_tab,
        SessionTab::Ask,
        "the question is still waiting, so the pane stays"
    );

    // Answered elsewhere: the chip is gone on the next refresh, and the pane
    // that answers it can no longer act on anything.
    state.sessions.workspaces[0].sessions[0].live_attention.clear();
    state.tick_answers();

    assert_eq!(state.shell.session_tab, SessionTab::Preview);
}

#[test]
fn the_composer_is_pointed_at_the_request_before_the_first_key() {
    // A request with no options has one place an answer can come from. Without
    // the retarget the focus is only initialised by the first key press, so
    // that key falls through to the screen's shortcuts instead of reaching the
    // composer.
    let mut state = waiting_on(ask(&[]));

    state.tick_answers();

    assert_eq!(state.fleet.ask_state.focus(), AskFocus::FreeText);
}

#[test]
fn a_tick_with_nothing_outstanding_writes_nothing() {
    let mut state = waiting_on(ask(&["Focused"]));
    state.tick_answers();
    let before = state.versions();

    state.tick_answers();

    assert_eq!(
        before,
        state.versions(),
        "no worker reported and nothing moved, so no section is framed again"
    );
}
