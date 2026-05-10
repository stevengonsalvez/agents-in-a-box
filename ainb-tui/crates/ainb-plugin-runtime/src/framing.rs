//! Async Content-Length stdio frame I/O.
//!
//! Mirrors the sync `read_frame` / `encode` in `ainb-plugin-protocol`
//! but operates on tokio's [`AsyncBufRead`] / [`AsyncWrite`] so the
//! per-plugin task can `select!` on frames + lifecycle events without
//! parking a thread. The wire format is byte-identical to the sync
//! version.

use ainb_plugin_protocol::errors::ProtocolError;
use ainb_plugin_protocol::framing::{encode, MAX_BODY_BYTES};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Header block soft cap (matches the sync decoder).
const HEADER_BUDGET: usize = 8 * 1024;

/// Read one Content-Length frame from a tokio buffered reader.
///
/// Returns `Ok(None)` only on a clean EOF *before* any header byte
/// has been read. Any partial header is treated as a decode error.
pub async fn read_frame<R>(r: &mut R) -> Result<Option<Vec<u8>>, ProtocolError>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    let mut content_length: Option<usize> = None;
    let mut header_bytes_seen = 0usize;
    let mut any_byte_read = false;

    loop {
        let mut line = String::new();
        let n = r
            .read_line(&mut line)
            .await
            .map_err(ProtocolError::Transport)?;

        if n == 0 {
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
        if !line.ends_with("\r\n") {
            return Err(ProtocolError::Decode(format!(
                "header line not CRLF-terminated: {line:?}"
            )));
        }
        let trimmed = line.trim_end_matches("\r\n");
        if trimmed.is_empty() {
            let len = content_length.ok_or_else(|| {
                ProtocolError::Decode("missing Content-Length header".into())
            })?;
            if len > MAX_BODY_BYTES {
                return Err(ProtocolError::Decode(format!(
                    "Content-Length {len} exceeds MAX_BODY_BYTES {MAX_BODY_BYTES}"
                )));
            }
            let mut body = vec![0u8; len];
            r.read_exact(&mut body)
                .await
                .map_err(ProtocolError::Transport)?;
            return Ok(Some(body));
        }
        let Some((name, value)) = trimmed.split_once(':') else {
            return Err(ProtocolError::Decode(format!(
                "malformed header line: {trimmed:?}"
            )));
        };
        let name = name.trim();
        let value = value.trim();
        if name.eq_ignore_ascii_case("Content-Length") {
            let parsed: usize = value.parse().map_err(|_| {
                ProtocolError::Decode(format!("non-numeric Content-Length: {value:?}"))
            })?;
            content_length = Some(parsed);
        } else {
            return Err(ProtocolError::Decode(format!(
                "unsupported header: {name}"
            )));
        }
    }
}

/// Write a single frame in one shot, then flush.
pub async fn write_frame<W>(w: &mut W, body: &[u8]) -> Result<(), ProtocolError>
where
    W: AsyncWrite + Unpin,
{
    let frame = encode(body);
    w.write_all(&frame).await.map_err(ProtocolError::Transport)?;
    w.flush().await.map_err(ProtocolError::Transport)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::BufReader;

    #[tokio::test]
    async fn round_trip_through_pipe() {
        let body = br#"{"jsonrpc":"2.0","id":1,"method":"plugin/init"}"#;
        let frame = encode(body);
        let mut reader = BufReader::new(&frame[..]);
        let decoded = read_frame(&mut reader).await.unwrap().unwrap();
        assert_eq!(decoded, body);
    }

    #[tokio::test]
    async fn clean_eof_returns_none() {
        let buf: Vec<u8> = Vec::new();
        let mut reader = BufReader::new(&buf[..]);
        assert!(read_frame(&mut reader).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn write_then_read_via_duplex() {
        let (a, b) = tokio::io::duplex(4096);
        let (read_a, mut write_a) = tokio::io::split(a);
        let (read_b, mut _write_b) = tokio::io::split(b);
        let body = br#"{"hello":"world"}"#;
        write_frame(&mut write_a, body).await.unwrap();
        drop(write_a);
        let mut reader = BufReader::new(read_b);
        let got = read_frame(&mut reader).await.unwrap().unwrap();
        assert_eq!(got, body);
        // Cleanup other half.
        drop(read_a);
    }
}
