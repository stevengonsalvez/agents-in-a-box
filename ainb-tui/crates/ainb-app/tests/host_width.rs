// ABOUTME: Layout clamps read the width of the host that dispatched, so two
// surfaces at different widths driving one crate never share a value.

use ainb_app::app::AppState;
use ainb_app::app::events::{EventHandler, KeyHost};
use ainb_app::app::keymap::{Chord, Keymap, ScrollAction};
use ainb_app::app::screens::ids;
use ainb_app::cli::statusline_install::StatuslineStatus;

/// A host with a fixed surface width and nothing else.
struct Surface(u16);

impl KeyHost for Surface {
    fn queue_scroll(&mut self, _action: ScrollAction) {}

    fn statusline_status(&mut self) -> Option<StatuslineStatus> {
        None
    }

    fn columns(&self) -> Option<u16> {
        Some(self.0)
    }
}

#[test]
fn layout_clamps_read_the_width_of_the_host_that_dispatched() {
    // Resizing persists the width to config.toml under HOME. This binary holds
    // one test, so pointing HOME at a scratch dir cannot race another test.
    let home = tempfile::tempdir().expect("scratch home");
    std::env::set_var("HOME", home.path());
    let keymap = Keymap::defaults();
    let grow = Chord::parse("]").expect("valid chord");
    let mut narrow = AppState::new();
    let mut wide = AppState::new();
    narrow.shell.current_screen = ids::SKILL_MANAGER.to_string();
    wide.shell.current_screen = ids::SKILL_MANAGER.to_string();
    for _ in 0..200 {
        EventHandler::handle_key_event_with_keymap(
            grow.clone(),
            &mut narrow,
            &keymap,
            &mut Surface(80),
        );
        EventHandler::handle_key_event_with_keymap(
            grow.clone(),
            &mut wide,
            &keymap,
            &mut Surface(200),
        );
    }
    // Each panel stops where its own surface leaves room for the Units table.
    let reserve = ainb_app::components::skill_manager_screen::SOURCES_UNITS_RESERVE;
    assert_eq!(
        narrow.skills.skill_manager_state.sources_width,
        80 - reserve
    );
    assert_eq!(wide.skills.skill_manager_state.sources_width, 200 - reserve);
}
