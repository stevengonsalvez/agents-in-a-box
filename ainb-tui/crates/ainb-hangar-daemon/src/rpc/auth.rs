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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, MutexGuard, PoisonError};

use ainb_hangar_core::clock::{HangarClock, SystemClock};
use ainb_hangar_core::token::{TokenKind, mint, sha256_hex};
use ainb_hangar_proto::auth::{HelloParams, UNAUTHORIZED};
use ainb_hangar_proto::{RpcError, RpcId, RpcRequest, RpcResponse, methods};
use ainb_hangar_store::repo::token::SocketTokenRepo;
use sqlx::SqlitePool;

/// Which surface a connection authenticated as.
///
/// The daemon used to have exactly ONE credential, so every authenticated
/// connection was the operator by definition. That stopped being true the
/// moment a MODEL's tool call could mint a confirm card a human is supposed to
/// be the only one who can answer: the copilot's tool server is a process the
/// operator's agent steers, and handing it the operator's own token would let
/// it answer its own cards, forge `fleet/message_send {actor: "operator"}`, and
/// call `attention/answer` around the gate entirely.
///
/// So the copilot gets its OWN token, minted per channel scope, and the
/// connection carries what that token means for as long as it lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Caller {
    /// An operator surface (the TUI plugin, the CLI, the macOS client): the
    /// daemon's own `0600` token.
    Operator,
    /// The copilot's MCP tool server, on a token minted for ONE channel scope.
    Copilot {
        /// The copilot channel the token was minted against. The gate files its
        /// cards here, so a card's `scope_key` names the conversation the call
        /// actually came from.
        scope_key: String,
    },
}

/// Every method a copilot connection may call, and nothing else.
///
/// The read tools, the gate, and the two writes the tool table can reach after
/// the gate said run. Deliberately absent: `fleet/confirm_answer` (answering its
/// own cards), `fleet/copilot_configure` (rewriting its own persona),
/// `fleet/acp_session_create`, and every `hangar/*` method.
///
/// `attention/answer` IS here, because `answer_need` is a real tool. Binding it
/// to the gate verdict that approved it needs a per-call capability the gate
/// would have to issue; until then the confirm card is what stands between an
/// injected transcript and that call.
const COPILOT_METHODS: &[&str] = &[
    methods::PING,
    methods::FLEET_COPILOT_GATE,
    methods::FLEET_SNAPSHOT,
    methods::ATTENTION_LIST,
    methods::ATTENTION_ANSWER,
    methods::FLEET_TRANSCRIPT_LIST,
    methods::FLEET_MESSAGE_SEND,
];

impl Caller {
    /// Refuse a method this caller's surface is not allowed to reach.
    ///
    /// This is the per-CONNECTION check. `require_fleet_capability` is a
    /// build-time gate over a static const array — it says what the daemon
    /// serves, never who may ask for it.
    ///
    /// # Errors
    ///
    /// [`UNAUTHORIZED`] when a copilot connection asks for a method outside
    /// [`COPILOT_METHODS`].
    pub fn authorize(&self, method: &str) -> Result<(), RpcError> {
        match self {
            Self::Operator => Ok(()),
            Self::Copilot { .. } if COPILOT_METHODS.contains(&method) => Ok(()),
            Self::Copilot { .. } => Err(RpcError {
                code: UNAUTHORIZED,
                message: format!("the copilot credential may not call {method}"),
                data: None,
            }),
        }
    }

    /// The copilot channel scope this connection is bound to, if any.
    #[must_use]
    pub fn copilot_scope(&self) -> Option<&str> {
        match self {
            Self::Operator => None,
            Self::Copilot { scope_key } => Some(scope_key),
        }
    }
}

/// Live copilot credentials: `sha256(plaintext) -> scope_key`.
///
/// ponytail: process-memory, not a table. A copilot token is only useful to the
/// tool-server process the daemon spawned through an ACP adapter it owns, and
/// that process dies with the daemon — so a credential that does not survive a
/// restart cannot strand anything. Move it into the store if the tool server
/// ever outlives its daemon.
static COPILOT_TOKENS: LazyLock<Mutex<HashMap<String, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn copilot_tokens() -> MutexGuard<'static, HashMap<String, String>> {
    COPILOT_TOKENS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Mint the credential the copilot's tool server presents, bound to `scope_key`.
///
/// Returns the PLAINTEXT, which the caller writes to a `0600` file and nowhere
/// else. Any previous credential for the same scope is revoked here: a session
/// is re-configured on `session/load`, and the adapter holding the old token is
/// already gone.
#[must_use]
pub fn mint_copilot_token(scope_key: &str) -> String {
    let minted = mint(TokenKind::Daemon, &mut rand::rngs::OsRng);
    let mut tokens = copilot_tokens();
    tokens.retain(|_, bound| bound != scope_key);
    tokens.insert(minted.sha256_hex, scope_key.to_string());
    minted.plaintext
}

