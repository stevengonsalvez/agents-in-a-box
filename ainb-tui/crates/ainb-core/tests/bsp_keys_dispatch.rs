// P5 dispatch truth table: (prefix_active, key) -> Option<BspAction>.

use ainb::ui::bsp::SplitDir;
use ainb::ui::bsp_keys::{dispatch_key, BspAction};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn special(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

#[test]
fn ctrl_w_enters_prefix_mode_when_inactive() {
    assert_eq!(dispatch_key(false, ctrl('w')), Some(BspAction::EnterPrefix));
}

#[test]
fn plain_keys_pass_through_when_prefix_inactive() {
    assert_eq!(dispatch_key(false, key('v')), None);
    assert_eq!(dispatch_key(false, key('x')), None);
    assert_eq!(dispatch_key(false, key('q')), None);
    assert_eq!(dispatch_key(false, special(KeyCode::Esc)), None);
    assert_eq!(dispatch_key(false, special(KeyCode::Tab)), None);
}

#[test]
fn split_keys_emit_correct_action_in_prefix_mode() {
    assert_eq!(dispatch_key(true, key('v')), Some(BspAction::SplitVertical));
    assert_eq!(dispatch_key(true, key('s')), Some(BspAction::SplitHorizontal));
}

#[test]
fn split_actions_map_to_split_dir() {
    assert_eq!(BspAction::SplitVertical.as_split_dir(), Some(SplitDir::Vertical));
    assert_eq!(BspAction::SplitHorizontal.as_split_dir(), Some(SplitDir::Horizontal));
    assert_eq!(BspAction::CloseFocused.as_split_dir(), None);
}

#[test]
fn close_focused_emits_in_prefix_mode() {
    assert_eq!(dispatch_key(true, key('x')), Some(BspAction::CloseFocused));
}

#[test]
fn cycle_focus_keys_in_prefix_mode() {
    assert_eq!(dispatch_key(true, key('o')), Some(BspAction::CycleFocusNext));
    assert_eq!(dispatch_key(true, special(KeyCode::Tab)), Some(BspAction::CycleFocusNextAlt));
}

#[test]
fn esc_exits_prefix_mode() {
    assert_eq!(dispatch_key(true, special(KeyCode::Esc)), Some(BspAction::ExitPrefix));
}

#[test]
fn unknown_key_in_prefix_mode_exits_prefix() {
    // user fat-fingered `z` — don't trap them; bail out of prefix.
    assert_eq!(dispatch_key(true, key('z')), Some(BspAction::ExitPrefix));
    assert_eq!(dispatch_key(true, key('q')), Some(BspAction::ExitPrefix));
}

#[test]
fn resize_key_in_prefix_mode_enters_resize() {
    assert_eq!(dispatch_key(true, key('r')), Some(BspAction::EnterResize));
}

#[test]
fn ctrl_w_in_prefix_mode_does_not_re_enter_prefix() {
    // chord key in prefix mode is just an unknown key → ExitPrefix
    assert_eq!(dispatch_key(true, ctrl('w')), Some(BspAction::ExitPrefix));
}
