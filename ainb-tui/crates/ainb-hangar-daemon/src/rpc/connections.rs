//! In-memory registry for authenticated daemon surface connections.

use std::collections::HashMap;
use std::sync::Arc;

use ainb_hangar_proto::connections::{ConnectionRow, ConnectionsListResult, SurfaceInfo};
use chrono::Utc;
use tokio::sync::Mutex;

/// Live daemon-side connection registry.
///
/// Rows deliberately never reach SQLite: a socket close, daemon restart, or
/// crashed client removes their authority, so only process-local state can be
/// truthful. Every returned row is sorted by `conn_id` for stable client views
/// and deterministic tests.
#[derive(Debug, Clone)]
pub struct ConnectionRegistry {
    state: Arc<Mutex<RegistryState>>,
    host: String,
}

#[derive(Debug)]
struct RegistryState {
    next_conn_id: u64,
    rows: HashMap<u64, Entry>,
}

/// One authenticated connection and whether it counts as a surface presence.
#[derive(Debug)]
struct Entry {
    row: ConnectionRow,
    /// `false` for a transient call connection (#963): served and stamped like
    /// any other, never listed, so one running surface is one row.
    listed: bool,
}

impl ConnectionRegistry {
    /// Start an empty registry, stamping all rows with this daemon's hostname.
    #[must_use]
    pub fn new() -> Self {
        let host = gethostname::gethostname().to_string_lossy().into_owned();
        Self {
            state: Arc::new(Mutex::new(RegistryState {
                next_conn_id: 1,
                rows: HashMap::new(),
            })),
            host,
        }
    }

    /// Insert one successfully authenticated connection and return its row,
    /// plus whether the row is listed.
    ///
    /// The DAEMON decides whether a connection is transient (#963): the
    /// client's `transient` request is honoured only when a listed row already
    /// exists for the same non-zero surface pid, which is the process's
    /// presence connection. Otherwise the connection is listed like any other,
    /// so no client can make itself invisible by asking. A transient connection
    /// still gets a row, because provenance for its requests is stamped from
    /// it, but it is never listed and never changes what [`Self::list`]
    /// returns.
    pub async fn insert(
        &self,
        surface: Option<SurfaceInfo>,
        requested_transient: bool,
    ) -> (ConnectionRow, bool) {
        let mut state = self.state.lock().await;
        let presence_held = surface.as_ref().is_some_and(|surface| {
            surface.pid != 0
                && state
                    .rows
                    .values()
                    .any(|entry| entry.listed && entry.row.surface.pid == surface.pid)
        });
        let listed = !(requested_transient && presence_held);
        let conn_id = state.next_conn_id;
        state.next_conn_id = state.next_conn_id.saturating_add(1);
        let row = ConnectionRow {
            conn_id,
            surface: surface.unwrap_or_else(SurfaceInfo::unknown),
            host: self.host.clone(),
            connected_at: Utc::now(),
            tmux_clients: Vec::new(),
        };
        state.rows.insert(
            conn_id,
            Entry {
                row: row.clone(),
                listed,
            },
        );
        (row, listed)
    }

    /// Remove a connection which reached EOF or failed its request loop.
    ///
    /// Returns whether a LISTED row existed, so callers only emit a lifecycle
    /// event when the listed snapshot really changed.
    pub async fn remove(&self, conn_id: u64) -> bool {
        self.state.lock().await.rows.remove(&conn_id).is_some_and(|entry| entry.listed)
    }

    /// Return the current registry in deterministic connection-id order.
    pub async fn list(&self) -> ConnectionsListResult {
        let state = self.state.lock().await;
        let mut connections: Vec<_> = state
            .rows
            .values()
            .filter(|entry| entry.listed)
            .map(|entry| entry.row.clone())
            .collect();
        connections.sort_unstable_by_key(|row| row.conn_id);
        ConnectionsListResult { connections }
    }

    /// Run one bounded tmux client probe and update every live row.
    ///
    /// Surface-to-session attribution arrives in a later phase. Until then each
    /// row carries the daemon's complete `tmux list-clients` picture, grouped by
    /// session in each string, so a surface can truthfully show the host count.
    /// A missing tmux binary, no server, or a failed command preserves the last
    /// successful snapshot and emits nothing.
    pub async fn refresh_tmux_clients(&self) -> bool {
        self.refresh_tmux_clients_with(Self::probe_tmux_clients).await
    }

