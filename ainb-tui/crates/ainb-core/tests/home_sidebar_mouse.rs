#![allow(missing_docs)]

use ainb::app::events::{AppEvent, EventHandler};
use ainb::app::screens::ids as screen_ids;
use ainb::app::state::AppState;
use ainb::components::sidebar::SidebarItem;
use ainb::config::AppConfig;
use ratatui::layout::Rect;
use std::sync::Mutex;
use tempfile::TempDir;

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn home_sidebar_mouse_click_selects_and_double_click_navigates() {
    let _guard = ENV_LOCK.lock().unwrap();
    let temp_home = TempDir::new().unwrap();
    std::env::set_var("HOME", temp_home.path());

    let mut state = AppState::default();
    state.current_screen = screen_ids::HOME.to_string();
    state.home_screen_v2_state.last_sidebar_rect = Some(Rect::new(0, 4, 26, 30));

    let first = EventHandler::handle_mouse_event(AppEvent::MouseClick { x: 3, y: 10 }, &mut state);
    assert!(first.is_none());
    assert_eq!(
        state.home_screen_v2_state.sidebar.selected_item(),
        SidebarItem::Inbox
    );

    let second = EventHandler::handle_mouse_event(AppEvent::MouseClick { x: 3, y: 10 }, &mut state);
    assert!(matches!(second, Some(AppEvent::HomeScreenSidebarSelect)));
}

#[test]
fn home_sidebar_resize_release_persists_width_to_isolated_home() {
    let _guard = ENV_LOCK.lock().unwrap();
    let temp_home = TempDir::new().unwrap();
    std::env::set_var("HOME", temp_home.path());

    let mut state = AppState::default();
    state.current_screen = screen_ids::HOME.to_string();
    state.home_screen_v2_state.last_sidebar_rect = Some(Rect::new(0, 4, 26, 30));

    let down = EventHandler::handle_mouse_event(AppEvent::MouseClick { x: 25, y: 10 }, &mut state);
    assert!(down.is_none());
    assert!(state.home_screen_v2_state.sidebar_resize_active);

    let drag =
        EventHandler::handle_mouse_event(AppEvent::MouseDragging { x: 44, y: 10 }, &mut state);
    assert!(drag.is_none());
    assert_eq!(state.home_screen_v2_state.sidebar.preferred_width, 30);

    let up = EventHandler::handle_mouse_event(AppEvent::MouseDragEnd { x: 44, y: 10 }, &mut state);
    assert!(up.is_none());
    assert!(!state.home_screen_v2_state.sidebar_resize_active);

    let loaded = AppConfig::load().unwrap();
    assert_eq!(loaded.ui_preferences.home_sidebar_width, Some(30));

    let restored = AppState::default();
    assert_eq!(restored.home_screen_v2_state.sidebar.preferred_width, 30);
}
