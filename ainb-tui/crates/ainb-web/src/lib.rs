//! `ainb-web` — an SSE-live web dashboard + remote-control surface for the ainb
//! agent fleet.
//!
//! The dashboard surfaces three things at a glance: the live session list,
//! fleet `needs` (ASK/ERR/IDLE/WAIT), and cost rollups. On top of that read
//! surface it adds two control/depth features:
//!
//! * a **live terminal** ([`terminal`]) — `GET /ws/session/{id}` attaches to a
//!   session's tmux pane over a WebSocket (render + input + resize). This is a
//!   write surface, so it is auth-gated *and* refused in `--read-only` mode.
//! * **web-push** ([`push`]) — VAPID-authenticated browser notifications fired
//!   when a session enters an attention state, plus an installable PWA.
//!
//! ## Security model
//!
//! * Binds to loopback by default.
//! * Refuses a non-loopback bind unless a bearer `--token` is supplied or
//!   `--insecure-bind` is set explicitly (see [`config::WebConfig::check_bind_security`]).
//! * When a token is configured, every `/api/*` route (and the WS terminal)
//!   requires `Authorization: Bearer <token>` (401 otherwise). The `?token=`
//!   query fallback is scoped to the two streaming surfaces that cannot send a
//!   header — the SSE stream and the WS terminal — and no other route.
//! * The WS terminal is additionally refused with `403` whenever the server
//!   runs `--read-only`.
//!
//! ## Data flow
//!
//! Data is proxied from the existing `ainb --format json` commands
//! ([`data::AinbCliSource`]) so the browser view never drifts from the CLI/TUI.
//! A background poller refreshes the snapshot and pushes Server-Sent Events to
//! connected clients whenever the content fingerprint changes. The push
//! delivery loop and the terminal both read that same cached snapshot.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod assets;
pub mod auth;
pub mod config;
pub mod daemon;
pub mod data;
pub mod push;
pub mod routes;
pub mod sender;
pub mod terminal;

use std::sync::Arc;

pub use config::{BindError, WebConfig};
pub use daemon::{Answerer, DaemonAnswerer, DaemonClient, DaemonError};
pub use data::{AinbCliSource, DataError, DataSource, FleetSnapshot};
pub use routes::{AppState, router};

/// Errors raised while starting or running the dashboard server.
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    /// The requested bind address violated the security policy.
    #[error(transparent)]
    Bind(#[from] BindError),
    /// Binding the TCP listener failed (port in use, permission, …).
    #[error("failed to bind {addr}: {source}")]
    Listen {
        /// The address we tried to bind.
        addr: std::net::SocketAddr,
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// The HTTP server exited with an error.
    #[error("server error: {0}")]
    Server(#[source] std::io::Error),
}

/// Bind and serve the dashboard until the process is interrupted.
///
/// Enforces [`WebConfig::check_bind_security`] *before* opening any socket, so
/// an unsafe bind is refused with a clear error and never listens.
pub async fn serve(config: WebConfig, data: Arc<dyn DataSource>) -> Result<(), ServeError> {
    config.check_bind_security()?;

    let addr = config.listen;

    // Best-effort web-push init. A failure here (e.g. unwritable home dir) must
    // not take down the dashboard — push is an enhancement, the read surface
    // and terminal still work without it.
    let push = match push::PushState::init(
        "mailto:ainb@localhost",
        Arc::new(sender::WebPushSender::new()),
    ) {
        Ok(p) => Some(p),
        Err(e) => {
            tracing::warn!(error = %e, "web-push disabled (init failed)");
            None
        }
    };

    let state = AppState::with_push(config, data, push.clone());

    // Spawn the attention → push delivery loop only when push is configured.
    if let Some(push) = push {
        push::spawn_delivery(state.clone(), push);
    }

    let app = router(state);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|source| ServeError::Listen { addr, source })?;

    tracing::info!(%addr, "ainb web dashboard listening");
    axum::serve(listener, app).await.map_err(ServeError::Server)
}
