#![allow(missing_docs)]

// ABOUTME: The settings tree's selection is the reducer's (D3d): a click on a
// node names it by id, `config.select_node`, and the reducer moves the
// selection and the right pane's rows. A renderer keeps no selection of its
// own, so two surfaces drawing one frame show the same rows.

#[path = "support/home.rs"]
mod home;

use ainb_app::app::NoRenderer;
use ainb_app::app::pointer;
use ainb_app::app::screens::ids as screen_ids;
use ainb_app::{AppState, Keymap, SectionId, dispatch};

fn on_config() -> AppState {
    home::shared();
    let mut state = AppState::new();
    state.shell.current_screen = screen_ids::CONFIG.to_string();
    state
}

#[test]
fn a_click_on_a_visible_node_selects_it_and_shows_its_rows() {
    let keymap = Keymap::defaults();
    let mut state = on_config();
    let screen = &state.config.config_screen_state;
    // A root other than the first, so the selection has to move.
    let target = screen
        .visible_nodes
        .iter()
        .map(|index| &screen.tree[*index])
        .find(|node| node.depth == 0 && node.category != screen.tree[0].category)
        .expect("a second category root");
    let (id, category, rows) = (target.id(), target.category, target.rows.clone());
    let before = state.versions();

    let _ = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        pointer::select_config_node(&id),
    );

    let screen = &state.config.config_screen_state;
    assert_eq!(screen.current_node().map(|node| node.id()), Some(id));
    assert_eq!(screen.selected_setting, 0);
    assert_eq!(
        screen.visible_rows,
        rows.into_iter().map(|index| (category, index)).collect::<Vec<_>>(),
        "the right pane shows the node's subtree"
    );
    let moved: Vec<SectionId> = SectionId::ALL
        .into_iter()
        .filter(|id| before[id.index()] != state.versions()[id.index()])
        .collect();
    assert!(moved.contains(&SectionId::Config));
}

#[test]
fn a_click_on_a_node_that_is_not_on_screen_changes_nothing() {
    let keymap = Keymap::defaults();
    let mut state = on_config();
    let screen = &state.config.config_screen_state;
    let hidden = screen
        .tree
        .iter()
        .enumerate()
        .find(|(index, node)| node.depth > 0 && !screen.visible_nodes.contains(index))
        .map(|(_, node)| node.id())
        .expect("a collapsed child node");
    let before = state.versions();

    let _ = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        pointer::select_config_node(&hidden),
    );
    let _ = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        pointer::select_config_node("no|such"),
    );

    assert_eq!(before, state.versions());
    assert_eq!(state.config.config_screen_state.selected_node, 0);
}

#[test]
fn the_node_click_runs_only_on_the_config_screen() {
    let keymap = Keymap::defaults();
    let mut state = on_config();
    let id = state.config.config_screen_state.tree[1].id();
    state.shell.current_screen = screen_ids::SESSION_LIST.to_string();
    let before = state.versions();
    let _ = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        pointer::select_config_node(&id),
    );
    assert_eq!(before, state.versions());
}
