//! Connection-auth frame shape for the daemon's unix-socket RPC server.
//!
//! The daemon requires the **first frame** of every socket connection to be an
//! [`crate::methods::AUTH_HELLO`] request carrying the minted daemon token in
//! its params. This module is the single shared definition of that frame so
//! the daemon and every client (the hangar-tui plugin, test harnesses) agree
//! on the wire shape byte-for-byte.
//!
//! ## Handshake
//!
//! ```text
//! client                          daemon
//!   │  auth/hello {token}  ──────▶ │ verify sha256(token) vs stored hash
//!   │ ◀──────  {} ok               │ (constant-time, core::token::verify)
//!   │  workspace/subscribe ──────▶ │ normal dispatch from here on
//! ```
//!
//! An unauthenticated or wrong-token connection receives a JSON-RPC error
//! with code [`UNAUTHORIZED`] and the daemon closes the connection.
//!
//! ## Token handoff
//!
//! The daemon mints the token on boot (if absent), stores only its SHA-256
//! hex digest in the database, and writes the plaintext **once** to
//! `{hangar_home}/hangar/daemon.token` with `0600` permissions. Clients read
//! that file ([`default_token_file`]) and present its contents verbatim.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{RpcId, RpcRequest, jsonrpc_version, methods};

/// JSON-RPC error code the daemon answers when a connection's first frame is
/// not a valid `auth/hello`, or the presented token does not verify.
///
/// `-32000` is the first code in the JSON-RPC server-error range
/// (`-32000..=-32099`), distinct from the spec-reserved parse/dispatch codes.
pub const UNAUTHORIZED: i32 = -32000;

/// Params of an [`crate::methods::AUTH_HELLO`] request: the plaintext daemon
/// token read from the token file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloParams {
    /// The plaintext daemon token (`mdt_…`).
    pub token: String,
}

/// Build the `auth/hello` request envelope a client sends as its first frame.
#[must_use]
pub fn hello_request(id: i64, token: &str) -> RpcRequest {
    RpcRequest {
        jsonrpc: jsonrpc_version(),
        id: RpcId::Number(id),
        method: methods::AUTH_HELLO.to_string(),
        params: serde_json::json!(HelloParams {
            token: token.to_string(),
        }),
    }
}

/// The daemon token file inside a resolved Hangar home directory:
/// `{hangar_home}/hangar/daemon.token`.
///
/// Pure path join — resolution of the home itself is the caller's concern
/// (or [`default_token_file`]'s).
#[must_use]
pub fn token_file_in(hangar_home: &Path) -> PathBuf {
    hangar_home.join("hangar").join("daemon.token")
}

/// Resolve the daemon token file from the environment.
///
/// Mirrors every other Hangar consumer's home resolution: `$AINB_HANGAR_HOME`
/// when set and non-empty, else `$HOME/.agents-in-a-box`. Returns `None` when neither is
/// available. Both the daemon (writer) and the plugin/CLI clients (readers)
/// run under the same `$HOME`, so this resolves to the one file the daemon
/// wrote at boot.
#[must_use]
pub fn default_token_file() -> Option<PathBuf> {
    let home = match std::env::var_os("AINB_HANGAR_HOME").filter(|p| !p.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => PathBuf::from(std::env::var_os("HOME").filter(|p| !p.is_empty())?).join(".agents-in-a-box"),
    };
    Some(token_file_in(&home))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hello frame carries the method const and the token in params, and
    /// round-trips through the params struct.
    #[test]
    fn hello_request_shape_round_trips() {
        let req = hello_request(7, "mdt_SECRET");
        assert_eq!(req.method, methods::AUTH_HELLO);
        assert_eq!(req.id, RpcId::Number(7));
        let params: HelloParams = serde_json::from_value(req.params).unwrap();
        assert_eq!(params.token, "mdt_SECRET");
    }

    /// The token file lives at `{home}/hangar/daemon.token`.
    #[test]
    fn token_file_path_is_under_hangar_dir() {
        let p = token_file_in(Path::new("/tmp/h"));
        assert_eq!(p, PathBuf::from("/tmp/h/hangar/daemon.token"));
    }

    /// The unauthorized code sits in the JSON-RPC server-error range and away
    /// from the spec-reserved parse/dispatch codes.
    #[test]
    fn unauthorized_code_in_server_error_range() {
        assert!((-32099..=-32000).contains(&UNAUTHORIZED));
    }
}
