//! Socket-connection authentication for the daemon's RPC server (e38.1).
//!
//! Two layers gate every accepted connection before any `hangar/*` method is
//! reachable:
//!
//! 1. **Same-uid peer credentials** — the kernel-reported peer uid must match
//!    the daemon's own uid ([`same_uid_peer`]). `tokio`'s
//!    [`peer_cred`](tokio::net::UnixStream::peer_cred) reads `SO_PEERCRED` on
//!    Linux and `LOCAL_PEERCRED`/`getpeereid` on macOS, so both cfg paths are
//!    covered by the one call.
//! 2. **First-frame token auth** — the first decoded frame must be an
//!    `auth/hello` request ([`ainb_hangar_proto::auth`]) whose token verifies
//!    against the stored digest through the constant-time
//!    [`ainb_hangar_core::token::verify`] seam
//!    ([`SocketTokenRepo::verify`]). Anything else is answered with an
//!    [`UNAUTHORIZED`](ainb_hangar_proto::auth::UNAUTHORIZED) error and the
//!    connection is closed.
//!
//! ## Token lifecycle
//!
//! [`ensure_socket_token`] runs at boot, **before** the socket binds: when the
//! stored digest and the on-disk plaintext agree, the credential is reused;
//! otherwise a fresh `mdt_…` token is minted (CSPRNG), only its SHA-256 hex
//! digest is persisted (`daemon_socket_token`, migration 0011), and the
//! plaintext is written exactly once to `{hangar_home}/hangar/daemon.token`
//! with `0600` permissions. Clients (the hangar-tui plugin, test harnesses)
//! read that file and present it on their first frame.

use std::path::{Path, PathBuf};

use ainb_hangar_core::clock::{HangarClock, SystemClock};
use ainb_hangar_core::token::{TokenKind, mint};
use ainb_hangar_proto::auth::{HelloParams, UNAUTHORIZED};
use ainb_hangar_proto::{RpcError, RpcId, RpcRequest, RpcResponse, methods};
use ainb_hangar_store::repo::token::SocketTokenRepo;
use sqlx::SqlitePool;

/// Ensure a valid socket-auth credential exists, returning the token file path.
///
/// Reuses the existing credential when the database digest and the on-disk
/// plaintext still agree (so daemon restarts do not invalidate connected
/// clients' token files); otherwise mints a fresh token, stores its digest,
/// and (re)writes the plaintext file with `0600` permissions.
///
/// # Errors
///
/// Returns an error when the store read/write fails or the token file cannot
/// be written.
pub async fn ensure_socket_token(pool: &SqlitePool, hangar_home: &Path) -> anyhow::Result<PathBuf> {
    let path = ainb_hangar_proto::auth::token_file_in(hangar_home);

    // Reuse: both halves present and still matching.
    if let Some(stored) = SocketTokenRepo::get(pool).await? {
        if let Ok(existing) = std::fs::read_to_string(&path) {
            if ainb_hangar_core::token::verify(existing.trim(), &stored) {
                return Ok(path);
            }
        }
    }

    // Mint fresh: either half missing (or drifted) makes the pair unusable —
    // the plaintext is unrecoverable from the digest, so replace both.
    let minted = mint(TokenKind::Daemon, &mut rand::rngs::OsRng);
    SocketTokenRepo::set(pool, &minted.sha256_hex, SystemClock.now_ms()).await?;
    write_token_file(&path, &minted.plaintext)?;
    Ok(path)
}

/// Write the plaintext token to `path` with `0600` permissions.
///
/// The file is created fresh (any previous file is removed first) so the mode
/// is applied at create time and never widened by a pre-existing file's perms.
fn write_token_file(path: &Path, plaintext: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(path);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    writeln!(f, "{plaintext}")?;
    Ok(())
}

/// `true` when the connection's kernel-reported peer uid matches this
/// process's uid. Covers `SO_PEERCRED` (Linux) and `LOCAL_PEERCRED`/
/// `getpeereid` (macOS) through tokio's one cross-platform call.
///
/// # Errors
///
/// Propagates the `getsockopt` failure (the caller treats it as a rejection).
pub fn same_uid_peer(stream: &tokio::net::UnixStream) -> std::io::Result<bool> {
    let cred = stream.peer_cred()?;
    Ok(cred.uid() == nix::unistd::Uid::current().as_raw())
}

