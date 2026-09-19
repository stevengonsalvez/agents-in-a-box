// ABOUTME: Pointer commands live in the one keymap registry, take their
// payloads through `KeyAction::with_args`, and name what was hit by identity,
// so a click resolved against an older frame never acts on the wrong row.

use ainb_app::app::NoRenderer;
use ainb_app::app::pointer::{self, ids};
use ainb_app::app::reports;
use ainb_app::app::screens::ids as screen_ids;
use ainb_app::app::state::SessionListRowId;
use ainb_app::models::{Session, Workspace};
use ainb_app::{AppState, CommandId, Intent, Keymap, SectionId, dispatch};

fn bumped(before: &[u64], after: &[u64]) -> Vec<SectionId> {
    SectionId::ALL
        .into_iter()
        .filter(|id| before[id.index()] != after[id.index()])
        .collect()
}

#[test]
fn every_pointer_command_is_an_unbound_row_in_the_one_registry() {
    let keymap = Keymap::defaults();
    let listed: Vec<String> = keymap.commands().map(|(id, _)| id.to_string()).collect();
    for id in ids::ALL {
        assert!(
            listed.iter().any(|candidate| candidate == id),
            "{id} is not listed"
        );
        let row = keymap.command(&CommandId::new(*id)).expect("row resolves");
        assert!(row.chord.is_none(), "{id} has a key");
    }
    let mut unbound: Vec<&str> = listed
        .iter()
        .map(String::as_str)
        .filter(|id| keymap.command(&CommandId::new(*id)).is_some_and(|row| row.chord.is_none()))
        .collect();
    let mut host_commands: Vec<&str> = ids::ALL
        .iter()
        .chain(reports::ids::ALL)
        .chain(ainb_app::app::plugin_action::ids::ALL)
        // The slash palette's commands that run from any screen.
        .chain(&["global.open_learnings"])
        .copied()
        .collect();
    unbound.sort_unstable();
    host_commands.sort_unstable();
    assert_eq!(
        unbound, host_commands,
        "only pointer, report, plugin action and slash palette commands are unbound"
    );
}

#[test]
fn a_pointer_command_run_without_its_payload_changes_nothing() {
    let keymap = Keymap::defaults();
    for id in ids::ALL.iter().chain(reports::ids::ALL) {
        let row = keymap.command(&CommandId::new(*id)).expect("row resolves");
        if row.action.with_args(&serde_json::Value::Null).is_some() {
            // The rows that take no payload.
            assert!(
                [ids::SKILL_MANAGER_ALL_SOURCES, reports::ids::DETACHED].contains(id),
                "{id} runs bare"
            );
            continue;
        }
        let mut state = AppState::new();
        state.shell.current_screen = screen_ids::SESSION_LIST.to_string();
        let before = state.versions();
        let effects = dispatch(
            &mut state,
            &keymap,
            &mut NoRenderer,
            Intent::Command(CommandId::new(*id), serde_json::Value::Null),
        );
        assert!(effects.is_empty(), "{id}");
        assert!(bumped(&before, &state.versions()).is_empty(), "{id}");
    }
}

/// A list of two workspaces, one session each, with nothing selected.
fn two_workspaces() -> AppState {
    let mut state = AppState::new();
    state.shell.current_screen = screen_ids::SESSION_LIST.to_string();
    let mut first = Workspace::new("api".to_string(), "/parity/api".into());
    first.add_session(Session::new(
        "feat-login".to_string(),
        "/parity/api/wt".to_string(),
    ));
    let mut second = Workspace::new("web".to_string(), "/parity/web".into());
    second.add_session(Session::new(
        "spike-ssr".to_string(),
        "/parity/web/wt".to_string(),
    ));
    state.sessions.workspaces = vec![first, second];
    state.sessions.selected_workspace_index = None;
    state.sessions.selected_session_index = None;
    state
}

#[test]
fn a_click_captured_before_its_workspace_is_removed_selects_nothing() {
    let keymap = Keymap::defaults();
    let mut state = two_workspaces();
    let removed_session = state.sessions.workspaces[0].sessions[0].id;
    // The frame the user clicked showed feat-login in the first workspace.
    let click = pointer::select_session_row(&SessionListRowId::Session(removed_session), false);

    // A refresh lands before the click is applied and drops that workspace, so
    // spike-ssr now sits where feat-login was.
    state.sessions.workspaces.remove(0);
    let before = state.versions();

    let effects = dispatch(&mut state, &keymap, &mut NoRenderer, click);

    assert!(effects.is_empty());
    assert_eq!(
        state.sessions.selected_workspace_index, None,
        "no workspace selected"
    );
    assert_eq!(
        state.sessions.selected_session_index, None,
        "no session selected"
    );
    assert!(bumped(&before, &state.versions()).is_empty());
}

