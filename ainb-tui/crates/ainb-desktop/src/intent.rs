//! What the webview may ask of the host.

use ainb_app::{Chord, CommandId, Intent, Keymap};
use serde::{Deserialize, Serialize};

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

/// Why an intent from the webview was not applied, for the webview to show:
/// the row it would have run and the reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Refusal {
    pub command: CommandId,
    pub reason: &'static str,
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

/// Whether the webview may not send `id`.
///
/// Two families: a host-authored row ([`is_host_authored`]), and a row that
/// writes outside ainb, which runs only from its key
/// ([`Keymap::is_key_only`]), whatever surface names it.
///
/// This is the one list: the palette is built from it too, so what the webview
/// may offer and what it may send cannot drift apart.
#[must_use]
pub fn refused_from_webview(keymap: &Keymap, id: &CommandId) -> bool {
    is_host_authored(id) || keymap.is_key_only(id)
}

/// Whether a palette may offer `row`, which is [`refused_from_webview`] plus
/// the rows that cannot run from a name alone.
///
/// A row a palette names carries no payload, so a row whose action refuses
/// `Args::Null` (a pointer row parsing a position, a step or a character) has
/// nothing to run with and is not offered. Both halves live here, so what a
/// surface may offer and what it may send are one list.
#[must_use]
pub fn palette_offers(
    keymap: &Keymap,
    id: &CommandId,
    row: &ainb_app::app::keymap::Binding,
) -> bool {
    !refused_from_webview(keymap, id) && row.action.with_args(&serde_json::Value::Null).is_some()
}

impl TryFrom<RendererIntent> for Intent {
    /// The host-authored command the webview tried to send. A key-only id
    /// passes here and is refused by the shell, which holds the keymap.
    type Error = Refusal;

    fn try_from(intent: RendererIntent) -> Result<Self, Refusal> {
        match intent {
            RendererIntent::Key(chord) => Ok(Self::Key(chord)),
            RendererIntent::Command(id, _) if is_host_authored(&id) => {
                tracing::warn!("command `{id}` is host-authored; refused from the webview");
                Err(Refusal {
                    command: id,
                    reason: "a host sends it, not the window",
                })
            }
            RendererIntent::Command(id, args) => Ok(Self::Command(id, args)),
            RendererIntent::Text(text) => Ok(Self::Text(text)),
        }
    }
}
