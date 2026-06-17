// ABOUTME: The daemon heartbeat status file — `~/.agents-in-a-box/daemons/<name>.json`.
//
// A long-running ainb daemon (the phone bridge, the fleet auto-continue watcher)
// rewrites this tiny file on startup and on every heartbeat tick. The Daemons
// observability surface (the `ainb fleet daemons` CLI verb + the TUI screen)
// reads it and cross-checks `pid` liveness so a stale file from a *crashed*
// daemon shows "stopped (stale heartbeat)" rather than a false "running".
//
// The shape is deliberately tiny and stable so it stays cheap to rewrite every
// few seconds and forward-compatible (new fields are added with `#[serde(default)]`):
//
//   {
//     "pid": 12345,
//     "started_at": 1700000000000,        // epoch ms — daemon process start
//     "last_heartbeat_at": 1700000005000, // epoch ms — last refresh of this file
//     "last_activity_at": 1700000004000,  // epoch ms — last real work (relay, scan)
//     "connected": true,                  // channel/peer health (daemon-specific)
//     "channel": "Telegram (@mybot)",     // optional human label for the connection
//     "last_error": "getUpdates timeout", // optional last error string
//     "error_count": 0                    // monotonic error counter for this run
//   }
//
// Heartbeats that already have a richer signal elsewhere (ATC's
// `heartbeat-state.json`, notifyd's PID file + socket + DB) are READ in
// `super::probe` instead of re-emitted — only daemons with no existing
// observability write this file.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::fleet::plumbing::atomic::write_atomic_json;

/// Epoch-milliseconds clock, matching the rest of the plumbing's `ts` fields.
#[must_use]
pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// `<ainb_home>/daemons` — the directory holding every daemon heartbeat file.
/// Honours `$AINB_HOME` via the shared plumbing resolver so tests isolate to a
/// tempdir without touching `$HOME`.
pub fn daemons_dir() -> Result<PathBuf> {
    Ok(crate::fleet::plumbing::paths::ainb_home()?.join("daemons"))
}

/// `<home>/daemons` under an explicit ainb home (the test seam).
#[must_use]
pub fn daemons_dir_in(home: &Path) -> PathBuf {
    home.join("daemons")
}

/// Path to `<name>`'s heartbeat file under the default ainb home.
pub fn heartbeat_path(name: &str) -> Result<PathBuf> {
    Ok(daemons_dir()?.join(format!("{}.json", sanitize(name))))
}

/// Path to `<name>`'s heartbeat file under an explicit ainb home.
#[must_use]
pub fn heartbeat_path_in(home: &Path, name: &str) -> PathBuf {
    daemons_dir_in(home).join(format!("{}.json", sanitize(name)))
}

/// The on-disk heartbeat record one daemon rewrites on each tick.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonHeartbeat {
    /// OS pid of the daemon process that wrote this record.
    pub pid: u32,
    /// Epoch ms at which the daemon process started.
    pub started_at: i64,
    /// Epoch ms at which this record was last rewritten (the liveness clock).
    pub last_heartbeat_at: i64,
    /// Epoch ms of the last real unit of work (a relayed message, a scan that
    /// acted). `None` until the daemon has done anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_activity_at: Option<i64>,
    /// Daemon-specific connection health: bridge = channel online, fleet = peer
    /// registered. `false` until proven connected.
    #[serde(default)]
    pub connected: bool,
    /// Optional human label for the connection (e.g. `"Telegram (@mybot)"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    /// The most recent error string this run, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Monotonic count of errors observed since this daemon started.
    #[serde(default)]
    pub error_count: u64,
}

impl DaemonHeartbeat {
    /// A fresh heartbeat for the current process at startup: `started_at` and
    /// `last_heartbeat_at` both `now`, nothing relayed yet, not connected.
    #[must_use]
    pub fn starting() -> Self {
        let now = now_ms();
        Self {
            pid: std::process::id(),
            started_at: now,
            last_heartbeat_at: now,
            last_activity_at: None,
            connected: false,
            channel: None,
            last_error: None,
            error_count: 0,
        }
    }

    /// Refresh the liveness clock to `now`, preserving every other field. Called
    /// on each heartbeat tick when nothing else changed.
    pub fn touch(&mut self) {
        self.last_heartbeat_at = now_ms();
    }

