//! Daemon connection state machine.
//!
//! P3.7's job is to dial the Hangar daemon through the host
//! `unix_socket_dial` cap, subscribe to `workspace:default`, and render a
//! colour-coded footer reflecting the link state. This module is the
//! *pure* heart of that: a state machine with no socket and no tokio, so
//! it is exhaustively unit-testable in isolation. The plugin glue
//! ([`crate::plugin`]) feeds it host events; it never touches I/O itself.
//!
//! ## State graph
//!
//! ```text
//! Disconnected
//!   │ dialing()                       (plugin/init → host cap dial issued)
//!   ▼
//! Dialing
//!   │ on_dialed(stream_id)            (host returned a socket stream id)
//!   ▼  on_error(...) → Error
//! Handshake
//!   │ on_subscribe_ack()             (daemon answered workspace/subscribe ok)
//!   ▼  on_error(...) → Error
//! Connected
//!   │ on_event()                     (workspace snapshot frames; stays Connected)
//!   │ on_eof()/on_error(...) → Disconnected/Error
//! ```
//!
//! `Disconnected` and `Error` are terminal-ish: a fresh `plugin/init`
//! (process restart) starts the cycle over from `dialing()`.

use std::fmt;

/// Default workspace the plugin subscribes to on connect.
pub const DEFAULT_WORKSPACE_ID: &str = "default";

/// Lifecycle state of the plugin's link to the Hangar daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnState {
    /// No link. Initial state and the state a clean teardown returns to.
    Disconnected,
    /// `unix_socket_dial` issued; awaiting the host's `stream_id`.
    Dialing,
    /// Socket open; `workspace/subscribe` sent, awaiting the daemon ack.
    Handshake,
    /// Subscribe acknowledged; receiving workspace events. Terminal-happy.
    Connected,
    /// A transport / protocol error broke the link. Carries a message for
    /// the footer + logs.
    Error(String),
}

impl ConnState {
    /// Footer label shown to the user (`"Hangar: <label>"`).
    #[must_use]
    pub const fn label(&self) -> &str {
        match self {
            Self::Disconnected => "Disconnected",
            Self::Dialing => "Dialing",
            Self::Handshake => "Handshake",
            Self::Connected => "Connected",
            Self::Error(_) => "Error",
        }
    }
}

impl fmt::Display for ConnState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Error(msg) => write!(f, "Error: {msg}"),
            other => f.write_str(other.label()),
        }
    }
}

/// The connection driver. Holds the current [`ConnState`] plus, once
/// dialled, the host-minted socket `stream_id` the plugin writes the
/// subscribe request to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connection {
    state: ConnState,
    /// Socket stream id from the host once `Dialing` completes.
    stream_id: Option<String>,
    /// Workspace this connection subscribes to.
    workspace_id: String,
}

impl Default for Connection {
    fn default() -> Self {
        Self::new()
    }
}