#[test]
fn a_click_on_a_row_that_moved_selects_that_row_where_it_is_now() {
    let keymap = Keymap::defaults();
    let mut state = two_workspaces();
    let kept_session = state.sessions.workspaces[1].sessions[0].id;
    let click = pointer::select_session_row(&SessionListRowId::Session(kept_session), false);

    state.sessions.workspaces.remove(0);
    let _ = dispatch(&mut state, &keymap, &mut NoRenderer, click);

    assert_eq!(state.sessions.selected_workspace_index, Some(0));
    assert_eq!(state.sessions.selected_session_index, Some(0));
    assert_eq!(
        state.sessions.workspaces[0].sessions[0].id, kept_session,
        "the session the user clicked, not whatever took its old position"
    );
}

#[test]
fn row_identities_round_trip_through_the_list() {
    let state = two_workspaces();
    let mut row = 0;
    while let Some(target) = state.session_list_row_target(row) {
        let id = state.session_list_row_id(target).expect("every listed row has an identity");
        assert_eq!(
            state.session_list_row_target_for(&id),
            Some(target),
            "row {row}"
        );
        row += 1;
    }
    assert!(row > 0, "the fixture lists rows");
}

#[test]
fn a_click_on_the_strip_shows_the_tab_it_names_unless_that_tab_is_dead() {
    use ainb_app::components::session_tabs::SessionTab;
    let keymap = Keymap::defaults();
    let mut state = two_workspaces();

    let _ = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        pointer::select_session_tab(SessionTab::Pal),
    );
    assert_eq!(state.shell.session_tab, SessionTab::Pal);

    // Nothing is selected, so `log` is disabled; the click lands where the key
    // would have put it rather than on a pane that cannot draw.
    let _ = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        pointer::select_session_tab(SessionTab::Log),
    );
    assert_eq!(state.shell.session_tab, SessionTab::Preview);
}

#[test]
fn a_palette_cannot_offer_the_tab_click() {
    // A palette row runs with no payload. `app::palette::nameable` keeps a row
    // out when its action refuses `Args::Null`, so this is the property that
    // keeps `select_tab` off the palette: a palette that named it would offer a
    // row that cannot run.
    let keymap = Keymap::defaults();
    let row = keymap
        .command(&CommandId::new(ids::SESSION_LIST_SELECT_TAB))
        .expect("the row resolves");
    assert!(row.action.with_args(&serde_json::Value::Null).is_none());
    assert!(row.chord.is_none(), "and no key reaches it either");
}

#[test]
fn a_command_scoped_to_another_screen_changes_nothing() {
    let keymap = Keymap::defaults();
    let mut state = AppState::new();
    state.shell.current_screen = screen_ids::SESSION_LIST.to_string();
    let before = state.versions();

    let effects = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        Intent::Command(CommandId::new("home.learnings"), serde_json::Value::Null),
    );

    assert!(effects.is_empty());
    assert_eq!(state.shell.current_screen, screen_ids::SESSION_LIST);
    assert!(bumped(&before, &state.versions()).is_empty());

    // The same row runs where its context is active.
    state.shell.current_screen = screen_ids::HOME.to_string();
    let _ = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        Intent::Command(CommandId::new("home.learnings"), serde_json::Value::Null),
    );
    assert_eq!(state.shell.current_screen, screen_ids::LEARNINGS);
}

#[test]
fn the_slash_palette_opens_learnings_from_any_screen() {
    let keymap = Keymap::defaults();
    let mut state = AppState::new();
    state.shell.current_screen = screen_ids::SESSION_LIST.to_string();
    let intent = ainb_app::app::slash_command_intent("recall").expect("recall maps");

    let _ = dispatch(&mut state, &keymap, &mut NoRenderer, intent);

    assert_eq!(state.shell.current_screen, screen_ids::LEARNINGS);
}

