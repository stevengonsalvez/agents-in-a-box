#![allow(missing_docs)]

use ainb::app::mouse::{Gesture, gesture};
use ainb::app::screens::ids as screen_ids;
use ainb::app::state::AppState;
use ainb::app::ui_state::UiState;
use ainb::app::{Btn, Intent, Keymap, Pos, dispatch};
use ainb::components::sidebar::SidebarItem;
use ainb::config::AppConfig;
use std::sync::Mutex;
use tempfile::TempDir;

static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Left-click at (`x`, `y`) through dispatch, with `ui` as the hit-testing host.
fn click(state: &mut AppState, ui: &mut UiState, x: u16, y: u16) {
    let effects = dispatch(
        state,
        &Keymap::defaults(),
        ui,
        Intent::Mouse(Pos { x, y }, Btn::Left),
    );
    assert!(
        effects.is_empty(),
        "the home sidebar asks its host for nothing"
    );
}

#[test]
fn home_sidebar_mouse_click_selects_and_double_click_navigates() {
    // Recover a poisoned lock: these tests mutate $HOME under the shared
    // guard, so a panic in one must not cascade-poison the sibling.
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let temp_home = TempDir::new().unwrap();
    std::env::set_var("HOME", temp_home.path());

    let mut state = AppState::default();
    let mut ui = UiState::default();
    state.shell.current_screen = screen_ids::HOME.to_string();
    state.shell.home_screen_v2_state.last_sidebar_rect =
        Some(ainb::geometry::Area::new(0, 4, 26, 30));

    // Sidebar rect starts at y=4, so first item row is y=7. With Sessions
    // (index 0) selected and thus 2 rows tall, y=10 lands on index 2 =
    // Config per SidebarItem::all().
    click(&mut state, &mut ui, 3, 10);
    assert_eq!(
        state.shell.home_screen_v2_state.sidebar.selected_item(),
        SidebarItem::Config
    );
    assert_eq!(state.shell.current_screen, screen_ids::HOME);

    click(&mut state, &mut ui, 3, 10);
    assert_eq!(
        state.shell.current_screen,
        screen_ids::CONFIG,
        "a double-click opens the item"
    );
}

#[test]
fn home_sidebar_resize_release_persists_width_to_isolated_home() {
    // Recover a poisoned lock: these tests mutate $HOME under the shared
    // guard, so a panic in one must not cascade-poison the sibling.
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let temp_home = TempDir::new().unwrap();
    std::env::set_var("HOME", temp_home.path());

    let mut state = AppState::default();
    let mut ui = UiState::default();
    state.shell.current_screen = screen_ids::HOME.to_string();
    state.shell.home_screen_v2_state.last_sidebar_rect =
        Some(ainb::geometry::Area::new(0, 4, 26, 30));

    click(&mut state, &mut ui, 25, 10);
    assert!(state.shell.home_screen_v2_state.sidebar_resize_active);

    assert!(gesture(Gesture::Drag, Pos { x: 29, y: 10 }, &mut state, &mut ui).is_none());
    assert_eq!(state.shell.home_screen_v2_state.sidebar.preferred_width, 30);

    assert!(gesture(Gesture::Release, Pos { x: 29, y: 10 }, &mut state, &mut ui).is_none());
    assert!(!state.shell.home_screen_v2_state.sidebar_resize_active);

    let loaded = AppConfig::load().unwrap();
    assert_eq!(loaded.ui_preferences.home_sidebar_width, Some(30));

    let restored = AppState::default();
    assert_eq!(
        restored.shell.home_screen_v2_state.sidebar.preferred_width,
        30
    );
}
