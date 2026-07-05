//! JSON-RPC framing over the cap-mediated daemon socket stream.
//!
//! The plugin does **not** own a raw `UnixStream` — it speaks to the
//! daemon through the host `unix_socket_dial` cap. Outbound bytes go via
//! [`HostClient::unix_socket_send`](ainb_plugin_sdk::HostClient::unix_socket_send);
//! inbound bytes arrive as `socket:<stream_id>` `plugin/handle_event`
//! deliveries carrying a
//! [`UnixSocketEvent`](ainb_plugin_sdk::UnixSocketEvent).
//!
//! Both directions carry the daemon's Content-Length JSON-RPC framing
//! (the same LSP-style wire format the daemon already speaks on stdio).
//! This module is the codec only: [`encode_request`] builds an outbound
//! frame; [`FrameDecoder`] reassembles inbound byte chunks into whole
//! [`RpcResponse`] envelopes. It is deliberately framing-crate-free —
//! the cap stream is its own transport, not the host's stdio link — so a
//! tiny self-contained Content-Length parser lives here.

use ainb_hangar_proto::{JSONRPC_VERSION, RpcId, RpcRequest, RpcResponse};

/// Encode a daemon JSON-RPC request into a Content-Length frame ready to
/// hand to `host.unix_socket_send`.
///
/// # Errors
///
/// Returns [`serde_json::Error`] only if `params` fails to serialise —
/// in practice never, since callers pass owned `serde_json::Value`s.
pub fn encode_request(
    id: i64,
    method: &str,
    params: serde_json::Value,
) -> Result<Vec<u8>, serde_json::Error> {
    let req = RpcRequest {
        jsonrpc: JSONRPC_VERSION.to_string(),
        id: RpcId::Number(id),
        method: method.to_string(),
        params,
    };
    let body = serde_json::to_vec(&req)?;
    Ok(frame(&body))
}

/// Wrap a serialised body in a Content-Length frame.
fn frame(body: &[u8]) -> Vec<u8> {
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    let mut out = Vec::with_capacity(header.len() + body.len());
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(body);
    out
}

/// Soft cap on a single frame body. Daemon snapshots are tiny; anything
/// larger is treated as a desync and surfaced as a decode error rather
/// than buffered unboundedly.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

/// Streaming reassembler for inbound daemon frames.
///
/// Socket data arrives in arbitrary chunks (the host may split or coalesce
/// reads). [`Self::push`] feeds a chunk and drains every *complete* frame
/// it now holds, decoding each into an [`RpcResponse`]. Partial trailing
/// bytes are retained for the next push.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    buf: Vec<u8>,
}

impl FrameDecoder {
    /// A fresh decoder with an empty buffer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append `chunk` and return every fully-framed [`RpcResponse`] now
    /// available, in receive order.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError`] on a malformed header, an oversized
    /// Content-Length, or a body that fails JSON-RPC decoding. On error the
    /// decoder's buffer is left intact so the caller can choose to reset.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<RpcResponse>, DecodeError> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(body) = self.try_take_frame()? {
            let resp: RpcResponse =
                serde_json::from_slice(&body).map_err(|e| DecodeError::Json(e.to_string()))?;
            out.push(resp);
        }
        Ok(out)
    }

    /// Append `chunk` and return every fully-framed **raw body** now available,
    /// in receive order, WITHOUT decoding it into an [`RpcResponse`] (e38.29).
    ///
    /// The daemon multiplexes two kinds of frame over one connection on the same
    /// Content-Length envelope: JSON-RPC *responses* (carry an `id`) and pushed
    /// `hangar/event` *notifications* (carry a `method`, no `id`). Decoding every
    /// body eagerly as an `RpcResponse` (as [`push`](Self::push) does) treats a
    /// pushed event as a malformed response (`missing field id`) and tears the
    /// link down. The live socket path uses this body-level split so the caller
    /// can classify each frame by shape (response vs notification) and route it,
    /// instead of failing the whole connection on the first event push.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError`] only on a malformed header / oversized
    /// Content-Length — never on body content (the caller owns body parsing).
    pub fn push_frames(&mut self, chunk: &[u8]) -> Result<Vec<Vec<u8>>, DecodeError> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(body) = self.try_take_frame()? {
            out.push(body);
        }
        Ok(out)
    }

    /// Try to split one whole frame off the front of the buffer.
    ///
    /// Returns `Ok(Some(body))` and advances the buffer when a complete
    /// header + body is present, `Ok(None)` when more bytes are needed.
    fn try_take_frame(&mut self) -> Result<Option<Vec<u8>>, DecodeError> {
        const SEP: &[u8] = b"\r\n\r\n";
        let Some(header_end) = find(&self.buf, SEP) else {
            return Ok(None);
        };
        let header = std::str::from_utf8(&self.buf[..header_end])
            .map_err(|_| DecodeError::Header("non-UTF8 header".to_string()))?;

        let mut content_length: Option<usize> = None;
        for line in header.split("\r\n").filter(|l| !l.is_empty()) {
            let (name, value) = line
                .split_once(':')
                .ok_or_else(|| DecodeError::Header(format!("malformed header line: {line:?}")))?;
            if name.trim().eq_ignore_ascii_case("Content-Length") {
                let len: usize = value
                    .trim()
                    .parse()
                    .map_err(|_| DecodeError::Header(format!("bad Content-Length: {value:?}")))?;
                content_length = Some(len);
            } else {
                return Err(DecodeError::Header(format!(
                    "unsupported header: {}",
                    name.trim()
                )));
            }
        }

        let len = content_length
            .ok_or_else(|| DecodeError::Header("missing Content-Length".to_string()))?;
        if len > MAX_BODY_BYTES {
            return Err(DecodeError::Header(format!(
                "Content-Length {len} exceeds cap {MAX_BODY_BYTES}"
            )));
        }

        let body_start = header_end + SEP.len();
        if self.buf.len() < body_start + len {
            // Whole body not here yet.
            return Ok(None);
        }
        let body = self.buf[body_start..body_start + len].to_vec();
        self.buf.drain(..body_start + len);
        Ok(Some(body))
    }
}

