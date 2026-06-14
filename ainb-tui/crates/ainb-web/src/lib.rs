//! `ainb-web` — a read-only, SSE-live web dashboard for the ainb agent fleet.
//!
//! The dashboard surfaces three things at a glance: the live session list,
//! fleet `needs` (ASK/ERR/IDLE/WAIT), and cost rollups. It is **read-only** —
//! there are no mutate endpoints, no terminal bridge, and no web-push in this
//! cut (those are deliberate, clean extension points).
//!
//! ## Security model
//!
//! * Binds to loopback by default.
//! * Refuses a non-loopback bind unless a bearer `--token` is supplied or
//!   `--insecure-bind` is set explicitly (see [`config::WebConfig::check_bind_security`]).
//! * When a token is configured, every `/api/*` route requires
//!   `Authorization: Bearer <token>` (401 otherwise).
//!
//! ## Data flow
//!
//! Data is proxied from the existing `ainb --format json` commands
//! ([`data::AinbCliSource`]) so the browser view never drifts from the CLI/TUI.
//! A background poller refreshes the snapshot and pushes Server-Sent Events to
//! connected clients whenever the content fingerprint changes.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod assets;
pub mod auth;
pub mod config;
pub mod data;
pub mod routes;

use std::sync::Arc;

pub use config::{BindError, WebConfig};
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
    let state = AppState::new(config, data);
    let app = router(state);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|source| ServeError::Listen { addr, source })?;

    tracing::info!(%addr, "ainb web dashboard listening");
    axum::serve(listener, app).await.map_err(ServeError::Server)
}
