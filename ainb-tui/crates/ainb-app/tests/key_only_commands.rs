#![allow(missing_docs)]

// ABOUTME: A row that writes outside ainb runs from its key and never from a
// command a palette, a click or another host could send by name.

#[path = "support/home.rs"]
mod home;

use home::ScopedHome;

use ainb_app::app::NoRenderer;
use ainb_app::{AppState, Chord, CommandId, Intent, Keymap, dispatch};

/// `W` wires Claude Code's statusline into `~/.claude/settings.json`. Sent as a
/// command it does nothing; pressed, it does what it always did.
#[test]
fn wiring_the_statusline_runs_from_its_key_and_not_by_name() {
    let home = ScopedHome::new();
    let settings = home.path().join(".claude").join("settings.json");
    let keymap = Keymap::defaults();
    let mut state = AppState::new();

    let _ = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        Intent::Command(
            CommandId::new("global.wire_statusline"),
            serde_json::Value::Null,
        ),
    );
    assert!(
        !settings.exists(),
        "the command must not touch settings.json"
    );
    assert!(
        state.shell.notifications.is_empty(),
        "the command must not attempt the install"
    );

    let _ = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        Intent::Key(Chord::parse("W").expect("valid chord")),
    );
    assert!(
        settings.exists() || !state.shell.notifications.is_empty(),
        "the key still wires the statusline (or reports why it could not)"
    );
}