    /// Mark a real unit of work: bumps both `last_activity_at` and the liveness
    /// clock to `now`.
    pub fn record_activity(&mut self) {
        let now = now_ms();
        self.last_activity_at = Some(now);
        self.last_heartbeat_at = now;
    }

    /// Mark the channel/peer connection state, refreshing the liveness clock and
    /// (optionally) labelling the channel.
    pub fn set_connected(&mut self, connected: bool, channel: Option<String>) {
        self.connected = connected;
        if channel.is_some() {
            self.channel = channel;
        }
        self.last_heartbeat_at = now_ms();
    }

    /// Record an error: increments the counter, stores the message, and refreshes
    /// the liveness clock (the daemon is still alive, just unhappy).
    pub fn record_error(&mut self, message: impl Into<String>) {
        self.error_count = self.error_count.saturating_add(1);
        self.last_error = Some(message.into());
        self.last_heartbeat_at = now_ms();
    }

    /// Atomically write this record to `<name>`'s heartbeat file under `home`.
    pub fn write_in(&self, home: &Path, name: &str) -> Result<()> {
        write_atomic_json(&heartbeat_path_in(home, name), self)
    }

    /// Atomically write this record to `<name>`'s heartbeat file under the
    /// default ainb home. Best-effort callers (daemons) should log-and-continue
    /// on error — a failed heartbeat must never crash the daemon.
    pub fn write(&self, name: &str) -> Result<()> {
        self.write_in(&crate::fleet::plumbing::paths::ainb_home()?, name)
    }

    /// Read `<name>`'s heartbeat file under `home`, if present and parseable.
    #[must_use]
    pub fn read_in(home: &Path, name: &str) -> Option<Self> {
        let text = std::fs::read_to_string(heartbeat_path_in(home, name)).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Read `<name>`'s heartbeat file under the default ainb home.
    #[must_use]
    pub fn read(name: &str) -> Option<Self> {
        let home = crate::fleet::plumbing::paths::ainb_home().ok()?;
        Self::read_in(&home, name)
    }
}

/// Best-effort liveness check: is `pid` still backing a running process? Uses
/// `kill(pid, 0)`, which succeeds when the process exists (and we may signal
/// it) and returns `ESRCH` otherwise. Mirrors `ainb-plugin-notifyd::pid`.
///
/// LIVENESS IS NOT IDENTITY. The OS recycles pids, so a `true` here only means
/// *some* process owns `pid` right now — not that it is the daemon that wrote
/// the heartbeat. Use [`pid_identity`] to distinguish the original process from
/// a recycled-pid impostor.
#[must_use]
pub fn is_pid_alive(pid: u32) -> bool {
    use nix::sys::signal::kill;
    use nix::unistd::Pid;
    matches!(kill(Pid::from_raw(pid as i32), None), Ok(()))
}

/// Tolerance (ms) for matching a heartbeat's `started_at` against the live
/// process's OS-reported start time. The heartbeat clock (`chrono` epoch ms,
/// stamped *inside* the process a moment after `fork`/`exec`) and the kernel's
/// start time (whole seconds, stamped at `fork`) are recorded by different
/// clocks at slightly different instants, so an exact match is impossible. 5s
/// comfortably covers that skew while staying far tighter than any realistic
/// pid-recycle gap (a recycled pid's new process started seconds-to-hours after
/// the dead daemon, never within 5s of the dead daemon's recorded start).
pub const PID_IDENTITY_TOLERANCE_MS: i64 = 5_000;

/// The verdict of cross-checking a heartbeat's pid against the live process
/// identity. The whole point of the H1 fix: liveness alone must never equal
/// "running".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PidCheck {
    /// A live process owns `pid` and its start time matches the heartbeat's
    /// `started_at` — this is (overwhelmingly likely) the original daemon.
    Matched,
    /// A live process owns `pid` but its start time does NOT match — the pid was
    /// recycled after the daemon died. The heartbeat is a tombstone.
    Recycled,
    /// No live process owns `pid`. The daemon is gone.
    Dead,
}

