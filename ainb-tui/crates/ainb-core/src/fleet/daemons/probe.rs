// ABOUTME: The per-daemon probes + the [`collect`] aggregator the CLI and TUI share.
//
// Each probe turns one daemon's on-disk signals into a typed [`DaemonStatus`].
// Two probe shapes:
//
//   * HEARTBEAT-BACKED (bridge, fleet daemon): read the daemon's
//     `~/.agents-in-a-box/daemons/<name>.json` heartbeat and cross-check it.
//     A heartbeat whose `pid` is dead, or whose `last_heartbeat_at` is older
//     than [`STALE_AFTER_MS`], is reported `Stopped` with a "stale heartbeat"
//     reason — the crashed-daemon-shows-stale guarantee. No heartbeat file at
//     all → `Stopped` (clean: never started this session).
//
//   * NATIVE-SIGNAL (notifyd, ATC): read the signals the daemon already
//     maintains instead of a duplicate heartbeat. notifyd = PID-file liveness
//     + Unix socket present + sqlite DB present. ATC = its existing
//     `heartbeat-state.json` cadence across all provisioned instances.
//
// `STALE_AFTER_MS` is deliberately generous (90s) so a daemon that heartbeats
// every few seconds is never flagged stale by a slow tick, while a truly dead
// daemon (whose pid is also gone) is caught immediately by the pid cross-check
// regardless of the window.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::heartbeat::{DaemonHeartbeat, is_pid_alive};

/// A heartbeat older than this (and whose pid is *still* alive — e.g. a wedged
/// daemon that stopped ticking) is treated as stale. A dead pid is caught
/// immediately regardless of this window. 90s comfortably covers the bridge's
/// 30s long-poll cycle and the fleet daemon's 5s tick with headroom.
pub const STALE_AFTER_MS: i64 = 90_000;

/// Which daemon a [`DaemonStatus`] describes. Stable wire strings for the JSON
/// surface and stable display names for the text/TUI surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DaemonKind {
    /// Native Telegram/Slack phone bridge.
    Bridge,
    /// Unix-socket notification daemon (`ainb-notifyd`).
    Notifyd,
    /// Air Traffic Control fleet brain.
    Atc,
    /// Auto-continue API-error watcher (`ainb fleet daemon`).
    FleetDaemon,
}

impl DaemonKind {
    /// Stable lowercase id (heartbeat filename + JSON `kind`).
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::Bridge => "bridge",
            Self::Notifyd => "notifyd",
            Self::Atc => "atc",
            Self::FleetDaemon => "fleet-daemon",
        }
    }

    /// Human display name for the text/TUI surfaces.
    #[must_use]
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Bridge => "phone bridge",
            Self::Notifyd => "notifyd",
            Self::Atc => "ATC",
            Self::FleetDaemon => "fleet daemon",
        }
    }
}

/// Coarse runtime state of a daemon. The whole point of the feature: distinguish
/// a live-and-working daemon from a dead one, and a clean stop from a crash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DaemonState {
    /// Process alive and heartbeating (or native signals confirm it's serving).
    Running,
    /// Not running. The `reason` on [`DaemonStatus`] says whether it's a clean
    /// stop, a stale heartbeat from a crash, or never-configured.
    Stopped,
    /// We couldn't determine the state (e.g. an error reading a signal). Rare;
    /// surfaced rather than guessed so the operator knows the probe itself is
    /// the problem, not the daemon.
    Unknown,
}

impl DaemonState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Stopped => "stopped",
            Self::Unknown => "unknown",
        }
    }
}

/// One row of the Daemons view — the typed model both surfaces render.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonStatus {
    /// Which daemon this describes.
    pub kind: DaemonKind,
    /// Coarse runtime state.
    pub state: DaemonState,
    /// OS pid, when known (from a heartbeat or a PID file).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// Milliseconds since the daemon started, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uptime_ms: Option<i64>,
    /// Daemon-specific connection health (Telegram online / peer registered /
    /// socket+DB reachable / heartbeat alive).
    #[serde(default)]
    pub connected: bool,
    /// Human label for the connection (e.g. `"Telegram (@bot)"`, `"socket+db"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    /// Epoch ms of the daemon's last real activity, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_activity_at: Option<i64>,
    /// Count of errors observed this run.
    #[serde(default)]
    pub error_count: u64,
    /// The most recent error string, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// A short human explanation of the state — the load-bearing field for
    /// telling "clean stop" from "crashed (stale heartbeat)".
    pub reason: String,
}

