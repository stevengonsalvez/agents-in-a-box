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

use super::heartbeat::{DaemonHeartbeat, PidCheck, is_pid_alive, pid_identity, process_binary};

/// A heartbeat older than this (and whose pid is *still* alive — e.g. a wedged
/// daemon that stopped ticking) is treated as stale. A dead pid is caught
/// immediately regardless of this window. 90s comfortably covers the bridge's
/// 30s long-poll cycle and the fleet daemon's 5s tick with headroom.
pub const STALE_AFTER_MS: i64 = 90_000;

/// [`STALE_AFTER_MS`] with `daemons.stale_after_ms` applied.
///
/// A function, not a const, because the value now comes from config; the const
/// stays as the coded default and as the value the unit tests reason against.
#[must_use]
pub fn stale_after_ms() -> i64 {
    crate::config::tunables::snapshot().daemons.stale_after_ms
}

/// The outbound-push staleness window for the bridge.
///
/// How long the bridge may go without a SUCCESSFUL poll of the attention source
/// before its outbound (proactive phone push) half is considered broken. Three
/// times the outbound worker's 15s default poll interval, so a single missed or
/// slow tick never degrades the row while a genuinely unreachable daemon is
/// caught within a minute.
///
/// This is a distinct clock from [`STALE_AFTER_MS`]: that one asks "is the
/// process alive?", this one asks "can the process still do the job?". The
/// bridge answered yes to the first and (silently) no to the second for its
/// entire life, which is the blind spot this closes.
pub const ATTENTION_STALE_AFTER_MS: i64 = 45_000;

/// [`ATTENTION_STALE_AFTER_MS`] with `daemons.attention_stale_after_ms` applied.
#[must_use]
pub fn attention_stale_after_ms() -> i64 {
    crate::config::tunables::snapshot().daemons.attention_stale_after_ms
}

/// Which daemon a [`DaemonStatus`] describes. Stable wire strings for the JSON
/// surface and stable display names for the text/TUI surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DaemonKind {
    /// Native Telegram/Slack phone bridge.
    Bridge,
    /// Unix-socket notification daemon (`ainb-notifyd`).
    Notifyd,
    /// Synchronous permission approve/deny broker (`approve.sock`, served on
    /// notifyd's runtime). A distinct socket, tracked as its own row so every
    /// socket ainb serves is visible in the Daemons view.
    ApproveBroker,
    /// Air Traffic Control fleet brain.
    Atc,
    /// Auto-continue API-error watcher (`ainb fleet daemon`).
    FleetDaemon,
    /// Shared MCP server pool (`ainb mcp daemon`), reachable on its control
    /// socket. One process serving every session's MCP servers.
    McpPool,
    /// Hangar daemon — the board/fleet backend behind the `g` screen.
    HangarDaemon,
    /// Headroom context proxy (`headroom proxy`), when ainb manages one.
    HeadroomProxy,
    /// Daily short-lived signed release checker.
    ReleaseChecker,
}

impl DaemonKind {
    /// Stable lowercase id (heartbeat filename + JSON `kind`).
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::Bridge => "bridge",
            Self::Notifyd => "notifyd",
            Self::ApproveBroker => "approve-broker",
            Self::Atc => "atc",
            Self::FleetDaemon => "fleet-daemon",
            Self::McpPool => "mcp-pool",
            Self::HangarDaemon => "hangar-daemon",
            Self::HeadroomProxy => "headroom-proxy",
            Self::ReleaseChecker => "release-checker",
        }
    }

    /// Human display name for the text/TUI surfaces.
    #[must_use]
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Bridge => "phone bridge",
            Self::Notifyd => "notifyd",
            Self::ApproveBroker => "approve broker",
            Self::Atc => "ATC",
            Self::FleetDaemon => "fleet daemon",
            Self::McpPool => "mcp pool",
            Self::HangarDaemon => "hangar daemon",
            Self::HeadroomProxy => "headroom proxy",
            Self::ReleaseChecker => "release checker",
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
    /// Process alive, but one of the jobs it exists to do is provably not
    /// happening. The bridge case: the chat gateway is connected (inbound works)
    /// while the outbound worker cannot reach the attention source, so nothing
    /// is ever pushed to the phone. `Running` would be a lie and `Stopped` would
    /// be one too; partial health needs its own word.
    Degraded,
    /// Not running. The `reason` on [`DaemonStatus`] says whether it's a clean
    /// stop, a stale heartbeat from a crash, or never-configured.
    Stopped,
    /// We couldn't determine the state (e.g. an error reading a signal). Rare;
    /// surfaced rather than guessed so the operator knows the probe itself is
    /// the problem, not the daemon.
    Unknown,
}

impl DaemonState {
    /// Is the daemon fully healthy?
    ///
    /// `Degraded` deliberately answers `false`: every caller that used to ask
    /// `state == Running` to mean "all good" gets the honest answer without
    /// having to learn the new variant.
    #[must_use]
    pub const fn is_healthy(self) -> bool {
        matches!(self, Self::Running)
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
    /// Ainb version which owns this runtime, when it can be established.
    /// `None` is intentionally honest: an old heartbeat or a scheduled job is
    /// not evidence that it runs this binary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Whether [`Self::version`] equals this invoking Ainb binary.  `None`
    /// means version unknown / not meaningful for this kind of runtime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_current: Option<bool>,
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
    /// Epoch ms of the last SUCCESSFUL poll of the attention source by this
    /// daemon's outbound worker (bridge only). `None` = never polled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_attention_poll_at: Option<i64>,
    /// The last attention-source failure, when known (bridge only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_attention_error: Option<String>,
    /// How many INBOUND chat channels the daemon started (bridge only). `0` for
    /// a daemon that makes no inbound claim.
    #[serde(default)]
    pub inbound_expected: u32,
    /// How many of those are still running. Reported alongside
    /// [`Self::last_attention_poll_at`], never folded into it: the operator has
    /// to be able to see which HALF of the bridge is broken.
    #[serde(default)]
    pub inbound_live: u32,
    /// Why the last inbound channel stopped, when known (bridge only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_inbound_error: Option<String>,
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
            version: None,
            version_current: None,
            connected: false,
            channel: None,
            last_activity_at: None,
            error_count: 0,
            last_error: None,
            last_attention_poll_at: None,
            last_attention_error: None,
            inbound_expected: 0,
            inbound_live: 0,
            last_inbound_error: None,
            reason: reason.into(),
        }
    }

    /// A blank `Running` row for the socket-probed daemons, which have no
    /// heartbeat file to read uptime, activity, or error counts from. Callers
    /// fill in what they actually know via struct-update syntax; everything
    /// left at the default stays honestly empty rather than being invented.
    fn running(kind: DaemonKind) -> Self {
        Self {
            state: DaemonState::Running,
            ..Self::stopped(kind, String::new())
        }
    }
}

/// Version carried by a self-reporting heartbeat. Old heartbeats did not have
/// this field; leave them unknown instead of pretending an upgraded observer
/// upgraded a still-running daemon.
fn heartbeat_version(hb: &DaemonHeartbeat) -> (Option<String>, Option<bool>) {
    let version = hb.ainb_version.clone();
    let current = version.as_deref().map(|running| running == env!("CARGO_PKG_VERSION"));
    (version, current)
}

/// True only for a known released version strictly older than this Ainb
/// binary. Unknown, prerelease, equal, and newer versions return false: repair
/// paths are upgrade-only and must never downgrade a live daemon.
#[must_use]
pub(crate) fn release_version_is_older(running: &str, current: &str) -> bool {
    fn parts(version: &str) -> Option<[u64; 3]> {
        let mut parts = version.split('.').map(str::parse::<u64>);
        let parsed = [
            parts.next()?.ok()?,
            parts.next()?.ok()?,
            parts.next()?.ok()?,
        ];
        parts.next().is_none().then_some(parsed)
    }
    match (parts(running), parts(current)) {
        (Some(running), Some(current)) => running < current,
        _ => false,
    }
}

/// Version evidence for a socket daemon which predates version sidecars.
/// Homebrew paths carry an immutable release version. Development and Cargo
/// paths are only called current when they resolve to this exact executable;
/// every other path remains unknown rather than guessed.
fn process_version(pid: Option<u32>) -> (Option<String>, Option<bool>) {
    let Some(path) = pid.and_then(process_binary) else {
        return (None, None);
    };
    let raw = path.to_string_lossy();
    if let Some((_, rest)) = raw.split_once("/Cellar/ainb/") {
        if let Some(version) = rest.split('/').next().filter(|value| !value.is_empty()) {
            let version = version.to_string();
            return (
                Some(version.clone()),
                Some(version == env!("CARGO_PKG_VERSION")),
            );
        }
    }
    let same_binary = std::env::current_exe()
        .ok()
        .and_then(|current| {
            std::fs::canonicalize(current)
                .ok()
                .zip(std::fs::canonicalize(&path).ok())
                .map(|(current, running)| current == running)
        })
        .unwrap_or(path == std::env::current_exe().unwrap_or_default());
    if same_binary {
        return (Some(env!("CARGO_PKG_VERSION").to_string()), Some(true));
    }
    (None, None)
}

/// The outbound (proactive phone push) verdict for one heartbeat. Separated from
/// the process-liveness verdict because they answer different questions: a bridge
/// can be perfectly alive and connected to Discord while pushing nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
enum OutboundVerdict {
    /// This daemon has no outbound attention worker, so nothing to judge.
    NotApplicable,
    /// The worker exists but has not had time for its first poll yet.
    Starting,
    /// A successful attention poll happened inside the window.
    Healthy,
    /// The worker cannot reach the attention source. Carries the operator-facing
    /// explanation, which always names WHAT is unreachable.
    Unreachable(String),
    /// The worker reaches the attention source fine, but a push did not reach
    /// the human: the channel send failed and an ask is sitting undelivered.
    /// Carries the (scrubbed) channel error.
    Undelivered(String),
}

