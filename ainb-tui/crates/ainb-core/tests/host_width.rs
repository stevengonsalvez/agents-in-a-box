//! One `AppState`, two terminal hosts at different widths: resizing the Skill
//! Manager's Sources panel on one surface never moves the panel the other
//! draws.

#![allow(missing_docs)]

use ainb::app::screens::Screen;
use ainb::app::screens::builtin::SkillManagerScreen;
use ainb::app::screens::ids;
use ainb::app::state::AppState;
use ainb::app::ui_state::UiState;
use ainb::app::{Chord, Intent, Keymap, dispatch};
use ainb::components::LayoutComponent;
use ainb::components::skill_manager_screen::{DEFAULT_SOURCES_WIDTH, SOURCES_UNITS_RESERVE};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

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
        dispatch(state, keymap, &mut self.ui, Intent::Key(chord));
        for action in self.ui.take_queued() {
            if let Some(save) = self.ui.apply_host(action, &mut self.layout, state, self.columns) {
                dispatch(state, keymap, &mut self.ui, save);
            }
        }
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
    // Each step saves the width to config.toml under HOME. This binary holds
    // one test, so pointing HOME at a scratch dir cannot race another test.
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
