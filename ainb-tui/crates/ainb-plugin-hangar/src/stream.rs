//! Daemon event-stream client wrapper.
//!
//! The Hangar daemon pushes domain events to subscribed plugins as JSON-RPC
//! *notifications* (`{jsonrpc, method, params}`, no `id`) over the
//! cap-mediated unix socket, in the same Content-Length framing the daemon
//! already speaks. [`StreamClient`] is the pure, IO-free heart of consuming
//! that stream:
//!
//! - [`StreamClient::push`] feeds raw socket byte chunks (the plugin glue
//!   relays them from the host `unix_socket_dial` cap), reassembles whole
//!   notification frames, decodes each `hangar/event` payload into a typed
//!   [`HangarEvent`], and returns them in receive order.
//! - Non-`hangar/event` notifications (e.g. daemon heartbeats) are skipped, not
//!   errored.
//! - An unknown event discriminant surfaces as [`StreamError::Decode`] — never
//!   a panic, never a silent drop (forward-compat surface).
//! - [`StreamClient::on_eof`] models reconnect-with-backoff: it resets the
//!   frame buffer (so a stale partial body can't poison the fresh stream) and
//!   returns the [`SubscribeReplay`] the caller must re-issue so the daemon
//!   resumes the same subscription.
//! - Reconnect timing lives in [`Backoff`] (50 ms / 200 ms / 1 s / 5 s, then
//!   capped at 5 s), reset on a successful connect.
//!
//! IO (dialling, sending, the actual async loop) is the plugin glue's job; this
//! module deliberately holds no socket and no tokio so the partial-read,
//! reconnect, and forward-compat behaviours are exhaustively unit-testable. The
//! framing reader is the [`FrameReader`] trait so a tripwire can fake the
//! socket with canned chunks.

use std::time::Duration;

use ainb_hangar_proto::events::{EVENT_METHOD, HangarEvent};

/// Soft cap on a single notification body. Daemon events are tiny; anything
/// larger is treated as a desync and surfaced as a [`StreamError`] rather than
/// buffered unboundedly.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

/// Reassembles Content-Length JSON-RPC notification frames from raw byte
/// chunks.
///
/// Extracted behind a trait so tripwires (and unit tests) can substitute a fake
/// that yields canned frames without a real socket. The production
/// implementation is [`ContentLengthReader`].
pub trait FrameReader {
    /// Append `chunk` and drain every complete frame body now available, in
    /// receive order. Partial trailing bytes are retained for the next call.
    ///
    /// # Errors
    ///
    /// Returns [`StreamError::Frame`] on a malformed header or an oversized
    /// Content-Length.
    fn push(&mut self, chunk: &[u8]) -> Result<Vec<Vec<u8>>, StreamError>;

    /// Discard any buffered partial frame (called on reconnect so a truncated
    /// body from the dead socket can't poison the fresh stream).
    fn reset(&mut self);
}

/// Production [`FrameReader`]: a self-contained Content-Length parser matching
/// the daemon's LSP-style wire framing.
#[derive(Debug, Default)]
pub struct ContentLengthReader {
    buf: Vec<u8>,
}