/// Validate a connection's first frame: it must be a well-formed `auth/hello`
/// whose token verifies against the stored digest.
///
/// Returns `Ok(ack)` (the `{}` success envelope to write back) when the
/// handshake passes, or `Err(error_envelope)` — the caller writes it and
/// closes the connection.
pub async fn authenticate_first_frame(
    pool: &SqlitePool,
    body: &[u8],
) -> Result<RpcResponse, RpcResponse> {
    let Ok(req) = serde_json::from_slice::<RpcRequest>(body) else {
        return Err(unauthorized(
            RpcId::Number(0),
            "first frame must be a well-formed auth/hello request",
        ));
    };
    if req.method != methods::AUTH_HELLO {
        return Err(unauthorized(
            req.id,
            "unauthenticated: first frame must be auth/hello with the daemon token",
        ));
    }
    let Ok(params) = serde_json::from_value::<HelloParams>(req.params.clone()) else {
        return Err(unauthorized(req.id, "auth/hello params must be { token }"));
    };
    match SocketTokenRepo::verify(pool, &params.token).await {
        Ok(true) => Ok(RpcResponse {
            jsonrpc: ainb_hangar_proto::jsonrpc_version(),
            id: req.id,
            result: Some(serde_json::json!({})),
            error: None,
        }),
        Ok(false) => Err(unauthorized(req.id, "invalid daemon token")),
        Err(e) => {
            tracing::warn!(error = %e, "hangar rpc: socket-token lookup failed");
            Err(unauthorized(req.id, "token verification unavailable"))
        }
    }
}

/// Build an `UNAUTHORIZED` error envelope echoing `id`.
fn unauthorized(id: RpcId, message: &str) -> RpcResponse {
    RpcResponse {
        jsonrpc: ainb_hangar_proto::jsonrpc_version(),
        id,
        result: None,
        error: Some(RpcError {
            code: UNAUTHORIZED,
            message: message.to_string(),
            data: None,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_hangar_store::Store;

    /// `ensure_socket_token` mints once and is then stable across calls: the
    /// digest in the database matches the sha256 of the on-disk plaintext, the
    /// file is `0600`, and a second call reuses (not replaces) the credential.
    #[tokio::test]
    async fn ensure_mints_once_then_reuses() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();

        let path = ensure_socket_token(store.pool(), dir.path()).await.unwrap();
        assert_eq!(path, dir.path().join("hangar").join("daemon.token"));

        let plaintext = std::fs::read_to_string(&path).unwrap().trim().to_string();
        assert!(plaintext.starts_with("mdt_"), "{plaintext}");
        let stored = SocketTokenRepo::get(store.pool()).await.unwrap().unwrap();
        assert_eq!(stored, ainb_hangar_core::token::sha256_hex(&plaintext));

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "token file must be owner-only");

        // Second call: same plaintext survives (no re-mint).
        ensure_socket_token(store.pool(), dir.path()).await.unwrap();
        let again = std::fs::read_to_string(&path).unwrap().trim().to_string();
        assert_eq!(
            again, plaintext,
            "a valid pair must be reused, not replaced"
        );
    }

    /// A missing token file (digest present in the DB) forces a re-mint — the
    /// plaintext is unrecoverable from the digest alone.
    #[tokio::test]
    async fn missing_file_forces_remint() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();

        let path = ensure_socket_token(store.pool(), dir.path()).await.unwrap();
        let first = SocketTokenRepo::get(store.pool()).await.unwrap().unwrap();
        std::fs::remove_file(&path).unwrap();

        ensure_socket_token(store.pool(), dir.path()).await.unwrap();
        let second = SocketTokenRepo::get(store.pool()).await.unwrap().unwrap();
        assert_ne!(first, second, "a lost plaintext must rotate the digest");
        let plaintext = std::fs::read_to_string(&path).unwrap().trim().to_string();
        assert!(SocketTokenRepo::verify(store.pool(), &plaintext).await.unwrap());
    }
}