/// The scope a presented token is bound to, when it is a copilot credential.
fn copilot_scope_for(token: &str) -> Option<String> {
    copilot_tokens().get(&sha256_hex(token)).cloned()
}

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
///
/// # Errors
///
/// Propagates the create/write failure.
pub fn write_token_file(path: &Path, plaintext: &str) -> std::io::Result<()> {
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
/// whose token verifies against the stored digest, or against a live copilot
/// credential.
///
/// Returns `Ok((ack, caller))` — the `{}` success envelope to write back plus
/// WHO this connection is for the rest of its life — or `Err(error_envelope)`,
/// which the caller writes before closing the connection.
pub async fn authenticate_first_frame(
    pool: &SqlitePool,
    body: &[u8],
) -> Result<(RpcResponse, Caller), RpcResponse> {
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
    // The copilot credential FIRST, and it is never the daemon token: a scoped
    // credential that also verified as the operator's would be no scope at all.
    if let Some(scope_key) = copilot_scope_for(&params.token) {
        return Ok((ack(req.id), Caller::Copilot { scope_key }));
    }
    match SocketTokenRepo::verify(pool, &params.token).await {
        Ok(true) => Ok((ack(req.id), Caller::Operator)),
        Ok(false) => Err(unauthorized(req.id, "invalid daemon token")),
        Err(e) => {
            tracing::warn!(error = %e, "hangar rpc: socket-token lookup failed");
            Err(unauthorized(req.id, "token verification unavailable"))
        }
    }
}

/// The `{}` success envelope echoing `id`.
fn ack(id: RpcId) -> RpcResponse {
    RpcResponse {
        jsonrpc: ainb_hangar_proto::jsonrpc_version(),
        id,
        result: Some(serde_json::json!({})),
        error: None,
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

    /// A copilot credential authenticates as the COPILOT, bound to its scope,
    /// and the daemon's own token still authenticates as the operator.
    #[tokio::test]
    async fn a_copilot_token_authenticates_as_the_copilot_and_not_the_operator() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let path = ensure_socket_token(store.pool(), dir.path()).await.unwrap();
        let daemon = std::fs::read_to_string(&path).unwrap().trim().to_string();

        let copilot = mint_copilot_token("channel:01J0COPILOT");
        assert_ne!(
            copilot, daemon,
            "the copilot must not be handed the daemon's credential"
        );

        let hello = |token: &str| {
            serde_json::to_vec(&serde_json::json!({
                "jsonrpc": "2.0", "id": 1, "method": methods::AUTH_HELLO,
                "params": { "token": token }
            }))
            .unwrap()
        };
        let (_, caller) = authenticate_first_frame(store.pool(), &hello(&copilot))
            .await
            .expect("the copilot credential must authenticate");
        assert_eq!(
            caller,
            Caller::Copilot {
                scope_key: "channel:01J0COPILOT".to_string()
            }
        );
        let (_, operator) = authenticate_first_frame(store.pool(), &hello(&daemon))
            .await
            .expect("the daemon token still authenticates");
        assert_eq!(operator, Caller::Operator);

        // A re-mint for the same scope REVOKES the previous credential: the
        // adapter holding it is already gone.
        let replacement = mint_copilot_token("channel:01J0COPILOT");
        assert_ne!(replacement, copilot);
        assert!(
            authenticate_first_frame(store.pool(), &hello(&copilot)).await.is_err(),
            "a revoked copilot credential still authenticated"
        );
    }

    /// The copilot's allowed method set is exactly the tool table's reach.
    /// Everything a card's own answer flows through is refused.
    #[test]
    fn a_copilot_connection_cannot_answer_its_own_cards() {
        let copilot = Caller::Copilot {
            scope_key: "channel:01J0COPILOT".to_string(),
        };
        for allowed in COPILOT_METHODS {
            assert!(
                copilot.authorize(allowed).is_ok(),
                "{allowed} must be reachable"
            );
        }
        for refused in [
            methods::FLEET_CONFIRM_ANSWER,
            methods::FLEET_CONFIRM_LIST,
            methods::FLEET_COPILOT_CONFIGURE,
            methods::FLEET_ACP_SESSION_CREATE,
            methods::FLEET_CHANNEL_CREATE,
            methods::FLEET_ACTION,
        ] {
            let error = copilot.authorize(refused).expect_err("{refused} must be refused");
            assert_eq!(error.code, UNAUTHORIZED, "{refused}: {error:?}");
            // The operator's own surfaces are unaffected.
            assert!(Caller::Operator.authorize(refused).is_ok());
        }
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
