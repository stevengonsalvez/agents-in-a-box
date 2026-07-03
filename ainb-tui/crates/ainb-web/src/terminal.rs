//! Live WebSocket terminal bridge — `GET /ws/session/{id}`.
//!
//! This is the dashboard's one **write** surface: it attaches to a session's
//! tmux pane and streams it to the browser, forwarding keystrokes and resize
//! events back. Everything else in the crate is read-only.
//!
//! ## Security
//!
//! The terminal is gated twice over:
//!
//! * **Auth.** The route lives under `/api/*`'s sibling `/ws/session/` prefix
//!   and is covered by the same bearer-token middleware. Because a browser
//!   cannot set an `Authorization` header on a `WebSocket` upgrade, the token
//!   is accepted via the `?token=` query string — but *only* on this route and
//!   the SSE stream (see [`crate::auth`]); every JSON route still requires the
//!   header. The middleware runs before the upgrade, so an unauthenticated
//!   client never reaches [`session_ws`].
//! * **Read-only posture.** When the server runs `--read-only`, the upgrade is
//!   refused outright with `403` — the write surface simply does not exist in
//!   that mode. There is no "connect then reject input" half-state.
//!
//! ## Wire protocol
//!
//! After the upgrade the server emits JSON status frames and raw binary pane
//! output; the client sends JSON control frames:
//!
//! | Direction | Frame | Meaning |
//! |-----------|-------|---------|
//! | S → C | text  | `{"type":"status","event":"attached","session":"…"}` etc. |
//! | S → C | text  | `{"type":"error","code":"…","message":"…"}` |
//! | S → C | binary | raw PTY bytes — fed straight into xterm.js |
//! | C → S | text  | `{"type":"input","data":"ls\n"}` |
//! | C → S | text  | `{"type":"resize","cols":120,"rows":35}` |
//! | C → S | text  | `{"type":"ping"}` → `{"type":"status","event":"pong"}` |
//!
//! The PTY model mirrors agent-deck's `tmuxPTYBridge`: a real
//! `tmux attach-session` runs in a pseudo-terminal, so the pane is fully
//! interactive (TUIs, colour, cursor) rather than a polled text scrape. Raw
//! bytes flow to the browser unparsed because xterm.js *is* the terminal
//! emulator — re-parsing them server-side would be wasted work.

use axum::extract::ws::{Message, WebSocket};
use axum::extract::{Path, State, WebSocketUpgrade};
use axum::http::{Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc;

use crate::routes::AppState;

/// Minimum terminal dimensions accepted on a resize. Mirrors agent-deck's
/// `tmuxPTYBridge.Resize` lower bounds so a degenerate viewport can't drive
/// tmux into a 1x1 corner.
const MIN_COLS: u16 = 10;
/// See [`MIN_COLS`].
const MIN_ROWS: u16 = 3;

/// A control frame from the browser.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum ClientFrame {
    /// Forward keystrokes / paste to the pane.
    Input {
        /// Raw bytes to write to the PTY (already a UTF-8 string from the
        /// browser's keydown handler).
        data: String,
    },
    /// Resize the attached pane.
    Resize {
        /// New column count.
        cols: u16,
        /// New row count.
        rows: u16,
    },
    /// Liveness probe; answered with a `pong` status frame.
    Ping,
}

/// Resolve a session id to its tmux session name using the **cached** snapshot,
/// so a terminal connection never spawns its own `ainb list` subprocess — it
/// reads the same poller-maintained cache every other route does.
///
/// Matches either the session's `session_id` (the value in `/ws/session/{id}`)
/// or, as a convenience, its `tmux_session_name`. Returns `None` when no live
/// session matches or the pane has no tmux name.
async fn resolve_tmux_name(state: &AppState, id: &str) -> Option<String> {
    let snap = state.resolve_snapshot().await.ok()?;
    let sessions = snap.sessions.as_array()?;
    sessions.iter().find_map(|s| {
        let sid = s.get("session_id").and_then(|v| v.as_str());
        let tmux = s.get("tmux_session_name").and_then(|v| v.as_str());
        let matches = sid == Some(id) || tmux == Some(id);
        if matches {
            tmux.filter(|t| !t.is_empty()).map(str::to_string)
        } else {
            None
        }
    })
}

