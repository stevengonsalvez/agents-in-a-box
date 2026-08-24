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
//     "error_count": 0,                   // monotonic error counter for this run
//     "last_attention_poll_at": 170000004000, // epoch ms of last OK attention poll
//     "last_attention_error": "connect …",    // optional last attention-source error
//     "last_delivery_error": "outbound push…",// optional undelivered proactive push
//     "inbound_expected": 1,                  // chat channels this daemon started
//     "inbound_live": 0,                      // how many are still running
//     "last_inbound_error": "Telegram …"      // why the last one stopped
//   }
//
// The two `attention` fields are the OUTBOUND liveness signal: a bridge that is
// connected to its chat gateway can still be completely unable to reach the
// daemon's attention inbox, in which case it pushes nothing to the phone while
// looking perfectly healthy. Only the worker that actually polls the attention
// source can prove otherwise, so it stamps these fields and `super::probe`
// degrades the row when they say the poll is not happening.
//
// The three `inbound_*` fields are the INBOUND liveness signal, and they exist
// for the mirror-image reason. `connected` is set ONCE per chat channel at its
// handshake and is never reset when that channel's task dies, while the idle
// ticker and the outbound poll both keep `last_heartbeat_at` fresh forever. So a
// bridge whose Telegram/Slack/Discord task has exited kept reporting
// "running + connected": the human could still be pushed asks and had no way to
// answer any of them. Counting the channels that were STARTED against the ones
// still RUNNING is the only thing that can tell those apart, and it is kept
// deliberately separate from the outbound fields so the health reason can name
// WHICH half is broken instead of collapsing both into one verdict.
//
// `last_delivery_error` covers the OTHER half of the same blind spot. Reaching
// the attention source proves only that the bridge can READ the ask; the send to
// Discord/Slack/Telegram can still fail (429, revoked token, DMs disabled) and
// the human hears nothing. That failure is invisible in `last_error` alone
// (`super::probe` never reads it), so it gets its own sticky field, cleared the
// moment a poll finds nothing left undelivered.
//
// Heartbeats that already have a richer signal elsewhere (ATC's
// `heartbeat-state.json`, notifyd's PID file + socket + DB) are READ in
// `super::probe` instead of re-emitted — only daemons with no existing
// observability write this file.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
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
    /// Ainb release which started this daemon.  This is written once at
    /// process start, rather than inferred from a mutable launcher symlink, so
    /// an upgrade can reliably identify an old long-running process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ainb_version: Option<String>,
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
    /// Epoch ms of the last SUCCESSFUL poll of the attention source (the hangar
    /// daemon's open attention inbox). `None` means the outbound worker has
    /// never completed a poll this run: either it is not running at all, or
    /// every attempt has failed. Forward/backward compatible: an older
    /// `bridge.json` without the key reads as `None`, which is exactly the
    /// "never polled" state such a daemon is in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_attention_poll_at: Option<i64>,
    /// The most recent attention-source failure this run (scrubbed), e.g. the
    /// socket dial error. Carried into the health reason so the operator sees
    /// WHAT is unreachable, not just that something is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_attention_error: Option<String>,
    /// The most recent PROACTIVE PUSH that did not reach the human (scrubbed):
    /// the channel send failed, so an open ask is sitting undelivered. Sticky
    /// until a poll finds nothing left undelivered, and read by `super::probe`
    /// so the row degrades. Distinct from [`Self::last_attention_error`]: that
    /// one means the bridge could not READ the ask, this one means it could not
    /// DELIVER it, and the operator fix is different for each.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_delivery_error: Option<String>,
    /// How many INBOUND chat channels this daemon STARTED (bridge only).
    ///
    /// `0` means the daemon makes no inbound claim at all: every non-bridge
    /// daemon, and any `bridge.json` written before these fields existed. The
    /// probe treats that as "nothing to judge", so an older record can never be
    /// mistaken for a bridge whose channels all died.
    #[serde(default)]
    pub inbound_expected: u32,
    /// How many of those channels are STILL running. Decremented the moment a
    /// channel task returns, which is the only proof that the phone can no
    /// longer reach the fleet through it.
    #[serde(default)]
    pub inbound_live: u32,
    /// Why the last inbound channel stopped (scrubbed). Carried into the health
    /// reason so the operator sees WHICH channel died and why, not just that the
    /// count dropped. Distinct from [`Self::last_attention_error`] and
    /// [`Self::last_delivery_error`]: those describe the outbound half, and the
    /// operator fix for each of the three is different.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_inbound_error: Option<String>,
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
            ainb_version: Some(env!("CARGO_PKG_VERSION").to_string()),
            last_activity_at: None,
            connected: false,
            channel: None,
            last_error: None,
            error_count: 0,
            last_attention_poll_at: None,
            last_attention_error: None,
            last_delivery_error: None,
            inbound_expected: 0,
            inbound_live: 0,
            last_inbound_error: None,
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
    ///
    /// SECURITY (H-D1): the message is scrubbed of known secret shapes (bot
    /// tokens, Slack/Discord tokens — see [`crate::fleet::bridge::redact::scrub`])
    /// BEFORE it is stored. A `reqwest::Error`'s `Display` embeds the request URL,
    /// and the Telegram Bot API carries the bot token in that URL path, so a raw
    /// error string can leak a token into the on-disk `last_error` (which the CLI
    /// and TUI render). Scrubbing here makes EVERY daemon's error sink safe by
    /// default — the bridge's own pre-scrub then becomes a harmless double-scrub.
    pub fn record_error(&mut self, message: impl Into<String>) {
        self.error_count = self.error_count.saturating_add(1);
        self.last_error = Some(crate::fleet::bridge::redact::scrub(&message.into()));
        self.last_heartbeat_at = now_ms();
    }

    /// Record a SUCCESSFUL poll of the attention source: stamps
    /// `last_attention_poll_at`, clears the sticky attention error, and
    /// refreshes the liveness clock. This is the only thing that proves the
    /// outbound half of the bridge can reach the daemon inbox.
    pub fn record_attention_poll(&mut self) {
        let now = now_ms();
        self.last_attention_poll_at = Some(now);
        self.last_attention_error = None;
        self.last_heartbeat_at = now;
    }

    /// Record a FAILED attempt to reach the attention source. Unlike
    /// [`Self::record_error`] this does NOT clear on the next tick: it stays
    /// until a poll actually succeeds, so the health reason can name the cause.
    /// Counts into `error_count` too: an outbound worker that cannot reach the
    /// daemon is an error the operator must see, not a debug-level shrug.
    ///
    /// The message is scrubbed exactly like [`Self::record_error`]: it is
    /// persisted and rendered, so a token-bearing diagnostic must never reach it.
    pub fn record_attention_error(&mut self, message: impl Into<String>) {
        let scrubbed = crate::fleet::bridge::redact::scrub(&message.into());
        self.error_count = self.error_count.saturating_add(1);
        self.last_attention_error = Some(scrubbed.clone());
        self.last_error = Some(scrubbed);
        self.last_heartbeat_at = now_ms();
    }

    /// Record a proactive push that did NOT reach the human. Sticky like
    /// [`Self::record_attention_error`] and for the same reason: the operator
    /// must be able to see that an ask is sitting undelivered, and the bridge
    /// must not render as healthy while it is.
    ///
    /// Scrubbed on the way in, because a channel send error embeds the request
    /// URL and this string is persisted and rendered.
    pub fn record_delivery_error(&mut self, message: impl Into<String>) {
        let scrubbed = crate::fleet::bridge::redact::scrub(&message.into());
        self.error_count = self.error_count.saturating_add(1);
        self.last_delivery_error = Some(scrubbed.clone());
        self.last_error = Some(scrubbed);
        self.last_heartbeat_at = now_ms();
    }

    /// Clear the sticky delivery verdict: nothing is outstanding any more,
    /// because every open phone-routed ask has been delivered (or was answered
    /// and closed). This is what lets a bridge recover from a transient channel
    /// outage without a restart.
    pub fn clear_delivery_error(&mut self) {
        self.last_delivery_error = None;
        self.last_heartbeat_at = now_ms();
    }

    /// Declare how many INBOUND chat channels this daemon started, and mark all
    /// of them live. Called once, before the channel tasks are spawned: from
    /// that moment the record carries a claim the daemon can be held to, and
    /// [`Self::record_inbound_exit`] is the only thing that walks it back.
    pub fn set_inbound_expected(&mut self, channels: u32) {
        self.inbound_expected = channels;
        self.inbound_live = channels;
        self.last_heartbeat_at = now_ms();
    }

    /// Record that one inbound chat channel has STOPPED: decrement the live
    /// count, store why, and count it as an error.
    ///
    /// A channel task that returns is terminal (nothing restarts it), so this is
    /// a one-way move. Scrubbed on the way in like every other persisted
    /// diagnostic, because a channel's exit error can embed its request URL.
    pub fn record_inbound_exit(&mut self, message: impl Into<String>) {
        let scrubbed = crate::fleet::bridge::redact::scrub(&message.into());
        self.inbound_live = self.inbound_live.saturating_sub(1);
        self.error_count = self.error_count.saturating_add(1);
        self.last_inbound_error = Some(scrubbed.clone());
        self.last_error = Some(scrubbed);
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

    /// Delete `<name>`'s heartbeat under `home`, marking a CLEAN stop.
    ///
    /// The absence of a heartbeat is what distinguishes "stopped on purpose"
    /// from "crashed": nothing used to remove these files, so a record outlived
    /// its process forever and every deliberately-stopped daemon reported
    /// `stale heartbeat, pid not alive (crashed)`.
    ///
    /// A stop that never runs (SIGKILL, power loss) leaves the record behind
    /// and therefore still reads as a crash. That is correct: from the outside
    /// a killed daemon and a crashed one are the same event.
    ///
    /// Missing file is success, so a double stop is not an error.
    pub fn clear_in(home: &Path, name: &str) -> Result<()> {
        let path = heartbeat_path_in(home, name);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| format!("clearing heartbeat {}", path.display())),
        }
    }

    /// Delete `<name>`'s heartbeat under the default ainb home.
    pub fn clear(name: &str) -> Result<()> {
        Self::clear_in(&crate::fleet::plumbing::paths::ainb_home()?, name)
    }

    /// Read `<name>`'s heartbeat under `home`, if present and parseable.
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

