//! The host task that keeps section 20 (agent status) current (T0-section, #1015).
//!
//! `ainb-app` reduces; the host owns the socket. This task holds a Fleet
//! subscription, and for every revision it is told about it pays ONE daemon read,
//! the joined `fleet/roster_status`, and hands the reply to the TUI loop, which
//! folds it into section 20 through the section's reducer.
//!
//! ```text
//!  daemon ──fleet/event──▶ task ──Head(rev)──▶ mpsc ──▶ drain_into ──▶ section 20
//!         ◀─roster_status─      ──Read(rows)─▶
//!  refused / gone        ──Failed / Absent──▶ (rows frozen, or absent, and why)
//! ```
//!
//! Failures never become a state: a read that fails freezes the section's rows
//! as unreachable, a daemon without the method leaves it absent, and the task
//! retries with a bounded backoff. It is panic-free, because the TUI's panic
//! handler tears the terminal down.

use std::time::Duration;

use ainb_app::app::state::AppState;
use ainb_app::fleet::bridge::daemon::{DaemonClient, DaemonError, FleetStreamEvent};
use ainb_hangar_proto::agent_status::RosterStatusResult;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// JSON-RPC "method not found": a daemon older than `fleet/roster_status`.
const METHOD_NOT_FOUND: i32 = -32601;
/// First retry wait after a failure.
const BACKOFF_INITIAL: Duration = Duration::from_millis(500);
/// Longest retry wait.
const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// One thing the task learned, for the TUI loop to fold into section 20.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentStatusUpdate {
    /// A joined read landed, received at this local epoch-ms clock.
    Read(RosterStatusResult, i64),
    /// A newer Fleet revision was observed.
    Head(i64),
    /// A read or the subscription failed, for this reason, at this local clock.
    Failed(String, i64),
    /// The daemon cannot serve the joined read.
    Absent(String),
}

/// Resolves a fresh daemon client for every connection attempt.
pub type Dialer = Box<dyn Fn() -> Result<DaemonClient, DaemonError> + Send + Sync>;

/// The running task and the channel its updates arrive on.
pub struct AgentStatusHost {
    updates: mpsc::UnboundedReceiver<AgentStatusUpdate>,
    task: JoinHandle<()>,
}

impl AgentStatusHost {
    /// Start the task. Must be called inside a tokio runtime.
    #[must_use]
    pub fn spawn(dialer: Dialer) -> Self {
        let (tx, updates) = mpsc::unbounded_channel();
        let task = tokio::spawn(run(dialer, tx));
        Self { updates, task }
    }

    /// Fold every update that has arrived into section 20. Returns whether
    /// section 20's version moved.
    pub fn drain_into(&mut self, state: &mut AppState) -> bool {
        let mut changed = false;
        while let Ok(update) = self.updates.try_recv() {
            changed |= apply(state, update);
        }
        changed
    }
}

impl Drop for AgentStatusHost {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Fold one update into section 20 through its reducer entry points.
pub fn apply(state: &mut AppState, update: AgentStatusUpdate) -> bool {
    match update {
        AgentStatusUpdate::Read(read, received_at_ms) => {
            state.apply_agent_status_read(read, received_at_ms)
        }
        AgentStatusUpdate::Head(revision) => state.observe_agent_status_head(revision),
        AgentStatusUpdate::Failed(reason, now_ms) => state.agent_status_read_failed(reason, now_ms),
        AgentStatusUpdate::Absent(reason) => state.agent_status_absent(reason),
    }
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| {
        i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
    })
}

/// Why a connection ended.
enum Ended {
    /// The daemon does not serve the joined read; wait long before asking again.
    Absent,
    /// Anything else: retry on the ordinary backoff.
    Failed,
    /// The update channel is gone: the TUI quit.
    Closed,
}

