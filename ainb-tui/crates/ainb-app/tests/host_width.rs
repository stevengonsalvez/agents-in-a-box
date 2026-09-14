// ABOUTME: A panel width is layout, so a resize key goes to the host that
// dispatched it and never into the state two surfaces share.

use ainb_app::app::RendererHost;
use ainb_app::app::keymap::HostAction;
use ainb_app::app::screens::ids;
use ainb_app::{AppState, Btn, CommandId, Intent, Keymap, Pos, dispatch};

/// A host that records the layout work handed to it.
#[derive(Default)]
struct Surface(Vec<HostAction>);

impl RendererHost for Surface {
    fn queue(&mut self, action: HostAction) {
        self.0.push(action);
    }

    fn pointer(&mut self, _state: &AppState, _pos: Pos, _btn: Btn) -> Option<Intent> {
        None
    }
}

#[test]
fn a_resize_step_reaches_only_the_host_that_dispatched_it() {
    let keymap = Keymap::defaults();
    let mut state = AppState::new();
    state.shell.current_screen = ids::SKILL_MANAGER.to_string();
    let step = |name: &str| Intent::Command(CommandId::new(name), serde_json::Value::Null);
    let (mut narrow, mut wide) = (Surface::default(), Surface::default());
    let before = state.versions();

    let effects = dispatch(
        &mut state,
        &keymap,
        &mut wide,
        step("skill_manager.grow_sources"),
    );
    dispatch(
        &mut state,
        &keymap,
        &mut narrow,
        step("skill_manager.shrink_sources"),
    );

    assert!(effects.is_empty());
    assert_eq!(wide.0, [HostAction::GrowSkillSources]);
    assert_eq!(narrow.0, [HostAction::ShrinkSkillSources]);
    assert_eq!(
        state.versions(),
        before,
        "the shared state holds no panel width"
    );
}