impl DaemonStatus {
    fn stopped(kind: DaemonKind, reason: impl Into<String>) -> Self {
        Self {
            kind,
            state: DaemonState::Stopped,
            pid: None,
            uptime_ms: None,
            connected: false,
            channel: None,
            last_activity_at: None,
            error_count: 0,
            last_error: None,
            reason: reason.into(),
        }
    }
}

/// Decide a heartbeat-backed daemon's status from its heartbeat (if any),
/// cross-checking pid liveness and the staleness window against `now_ms`. Pure
/// over its inputs so it is exhaustively unit-testable without touching disk.
#[must_use]
pub fn classify_heartbeat(
    kind: DaemonKind,
    hb: Option<DaemonHeartbeat>,
    pid_alive: bool,
    now_ms: i64,
) -> DaemonStatus {
    let Some(hb) = hb else {
        return DaemonStatus::stopped(kind, "no heartbeat — not running this session");
    };

    let age = now_ms.saturating_sub(hb.last_heartbeat_at);
    let uptime = Some(now_ms.saturating_sub(hb.started_at));

    // A dead pid is the strongest possible signal: the writing process is gone,
    // so the heartbeat is a tombstone no matter how recent it looks.
    if !pid_alive {
        return DaemonStatus {
            kind,
            state: DaemonState::Stopped,
            pid: Some(hb.pid),
            uptime_ms: None,
            connected: false,
            channel: hb.channel,
            last_activity_at: hb.last_activity_at,
            error_count: hb.error_count,
            last_error: hb.last_error,
            reason: format!("stale heartbeat — pid {} not alive (crashed)", hb.pid),
        };
    }

    // The pid is alive but the heartbeat went quiet past the window: the process
    // exists but its loop is wedged or paused. Report stopped+stale so we don't
    // claim a healthy daemon.
    if age > STALE_AFTER_MS {
        return DaemonStatus {
            kind,
            state: DaemonState::Stopped,
            pid: Some(hb.pid),
            uptime_ms: uptime,
            connected: false,
            channel: hb.channel,
            last_activity_at: hb.last_activity_at,
            error_count: hb.error_count,
            last_error: hb.last_error,
            reason: format!("stale heartbeat — last beat {}s ago (wedged?)", age / 1000),
        };
    }

    DaemonStatus {
        kind,
        state: DaemonState::Running,
        pid: Some(hb.pid),
        uptime_ms: uptime,
        connected: hb.connected,
        channel: hb.channel,
        last_activity_at: hb.last_activity_at,
        error_count: hb.error_count,
        last_error: hb.last_error,
        reason: if hb.connected {
            "running + connected".to_string()
        } else {
            "running (connecting…)".to_string()
        },
    }
}

/// Probe a heartbeat-backed daemon (bridge / fleet daemon) under an explicit
/// ainb home. Reads the heartbeat, checks pid liveness, classifies.
#[must_use]
pub fn probe_heartbeat_daemon(home: &Path, kind: DaemonKind, now_ms: i64) -> DaemonStatus {
    let hb = DaemonHeartbeat::read_in(home, kind.id());
    let pid_alive = hb.as_ref().is_some_and(|h| is_pid_alive(h.pid));
    classify_heartbeat(kind, hb, pid_alive, now_ms)
}

/// Probe notifyd from the signals it already maintains: PID-file liveness, the
/// Unix socket, and the sqlite DB. `base` is notifyd's home (it does NOT honour
/// `$AINB_HOME` — it always uses `~/.agents-in-a-box` in production — so the
/// caller passes the resolved base, and tests pass a tempdir).
#[must_use]
pub fn probe_notifyd(base: &Path, now_ms: i64) -> DaemonStatus {
    let kind = DaemonKind::Notifyd;
    let pid_path = base.join("notify.pid");
    let socket_path = base.join("notify.sock");
    let db_path = base.join("notifications.db");

    let pid = read_pid_file(&pid_path);
    let pid_alive = pid.is_some_and(is_pid_alive);
    let socket_ok = socket_path.exists();
    let db_ok = db_path.exists();

    if !pid_alive {
        // No live pid. If a pid file lingers, the daemon crashed; otherwise it's
        // simply not running.
        let reason = match pid {
            Some(p) => format!("stale pid {p} — not running (crashed)"),
            None => "no pid file — not running".to_string(),
        };
        return DaemonStatus {
            kind,
            pid,
            ..DaemonStatus::stopped(kind, reason)
        };
    }

    // Pid is alive. Connection health = the socket is bound AND the DB exists.
    let connected = socket_ok && db_ok;
    let reason = if connected {
        "running + connected (socket bound, db reachable)".to_string()
    } else if !socket_ok {
        "running but socket not bound yet".to_string()
    } else {
        "running but db not reachable".to_string()
    };
    DaemonStatus {
        kind,
        state: DaemonState::Running,
        pid,
        uptime_ms: None, // notifyd's PID file carries no start time
        connected,
        channel: Some("unix socket + sqlite".to_string()),
        last_activity_at: db_mtime_ms(&db_path),
        error_count: 0,
        last_error: None,
        reason,
    }
    .with_now(now_ms)
}