/// The inbound (chat gateway the human TALKS to) verdict for one heartbeat.
///
/// Deliberately a separate type from [`OutboundVerdict`], and never folded into
/// it: the two halves fail independently, they fail for different reasons, and
/// the operator's fix is different for each, so the health line has to be able
/// to name which one is down.
#[derive(Debug, Clone, PartialEq, Eq)]
enum InboundVerdict {
    /// This daemon runs no inbound chat channel, or its heartbeat predates the
    /// inbound accounting, so there is no claim to judge.
    NotApplicable,
    /// Every channel the daemon started is still running. Carries the `live/expected`
    /// tally so a healthy row can SHOW the inbound half rather than imply it.
    Healthy(String),
    /// Some, but not all, channels have stopped. The phone can still reach the
    /// fleet on the survivors, which is exactly why this must not be silent.
    Partial(String),
    /// Every channel has stopped: nothing the human sends from the phone can
    /// reach the fleet any more, however healthy the outbound push looks.
    Dead(String),
}

/// Judge the inbound half of a bridge heartbeat: how many chat channels are
/// still running out of the number the daemon started.
///
/// Pure over its inputs. `inbound_expected == 0` is the "no claim" case (a
/// non-bridge daemon, or a `bridge.json` written before the fields existed) and
/// must never degrade a row, or every pre-existing heartbeat would read as
/// broken the moment it is upgraded.
fn classify_inbound(kind: DaemonKind, hb: &DaemonHeartbeat) -> InboundVerdict {
    if kind != DaemonKind::Bridge || hb.inbound_expected == 0 {
        return InboundVerdict::NotApplicable;
    }
    let expected = hb.inbound_expected;
    // A live count above the declared total would be nonsense on disk; clamp so
    // a hand-edited or truncated record can never read as more-than-healthy.
    let live = hb.inbound_live.min(expected);
    let tally = format!("{live}/{expected} chat channels running");
    if live == expected {
        return InboundVerdict::Healthy(tally);
    }
    let cause = hb.last_inbound_error.as_deref().map_or_else(
        || "no exit reason recorded".to_string(),
        |e| format!("last exit: {e}"),
    );
    let detail = format!("{tally}, {cause}");
    if live == 0 {
        InboundVerdict::Dead(detail)
    } else {
        InboundVerdict::Partial(detail)
    }
}

/// Turn the two half-verdicts into the row's state + operator-facing reason.
///
/// EITHER half being broken degrades the row, and the reason names which one (or
/// both), because "the bridge is degraded" without saying whether the human
/// cannot be TOLD or cannot ANSWER sends the operator to the wrong place.
fn health_verdict(
    inbound: &InboundVerdict,
    outbound: &OutboundVerdict,
    connected: bool,
) -> (DaemonState, String) {
    let head = if connected {
        "running + connected"
    } else {
        "running (connecting…)"
    };

    let mut faults: Vec<String> = Vec::new();
    match inbound {
        InboundVerdict::Dead(detail) => faults.push(format!(
            "the INBOUND chat gateway is dead, so nothing from the phone can reach the fleet ({detail})"
        )),
        InboundVerdict::Partial(detail) => {
            faults.push(format!("an INBOUND chat channel has stopped ({detail})"));
        }
        InboundVerdict::Healthy(_) | InboundVerdict::NotApplicable => {}
    }
    match outbound {
        OutboundVerdict::Unreachable(detail) => faults.push(format!(
            "outbound cannot reach the attention source (hangar daemon attention/list): {detail}"
        )),
        OutboundVerdict::Undelivered(detail) => faults.push(format!(
            "a proactive push did not reach the human (channel send failed): {detail}"
        )),
        OutboundVerdict::Healthy | OutboundVerdict::Starting | OutboundVerdict::NotApplicable => {}
    }
    if !faults.is_empty() {
        return (
            DaemonState::Degraded,
            format!("{head}, but {}", faults.join(", and ")),
        );
    }

    // Nothing is broken. A not-yet-connected gateway is still the benign
    // starting state, and says nothing more than that.
    if !connected {
        return (DaemonState::Running, "running (connecting…)".to_string());
    }
    let mut notes: Vec<String> = Vec::new();
    if let InboundVerdict::Healthy(tally) = inbound {
        notes.push(format!("inbound {tally}"));
    }
    match outbound {
        OutboundVerdict::Starting => notes.push("outbound push starting…".to_string()),
        OutboundVerdict::Healthy => notes.push("outbound push live".to_string()),
        _ => {}
    }
    let reason = if notes.is_empty() {
        head.to_string()
    } else {
        format!("{head} ({})", notes.join(", "))
    };
    (DaemonState::Running, reason)
}

/// Judge the outbound half of a bridge heartbeat. Pure over its inputs so every
/// arm is unit-testable without a socket, a daemon, or a clock.
///
/// Only the bridge is judged: it is the only daemon that owes the human a
/// proactive push, and the only one whose outbound worker stamps
/// `last_attention_poll_at`.
fn classify_outbound(
    kind: DaemonKind,
    hb: &DaemonHeartbeat,
    uptime_ms: i64,
    now_ms: i64,
) -> OutboundVerdict {
    if kind != DaemonKind::Bridge {
        return OutboundVerdict::NotApplicable;
    }
    // Ask the delivery question FIRST. A fresh `last_attention_poll_at` only
    // proves the bridge can READ the fleet's asks; judging on it alone is how a
    // bridge whose every Discord send 429'd still rendered "outbound push live"
    // while the human got nothing. An undelivered push outranks a healthy poll.
    if let Some(detail) = hb.last_delivery_error.as_deref() {
        return OutboundVerdict::Undelivered(detail.to_string());
    }
    let cause = hb.last_attention_error.as_deref().map_or_else(
        || "no error recorded, so the outbound push worker is probably not running".to_string(),
        |e| format!("last error: {e}"),
    );
    match hb.last_attention_poll_at {
        Some(last) => {
            let age = now_ms.saturating_sub(last);
            if age <= attention_stale_after_ms() {
                OutboundVerdict::Healthy
            } else {
                OutboundVerdict::Unreachable(format!(
                    "last successful attention/list poll {}s ago ({cause})",
                    age / 1000
                ))
            }
        }
        // Never polled. Give the worker one window to complete its first poll
        // before calling it broken, then say so in the operator's words.
        None if uptime_ms <= attention_stale_after_ms() => OutboundVerdict::Starting,
        None => OutboundVerdict::Unreachable(format!(
            "no successful attention/list poll in {}s of uptime ({cause})",
            uptime_ms / 1000
        )),
    }
}

/// Decide a heartbeat-backed daemon's status from its heartbeat (if any),
/// cross-checking process *identity* (not mere liveness) and the staleness
/// window against `now_ms`. Pure over its inputs so it is exhaustively
/// unit-testable without touching disk.
///
/// `pid_check` is the identity verdict from [`super::heartbeat::pid_identity`]:
/// liveness alone is never enough — a recycled pid (a different process that
/// inherited the dead daemon's pid) must report Stopped, not Running.
#[must_use]
pub fn classify_heartbeat(
    kind: DaemonKind,
    hb: Option<DaemonHeartbeat>,
    pid_check: PidCheck,
    now_ms: i64,
) -> DaemonStatus {
    let Some(hb) = hb else {
        return DaemonStatus::stopped(kind, "no heartbeat — not running this session");
    };

    let age = now_ms.saturating_sub(hb.last_heartbeat_at);
    let uptime_ms = now_ms.saturating_sub(hb.started_at);
    let uptime = Some(uptime_ms);
    let (version, version_current) = heartbeat_version(&hb);

    // A dead OR recycled pid is the strongest possible signal: the process that
    // wrote this heartbeat is gone, so the heartbeat is a tombstone no matter
    // how recent it looks. A recycled pid is *alive* but is a different process
    // that merely inherited the dead daemon's pid — treating it as Running was
    // the bug.
    if matches!(pid_check, PidCheck::Dead | PidCheck::Recycled) {
        let reason = match pid_check {
            PidCheck::Recycled => format!(
                "stale heartbeat — pid {} recycled (different process, crashed)",
                hb.pid
            ),
            // PidCheck::Dead (Matched is excluded by the match arm).
            _ => format!("stale heartbeat — pid {} not alive (crashed)", hb.pid),
        };
        return DaemonStatus {
            kind,
            state: DaemonState::Stopped,
            pid: Some(hb.pid),
            uptime_ms: None,
            version,
            version_current,
            connected: false,
            channel: hb.channel,
            last_activity_at: hb.last_activity_at,
            error_count: hb.error_count,
            last_error: hb.last_error,
            last_attention_poll_at: hb.last_attention_poll_at,
            last_attention_error: hb.last_attention_error,
            inbound_expected: hb.inbound_expected,
            inbound_live: hb.inbound_live,
            last_inbound_error: hb.last_inbound_error,
            reason,
        };
    }

    // The pid is alive but the heartbeat went quiet past the window: the process
    // exists but its loop is wedged or paused. Report stopped+stale so we don't
    // claim a healthy daemon.
    if age > stale_after_ms() {
        return DaemonStatus {
            kind,
            state: DaemonState::Stopped,
            pid: Some(hb.pid),
            uptime_ms: uptime,
            version,
            version_current,
            connected: false,
            channel: hb.channel,
            last_activity_at: hb.last_activity_at,
            error_count: hb.error_count,
            last_error: hb.last_error,
            last_attention_poll_at: hb.last_attention_poll_at,
            last_attention_error: hb.last_attention_error,
            inbound_expected: hb.inbound_expected,
            inbound_live: hb.inbound_live,
            last_inbound_error: hb.last_inbound_error,
            reason: format!("stale heartbeat — last beat {}s ago (wedged?)", age / 1000),
        };
    }

    // The process is alive and beating. Now ask the SECOND question the old
    // classifier never asked: is the work actually getting done? For the bridge
    // that means the outbound worker reaching the daemon's attention inbox.
    // "Connected" is only ever about the INBOUND chat gateway, so a bridge whose
    // outbound push is absent used to read as fully healthy while 18 phone-routed
    // asks sat undelivered.
    let outbound = classify_outbound(kind, &hb, uptime_ms, now_ms);
    // And the THIRD question, which `connected` cannot answer either: are the
    // chat channels the human talks to still running? `connected` is stamped
    // once at each channel's handshake and never reset when that channel's task
    // dies, so a bridge that can no longer hear the phone at all reported
    // "running + connected" for as long as the process lived.
    let inbound = classify_inbound(kind, &hb);
    let (state, reason) = health_verdict(&inbound, &outbound, hb.connected);

    DaemonStatus {
        kind,
        state,
        pid: Some(hb.pid),
        uptime_ms: uptime,
        version,
        version_current,
        connected: hb.connected,
        channel: hb.channel,
        last_activity_at: hb.last_activity_at,
        error_count: hb.error_count,
        last_error: hb.last_error,
        last_attention_poll_at: hb.last_attention_poll_at,
        last_attention_error: hb.last_attention_error,
        inbound_expected: hb.inbound_expected,
        inbound_live: hb.inbound_live,
        last_inbound_error: hb.last_inbound_error,
        reason,
    }
}