/// Read the live process's start time as epoch milliseconds, if `pid` is backed
/// by a process we can inspect. `sysinfo` reports whole-second granularity on
/// both macOS (`kinfo_proc`) and Linux (`/proc/<pid>/stat`), so this is the
/// second floored to ms. `None` means the process is gone or unreadable.
#[must_use]
pub fn process_start_ms(pid: u32) -> Option<i64> {
    use sysinfo::{Pid, ProcessesToUpdate, System};
    let mut sys = System::new();
    let spid = Pid::from_u32(pid);
    // Refresh ONLY this pid (no full process-table sweep) and don't mutate a
    // shared map — `System::new()` starts empty.
    sys.refresh_processes(ProcessesToUpdate::Some(&[spid]), false);
    let secs = sys.process(spid)?.start_time();
    i64::try_from(secs).ok().map(|s| s * 1000)
}

/// Cross-check a heartbeat's `pid`/`started_at` against the live process,
/// returning whether it's the original daemon, a recycled-pid impostor, or
/// dead. This is the identity guard the bare `kill(pid,0)` liveness check
/// lacked.
#[must_use]
pub fn pid_identity(pid: u32, started_at_ms: i64) -> PidCheck {
    classify_pid_identity(started_at_ms, process_start_ms(pid))
}

/// Pure core of [`pid_identity`]: given the heartbeat's recorded `started_at`
/// and the live process's start time (`None` when no process owns the pid),
/// decide the verdict. Separated so it is exhaustively unit-testable without a
/// real process.
#[must_use]
pub fn classify_pid_identity(started_at_ms: i64, live_start_ms: Option<i64>) -> PidCheck {
    match live_start_ms {
        None => PidCheck::Dead,
        Some(live) => {
            if (live - started_at_ms).abs() <= PID_IDENTITY_TOLERANCE_MS {
                PidCheck::Matched
            } else {
                PidCheck::Recycled
            }
        }
    }
}