/// Middleware that refuses a **write surface** with `403 READ_ONLY` whenever the
/// server runs `--read-only`. It gates both fleet-state write surfaces: the WS
/// terminal (`/ws/session/:id`) and `POST /api/answer` (the daemon send seam).
/// It runs *before* the route handler (for the terminal, before the
/// [`WebSocketUpgrade`] extractor), so the surface is rejected at the routing
/// layer — there is no "upgrade/parse then reject" window, and the refusal is
/// observable without a real socket. Auth is enforced by
/// [`crate::auth::require_bearer`] alongside the rest of `/api/*`; this is the
/// second, posture-based gate.
pub async fn read_only_gate(
    State(state): State<AppState>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    if state.config.read_only {
        let body = axum::Json(json!({
            "error": {
                "code": "READ_ONLY",
                "message": "write surface is disabled in --read-only mode"
            }
        }));
        return (StatusCode::FORBIDDEN, body).into_response();
    }
    next.run(req).await
}

/// `GET /ws/session/{id}` — upgrade to a live terminal for session `{id}`.
///
/// The `--read-only` refusal happens in [`read_only_gate`] ahead of this
/// handler, and auth in the bearer middleware, so by the time we're here the
/// request is authenticated and the server is in control mode. We resolve the
/// pane and attach a PTY.
pub async fn session_ws(
    State(state): State<AppState>,
    Path(id): Path<String>,
    ws: WebSocketUpgrade,
) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, state, id))
}

/// Drive one upgraded WebSocket: resolve the pane, attach a PTY, then pump
/// PTY→socket and socket→PTY until either side closes.
async fn handle_socket(socket: WebSocket, state: AppState, id: String) {
    let (mut sink, mut stream) = {
        use futures_util::StreamExt;
        socket.split()
    };
    use futures_util::SinkExt;

    let Some(tmux_name) = resolve_tmux_name(&state, &id).await else {
        let _ = sink
            .send(Message::Text(
                json!({
                    "type": "error",
                    "code": "SESSION_NOT_FOUND",
                    "message": format!("no live tmux session for id {id}"),
                })
                .to_string(),
            ))
            .await;
        let _ = sink.send(Message::Close(None)).await;
        return;
    };

    // Spawn the PTY attach. `PtyBridge` owns the tmux child + reader/writer
    // threads; dropping it tears everything down.
    let mut bridge = match PtyBridge::attach(&tmux_name) {
        Ok(b) => b,
        Err(e) => {
            let _ = sink
                .send(Message::Text(
                    json!({
                        "type": "error",
                        "code": "ATTACH_FAILED",
                        "message": format!("could not attach to {tmux_name}: {e}"),
                    })
                    .to_string(),
                ))
                .await;
            let _ = sink.send(Message::Close(None)).await;
            return;
        }
    };

    let _ = sink
        .send(Message::Text(
            json!({
                "type": "status",
                "event": "attached",
                "session": tmux_name,
            })
            .to_string(),
        ))
        .await;

    let mut output_rx = bridge.take_output();

    // PTY → socket: raw pane bytes go out as binary frames; injected control
    // frames (e.g. a `pong`) go out as text. Runs until the PTY closes (channel
    // drains) or the socket write fails.
    let pump_out = tokio::spawn(async move {
        while let Some(out) = output_rx.recv().await {
            let frame = match out {
                Outbound::Pane(bytes) => Message::Binary(bytes),
                Outbound::Control(text) => Message::Text(text),
            };
            if sink.send(frame).await.is_err() {
                break;
            }
        }
        let _ = sink.send(Message::Close(None)).await;
    });

    // socket → PTY: decode control frames and apply them.
    while let Some(Ok(msg)) = {
        use futures_util::StreamExt;
        stream.next().await
    } {
        match msg {
            Message::Text(txt) => match serde_json::from_str::<ClientFrame>(&txt) {
                Ok(ClientFrame::Input { data }) => {
                    if bridge.write_input(data.as_bytes()).is_err() {
                        break;
                    }
                }
                Ok(ClientFrame::Resize { cols, rows }) => {
                    let cols = cols.max(MIN_COLS);
                    let rows = rows.max(MIN_ROWS);
                    let _ = bridge.resize(rows, cols);
                }
                Ok(ClientFrame::Ping) => {
                    // Liveness is implicit in the binary stream; the pong is
                    // emitted on the output channel so ordering is preserved.
                    bridge.send_pong();
                }
                Err(_) => { /* ignore malformed control frame */ }
            },
            Message::Close(_) => break,
            // Binary / ping / pong from the client are not part of the
            // protocol; ignore them.
            _ => {}
        }
    }

    // Either side closed — drop the bridge (kills the tmux attach client and
    // its threads) and stop the output pump.
    drop(bridge);
    pump_out.abort();
}