async fn run(dialer: Dialer, tx: mpsc::UnboundedSender<AgentStatusUpdate>) {
    let mut backoff = BACKOFF_INITIAL;
    loop {
        let ended = match dialer() {
            Ok(client) => serve(&client, &tx, &mut backoff).await,
            Err(error) => {
                if tx.send(AgentStatusUpdate::Failed(error.to_string(), now_ms())).is_err() {
                    Ended::Closed
                } else {
                    Ended::Failed
                }
            }
        };
        let wait = match ended {
            Ended::Closed => return,
            Ended::Absent => BACKOFF_MAX,
            Ended::Failed => backoff,
        };
        tokio::time::sleep(wait).await;
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

/// One connection: read, subscribe, and read once per revision until it ends.
async fn serve(
    client: &DaemonClient,
    tx: &mpsc::UnboundedSender<AgentStatusUpdate>,
    backoff: &mut Duration,
) -> Ended {
    let first = match read(client, tx).await {
        Ok(read_revision) => read_revision,
        Err(ended) => return ended,
    };
    let (_seed, mut subscription) = match client.open_fleet_subscription(first).await {
        Ok(opened) => opened,
        Err(error) => return report(tx, &error),
    };
    *backoff = BACKOFF_INITIAL;
    loop {
        match subscription.next_event().await {
            Ok(FleetStreamEvent::Revision(event)) => {
                if tx.send(AgentStatusUpdate::Head(event.revision)).is_err() {
                    return Ended::Closed;
                }
                if let Err(ended) = read(client, tx).await {
                    return ended;
                }
            }
            Ok(FleetStreamEvent::ResyncRequired) => {
                if let Err(ended) = read(client, tx).await {
                    return ended;
                }
            }
            Err(error) => return report(tx, &error),
        }
    }
}

/// One joined read: sends the reply and returns its revision, or sends the
/// failure and returns why the connection ends.
async fn read(
    client: &DaemonClient,
    tx: &mpsc::UnboundedSender<AgentStatusUpdate>,
) -> Result<i64, Ended> {
    match client.fleet_roster_status().await {
        Ok(result) => {
            let revision = result.read_revision;
            tx.send(AgentStatusUpdate::Read(result, now_ms())).map_err(|_| Ended::Closed)?;
            Ok(revision)
        }
        Err(error) => Err(report(tx, &error)),
    }
}

/// Send a failure update and say how the connection ends.
fn report(tx: &mpsc::UnboundedSender<AgentStatusUpdate>, error: &DaemonError) -> Ended {
    let (update, ended) = match error {
        DaemonError::Rpc { code, .. } if *code == METHOD_NOT_FOUND => (
            AgentStatusUpdate::Absent("daemon has no fleet/roster_status".to_string()),
            Ended::Absent,
        ),
        other => (
            AgentStatusUpdate::Failed(other.to_string(), now_ms()),
            Ended::Failed,
        ),
    };
    if tx.send(update).is_err() {
        Ended::Closed
    } else {
        ended
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The loop folds updates through section 20's reducers, and the version
    /// moves only for a change.
    #[test]
    fn updates_fold_into_section_20_and_only_changes_bump_it() {
        let mut state = AppState::default();
        let before = state.agent_status.version();
        assert!(apply(
            &mut state,
            AgentStatusUpdate::Absent("daemon has no fleet/roster_status".into())
        ));
        assert!(!apply(
            &mut state,
            AgentStatusUpdate::Absent("daemon has no fleet/roster_status".into())
        ));
        assert_eq!(state.agent_status.version(), before + 1);
        assert_eq!(
            state.agent_status.absent.as_deref(),
            Some("daemon has no fleet/roster_status")
        );

        let empty = RosterStatusResult {
            rows: Vec::new(),
            read_revision: 4,
            unknown_events: Vec::new(),
        };
        assert!(apply(
            &mut state,
            AgentStatusUpdate::Read(empty.clone(), 10)
        ));
        assert!(!apply(
            &mut state,
            AgentStatusUpdate::Read(
                RosterStatusResult {
                    read_revision: 5,
                    ..empty
                },
                11
            )
        ));
        assert!(
            apply(&mut state, AgentStatusUpdate::Head(9)),
            "a newer head makes it stale"
        );
        assert!(apply(
            &mut state,
            AgentStatusUpdate::Failed("connection refused".into(), 12)
        ));
    }
}
