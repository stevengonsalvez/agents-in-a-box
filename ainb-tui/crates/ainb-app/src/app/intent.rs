// ABOUTME: The renderer contract's input side. Every surface turns what the
// user did into an `Intent` and hands it to `dispatch`; the event enum the
// reducer matches on stays behind this boundary.

use serde::{Deserialize, Serialize};

use crate::app::effect::Effect;
use crate::app::events::{EventHandler, RendererHost};
use crate::app::keymap::{Chord, CommandId, Keymap};
use crate::app::state::AppState;

/// Something a user did, in terms every renderer can produce.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Intent {
    /// A key press, resolved through the merged keymap.
    Key(Chord),
    /// A keymap command invoked by name, from a palette or a click.
    Command(CommandId, Args),
    /// A pointer press at a cell of the renderer's last frame.
    Mouse(Pos, Btn),
    /// Text entered in one go: a paste, or IME composition.
    Text(String),
}

/// Arguments to a [`Intent::Command`]. `Args::Null` runs the row as the keymap
/// wrote it; a row that carries a payload (a session position, a step, a
/// character) takes a replacement payload here instead. See
/// [`crate::app::keymap::KeyAction::with_args`].
pub type Args = serde_json::Value;

/// A cell position in the renderer's frame, column then row from top left.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Pos {
    pub x: u16,
    pub y: u16,
}

/// The pointer button pressed. Wheel motion is not an intent: scroll position
/// belongs to the renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Btn {
    Left,
    Right,
    Middle,
}

/// Apply one intent to `state` and return the effects it queued, for the host
/// to execute now that the state is written.
///
/// This is the whole input surface for a renderer with no host-side events of
/// its own. The TUI host uses [`EventHandler::resolve_intent`] instead,
/// because it handles a few resolved events (embed sizing, sidebar collapse)
/// against its own layout before the reducer sees the rest.
pub fn dispatch(
    state: &mut AppState,
    keymap: &Keymap,
    host: &mut dyn RendererHost,
    intent: Intent,
) -> Vec<Effect> {
    if let Some(event) = EventHandler::resolve_intent(intent, state, keymap, host) {
        EventHandler::process_event(event, state);
    }
    state.take_effects()
}