/// Best-effort resolved executable of `pid`. Kept beside the identity probe so
/// daemon health can inspect a process without shelling out from the TUI.
#[must_use]
pub fn process_binary(pid: u32) -> Option<PathBuf> {
    use sysinfo::{Pid, ProcessesToUpdate, System};
    let mut sys = System::new();
    let spid = Pid::from_u32(pid);
    sys.refresh_processes(ProcessesToUpdate::Some(&[spid]), false);
    sys.process(spid)?.exe().map(Path::to_path_buf)
}

/// Cross-check a heartbeat's `pid`/`started_at` against the live process,
/// returning whether it's the original daemon, a recycled-pid impostor, or
/// dead. This is the identity guard the bare `kill(pid,0)` liveness check
/// lacked.
#[must_use]
/// Delete heartbeats whose process is provably gone, returning the names swept.
///
/// `clear_in` only helps daemons stopped AFTER it ships. Records orphaned
/// before that (the stopping process is long gone, so nothing will ever come
/// back for them) would report `crashed` forever. This sweep collects them.
///
/// ONLY `Dead` and `Recycled` are swept. `Matched` means the process is alive
/// and owns the record; `Recycled` means the pid was reused by an unrelated
/// process, so the record is stale in a different way but equally orphaned.
/// A running daemon's heartbeat is never touched.
///
/// Best-effort per entry: one unreadable file must not abort the sweep.
pub fn sweep_orphaned_in(home: &Path) -> Vec<String> {
    let dir = daemons_dir_in(home);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut swept = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Some(name) = path.file_stem().and_then(|s| s.to_str()).map(str::to_string) else {
            continue;
        };
        let Some(hb) = DaemonHeartbeat::read_in(home, &name) else {
            continue;
        };
        if matches!(
            pid_identity(hb.pid, hb.started_at),
            PidCheck::Dead | PidCheck::Recycled
        ) && DaemonHeartbeat::clear_in(home, &name).is_ok()
        {
            swept.push(name);
        }
    }
    swept.sort();
    swept
}

