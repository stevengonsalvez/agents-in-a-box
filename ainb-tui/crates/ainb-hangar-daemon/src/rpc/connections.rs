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
    rows: HashMap<u64, ConnectionRow>,
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

    /// Insert one successfully authenticated connection and return its row.
    pub async fn insert(&self, surface: Option<SurfaceInfo>) -> ConnectionRow {
        let mut state = self.state.lock().await;
        let conn_id = state.next_conn_id;
        state.next_conn_id = state.next_conn_id.saturating_add(1);
        let row = ConnectionRow {
            conn_id,
            surface: surface.unwrap_or_else(SurfaceInfo::unknown),
            host: self.host.clone(),
            connected_at: Utc::now(),
            tmux_clients: Vec::new(),
        };
        state.rows.insert(conn_id, row.clone());
        row
    }

    /// Remove a connection which reached EOF or failed its request loop.
    ///
    /// Returns whether a row existed, so callers only emit a lifecycle event
    /// when a live snapshot really changed.
    pub async fn remove(&self, conn_id: u64) -> bool {
        self.state.lock().await.rows.remove(&conn_id).is_some()
    }

    /// Return the current registry in deterministic connection-id order.
    pub async fn list(&self) -> ConnectionsListResult {
        let state = self.state.lock().await;
        let mut connections: Vec<_> = state.rows.values().cloned().collect();
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
            Ok(_) | Err(_) => return false,
        };
        let clients: Vec<String> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect();
        let mut state = self.state.lock().await;
        let changed = state.rows.values().any(|row| row.tmux_clients != clients);
        if changed {
            for row in state.rows.values_mut() {
                row.tmux_clients.clone_from(&clients);
            }
        }
        changed
    }
}

impl Default for ConnectionRegistry {
    fn default() -> Self {
        Self::new()
    }
}