impl ContentLengthReader {
    /// A fresh reader with an empty buffer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Try to split one whole frame body off the front of the buffer.
    fn try_take_frame(&mut self) -> Result<Option<Vec<u8>>, StreamError> {
        const SEP: &[u8] = b"\r\n\r\n";
        let Some(header_end) = find(&self.buf, SEP) else {
            return Ok(None);
        };
        let header = std::str::from_utf8(&self.buf[..header_end])
            .map_err(|_| StreamError::Frame("non-UTF8 header".to_string()))?;

        let mut content_length: Option<usize> = None;
        for line in header.split("\r\n").filter(|l| !l.is_empty()) {
            let (name, value) = line
                .split_once(':')
                .ok_or_else(|| StreamError::Frame(format!("malformed header line: {line:?}")))?;
            if name.trim().eq_ignore_ascii_case("Content-Length") {
                let len: usize = value
                    .trim()
                    .parse()
                    .map_err(|_| StreamError::Frame(format!("bad Content-Length: {value:?}")))?;
                content_length = Some(len);
            } else {
                return Err(StreamError::Frame(format!(
                    "unsupported header: {}",
                    name.trim()
                )));
            }
        }

        let len = content_length
            .ok_or_else(|| StreamError::Frame("missing Content-Length".to_string()))?;
        if len > MAX_BODY_BYTES {
            return Err(StreamError::Frame(format!(
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

impl FrameReader for ContentLengthReader {
    fn push(&mut self, chunk: &[u8]) -> Result<Vec<Vec<u8>>, StreamError> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(body) = self.try_take_frame()? {
            out.push(body);
        }
        Ok(out)
    }

    fn reset(&mut self) {
        self.buf.clear();
    }
}

/// Find the first occurrence of `needle` in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// The subscribe request to re-issue after a reconnect so the daemon resumes
/// the same stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscribeReplay {
    /// JSON-RPC method (e.g. `"workspace/subscribe"`).
    pub method: String,
    /// JSON-RPC params (e.g. `{"workspace_id": "default"}`).
    pub params: serde_json::Value,
}

/// Exponential reconnect backoff, capped at 5 s.
///
/// Yields 50 ms, 200 ms, 1 s, 5 s, then holds at 5 s. [`Backoff::reset`]
/// restarts the schedule after a successful connect.
#[derive(Debug, Clone)]
pub struct Backoff {
    step: usize,
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new()
    }
}

impl Backoff {
    /// The fixed delay schedule in milliseconds; the last value is the cap.
    const SCHEDULE_MS: [u64; 4] = [50, 200, 1_000, 5_000];

    /// A fresh backoff positioned at the first delay.
    #[must_use]
    pub const fn new() -> Self {
        Self { step: 0 }
    }

    /// The next reconnect delay, advancing the schedule (saturating at the cap).
    pub fn next_delay(&mut self) -> Duration {
        let idx = self.step.min(Self::SCHEDULE_MS.len() - 1);
        let ms = Self::SCHEDULE_MS[idx];
        self.step = self.step.saturating_add(1);
        Duration::from_millis(ms)
    }

    /// Restart the schedule (call on a successful connect).
    pub const fn reset(&mut self) {
        self.step = 0;
    }
}

/// Pure consumer of one daemon event subscription.
///
/// Holds a [`FrameReader`] (the production [`ContentLengthReader`]), the
/// subscription's stream id (for namespacing distinct subscriptions so they
/// don't cross-talk), the subscribe request to replay on reconnect, and the
/// [`Backoff`] schedule. IO is the caller's responsibility.
pub struct StreamClient {
    stream_id: String,
    reader: ContentLengthReader,
    subscribe: SubscribeReplay,
    backoff: Backoff,
}

impl StreamClient {
    /// A client for `stream_id` with a default `workspace/subscribe` replay.
    ///
    /// Most callers use [`Self::with_subscribe`] to record the exact request to
    /// replay; this convenience seeds the common workspace subscription.
    #[must_use]
    pub fn new(stream_id: impl Into<String>) -> Self {
        Self::with_subscribe(
            stream_id,
            "workspace/subscribe",
            serde_json::json!({ "workspace_id": "default" }),
        )
    }

    /// A client that replays the exact `method` + `params` on reconnect.
    #[must_use]
    pub fn with_subscribe(
        stream_id: impl Into<String>,
        method: impl Into<String>,
        params: serde_json::Value,
    ) -> Self {
        Self {
            stream_id: stream_id.into(),
            reader: ContentLengthReader::new(),
            subscribe: SubscribeReplay {
                method: method.into(),
                params,
            },
            backoff: Backoff::new(),
        }
    }

    /// This subscription's stream id (namespacing rail).
    #[must_use]
    pub fn stream_id(&self) -> &str {
        &self.stream_id
    }

    /// The next reconnect delay (advances the backoff schedule).
    pub fn next_backoff(&mut self) -> Duration {
        self.backoff.next_delay()
    }

    /// Reset the backoff after a successful (re)connect.
    pub const fn on_connected(&mut self) {
        self.backoff.reset();
    }

    /// Feed a raw socket byte chunk and return every typed [`HangarEvent`] now
    /// available, in receive order.
    ///
    /// Non-`hangar/event` notifications are skipped. An unknown event
    /// discriminant returns [`StreamError::Decode`].
    ///
    /// # Errors
    ///
    /// Returns [`StreamError::Frame`] on a malformed frame and
    /// [`StreamError::Decode`] on a frame whose body isn't a decodable
    /// `hangar/event` payload.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<HangarEvent>, StreamError> {
        let frames = self.reader.push(chunk)?;
        let mut out = Vec::new();
        for body in frames {
            if let Some(ev) = decode_event_frame(&body)? {
                out.push(ev);
            }
        }
        Ok(out)
    }

    /// The daemon closed the socket. Reset the frame buffer (drop any truncated
    /// body) and return the [`SubscribeReplay`] the caller re-issues on
    /// reconnect.
    pub fn on_eof(&mut self) -> SubscribeReplay {
        self.reader.reset();
        self.subscribe.clone()
    }
}

/// Decode one notification frame body into an optional [`HangarEvent`].
///
/// Returns `Ok(None)` for a well-formed notification whose method is not
/// [`EVENT_METHOD`] (ignored, not an error). Returns [`StreamError::Decode`]
/// when the frame isn't a valid notification or the event payload is unknown.
fn decode_event_frame(body: &[u8]) -> Result<Option<HangarEvent>, StreamError> {
    let frame: NotificationFrame =
        serde_json::from_slice(body).map_err(|e| StreamError::Decode(e.to_string()))?;
    if frame.method != EVENT_METHOD {
        return Ok(None);
    }
    let ev: HangarEvent =
        serde_json::from_value(frame.params).map_err(|e| StreamError::Decode(e.to_string()))?;
    Ok(Some(ev))
}

/// The shape of an inbound JSON-RPC notification (no `id`).
#[derive(serde::Deserialize)]
struct NotificationFrame {
    method: String,
    #[serde(default)]
    params: serde_json::Value,
}

/// A failure consuming the event stream.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StreamError {
    /// The Content-Length frame header was malformed / unsupported.
    #[error("frame error: {0}")]
    Frame(String),
    /// The frame body wasn't a decodable `hangar/event` notification (bad JSON
    /// or an unknown event discriminant).
    #[error("event decode error: {0}")]
    Decode(String),
}
