//! Content-Length stdio framing — LSP-style.
//!
//! Wire format:
//!
//! ```text
//! Content-Length: <decimal-bytes>\r\n
//! \r\n
//! <body bytes — exactly Content-Length>
//! ```
//!
//! The body is opaque to this module — it is JSON in practice but we
//! intentionally don't decode it here so framing stays composable
//! with batched/streamed JSON serializers.
//!
//! Headers are ASCII; Content-Length is the *only* header we accept.
//! Other headers, malformed values, or oversized payloads are
//! rejected at decode time before any body is read.

use std::io::{BufRead, Write};

use crate::errors::ProtocolError;

/// Maximum body size we will read from a single frame.
///
/// Sized at 16 MiB. Render buffers and snapshot payloads sit well
/// under this; bigger payloads almost certainly indicate a runaway
/// loop on the peer side and we'd rather fail closed.
pub const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

/// Encode an already-serialised JSON body into a Content-Length frame.
///
/// Note: takes the body as raw bytes so callers can choose their JSON
/// serializer (`serde_json::to_vec`, `simd-json`, ...). The output is the
/// full wire frame: header + CRLFCRLF + body.
pub fn encode(body: &[u8]) -> Vec<u8> {
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    let mut out = Vec::with_capacity(header.len() + body.len());
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(body);
    out
}

/// Encode a JSON value into a Content-Length frame.
///
/// Convenience for the common path where the caller is producing a
/// `serde_json::Value` already.
pub fn encode_json(body: &serde_json::Value) -> Result<Vec<u8>, ProtocolError> {
    let bytes = serde_json::to_vec(body)?;
    Ok(encode(&bytes))
}

/// Write a frame to a writer in one shot.
///
/// Errors propagate from the underlying writer. The writer is *not*
/// flushed; callers running over a buffered stdout/PipeWriter should
/// flush after every frame to keep latency predictable.
pub fn write_frame<W: Write>(w: &mut W, body: &[u8]) -> Result<(), ProtocolError> {
    w.write_all(&encode(body))?;
    Ok(())
}

