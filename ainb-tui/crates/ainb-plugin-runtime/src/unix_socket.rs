//! Host-side registry for `host/unix_socket_dial`.
//!
//! A *dialled socket* is a cap-gated, host-allocated connection to a
//! whitelisted `AF_UNIX` socket path. Unlike the
//! [`crate::event_stream`] registry (which re-emits topic publishes), a
//! dialled socket is a live bidirectional byte stream: the host owns the
//! [`UnixStream`], reads from it on a background task, and re-emits each
//! read under `plugin/handle_event` topic `socket:<stream_id>` wrapped in
//! a [`UnixSocketEvent`] envelope. The plugin writes back via
//! `host/unix_socket_send` and tears down via `host/unix_socket_close`.
//!
//! ## Why the host owns the socket
//!
//! Routing the dial through the host (instead of letting the plugin open
//! an `AF_UNIX` socket itself) buys two things:
//!
//! - **Whitelist enforcement.** The cap MUST be list-form; the requested
//!   path is canonicalized (symlinks resolved) and compared against the
//!   allow-list before any `connect`. This defends a shared dev box
//!   against arbitrary `AF_UNIX` abuse and against a symlink that resolves
//!   `~/.ainb/hangar.sock` to, say, `/var/run/docker.sock`.
//! - **Deterministic teardown.** Connections are keyed by which plugin
//!   dialled them; on plugin shutdown / crash / quarantine the host drops
//!   that plugin's sockets (aborting the read task + closing the stream)
//!   so no event leaks to a dead process.
//!
//! ## Stream ids
//!
//! `stream_id`s are host-minted ULIDs the plugin cannot forge — the plugin
//! observes events under `socket:<id>` and writes / closes using the same
//! id. A plugin can hold many concurrent dials, each with its own id.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use ainb_plugin_protocol::params::{UnixSocketEvent, UnixSocketEventKind};
use bytes::Bytes;
use parking_lot::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::UnixStream;

use crate::error::RuntimeError;
use crate::plugin_task::{Command, Inbox};
use crate::types::{PluginId, Topic};

/// Host-minted, unforgeable socket-stream identifier.
///
/// Backed by a ULID + monotonic suffix (same shape as
/// [`crate::event_stream::StreamId`]) so two dials in the same
/// millisecond produce distinct ids even under a frozen clock.
pub type SocketStreamId = String;

/// Allocator for [`SocketStreamId`]s. Cheap to clone (shares one `Arc`).
#[derive(Clone)]
pub struct SocketIdGen {
    seq: Arc<AtomicU64>,
}

impl std::fmt::Debug for SocketIdGen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SocketIdGen").finish_non_exhaustive()
    }
}

impl Default for SocketIdGen {
    fn default() -> Self {
        Self::new()
    }
}

impl SocketIdGen {
    /// Construct a fresh allocator.
    #[must_use]
    pub fn new() -> Self {
        Self {
            seq: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Allocate the next opaque, unforgeable socket-stream id.
    pub fn allocate(&self) -> SocketStreamId {
        let n = self.seq.fetch_add(1, Ordering::Relaxed);
        format!("{}-{n:08x}", ulid::Ulid::new())
    }
}

/// One live dialled socket: which plugin owns it, the write half (for
/// `host/unix_socket_send`), and the read-loop task handle (aborted on
/// close / teardown).
///
/// The write half is behind its own `tokio::sync::Mutex<Arc<_>>` so a
/// `send` can clone the `Arc` out under the (synchronous) registry lock,
/// release that lock, and only THEN `.await` the write — never holding the
/// `parking_lot` guard across an await point (which would make the
/// per-plugin task future non-`Send`).
struct DialedSocket {
    plugin: PluginId,
    write_half: Arc<tokio::sync::Mutex<OwnedWriteHalf>>,
    reader: tokio::task::JoinHandle<()>,
}

/// Concurrent registry of live dialled sockets.
///
/// Cheap to clone (single `Arc`). Shared between the [`crate::Runtime`]
/// and each per-plugin task (dial / send / close / teardown).
#[derive(Clone, Default)]
pub struct UnixSocketRegistry {
    inner: Arc<Mutex<HashMap<SocketStreamId, DialedSocket>>>,
    ids: SocketIdGen,
}

impl std::fmt::Debug for UnixSocketRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UnixSocketRegistry")
            .field("live", &self.inner.lock().len())
            .finish_non_exhaustive()
    }
}