/// One selected session blocking on a daemon question with `options`.
fn waiting_on(options: &[&str]) -> (AppState, ainb_app::fleet::attention::SessionAttention) {
    use ainb_app::fleet::attention::{AttentionKind, AttentionOption, SessionAttention};
    let chip = SessionAttention::daemon(AttentionKind::Ask, 1_000, "att-7".into()).with_options(
        options
            .iter()
            .map(|label| AttentionOption {
                label: (*label).to_string(),
                description: String::new(),
            })
            .collect(),
    );
    let mut state = AppState::new();
    state.shell.current_screen = screen_ids::SESSION_LIST.to_string();
    let mut workspace = Workspace::new("api".to_string(), "/parity/api".into());
    let mut session = Session::new("feat-login".to_string(), "/parity/api/wt".to_string());
    session.live_attention = vec![chip.clone()];
    workspace.add_session(session);
    state.sessions.workspaces = vec![workspace];
    state.sessions.selected_workspace_index = Some(0);
    state.sessions.selected_session_index = Some(0);
    state.shell.session_tab = ainb_app::components::session_tabs::SessionTab::Ask;
    (state, chip)
}

#[test]
fn a_pick_by_index_sends_that_option_as_a_pick_not_a_draft() {
    use ainb_app::fleet::answer::AnswerPhase;
    let keymap = Keymap::defaults();
    let (mut state, chip) = waiting_on(&["staging", "production", "local"]);

    let _ = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        pointer::pick_answer("att-7", 2, "local"),
    );

    assert_eq!(state.fleet.ask_state.cursor(), 2);
    assert_eq!(
        state.fleet.ask_state.answer_text(&chip).as_deref(),
        Ok("local")
    );
    assert!(
        matches!(
            state.fleet.ask_state.phase_for(&chip),
            Some(AnswerPhase::InFlight { draft: None, .. })
        ),
        "the send is out, as a pick rather than a draft: {:?}",
        state.fleet.ask_state.phase_for(&chip)
    );
}

#[test]
fn a_pick_against_options_that_reordered_under_it_sends_nothing_and_says_so() {
    // The frame the person clicked offered staging, production, local, and
    // they clicked the third. Before the click lands, a refresh reorders the
    // options: position 2 is not what they read there, so nothing goes out.
    let keymap = Keymap::defaults();
    let (mut state, chip) = waiting_on(&["local", "production", "staging"]);

    let _ = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        pointer::pick_answer("att-7", 2, "local"),
    );

    assert!(
        state.fleet.ask_state.phase_for(&chip).is_none(),
        "nothing went out"
    );
    assert_eq!(state.fleet.ask_state.cursor(), 0, "the cursor did not move");
    assert_eq!(
        state.shell.notifications.last().map(|note| note.message.as_str()),
        Some("the options changed under the pick; read them again")
    );
}

#[test]
fn a_pick_of_a_label_the_question_does_not_offer_sends_nothing_and_says_so() {
    let keymap = Keymap::defaults();
    let (mut state, chip) = waiting_on(&["staging", "production"]);

    let _ = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        pointer::pick_answer("att-7", 5, "prod"),
    );

    assert!(
        state.fleet.ask_state.phase_for(&chip).is_none(),
        "nothing went out"
    );
    assert_eq!(state.fleet.ask_state.cursor(), 0, "the cursor did not move");
    assert_eq!(
        state.shell.notifications.last().map(|note| note.message.as_str()),
        Some("that option is not offered here")
    );
}

#[test]
fn a_pick_naming_the_question_that_moved_on_sends_nothing() {
    // The frame the person read carried att-6 with the same labels. Before the
    // click lands, that question is answered elsewhere and att-7 takes its
    // place on the row: a pick against att-6 must not answer att-7, however
    // ordinary the label.
    let keymap = Keymap::defaults();
    let (mut state, chip) = waiting_on(&["yes", "no"]);
    let before = state.versions();

    let _ = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        pointer::pick_answer("att-6", 0, "yes"),
    );

    assert!(
        state.fleet.ask_state.phase_for(&chip).is_none(),
        "nothing went out"
    );
    assert_eq!(
        state.shell.notifications.last().map(|note| note.message.as_str()),
        Some("that question has moved on; read the new one")
    );
    assert!(
        bumped(&before, &state.versions()).iter().all(|id| *id == SectionId::Shell),
        "only the notice moved"
    );
}

#[test]
fn a_pick_runs_only_while_the_ask_pane_is_showing() {
    // The row sits in the `ask` context, as its keys do: with the pane on
    // another tab the command is not active, so it changes nothing rather than
    // answering a question the person is not looking at.
    let keymap = Keymap::defaults();
    let (mut state, chip) = waiting_on(&["staging", "production"]);
    state.shell.session_tab = ainb_app::components::session_tabs::SessionTab::Preview;
    let before = state.versions();

    let effects = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        pointer::pick_answer("att-7", 0, "staging"),
    );

    assert!(effects.is_empty());
    assert!(state.fleet.ask_state.phase_for(&chip).is_none());
    assert!(bumped(&before, &state.versions()).is_empty());
}
