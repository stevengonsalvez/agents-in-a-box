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