// ── PTY bridge ────────────────────────────────────────────────────────────

use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

/// Channel buffer for PTY output chunks before they're flushed to the socket.
/// Bounded so a stalled socket applies back-pressure on the reader thread
/// rather than buffering the pane unboundedly.
const OUTPUT_CHANNEL_CAP: usize = 256;

/// An item bound for the WebSocket, carried on the single ordered output
/// channel so pane bytes and injected control frames never race.
enum Outbound {
    /// Raw PTY bytes → a binary frame fed straight into xterm.js.
    Pane(Vec<u8>),
    /// A JSON control frame (e.g. a `pong`) → a text frame.
    Control(String),
}

/// Owns a `tmux attach-session` running in a pseudo-terminal plus the threads
/// streaming its I/O. Dropping it closes the PTY (the attach client detaches)
/// and joins nothing — the threads observe EOF / a closed channel and exit.
struct PtyBridge {
    master: Box<dyn MasterPty + Send>,
    /// Sender for keystrokes into the writer thread. Dropping it closes the
    /// channel and the writer thread exits.
    input_tx: std::sync::mpsc::SyncSender<Vec<u8>>,
    /// Receiver end of PTY output, taken once by [`PtyBridge::take_output`].
    output_rx: Option<mpsc::Receiver<Outbound>>,
    /// Cloned sender so we can inject protocol frames (e.g. a `pong`) into the
    /// same ordered stream the pane bytes flow through.
    output_tx: mpsc::Sender<Outbound>,
}

impl PtyBridge {
    /// Attach to `tmux_name` in a fresh PTY and start the reader/writer threads.
    fn attach(tmux_name: &str) -> anyhow::Result<Self> {
        let pty = native_pty_system();
        let pair = pty.openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let mut cmd = CommandBuilder::new("tmux");
        cmd.arg("attach-session");
        cmd.arg("-t");
        cmd.arg(tmux_name);
        cmd.env("TERM", "xterm-256color");

        // Spawn the child attached to the slave; we keep the master.
        let mut child = pair.slave.spawn_command(cmd)?;
        // The slave handle is no longer needed in this process once the child
        // owns it; dropping it lets the PTY signal EOF correctly on child exit.
        drop(pair.slave);

        let master = pair.master;

        // Reader thread: master → bounded async channel of byte chunks.
        let (output_tx, output_rx) = mpsc::channel::<Outbound>(OUTPUT_CHANNEL_CAP);
        {
            let mut reader = master.try_clone_reader()?;
            let tx = output_tx.clone();
            std::thread::Builder::new().name("ainb-web-pty-reader".into()).spawn(move || {
                use std::io::Read;
                let mut buf = [0u8; 8192];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break, // EOF: pane closed / session gone.
                        Ok(n) => {
                            // `blocking_send` applies back-pressure when the
                            // socket is slow; a closed channel means the WS
                            // is gone, so stop reading.
                            if tx.blocking_send(Outbound::Pane(buf[..n].to_vec())).is_err() {
                                break;
                            }
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(_) => break,
                    }
                }
            })?;
        }

