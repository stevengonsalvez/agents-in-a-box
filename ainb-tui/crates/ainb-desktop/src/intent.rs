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

/// Whether `id` names a row only a host authors: an effect's report, or a
/// plugin action naming its plugin.
///
/// The reducer exempts those rows from the context gate because a host sends
/// them. The webview is not the host (the desktop's own reports come back
/// through `Shell::tick`), so from the webview they are refused outright.
#[must_use]
pub fn is_host_authored(id: &CommandId) -> bool {
    ainb_app::app::reports::ids::ALL.contains(&id.as_str())
        || ainb_app::app::plugin_action::ids::ALL.contains(&id.as_str())
}

impl TryFrom<RendererIntent> for Intent {
    /// The host-authored command id the webview tried to send.
    type Error = CommandId;

    fn try_from(intent: RendererIntent) -> Result<Self, CommandId> {
        match intent {
            RendererIntent::Key(chord) => Ok(Self::Key(chord)),
            RendererIntent::Command(id, _) if is_host_authored(&id) => {
                tracing::warn!("command `{id}` is host-authored; refused from the webview");
                Err(id)
            }
            RendererIntent::Command(id, args) => Ok(Self::Command(id, args)),
            RendererIntent::Text(text) => Ok(Self::Text(text)),
        }
    }
}
