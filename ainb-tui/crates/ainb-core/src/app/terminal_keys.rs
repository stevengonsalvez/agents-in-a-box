// ABOUTME: The TUI's key input edge: turns crossterm key events into the
// renderer-agnostic `Chord` that key dispatch in `ainb-app` consumes. Nothing
// past this module sees a crossterm key type.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::keymap::{Chord, Key, Mods};

/// The chord a crossterm key event stands for, or `None` for keys the keymap
/// has no spelling for (media keys, lone modifiers, `Null`).
///
/// Shift+Tab arrives from crossterm as `BackTab` (with Shift set) and becomes
/// `Tab` with [`Mods::SHIFT`], its one canonical form.
#[must_use]
pub fn chord_from_key_event(event: &KeyEvent) -> Option<Chord> {
    let key = match event.code {
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Enter => Key::Enter,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::Tab | KeyCode::BackTab => Key::Tab,
        KeyCode::Delete => Key::Delete,
        KeyCode::Insert => Key::Insert,
        KeyCode::F(number) => Key::F(number),
        KeyCode::Esc => Key::Esc,
        KeyCode::Char(character) => Key::Char(character),
        _ => return None,
    };
    let mut mods = Mods::NONE;
    if event.modifiers.contains(KeyModifiers::CONTROL) {
        mods = mods | Mods::CTRL;
    }
    if event.modifiers.contains(KeyModifiers::ALT) {
        mods = mods | Mods::ALT;
    }
    if event.modifiers.contains(KeyModifiers::SHIFT) {
        mods = mods | Mods::SHIFT;
    }
    Some(Chord::new(key, mods))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_keys_become_their_canonical_chords() {
        let chord = |code, modifiers| chord_from_key_event(&KeyEvent::new(code, modifiers));
        assert_eq!(
            chord(KeyCode::Char(':'), KeyModifiers::SHIFT).unwrap().as_str(),
            ":"
        );
        assert_eq!(
            chord(KeyCode::Char('G'), KeyModifiers::SHIFT).unwrap().as_str(),
            "G"
        );
        assert_eq!(
            chord(KeyCode::Char('k'), KeyModifiers::CONTROL).unwrap().as_str(),
            "ctrl+k"
        );
        assert_eq!(
            chord(KeyCode::BackTab, KeyModifiers::SHIFT).unwrap().as_str(),
            "shift+tab"
        );
        assert_eq!(
            chord(KeyCode::Char(' '), KeyModifiers::NONE).unwrap().as_str(),
            "space"
        );
        assert_eq!(
            chord(KeyCode::Char('+'), KeyModifiers::NONE).unwrap().as_str(),
            "plus"
        );
        assert_eq!(
            chord(KeyCode::F(5), KeyModifiers::NONE).unwrap().code(),
            Key::F(5)
        );
        assert!(chord(KeyCode::Null, KeyModifiers::CONTROL).is_none());
    }
}
