//! What the webview may ask of the host.

use ainb_app::{Chord, CommandId, Intent};
use serde::Deserialize;

/// The intents a DOM renderer sends: a key, a named command, pasted text.
///
/// A subset of [`Intent`] with the same wire spelling. There is no `Mouse`:
/// the webview hit-tests its own DOM and sends the command a press means, so a
/// terminal cell position has nothing to hit here.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub enum RendererIntent {
    Key(Chord),
    Command(CommandId, ainb_app::app::Args),
    Text(String),
}

impl From<RendererIntent> for Intent {
    fn from(intent: RendererIntent) -> Self {
        match intent {
            RendererIntent::Key(chord) => Self::Key(chord),
            RendererIntent::Command(id, args) => Self::Command(id, args),
            RendererIntent::Text(text) => Self::Text(text),
        }
    }
}