/// Probe a heartbeat-backed daemon (bridge / fleet daemon) under an explicit
/// ainb home. Reads the heartbeat, checks pid liveness, classifies.
#[must_use]
pub fn probe_heartbeat_daemon(home: &Path, kind: DaemonKind, now_ms: i64) -> DaemonStatus {
    let hb = DaemonHeartbeat::read_in(home, kind.id());
    // Cross-check process IDENTITY, not bare liveness: a recycled pid is alive
    // but is not our daemon, so it must classify as Stopped (H1).
    let pid_check = hb.as_ref().map_or(PidCheck::Dead, |h| pid_identity(h.pid, h.started_at));
    classify_heartbeat(kind, hb, pid_check, now_ms)
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
    let (version, version_current) = process_version(pid);
    let pid_alive = pid.is_some_and(is_pid_alive);
    // L1: a bound, ACCEPTING listener — not merely a socket file on disk. A
    // crashed daemon can leave a stale `notify.sock` behind; `exists()` would
    // then falsely report "connected". A non-blocking connect proves a listener
    // is actually serving.
    let socket_ok = socket_is_listening(&socket_path);
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

    // Pid is alive. Connection health = the socket is bound AND the DB file is
    // present. M-D1: we deliberately say "db file present", NOT "db reachable" —
    // `db_path.exists()` only proves the FILE is there, not that it opens and
    // accepts writes. A truncated/corrupt `notifications.db` still `exists()`, so
    // claiming "reachable" would show green while every insert silently fails into
    // the fallback file. Honest cheap wording avoids a real (blocking) DB open on
    // this code path.
    let connected = socket_ok && db_ok;
    let reason = if connected {
        "running + connected (socket bound, db file present)".to_string()
    } else if !socket_ok {
        "running but socket not bound yet".to_string()
    } else {
        "running but db file missing".to_string()
    };
    DaemonStatus {
        kind,
        state: DaemonState::Running,
        pid,
        uptime_ms: None, // notifyd's PID file carries no start time
        version,
        version_current,
        connected,
        channel: Some("unix socket + sqlite".to_string()),
        last_activity_at: db_mtime_ms(&db_path),
        error_count: 0,
        last_error: None,
        last_attention_poll_at: None,
        last_attention_error: None,
        inbound_expected: 0,
        inbound_live: 0,
        last_inbound_error: None,
        reason,
    }
    .with_now(now_ms)
}