impl Connection {
    /// A fresh, [`ConnState::Disconnected`] connection bound to
    /// [`DEFAULT_WORKSPACE_ID`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: ConnState::Disconnected,
            stream_id: None,
            workspace_id: DEFAULT_WORKSPACE_ID.to_string(),
        }
    }

    /// Current link state.
    #[must_use]
    pub const fn state(&self) -> &ConnState {
        &self.state
    }

    /// Whether the link is fully established.
    #[must_use]
    pub const fn is_connected(&self) -> bool {
        matches!(self.state, ConnState::Connected)
    }

    /// The host socket `stream_id`, once dialled.
    #[must_use]
    pub fn stream_id(&self) -> Option<&str> {
        self.stream_id.as_deref()
    }

    /// The workspace id this connection subscribes to.
    #[must_use]
    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }

    /// Transition into [`ConnState::Dialing`] — a dial has been issued to
    /// the host cap. Idempotent: re-dialling drops any stale `stream_id`.
    pub fn dialing(&mut self) {
        self.stream_id = None;
        self.state = ConnState::Dialing;
    }

    /// The host returned a socket `stream_id`; move to
    /// [`ConnState::Handshake`]. Only valid from [`ConnState::Dialing`];
    /// a stray call from another state moves to [`ConnState::Error`].
    pub fn on_dialed(&mut self, stream_id: impl Into<String>) {
        if self.state == ConnState::Dialing {
            self.stream_id = Some(stream_id.into());
            self.state = ConnState::Handshake;
        } else {
            self.fail(format!(
                "on_dialed in unexpected state {}",
                self.state.label()
            ));
        }
    }

    /// The daemon acknowledged `workspace/subscribe`; move to
    /// [`ConnState::Connected`]. Only valid from [`ConnState::Handshake`].
    pub fn on_subscribe_ack(&mut self) {
        if self.state == ConnState::Handshake {
            self.state = ConnState::Connected;
        } else {
            self.fail(format!(
                "subscribe ack in unexpected state {}",
                self.state.label()
            ));
        }
    }

    /// A workspace event arrived while [`ConnState::Connected`]. Keeps the
    /// link `Connected`; out-of-state events are ignored (notifications
    /// race against teardown and must not flip a healthy link to `Error`).
    pub const fn on_event(&mut self) {
        // No state change: receiving events is the steady state.
    }

    /// The daemon closed the socket cleanly (`EOF`). Drops to
    /// [`ConnState::Disconnected`] and forgets the `stream_id`.
    pub fn on_eof(&mut self) {
        self.stream_id = None;
        self.state = ConnState::Disconnected;
    }

    /// A transport / protocol error broke the link. Moves to
    /// [`ConnState::Error`] and forgets the `stream_id`.
    pub fn on_error(&mut self, msg: impl Into<String>) {
        self.fail(msg.into());
    }

    fn fail(&mut self, msg: String) {
        self.stream_id = None;
        self.state = ConnState::Error(msg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_disconnected() {
        let c = Connection::new();
        assert_eq!(*c.state(), ConnState::Disconnected);
        assert!(!c.is_connected());
        assert_eq!(c.stream_id(), None);
        assert_eq!(c.workspace_id(), "default");
    }

    #[test]
    fn happy_path_reaches_connected() {
        let mut c = Connection::new();
        c.dialing();
        assert_eq!(*c.state(), ConnState::Dialing);

        c.on_dialed("sock-01");
        assert_eq!(*c.state(), ConnState::Handshake);
        assert_eq!(c.stream_id(), Some("sock-01"));

        c.on_subscribe_ack();
        assert_eq!(*c.state(), ConnState::Connected);
        assert!(c.is_connected());
    }

    #[test]
    fn events_keep_connection_connected() {
        let mut c = Connection::new();
        c.dialing();
        c.on_dialed("s");
        c.on_subscribe_ack();
        c.on_event();
        c.on_event();
        assert!(c.is_connected());
    }

    #[test]
    fn eof_returns_to_disconnected() {
        let mut c = Connection::new();
        c.dialing();
        c.on_dialed("s");
        c.on_subscribe_ack();
        c.on_eof();
        assert_eq!(*c.state(), ConnState::Disconnected);
        assert_eq!(c.stream_id(), None);
    }

    #[test]
    fn error_during_dial_moves_to_error() {
        let mut c = Connection::new();
        c.dialing();
        c.on_error("dial refused");
        assert_eq!(*c.state(), ConnState::Error("dial refused".to_string()));
        assert_eq!(c.stream_id(), None);
    }

    #[test]
    fn subscribe_ack_out_of_state_is_error() {
        let mut c = Connection::new();
        // Never dialled — ack arrives unexpectedly.
        c.on_subscribe_ack();
        assert!(matches!(c.state(), ConnState::Error(_)));
    }

    #[test]
    fn dialed_out_of_state_is_error() {
        let mut c = Connection::new();
        // Not in Dialing.
        c.on_dialed("oops");
        assert!(matches!(c.state(), ConnState::Error(_)));
    }

    #[test]
    fn redialing_clears_stale_stream_id() {
        let mut c = Connection::new();
        c.dialing();
        c.on_dialed("old");
        assert_eq!(c.stream_id(), Some("old"));
        // Simulate a restart loop: dial again.
        c.dialing();
        assert_eq!(*c.state(), ConnState::Dialing);
        assert_eq!(c.stream_id(), None);
    }

    #[test]
    fn labels_are_stable() {
        assert_eq!(ConnState::Disconnected.label(), "Disconnected");
        assert_eq!(ConnState::Dialing.label(), "Dialing");
        assert_eq!(ConnState::Handshake.label(), "Handshake");
        assert_eq!(ConnState::Connected.label(), "Connected");
        assert_eq!(ConnState::Error("x".into()).label(), "Error");
        assert_eq!(ConnState::Error("boom".into()).to_string(), "Error: boom");
    }
}
