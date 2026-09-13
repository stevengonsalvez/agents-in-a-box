// ABOUTME: Unified daemon observability — one aggregator, two surfaces.
//
// This module answers a single question for every long-running ainb daemon:
// "is it actually running, connected, and doing work — right now?" — not merely
// "is a launchd service installed?". It powers both the `ainb fleet daemons`
// CLI verb (`cli/fleet/daemons.rs`) and the TUI Daemons screen
// (`components/daemons.rs`), which render from the SAME [`collect`] function so
// the two views can never drift.
//
// The mechanism is a lightweight heartbeat file per daemon
// (`heartbeat.rs` → `~/.agents-in-a-box/daemons/<name>.json`), cross-checked
// against real liveness signals (is the `pid` alive? is the socket reachable?):
//
//   * the phone bridge + the fleet auto-continue watcher WRITE a heartbeat
//     (they had no observability at all — the bridge's was the original blind
//     spot this feature closes), and
//   * notifyd + ATC are READ from the signals they already maintain (notifyd's
//     PID file + Unix socket + sqlite DB; ATC's `heartbeat-state.json`), so we
//     don't duplicate state a daemon already owns.
//
// The aggregator's contract is the stale-heartbeat cross-check: a heartbeat
// whose `pid` is no longer alive — or whose `last_heartbeat_at` is older than
// the staleness window — reports `Stopped` (with a "stale heartbeat" reason),
// never a false `Running`. That is what makes a crashed daemon visible.

pub mod heartbeat;
pub mod probe;

pub use heartbeat::{DaemonHeartbeat, is_pid_alive};
pub use probe::{DaemonKind, DaemonState, DaemonStatus, collect, collect_in};

/// Bring long-running daemons onto THIS binary at launch, and clear the
/// heartbeats of ones that are already gone.
///
/// Mirrors `ensure_hangar_daemon`, which has always upgraded an older released
/// hangar daemon on TUI start. notifyd never got the same treatment, so after a
/// `brew upgrade` the old process kept serving until someone noticed the drift
/// column and restarted it by hand.
///
/// Only the LIVE OWNER is considered, and only when its binary differs from
/// this one. Stale owners and orphans are a reap concern, not an upgrade
/// concern, and restarting on anything less than a real drift would cycle a
/// healthy daemon on every launch.
///
/// Deliberately does NOT touch a daemon whose binary matches: a dev build
/// another worktree is deliberately running reports its own path, and cycling
/// it would break that session.
///
/// Best-effort and quiet, like the hangar equivalent: the TUI owns the
/// terminal, and a failed upgrade must never stop it launching.
pub fn ensure_daemons_current() {
    for name in heartbeat::sweep_orphaned() {
        tracing::info!(daemon = %name, "cleared heartbeat for a daemon that is no longer running");
    }

    let drifted = ainb_plugin_notifyd::procs::scan()
        .into_iter()
        .any(|d| matches!(d.class, ainb_plugin_notifyd::DaemonClass::LiveOwner) && d.binary_drift);
    if !drifted {
        return;
    }
    match ainb_plugin_notifyd::procs::restart(std::time::Duration::from_secs(3)) {
        Ok(out) => tracing::info!(
            socket_bound = out.socket_bound,
            "notifyd restarted onto this binary after an upgrade"
        ),
        Err(e) => tracing::warn!(error = %e, "notifyd upgrade failed (TUI continues)"),
    }
}
