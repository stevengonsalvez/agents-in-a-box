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
    ///
    /// The message is scrubbed of any known secret shape (Telegram bot tokens,
    /// Slack `xox*` tokens) BEFORE it is stored — `last_error` is persisted to
    /// `bridge.json` and shown in the CLI/TUI, and a `reqwest::Error` Display can
    /// carry the token in the request URL. This is the defense-in-depth sink:
    /// even a caller that forgot to redact can't leak a token to disk (M1).
    pub fn record_error(&self, message: impl Into<String>) {
        let message = super::redact::scrub_secrets(&message.into());
        self.mutate(|hb| hb.record_error(message));
    }

    /// Record a SUCCESSFUL poll of the daemon's attention inbox by the outbound
    /// worker. This is the ONLY proof that the proactive-push half of the bridge
    /// is alive: the chat gateway being connected says nothing about it.
    pub fn record_attention_poll(&self) {
        self.mutate(DaemonHeartbeat::record_attention_poll);
    }

    /// Record a FAILED attempt to reach the attention source (socket down, token
    /// stale, daemon wedged). Scrubbed on the way in like every other persisted
    /// diagnostic, and sticky until a poll succeeds so the health surface can
    /// name the cause.
    pub fn record_attention_error(&self, message: impl Into<String>) {
        let message = super::redact::scrub_secrets(&message.into());
        self.mutate(|hb| hb.record_attention_error(message));
    }

    /// Record a proactive push that did NOT reach the human (the channel send
    /// failed). Sticky like the attention error and read by the health probe,
    /// because a bridge that polls fine but delivers nothing is exactly as
    /// useless to the human as one that cannot poll at all.
    pub fn record_delivery_error(&self, message: impl Into<String>) {
        let message = super::redact::scrub_secrets(&message.into());
        self.mutate(|hb| hb.record_delivery_error(message));
    }

    /// Clear the sticky delivery verdict once nothing is left undelivered.
    ///
    /// Guarded so the healthy path (every poll, forever) does not rewrite the
    /// file just to store the same `None`: the poll it accompanies has already
    /// refreshed the liveness clock.
    pub fn clear_delivery_error(&self) {
        let outstanding = self.inner.lock().is_ok_and(|hb| hb.last_delivery_error.is_some());
        if outstanding {
            self.mutate(DaemonHeartbeat::clear_delivery_error);
        }
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
    fn record_error_scrubs_telegram_and_slack_tokens_on_disk() {
        // M1 regression: a diagnostic carrying a bot token (as it appears in the
        // Telegram API URL) and a Slack token must NEVER reach the persisted
        // `last_error` — it is written to bridge.json and shown in the CLI/TUI.
        let home = tempfile::tempdir().unwrap();
        let hb = BridgeHeartbeat::start_in(home.path());
        hb.record_error(
            "getUpdates: phase=request kind=connect status=None source=[error sending request \
             for url (https://api.telegram.org/bot123456789:ABC-tok_ghiJKLmnopqrstuvwxyz012345/getUpdates)]",
        );
        hb.record_error("socket error: auth failed for xoxb-9999-8888-supersecretslacktoken");
        let on_disk = DaemonHeartbeat::read_in(home.path(), "bridge").unwrap();
        let last = on_disk.last_error.expect("last_error recorded");
        assert!(last.contains("<redacted>"), "expected redaction: {last}");
        assert!(
            !last.contains("ABC-tok_ghiJKLmnopqrstuvwxyz012345"),
            "telegram token body leaked to disk: {last}"
        );
        assert!(
            !last.contains("bot123456789:"),
            "telegram token prefix leaked to disk: {last}"
        );
        assert!(
            !last.contains("xoxb-9999-8888-supersecretslacktoken"),
            "slack token leaked to disk: {last}"
        );
        // The error count still increments — scrubbing must not drop the signal.
        assert_eq!(on_disk.error_count, 2);
    }

    #[test]
    fn attention_poll_and_error_persist_on_disk() {
        let home = tempfile::tempdir().unwrap();
        let hb = BridgeHeartbeat::start_in(home.path());
        // A brand-new bridge has never polled the attention source.
        let fresh = DaemonHeartbeat::read_in(home.path(), "bridge").unwrap();
        assert!(fresh.last_attention_poll_at.is_none());

        hb.record_attention_error("connect /tmp/hangar.sock: Connection refused");
        let failed = DaemonHeartbeat::read_in(home.path(), "bridge").unwrap();
        assert_eq!(failed.error_count, 1);
        assert!(failed.last_attention_error.unwrap().contains("hangar.sock"));
        assert!(failed.last_attention_poll_at.is_none());

        hb.record_attention_poll();
        let ok = DaemonHeartbeat::read_in(home.path(), "bridge").unwrap();
        assert!(ok.last_attention_poll_at.is_some());
        assert!(
            ok.last_attention_error.is_none(),
            "a successful poll clears the sticky attention error on disk"
        );
    }

    #[test]
    fn delivery_error_persists_and_clears_on_disk() {
        let home = tempfile::tempdir().unwrap();
        let hb = BridgeHeartbeat::start_in(home.path());
        let fresh = DaemonHeartbeat::read_in(home.path(), "bridge").unwrap();
        assert!(fresh.last_delivery_error.is_none());

        hb.record_delivery_error("outbound push: Discord: HTTP 429 rate limited");
        let failed = DaemonHeartbeat::read_in(home.path(), "bridge").unwrap();
        assert_eq!(failed.error_count, 1);
        assert!(
            failed.last_delivery_error.unwrap().contains("HTTP 429"),
            "the push failure must be visible to the health probe"
        );

        hb.clear_delivery_error();
        let cleared = DaemonHeartbeat::read_in(home.path(), "bridge").unwrap();
        assert!(cleared.last_delivery_error.is_none());
        assert_eq!(
            cleared.error_count, 1,
            "clearing the verdict must not rewrite history"
        );
    }

    #[test]
    fn clearing_a_delivery_error_that_is_not_set_writes_nothing() {
        // The healthy path runs this on every poll, forever. It must not churn
        // the file just to store the same `None`.
        let home = tempfile::tempdir().unwrap();
        let hb = BridgeHeartbeat::start_in(home.path());
        let before = std::fs::metadata(heartbeat_path_in(home.path(), "bridge"))
            .unwrap()
            .modified()
            .unwrap();
        let before_record = DaemonHeartbeat::read_in(home.path(), "bridge").unwrap();
        hb.clear_delivery_error();
        let after = std::fs::metadata(heartbeat_path_in(home.path(), "bridge"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(before, after, "a no-op clear must not rewrite the file");
        assert_eq!(
            DaemonHeartbeat::read_in(home.path(), "bridge").unwrap(),
            before_record
        );
    }

    #[test]
    fn record_delivery_error_scrubs_tokens_on_disk() {
        let home = tempfile::tempdir().unwrap();
        let hb = BridgeHeartbeat::start_in(home.path());
        hb.record_delivery_error("outbound push: xoxb-9999-8888-supersecretslacktoken rejected");
        let on_disk = DaemonHeartbeat::read_in(home.path(), "bridge").unwrap();
        let last = on_disk.last_delivery_error.expect("delivery error recorded");
        assert!(
            !last.contains("xoxb-9999-8888-supersecretslacktoken"),
            "slack token leaked to disk: {last}"
        );
        assert!(last.contains("<redacted>"), "expected redaction: {last}");
    }

    #[test]
    fn record_attention_error_scrubs_tokens_on_disk() {
        let home = tempfile::tempdir().unwrap();
        let hb = BridgeHeartbeat::start_in(home.path());
        hb.record_attention_error("poll failed: xoxb-9999-8888-supersecretslacktoken");
        let on_disk = DaemonHeartbeat::read_in(home.path(), "bridge").unwrap();
        let last = on_disk.last_attention_error.expect("attention error recorded");
        assert!(
            !last.contains("xoxb-9999-8888-supersecretslacktoken"),
            "slack token leaked to disk: {last}"
        );
        assert!(last.contains("<redacted>"), "expected redaction: {last}");
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