    async fn refresh_tmux_clients_with<Probe, ProbeFuture>(&self, probe: Probe) -> bool
    where
        Probe: FnOnce() -> ProbeFuture,
        ProbeFuture: std::future::Future<Output = Option<Vec<String>>>,
    {
        if !self.state.lock().await.rows.values().any(|entry| entry.listed) {
            return false;
        }

        let Some(clients) = probe().await else {
            return false;
        };
        let mut state = self.state.lock().await;
        let changed = state
            .rows
            .values()
            .any(|entry| entry.listed && entry.row.tmux_clients != clients);
        if changed {
            for entry in state.rows.values_mut() {
                entry.row.tmux_clients.clone_from(&clients);
            }
        }
        changed
    }

    async fn probe_tmux_clients() -> Option<Vec<String>> {
        let output = match tokio::process::Command::new("tmux")
            .args([
                "list-clients",
                "-F",
                "#{session_name} #{client_tty} #{client_width}x#{client_height}",
            ])
            .output()
            .await
        {
            Ok(output) if output.status.success() => output,
            Ok(_) | Err(_) => return None,
        };
        Some(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect(),
        )
    }
}

impl Default for ConnectionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use ainb_hangar_proto::connections::{SurfaceInfo, SurfaceKind};

    use super::ConnectionRegistry;

    #[tokio::test]
    async fn empty_registry_skips_tmux_probe() {
        let registry = ConnectionRegistry::new();
        let probe_calls = AtomicUsize::new(0);

        let changed = registry
            .refresh_tmux_clients_with(|| {
                probe_calls.fetch_add(1, Ordering::SeqCst);
                std::future::ready(Some(vec!["session /dev/ttys001 80x24".to_string()]))
            })
            .await;

        assert!(!changed);
        assert_eq!(probe_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn transient_connections_are_never_listed_and_never_change_the_snapshot() {
        let registry = ConnectionRegistry::new();
        let tui = SurfaceInfo {
            kind: SurfaceKind::Tui,
            pid: 7,
        };

        let (presence, listed) = registry.insert(Some(tui.clone()), false).await;
        assert!(listed);
        let (call, listed) = registry.insert(Some(tui.clone()), true).await;
        assert!(!listed, "a call beside its process's presence is transient");
        assert_ne!(presence.conn_id, call.conn_id, "both connections get a row");
        assert_eq!(call.surface, tui, "the call row still carries provenance");

        let listed = registry.list().await.connections;
        assert_eq!(listed.len(), 1, "{listed:?}");
        assert_eq!(listed[0].conn_id, presence.conn_id);

        assert!(
            !registry.remove(call.conn_id).await,
            "closing a call connection must not emit a lifecycle event"
        );
        assert!(registry.remove(presence.conn_id).await);
        assert!(registry.list().await.connections.is_empty());
    }

    #[tokio::test]
    async fn a_transient_request_without_a_presence_at_its_pid_is_listed() {
        let registry = ConnectionRegistry::new();
        let web = SurfaceInfo {
            kind: SurfaceKind::Web,
            pid: 9,
        };

        // No presence yet at pid 9: the request is refused and the row listed.
        let (early, listed) = registry.insert(Some(web.clone()), true).await;
        assert!(listed, "no client can hide itself by asking");
        // A presence at ANOTHER pid does not make pid 9's request honoured.
        registry
            .insert(
                Some(SurfaceInfo {
                    kind: SurfaceKind::Web,
                    pid: 10,
                }),
                false,
            )
            .await;
        let (_, listed) = registry.insert(Some(web.clone()), true).await;
        assert!(!listed, "the listed early row now holds pid 9's presence");
        // No surface, or pid 0, can never match a presence.
        let (_, listed) = registry.insert(None, true).await;
        assert!(listed);
        assert!(registry.remove(early.conn_id).await);
    }

    #[tokio::test]
    async fn a_refused_transient_request_is_probed_like_any_listed_row() {
        let registry = ConnectionRegistry::new();
        registry.insert(None, true).await;
        let probe_calls = AtomicUsize::new(0);

        let changed = registry
            .refresh_tmux_clients_with(|| {
                probe_calls.fetch_add(1, Ordering::SeqCst);
                std::future::ready(Some(vec!["session /dev/ttys001 80x24".to_string()]))
            })
            .await;

        assert!(changed);
        assert_eq!(probe_calls.load(Ordering::SeqCst), 1);
    }
}