/// Find the first occurrence of `needle` in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// An inbound-frame decoding failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    /// The frame header was malformed / unsupported.
    #[error("frame header error: {0}")]
    Header(String),
    /// The frame body failed JSON-RPC decoding.
    #[error("json decode error: {0}")]
    Json(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_request_round_trips_via_decoder() {
        let frame = encode_request(
            7,
            "workspace/subscribe",
            serde_json::json!({"workspace_id": "default"}),
        )
        .unwrap();
        // A request is not an RpcResponse, but the framing layer is shared —
        // re-decode the body manually to prove the header is well-formed.
        let sep = b"\r\n\r\n";
        let i = find(&frame, sep).unwrap();
        let body = &frame[i + sep.len()..];
        let req: RpcRequest = serde_json::from_slice(body).unwrap();
        assert_eq!(req.method, "workspace/subscribe");
        assert_eq!(req.id, RpcId::Number(7));
        assert_eq!(req.params["workspace_id"], "default");
        // Content-Length matches the body length.
        let header = std::str::from_utf8(&frame[..i]).unwrap();
        assert_eq!(header, format!("Content-Length: {}", body.len()));
    }

    fn response_frame(id: i64, result: &serde_json::Value) -> Vec<u8> {
        let resp = serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result});
        let body = serde_json::to_vec(&resp).unwrap();
        frame(&body)
    }

    #[test]
    fn decoder_yields_complete_frame() {
        let mut d = FrameDecoder::new();
        let f = response_frame(1, &serde_json::json!({"ok": true}));
        let out = d.push(&f).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, RpcId::Number(1));
        assert_eq!(out[0].result.as_ref().unwrap()["ok"], true);
    }

    #[test]
    fn decoder_reassembles_split_frame() {
        let mut d = FrameDecoder::new();
        let f = response_frame(2, &serde_json::json!({}));
        let mid = f.len() / 2;
        assert!(
            d.push(&f[..mid]).unwrap().is_empty(),
            "partial yields nothing"
        );
        let out = d.push(&f[mid..]).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, RpcId::Number(2));
    }

    #[test]
    fn decoder_splits_two_coalesced_frames() {
        let mut d = FrameDecoder::new();
        let mut both = response_frame(1, &serde_json::json!({}));
        both.extend(response_frame(2, &serde_json::json!({})));
        let out = d.push(&both).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].id, RpcId::Number(1));
        assert_eq!(out[1].id, RpcId::Number(2));
    }

    #[test]
    fn decoder_rejects_unsupported_header() {
        let mut d = FrameDecoder::new();
        let bad = b"Content-Type: x\r\nContent-Length: 2\r\n\r\n{}".to_vec();
        let err = d.push(&bad).unwrap_err();
        assert!(matches!(err, DecodeError::Header(_)));
    }

    #[test]
    fn decoder_rejects_bad_json_body() {
        let mut d = FrameDecoder::new();
        let body = b"not json";
        let mut bad = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        bad.extend_from_slice(body);
        let err = d.push(&bad).unwrap_err();
        assert!(matches!(err, DecodeError::Json(_)));
    }
}