        // Writer thread: keystroke queue → master.
        let (input_tx, input_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(1024);
        {
            let mut writer = master.take_writer()?;
            std::thread::Builder::new().name("ainb-web-pty-writer".into()).spawn(move || {
                use std::io::Write;
                while let Ok(bytes) = input_rx.recv() {
                    if writer.write_all(&bytes).and_then(|()| writer.flush()).is_err() {
                        break;
                    }
                }
            })?;
        }

        // Reaper thread: wait on the child so it doesn't linger as a zombie
        // after the pane closes. The thread exits when the child does.
        std::thread::Builder::new().name("ainb-web-pty-reaper".into()).spawn(move || {
            let _ = child.wait();
        })?;

        Ok(Self {
            master,
            input_tx,
            output_rx: Some(output_rx),
            output_tx,
        })
    }

    /// Take the PTY output receiver.
    ///
    /// # Invariant
    ///
    /// This consumes the single receiver and may be called **exactly once** per
    /// [`PtyBridge`]. The contract holds today: the only caller is
    /// [`handle_socket`], which owns the bridge for one WebSocket lifetime and
    /// takes the receiver once to drive the PTY→socket pump. The `debug_assert`
    /// below turns a future second-call regression into a loud test/dev failure
    /// instead of a latent production `expect` panic; behavior is unchanged.
    fn take_output(&mut self) -> mpsc::Receiver<Outbound> {
        debug_assert!(
            self.output_rx.is_some(),
            "take_output must be called exactly once per PtyBridge"
        );
        self.output_rx.take().expect("take_output called more than once")
    }

    /// Queue keystrokes for the PTY writer thread. Drops input (with a warning)
    /// only if the bounded queue is full (a wedged pane); errors only when the
    /// writer thread is gone.
    fn write_input(&self, bytes: &[u8]) -> anyhow::Result<()> {
        use std::sync::mpsc::TrySendError;
        match self.input_tx.try_send(bytes.to_vec()) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => {
                tracing::warn!(dropped = bytes.len(), "pty input queue full; dropping");
                Ok(())
            }
            Err(TrySendError::Disconnected(_)) => Err(anyhow::anyhow!("pty writer thread gone")),
        }
    }

    /// Resize the PTY (sends SIGWINCH to the tmux attach client, which
    /// re-arbitrates the pane). Bounds are already clamped by the caller.
    fn resize(&self, rows: u16, cols: u16) -> anyhow::Result<()> {
        self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        Ok(())
    }

    /// Inject a `pong` status frame into the output stream (answers a client
    /// `ping`). Best-effort: a full/closed channel just drops it. Using
    /// `try_send` keeps this non-blocking so a `ping` can never wedge the
    /// socket→PTY loop behind a slow consumer.
    fn send_pong(&self) {
        let frame = json!({ "type": "status", "event": "pong" }).to_string();
        let _ = self.output_tx.try_send(Outbound::Control(frame));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn client_frame_parses_input() {
        let f: ClientFrame = serde_json::from_str(r#"{"type":"input","data":"ls\n"}"#).unwrap();
        match f {
            ClientFrame::Input { data } => assert_eq!(data, "ls\n"),
            _ => panic!("expected input"),
        }
    }

    #[test]
    fn client_frame_parses_resize() {
        let f: ClientFrame =
            serde_json::from_str(r#"{"type":"resize","cols":120,"rows":35}"#).unwrap();
        match f {
            ClientFrame::Resize { cols, rows } => {
                assert_eq!(cols, 120);
                assert_eq!(rows, 35);
            }
            _ => panic!("expected resize"),
        }
    }

    #[test]
    fn client_frame_parses_ping() {
        let f: ClientFrame = serde_json::from_str(r#"{"type":"ping"}"#).unwrap();
        assert!(matches!(f, ClientFrame::Ping));
    }

    #[test]
    fn malformed_frame_is_an_error() {
        assert!(serde_json::from_str::<ClientFrame>(r#"{"type":"nope"}"#).is_err());
        assert!(serde_json::from_str::<ClientFrame>("not json").is_err());
    }
}
