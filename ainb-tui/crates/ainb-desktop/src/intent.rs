//! What the webview may ask of the host.

use ainb_app::{Chord, CommandId, Intent};
use serde::Deserialize;

/// The intents a DOM renderer sends: a key, a named command, pasted text.
///
/// A subset of [`Intent`] with the same wire spelling. There is no `Mouse`:
/// the webview hit-tests its own DOM and sends the command a press means, so a
/// terminal cell position has nothing to hit here.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
pub enum RendererIntent {
    // A chord and a command id are strings on the wire by their own serde
    // attributes, and an argument payload is arbitrary JSON; named here so this
    // crate's bindings do not depend on how `ainb-app` was built.
    Key(#[cfg_attr(feature = "typescript-bindings", specta(type = String))] Chord),
    Command(
        #[cfg_attr(feature = "typescript-bindings", specta(type = String))] CommandId,
        #[cfg_attr(feature = "typescript-bindings", specta(type = specta_typescript::Unknown))]
        ainb_app::app::Args,
    ),
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
            RendererIntent::Text(text) => Ok(Self::Text(typed_text(&text))),
        }
    }
}

/// The most characters one `Text` intent from the webview carries.
///
/// The composer's `maxlength` is the page's to honour or not; this is the
/// host's, and a script in the page cannot talk past it.
pub const MAX_TEXT_CHARS: usize = 2_000;

/// `text` as the reducer may receive it from the webview: control and format
/// characters removed, then cut to [`MAX_TEXT_CHARS`].
///
/// Format characters (Unicode `Cf`: bidi overrides and isolates, zero-width
/// joiners and spaces, the byte-order mark, tag characters) are invisible where
/// every display path strips them, so a bidi override typed or pasted into an
/// answer would reach the agent's composer reading differently from what the
/// person saw. Control characters would submit or jump fields mid-text.
#[must_use]
pub fn typed_text(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() && !is_format(*c))
        .take(MAX_TEXT_CHARS)
        .collect()
}

/// Whether `c` is in Unicode's `Cf` (format) category.
fn is_format(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{0600}'..='\u{0605}'
            | '\u{061C}'
            | '\u{06DD}'
            | '\u{070F}'
            | '\u{0890}'..='\u{0891}'
            | '\u{08E2}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206F}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{110BD}'
            | '\u{110CD}'
            | '\u{13430}'..='\u{1343F}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0001}'
            | '\u{E0020}'..='\u{E007F}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_text_loses_what_a_person_cannot_see() {
        let override_ = "\u{202E}";
        let typed = typed_text(&format!("stag{override_}ing\u{200B}\u{0007}"));
        assert_eq!(typed, "staging");
    }

    #[test]
    fn typed_text_is_capped_on_characters() {
        let long = "é".repeat(MAX_TEXT_CHARS + 10);
        assert_eq!(typed_text(&long).chars().count(), MAX_TEXT_CHARS);
    }
}