/// Probe ATC from its existing per-instance `heartbeat-state.json` files. ATC is
/// timer-driven (no foreground daemon), so "running" here means: at least one
/// provisioned instance with `heartbeat_enabled` whose last heartbeat is within
/// the cadence window. Reads, never re-emits.
#[must_use]
pub fn probe_atc(home: &Path, now_ms: i64) -> DaemonStatus {
    use crate::fleet::atc::heartbeat::HeartbeatState;
    use crate::fleet::atc::meta::AtcMeta;
    use crate::fleet::atc::paths::{AtcPaths, list_instance_names_in};

    let kind = DaemonKind::Atc;
    let atc_root = home.join("atc");
    let names = list_instance_names_in(&atc_root);
    if names.is_empty() {
        return DaemonStatus::stopped(kind, "no ATC instance provisioned");
    }

    // Pick the most-recently-beating instance as the representative row, and
    // report it. (v1 surfaces ATC as one daemon; the instance name goes in the
    // channel label.)
    let mut best: Option<(String, HeartbeatState, u32)> = None;
    for name in &names {
        let p = AtcPaths::under_root(&atc_root, name);
        let interval_min = std::fs::read_to_string(&p.meta)
            .ok()
            .and_then(|s| AtcMeta::from_json(&s).ok())
            .map_or(15, |m| m.heartbeat_interval_min.max(1));
        let Some(hbs) = std::fs::read_to_string(&p.heartbeat_state)
            .ok()
            .map(|s| HeartbeatState::from_json_or_default(&s))
        else {
            continue;
        };
        let take = match &best {
            Some((_, prev, _)) => hbs.last_heartbeat_ms > prev.last_heartbeat_ms,
            None => true,
        };
        if take {
            best = Some((name.clone(), hbs, interval_min));
        }
    }

    let Some((name, hbs, interval_min)) = best else {
        return DaemonStatus::stopped(
            kind,
            format!(
                "{} instance(s) provisioned, none have beaten yet",
                names.len()
            ),
        );
    };

    if hbs.last_heartbeat_ms == 0 {
        return DaemonStatus {
            kind,
            channel: Some(name),
            ..DaemonStatus::stopped(kind, "instance provisioned but no heartbeat yet")
        };
    }

    // The OS timer fires every `interval_min`; allow 2x the cadence + a 90s grace
    // before we call it stale (timers are not punctual to the second).
    let window_ms = (interval_min as i64) * 60_000 * 2 + STALE_AFTER_MS;
    let age = now_ms.saturating_sub(hbs.last_heartbeat_ms);
    if age > window_ms {
        return DaemonStatus {
            kind,
            channel: Some(name),
            last_activity_at: hbs.last_active_ms,
            ..DaemonStatus::stopped(
                kind,
                format!(
                    "stale — last heartbeat {}m ago (timer stopped?)",
                    age / 60_000
                ),
            )
        };
    }

    DaemonStatus {
        kind,
        state: DaemonState::Running,
        pid: None, // timer-driven; no resident pid
        uptime_ms: None,
        connected: true,
        channel: Some(format!("{name} (every {interval_min}m)")),
        last_activity_at: hbs.last_active_ms,
        error_count: 0,
        last_error: None,
        reason: format!("heartbeat alive — last beat {}m ago", age / 60_000),
    }
}

impl DaemonStatus {
    /// No-op hook reserved for relative-time rendering; keeps `now_ms` threaded
    /// through the probe signature so callers always pass a single clock.
    fn with_now(self, _now_ms: i64) -> Self {
        self
    }
}

