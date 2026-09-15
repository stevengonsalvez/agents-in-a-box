//! In-memory registry for authenticated daemon surface connections.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use ainb_hangar_proto::connections::{
    ConnectionRow, ConnectionsListResult, SurfaceHost, SurfaceInfo, SurfaceKind,
};
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

/// One authenticated connection and what it may fold into.
#[derive(Debug)]
struct Entry {
    row: ConnectionRow,
    /// The client asked to be transient (#963): served and stamped like any
    /// other, and left out of the listing while a presence exists at `anchor`.
    transient: bool,
    /// The pid whose presence this connection folds into: its own surface pid,
    /// or for a plugin the host pid the daemon verified against the peer
    /// (#1040). `None` never folds.
    anchor: Option<u32>,
}

/// The parent process of `pid`, from the kernel's process table.
fn parent_pid(pid: u32) -> Option<u32> {
    #[cfg(target_os = "linux")]
    {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // `pid (comm) state ppid ...`; `comm` may itself hold spaces or parens.
        let after_comm = stat.rsplit_once(')')?.1;
        after_comm.split_whitespace().nth(1)?.parse().ok()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let output = std::process::Command::new("ps")
            .args(["-o", "ppid=", "-p", &pid.to_string()])
            .output()
            .ok()?;
        String::from_utf8_lossy(&output.stdout).trim().parse().ok()
    }
}

