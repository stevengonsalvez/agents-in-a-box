// ABOUTME: Layout clamps read the width of the host that dispatched, so two
// surfaces at different widths driving one crate never share a value.

use ainb_app::app::RendererHost;
use ainb_app::app::events::AppEvent;
use ainb_app::app::keymap::ScrollAction;
use ainb_app::cli::statusline_install::StatuslineStatus;
use ainb_app::{AppState, Btn, CommandId, Intent, Keymap, Pos, dispatch};
/// A host with a fixed surface width and nothing else.
struct Surface(u16);

impl RendererHost for Surface {
    fn queue_scroll(&mut self, _action: ScrollAction) {}

    fn statusline_status(&mut self) -> Option<StatuslineStatus> {
        None
    }

    fn columns(&self) -> Option<u16> {
        Some(self.0)
    }

    fn pointer(&mut self, _state: &mut AppState, _pos: Pos, _btn: Btn) -> Option<AppEvent> {
        None
    }
}

#[test]
fn layout_clamps_read_the_width_of_the_host_that_dispatched() {
    // Resizing persists the width to config.toml under HOME. This binary holds
    // one test, so pointing HOME at a scratch dir cannot race another test.
    let home = tempfile::tempdir().expect("scratch home");
    std::env::set_var("HOME", home.path());
    let keymap = Keymap::defaults();
    let grow = || {
        Intent::Command(
            CommandId::new("skill_manager.grow_sources"),
            serde_json::Value::Null,
        )
    };
    let mut narrow = AppState::new();
    let mut wide = AppState::new();
    for _ in 0..200 {
        dispatch(&mut narrow, &keymap, &mut Surface(80), grow());
        dispatch(&mut wide, &keymap, &mut Surface(200), grow());
    }
    // Each panel stops where its own surface leaves room for the Units table.
    let reserve = ainb_app::components::skill_manager_screen::SOURCES_UNITS_RESERVE;
    assert_eq!(
        narrow.skills.skill_manager_state.sources_width,
        80 - reserve
    );
    assert_eq!(wide.skills.skill_manager_state.sources_width, 200 - reserve);
}
