//! One `AppState`, two terminal hosts at different widths: resizing the Skill
//! Manager's Sources panel or the home sidebar on one surface never moves the
//! panel the other draws, and a saved width draws in proportion on both.

#![allow(missing_docs)]

use ainb::app::screens::Screen;
use ainb::app::screens::builtin::{HomeScreen, SkillManagerScreen};
use ainb::app::screens::ids;
use ainb::app::state::AppState;
use ainb::app::ui_state::UiState;
use ainb::app::{Chord, Intent, Keymap, dispatch};
use ainb::components::LayoutComponent;
use ainb::components::skill_manager_screen::{DEFAULT_SOURCES_WIDTH, SOURCES_UNITS_RESERVE};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

static HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A terminal host: its renderer state, its layout and its width.
struct Host {
    ui: UiState,
    layout: LayoutComponent,
    columns: u16,
}

impl Host {
    fn new(columns: u16) -> Self {
        Self {
            ui: UiState::default(),
            layout: LayoutComponent::new(),
            columns,
        }
    }

    /// Press `key` the way the run loop does: dispatch it, apply the layout
    /// work it queued, then dispatch whatever that work saves.
    fn press(&mut self, state: &mut AppState, keymap: &Keymap, key: &str) {
        let chord = Chord::parse(key).expect("valid chord");
        let _ = dispatch(state, keymap, &mut self.ui, Intent::Key(chord));
        for action in self.ui.take_queued() {
            if let Some(save) = self.ui.apply_host(action, &mut self.layout, state, self.columns) {
                let _ = dispatch(state, keymap, &mut self.ui, save);
            }
        }
    }

    /// The home sidebar's width in a frame this host draws.
    fn drawn_home_sidebar(&mut self, state: &AppState) -> u16 {
        let mut terminal =
            Terminal::new(TestBackend::new(self.columns, 40)).expect("test terminal");
        terminal
            .draw(|frame| HomeScreen::default().render(frame, frame.area(), state, &mut self.ui))
            .expect("draw home");
        self.ui.home_sidebar_rect.expect("the sidebar is drawn").width
    }

    /// The column where the Units panel starts in a frame this host draws.
    fn drawn_divider(&mut self, state: &AppState) -> u16 {
        let mut terminal =
            Terminal::new(TestBackend::new(self.columns, 30)).expect("test terminal");
        terminal
            .draw(|frame| SkillManagerScreen.render(frame, frame.area(), state, &mut self.ui))
            .expect("draw skill manager");
        let buffer = terminal.backend().buffer();
        // Both top-row panels are rounded blocks, so the Units panel's
        // top-left corner is the first `╭` after the Sources panel's.
        (1..self.columns)
            .find(|&x| buffer[(x, 0)].symbol() == "╭")
            .expect("the Units panel is drawn")
    }
}

#[test]
fn a_sources_resize_on_one_host_leaves_the_other_hosts_panel_alone() {
    // Each step saves the width to config.toml under HOME; the lock keeps the
    // tests in this binary from swapping HOME under each other.
    let _home_lock = HOME_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let home = tempfile::tempdir().expect("scratch home");
    std::env::set_var("HOME", home.path());
    let keymap = Keymap::defaults();
    let mut state = AppState::new();
    state.shell.current_screen = ids::SKILL_MANAGER.to_string();
    let (mut narrow, mut wide) = (Host::new(80), Host::new(200));

    assert_eq!(narrow.drawn_divider(&state), DEFAULT_SOURCES_WIDTH);
    assert_eq!(wide.drawn_divider(&state), DEFAULT_SOURCES_WIDTH);

    // Grow as far as the wide surface allows.
    for _ in 0..200 {
        wide.press(&mut state, &keymap, "]");
    }
    let wide_max = 200 - SOURCES_UNITS_RESERVE;
    assert_eq!(wide.drawn_divider(&state), wide_max);
    // The narrow host has not resized, so it starts from the saved width and
    // clamps it to its own surface.
    assert_eq!(narrow.drawn_divider(&state), 80 - SOURCES_UNITS_RESERVE);

    // Narrowing on the small surface steps from what that surface draws...
    for _ in 0..5 {
        narrow.press(&mut state, &keymap, "[");
    }
    assert_eq!(
        narrow.drawn_divider(&state),
        80 - SOURCES_UNITS_RESERVE - 10
    );
    // ...and the wide surface keeps the width its user set.
    assert_eq!(wide.drawn_divider(&state), wide_max);
}

#[test]
fn a_saved_width_draws_in_proportion_on_every_host() {
    let _home_lock = HOME_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let home = tempfile::tempdir().expect("scratch home");
    std::env::set_var("HOME", home.path());
    let mut state = AppState::new();
    state.config.app_config.ui_preferences.skill_manager_sources_fraction = Some(0.3);
    state.config.app_config.ui_preferences.home_sidebar_fraction = Some(0.25);
    let (mut narrow, mut wide) = (Host::new(80), Host::new(200));

    assert_eq!(narrow.drawn_divider(&state), 24, "0.3 of 80 columns");
    assert_eq!(wide.drawn_divider(&state), 60, "0.3 of 200 columns");
    assert_eq!(narrow.drawn_home_sidebar(&state), 20, "0.25 of 80 columns");
    assert_eq!(wide.drawn_home_sidebar(&state), 50, "0.25 of 200 columns");
}

#[test]
fn a_home_sidebar_resize_on_one_host_leaves_the_other_hosts_sidebar_alone() {
    let _home_lock = HOME_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let home = tempfile::tempdir().expect("scratch home");
    std::env::set_var("HOME", home.path());
    let state = AppState::new();
    let (mut narrow, mut wide) = (Host::new(80), Host::new(200));
    let default = narrow.drawn_home_sidebar(&state);
    assert_eq!(wide.drawn_home_sidebar(&state), default);

    // The user drags the wide surface's sidebar out to 70 columns.
    wide.ui.home_sidebar.set_width(70);

    assert_eq!(wide.drawn_home_sidebar(&state), 70);
    assert_eq!(
        narrow.drawn_home_sidebar(&state),
        default,
        "the narrow surface keeps its own"
    );
}
