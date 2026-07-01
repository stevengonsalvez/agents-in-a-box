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
