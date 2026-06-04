// ABOUTME: Encodes crossterm KeyEvents into the byte sequences a real terminal would
// send, for forwarding into the embedded tmux-attach PTY. Mirrors the xterm/tui-term
// conventions so the inner program (tmux → shell → Claude Code) sees normal input.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

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

/// Map a Ctrl+<char> chord to its control byte. Returns `None` for combinations
/// with no control code.
fn ctrl_byte(c: char) -> Option<Vec<u8>> {
    let upper = c.to_ascii_uppercase();
    let b = match upper {
        ' ' | '@' => 0x00,           // Ctrl+Space / Ctrl+@ → NUL
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
        KeyEvent { code, modifiers, kind: KeyEventKind::Press, state: KeyEventState::NONE }
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
        assert_eq!(encode_key_event(&key_mod(KeyCode::Char('c'), KeyModifiers::CONTROL)), Some(vec![0x03])); // Ctrl+C
        assert_eq!(encode_key_event(&key_mod(KeyCode::Char('d'), KeyModifiers::CONTROL)), Some(vec![0x04])); // Ctrl+D
        assert_eq!(encode_key_event(&key_mod(KeyCode::Char('z'), KeyModifiers::CONTROL)), Some(vec![0x1a])); // Ctrl+Z
        assert_eq!(encode_key_event(&key_mod(KeyCode::Char('a'), KeyModifiers::CONTROL)), Some(vec![0x01]));
        // Ctrl+Q (the embed release key — ainb intercepts it BEFORE encoding, but
        // the raw encoding is XON 0x11 if it ever reached here).
        assert_eq!(encode_key_event(&key_mod(KeyCode::Char('q'), KeyModifiers::CONTROL)), Some(vec![0x11]));
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