/// Neutralise a daemon name for use as a filename: keep alphanumerics, `-`, `_`;
/// everything else becomes `_`. Daemon names are fixed literals in practice, so
/// this is a no-op guard against a path-traversal name.
fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn starting_stamps_pid_and_clocks() {
        let hb = DaemonHeartbeat::starting();
        assert_eq!(hb.pid, std::process::id());
        assert_eq!(hb.started_at, hb.last_heartbeat_at);
        assert!(hb.last_activity_at.is_none());
        assert!(!hb.connected);
        assert_eq!(hb.error_count, 0);
    }

    #[test]
    fn touch_advances_only_the_liveness_clock() {
        let mut hb = DaemonHeartbeat::starting();
        let started = hb.started_at;
        let before = hb.last_heartbeat_at;
        hb.last_heartbeat_at = before - 10_000; // pretend time passed
        hb.touch();
        assert!(hb.last_heartbeat_at >= before - 10_000);
        // started_at never moves on a touch.
        assert_eq!(hb.started_at, started);
        assert!(hb.last_activity_at.is_none());
    }

    #[test]
    fn record_activity_sets_activity_and_liveness() {
        let mut hb = DaemonHeartbeat::starting();
        hb.last_heartbeat_at = 0;
        hb.record_activity();
        assert!(hb.last_activity_at.is_some());
        assert_eq!(hb.last_activity_at, Some(hb.last_heartbeat_at));
    }

    #[test]
    fn set_connected_labels_channel_and_keeps_it() {
        let mut hb = DaemonHeartbeat::starting();
        hb.set_connected(true, Some("Telegram (@bot)".into()));
        assert!(hb.connected);
        assert_eq!(hb.channel.as_deref(), Some("Telegram (@bot)"));
        // A later connected=false with no label must not wipe the label.
        hb.set_connected(false, None);
        assert!(!hb.connected);
        assert_eq!(hb.channel.as_deref(), Some("Telegram (@bot)"));
    }

    #[test]
    fn record_error_increments_and_stores() {
        let mut hb = DaemonHeartbeat::starting();
        hb.record_error("boom");
        hb.record_error("bang");
        assert_eq!(hb.error_count, 2);
        assert_eq!(hb.last_error.as_deref(), Some("bang"));
    }

    #[test]
    fn write_then_read_round_trips() {
        let home = TempDir::new().unwrap();
        let mut hb = DaemonHeartbeat::starting();
        hb.set_connected(true, Some("Telegram".into()));
        hb.record_activity();
        hb.write_in(home.path(), "bridge").unwrap();
        let back = DaemonHeartbeat::read_in(home.path(), "bridge").unwrap();
        assert_eq!(back, hb);
    }

    #[test]
    fn missing_heartbeat_reads_none() {
        let home = TempDir::new().unwrap();
        assert!(DaemonHeartbeat::read_in(home.path(), "nope").is_none());
    }

    #[test]
    fn corrupt_heartbeat_reads_none_not_panic() {
        let home = TempDir::new().unwrap();
        std::fs::create_dir_all(daemons_dir_in(home.path())).unwrap();
        std::fs::write(heartbeat_path_in(home.path(), "bridge"), b"{not json").unwrap();
        assert!(DaemonHeartbeat::read_in(home.path(), "bridge").is_none());
    }

    #[test]
    fn path_honours_explicit_home() {
        let home = Path::new("/tmp/x/.agents-in-a-box");
        assert_eq!(
            heartbeat_path_in(home, "bridge"),
            home.join("daemons").join("bridge.json")
        );
    }

    #[test]
    fn sanitize_blocks_path_traversal() {
        let home = TempDir::new().unwrap();
        let p = heartbeat_path_in(home.path(), "../../etc/evil");
        assert!(p.starts_with(daemons_dir_in(home.path())));
        assert!(!p.to_string_lossy().contains(".."));
    }

    #[test]
    fn is_pid_alive_true_for_self_false_for_impossible() {
        assert!(is_pid_alive(std::process::id()));
        assert!(!is_pid_alive(0x7fff_ffff));
    }

    #[test]
    fn classify_pid_identity_dead_when_no_live_process() {
        // No process owns the pid → Dead regardless of recorded start.
        assert_eq!(classify_pid_identity(1_000_000, None), PidCheck::Dead);
    }

    #[test]
    fn classify_pid_identity_matches_within_tolerance() {
        let started = 1_700_000_000_000;
        // Live start within the tolerance window (sub-second skew between the
        // chrono stamp and the kernel's whole-second start time).
        assert_eq!(
            classify_pid_identity(started, Some(started + 2_000)),
            PidCheck::Matched
        );
        assert_eq!(
            classify_pid_identity(started, Some(started - PID_IDENTITY_TOLERANCE_MS)),
            PidCheck::Matched
        );
    }

    #[test]
    fn classify_pid_identity_recycled_when_live_start_far_off() {
        // H1 core: the pid is alive but the live process started long after the
        // heartbeat's daemon did → a recycled pid, NOT our daemon.
        let started = 1_700_000_000_000;
        assert_eq!(
            classify_pid_identity(started, Some(started + 3_600_000)),
            PidCheck::Recycled
        );
        // Just past the tolerance edge is already Recycled (no false Matched).
        assert_eq!(
            classify_pid_identity(started, Some(started + PID_IDENTITY_TOLERANCE_MS + 1)),
            PidCheck::Recycled
        );
    }

    #[test]
    fn process_start_ms_reads_self_and_is_in_the_past() {
        // The live identity probe must read SOMETHING for our own pid, and that
        // start time must be at or before now (a process can't start in the
        // future). This is what makes the recycled-pid cross-check possible.
        let start = process_start_ms(std::process::id()).expect("self start readable");
        assert!(start > 0);
        assert!(
            start <= now_ms() + PID_IDENTITY_TOLERANCE_MS,
            "self start {start} must not be in the future (now {})",
            now_ms()
        );
    }

    #[test]
    fn pid_identity_recycled_for_self_pid_with_bogus_started_at() {
        // End-to-end identity check over a real live pid (ourself) whose recorded
        // started_at is an hour off the real OS start: alive, but NOT a match →
        // Recycled. This is exactly the H1 dead-daemon-pid-reuse signature.
        let bogus_started = now_ms() - 3_600_000;
        assert_eq!(
            pid_identity(std::process::id(), bogus_started),
            PidCheck::Recycled
        );
    }

    #[test]
    fn pid_identity_dead_for_impossible_pid() {
        assert_eq!(pid_identity(0x7fff_ffff, now_ms()), PidCheck::Dead);
    }
}
