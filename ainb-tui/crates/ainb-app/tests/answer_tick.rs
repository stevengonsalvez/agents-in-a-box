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
    state.tick_surfaces(0);

    state.fleet.ask_state.reports().lock().expect("inbox").push((
        request_id(&chip),
        AnswerPhase::Delivered {
            via: "tmux (feat-login)".to_string(),
        },
    ));
    let before = state.versions();
    state.tick_surfaces(0);

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
    state.tick_surfaces(0);
    assert_eq!(
        state.shell.session_tab,
        SessionTab::Ask,
        "the question is still waiting, so the pane stays"
    );

    // Answered elsewhere: the chip is gone on the next refresh, and the pane
    // that answers it can no longer act on anything.
    state.sessions.workspaces[0].sessions[0].live_attention.clear();
    state.tick_surfaces(0);

    assert_eq!(state.shell.session_tab, SessionTab::Preview);
}

#[test]
fn the_composer_is_pointed_at_the_request_before_the_first_key() {
    // A request with no options has one place an answer can come from. Without
    // the retarget the focus is only initialised by the first key press, so
    // that key falls through to the screen's shortcuts instead of reaching the
    // composer.
    let mut state = waiting_on(ask(&[]));

    state.tick_surfaces(0);

    assert_eq!(state.fleet.ask_state.focus(), AskFocus::FreeText);
}

/// The frame carries the rows a surface draws, not the filter (#1180): a
/// stopped row the filter hides is not on it, and the selection travels by id,
/// because an index into the section's full list would name the wrong row.
#[test]
fn the_frame_carries_only_the_rows_the_filter_shows() {
    use ainb_app::app::state::SessionFilter;
    use ainb_app::models::{SessionMode, SessionStatus};
    use ainb_app::wire::frame::HostId;
    use ainb_app::{SectionId, wire::section_json};

    let mut state = waiting_on(ask(&["Focused"]));
    let stopped = {
        let mut session = Session::new("spike-ssr".to_string(), "/parity/api/old".to_string());
        session.mode = SessionMode::Interactive;
        session.status = SessionStatus::Stopped;
        session
    };
    let stopped_id = stopped.id;
    // The stopped row goes first, so the running row's index in the section
    // (1) differs from its place on the frame (0).
    state.sessions.workspaces[0].sessions.insert(0, stopped);
    let running = &mut state.sessions.workspaces[0].sessions[1];
    running.mode = SessionMode::Interactive;
    running.status = SessionStatus::Running;
    let running_id = running.id;
    state.sessions.selected_workspace_index = Some(0);
    state.sessions.selected_session_index = Some(1);

    let row_ids = |state: &AppState| -> (Vec<String>, serde_json::Value) {
        let frame = section_json(state, SectionId::Sessions, &HostId::local());
        let ids = frame["workspaces"][0]["sessions"]
            .as_array()
            .expect("the workspace's rows")
            .iter()
            .map(|row| row["id"].as_str().expect("a row id").to_string())
            .collect();
        (ids, frame["selected_session_id"].clone())
    };

    state.sessions.session_filter = SessionFilter::ActiveOnly;
    let (ids, selected) = row_ids(&state);
    assert_eq!(
        ids,
        vec![running_id.to_string()],
        "the stopped row is not framed"
    );
    assert_eq!(selected, serde_json::json!(running_id.to_string()));
    assert!(
        state.sessions.workspaces[0].sessions.iter().any(|row| row.id == stopped_id),
        "the section keeps the row: only the frame leaves it out"
    );

    state.sessions.session_filter = SessionFilter::All;
    let (ids, selected) = row_ids(&state);
    assert_eq!(ids, vec![stopped_id.to_string(), running_id.to_string()]);
    assert_eq!(selected, serde_json::json!(running_id.to_string()));
    let frame = section_json(&state, SectionId::Sessions, &HostId::local());
    assert!(frame.get("hidden_sessions").is_none());
    assert!(frame.get("selected_session_index").is_none());
}

/// A selected row can leave the filter without the selection moving: a running
/// row that stops while the filter shows only active ones. The frame no longer
/// carries that row, so it must not name it as selected. (Cycling the filter
/// itself clears the selection.)
#[test]
fn the_frame_names_no_selection_the_filter_hides() {
    use ainb_app::app::state::SessionFilter;
    use ainb_app::models::{SessionMode, SessionStatus};
    use ainb_app::wire::frame::HostId;
    use ainb_app::{SectionId, wire::section_json};

    let mut state = waiting_on(ask(&["Focused"]));
    let row = &mut state.sessions.workspaces[0].sessions[0];
    row.mode = SessionMode::Interactive;
    row.status = SessionStatus::Running;
    let row_id = row.id;
    state.sessions.selected_workspace_index = Some(0);
    state.sessions.selected_session_index = Some(0);
    state.sessions.session_filter = SessionFilter::ActiveOnly;

    let selected = |state: &AppState| {
        section_json(state, SectionId::Sessions, &HostId::local())["selected_session_id"].clone()
    };
    assert_eq!(selected(&state), serde_json::json!(row_id.to_string()));

    // The session stops; nothing moves the selection off it.
    state.sessions.workspaces[0].sessions[0].status = SessionStatus::Stopped;
    assert_eq!(state.sessions.selected_session_index, Some(0));
    assert_eq!(
        selected(&state),
        serde_json::Value::Null,
        "the frame does not carry the stopped row, so it names no selection"
    );

    // Cycling the filter clears the selection outright.
    state.cycle_session_filter();
    assert_eq!(state.sessions.selected_session_index, None);
    assert_eq!(selected(&state), serde_json::Value::Null);
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
    state.tick_surfaces(0);
    for c in "staging".chars() {
        state.fleet.update(|fleet| {
            fleet.ask_state.push_char(c);
            true
        });
    }

    state.sessions.selected_session_index = Some(1);
    state.tick_surfaces(0);
    assert_eq!(
        state.fleet.ask_state.free_text(),
        "",
        "the other question starts empty"
    );

    state.sessions.selected_session_index = Some(0);
    state.tick_surfaces(0);
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
    state.tick_surfaces(0);
    let before = state.versions();

    state.tick_surfaces(0);

    assert_eq!(
        before,
        state.versions(),
        "no worker reported and nothing moved, so no section is framed again"
    );
}
