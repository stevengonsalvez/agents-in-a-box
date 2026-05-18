//! Key-dispatch primitive for the BSP prefix-mode UI.
//!
//! Layout follows the tmux mental model: a chord key (default `Ctrl+W`)
//! enters "prefix mode", after which a second keystroke triggers a BSP
//! action (split, close, cycle, resize). [`BspAction`] is the typed output
//! the host event loop dispatches into the layout tree.
//!
//! [`dispatch_key`] is pure — it doesn't read or mutate any global state.
//! The caller passes the current prefix-active flag and the inbound
//! `KeyEvent`, and gets back the action to take (or `None` to let the key
//! fall through to whoever owns it next, e.g. a focused leaf's component).
//!
//! Resize-mode dispatch (`h/j/k/l` after `Ctrl+W r`) is a separate
//! follow-up bead; the foundation here covers the prefix entry / exit
//! and the four headline split/close/cycle actions.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::ui::bsp::SplitDir;

/// All BSP actions the host's main event loop can receive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BspAction {
    /// User pressed the chord key to enter prefix mode.
    EnterPrefix,
    /// User pressed `Esc` (or anything else) from prefix mode without
    /// committing an action.
    ExitPrefix,
    /// User pressed `v` in prefix mode: split the focused tile vertically
    /// (stack the new tile under the focused one).
    SplitVertical,
    /// User pressed `s` in prefix mode: split horizontally (place the
    /// new tile to the right of the focused one).
    SplitHorizontal,
    /// User pressed `x` in prefix mode: close the focused tile.
    CloseFocused,
    /// User pressed `o` in prefix mode: cycle focus forward.
    CycleFocusNext,
    /// User pressed `Tab` in prefix mode: cycle focus forward (alt
    /// binding).
    CycleFocusNextAlt,
    /// User pressed `r` in prefix mode: enter resize mode.
    EnterResize,
}

impl BspAction {
    /// Convert a "split-*" action into the [`SplitDir`] it triggers, if
    /// applicable.
    pub fn as_split_dir(self) -> Option<SplitDir> {
        match self {
            BspAction::SplitVertical => Some(SplitDir::Vertical),
            BspAction::SplitHorizontal => Some(SplitDir::Horizontal),
            _ => None,
        }
    }
}

/// The default chord that enters prefix mode. Matches herdr's tmux-style
/// chord; configurable via `keys.toml [bsp] prefix = "ctrl+b"` in a
/// follow-up bead.
pub const DEFAULT_PREFIX_CHORD: (KeyCode, KeyModifiers) =
    (KeyCode::Char('w'), KeyModifiers::CONTROL);

/// Dispatch a key event against the BSP key table.
///
/// When `prefix_active` is `false`, only the chord key is recognised
/// (returns `Some(EnterPrefix)`); all other keys yield `None` so the
/// caller can route them to the focused leaf's component.
///
/// When `prefix_active` is `true`, the second keystroke is matched
/// against the action table: split/close/cycle/resize. Unknown
/// keys exit prefix mode (`ExitPrefix`) so the user isn't trapped.
pub fn dispatch_key(prefix_active: bool, event: KeyEvent) -> Option<BspAction> {
    if !prefix_active {
        if event.code == DEFAULT_PREFIX_CHORD.0 && event.modifiers == DEFAULT_PREFIX_CHORD.1 {
            return Some(BspAction::EnterPrefix);
        }
        return None;
    }

    // prefix active — match the second keystroke
    match event.code {
        KeyCode::Char('v') => Some(BspAction::SplitVertical),
        KeyCode::Char('s') => Some(BspAction::SplitHorizontal),
        KeyCode::Char('x') => Some(BspAction::CloseFocused),
        KeyCode::Char('o') => Some(BspAction::CycleFocusNext),
        KeyCode::Tab => Some(BspAction::CycleFocusNextAlt),
        KeyCode::Char('r') => Some(BspAction::EnterResize),
        KeyCode::Esc => Some(BspAction::ExitPrefix),
        // anything else exits prefix mode rather than trapping the user
        _ => Some(BspAction::ExitPrefix),
    }
}
