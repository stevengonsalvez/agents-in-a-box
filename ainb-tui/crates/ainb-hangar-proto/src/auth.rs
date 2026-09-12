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

use crate::{
    RpcId, RpcRequest, connections::SurfaceInfo, jsonrpc_version, methods, protocol::ProtocolRange,
};

/// JSON-RPC error code the daemon answers when a connection's first frame is
/// not a valid `auth/hello`, or the presented token does not verify.
///
/// `-32000` is the first code in the JSON-RPC server-error range
/// (`-32000..=-32099`), distinct from the spec-reserved parse/dispatch codes.
pub const UNAUTHORIZED: i32 = -32000;

/// Params of an [`crate::methods::AUTH_HELLO`] request.
///
/// This is the FINAL shape (D17): `{ token, surface?, protocol, capabilities,
/// device? }`. It reaches it once, in W0-wire, and R1 adds nothing to hello,
/// the phase that introduces off-box devices fills in [`Self::device`], which
/// is why the member is here from the start rather than bolted on later.
///
/// Every member except `token` is absent-by-default, so the original
/// `{ token }` frame a pre-W0-wire client sends still decodes: it is read as
/// [`ProtocolRange::legacy`] with no declared capabilities, which is exactly
/// what that build is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloParams {
    /// The plaintext daemon token (`mdt_…`).
    pub token: String,
    /// Optional identity of the connected surface.
    ///
    /// Omitted by pre-registry clients. The daemon records those connections as
    /// [`crate::connections::SurfaceKind::Unknown`], preserving the original
    /// `{ token }` handshake shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface: Option<SurfaceInfo>,
    /// The protocol versions this client can speak (D17).
    ///
    /// Defaults to [`ProtocolRange::legacy`], version 1 and only 1, because
    /// that is what a client that does not send the member is.
    #[serde(default = "ProtocolRange::legacy")]
    pub protocol: ProtocolRange,
    /// The capability strings this client understands.
    ///
    /// Advisory in this direction: the daemon does not gate on it, it records
    /// it so a surface census can answer "which of my clients can already read
    /// the new event kind" without a release audit.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    /// The paired device this connection belongs to (R1, off-box only).
    ///
    /// Always `None` on the local unix leg, whose principal is the peer uid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<DeviceInfo>,
}

/// The paired device presenting a per-device token (D13 / R1).
///
/// Carried in hello rather than derived from the token so a daemon can log and
/// display WHICH device a socket belongs to before it has looked the token up,
/// and so the registry row and the connection agree on one id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfo {
    /// The device id minted at pairing (`device:<id>` is the ledger principal).
    pub device_id: String,
    /// Human-readable name, for the "paired: <name>" confirmation and the
    /// revoke list. Client-owned and renameable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

/// Result of a successful [`crate::methods::AUTH_HELLO`].
///
/// Pre-W0-wire daemons answer a bare `{}`, which decodes into this struct as
/// the legacy range with an empty catalogue: the honest reading of a daemon
/// that cannot tell you what it serves. That is the N-1-daemon leg of the skew
/// matrix, and it is why every member defaults.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct HelloResult {
    /// The protocol versions the DAEMON can speak.
    #[serde(default)]
    pub protocol: ProtocolRange,
    /// The version the two peers settled on: the highest both can speak.
    ///
    /// `None` from a daemon that does not negotiate, which a client reads as
    /// protocol 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected: Option<u32>,
    /// The daemon's capability catalogue.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    /// The daemon build version, for diagnostics only. Never branched on:
    /// that is what the version integer and the catalogue are for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daemon_version: Option<String>,
}

impl HelloResult {
    /// Whether the daemon advertised `capability`.
    #[must_use]
    pub fn advertises(&self, capability: &str) -> bool {
        self.capabilities.iter().any(|c| c == capability)
    }

    /// The negotiated version, treating a silent daemon as protocol 1.
    #[must_use]
    pub fn selected_or_legacy(&self) -> u32 {
        self.selected.unwrap_or(1)
    }
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
            surface: None,
            protocol: ProtocolRange::supported(),
            capabilities: crate::protocol::catalogue_strings(),
            device: None,
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
/// Delegates to the shared [`ainb_hangar_core::hangar_home`] resolver
/// (`$AINB_HANGAR_HOME` when set and non-empty, else `~/.agents-in-a-box` via
/// `dirs::home_dir`). Returns `None` when the home cannot be resolved. Both the
/// daemon (writer) and the plugin/CLI clients (readers) resolve the home the
/// same way, so this resolves to the one file the daemon wrote at boot — using
/// the shared helper (not a private `$HOME` read) is what keeps the writer and
/// reader from splitting when `dirs::home_dir` and `$HOME` disagree.
#[must_use]
pub fn default_token_file() -> Option<PathBuf> {
    Some(token_file_in(&ainb_hangar_core::hangar_home()?))
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
        assert_eq!(params.surface, None);
        assert_eq!(params.protocol, ProtocolRange::supported());
        assert!(
            params
                .capabilities
                .contains(&crate::protocol::CAP_AUTH_HELLO_NEGOTIATED.to_string())
        );
        assert_eq!(params.device, None);
    }

    /// The N-1 client leg of the skew matrix: a bare `{ token }` frame is what
    /// every pre-W0-wire client sends, and it must still decode, as version 1
    /// with nothing declared, never as an error.
    #[test]
    fn a_bare_token_frame_decodes_as_a_legacy_client() {
        let params: HelloParams =
            serde_json::from_value(serde_json::json!({ "token": "mdt_OLD" })).unwrap();
        assert_eq!(params.token, "mdt_OLD");
        assert_eq!(params.protocol, ProtocolRange::legacy());
        assert!(params.capabilities.is_empty());
        assert_eq!(params.device, None);
    }

    /// The N-1 daemon leg: a bare `{}` reply is what every pre-W0-wire daemon
    /// answers, and a current client must read it as "protocol 1, tells me
    /// nothing" rather than failing to decode.
    #[test]
    fn a_bare_ack_decodes_as_a_legacy_daemon() {
        let result: HelloResult = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(result.protocol, ProtocolRange::legacy());
        assert_eq!(result.selected_or_legacy(), 1);
        assert!(result.capabilities.is_empty());
        assert!(!result.advertises(crate::protocol::CAP_MUTATION_OP_ID));
    }

    /// A current daemon's reply survives an N-1 client's decoder: the extra
    /// members are ignored, which is the property that keeps the matrix green.
    #[test]
    fn a_negotiated_reply_still_decodes_into_the_old_empty_result() {
        let reply = serde_json::to_value(HelloResult {
            protocol: ProtocolRange::supported(),
            selected: Some(1),
            capabilities: crate::protocol::catalogue_strings(),
            daemon_version: Some("0.1.0".to_string()),
        })
        .unwrap();
        // The pre-W0-wire client deserialized the ack as an empty struct.
        #[derive(Deserialize)]
        struct LegacyAck {}
        assert!(serde_json::from_value::<LegacyAck>(reply).is_ok());
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