/// The pid a new connection may fold into, or `None`.
///
/// Any surface folds by its own non-zero pid, the #963 rule. A plugin folds by
/// its HOST's pid, and only when that pid is the connection's peer process (the
/// host runtime dialled on the plugin's behalf) or the peer's parent (the
/// plugin dialled itself), never pid 1: a claim the kernel does not back stays
/// listed (#1040).
fn anchor_for(
    surface: Option<&SurfaceInfo>,
    host: Option<SurfaceHost>,
    peer_pid: Option<u32>,
    parent_of: impl Fn(u32) -> Option<u32>,
) -> Option<u32> {
    let surface = surface?;
    if surface.kind != SurfaceKind::Plugin {
        return (surface.pid != 0).then_some(surface.pid);
    }
    let host = host.filter(|host| host.pid > 1)?;
    let peer = peer_pid?;
    (peer == host.pid || parent_of(peer) == Some(host.pid)).then_some(host.pid)
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
    ///
    /// Whether a transient row is listed is decided when the registry is READ,
    /// not latched here, so a connection that arrives in a presence gap (a
    /// plugin redialling before its host's lease after a daemon restart) folds
    /// as soon as the presence is back (#1040).
    pub async fn insert(
        &self,
        surface: Option<SurfaceInfo>,
        requested_transient: bool,
        host: Option<SurfaceHost>,
        peer_pid: Option<u32>,
    ) -> (ConnectionRow, bool) {
        let anchor = anchor_for(surface.as_ref(), host, peer_pid, parent_pid);
        let mut state = self.state.lock().await;
        let conn_id = state.next_conn_id;
        state.next_conn_id = state.next_conn_id.saturating_add(1);
        // A plugin acts for its host only once the kernel backed the claim
        // (`anchor_for`), so attribution (#1073) and folding share one check.
        let host_surface = host.filter(|host| {
            surface.as_ref().is_some_and(|surface| surface.kind == SurfaceKind::Plugin)
                && anchor == Some(host.pid)
        });
        let row = ConnectionRow {
            conn_id,
            surface: surface.unwrap_or_else(SurfaceInfo::unknown),
            host: self.host.clone(),
            connected_at: Utc::now(),
            tmux_clients: Vec::new(),
            host_surface,
        };
        state.rows.insert(
            conn_id,
            Entry {
                row: row.clone(),
                transient: requested_transient,
                anchor,
            },
        );
        let listed = state.listed_ids().contains(&conn_id);
        (row, listed)
    }

    /// Remove a connection which reached EOF or failed its request loop.
    ///
    /// Returns whether a LISTED row existed, so callers only emit a lifecycle
    /// event when the listed snapshot really changed.
    pub async fn remove(&self, conn_id: u64) -> bool {
        let mut state = self.state.lock().await;
        let listed = state.listed_ids().contains(&conn_id);
        state.rows.remove(&conn_id).is_some() && listed
    }

    /// Return the current registry in deterministic connection-id order.
    pub async fn list(&self) -> ConnectionsListResult {
        let state = self.state.lock().await;
        let listed = state.listed_ids();
        let mut connections: Vec<_> = state
            .rows
            .values()
            .filter(|entry| listed.contains(&entry.row.conn_id))
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
        if self.state.lock().await.listed_ids().is_empty() {
            return false;
        }

        let Some(clients) = probe().await else {
            return false;
        };
        let mut state = self.state.lock().await;
        let listed = state.listed_ids();
        let changed = state
            .rows
            .values()
            .any(|entry| listed.contains(&entry.row.conn_id) && entry.row.tmux_clients != clients);
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

impl RegistryState {
    /// The connection ids the registry lists right now.
    ///
    /// A presence is any connection that did not ask to be transient. A
    /// transient connection folds when a presence exists at its anchor, or when
    /// an earlier transient connection at the same pid was itself listed (so a
    /// process with no presence shows one row, not one per call). Evaluated in
    /// connection-id order so the answer is deterministic.
    fn listed_ids(&self) -> HashSet<u64> {
        let mut presences: HashSet<u32> = self
            .rows
            .values()
            .filter(|entry| !entry.transient)
            .filter_map(|entry| (entry.row.surface.pid != 0).then_some(entry.row.surface.pid))
            .collect();
        let mut entries: Vec<&Entry> = self.rows.values().collect();
        entries.sort_unstable_by_key(|entry| entry.row.conn_id);
        let mut listed = HashSet::new();
        for entry in entries {
            let folds =
                entry.transient && entry.anchor.is_some_and(|anchor| presences.contains(&anchor));
            if !folds {
                listed.insert(entry.row.conn_id);
                if let Some(anchor) = entry.anchor {
                    presences.insert(anchor);
                }
            }
        }
        listed
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

    use super::{ConnectionRegistry, anchor_for};

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

        let (presence, listed) = registry.insert(Some(tui.clone()), false, None, None).await;
        assert!(listed);
        let (call, listed) = registry.insert(Some(tui.clone()), true, None, None).await;
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
        let (early, listed) = registry.insert(Some(web.clone()), true, None, None).await;
        assert!(listed, "no client can hide itself by asking");
        // A presence at ANOTHER pid does not make pid 9's request honoured.
        registry
            .insert(
                Some(SurfaceInfo {
                    kind: SurfaceKind::Web,
                    pid: 10,
                }),
                false,
                None,
                None,
            )
            .await;
        let (_, listed) = registry.insert(Some(web.clone()), true, None, None).await;
        assert!(!listed, "the listed early row now holds pid 9's presence");
        // No surface, or pid 0, can never match a presence.
        let (_, listed) = registry.insert(None, true, None, None).await;
        assert!(listed);
        assert!(registry.remove(early.conn_id).await);
    }

    #[tokio::test]
    async fn a_refused_transient_request_is_probed_like_any_listed_row() {
        let registry = ConnectionRegistry::new();
        registry.insert(None, true, None, None).await;
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

    fn plugin(pid: u32) -> SurfaceInfo {
        SurfaceInfo {
            kind: SurfaceKind::Plugin,
            pid,
        }
    }

    fn tui_host(pid: u32) -> Option<ainb_hangar_proto::connections::SurfaceHost> {
        Some(ainb_hangar_proto::connections::SurfaceHost {
            kind: SurfaceKind::Tui,
            pid,
        })
    }

    /// #1053 review item 1: the fold is decided when the registry is read, so
    /// a plugin that dials first, in a presence gap after a daemon restart,
    /// folds once its host's lease is back.
    #[tokio::test]
    async fn a_plugin_that_dials_before_the_lease_folds_once_the_lease_is_back() {
        let registry = ConnectionRegistry::new();
        let tui_pid = std::process::id();
        let (plugin_row, listed) = registry
            .insert(
                Some(plugin(tui_pid + 1)),
                true,
                tui_host(tui_pid),
                Some(tui_pid),
            )
            .await;
        assert!(listed, "no presence yet: the plugin is listed");
        let (lease, listed) = registry
            .insert(
                Some(SurfaceInfo {
                    kind: SurfaceKind::Tui,
                    pid: tui_pid,
                }),
                false,
                None,
                None,
            )
            .await;
        assert!(listed);
        let rows = registry.list().await.connections;
        assert_eq!(
            rows.iter().map(|row| row.conn_id).collect::<Vec<_>>(),
            [lease.conn_id],
            "one row once the lease is back"
        );
        assert!(
            !registry.remove(plugin_row.conn_id).await,
            "a folded row's close is no change"
        );
        assert!(registry.remove(lease.conn_id).await);
    }

    /// #1053 review item 2: a plugin folds into its host only when the kernel
    /// backs the claim. Host pid equal to the peer (a host-relayed dial) or to
    /// the peer's parent folds; a pid that is neither stays listed, and pid 1
    /// never folds.
    #[test]
    fn a_plugin_host_claim_folds_only_when_the_peer_backs_it() {
        let parent_of = |pid: u32| (pid == 500).then_some(400);
        let surface = plugin(500);
        assert_eq!(
            anchor_for(Some(&surface), tui_host(400), Some(400), parent_of),
            Some(400),
            "the host runtime dialled: the peer is the host"
        );
        assert_eq!(
            anchor_for(Some(&surface), tui_host(400), Some(500), parent_of),
            Some(400),
            "the plugin dialled itself: its parent is the host"
        );
        assert_eq!(
            anchor_for(Some(&surface), tui_host(401), Some(500), parent_of),
            None,
            "a pid that is neither the peer nor its parent does not fold"
        );
        assert_eq!(
            anchor_for(Some(&surface), tui_host(1), Some(1), parent_of),
            None
        );
        assert_eq!(anchor_for(Some(&surface), None, Some(400), parent_of), None);
        assert_eq!(
            anchor_for(Some(&surface), tui_host(400), None, parent_of),
            None
        );
    }

    /// The kernel's parent lookup the host check relies on.
    #[test]
    fn parent_pid_reads_this_process_parent() {
        assert_eq!(
            super::parent_pid(std::process::id()),
            Some(std::os::unix::process::parent_id())
        );
    }

    /// #1053 review item 2, through the registry: a plugin claiming a host the
    /// peer does not back is listed beside that host's lease.
    #[tokio::test]
    async fn an_unbacked_plugin_claim_stays_listed_beside_the_lease() {
        let registry = ConnectionRegistry::new();
        let tui_pid = std::process::id();
        registry
            .insert(
                Some(SurfaceInfo {
                    kind: SurfaceKind::Tui,
                    pid: tui_pid,
                }),
                false,
                None,
                None,
            )
            .await;
        // The peer is pid 1 (init): neither the claimed host nor its child.
        let (_, listed) = registry.insert(Some(plugin(77)), true, tui_host(tui_pid), Some(1)).await;
        assert!(listed);
        assert_eq!(registry.list().await.connections.len(), 2);
    }

    /// #1073: with the TUI's presence lease and the hangar plugin's dial both
    /// in the registry, an answer through the plugin's connection is attributed
    /// `tui@<host>`, and a claim the peer does not back stays `plugin@<host>`.
    #[tokio::test]
    async fn an_answer_through_a_backed_plugin_dial_is_attributed_to_its_host() {
        let registry = ConnectionRegistry::new();
        let tui_pid = std::process::id();
        registry
            .insert(
                Some(SurfaceInfo {
                    kind: SurfaceKind::Tui,
                    pid: tui_pid,
                }),
                false,
                None,
                None,
            )
            .await;
        let (plugin_row, listed) = registry
            .insert(
                Some(plugin(tui_pid + 1)),
                true,
                tui_host(tui_pid),
                Some(tui_pid),
            )
            .await;
        assert!(!listed, "the plugin folds into the TUI");
        assert_eq!(
            crate::answer::answered_by(&plugin_row),
            format!("tui@{}", plugin_row.host)
        );

        let (unbacked, _) = registry
            .insert(Some(plugin(tui_pid + 2)), true, tui_host(tui_pid), Some(1))
            .await;
        assert_eq!(unbacked.host_surface, None);
        assert_eq!(
            crate::answer::answered_by(&unbacked),
            format!("plugin@{}", unbacked.host)
        );
    }
}
