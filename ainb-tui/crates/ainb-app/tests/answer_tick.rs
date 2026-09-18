// ABOUTME: The reducer's own answer tick: the outcome a send worker reports
// lands, the session tab is reconciled against what is available, and the
// composer is pointed at the request being shown, all without a renderer in
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
    state.tick_surfaces();

    state.fleet.ask_state.reports().lock().expect("inbox").push((
        request_id(&chip),
        AnswerPhase::Delivered {
            via: "tmux (feat-login)".to_string(),
        },
    ));
    let before = state.versions();
    state.tick_surfaces();

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
    state.tick_surfaces();
    assert_eq!(
        state.shell.session_tab,
        SessionTab::Ask,
        "the question is still waiting, so the pane stays"
    );

    // Answered elsewhere: the chip is gone on the next refresh, and the pane
    // that answers it can no longer act on anything.
    state.sessions.workspaces[0].sessions[0].live_attention.clear();
    state.tick_surfaces();

    assert_eq!(state.shell.session_tab, SessionTab::Preview);
}

#[test]
fn the_composer_is_pointed_at_the_request_before_the_first_key() {
    // A request with no options has one place an answer can come from. Without
    // the retarget the focus is only initialised by the first key press, so
    // that key falls through to the screen's shortcuts instead of reaching the
    // composer.
    let mut state = waiting_on(ask(&[]));

    state.tick_surfaces();

    assert_eq!(state.fleet.ask_state.focus(), AskFocus::FreeText);
}

#[test]
fn the_frame_says_which_rows_the_filter_hides() {
    use ainb_app::app::state::SessionFilter;
    use ainb_app::models::{SessionMode, SessionStatus};

    let mut state = waiting_on(ask(&["Focused"]));
    let stopped = {
        let mut session = Session::new("spike-ssr".to_string(), "/parity/api/old".to_string());
        session.mode = SessionMode::Interactive;
        session.status = SessionStatus::Stopped;
        session
    };
    let stopped_id = stopped.id;
    state.sessions.workspaces[0].add_session(stopped);
    state.sessions.workspaces[0].sessions[0].mode = SessionMode::Interactive;
    state.sessions.workspaces[0].sessions[0].status = SessionStatus::Running;

    state.sessions.session_filter = SessionFilter::ActiveOnly;
    state.tick_surfaces();
    assert_eq!(
        state.sessions.hidden_sessions,
        std::collections::HashSet::from([stopped_id]),
        "the stopped row is hidden, and the list keeps it so the indices hold"
    );
    assert_eq!(
        state.sessions.workspaces[0].sessions.len(),
        2,
        "the frame carries every row: the selection is an index into this list"
    );

    state.sessions.session_filter = SessionFilter::All;
    state.tick_surfaces();
    assert!(state.sessions.hidden_sessions.is_empty());
}

#[test]
fn an_unsent_answer_survives_looking_at_another_question() {
    // Typed on one row, not sent, then the cursor moved to another blocking
    // row: the tick retargets on every pass, whichever pane is showing, so
    // without a draft per request the answer was gone on the way back.
    let first = ask(&[]);
    let mut state = waiting_on(first);
    let mut second_session = Session::new("spike".to_string(), "/parity/api/two".to_string());
    second_session.live_attention = vec![SessionAttention::daemon(
        AttentionKind::Ask,
        2_000,
        "att-2".into(),
    )];
    state.sessions.workspaces[0].add_session(second_session);
    state.tick_surfaces();
    for c in "staging".chars() {
        state.fleet.update(|fleet| {
            fleet.ask_state.push_char(c);
            true
        });
    }

    state.sessions.selected_session_index = Some(1);
    state.tick_surfaces();
    assert_eq!(
        state.fleet.ask_state.free_text(),
        "",
        "the other question starts empty"
    );

    state.sessions.selected_session_index = Some(0);
    state.tick_surfaces();
    assert_eq!(
        state.fleet.ask_state.free_text(),
        "staging",
        "and the first one kept its answer"
    );
    assert_eq!(state.fleet.ask_state.focus(), AskFocus::FreeText);
}

#[test]
fn a_tick_with_nothing_outstanding_writes_nothing() {
    let mut state = waiting_on(ask(&["Focused"]));
    state.tick_surfaces();
    let before = state.versions();

    state.tick_surfaces();

    assert_eq!(
        before,
        state.versions(),
        "no worker reported and nothing moved, so no section is framed again"
    );
}
