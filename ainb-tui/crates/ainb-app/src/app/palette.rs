// ABOUTME: The rows a palette may offer, owned by the reducer so every surface
// asks one list rather than each building its own (#1161).
//
// The split this draws, and why it is here rather than in a host:
//
//   reducer  what rows exist, what each says, which context it belongs to,
//            whether the reducer would run it now, and whether it can be named
//            at all (a row that writes outside ainb runs only from its key; a
//            row whose action refuses `Args::Null` has no payload to run with)
//   surface  whether THIS surface may send it, and how to rank and draw it
//
// A surface that filtered "what may be named" itself is how two surfaces end up
// offering different rows for one state, which is the drift this removes.

use crate::app::keymap::{Binding, KeyContext};
use crate::app::{AppState, CommandId, Keymap};

/// One row a palette may offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteRow {
    /// The command a surface sends to run it.
    pub id: CommandId,
    /// What the row does, as the keymap documents it.
    pub doc: &'static str,
    /// The context the row belongs to, for a palette to group by.
    pub context: KeyContext,
    /// The key that runs it, when it has one.
    pub chord: Option<String>,
    /// Whether the reducer would run it in the state as it stands. A row that
    /// is not active is still offered, greyed, rather than vanishing as the
    /// person moves around.
    pub active: bool,
}

/// Whether `binding` can be run by name at all, whatever surface asks.
///
/// Two reducer facts, not surface taste: a row whose action writes outside ainb
/// runs only from its key ([`Binding::key_only`]), and a row whose action
/// refuses `Args::Null` (a pointer row parsing a position, a step, a character)
/// has nothing to run with when a palette names it carrying no payload.
#[must_use]
pub fn nameable(binding: &Binding) -> bool {
    !binding.key_only() && binding.action.with_args(&serde_json::Value::Null).is_some()
}

/// Every row a palette may offer in `state`, in the keymap's own order.
///
/// A surface narrows this by what IT may send (the desktop refuses
/// host-authored ids, which no person types), ranks it, and draws it.
#[must_use]
pub fn rows(state: &AppState, keymap: &Keymap) -> Vec<PaletteRow> {
    let contexts = crate::app::keymap::command_contexts(state);
    keymap
        .commands()
        .filter(|(_, binding)| nameable(binding))
        .map(|(id, binding)| PaletteRow {
            id,
            doc: binding.doc,
            context: binding.ctx.clone(),
            chord: binding.chord.as_ref().map(|chord| chord.as_str().to_string()),
            active: contexts.contains(&binding.ctx),
        })
        .collect()
}
