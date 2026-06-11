// ABOUTME: Encodes crossterm Key/Mouse events into the byte sequences a real terminal
// would send, for forwarding into the embedded tmux-attach PTY. Mirrors the xterm/
// tui-term conventions so the inner program (tmux → shell → Claude Code) sees normal
// input.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent};
use ratatui::layout::Rect;

/// Encode a key event into the bytes to write to the embed PTY. Returns `None`
/// for keys with no terminal byte representation (e.g. bare modifier presses).
pub fn encode_key_event(key: &KeyEvent) -> Option<Vec<u8>> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    let bytes: Vec<u8> = match key.code {
        KeyCode::Char(c) => {
            if ctrl {
                ctrl_byte(c)?
            } else {
                let mut b = c.to_string().into_bytes();
                if alt {
                    // Alt/Meta: ESC-prefix the char.
                    let mut out = vec![0x1b];
                    out.append(&mut b);
                    return Some(out);
                }
                b
            }
        }
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Backspace => vec![0x7f], // DEL, what xterm sends
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => vec![0x1b, b'[', b'Z'],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Left => vec![0x1b, b'[', b'D'],
        KeyCode::Right => vec![0x1b, b'[', b'C'],
        KeyCode::Up => vec![0x1b, b'[', b'A'],
        KeyCode::Down => vec![0x1b, b'[', b'B'],
        KeyCode::Home => vec![0x1b, b'[', b'H'],
        KeyCode::End => vec![0x1b, b'[', b'F'],
        KeyCode::PageUp => vec![0x1b, b'[', b'5', b'~'],
        KeyCode::PageDown => vec![0x1b, b'[', b'6', b'~'],
        KeyCode::Delete => vec![0x1b, b'[', b'3', b'~'],
        KeyCode::Insert => vec![0x1b, b'[', b'2', b'~'],
        KeyCode::F(n) if (1..=4).contains(&n) => {
            // F1-F4: SS3 sequences (ESC O P/Q/R/S).
            vec![0x1b, b'O', b'P' + (n - 1)]
        }
        KeyCode::F(n) if (5..=12).contains(&n) => {
            // F5-F12: CSI ~ sequences with xterm's discontinuous codes.
            let code: &[u8] = match n {
                5 => b"15",
                6 => b"17",
                7 => b"18",
                8 => b"19",
                9 => b"20",
                10 => b"21",
                11 => b"23",
                12 => b"24",
                _ => unreachable!(),
            };
            let mut out = vec![0x1b, b'['];
            out.extend_from_slice(code);
            out.push(b'~');
            out
        }
        _ => return None,
    };
    Some(bytes)
}

/// Encode a mouse event into SGR (mode 1006) bytes for the embed PTY, translating
/// terminal-global coordinates into 1-based pane-local ones. `inner` is the embed's
/// interior rect (inside the border) — the cells the PseudoTerminal actually
/// occupies. Returns `None` when the event lies outside `inner` or has no terminal
/// representation (motion without a held button is only sent under all-motion
/// tracking; forwarding it unconditionally would flood the PTY).
pub fn encode_mouse_event(mouse: &MouseEvent, inner: Rect) -> Option<Vec<u8>> {
    use crossterm::event::MouseEventKind;

    if inner.width == 0 || inner.height == 0 {
        return None;
    }
    let in_x = mouse.column >= inner.x && mouse.column < inner.x.saturating_add(inner.width);
    let in_y = mouse.row >= inner.y && mouse.row < inner.y.saturating_add(inner.height);
    if !in_x || !in_y {
        return None;
    }

    let (button, release) = match mouse.kind {
        MouseEventKind::Down(b) => (button_code(b), false),
        MouseEventKind::Up(b) => (button_code(b), true),
        // Drag: the held button with the motion flag (+32).
        MouseEventKind::Drag(b) => (button_code(b) + 32, false),
        MouseEventKind::ScrollUp => (64, false),
        MouseEventKind::ScrollDown => (65, false),
        MouseEventKind::ScrollLeft => (66, false),
        MouseEventKind::ScrollRight => (67, false),
        MouseEventKind::Moved => return None,
    };

    let button = button + modifier_bits(mouse.modifiers);
    let x = mouse.column - inner.x + 1;
    let y = mouse.row - inner.y + 1;
    let suffix = if release { 'm' } else { 'M' };
    Some(format!("\x1b[<{button};{x};{y}{suffix}").into_bytes())
}