/// Sweep orphaned heartbeats under the default ainb home.
pub fn sweep_orphaned() -> Vec<String> {
    crate::fleet::plumbing::paths::ainb_home()
        .map(|home| sweep_orphaned_in(&home))
        .unwrap_or_default()
}

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
        assert_eq!(hb.ainb_version.as_deref(), Some(env!("CARGO_PKG_VERSION")));
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
    fn record_error_scrubs_secrets_before_storing() {
        // H-D1: a token-bearing error must never reach `last_error` verbatim.
        let mut hb = DaemonHeartbeat::starting();
        hb.record_error(
            "error sending request for url \
             (https://api.telegram.org/bot123456789:ABC-DEF_ghiJKLmnopqrstuvwxyz012345/getUpdates)",
        );
        let stored = hb.last_error.as_deref().unwrap();
        assert!(
            !stored.contains("ABC-DEF_ghiJKLmnopqrstuvwxyz012345"),
            "token body leaked into last_error: {stored}"
        );
        assert!(
            !stored.contains("bot123456789:"),
            "token prefix leaked into last_error: {stored}"
        );
        assert!(
            stored.contains("<redacted>"),
            "expected redaction: {stored}"
        );
    }

    #[test]
    fn record_error_persists_redacted_last_error_on_disk() {
        // H-D1 end-to-end: the on-disk heartbeat file (rendered by CLI/TUI) must
        // carry a redacted last_error, not the raw token.
        let home = TempDir::new().unwrap();
        let mut hb = DaemonHeartbeat::starting();
        hb.record_error("getUpdates: 123456789:ABCdefGHIjklMNOpqrstuvwx failed");
        hb.write_in(home.path(), "bridge").unwrap();
        let raw = std::fs::read_to_string(heartbeat_path_in(home.path(), "bridge")).unwrap();
        assert!(
            !raw.contains("ABCdefGHIjklMNOpqrstuvwx"),
            "token leaked to disk: {raw}"
        );
        assert!(
            raw.contains("<redacted>"),
            "on-disk last_error not redacted: {raw}"
        );
    }

    #[test]
    fn starting_has_never_polled_the_attention_source() {
        let hb = DaemonHeartbeat::starting();
        assert!(hb.last_attention_poll_at.is_none());
        assert!(hb.last_attention_error.is_none());
    }

    #[test]
    fn record_attention_poll_stamps_and_clears_the_error() {
        let mut hb = DaemonHeartbeat::starting();
        hb.record_attention_error("connect /tmp/hangar.sock: refused");
        assert!(hb.last_attention_error.is_some());
        hb.record_attention_poll();
        assert!(hb.last_attention_poll_at.is_some());
        assert_eq!(
            hb.last_attention_error, None,
            "a successful poll clears the sticky attention error"
        );
        assert_eq!(hb.last_attention_poll_at, Some(hb.last_heartbeat_at));
    }

    #[test]
    fn record_attention_error_counts_and_sticks() {
        let mut hb = DaemonHeartbeat::starting();
        hb.record_attention_error("connect /tmp/hangar.sock: refused");
        hb.record_attention_error("daemon timed out after 5s");
        assert_eq!(
            hb.error_count, 2,
            "an unreachable attention source must count as an error"
        );
        assert_eq!(
            hb.last_attention_error.as_deref(),
            Some("daemon timed out after 5s")
        );
        assert_eq!(hb.last_error.as_deref(), Some("daemon timed out after 5s"));
        assert!(hb.last_attention_poll_at.is_none());
    }

    #[test]
    fn record_delivery_error_counts_sticks_and_clears() {
        let mut hb = DaemonHeartbeat::starting();
        assert!(hb.last_delivery_error.is_none());

        hb.record_delivery_error("outbound push: Discord HTTP 429");
        assert_eq!(hb.error_count, 1);
        assert_eq!(
            hb.last_delivery_error.as_deref(),
            Some("outbound push: Discord HTTP 429")
        );
        assert_eq!(
            hb.last_error.as_deref(),
            Some("outbound push: Discord HTTP 429")
        );

        // A successful ATTENTION poll must not clear it: reaching the daemon
        // says nothing about whether the human got the message. Conflating the
        // two is the whole defect.
        hb.record_attention_poll();
        assert!(
            hb.last_delivery_error.is_some(),
            "a healthy poll must not erase an undelivered push"
        );

        hb.clear_delivery_error();
        assert!(hb.last_delivery_error.is_none());
    }

    #[test]
    fn record_delivery_error_scrubs_secrets() {
        let mut hb = DaemonHeartbeat::starting();
        hb.record_delivery_error(
            "outbound push: error sending request for url \
             (https://api.telegram.org/bot123456789:ABC-DEF_ghiJKLmnopqrstuvwxyz012345/sendMessage)",
        );
        let stored = hb.last_delivery_error.as_deref().unwrap();
        assert!(
            !stored.contains("ABC-DEF_ghiJKLmnopqrstuvwxyz012345"),
            "token leaked into last_delivery_error: {stored}"
        );
        assert!(
            stored.contains("<redacted>"),
            "expected redaction: {stored}"
        );
    }

    #[test]
    fn record_attention_error_scrubs_secrets() {
        let mut hb = DaemonHeartbeat::starting();
        hb.record_attention_error(
            "poll failed for https://api.telegram.org/bot123456789:ABC-DEF_ghiJKLmnopqrstuvwxyz012345/x",
        );
        let stored = hb.last_attention_error.as_deref().unwrap();
        assert!(
            !stored.contains("ABC-DEF_ghiJKLmnopqrstuvwxyz012345"),
            "token leaked into last_attention_error: {stored}"
        );
        assert!(
            stored.contains("<redacted>"),
            "expected redaction: {stored}"
        );
    }

    #[test]
    fn inbound_exit_decrements_the_live_count_and_says_why() {
        let mut hb = DaemonHeartbeat::starting();
        // Nothing declared yet: no inbound claim to hold the daemon to.
        assert_eq!(hb.inbound_expected, 0);
        assert_eq!(hb.inbound_live, 0);

        hb.set_inbound_expected(2);
        assert_eq!((hb.inbound_expected, hb.inbound_live), (2, 2));
        assert!(hb.last_inbound_error.is_none());

        hb.record_inbound_exit("Telegram channel stopped: building HTTP client failed");
        assert_eq!(
            (hb.inbound_expected, hb.inbound_live),
            (2, 1),
            "one channel died, the other is still relaying"
        );
        assert_eq!(hb.error_count, 1);
        assert!(hb.last_inbound_error.as_deref().unwrap().contains("Telegram"));

        hb.record_inbound_exit("Slack channel stopped: socket closed");
        assert_eq!(hb.inbound_live, 0, "no channel is left to hear the phone");
        assert_eq!(hb.error_count, 2);

        // A stopped channel is never restarted, so the count can only fall, and
        // must never wrap below zero.
        hb.record_inbound_exit("Slack channel stopped again (impossible)");
        assert_eq!(hb.inbound_live, 0);
    }

    #[test]
    fn inbound_exit_does_not_touch_the_outbound_verdicts() {
        // The two halves must stay separately observable: a dead chat channel
        // says nothing about whether the outbound push worker is fine.
        let mut hb = DaemonHeartbeat::starting();
        hb.set_inbound_expected(1);
        hb.record_attention_poll();
        hb.record_inbound_exit("Discord channel stopped: gateway closed");
        assert!(
            hb.last_attention_poll_at.is_some(),
            "the outbound poll stamp survives an inbound death"
        );
        assert!(hb.last_attention_error.is_none());
        assert!(hb.last_delivery_error.is_none());
        assert!(hb.last_inbound_error.is_some());
    }

    #[test]
    fn record_inbound_exit_scrubs_secrets() {
        let mut hb = DaemonHeartbeat::starting();
        hb.set_inbound_expected(1);
        hb.record_inbound_exit(
            "Telegram channel stopped: error sending request for url \
             (https://api.telegram.org/bot123456789:ABC-DEF_ghiJKLmnopqrstuvwxyz012345/getUpdates)",
        );
        let stored = hb.last_inbound_error.as_deref().unwrap();
        assert!(
            !stored.contains("ABC-DEF_ghiJKLmnopqrstuvwxyz012345"),
            "token leaked into last_inbound_error: {stored}"
        );
        assert!(
            stored.contains("<redacted>"),
            "expected redaction: {stored}"
        );
    }

    #[test]
    fn heartbeat_without_inbound_keys_makes_no_inbound_claim() {
        // Forward compatibility: a bridge.json written before the inbound
        // liveness fields existed must still parse, and must read as "no claim"
        // rather than as a bridge whose every channel just died.
        let home = TempDir::new().unwrap();
        std::fs::create_dir_all(daemons_dir_in(home.path())).unwrap();
        std::fs::write(
            heartbeat_path_in(home.path(), "bridge"),
            br#"{"pid":89585,"started_at":1786121049405,"last_heartbeat_at":1786125144422,
                "connected":true,"channel":"Discord (gateway)","error_count":0}"#,
        )
        .unwrap();
        let hb = DaemonHeartbeat::read_in(home.path(), "bridge").expect("legacy record parses");
        assert_eq!(hb.inbound_expected, 0);
        assert_eq!(hb.inbound_live, 0);
        assert!(hb.last_inbound_error.is_none());
    }

    #[test]
    fn heartbeat_without_attention_keys_reads_as_never_polled() {
        // Forward compatibility: a bridge.json written before the outbound
        // liveness fields existed must still parse, and must read as "never
        // polled", which is exactly what such a daemon was doing.
        let home = TempDir::new().unwrap();
        std::fs::create_dir_all(daemons_dir_in(home.path())).unwrap();
        std::fs::write(
            heartbeat_path_in(home.path(), "bridge"),
            br#"{"pid":89585,"started_at":1786121049405,"last_heartbeat_at":1786125144422,
                "connected":true,"channel":"Discord (gateway)","error_count":0}"#,
        )
        .unwrap();
        let hb = DaemonHeartbeat::read_in(home.path(), "bridge").expect("legacy record parses");
        assert!(hb.connected);
        assert!(hb.last_attention_poll_at.is_none());
        assert!(hb.last_attention_error.is_none());
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

    /// A CLEAN stop and a CRASH must stay distinguishable.
    ///
    /// This is the whole reason for clearing the record instead of hiding the
    /// row: if both cases looked alike, the fix would be row-hiding with extra
    /// steps. Clean stop removes the record so the daemon reads as stopped; a
    /// process that died without clearing leaves it, and still reads as dead.
    #[test]
    fn a_clean_stop_and_a_crash_do_not_look_alike() {
        let home = TempDir::new().unwrap();

        // Clean stop: the daemon cleared its own record on the way out.
        DaemonHeartbeat::starting().write_in(home.path(), "fleet-daemon").unwrap();
        DaemonHeartbeat::clear_in(home.path(), "fleet-daemon").unwrap();
        assert!(
            DaemonHeartbeat::read_in(home.path(), "fleet-daemon").is_none(),
            "a clean stop must leave no record, so the row reads stopped, not crashed"
        );

        // Crash (or SIGKILL): nothing ran on the way out, so the record stays
        // and its pid is dead. pid 1 is alive but did not start when this
        // record claims, so identity does not match: exactly the crash shape.
        let mut crashed = DaemonHeartbeat::starting();
        crashed.pid = 1;
        crashed.started_at = 0;
        crashed.write_in(home.path(), "notifyd").unwrap();
        let back = DaemonHeartbeat::read_in(home.path(), "notifyd")
            .expect("a crash leaves the record behind");
        assert!(
            matches!(
                pid_identity(back.pid, back.started_at),
                PidCheck::Dead | PidCheck::Recycled
            ),
            "a crashed daemon's record must still classify as not-alive"
        );
    }

    /// The sweep collects records orphaned BEFORE the clear-on-stop shipped.
    ///
    /// Without it, an already-dead pid keeps reporting `crashed` forever: the
    /// process that would have cleaned up is long gone.
    #[test]
    fn the_sweep_takes_orphans_and_spares_the_living() {
        let home = TempDir::new().unwrap();

        // Orphan: dead pid identity, nobody left to clean it.
        let mut orphan = DaemonHeartbeat::starting();
        orphan.pid = 1;
        orphan.started_at = 0;
        orphan.write_in(home.path(), "fleet-daemon").unwrap();

        // Living: this very process, so identity matches.
        let mut alive = DaemonHeartbeat::starting();
        alive.pid = std::process::id();
        alive.started_at = process_start_ms(std::process::id()).unwrap_or(now_ms());
        alive.write_in(home.path(), "atc").unwrap();

        let swept = sweep_orphaned_in(home.path());
        assert!(
            swept.contains(&"fleet-daemon".to_string()),
            "the orphaned record must be swept, got {swept:?}"
        );
        assert!(
            DaemonHeartbeat::read_in(home.path(), "atc").is_some(),
            "a LIVE daemon's heartbeat must never be swept"
        );
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
