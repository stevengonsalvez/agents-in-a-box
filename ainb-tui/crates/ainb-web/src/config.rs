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
    /// agent-deck's `--insecure-bind`.
    pub insecure_bind: bool,
    /// Always `true` in this cut — the dashboard never mutates fleet state.
    /// Surfaced in `/healthz` and reserved as a clean extension point.
    pub read_only: bool,
}

impl WebConfig {
    /// Validate the bind address against the security policy. Returns the
    /// refusal reason if the bind would expose an unauthenticated surface.
    ///
    /// Policy (first matching rule wins):
    /// 1. A bearer token is configured → allow (the surface is authenticated).
    /// 2. `--insecure-bind` was passed → allow (explicit operator override).
    /// 3. The host is loopback (`127.0.0.0/8`, `::1`) → allow.
    /// 4. Otherwise → refuse.
    pub fn check_bind_security(&self) -> Result<(), BindError> {
        if self.token.is_some() || self.insecure_bind {
            return Ok(());
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
        WebConfig {
            listen: addr.parse().unwrap(),
            token: token.map(str::to_string),
            insecure_bind: insecure,
            read_only: true,
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
    fn non_loopback_with_insecure_override_is_allowed() {
        assert!(cfg("0.0.0.0:8420", None, true).check_bind_security().is_ok());
    }
}