fn button_code(b: MouseButton) -> u16 {
    match b {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    }
}

/// SGR modifier bits: shift +4, alt/meta +8, ctrl +16.
fn modifier_bits(mods: KeyModifiers) -> u16 {
    let mut bits = 0;
    if mods.contains(KeyModifiers::SHIFT) {
        bits += 4;
    }
    if mods.contains(KeyModifiers::ALT) {
        bits += 8;
    }
    if mods.contains(KeyModifiers::CONTROL) {
        bits += 16;
    }
    bits
}

/// Map a Ctrl+<char> chord to its control byte. Returns `None` for combinations
/// with no control code.
fn ctrl_byte(c: char) -> Option<Vec<u8>> {
    let upper = c.to_ascii_uppercase();
    let b = match upper {
        ' ' | '@' => 0x00,                     // Ctrl+Space / Ctrl+@ → NUL
        'A'..='Z' => (upper as u8) - b'A' + 1, // Ctrl+A..Z → 0x01..0x1A
        '[' => 0x1b,
        '\\' => 0x1c,
        ']' => 0x1d,
        '^' => 0x1e,
        '_' | '-' => 0x1f,
        '?' => 0x7f,
        _ => return None,
    };
    Some(vec![b])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, KeyEventState};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn key_mod(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn enc(code: KeyCode) -> Vec<u8> {
        encode_key_event(&key(code)).expect("encodable")
    }

    // ── exhaustive table (validation matrix C10) ───────────────────────────
    #[test]
    fn special_keys_map_to_terminal_sequences() {
        assert_eq!(enc(KeyCode::Enter), vec![b'\r']);
        assert_eq!(enc(KeyCode::Backspace), vec![0x7f]);
        assert_eq!(enc(KeyCode::Tab), vec![b'\t']);
        assert_eq!(enc(KeyCode::BackTab), vec![0x1b, b'[', b'Z']);
        assert_eq!(enc(KeyCode::Esc), vec![0x1b]);
        assert_eq!(enc(KeyCode::Left), vec![0x1b, b'[', b'D']);
        assert_eq!(enc(KeyCode::Right), vec![0x1b, b'[', b'C']);
        assert_eq!(enc(KeyCode::Up), vec![0x1b, b'[', b'A']);
        assert_eq!(enc(KeyCode::Down), vec![0x1b, b'[', b'B']);
        assert_eq!(enc(KeyCode::Home), vec![0x1b, b'[', b'H']);
        assert_eq!(enc(KeyCode::End), vec![0x1b, b'[', b'F']);
        assert_eq!(enc(KeyCode::PageUp), vec![0x1b, b'[', b'5', b'~']);
        assert_eq!(enc(KeyCode::PageDown), vec![0x1b, b'[', b'6', b'~']);
        assert_eq!(enc(KeyCode::Delete), vec![0x1b, b'[', b'3', b'~']);
        assert_eq!(enc(KeyCode::Insert), vec![0x1b, b'[', b'2', b'~']);
    }

    #[test]
    fn plain_chars_pass_through_as_utf8() {
        assert_eq!(enc(KeyCode::Char('a')), vec![0x61]);
        assert_eq!(enc(KeyCode::Char('Z')), vec![0x5a]);
        assert_eq!(enc(KeyCode::Char('1')), vec![0x31]);
        assert_eq!(enc(KeyCode::Char(' ')), vec![0x20]);
        // multi-byte UTF-8 passes through whole
        assert_eq!(enc(KeyCode::Char('é')), "é".as_bytes().to_vec());
    }

    #[test]
    fn ctrl_letters_map_to_control_bytes() {
        assert_eq!(
            encode_key_event(&key_mod(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(vec![0x03])
        ); // Ctrl+C
        assert_eq!(
            encode_key_event(&key_mod(KeyCode::Char('d'), KeyModifiers::CONTROL)),
            Some(vec![0x04])
        ); // Ctrl+D
        assert_eq!(
            encode_key_event(&key_mod(KeyCode::Char('z'), KeyModifiers::CONTROL)),
            Some(vec![0x1a])
        ); // Ctrl+Z
        assert_eq!(
            encode_key_event(&key_mod(KeyCode::Char('a'), KeyModifiers::CONTROL)),
            Some(vec![0x01])
        );
        // Ctrl+Q (the embed release key — ainb intercepts it BEFORE encoding, but
        // the raw encoding is XON 0x11 if it ever reached here).
        assert_eq!(
            encode_key_event(&key_mod(KeyCode::Char('q'), KeyModifiers::CONTROL)),
            Some(vec![0x11])
        );
    }

    #[test]
    fn alt_char_is_esc_prefixed() {
        assert_eq!(
            encode_key_event(&key_mod(KeyCode::Char('b'), KeyModifiers::ALT)),
            Some(vec![0x1b, b'b'])
        );
    }

    #[test]
    fn unencodable_keys_return_none() {
        assert_eq!(encode_key_event(&key(KeyCode::Null)), None);
    }

    // ── SGR mouse encoding ─────────────────────────────────────────────────
    use crossterm::event::{MouseEvent, MouseEventKind};

    /// Embed interior used by the mouse tests: border at x=9/y=4, so the
    /// interior's top-left terminal cell is (10, 5).
    fn inner() -> Rect {
        Rect::new(10, 5, 50, 20)
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16, modifiers: KeyModifiers) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers,
        }
    }

    #[test]
    fn mouse_click_at_interior_top_left_is_sgr_1_1() {
        // (10,5) is the first interior cell → pane-local 1;1 (1-based), NOT 0;0.
        let ev = mouse(
            MouseEventKind::Down(MouseButton::Left),
            10,
            5,
            KeyModifiers::NONE,
        );
        assert_eq!(
            encode_mouse_event(&ev, inner()),
            Some(b"\x1b[<0;1;1M".to_vec())
        );
    }

    #[test]
    fn mouse_release_uses_lowercase_m() {
        let ev = mouse(
            MouseEventKind::Up(MouseButton::Left),
            10,
            5,
            KeyModifiers::NONE,
        );
        assert_eq!(
            encode_mouse_event(&ev, inner()),
            Some(b"\x1b[<0;1;1m".to_vec())
        );
    }

    #[test]
    fn mouse_buttons_map_to_sgr_codes() {
        let mid = mouse(
            MouseEventKind::Down(MouseButton::Middle),
            10,
            5,
            KeyModifiers::NONE,
        );
        let right = mouse(
            MouseEventKind::Down(MouseButton::Right),
            10,
            5,
            KeyModifiers::NONE,
        );
        assert_eq!(
            encode_mouse_event(&mid, inner()),
            Some(b"\x1b[<1;1;1M".to_vec())
        );
        assert_eq!(
            encode_mouse_event(&right, inner()),
            Some(b"\x1b[<2;1;1M".to_vec())
        );
    }

    #[test]
    fn wheel_maps_to_64_and_65_with_translated_coords() {
        let up = mouse(MouseEventKind::ScrollUp, 20, 10, KeyModifiers::NONE);
        let down = mouse(MouseEventKind::ScrollDown, 20, 10, KeyModifiers::NONE);
        assert_eq!(
            encode_mouse_event(&up, inner()),
            Some(b"\x1b[<64;11;6M".to_vec())
        );
        assert_eq!(
            encode_mouse_event(&down, inner()),
            Some(b"\x1b[<65;11;6M".to_vec())
        );
    }

    #[test]
    fn drag_adds_the_motion_flag() {
        let ev = mouse(
            MouseEventKind::Drag(MouseButton::Left),
            12,
            7,
            KeyModifiers::NONE,
        );
        assert_eq!(
            encode_mouse_event(&ev, inner()),
            Some(b"\x1b[<32;3;3M".to_vec())
        );
    }

    #[test]
    fn ctrl_wheel_adds_modifier_bits() {
        let ev = mouse(MouseEventKind::ScrollUp, 10, 5, KeyModifiers::CONTROL);
        assert_eq!(
            encode_mouse_event(&ev, inner()),
            Some(b"\x1b[<80;1;1M".to_vec())
        );
        let ev = mouse(
            MouseEventKind::Down(MouseButton::Left),
            10,
            5,
            KeyModifiers::SHIFT | KeyModifiers::ALT,
        );
        assert_eq!(
            encode_mouse_event(&ev, inner()),
            Some(b"\x1b[<12;1;1M".to_vec())
        );
    }

    #[test]
    fn events_on_the_border_or_outside_are_not_forwarded() {
        // One cell left/up of the interior = the border itself.
        let on_left_border = mouse(
            MouseEventKind::Down(MouseButton::Left),
            9,
            5,
            KeyModifiers::NONE,
        );
        let on_top_border = mouse(
            MouseEventKind::Down(MouseButton::Left),
            10,
            4,
            KeyModifiers::NONE,
        );
        // First cell PAST the interior (x + width / y + height) = right/bottom border.
        let past_right = mouse(
            MouseEventKind::Down(MouseButton::Left),
            60,
            5,
            KeyModifiers::NONE,
        );
        let past_bottom = mouse(
            MouseEventKind::Down(MouseButton::Left),
            10,
            25,
            KeyModifiers::NONE,
        );
        assert_eq!(encode_mouse_event(&on_left_border, inner()), None);
        assert_eq!(encode_mouse_event(&on_top_border, inner()), None);
        assert_eq!(encode_mouse_event(&past_right, inner()), None);
        assert_eq!(encode_mouse_event(&past_bottom, inner()), None);
        // Last interior cell maps to the pane's full extent.
        let bottom_right = mouse(
            MouseEventKind::Down(MouseButton::Left),
            59,
            24,
            KeyModifiers::NONE,
        );
        assert_eq!(
            encode_mouse_event(&bottom_right, inner()),
            Some(b"\x1b[<0;50;20M".to_vec())
        );
    }

    #[test]
    fn motion_without_a_held_button_is_dropped() {
        let ev = mouse(MouseEventKind::Moved, 20, 10, KeyModifiers::NONE);
        assert_eq!(encode_mouse_event(&ev, inner()), None);
    }

    #[test]
    fn empty_inner_rect_forwards_nothing() {
        let ev = mouse(
            MouseEventKind::Down(MouseButton::Left),
            0,
            0,
            KeyModifiers::NONE,
        );
        assert_eq!(encode_mouse_event(&ev, Rect::new(0, 0, 0, 0)), None);
    }

    // ── property invariants (validation matrix C10) ────────────────────────
    proptest::proptest! {
        #[test]
        fn printable_ascii_no_mods_is_its_own_byte(c in 0x20u8..=0x7e) {
            let ch = c as char;
            let out = encode_key_event(&key(KeyCode::Char(ch))).unwrap();
            proptest::prop_assert_eq!(out, vec![c]);
        }

        #[test]
        fn ctrl_lowercase_letter_is_in_control_range(c in b'a'..=b'z') {
            let ch = c as char;
            let out = encode_key_event(&key_mod(KeyCode::Char(ch), KeyModifiers::CONTROL)).unwrap();
            proptest::prop_assert_eq!(out.len(), 1);
            proptest::prop_assert!((0x01..=0x1a).contains(&out[0]));
        }
    }
}