/// Decode exactly one frame from a buffered reader.
///
/// Returns `Ok(None)` only if the reader hits EOF cleanly *before*
/// any byte of a header has been read — the standard "peer closed
/// the pipe" signal. A partial header (any byte read) is treated as
/// an error.
///
/// Rejects:
/// - non-ASCII / non-UTF8 header lines
/// - headers other than `Content-Length`
/// - missing or non-numeric `Content-Length`
/// - `Content-Length` greater than [`MAX_BODY_BYTES`]
/// - oversized headers (> 8 KiB pre-body)
pub fn read_frame<R: BufRead>(r: &mut R) -> Result<Option<Vec<u8>>, ProtocolError> {
    /// Soft cap on header bytes (lines + CRLFs). 8 KiB is generous —
    /// the only legal header today is `Content-Length: <number>`.
    const HEADER_BUDGET: usize = 8 * 1024;

    let mut content_length: Option<usize> = None;
    let mut header_bytes_seen = 0usize;
    let mut any_byte_read = false;

    loop {
        let mut line = String::new();
        let n = r
            .read_line(&mut line)
            .map_err(ProtocolError::Transport)?;

        if n == 0 {
            // Clean EOF.
            if any_byte_read {
                return Err(ProtocolError::Decode(
                    "unexpected EOF inside header block".into(),
                ));
            }
            return Ok(None);
        }
        any_byte_read = true;
        header_bytes_seen += n;
        if header_bytes_seen > HEADER_BUDGET {
            return Err(ProtocolError::Decode(format!(
                "header block exceeded {HEADER_BUDGET} bytes"
            )));
        }

        // Lines must terminate with CRLF per spec.
        if !line.ends_with("\r\n") {
            return Err(ProtocolError::Decode(format!(
                "header line not CRLF-terminated: {line:?}"
            )));
        }
        let trimmed = line.trim_end_matches("\r\n");

        if trimmed.is_empty() {
            // End of headers. Need a Content-Length to proceed.
            let len = content_length
                .ok_or_else(|| ProtocolError::Decode("missing Content-Length header".into()))?;
            if len > MAX_BODY_BYTES {
                return Err(ProtocolError::Decode(format!(
                    "Content-Length {len} exceeds MAX_BODY_BYTES {MAX_BODY_BYTES}"
                )));
            }
            let mut body = vec![0u8; len];
            r.read_exact(&mut body).map_err(ProtocolError::Transport)?;
            return Ok(Some(body));
        }

        // Parse `Name: Value` strictly.
        let Some((name, value)) = trimmed.split_once(':') else {
            return Err(ProtocolError::Decode(format!(
                "malformed header line: {trimmed:?}"
            )));
        };
        let name = name.trim();
        let value = value.trim();

        if name.eq_ignore_ascii_case("Content-Length") {
            let parsed: usize = value
                .parse()
                .map_err(|_| ProtocolError::Decode(format!("non-numeric Content-Length: {value:?}")))?;
            content_length = Some(parsed);
        } else {
            return Err(ProtocolError::Decode(format!(
                "unsupported header: {name}"
            )));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn round_trip(body: &[u8]) {
        let frame = encode(body);
        let mut cursor = Cursor::new(frame);
        let decoded = read_frame(&mut cursor).unwrap().unwrap();
        assert_eq!(decoded, body);
    }

    #[test]
    fn round_trip_empty() {
        round_trip(b"");
    }

    #[test]
    fn round_trip_small_json() {
        round_trip(br#"{"jsonrpc":"2.0","id":1,"method":"plugin/init"}"#);
    }

    #[test]
    fn round_trip_unicode_body() {
        // Body bytes need not be UTF-8 either, but exercise the unicode case.
        round_trip("héllo — 🦀".as_bytes());
    }

    #[test]
    fn encode_json_helper() {
        let v = serde_json::json!({"hello": "world"});
        let frame = encode_json(&v).unwrap();
        let mut cursor = Cursor::new(frame);
        let body = read_frame(&mut cursor).unwrap().unwrap();
        let decoded: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(decoded, v);
    }

    #[test]
    fn clean_eof_returns_none() {
        let mut cursor = Cursor::new(Vec::new());
        let res = read_frame(&mut cursor).unwrap();
        assert!(res.is_none());
    }

    #[test]
    fn missing_content_length_rejected() {
        let mut cursor = Cursor::new(b"\r\n".to_vec());
        let err = read_frame(&mut cursor).unwrap_err();
        assert!(matches!(err, ProtocolError::Decode(_)));
    }

    #[test]
    fn negative_content_length_rejected() {
        let mut cursor = Cursor::new(b"Content-Length: -1\r\n\r\n".to_vec());
        let err = read_frame(&mut cursor).unwrap_err();
        assert!(matches!(err, ProtocolError::Decode(_)));
    }

    #[test]
    fn non_numeric_content_length_rejected() {
        let mut cursor = Cursor::new(b"Content-Length: abc\r\n\r\n".to_vec());
        let err = read_frame(&mut cursor).unwrap_err();
        assert!(matches!(err, ProtocolError::Decode(_)));
    }

    #[test]
    fn unsupported_header_rejected() {
        let mut cursor = Cursor::new(b"Content-Type: application/json\r\nContent-Length: 0\r\n\r\n".to_vec());
        let err = read_frame(&mut cursor).unwrap_err();
        assert!(matches!(err, ProtocolError::Decode(_)));
    }

    #[test]
    fn oversized_body_rejected() {
        let too_big = MAX_BODY_BYTES + 1;
        let header = format!("Content-Length: {too_big}\r\n\r\n");
        let mut cursor = Cursor::new(header.into_bytes());
        let err = read_frame(&mut cursor).unwrap_err();
        assert!(matches!(err, ProtocolError::Decode(_)));
    }

    #[test]
    fn missing_crlf_rejected() {
        let mut cursor = Cursor::new(b"Content-Length: 0\n\n".to_vec());
        let err = read_frame(&mut cursor).unwrap_err();
        assert!(matches!(err, ProtocolError::Decode(_)));
    }

    #[test]
    fn case_insensitive_content_length() {
        let frame = b"content-length: 2\r\n\r\nok";
        let mut cursor = Cursor::new(frame.to_vec());
        let body = read_frame(&mut cursor).unwrap().unwrap();
        assert_eq!(body, b"ok");
    }

    #[test]
    fn oversized_header_block_rejected() {
        let mut bytes = Vec::new();
        // We can't slip an unsupported header past — but we can repeat
        // valid `Content-Length` lines until we blow the budget.
        for _ in 0..2000 {
            bytes.extend_from_slice(b"Content-Length: 0\r\n");
        }
        bytes.extend_from_slice(b"\r\n");
        let mut cursor = Cursor::new(bytes);
        let err = read_frame(&mut cursor).unwrap_err();
        assert!(matches!(err, ProtocolError::Decode(_)));
    }

    #[test]
    fn write_frame_matches_encode() {
        let body = br#"{"id":42}"#;
        let mut buf: Vec<u8> = Vec::new();
        write_frame(&mut buf, body).unwrap();
        assert_eq!(buf, encode(body));
    }
}
