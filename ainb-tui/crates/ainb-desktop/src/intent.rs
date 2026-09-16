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

/// Whether the webview may not send `id`.
///
/// Two families. A row only a host authors (an effect's report, a plugin action
/// naming its plugin) is exempt from the reducer's context gate because a host
/// sends it; the webview is not the host (the desktop's own reports come back
/// through `Shell::tick`). And a row that writes outside ainb runs only from
/// its key ([`KEY_ONLY_COMMANDS`]), whatever surface names it.
///
/// This is the one list: the palette is built from it too, so what the webview
/// may offer and what it may send cannot drift apart.
#[must_use]
pub fn refused_from_webview(id: &CommandId) -> bool {
    ainb_app::app::reports::ids::ALL.contains(&id.as_str())
        || ainb_app::app::plugin_action::ids::ALL.contains(&id.as_str())
        || ainb_app::app::KEY_ONLY_COMMANDS.contains(&id.as_str())
}

/// Whether a palette may offer `row`, which is [`refused_from_webview`] plus
/// the rows that cannot run from a name alone.
///
/// A row a palette names carries no payload, so a row whose action refuses
/// `Args::Null` (a pointer row parsing a position, a step or a character) has
/// nothing to run with and is not offered. Both halves live here, so what a
/// surface may offer and what it may send are one list.
#[must_use]
pub fn palette_offers(id: &CommandId, row: &ainb_app::app::keymap::Binding) -> bool {
    !refused_from_webview(id) && row.action.with_args(&serde_json::Value::Null).is_some()
}

impl TryFrom<RendererIntent> for Intent {
    /// The refused command id the webview tried to send.
    type Error = CommandId;

    fn try_from(intent: RendererIntent) -> Result<Self, CommandId> {
        match intent {
            RendererIntent::Key(chord) => Ok(Self::Key(chord)),
            RendererIntent::Command(id, _) if refused_from_webview(&id) => {
                tracing::warn!("command `{id}` is not the webview's to send; refused");
                Err(id)
            }
            RendererIntent::Command(id, args) => Ok(Self::Command(id, args)),
            RendererIntent::Text(text) => Ok(Self::Text(text)),
        }
    }
}