/// Probe the permission approve/deny broker from its socket. The broker is
/// served on notifyd's runtime, so its liveness is `approve.sock` accepting a
/// connection — not a separate pid. A listening socket means the synchronous
/// round-trip is available; the pending-request count (cheap `client_list` over
/// the same socket) goes in the reason so the operator can see waiting hooks.
#[must_use]
pub fn probe_approve_broker(base: &Path, now_ms: i64) -> DaemonStatus {
    let kind = DaemonKind::ApproveBroker;
    let socket_path = base.join("approve.sock");

    // L1: a bound listener, not merely a socket file on disk. A crashed notifyd
    // can leave a stale `approve.sock`; a non-blocking connect proves a server.
    if !socket_is_listening(&socket_path) {
        let reason = if socket_path.exists() {
            "stale approve.sock — broker not accepting (notifyd down?)".to_string()
        } else {
            "no approve.sock — broker not running".to_string()
        };
        return DaemonStatus::stopped(kind, reason);
    }

    // Socket is serving. Fold in the pending-waiter count when the list RPC
    // answers; a transient RPC error must not flip the row to Stopped (the
    // listener is provably up), so degrade to a countless "serving" reason.
    let reason = ainb_plugin_notifyd::broker::client_list(&socket_path).map_or_else(
        |_| "serving (pending count unavailable)".to_string(),
        // A count with no verb is what made the queue feel un-inspectable, so
        // every non-zero row names the exact command that shows it.
        |pending| match pending.len() {
            0 => "serving, no pending requests".to_string(),
            1 => "serving, 1 pending request (see `ainb fleet approve`)".to_string(),
            n => format!("serving, {n} pending requests (see `ainb fleet approve`)"),
        },
    );
    let notify_pid = read_pid_file(&base.join("notify.pid"));
    let (version, version_current) = process_version(notify_pid);
    DaemonStatus {
        kind,
        state: DaemonState::Running,
        pid: None, // rides notifyd's process; no pid of its own
        uptime_ms: None,
        version,
        version_current,
        connected: true,
        channel: Some("approve socket".to_string()),
        last_activity_at: None,
        error_count: 0,
        last_error: None,
        last_attention_poll_at: None,
        last_attention_error: None,
        inbound_expected: 0,
        inbound_live: 0,
        last_inbound_error: None,
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
    //
    // Only instances whose `heartbeat_enabled` is true are running-sources: an
    // instance with the OS timer DISABLED is not running no matter how recent
    // its last (pre-disable) heartbeat looks — counting it was the M2 false
    // "running". Track how many enabled instances we saw so we can distinguish
    // "all disabled" from "none have beaten yet".
    let mut best: Option<(String, HeartbeatState, u32)> = None;
    let mut enabled_count = 0_usize;
    for name in &names {
        let p = AtcPaths::under_root(&atc_root, name);
        let meta = std::fs::read_to_string(&p.meta).ok().and_then(|s| AtcMeta::from_json(&s).ok());
        // Default to enabled when meta is unreadable: a missing/corrupt meta is a
        // probe problem, not proof the timer is off, so we don't silently hide a
        // possibly-running instance. `AtcMeta::new` defaults `heartbeat_enabled`
        // to true, matching this.
        let heartbeat_enabled = meta.as_ref().map_or(true, |m| m.heartbeat_enabled);
        let interval_min = meta.map_or(15, |m| m.heartbeat_interval_min.max(1));
        if !heartbeat_enabled {
            // Disabled instances are never a running-source. Skip before reading
            // the heartbeat state so a stale beat can't promote it.
            continue;
        }
        enabled_count += 1;
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
        // No enabled instance produced a usable heartbeat. If NONE of the
        // provisioned instances are even enabled, the daemon is configured off.
        if enabled_count == 0 {
            return DaemonStatus::stopped(
                kind,
                format!(
                    "{} instance(s) provisioned, all heartbeat-disabled",
                    names.len()
                ),
            );
        }
        return DaemonStatus::stopped(
            kind,
            format!(
                "{} enabled instance(s), none have beaten yet",
                enabled_count
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
    let window_ms = (interval_min as i64) * 60_000 * 2 + stale_after_ms();
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
        version: None,
        version_current: None,
        connected: true,
        channel: Some(format!("{name} (every {interval_min}m)")),
        last_activity_at: hbs.last_active_ms,
        error_count: 0,
        last_error: None,
        last_attention_poll_at: None,
        last_attention_error: None,
        inbound_expected: 0,
        inbound_live: 0,
        last_inbound_error: None,
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

/// Is a Unix-socket listener actually bound and accepting at `path`? (L1.)
///
/// `path.exists()` only proves a socket FILE is present — a crashed daemon
/// leaves a stale one behind. A `connect()` succeeds only when a listener is
/// bound and accepting; a stale socket file refuses the connection
/// (`ECONNREFUSED`). The connect is immediately dropped, so this is a cheap
/// liveness probe of the listener itself, not a duplicate read of
/// `socket_path.exists()`.
///
/// H-D2: the surrounding `collect` runs on a BACKGROUND tick, never the TUI
/// render thread, so the UI can never freeze on it. That is not the same as the
/// collector being safe, though: `connect(2)` on an AF_UNIX socket usually
/// returns at once (success or `ECONNREFUSED`), but it BLOCKS for as long as the
/// listener lives when a bound-but-not-accepting daemon has a full backlog —
/// the exact shape of a half-dead daemon. An unbounded connect there kills the
/// single collector thread for the rest of the process, which freezes the whole
/// table on its last snapshot with no way to recover it. So it is bounded.
fn socket_is_listening(path: &Path) -> bool {
    connect_bounded(path, SOCKET_PROBE_TIMEOUT).is_some()
}

/// How long any single socket probe may take before it is called dead.
pub(crate) const SOCKET_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);

/// Connect to `path`, giving up after `timeout`.
///
/// The connect runs on a throwaway thread because `std` offers no connect
/// timeout for AF_UNIX. A wedged connect leaks that one thread until the kernel
/// gives up on it, which is strictly better than leaking the collector.
pub(crate) fn connect_bounded(
    path: &Path,
    timeout: std::time::Duration,
) -> Option<std::os::unix::net::UnixStream> {
    let path = path.to_path_buf();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(std::os::unix::net::UnixStream::connect(path).ok());
    });
    rx.recv_timeout(timeout).ok().flatten()
}

/// Best-effort last-write time of a file as epoch ms — used as notifyd's
/// last-activity proxy (the DB is rewritten on every persisted notification).
fn db_mtime_ms(path: &Path) -> Option<i64> {
    let mtime = std::fs::metadata(path).ok()?.modified().ok()?;
    let dur = mtime.duration_since(std::time::UNIX_EPOCH).ok()?;
    i64::try_from(dur.as_millis()).ok()
}

/// Probe the shared MCP server pool from its own control socket.
///
/// Unlike the heartbeat daemons, the pool reports its identity over the wire —
/// so pid and version come from the process actually serving pooled tools, not
/// from a file some other binary wrote.
#[must_use]
pub fn probe_mcp_pool() -> DaemonStatus {
    let kind = DaemonKind::McpPool;
    // Probe the socket with a bounded connect BEFORE asking the client: its
    // `query` sets read/write timeouts only after an unbounded connect, so a
    // wedged listener would hang the collector there.
    let listening = crate::mcp_pool::paths::control_socket().ok().is_some_and(|path| {
        path.exists() && connect_bounded(&path, SOCKET_PROBE_TIMEOUT).is_some()
    });
    if !listening || !crate::mcp_pool::client::daemon_alive() {
        return DaemonStatus::stopped(kind, "control socket not answering".to_string());
    }
    let runtime = crate::mcp_pool::client::daemon_runtime_status();
    let reason = if runtime.old {
        "serving — older than this ainb, restart to upgrade".to_string()
    } else {
        "control socket serving".to_string()
    };
    DaemonStatus {
        pid: runtime.pid,
        version_current: runtime.version.as_deref().map(|v| v == env!("CARGO_PKG_VERSION")),
        version: runtime.version,
        connected: true,
        channel: Some("control socket".to_string()),
        reason,
        ..DaemonStatus::running(kind)
    }
}

/// Probe the Hangar daemon from its recorded ownership lock plus its socket.
///
/// The recorded owner is authoritative for pid/version — the same probe
/// `ainb hangar daemon status` uses, so the two surfaces cannot drift. The
/// socket connect answers the separate question of whether it is still serving.
#[must_use]
pub fn probe_hangar_daemon() -> DaemonStatus {
    let kind = DaemonKind::HangarDaemon;
    let runtime = crate::cli::hangar::daemon_runtime_status();
    let serving = crate::fleet::bridge::daemon::socket_path()
        .is_some_and(|socket| socket_is_listening(&socket));
    match (runtime.pid, serving) {
        (None, false) => DaemonStatus::stopped(kind, "not running".to_string()),
        // A recorded owner with a dead socket is the half-alive case the
        // Daemons screen exists to make visible.
        (Some(pid), false) => DaemonStatus {
            state: DaemonState::Degraded,
            pid: Some(pid),
            version_current: runtime.version.as_deref().map(|v| v == env!("CARGO_PKG_VERSION")),
            version: runtime.version,
            reason: format!("pid {pid} owns this home but the socket is not accepting"),
            ..DaemonStatus::running(kind)
        },
        (pid, true) => DaemonStatus {
            pid,
            version_current: runtime.version.as_deref().map(|v| v == env!("CARGO_PKG_VERSION")),
            version: runtime.version,
            connected: true,
            channel: Some("unix socket".to_string()),
            reason: if runtime.old {
                "serving — older than this ainb, restart to upgrade".to_string()
            } else {
                "socket serving".to_string()
            },
            ..DaemonStatus::running(kind)
        },
    }
}

/// Probe the Headroom context proxy.
///
/// Only the ainb-managed proxy records a pid; a proxy the user started by hand
/// still shows as running (the port answers) but with no pid, which is honest
/// rather than a guess.
#[must_use]
pub fn probe_headroom_proxy() -> DaemonStatus {
    let kind = DaemonKind::HeadroomProxy;
    let port = crate::headroom::proxy_port();
    if !crate::headroom::is_listening() {
        let reason = if crate::headroom::is_installed() {
            format!("nothing listening on port {port}")
        } else {
            "headroom is not installed".to_string()
        };
        return DaemonStatus::stopped(kind, reason);
    }
    DaemonStatus {
        pid: crate::headroom::pid(),
        connected: true,
        channel: Some(format!("http :{port}")),
        reason: format!("listening on port {port}"),
        ..DaemonStatus::running(kind)
    }
}

/// Probe the daily OS release-check registration.
///
/// This row deliberately models a scheduled oneshot, not a long-running
/// process. `Running` means the supervisor owns a daily job; the last verified
/// check time remains visible as daemon activity.
#[must_use]
pub fn probe_release_checker() -> DaemonStatus {
    let kind = DaemonKind::ReleaseChecker;
    if !crate::cli::update::schedule_is_enabled() {
        return DaemonStatus::stopped(kind, "daily release check is disabled".to_string());
    }
    let cached = crate::cli::update::cached_state();
    DaemonStatus {
        connected: true,
        channel: Some(if cfg!(target_os = "macos") {
            "launchd daily timer".to_string()
        } else {
            "systemd user timer".to_string()
        }),
        last_activity_at: cached.as_ref().map(|state| state.checked_at_ms),
        reason: cached
            .as_ref()
            .map(|state| match state.availability {
                crate::cli::update::UpdateAvailability::Available => format!(
                    "daily signed check enabled, ainb {} available",
                    state.available_version.as_deref().unwrap_or(&state.latest_version)
                ),
                crate::cli::update::UpdateAvailability::CurrentOrNewer => {
                    "daily signed check enabled, current".to_string()
                }
            })
            .unwrap_or_else(|| "daily signed check enabled, awaiting first check".to_string()),
        ..DaemonStatus::running(kind)
    }
}

/// Append the CAUSE to a bridge row that is not running, when the bridge could
/// not have started at all.
///
/// "stale heartbeat — pid 89585 not alive (crashed)" is a true row and a dead
/// end: it never says the crash was `ainb fleet bridge run` exiting 1 on a
/// config with no `[fleet.bridge]` table. This is the surface an operator
/// actually reads (`ainb doctor`, `ainb fleet daemons`, the TUI Daemons
/// screen), so the cause belongs on it. Only the first line of the problem is
/// used — the full setup skeleton belongs in `bridge status`, not a table cell.
///
/// Pure over `problem` so the annotation is testable without a config file.
/// Which bridge rows a config problem may be appended to.
///
/// `Degraded` is deliberately excluded even though it is not `Running`: that
/// state means the process IS alive but half its job is not happening. Telling
/// the operator a live daemon "cannot start" is simply false, and it fires for
/// real — a daemon launched with a baked-in `AINB_CONFIG_PATH` reads a config
/// this process cannot see, so the row would read "running (connected), but …
/// — cannot start: no bridge config".
fn annotatable_bridge_row(row: &DaemonStatus) -> bool {
    row.kind == DaemonKind::Bridge
        && matches!(row.state, DaemonState::Stopped | DaemonState::Unknown)
}

pub fn annotate_bridge_config(rows: &mut [DaemonStatus], problem: Option<&str>) {
    let Some(problem) = problem else { return };
    let head = problem.lines().next().unwrap_or(problem).trim();
    for row in rows.iter_mut().filter(|r| annotatable_bridge_row(r)) {
        row.reason = format!("{} — cannot start: {head}", row.reason);
    }
}

/// Aggregate every daemon under an explicit ainb home + notifyd base. The
/// test seam — every path is injected so a test isolates to a tempdir.
///
/// `now_ms` is the single clock the staleness checks measure against.
#[must_use]
pub fn collect_in(ainb_home: &Path, notifyd_base: &Path, now_ms: i64) -> Vec<DaemonStatus> {
    vec![
        probe_heartbeat_daemon(ainb_home, DaemonKind::Bridge, now_ms),
        probe_notifyd(notifyd_base, now_ms),
        probe_approve_broker(notifyd_base, now_ms),
        probe_atc(ainb_home, now_ms),
        probe_heartbeat_daemon(ainb_home, DaemonKind::FleetDaemon, now_ms),
    ]
}

/// The socket-probed daemons, appended after the heartbeat ones.
///
/// Kept out of [`collect_in`] on purpose: these three resolve their own
/// endpoints from the environment (MCP control socket, Hangar home, Headroom
/// port) rather than from injected paths, so folding them into the pure seam
/// would make every `collect_in` test read whatever happens to be running on
/// the developer's machine.
#[must_use]
pub fn collect_socket_daemons() -> Vec<DaemonStatus> {
    vec![
        probe_mcp_pool(),
        probe_hangar_daemon(),
        probe_headroom_proxy(),
        probe_release_checker(),
    ]
}

/// Aggregate every daemon from the real on-disk layout. Resolves the ainb
/// home (honouring `$AINB_HOME`) for bridge/fleet/ATC; notifyd's base comes
/// from `Paths::from_home()`, which honours the same `$AINB_HOME` override,
/// so the probe reads exactly the files the daemon writes.
pub fn collect() -> anyhow::Result<Vec<DaemonStatus>> {
    let ainb_home = crate::fleet::plumbing::paths::ainb_home()?;
    let notifyd_base = ainb_plugin_notifyd::Paths::from_home()
        .map(|p| p.base)
        .unwrap_or_else(|_| ainb_home.clone());
    let mut rows = collect_in(&ainb_home, &notifyd_base, super::heartbeat::now_ms());
    rows.extend(collect_socket_daemons());
    // Only pay for the config read when a bridge row is actually down, and take
    // the memoised verdict — this runs on the TUI's refresh loop every couple of
    // seconds, and an uncached read shells out to the keychain each time.
    if rows.iter().any(|r| annotatable_bridge_row(r)) {
        annotate_bridge_config(
            &mut rows,
            crate::fleet::bridge::config_problem_cached().as_deref(),
        );
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    /// Pin the tunables snapshot to defaults for this module.
    ///
    /// `stale_after_ms()` and `attention_stale_after_ms()` read the snapshot,
    /// which lazily loads the developer's real
    /// `~/.agents-in-a-box/config/config.toml` — so anyone who had set
    /// `daemons.stale_after_ms` failed these tests locally while CI, with no
    /// config file, passed. The fixtures below are built from the consts, so
    /// the snapshot has to agree with them.
    fn pin_default_snapshot() -> std::sync::MutexGuard<'static, ()> {
        // The shared lock, held by the CALLER for the length of its test: the
        // snapshot is process-global, so installing it without the lock races
        // every other test that installs or reads one.
        let guard =
            crate::config::tunables::TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::config::tunables::install_snapshot(crate::config::AppConfig::default());
        guard
    }

    use super::*;
    use tempfile::TempDir;

    fn hb(pid: u32, started: i64, last_beat: i64, connected: bool) -> DaemonHeartbeat {
        DaemonHeartbeat {
            pid,
            started_at: started,
            last_heartbeat_at: last_beat,
            ainb_version: Some(env!("CARGO_PKG_VERSION").to_string()),
            last_activity_at: Some(last_beat),
            connected,
            channel: Some("Telegram".into()),
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

    #[test]
    fn heartbeat_version_marks_only_this_binary_current() {
        let mut heartbeat = hb(42, 1, 2, true);
        heartbeat.ainb_version = Some(env!("CARGO_PKG_VERSION").to_string());
        assert_eq!(
            heartbeat_version(&heartbeat),
            (heartbeat.ainb_version.clone(), Some(true))
        );
        heartbeat.ainb_version = Some("0.0.0".to_string());
        assert_eq!(
            heartbeat_version(&heartbeat),
            (Some("0.0.0".to_string()), Some(false))
        );
    }

    #[test]
    fn release_version_repair_is_upgrade_only() {
        assert!(release_version_is_older("1.20.4", "1.20.5"));
        assert!(!release_version_is_older("1.20.5", "1.20.5"));
        assert!(!release_version_is_older("1.20.6", "1.20.5"));
        assert!(!release_version_is_older("dev", "1.20.5"));
    }

    /// A bridge heartbeat whose outbound worker last reached the attention
    /// source at `polled_at` (`None` = never).
    ///
    /// Declares ONE inbound chat channel, still live: the shape a healthy live
    /// bridge writes. Tests that want the dead-inbound shape override
    /// `inbound_live`.
    fn bridge_hb(
        started: i64,
        last_beat: i64,
        connected: bool,
        polled_at: Option<i64>,
    ) -> DaemonHeartbeat {
        DaemonHeartbeat {
            channel: Some("Discord (gateway)".into()),
            last_attention_poll_at: polled_at,
            inbound_expected: 1,
            inbound_live: 1,
            ..hb(42, started, last_beat, connected)
        }
    }

    #[test]
    fn no_heartbeat_is_clean_stopped() {
        let s = classify_heartbeat(DaemonKind::Bridge, None, PidCheck::Dead, 1000);
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
            PidCheck::Matched,
            now,
        );
        assert_eq!(s.state, DaemonState::Running);
        assert!(s.connected);
        assert_eq!(s.pid, Some(42));
        assert_eq!(s.uptime_ms, Some(5000));
        // 5s of uptime is inside the first outbound poll window, so the row is
        // healthy but says so honestly: the push has not proven itself yet.
        assert_eq!(s.reason, "running + connected (outbound push starting…)");
    }

    // ── Outbound (proactive phone push) health ──────────────────────────────
    //
    // The defect these pin: `ainb fleet daemons` reported the phone bridge
    // "● running ... Discord (gateway), running + connected" while its outbound
    // worker had never once reached the attention source, so every phone-routed
    // ask sat undelivered. `connected` is set by the INBOUND gateway handshake
    // and can never answer the outbound question.

    #[test]
    fn bridge_that_never_polled_the_attention_source_is_degraded() {
        let now = 1_000_000;
        // Connected to Discord, beating happily, up for an hour, never polled.
        let s = classify_heartbeat(
            DaemonKind::Bridge,
            Some(bridge_hb(now - 3_600_000, now - 1_000, true, None)),
            PidCheck::Matched,
            now,
        );
        assert_eq!(
            s.state,
            DaemonState::Degraded,
            "a bridge whose outbound worker has never polled must not read as healthy: {}",
            s.reason
        );
        assert!(
            !s.state.is_healthy(),
            "Degraded must answer false to is_healthy"
        );
        assert!(
            s.reason.contains("attention/list"),
            "the reason must name what is unreachable: {}",
            s.reason
        );
        assert!(
            s.reason.contains("outbound cannot reach the attention source"),
            "reason: {}",
            s.reason
        );
        assert!(
            s.reason.contains("outbound push worker is probably not running"),
            "with no recorded error the reason must point at the missing worker: {}",
            s.reason
        );
        // The gateway connection is still true. That is the whole point: the
        // row is connected AND degraded at the same time.
        assert!(s.connected);
    }

    #[test]
    fn bridge_with_a_fresh_attention_poll_is_running() {
        let now = 1_000_000;
        let s = classify_heartbeat(
            DaemonKind::Bridge,
            Some(bridge_hb(
                now - 3_600_000,
                now - 1_000,
                true,
                Some(now - 5_000),
            )),
            PidCheck::Matched,
            now,
        );
        assert_eq!(s.state, DaemonState::Running);
        assert!(s.state.is_healthy());
        // A green row SHOWS both halves rather than implying them: the operator
        // can see the chat channel is up as well as the push.
        assert_eq!(
            s.reason,
            "running + connected (inbound 1/1 chat channels running, outbound push live)"
        );
        assert_eq!(s.last_attention_poll_at, Some(now - 5_000));
    }

    // ── Inbound (the chat gateway the human TALKS to) health ────────────────
    //
    // The mirror of the outbound defect, and the one the outbound fix made
    // permanent: the outbound worker's poll keeps the liveness clock fresh
    // forever, so the process never exits and never goes stale, while
    // `connected` still reads true from a handshake that happened before the
    // channel task died. The result is a bridge that pushes asks to the phone
    // and can no longer receive a single answer, rendering as "● running".

    #[test]
    fn bridge_whose_only_chat_channel_died_is_degraded_and_names_the_inbound_half() {
        let now = 1_000_000;
        // The exact record: gateway handshake succeeded an hour ago, the idle
        // ticker and the outbound poll are both fresh, and the one channel task
        // has since exited.
        let mut h = bridge_hb(now - 3_600_000, now - 1_000, true, Some(now - 5_000));
        h.inbound_live = 0;
        h.last_inbound_error = Some("Telegram channel stopped: building HTTP client failed".into());
        let s = classify_heartbeat(DaemonKind::Bridge, Some(h), PidCheck::Matched, now);

        assert_eq!(
            s.state,
            DaemonState::Degraded,
            "a bridge that can no longer hear the phone must not read healthy: {}",
            s.reason
        );
        assert!(
            s.reason.contains("INBOUND"),
            "the reason must name WHICH half is dead: {}",
            s.reason
        );
        assert!(
            s.reason.contains("0/1 chat channels running"),
            "the reason must quantify the loss: {}",
            s.reason
        );
        assert!(
            s.reason.contains("building HTTP client failed"),
            "the recorded exit cause must be carried into the reason: {}",
            s.reason
        );
        assert!(
            !s.reason.contains("attention/list"),
            "the OUTBOUND half is fine and must not be blamed: {}",
            s.reason
        );
        // The gateway flag and the outbound stamps are untouched: the row is
        // connected, pushing fine, AND degraded, all at once.
        assert!(s.connected);
        assert_eq!(s.last_attention_poll_at, Some(now - 5_000));
        assert_eq!(s.inbound_expected, 1);
        assert_eq!(s.inbound_live, 0);
        assert!(s.last_inbound_error.is_some());
    }

    #[test]
    fn one_dead_channel_of_three_degrades_without_claiming_the_gateway_is_gone() {
        // Two channels still relay, so the phone is not cut off, but a silent
        // partial loss is exactly how a channel stays dead for a week.
        let now = 1_000_000;
        let mut h = bridge_hb(now - 3_600_000, now - 1_000, true, Some(now - 5_000));
        h.inbound_expected = 3;
        h.inbound_live = 2;
        h.last_inbound_error = Some("Slack channel stopped: socket closed".into());
        let s = classify_heartbeat(DaemonKind::Bridge, Some(h), PidCheck::Matched, now);
        assert_eq!(s.state, DaemonState::Degraded);
        assert!(
            s.reason.contains("2/3 chat channels running"),
            "reason: {}",
            s.reason
        );
        assert!(
            !s.reason.contains("dead, so nothing from the phone"),
            "two channels still work; the reason must not claim total loss: {}",
            s.reason
        );
    }

    #[test]
    fn both_halves_broken_names_both_not_just_the_first() {
        // The operator's fix differs per half, so a row that has lost both must
        // say so rather than picking a winner.
        let now = 1_000_000;
        let mut h = bridge_hb(now - 3_600_000, now - 1_000, true, None);
        h.inbound_live = 0;
        h.last_inbound_error = Some("Discord channel stopped: gateway closed".into());
        let s = classify_heartbeat(DaemonKind::Bridge, Some(h), PidCheck::Matched, now);
        assert_eq!(s.state, DaemonState::Degraded);
        assert!(s.reason.contains("INBOUND"), "reason: {}", s.reason);
        assert!(
            s.reason.contains("outbound cannot reach the attention source"),
            "reason: {}",
            s.reason
        );
    }

    #[test]
    fn a_dead_inbound_half_degrades_a_bridge_that_is_still_connecting() {
        // Not-yet-connected must not mask a dead channel behind the benign
        // "running (connecting…)" row (the same trap the delivery verdict had).
        let now = 1_000_000;
        let mut h = bridge_hb(now - 3_600_000, now - 1_000, false, Some(now - 5_000));
        h.inbound_live = 0;
        h.last_inbound_error = Some("Telegram channel stopped: getMe 401".into());
        let s = classify_heartbeat(DaemonKind::Bridge, Some(h), PidCheck::Matched, now);
        assert_eq!(s.state, DaemonState::Degraded);
        assert!(s.reason.contains("INBOUND"), "reason: {}", s.reason);
        assert!(s.reason.contains("401"), "reason: {}", s.reason);
    }

    #[test]
    fn a_legacy_heartbeat_that_makes_no_inbound_claim_is_not_degraded() {
        // Back-compat: a bridge.json written before the inbound accounting has
        // `inbound_expected == 0`. That is "nothing to judge", NOT "every
        // channel died", otherwise every upgraded bridge would flip to
        // degraded on first read.
        let now = 1_000_000;
        let mut h = bridge_hb(now - 3_600_000, now - 1_000, true, Some(now - 5_000));
        h.inbound_expected = 0;
        h.inbound_live = 0;
        let s = classify_heartbeat(DaemonKind::Bridge, Some(h), PidCheck::Matched, now);
        assert_eq!(s.state, DaemonState::Running);
        assert_eq!(s.reason, "running + connected (outbound push live)");
    }

    #[test]
    fn a_non_bridge_daemon_is_never_judged_on_inbound_channels() {
        // Only the bridge runs chat channels. The fleet daemon declaring none
        // must never be read as having lost them.
        let now = 1_000_000;
        let mut h = hb(42, now - 3_600_000, now - 1_000, true);
        h.inbound_expected = 2;
        h.inbound_live = 0;
        h.last_inbound_error = Some("not a bridge signal".into());
        let s = classify_heartbeat(DaemonKind::FleetDaemon, Some(h), PidCheck::Matched, now);
        assert_eq!(s.state, DaemonState::Running);
        assert_eq!(s.reason, "running + connected");
    }

    #[test]
    fn a_dead_inbound_half_still_reads_stopped_when_the_pid_is_gone() {
        // Precedence guard, mirroring the outbound one: a crashed process is a
        // stronger signal than a dead channel inside a live process.
        let now = 1_000_000;
        let mut h = bridge_hb(now - 3_600_000, now - 1_000, true, Some(now - 5_000));
        h.inbound_live = 0;
        let s = classify_heartbeat(DaemonKind::Bridge, Some(h), PidCheck::Dead, now);
        assert_eq!(s.state, DaemonState::Stopped);
        assert!(s.reason.contains("crashed"), "reason: {}", s.reason);
        assert_eq!(s.inbound_live, 0, "the counters still ride the row");
    }

    #[test]
    fn probe_reads_a_dead_inbound_half_off_disk() {
        // End-to-end through the on-disk record the bridge actually writes.
        let home = TempDir::new().unwrap();
        let started = super::super::heartbeat::process_start_ms(std::process::id())
            .expect("self process start time readable");
        let now = started + 3_600_000;
        let mut h = DaemonHeartbeat::starting();
        h.pid = std::process::id();
        h.started_at = started;
        h.last_heartbeat_at = now - 1_000;
        h.connected = true;
        h.channel = Some("Discord (gateway)".into());
        h.last_attention_poll_at = Some(now - 5_000);
        h.set_inbound_expected(1);
        h.record_inbound_exit("Discord channel stopped: gateway closed");
        // set_inbound_expected/record_inbound_exit stamp the liveness clock, so
        // restore the intended read instant before writing.
        h.last_heartbeat_at = now - 1_000;
        h.write_in(home.path(), DaemonKind::Bridge.id()).unwrap();

        let s = probe_heartbeat_daemon(home.path(), DaemonKind::Bridge, now);
        assert_eq!(
            s.state,
            DaemonState::Degraded,
            "the dead-inbound record must degrade off disk: {}",
            s.reason
        );
        assert!(s.reason.contains("INBOUND"), "reason: {}", s.reason);
        assert!(s.reason.contains("gateway closed"), "reason: {}", s.reason);
    }

    #[test]
    fn bridge_that_polled_fine_but_could_not_deliver_is_degraded() {
        // The P1 shape: the attention poll is fresh (the bridge can READ the
        // fleet's asks) and the chat gateway is connected, but the channel send
        // failed, so the human got nothing. Judging on the poll alone rendered
        // this exact heartbeat as "running + connected (outbound push live)".
        let now = 1_000_000;
        let mut h = bridge_hb(now - 3_600_000, now - 1_000, true, Some(now - 5_000));
        h.last_delivery_error = Some("outbound push: Discord: HTTP 429 rate limited".into());
        let s = classify_heartbeat(DaemonKind::Bridge, Some(h), PidCheck::Matched, now);
        assert_eq!(s.state, DaemonState::Degraded);
        assert!(!s.state.is_healthy());
        assert!(
            s.reason.contains("did not reach the human"),
            "the reason must say the human missed it: {}",
            s.reason
        );
        assert!(
            s.reason.contains("HTTP 429"),
            "the reason must name the channel failure: {}",
            s.reason
        );
        // Connected AND degraded: the inbound gateway is genuinely fine.
        assert!(s.connected);
    }

    #[test]
    fn a_delivery_failure_degrades_a_bridge_that_is_still_connecting() {
        // Not-yet-connected must not mask an undelivered push behind the
        // benign "running (connecting…)" row.
        let now = 1_000_000;
        let mut h = bridge_hb(now - 3_600_000, now - 1_000, false, Some(now - 5_000));
        h.last_delivery_error = Some("outbound push: Telegram: HTTP 401".into());
        let s = classify_heartbeat(DaemonKind::Bridge, Some(h), PidCheck::Matched, now);
        assert_eq!(s.state, DaemonState::Degraded);
        assert!(s.reason.contains("HTTP 401"), "reason: {}", s.reason);
    }

    #[test]
    fn a_cleared_delivery_verdict_lets_the_bridge_go_green_again() {
        let now = 1_000_000;
        let h = bridge_hb(now - 3_600_000, now - 1_000, true, Some(now - 5_000));
        assert!(h.last_delivery_error.is_none());
        let s = classify_heartbeat(DaemonKind::Bridge, Some(h), PidCheck::Matched, now);
        assert_eq!(s.state, DaemonState::Running);
        assert_eq!(
            s.reason,
            "running + connected (inbound 1/1 chat channels running, outbound push live)"
        );
    }

    #[test]
    fn a_delivery_failure_on_a_non_bridge_daemon_is_not_judged() {
        // Only the bridge owes the human a proactive push.
        let now = 1_000_000;
        let mut h = hb(42, now - 3_600_000, now - 1_000, true);
        h.last_delivery_error = Some("outbound push: whatever".into());
        let s = classify_heartbeat(DaemonKind::FleetDaemon, Some(h), PidCheck::Matched, now);
        assert_eq!(s.state, DaemonState::Running);
    }

    #[test]
    fn bridge_whose_last_attention_poll_went_stale_is_degraded() {
        let _snapshot_guard = pin_default_snapshot();
        let now = 1_000_000;
        let last_poll = now - (ATTENTION_STALE_AFTER_MS + 60_000);
        let mut h = bridge_hb(now - 3_600_000, now - 1_000, true, Some(last_poll));
        h.last_attention_error = Some("connect /home/.agents-in-a-box/hangar.sock: refused".into());
        let s = classify_heartbeat(DaemonKind::Bridge, Some(h), PidCheck::Matched, now);
        assert_eq!(s.state, DaemonState::Degraded);
        assert!(
            s.reason.contains("last successful attention/list poll 105s ago"),
            "reason must quantify the gap: {}",
            s.reason
        );
        assert!(
            s.reason.contains("hangar.sock"),
            "the recorded cause must be carried into the reason: {}",
            s.reason
        );
        assert_eq!(
            s.last_attention_error.as_deref(),
            Some("connect /home/.agents-in-a-box/hangar.sock: refused")
        );
    }

    #[test]
    fn bridge_poll_exactly_at_the_window_edge_is_still_running() {
        let _snapshot_guard = pin_default_snapshot();
        let now = 1_000_000;
        let s = classify_heartbeat(
            DaemonKind::Bridge,
            Some(bridge_hb(
                now - 3_600_000,
                now - 1_000,
                true,
                Some(now - ATTENTION_STALE_AFTER_MS),
            )),
            PidCheck::Matched,
            now,
        );
        assert_eq!(
            s.state,
            DaemonState::Running,
            "age == the window is not yet stale, so a punctual poll never flaps"
        );
    }

    #[test]
    fn freshly_started_bridge_gets_one_window_before_degrading() {
        let _snapshot_guard = pin_default_snapshot();
        let now = 1_000_000;
        // Up for 10s: the worker has not had its first poll yet. Degrading here
        // would make every bridge restart look broken for a minute.
        let s = classify_heartbeat(
            DaemonKind::Bridge,
            Some(bridge_hb(now - 10_000, now - 1_000, true, None)),
            PidCheck::Matched,
            now,
        );
        assert_eq!(s.state, DaemonState::Running);
        assert!(s.reason.contains("starting"), "reason: {}", s.reason);

        // One tick past the window with still no poll → degraded.
        let s = classify_heartbeat(
            DaemonKind::Bridge,
            Some(bridge_hb(
                now - (ATTENTION_STALE_AFTER_MS + 1_000),
                now - 1_000,
                true,
                None,
            )),
            PidCheck::Matched,
            now,
        );
        assert_eq!(s.state, DaemonState::Degraded);
        assert!(
            s.reason.contains("no successful attention/list poll in 46s of uptime"),
            "reason: {}",
            s.reason
        );
    }

    #[test]
    fn non_bridge_daemons_are_never_judged_on_outbound_push() {
        // Only the bridge owes the human a proactive push. The fleet daemon has
        // no outbound worker, so a `None` poll time must not degrade it.
        let now = 1_000_000;
        let s = classify_heartbeat(
            DaemonKind::FleetDaemon,
            Some(hb(42, now - 3_600_000, now - 1_000, true)),
            PidCheck::Matched,
            now,
        );
        assert_eq!(s.state, DaemonState::Running);
        assert_eq!(s.reason, "running + connected");
    }

    #[test]
    fn a_degraded_bridge_is_still_degraded_not_stopped_when_the_pid_is_gone() {
        // Precedence guard: a dead process is a STRONGER signal than a broken
        // outbound worker. A crashed bridge must still read as stopped/crashed.
        let now = 1_000_000;
        let s = classify_heartbeat(
            DaemonKind::Bridge,
            Some(bridge_hb(now - 3_600_000, now - 1_000, true, None)),
            PidCheck::Dead,
            now,
        );
        assert_eq!(s.state, DaemonState::Stopped);
        assert!(s.reason.contains("crashed"), "reason: {}", s.reason);
    }

    #[test]
    fn probe_reads_the_live_bridge_shape_and_degrades_it() {
        // The exact on-disk record the live bridge was writing: connected to the
        // Discord gateway, zero errors, no attention keys at all. This is the
        // shape that rendered as fully healthy.
        let home = TempDir::new().unwrap();
        let started = super::super::heartbeat::process_start_ms(std::process::id())
            .expect("self process start time readable");
        // Read the row an hour into this process's life so the uptime is
        // deterministic regardless of how long the test binary has been running.
        let now = started + 3_600_000;
        let mut h = DaemonHeartbeat::starting();
        h.pid = std::process::id();
        h.started_at = started;
        h.last_heartbeat_at = now - 1_000;
        h.connected = true;
        h.channel = Some("Discord (gateway)".into());
        h.write_in(home.path(), DaemonKind::Bridge.id()).unwrap();

        let s = probe_heartbeat_daemon(home.path(), DaemonKind::Bridge, now);
        assert_eq!(
            s.state,
            DaemonState::Degraded,
            "the live bridge shape must degrade, not report healthy: {}",
            s.reason
        );
        assert!(s.connected, "the gateway connection is unchanged");
        assert!(s.reason.contains("attention/list"), "reason: {}", s.reason);
    }

    #[test]
    fn fresh_heartbeat_not_yet_connected_is_running_connecting() {
        let now = 1_000_000;
        let s = classify_heartbeat(
            DaemonKind::Bridge,
            Some(hb(42, now - 5000, now - 1000, false)),
            PidCheck::Matched,
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
            PidCheck::Dead,
            now,
        );
        assert_eq!(s.state, DaemonState::Stopped);
        assert!(s.reason.contains("stale heartbeat"));
        assert!(s.reason.contains("crashed"));
        assert!(!s.connected, "a crashed daemon must not report connected");
    }

    #[test]
    fn recycled_pid_is_stale_even_with_recent_beat_and_live_pid() {
        // H1 regression: the writing daemon died, the OS recycled its pid, and a
        // DIFFERENT live process now owns it. A bare liveness check would say
        // "running"; the identity cross-check must say Stopped (recycled).
        let now = 1_000_000;
        let s = classify_heartbeat(
            DaemonKind::Bridge,
            Some(hb(42, now - 5000, now - 100, true)),
            PidCheck::Recycled,
            now,
        );
        assert_eq!(
            s.state,
            DaemonState::Stopped,
            "a recycled pid (live but not our process) must never report Running"
        );
        assert!(s.reason.contains("recycled"), "reason: {}", s.reason);
        assert!(s.reason.contains("crashed"), "reason: {}", s.reason);
        assert!(
            !s.connected,
            "a recycled-pid daemon must not report connected"
        );
    }

    #[test]
    fn wedged_daemon_live_pid_but_old_beat_is_stale() {
        let _snapshot_guard = pin_default_snapshot();
        let now = 1_000_000;
        let s = classify_heartbeat(
            DaemonKind::FleetDaemon,
            Some(hb(42, now - 200_000, now - (STALE_AFTER_MS + 5000), true)),
            PidCheck::Matched,
            now,
        );
        assert_eq!(s.state, DaemonState::Stopped);
        assert!(s.reason.contains("stale heartbeat"));
        assert!(s.reason.contains("wedged"));
    }

    #[test]
    fn beat_exactly_at_window_edge_is_still_running() {
        let _snapshot_guard = pin_default_snapshot();
        let now = 1_000_000;
        // age == STALE_AFTER_MS is NOT > the window, so still running. The
        // outbound poll is fresh so this isolates the liveness edge.
        let s = classify_heartbeat(
            DaemonKind::Bridge,
            Some(bridge_hb(
                now - 100_000,
                now - STALE_AFTER_MS,
                true,
                Some(now - 1_000),
            )),
            PidCheck::Matched,
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
        // The identity cross-check (H1) matches the heartbeat's `started_at`
        // against the live process's OS start time. `starting()` stamps
        // started_at = NOW, but the test process started earlier — so stamp the
        // real OS start time to simulate an honest daemon heartbeat.
        h.started_at = super::super::heartbeat::process_start_ms(std::process::id())
            .expect("self process start time readable");
        h.set_connected(true, Some("Telegram".into()));
        // A healthy bridge is one whose outbound worker is also reaching the
        // attention source; without this stamp the row would (correctly) degrade
        // once the test binary has been alive longer than one poll window.
        h.record_attention_poll();
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
    fn probe_heartbeat_daemon_stale_for_recycled_self_pid() {
        // H1 disk-backed regression: a heartbeat for a LIVE pid (this process)
        // whose `started_at` does NOT match the process's real start time is the
        // pid-recycle signature — a dead daemon whose pid the OS handed to a
        // different, live process. It must classify Stopped (recycled), never
        // Running, even though `kill(pid,0)` succeeds.
        let home = TempDir::new().unwrap();
        let mut h = DaemonHeartbeat::starting(); // pid = self (alive)
        let now = super::super::heartbeat::now_ms();
        // started_at one hour ago — far outside the identity tolerance vs the
        // real process start, but a recent beat so staleness alone wouldn't trip.
        h.started_at = now - 3_600_000;
        h.last_heartbeat_at = now - 1_000;
        h.set_connected(true, Some("Telegram".into()));
        h.write_in(home.path(), DaemonKind::Bridge.id()).unwrap();
        let s = probe_heartbeat_daemon(home.path(), DaemonKind::Bridge, now);
        assert_eq!(
            s.state,
            DaemonState::Stopped,
            "live-but-mismatched pid must be Stopped (recycled), not Running"
        );
        assert!(s.reason.contains("recycled"), "reason: {}", s.reason);
        assert!(!s.connected);
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
        // L1: connected requires an ACTUAL bound listener, not just a socket
        // file. Bind one and keep it alive for the duration of the probe.
        let _listener =
            std::os::unix::net::UnixListener::bind(base.path().join("notify.sock")).unwrap();
        std::fs::write(base.path().join("notifications.db"), b"").unwrap();
        let s = probe_notifyd(base.path(), super::super::heartbeat::now_ms());
        assert_eq!(s.state, DaemonState::Running);
        assert!(s.connected);
        assert!(s.reason.contains("socket bound"));
    }

    #[test]
    fn probe_notifyd_stale_socket_file_without_listener_is_not_connected() {
        // L1 regression: a crashed daemon left a stale `notify.sock` FILE but no
        // listener is bound. `exists()` would falsely report connected; a
        // connect() refuses, so we must report running-but-not-connected.
        let base = TempDir::new().unwrap();
        std::fs::write(
            base.path().join("notify.pid"),
            format!("{}\n", std::process::id()),
        )
        .unwrap();
        // A plain file at the socket path — present but NOT a listener.
        std::fs::write(base.path().join("notify.sock"), b"").unwrap();
        std::fs::write(base.path().join("notifications.db"), b"").unwrap();
        let s = probe_notifyd(base.path(), super::super::heartbeat::now_ms());
        assert_eq!(s.state, DaemonState::Running);
        assert!(
            !s.connected,
            "a stale socket file with no listener must not report connected"
        );
        assert!(
            s.reason.contains("socket not bound"),
            "reason: {}",
            s.reason
        );
    }

    #[test]
    fn probe_approve_broker_no_socket_is_stopped() {
        let base = TempDir::new().unwrap();
        let s = probe_approve_broker(base.path(), super::super::heartbeat::now_ms());
        assert_eq!(s.state, DaemonState::Stopped);
        assert!(!s.connected);
        assert!(s.reason.contains("no approve.sock"), "reason: {}", s.reason);
    }

    #[test]
    fn probe_approve_broker_stale_socket_file_is_stopped() {
        // A crashed notifyd left an `approve.sock` FILE with no listener. `exists()`
        // alone would falsely report it up; connect() refuses, so → Stopped.
        let base = TempDir::new().unwrap();
        std::fs::write(base.path().join("approve.sock"), b"").unwrap();
        let s = probe_approve_broker(base.path(), super::super::heartbeat::now_ms());
        assert_eq!(s.state, DaemonState::Stopped);
        assert!(!s.connected);
        assert!(
            s.reason.contains("stale approve.sock"),
            "reason: {}",
            s.reason
        );
    }

    #[test]
    fn probe_approve_broker_bound_listener_is_running_connected() {
        use std::io::{BufRead, BufReader, Write};
        let base = TempDir::new().unwrap();
        let listener =
            std::os::unix::net::UnixListener::bind(base.path().join("approve.sock")).unwrap();
        // Minimal broker stand-in: answer each `list` RPC with an empty pending
        // array so `client_list` returns fast (else it waits out the RPC timeout).
        // Detached — it blocks on accept and dies at process exit; the probe makes
        // only a couple of connects, so joining would hang on the next accept.
        std::thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let mut reader = BufReader::new(conn.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) > 0 {
                    let mut w = conn;
                    let _ = w.write_all(b"{\"pending\":[]}\n");
                    let _ = w.flush();
                }
            }
        });
        let s = probe_approve_broker(base.path(), super::super::heartbeat::now_ms());
        assert_eq!(s.state, DaemonState::Running);
        assert!(s.connected, "a bound listener must report connected");
        assert_eq!(s.channel.as_deref(), Some("approve socket"));
        assert!(
            s.reason.contains("no pending requests"),
            "empty pending list should read as no pending: {}",
            s.reason
        );
    }

    #[test]
    fn probe_approve_broker_pending_reason_names_the_command_that_lists_them() {
        // The count alone is what made the queue feel un-inspectable: the
        // operator sees "1 pending request" and has nowhere to go. Any non-zero
        // row MUST name the verb that renders the queue.
        use std::io::{BufRead, BufReader, Write};
        for (count, body) in [
            (
                1usize,
                r#"{"pending":[{"session_id":"s1","tool":"Bash","context":"{}","waiting_ms":4200}]}"#,
            ),
            (
                2usize,
                r#"{"pending":[{"session_id":"s1","tool":"Bash","context":"{}","waiting_ms":4200},{"session_id":"s2","tool":"Write","context":"{}","waiting_ms":900}]}"#,
            ),
        ] {
            let base = TempDir::new().unwrap();
            let listener =
                std::os::unix::net::UnixListener::bind(base.path().join("approve.sock")).unwrap();
            let body = body.to_string();
            std::thread::spawn(move || {
                for conn in listener.incoming().flatten() {
                    let mut reader = BufReader::new(conn.try_clone().unwrap());
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) > 0 {
                        let mut w = conn;
                        let _ = w.write_all(body.as_bytes());
                        let _ = w.write_all(b"\n");
                        let _ = w.flush();
                    }
                }
            });
            let s = probe_approve_broker(base.path(), super::super::heartbeat::now_ms());
            assert_eq!(s.state, DaemonState::Running);
            assert!(
                s.reason.contains("ainb fleet approve"),
                "a pending count must name the listing command ({count} pending): {}",
                s.reason
            );
        }
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
    fn probe_atc_heartbeat_disabled_instance_is_stopped_not_running() {
        // M2 regression: an instance with `heartbeat_enabled:false` but a very
        // recent `last_heartbeat_ms` must report Stopped — the timer is off, so
        // a leftover recent beat must not be counted as running.
        let home = TempDir::new().unwrap();
        let atc_dir = home.path().join("atc").join("primary");
        std::fs::create_dir_all(&atc_dir).unwrap();
        std::fs::write(
            atc_dir.join("meta.json"),
            r#"{"name":"primary","heartbeat_enabled":false,"heartbeat_interval_min":15,"idle_pause_min":60}"#,
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
        assert_eq!(
            s.state,
            DaemonState::Stopped,
            "a heartbeat-disabled ATC instance must never report Running"
        );
        assert!(
            s.reason.contains("disabled"),
            "reason should explain the disable: {}",
            s.reason
        );
    }

    #[test]
    fn probe_atc_disabled_does_not_mask_an_enabled_running_sibling() {
        // Defense for the multi-instance case: a disabled instance with a recent
        // beat sits next to an ENABLED instance that is genuinely beating. The
        // probe must report Running from the enabled one, not be confused by the
        // disabled sibling.
        let home = TempDir::new().unwrap();
        let now = super::super::heartbeat::now_ms();
        for (name, enabled) in [("off", false), ("on", true)] {
            let dir = home.path().join("atc").join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("meta.json"),
                format!(
                    r#"{{"name":"{name}","heartbeat_enabled":{enabled},"heartbeat_interval_min":15,"idle_pause_min":60}}"#
                ),
            )
            .unwrap();
            std::fs::write(
                dir.join("heartbeat-state.json"),
                format!(
                    r#"{{"last_heartbeat_ms":{},"continue_counts":{{}}}}"#,
                    now - 1000
                ),
            )
            .unwrap();
        }
        let s = probe_atc(home.path(), now);
        assert_eq!(s.state, DaemonState::Running);
        assert!(
            s.channel.as_deref().unwrap().contains("on"),
            "the enabled instance must be the representative row: {:?}",
            s.channel
        );
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
    fn collect_in_returns_all_heartbeat_daemons_in_stable_order() {
        let home = TempDir::new().unwrap();
        let notifyd = TempDir::new().unwrap();
        let rows = collect_in(
            home.path(),
            notifyd.path(),
            super::super::heartbeat::now_ms(),
        );
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0].kind, DaemonKind::Bridge);
        assert_eq!(rows[1].kind, DaemonKind::Notifyd);
        assert_eq!(rows[2].kind, DaemonKind::ApproveBroker);
        assert_eq!(rows[3].kind, DaemonKind::Atc);
        assert_eq!(rows[4].kind, DaemonKind::FleetDaemon);
        // Empty homes → everything stopped, never a false running.
        assert!(rows.iter().all(|r| r.state == DaemonState::Stopped));
    }

    /// The Daemons view is meant to be the ONE place every managed process
    /// shows up. MCP pool, the Hangar daemon and the Headroom proxy used to be
    /// real processes with no row here — visible only as ad-hoc lines in a
    /// separate panel, with no way to act on them.
    /// A `Degraded` bridge is ALIVE — half its job is not happening, but the
    /// process is up. Appending "cannot start" to it is simply false, and it
    /// fires for real: a daemon launched with a baked-in `AINB_CONFIG_PATH`
    /// reads a config this process cannot see.
    #[test]
    fn a_degraded_bridge_is_never_told_it_cannot_start() {
        let mut rows = vec![DaemonStatus {
            state: DaemonState::Degraded,
            reason: "running (connected), but a push did not reach the human".to_string(),
            ..DaemonStatus::stopped(DaemonKind::Bridge, String::new())
        }];
        annotate_bridge_config(&mut rows, Some("no bridge config: it does not exist"));
        assert!(
            !rows[0].reason.contains("cannot start"),
            "a live daemon must not be told it cannot start, got {:?}",
            rows[0].reason
        );
    }

    /// A stopped bridge DOES get the cause appended — that is the whole point.
    #[test]
    fn a_stopped_bridge_gets_the_config_cause() {
        let mut rows = vec![DaemonStatus::stopped(
            DaemonKind::Bridge,
            "no heartbeat".to_string(),
        )];
        annotate_bridge_config(&mut rows, Some("no bridge config: it does not exist"));
        assert!(rows[0].reason.contains("cannot start: no bridge config"));
    }

    #[test]
    fn the_socket_probed_daemons_complete_the_roster() {
        let kinds: Vec<DaemonKind> = collect_socket_daemons().iter().map(|r| r.kind).collect();
        assert_eq!(
            kinds,
            vec![
                DaemonKind::McpPool,
                DaemonKind::HangarDaemon,
                DaemonKind::HeadroomProxy,
                DaemonKind::ReleaseChecker,
            ]
        );
    }

    /// Every kind has a distinct id and display name. A collision would make
    /// two rows indistinguishable in the table and alias their heartbeat files.
    #[test]
    fn every_daemon_kind_has_a_unique_id_and_name() {
        let kinds = [
            DaemonKind::Bridge,
            DaemonKind::Notifyd,
            DaemonKind::ApproveBroker,
            DaemonKind::Atc,
            DaemonKind::FleetDaemon,
            DaemonKind::McpPool,
            DaemonKind::HangarDaemon,
            DaemonKind::HeadroomProxy,
            DaemonKind::ReleaseChecker,
        ];
        let ids: std::collections::HashSet<&str> = kinds.iter().map(|k| k.id()).collect();
        assert_eq!(ids.len(), kinds.len(), "daemon ids must be unique");
        let names: std::collections::HashSet<&str> =
            kinds.iter().map(|k| k.display_name()).collect();
        assert_eq!(names.len(), kinds.len(), "display names must be unique");
    }

    #[test]
    fn collect_in_flips_bridge_to_running_when_heartbeat_present() {
        let home = TempDir::new().unwrap();
        let notifyd = TempDir::new().unwrap();
        let mut h = DaemonHeartbeat::starting();
        // Stamp the real OS start time so the H1 identity cross-check matches
        // (the test process started earlier than `starting()`'s NOW).
        h.started_at = super::super::heartbeat::process_start_ms(std::process::id())
            .expect("self process start time readable");
        h.set_connected(true, Some("Telegram (@bot)".into()));
        h.record_activity();
        // Healthy means BOTH halves work: gateway connected and the outbound
        // worker reaching the attention source.
        h.record_attention_poll();
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

    #[test]
    fn stopped_bridge_row_names_the_config_cause() {
        let mut rows = vec![
            DaemonStatus::stopped(DaemonKind::Bridge, "stale heartbeat — pid 1 not alive"),
            DaemonStatus::stopped(DaemonKind::Notifyd, "not running"),
        ];
        annotate_bridge_config(
            &mut rows,
            Some(
                "/home/u/config.toml: config has no [fleet.bridge] table\n\nconfigure at least ONE channel",
            ),
        );
        assert!(
            rows[0]
                .reason
                .contains("cannot start: /home/u/config.toml: config has no [fleet.bridge] table"),
            "{}",
            rows[0].reason
        );
        // Only the first line — the table cell is not the place for the skeleton.
        assert!(!rows[0].reason.contains("configure at least ONE channel"));
        // Other daemons are untouched.
        assert_eq!(rows[1].reason, "not running");
    }

    #[test]
    fn running_bridge_row_is_not_annotated() {
        let mut rows = vec![DaemonStatus {
            state: DaemonState::Running,
            ..DaemonStatus::stopped(DaemonKind::Bridge, "connected")
        }];
        annotate_bridge_config(&mut rows, Some("some config problem"));
        assert_eq!(rows[0].reason, "connected");
    }

    #[test]
    fn no_config_problem_leaves_every_row_alone() {
        let mut rows = vec![DaemonStatus::stopped(DaemonKind::Bridge, "stale heartbeat")];
        annotate_bridge_config(&mut rows, None);
        assert_eq!(rows[0].reason, "stale heartbeat");
    }
}