impl UnixSocketRegistry {
    /// Construct an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Dial `path` on behalf of `plugin`, spawning a read loop that
    /// forwards every read into `plugin_inbox` as a
    /// `Command::HandleEvent` under topic `socket:<stream_id>`.
    ///
    /// The caller is responsible for the cap gate + path whitelist check
    /// ([`path_allowed`]) — this method only connects an already-vetted
    /// path. Returns the host-minted [`SocketStreamId`].
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError::Io`] if the connect fails (socket missing,
    /// permission denied, ...).
    pub async fn dial(
        &self,
        plugin: PluginId,
        path: &Path,
        plugin_inbox: Inbox,
    ) -> Result<SocketStreamId, RuntimeError> {
        let stream = UnixStream::connect(path).await.map_err(RuntimeError::Io)?;
        let (mut read_half, write_half) = stream.into_split();
        let stream_id = self.ids.allocate();
        let topic = Topic::from(socket_topic(&stream_id));

        let reader = tokio::spawn(async move {
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                match read_half.read(&mut buf).await {
                    Ok(0) => {
                        let _ = plugin_inbox.send(Command::HandleEvent {
                            topic: topic.clone(),
                            payload: encode_event(&UnixSocketEvent {
                                kind: UnixSocketEventKind::Eof,
                                bytes: None,
                                error: None,
                            }),
                        });
                        break;
                    }
                    Ok(n) => {
                        let chunk = Bytes::copy_from_slice(&buf[..n]);
                        if plugin_inbox
                            .send(Command::HandleEvent {
                                topic: topic.clone(),
                                payload: encode_event(&UnixSocketEvent {
                                    kind: UnixSocketEventKind::Data,
                                    bytes: Some(chunk),
                                    error: None,
                                }),
                            })
                            .is_err()
                        {
                            break; // plugin task gone
                        }
                    }
                    Err(e) => {
                        let _ = plugin_inbox.send(Command::HandleEvent {
                            topic: topic.clone(),
                            payload: encode_event(&UnixSocketEvent {
                                kind: UnixSocketEventKind::Error,
                                bytes: None,
                                error: Some(e.to_string()),
                            }),
                        });
                        break;
                    }
                }
            }
        });

        self.inner.lock().insert(
            stream_id.clone(),
            DialedSocket {
                plugin,
                write_half: Arc::new(tokio::sync::Mutex::new(write_half)),
                reader,
            },
        );
        Ok(stream_id)
    }

    /// Write `bytes` to a dialled socket the `plugin` owns.
    ///
    /// Ownership is enforced: a plugin can only write to a stream it
    /// dialled. Returns `true` if the write was dispatched (the stream was
    /// live + owned), `false` otherwise.
    pub async fn send(&self, plugin: &PluginId, stream_id: &str, bytes: &[u8]) -> bool {
        // Confirm ownership + clone the write-half `Arc` under the sync
        // registry lock, then DROP the guard before awaiting the write —
        // never hold a `parking_lot` guard across `.await`.
        let write_half = {
            let map = self.inner.lock();
            match map.get(stream_id) {
                Some(s) if &s.plugin == plugin => s.write_half.clone(),
                _ => return false,
            }
        };
        let mut wh = write_half.lock().await;
        wh.write_all(bytes).await.is_ok()
    }

    /// Close a single socket by id, if owned by `plugin`. Aborts its read
    /// task and drops the write half (closing the connection). Returns
    /// `true` if a socket was removed.
    pub fn close(&self, plugin: &PluginId, stream_id: &str) -> bool {
        let mut map = self.inner.lock();
        match map.get(stream_id) {
            Some(s) if &s.plugin == plugin => {
                if let Some(s) = map.remove(stream_id) {
                    s.reader.abort();
                }
                true
            }
            _ => false,
        }
    }

    /// Drop every socket owned by `plugin` (shutdown / crash /
    /// quarantine). Aborts each read task + closes each stream so no event
    /// leaks to a dead process. Returns the number of sockets dropped.
    pub fn drop_plugin(&self, plugin: &PluginId) -> usize {
        let mut map = self.inner.lock();
        let to_drop: Vec<SocketStreamId> = map
            .iter()
            .filter(|(_, s)| &s.plugin == plugin)
            .map(|(id, _)| id.clone())
            .collect();
        let n = to_drop.len();
        for id in to_drop {
            if let Some(s) = map.remove(&id) {
                s.reader.abort();
            }
        }
        n
    }

    /// Number of live sockets. Test/diagnostic helper.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.lock().len()
    }

    /// Whether the registry holds no sockets.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.lock().is_empty()
    }
}

/// JSON-encode a [`UnixSocketEvent`] for the `socket:<id>` event payload.
fn encode_event(ev: &UnixSocketEvent) -> Bytes {
    // The event is small + always serializable (plain structs / bytes).
    Bytes::from(serde_json::to_vec(ev).expect("UnixSocketEvent serializable"))
}

/// The delivery topic a dialled socket's frames arrive under:
/// `socket:<id>`.
#[must_use]
pub fn socket_topic(id: &str) -> String {
    format!("socket:{id}")
}

/// Expand a manifest allow-list entry (or a requested path) into a
/// concrete filesystem path.
///
/// Two expansions, both performed by the **host** (never the plugin):
/// - a leading `~/` is replaced with `$HOME`;
/// - `${VAR}` references are replaced with the value of `VAR` (empty if
///   unset).
///
/// Returns the expanded path as a [`PathBuf`]. No filesystem access — pure
/// string substitution; symlink resolution is a separate step
/// ([`canonical_for_compare`]).
#[must_use]
pub fn expand_path(raw: &str) -> PathBuf {
    let mut s = raw.to_string();
    // ${VAR} substitution.
    while let Some(start) = s.find("${") {
        if let Some(end_rel) = s[start..].find('}') {
            let end = start + end_rel;
            let var = &s[start + 2..end];
            let val = std::env::var(var).unwrap_or_default();
            s.replace_range(start..=end, &val);
        } else {
            break;
        }
    }
    // Leading `~/` expansion.
    if let Some(rest) = s.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return Path::new(&home).join(rest);
        }
    } else if s == "~" {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home);
        }
    }
    PathBuf::from(s)
}

/// Canonicalize `path` for whitelist comparison: resolve symlinks where
/// possible, else fall back to the lexically-expanded path.
///
/// `std::fs::canonicalize` requires the path to exist; a unix socket file
/// generally does exist when dialled, but we tolerate the missing case by
/// canonicalizing the **parent** directory and re-appending the final
/// component, so a non-existent socket whose parent is real still
/// compares correctly (and a symlinked parent is still resolved).
#[must_use]
pub fn canonical_for_compare(path: &Path) -> PathBuf {
    if let Ok(c) = std::fs::canonicalize(path) {
        return c;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => std::fs::canonicalize(parent)
            .map_or_else(|_| path.to_path_buf(), |p| p.join(name)),
        _ => path.to_path_buf(),
    }
}

/// Whether `requested` is permitted by a `unix_socket_dial` allow-list.
///
/// Both the requested path and each allow-list entry are expanded
/// ([`expand_path`]) and canonicalized ([`canonical_for_compare`], which
/// resolves symlinks) before an **exact** comparison — there is no
/// wildcard / prefix semantics for socket dials. This is what defends
/// against the symlink-redirect attack: a request for
/// `~/.ainb/hangar.sock` that is a symlink to `/var/run/docker.sock`
/// canonicalizes to `/var/run/docker.sock`, which won't match the
/// canonicalized allow-list entry.
///
/// The caller is responsible for the prior bool-true rejection (`-32003`)
/// and `is_granted()` gate — this function only answers "does the
/// allow-list name this (canonicalized) path?".
#[must_use]
pub fn path_allowed(allow_list: &[String], requested: &str) -> bool {
    let req = canonical_for_compare(&expand_path(requested));
    allow_list
        .iter()
        .any(|entry| canonical_for_compare(&expand_path(entry)) == req)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_topic_prefixes_id() {
        assert_eq!(socket_topic("01ABC"), "socket:01ABC");
    }

    #[test]
    fn socket_id_gen_unique_under_frozen_clock() {
        let g = SocketIdGen::new();
        let a = g.allocate();
        let b = g.allocate();
        assert_ne!(a, b);
        assert!(!a.is_empty());
    }

    #[test]
    fn expand_path_tilde() {
        std::env::set_var("HOME", "/home/cts");
        assert_eq!(expand_path("~/.ainb/hangar.sock"), PathBuf::from("/home/cts/.ainb/hangar.sock"));
        assert_eq!(expand_path("~"), PathBuf::from("/home/cts"));
    }

    #[test]
    fn expand_path_env_var() {
        std::env::set_var("CTS_RUNTIME_DIR", "/run/cts");
        assert_eq!(
            expand_path("${CTS_RUNTIME_DIR}/ainb-hangar.sock"),
            PathBuf::from("/run/cts/ainb-hangar.sock")
        );
    }

    #[test]
    fn path_allowed_exact_match() {
        // Use real temp paths so canonicalization is well-defined.
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("hangar.sock");
        std::fs::write(&sock, b"").unwrap();
        let allow = vec![sock.to_string_lossy().to_string()];
        assert!(path_allowed(&allow, &sock.to_string_lossy()));
    }

    #[test]
    fn path_allowed_rejects_unlisted() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("hangar.sock");
        std::fs::write(&sock, b"").unwrap();
        let other = dir.path().join("evil.sock");
        std::fs::write(&other, b"").unwrap();
        let allow = vec![sock.to_string_lossy().to_string()];
        assert!(!path_allowed(&allow, &other.to_string_lossy()));
    }

    #[test]
    fn path_allowed_empty_list_denies() {
        assert!(!path_allowed(&[], "/tmp/anything.sock"));
    }

    #[cfg(unix)]
    #[test]
    fn path_allowed_symlink_outside_whitelist_denied() {
        // The crux of the security test: an allow-listed name that is a
        // symlink resolving OUTSIDE the whitelist must be denied because
        // both sides canonicalize and the resolved target differs.
        let dir = tempfile::tempdir().unwrap();
        let real_target = dir.path().join("docker.sock");
        std::fs::write(&real_target, b"").unwrap();
        let whitelisted = dir.path().join("hangar.sock"); // the canonical allowed path
        std::fs::write(&whitelisted, b"").unwrap();

        // Attacker plants a symlink with a different name pointing at the
        // real (non-whitelisted) target, then requests it.
        let link = dir.path().join("attack.sock");
        std::os::unix::fs::symlink(&real_target, &link).unwrap();

        let allow = vec![whitelisted.to_string_lossy().to_string()];
        // Requesting the symlink resolves to docker.sock != hangar.sock.
        assert!(!path_allowed(&allow, &link.to_string_lossy()));
    }

    #[cfg(unix)]
    #[test]
    fn path_allowed_symlink_into_whitelist_allowed() {
        // Conversely, a symlink that resolves TO the whitelisted target
        // is allowed — the whitelist is about the resolved socket, not
        // the textual path the plugin happened to use.
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("hangar.sock");
        std::fs::write(&target, b"").unwrap();
        let link = dir.path().join("alias.sock");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let allow = vec![target.to_string_lossy().to_string()];
        assert!(path_allowed(&allow, &link.to_string_lossy()));
    }
}