/// Read a `notify.pid`-style PID file: a single integer, trimmed.
fn read_pid_file(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// Best-effort last-write time of a file as epoch ms — used as notifyd's
/// last-activity proxy (the DB is rewritten on every persisted notification).
fn db_mtime_ms(path: &Path) -> Option<i64> {
    let mtime = std::fs::metadata(path).ok()?.modified().ok()?;
    let dur = mtime.duration_since(std::time::UNIX_EPOCH).ok()?;
    i64::try_from(dur.as_millis()).ok()
}

/// Aggregate all four daemons under an explicit ainb home + notifyd base. The
/// test seam — every path is injected so a test isolates to a tempdir.
///
/// `now_ms` is the single clock the staleness checks measure against.
#[must_use]
pub fn collect_in(ainb_home: &Path, notifyd_base: &Path, now_ms: i64) -> Vec<DaemonStatus> {
    vec![
        probe_heartbeat_daemon(ainb_home, DaemonKind::Bridge, now_ms),
        probe_notifyd(notifyd_base, now_ms),
        probe_atc(ainb_home, now_ms),
        probe_heartbeat_daemon(ainb_home, DaemonKind::FleetDaemon, now_ms),
    ]
}

/// Aggregate all four daemons from the real on-disk layout. Resolves the ainb
/// home (honouring `$AINB_HOME`) for bridge/fleet/ATC, and notifyd's own base
/// (`~/.agents-in-a-box`, which it does NOT key off `$AINB_HOME`).
pub fn collect() -> anyhow::Result<Vec<DaemonStatus>> {
    let ainb_home = crate::fleet::plumbing::paths::ainb_home()?;
    // notifyd resolves its base via `dirs::home_dir()/.agents-in-a-box` and does
    // NOT honour `$AINB_HOME`; mirror that exactly so the probe reads the same
    // files the daemon writes. When `$AINB_HOME` is set (tests / unusual setups)
    // we still point notifyd at the ainb home so an isolated run is coherent.
    let notifyd_base = if std::env::var("AINB_HOME").map(|h| !h.is_empty()).unwrap_or(false) {
        ainb_home.clone()
    } else {
        dirs::home_dir()
            .map(|h| h.join(".agents-in-a-box"))
            .unwrap_or_else(|| ainb_home.clone())
    };
    Ok(collect_in(
        &ainb_home,
        &notifyd_base,
        super::heartbeat::now_ms(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn hb(pid: u32, started: i64, last_beat: i64, connected: bool) -> DaemonHeartbeat {
        DaemonHeartbeat {
            pid,
            started_at: started,
            last_heartbeat_at: last_beat,
            last_activity_at: Some(last_beat),
            connected,
            channel: Some("Telegram".into()),
            last_error: None,
            error_count: 0,
        }
    }

    #[test]
    fn no_heartbeat_is_clean_stopped() {
        let s = classify_heartbeat(DaemonKind::Bridge, None, false, 1000);
        assert_eq!(s.state, DaemonState::Stopped);
        assert!(s.reason.contains("not running"));
        assert!(s.pid.is_none());
    }

    #[test]
    fn fresh_heartbeat_with_live_pid_is_running_connected() {
        let now = 1_000_000;
        let s = classify_heartbeat(
            DaemonKind::Bridge,
            Some(hb(42, now - 5000, now - 1000, true)),
            true,
            now,
        );
        assert_eq!(s.state, DaemonState::Running);
        assert!(s.connected);
        assert_eq!(s.pid, Some(42));
        assert_eq!(s.uptime_ms, Some(5000));
        assert_eq!(s.reason, "running + connected");
    }

    #[test]
    fn fresh_heartbeat_not_yet_connected_is_running_connecting() {
        let now = 1_000_000;
        let s = classify_heartbeat(
            DaemonKind::Bridge,
            Some(hb(42, now - 5000, now - 1000, false)),
            true,
            now,
        );
        assert_eq!(s.state, DaemonState::Running);
        assert!(!s.connected);
        assert!(s.reason.contains("connecting"));
    }

    #[test]
    fn dead_pid_is_stale_even_with_recent_beat() {
        // The crash signal: a very recent heartbeat but the writing pid is gone.
        let now = 1_000_000;
        let s = classify_heartbeat(
            DaemonKind::Bridge,
            Some(hb(42, now - 5000, now - 100, true)),
            false,
            now,
        );
        assert_eq!(s.state, DaemonState::Stopped);
        assert!(s.reason.contains("stale heartbeat"));
        assert!(s.reason.contains("crashed"));
        assert!(!s.connected, "a crashed daemon must not report connected");
    }

    #[test]
    fn wedged_daemon_live_pid_but_old_beat_is_stale() {
        let now = 1_000_000;
        let s = classify_heartbeat(
            DaemonKind::FleetDaemon,
            Some(hb(42, now - 200_000, now - (STALE_AFTER_MS + 5000), true)),
            true,
            now,
        );
        assert_eq!(s.state, DaemonState::Stopped);
        assert!(s.reason.contains("stale heartbeat"));
        assert!(s.reason.contains("wedged"));
    }

    #[test]
    fn beat_exactly_at_window_edge_is_still_running() {
        let now = 1_000_000;
        // age == STALE_AFTER_MS is NOT > the window, so still running.
        let s = classify_heartbeat(
            DaemonKind::Bridge,
            Some(hb(7, now - 100_000, now - STALE_AFTER_MS, true)),
            true,
            now,
        );
        assert_eq!(s.state, DaemonState::Running);
    }

    #[test]
    fn probe_heartbeat_daemon_reads_disk_and_classifies_stale_for_dead_pid() {
        let home = TempDir::new().unwrap();
        // Write a heartbeat for an impossible pid → dead → stale.
        let mut h = DaemonHeartbeat::starting();
        h.pid = 0x7fff_ffff;
        h.set_connected(true, Some("Telegram".into()));
        h.write_in(home.path(), DaemonKind::Bridge.id()).unwrap();
        let s = probe_heartbeat_daemon(
            home.path(),
            DaemonKind::Bridge,
            super::super::heartbeat::now_ms(),
        );
        assert_eq!(s.state, DaemonState::Stopped);
        assert!(s.reason.contains("crashed"));
    }

    #[test]
    fn probe_heartbeat_daemon_running_for_self_pid() {
        let home = TempDir::new().unwrap();
        let mut h = DaemonHeartbeat::starting(); // pid = self, alive
        h.set_connected(true, Some("Telegram".into()));
        h.write_in(home.path(), DaemonKind::Bridge.id()).unwrap();
        let s = probe_heartbeat_daemon(
            home.path(),
            DaemonKind::Bridge,
            super::super::heartbeat::now_ms(),
        );
        assert_eq!(s.state, DaemonState::Running);
        assert!(s.connected);
    }

    #[test]
    fn probe_notifyd_no_pid_is_stopped() {
        let base = TempDir::new().unwrap();
        let s = probe_notifyd(base.path(), 0);
        assert_eq!(s.state, DaemonState::Stopped);
        assert!(s.reason.contains("no pid file"));
    }

    #[test]
    fn probe_notifyd_stale_pid_is_stopped_crashed() {
        let base = TempDir::new().unwrap();
        std::fs::write(base.path().join("notify.pid"), "2147483647\n").unwrap();
        let s = probe_notifyd(base.path(), 0);
        assert_eq!(s.state, DaemonState::Stopped);
        assert!(s.reason.contains("crashed"));
        assert_eq!(s.pid, Some(0x7fff_ffff));
    }

    #[test]
    fn probe_notifyd_live_pid_with_socket_and_db_is_connected() {
        let base = TempDir::new().unwrap();
        std::fs::write(
            base.path().join("notify.pid"),
            format!("{}\n", std::process::id()),
        )
        .unwrap();
        std::fs::write(base.path().join("notify.sock"), b"").unwrap();
        std::fs::write(base.path().join("notifications.db"), b"").unwrap();
        let s = probe_notifyd(base.path(), super::super::heartbeat::now_ms());
        assert_eq!(s.state, DaemonState::Running);
        assert!(s.connected);
        assert!(s.reason.contains("socket bound"));
    }

    #[test]
    fn probe_notifyd_live_pid_without_socket_is_running_not_connected() {
        let base = TempDir::new().unwrap();
        std::fs::write(
            base.path().join("notify.pid"),
            format!("{}\n", std::process::id()),
        )
        .unwrap();
        let s = probe_notifyd(base.path(), super::super::heartbeat::now_ms());
        assert_eq!(s.state, DaemonState::Running);
        assert!(!s.connected);
        assert!(s.reason.contains("socket not bound"));
    }

    #[test]
    fn probe_atc_no_instance_is_stopped() {
        let home = TempDir::new().unwrap();
        let s = probe_atc(home.path(), 0);
        assert_eq!(s.state, DaemonState::Stopped);
        assert!(s.reason.contains("no ATC instance"));
    }

    #[test]
    fn probe_atc_fresh_heartbeat_is_running() {
        let home = TempDir::new().unwrap();
        let atc_dir = home.path().join("atc").join("primary");
        std::fs::create_dir_all(&atc_dir).unwrap();
        std::fs::write(
            atc_dir.join("meta.json"),
            r#"{"name":"primary","heartbeat_enabled":true,"heartbeat_interval_min":15,"idle_pause_min":60}"#,
        )
        .unwrap();
        let now = super::super::heartbeat::now_ms();
        std::fs::write(
            atc_dir.join("heartbeat-state.json"),
            format!(
                r#"{{"last_heartbeat_ms":{},"last_active_ms":{},"continue_counts":{{}}}}"#,
                now - 1000,
                now - 2000
            ),
        )
        .unwrap();
        let s = probe_atc(home.path(), now);
        assert_eq!(s.state, DaemonState::Running);
        assert!(s.connected);
        assert!(s.channel.as_deref().unwrap().contains("primary"));
    }

    #[test]
    fn probe_atc_old_heartbeat_is_stale_stopped() {
        let home = TempDir::new().unwrap();
        let atc_dir = home.path().join("atc").join("primary");
        std::fs::create_dir_all(&atc_dir).unwrap();
        std::fs::write(
            atc_dir.join("meta.json"),
            r#"{"name":"primary","heartbeat_enabled":true,"heartbeat_interval_min":15,"idle_pause_min":60}"#,
        )
        .unwrap();
        let now = super::super::heartbeat::now_ms();
        // last beat hours ago — well past 2*15m + grace.
        std::fs::write(
            atc_dir.join("heartbeat-state.json"),
            format!(
                r#"{{"last_heartbeat_ms":{},"continue_counts":{{}}}}"#,
                now - 10 * 3_600_000
            ),
        )
        .unwrap();
        let s = probe_atc(home.path(), now);
        assert_eq!(s.state, DaemonState::Stopped);
        assert!(s.reason.contains("stale"));
    }

    #[test]
    fn collect_in_returns_all_four_in_stable_order() {
        let home = TempDir::new().unwrap();
        let notifyd = TempDir::new().unwrap();
        let rows = collect_in(
            home.path(),
            notifyd.path(),
            super::super::heartbeat::now_ms(),
        );
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].kind, DaemonKind::Bridge);
        assert_eq!(rows[1].kind, DaemonKind::Notifyd);
        assert_eq!(rows[2].kind, DaemonKind::Atc);
        assert_eq!(rows[3].kind, DaemonKind::FleetDaemon);
        // Empty homes → everything stopped, never a false running.
        assert!(rows.iter().all(|r| r.state == DaemonState::Stopped));
    }

    #[test]
    fn collect_in_flips_bridge_to_running_when_heartbeat_present() {
        let home = TempDir::new().unwrap();
        let notifyd = TempDir::new().unwrap();
        let mut h = DaemonHeartbeat::starting();
        h.set_connected(true, Some("Telegram (@bot)".into()));
        h.record_activity();
        h.write_in(home.path(), DaemonKind::Bridge.id()).unwrap();
        let rows = collect_in(
            home.path(),
            notifyd.path(),
            super::super::heartbeat::now_ms(),
        );
        assert_eq!(rows[0].kind, DaemonKind::Bridge);
        assert_eq!(rows[0].state, DaemonState::Running);
        assert!(rows[0].connected);
        assert_eq!(rows[0].channel.as_deref(), Some("Telegram (@bot)"));
    }

    #[test]
    fn kind_ids_are_stable_and_distinct() {
        let ids = [
            DaemonKind::Bridge.id(),
            DaemonKind::Notifyd.id(),
            DaemonKind::Atc.id(),
            DaemonKind::FleetDaemon.id(),
        ];
        assert_eq!(ids, ["bridge", "notifyd", "atc", "fleet-daemon"]);
        // No duplicates → no heartbeat-file collisions.
        let mut sorted = ids.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 4);
    }
}
