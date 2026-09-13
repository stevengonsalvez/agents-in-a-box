// ABOUTME: Parity between the terminal key edge and the renderer contract.
// For every built-in keymap row, the crossterm key a terminal reports for it
// must convert to the row's chord exactly as the pre-split converter did, and
// `Intent::Key` must resolve to the same event that key path produced.

use ainb::app::events::{EventHandler, NoRenderer};
use ainb::app::keymap::{Chord, Key, KeyAction, KeyContext, Mods, SubContext};
use ainb::app::terminal_keys::chord_from_key_event;
use ainb::{AppState, Intent, Keymap};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::sync::Mutex;

/// Some rows persist config (the Skill Manager resize keys), so every test
/// points HOME at a scratch dir and holds this lock while it does.
static HOME_LOCK: Mutex<()> = Mutex::new(());

/// The converter key dispatch used before `Chord` became crate-owned, kept
/// verbatim as the oracle (it was `Chord::from_key_event`).
fn legacy_chord(event: &KeyEvent) -> Chord {
    let mut modifiers = Vec::new();
    if event.modifiers.contains(KeyModifiers::CONTROL) {
        modifiers.push("ctrl");
    }
    if event.modifiers.contains(KeyModifiers::ALT) {
        modifiers.push("alt");
    }
    if event.modifiers.contains(KeyModifiers::SHIFT) {
        modifiers.push("shift");
    }
    let key = match event.code {
        KeyCode::Backspace => "backspace".to_string(),
        KeyCode::Enter => "enter".to_string(),
        KeyCode::Left => "left".to_string(),
        KeyCode::Right => "right".to_string(),
        KeyCode::Up => "up".to_string(),
        KeyCode::Down => "down".to_string(),
        KeyCode::Home => "home".to_string(),
        KeyCode::End => "end".to_string(),
        KeyCode::PageUp => "pageup".to_string(),
        KeyCode::PageDown => "pagedown".to_string(),
        KeyCode::Tab => "tab".to_string(),
        KeyCode::BackTab => "tab".to_string(),
        KeyCode::Delete => "delete".to_string(),
        KeyCode::Insert => "insert".to_string(),
        KeyCode::F(number) => format!("f{number}"),
        KeyCode::Esc => "esc".to_string(),
        KeyCode::Char(' ') => "space".to_string(),
        KeyCode::Char('+') => "plus".to_string(),
        KeyCode::Char(character) => character.to_string(),
        other => format!("{other:?}").to_ascii_lowercase(),
    };
    if matches!(&event.code, KeyCode::Char(_)) {
        modifiers.retain(|modifier| *modifier != "shift");
    }
    let source = modifiers
        .into_iter()
        .chain(std::iter::once(key.as_str()))
        .collect::<Vec<_>>()
        .join("+");
    Chord::parse(&source).expect("crossterm key normalisation is valid")
}

/// The key event a terminal reports when the user presses `chord`: Shift+Tab
/// arrives as `BackTab`, and an upper-case letter carries Shift.
fn terminal_event_for(chord: &Chord) -> KeyEvent {
    let mods = chord.modifiers();
    let mut modifiers = KeyModifiers::NONE;
    if mods.contains(Mods::CTRL) {
        modifiers |= KeyModifiers::CONTROL;
    }
    if mods.contains(Mods::ALT) {
        modifiers |= KeyModifiers::ALT;
    }
    if mods.contains(Mods::SHIFT) {
        modifiers |= KeyModifiers::SHIFT;
    }
    let code = match chord.code() {
        Key::Char(character) => {
            if character.is_ascii_uppercase() {
                modifiers |= KeyModifiers::SHIFT;
            }
            KeyCode::Char(character)
        }
        Key::Tab if mods.contains(Mods::SHIFT) => KeyCode::BackTab,
        Key::Enter => KeyCode::Enter,
        Key::Esc => KeyCode::Esc,
        Key::Tab => KeyCode::Tab,
        Key::Backspace => KeyCode::Backspace,
        Key::Delete => KeyCode::Delete,
        Key::Insert => KeyCode::Insert,
        Key::Up => KeyCode::Up,
        Key::Down => KeyCode::Down,
        Key::Left => KeyCode::Left,
        Key::Right => KeyCode::Right,
        Key::Home => KeyCode::Home,
        Key::End => KeyCode::End,
        Key::PageUp => KeyCode::PageUp,
        Key::PageDown => KeyCode::PageDown,
        Key::F(number) => KeyCode::F(number),
    };
    KeyEvent::new(code, modifiers)
}

/// A fresh state on the row's own screen, so screen rows are reachable.
fn state_for(context: &KeyContext) -> AppState {
    let mut state = AppState::new();
    if let KeyContext::Screen(screen, SubContext::None) = context {
        state.shell.current_screen = (*screen).to_string();
    }
    state
}

#[test]
fn every_default_row_converts_to_the_chord_the_legacy_path_produced() {
    for binding in Keymap::defaults().bindings() {
        let event = terminal_event_for(&binding.chord);
        let converted = chord_from_key_event(&event)
            .unwrap_or_else(|| panic!("{} [{}] has no chord", binding.id, binding.ctx.name()));
        assert_eq!(
            converted,
            legacy_chord(&event),
            "{} [{}]",
            binding.id,
            binding.ctx.name()
        );
        assert_eq!(
            converted,
            binding.chord,
            "{} [{}]",
            binding.id,
            binding.ctx.name()
        );
    }
}

#[test]
fn key_intent_resolves_to_the_event_the_key_event_path_produced_for_every_row() {
    let _home_lock = HOME_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let home = tempfile::tempdir().expect("scratch home");
    std::env::set_var("HOME", home.path());
    let keymap = Keymap::defaults();
    let mut reached = 0;
    for binding in keymap.bindings() {
        let event = terminal_event_for(&binding.chord);
        let label = format!(
            "{} [{}] `{}`",
            binding.id,
            binding.ctx.name(),
            binding.chord.as_str()
        );

        let mut legacy_state = state_for(&binding.ctx);
        let legacy = EventHandler::handle_key_event_with_keymap(
            legacy_chord(&event),
            &mut legacy_state,
            &keymap,
            &mut NoRenderer,
        );

        let mut state = state_for(&binding.ctx);
        let chord = chord_from_key_event(&event).expect("every row has a chord");
        let resolved =
            EventHandler::resolve_intent(Intent::Key(chord), &mut state, &keymap, &mut NoRenderer);

        assert_eq!(format!("{resolved:?}"), format!("{legacy:?}"), "{label}");
        assert_eq!(state.versions(), legacy_state.versions(), "{label}");

        // The row itself, looked up in its own context, yields its action.
        let action = keymap.resolve(std::slice::from_ref(&binding.ctx), &binding.chord);
        assert_eq!(
            format!("{action:?}"),
            format!("{:?}", Some(&binding.action)),
            "{label}"
        );

        if let (Some(event), KeyAction::App(expected)) = (&resolved, &binding.action) {
            if format!("{event:?}") == format!("{expected:?}") {
                reached += 1;
            }
        }
    }
    // Screen and global rows resolve to their own event from a fresh state;
    // overlay rows need their overlay open and resolve elsewhere, identically
    // on both paths.
    assert!(
        reached >= 155,
        "only {reached} rows reached their own event"
    );
}
