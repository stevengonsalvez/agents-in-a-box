// ABOUTME: Copy text to the terminal clipboard via the OSC 52 escape sequence.
// OSC 52 is handled by the terminal emulator itself, so it reaches the USER's
// machine through tmux (set-clipboard on), zellij, and ssh — unlike pbcopy /
// xclip / wl-copy, which would target the remote host. Best-effort: returns Err
// only on a stdout write failure; a terminal that ignores OSC 52 is a silent
// no-op (the text is still shown on screen).

use std::io::Write;

/// Copy `text` to the terminal clipboard via OSC 52.
pub fn copy_osc52(text: &str) -> std::io::Result<()> {
    let mut out = std::io::stdout();
    out.write_all(osc52_sequence(text).as_bytes())?;
    out.flush()
}

/// The OSC 52 "set clipboard" escape for `text` (pure — testable without a tty).
fn osc52_sequence(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", base64_encode(text.as_bytes()))
}

/// Standard base64 (no line breaks). Inlined to avoid a crate dependency for the
/// handful of bytes a clipboard payload needs.
fn base64_encode(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            T[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn osc52_sequence_wraps_base64_in_escape() {
        assert_eq!(osc52_sequence("foo"), "\x1b]52;c;Zm9v\x07");
    }

    #[test]
    fn base64_path_roundtrips_shape() {
        // A realistic path encodes without padding errors / panics.
        let s = base64_encode(b"/home/claude/.agents-in-a-box/installer/install-claude.sh");
        assert!(!s.contains(' '));
        assert_eq!(s.len() % 4, 0);
    }
}
