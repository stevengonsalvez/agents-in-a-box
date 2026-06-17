// ABOUTME: The phone bridge's heartbeat emitter — closes the original blind spot.
//
// Before this, a running bridge that was connected to Telegram and relaying
// looked identical to a dead one: `ainb fleet bridge status` only reported
// launchd *install* state. This module makes the bridge write the shared daemon
// heartbeat (`~/.agents-in-a-box/daemons/bridge.json`) so the Daemons surface
// can show "Telegram channel online" + last-relay + error count.
//
// [`BridgeHeartbeat`] is a cheap clonable handle (an `Arc<Mutex<…>>`) shared by
// the Telegram and Slack channel tasks. Each mutation rewrites the file
// atomically; a write failure is logged and swallowed — a heartbeat must never
// take the daemon down. A background ticker calls [`BridgeHeartbeat::touch`]
// every few seconds so the liveness clock stays fresh even when the bridge is
// idle (no inbound messages), which is the common case.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::fleet::daemons::heartbeat::DaemonHeartbeat;

/// The heartbeat file name for the phone bridge. Must match
/// `fleet::daemons::probe::DaemonKind::Bridge.id()`.
const BRIDGE_NAME: &str = "bridge";

/// Cadence of the idle liveness refresh. Comfortably inside the probe's 90s
/// staleness window so an idle-but-healthy bridge is never flagged stale.
const TICK: Duration = Duration::from_secs(15);

/// A clonable handle the bridge channels share to report their health. Cloning
/// is cheap (an `Arc` bump); every clone writes the same `bridge.json`.
///
/// The ainb home is resolved ONCE at construction and carried on the handle, so
/// each flush is a pure write to a known path (no env lookup per beat) and tests
/// can inject a tempdir without touching process-global env.
#[derive(Clone)]
pub struct BridgeHeartbeat {
    inner: Arc<Mutex<DaemonHeartbeat>>,
    home: Arc<PathBuf>,
}

impl BridgeHeartbeat {
    /// Create a fresh handle for the current process and write the initial
    /// "starting" record under the resolved ainb home. A failure to resolve the
    /// home or to write is logged, not fatal — the bridge runs regardless.
    #[must_use]
    pub fn start() -> Self {
        let home = crate::fleet::plumbing::paths::ainb_home().unwrap_or_else(|e| {
            tracing::warn!(error = %e, "bridge heartbeat: could not resolve ainb home");
            PathBuf::from(".")
        });
        Self::start_in(home)
    }

    /// Construction seam: create a handle rooted at an explicit ainb home. Used
    /// by `start()` (real home) and by tests (a tempdir).
    #[must_use]
    pub fn start_in(home: impl Into<PathBuf>) -> Self {
        let handle = Self {
            inner: Arc::new(Mutex::new(DaemonHeartbeat::starting())),
            home: Arc::new(home.into()),
        };
        handle.flush();
        handle
    }

    /// Spawn the background liveness ticker. It refreshes the heartbeat every
    /// [`TICK`] for as long as the handle lives, so an idle bridge still proves
    /// it is alive. Returns the join handle (the daemon ignores it — the task
    /// ends when the process does).
    pub fn spawn_ticker(&self) -> tokio::task::JoinHandle<()> {
        let handle = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(TICK);
            // Skip the immediate first tick — `start()` already wrote one.
            interval.tick().await;
            loop {
                interval.tick().await;
                handle.touch();
            }
        })
    }

    /// Mark a channel connected (or not), labelling the channel. Called after
    /// `getMe` / `auth_test` succeeds.
    pub fn set_connected(&self, connected: bool, channel: Option<String>) {
        self.mutate(|hb| hb.set_connected(connected, channel));
    }

    /// Record a relayed message — the "last relay" signal the operator needs.
    pub fn record_relay(&self) {
        self.mutate(DaemonHeartbeat::record_activity);
    }

    /// Record an error string (a failed getUpdates poll, a send failure, …).
    pub fn record_error(&self, message: impl Into<String>) {
        let message = message.into();
        self.mutate(|hb| hb.record_error(message));
    }

    /// Refresh the liveness clock (the idle ticker path).
    pub fn touch(&self) {
        self.mutate(DaemonHeartbeat::touch);
    }

    /// Apply `f` to the in-memory record, then flush it to disk.
    fn mutate(&self, f: impl FnOnce(&mut DaemonHeartbeat)) {
        if let Ok(mut hb) = self.inner.lock() {
            f(&mut hb);
        }
        self.flush();
    }

    /// Write the current record to `<home>/daemons/bridge.json`. Best-effort: a
    /// failure is logged and swallowed so a transient FS error can never crash
    /// the bridge.
    fn flush(&self) {
        let snapshot = match self.inner.lock() {
            Ok(hb) => hb.clone(),
            Err(_) => return,
        };
        if let Err(e) = snapshot.write_in(self.home.as_path(), BRIDGE_NAME) {
            tracing::warn!(error = %e, "bridge heartbeat write failed (continuing)");
        }
    }

    /// The resolved ainb home this handle writes under.
    #[must_use]
    pub fn home(&self) -> &Path {
        self.home.as_path()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::daemons::heartbeat::heartbeat_path_in;

    #[test]
    fn start_in_writes_initial_record() {
        let home = tempfile::tempdir().unwrap();
        let _hb = BridgeHeartbeat::start_in(home.path());
        let on_disk = DaemonHeartbeat::read_in(home.path(), "bridge").expect("heartbeat written");
        assert_eq!(on_disk.pid, std::process::id());
        assert!(!on_disk.connected);
        assert!(heartbeat_path_in(home.path(), "bridge").exists());
    }

    #[test]
    fn set_connected_and_relay_persist() {
        let home = tempfile::tempdir().unwrap();
        let hb = BridgeHeartbeat::start_in(home.path());
        hb.set_connected(true, Some("Telegram (@bot)".into()));
        hb.record_relay();
        let on_disk = DaemonHeartbeat::read_in(home.path(), "bridge").unwrap();
        assert!(on_disk.connected);
        assert_eq!(on_disk.channel.as_deref(), Some("Telegram (@bot)"));
        assert!(on_disk.last_activity_at.is_some());
    }

    #[test]
    fn record_error_increments_on_disk() {
        let home = tempfile::tempdir().unwrap();
        let hb = BridgeHeartbeat::start_in(home.path());
        hb.record_error("getUpdates timeout");
        hb.record_error("send failed");
        let on_disk = DaemonHeartbeat::read_in(home.path(), "bridge").unwrap();
        assert_eq!(on_disk.error_count, 2);
        assert_eq!(on_disk.last_error.as_deref(), Some("send failed"));
    }

    #[test]
    fn clones_share_one_file() {
        let home = tempfile::tempdir().unwrap();
        let hb = BridgeHeartbeat::start_in(home.path());
        let clone = hb.clone();
        hb.set_connected(true, Some("Telegram".into()));
        clone.record_relay();
        let on_disk = DaemonHeartbeat::read_in(home.path(), "bridge").unwrap();
        // Both mutations landed in the single shared record.
        assert!(on_disk.connected);
        assert!(on_disk.last_activity_at.is_some());
        // The clone writes under the same injected home.
        assert_eq!(clone.home(), home.path());
    }
}
