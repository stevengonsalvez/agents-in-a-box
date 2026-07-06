//! Runtime configuration + bind-security policy for `ainb web`.
//!
//! The security model mirrors agent-deck's `CheckBindSecurity`: the server
//! binds to loopback by default and *refuses* to start on a non-loopback
//! address unless the operator either supplies a bearer `--token` (so the
//! exposed surface is authenticated) or explicitly opts in with
//! `--insecure-bind`. Every `/api/*` route additionally requires the bearer
//! token when one is configured.

use std::net::{IpAddr, SocketAddr};

/// Why a requested bind address was refused. Carried out to the CLI layer so
/// it can render a clear, actionable refusal before any socket is opened.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BindError {
    /// The address is non-loopback and no token / insecure override was given.
    #[error(
        "refusing to bind to non-loopback address {addr} without authentication.\n\
         This would expose the dashboard to your network unauthenticated.\n\
         Pass --token <secret> to require a bearer token, or --insecure-bind to override."
    )]
    NonLoopbackWithoutToken {
        /// The offending address.
        addr: SocketAddr,
    },
    /// `--insecure-bind` was used to expose a *write-enabled* surface (no
    /// `--read-only`) with no token. The live WS terminal is effectively shell
    /// access to every fleet session, so we refuse this combination outright.
    #[error(
        "refusing --insecure-bind on {addr} while the live terminal write surface is enabled.\n\
         An unauthenticated --insecure-bind would expose interactive shell access to every\n\
         fleet session to your whole network. Pair --insecure-bind with --read-only (viewer\n\
         only, terminal disabled), or pass --token <secret> to require a bearer token."
    )]
    InsecureBindWritable {
        /// The offending address.
        addr: SocketAddr,
    },
}

/// Immutable runtime configuration for the dashboard server.
#[derive(Debug, Clone)]
pub struct WebConfig {
    /// Address to bind the HTTP listener to.
    pub listen: SocketAddr,
    /// Optional bearer token. When `Some`, every `/api/*` request must carry
    /// `Authorization: Bearer <token>`; without it the API returns 401.
    pub token: Option<String>,
    /// Explicit override allowing a non-loopback bind with no token. Mirrors
    /// agent-deck's `--insecure-bind`. Refused unless paired with `read_only`
    /// or a token, because an unauthenticated write surface exposes the fleet
    /// write paths (the live WS terminal — shell access to every session — and
    /// `POST /api/answer`, the daemon send seam) to the network.
    pub insecure_bind: bool,
    /// Defaults to the full control mode (the write surfaces are served).
    /// Setting `--read-only` flips this on, disabling EVERY write surface — the
    /// WS terminal and `POST /api/answer` (both gated by
    /// [`crate::terminal::read_only_gate`]) — so the dashboard is viewer-only and
    /// never mutates fleet state. Surfaced in `/healthz`.
    pub read_only: bool,
}

impl WebConfig {
    /// Validate the bind address against the security policy. Returns the
    /// refusal reason if the bind would expose an unauthenticated surface.
    ///
    /// Policy (first matching rule wins):
    /// 1. A bearer token is configured → allow (the surface is authenticated).
    /// 2. `--insecure-bind` was passed → allow ONLY when the surface is
    ///    `--read-only`; refuse otherwise, because an unauthenticated write
    ///    surface exposes the fleet write paths (the live WS terminal — shell
    ///    access to every session — and `POST /api/answer`, the daemon send
    ///    seam) to the network. In `--read-only` mode every write surface is
    ///    gated off, so no such path is reachable.
    /// 3. The host is loopback (`127.0.0.0/8`, `::1`) → allow.
    /// 4. Otherwise → refuse.
    pub fn check_bind_security(&self) -> Result<(), BindError> {
        if self.token.is_some() {
            return Ok(());
        }
        if self.insecure_bind {
            // The override is honored only for a viewer-only surface. A
            // write-enabled --insecure-bind with no token is exactly the
            // unauthenticated shell-access exposure we must refuse.
            if self.read_only {
                return Ok(());
            }
            return Err(BindError::InsecureBindWritable { addr: self.listen });
        }
        if is_loopback(self.listen.ip()) {
            return Ok(());
        }
        Err(BindError::NonLoopbackWithoutToken { addr: self.listen })
    }
}

/// True when `ip` is a loopback address. An unspecified address (`0.0.0.0` /
/// `::`) is treated as non-loopback because it binds every interface, so it
/// must clear the same authentication bar as an explicit public IP.
fn is_loopback(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => v6.is_loopback(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(addr: &str, token: Option<&str>, insecure: bool) -> WebConfig {
        cfg_ro(addr, token, insecure, true)
    }

    fn cfg_ro(addr: &str, token: Option<&str>, insecure: bool, read_only: bool) -> WebConfig {
        WebConfig {
            listen: addr.parse().unwrap(),
            token: token.map(str::to_string),
            insecure_bind: insecure,
            read_only,
        }
    }

    #[test]
    fn loopback_v4_allowed_without_token() {
        assert!(cfg("127.0.0.1:8420", None, false).check_bind_security().is_ok());
    }

    #[test]
    fn loopback_v6_allowed_without_token() {
        assert!(cfg("[::1]:8420", None, false).check_bind_security().is_ok());
    }

    #[test]
    fn non_loopback_without_token_is_refused() {
        let err = cfg("0.0.0.0:8420", None, false).check_bind_security().unwrap_err();
        assert!(matches!(err, BindError::NonLoopbackWithoutToken { .. }));
    }

    #[test]
    fn explicit_public_ip_without_token_is_refused() {
        let err = cfg("192.168.1.50:8420", None, false).check_bind_security().unwrap_err();
        assert!(matches!(err, BindError::NonLoopbackWithoutToken { .. }));
    }

    #[test]
    fn non_loopback_with_token_is_allowed() {
        assert!(cfg("0.0.0.0:8420", Some("s3cret"), false).check_bind_security().is_ok());
    }

    #[test]
    fn non_loopback_with_insecure_override_and_read_only_is_allowed() {
        // --insecure-bind is honored only for a viewer-only (read-only) surface.
        assert!(cfg_ro("0.0.0.0:8420", None, true, true).check_bind_security().is_ok());
    }

    #[test]
    fn insecure_bind_without_read_only_and_without_token_is_refused() {
        // The write surface (live WS terminal) is shell access to every fleet
        // session; --insecure-bind must NOT expose it unauthenticated.
        let err = cfg_ro("0.0.0.0:8420", None, true, false).check_bind_security().unwrap_err();
        assert!(matches!(err, BindError::InsecureBindWritable { .. }));
    }

    #[test]
    fn insecure_bind_writable_with_token_is_allowed() {
        // A token authenticates the write surface, so the override is moot.
        assert!(
            cfg_ro("0.0.0.0:8420", Some("s3cret"), true, false)
                .check_bind_security()
                .is_ok()
        );
    }
}
