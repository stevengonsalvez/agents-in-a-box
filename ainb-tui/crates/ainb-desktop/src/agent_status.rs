//! The desktop's reader for section 20 (agent status), which the board draws.
//!
//! `ainb-app` reduces and the host owns the socket, so the host pays the read:
//! one `fleet/roster_status` at a time on a worker thread, reported into an
//! inbox the host's tick folds through the section's own reducer.
//!
//! ```text
//!  tick ──due, none in flight──▶ worker ──fleet/roster_status──▶ daemon
//!    ▲                              │
//!    └──── one outcome per tick ◀───┘ Read(rows) | Failed(why)
//! ```
//!
//! A failed read freezes the rows as unreachable, never a state of its own.
//! The terminal has its own reader (`ainb-core`'s agent status host), which
//! follows the daemon's revisions; this one polls, which is enough for a board
//! a person reads.

use std::sync::{Arc, Mutex, PoisonError};

use ainb_app::AppState;
use ainb_hangar_proto::agent_status::RosterStatusResult;

/// How often a read is asked for, from the end of the last one.
pub const POLL_MS: i64 = 1_000;

/// What a read worker reported.
#[derive(Debug)]
pub enum StatusOutcome {
    /// The daemon answered.
    Read(RosterStatusResult),
    /// It did not, in the daemon client's words.
    Failed(String),
}

/// The inbox a read worker reports through.
pub type Reports = Arc<Mutex<Vec<StatusOutcome>>>;

/// Keeps section 20 current: at most one read in flight, one outcome folded
/// per tick.
#[derive(Debug, Default)]
pub struct AgentStatusPoll {
    /// Set while the sidecar has no daemon: nothing is read, and a read that
    /// lands is dropped rather than shown as current.
    down: bool,
    in_flight: bool,
    last_poll_ms: Option<i64>,
    inbox: Reports,
}

impl AgentStatusPoll {
    /// The channel a read worker reports through. A host test holds it to
    /// stand in for a worker.
    #[must_use]
    pub fn reports(&self) -> Reports {
        Arc::clone(&self.inbox)
    }

    /// The daemon is gone: mark the rows unreachable, and read nothing until
    /// [`Self::daemon_connected`].
    pub fn daemon_lost(&mut self, state: &mut AppState, reason: &str, now_ms: i64) {
        self.down = true;
        state.agent_status_read_failed(format!("the daemon is unreachable: {reason}"), now_ms);
    }

    /// The daemon is back: read on the next tick rather than a poll later.
    pub fn daemon_connected(&mut self) {
        self.down = false;
        self.last_poll_ms = None;
    }

    /// Fold what the worker reported into `state`, and ask for the next read
    /// when one is due. `read` starts it; the host passes the real one.
    pub fn tick(&mut self, state: &mut AppState, now_ms: i64, read: impl FnOnce(Reports)) {
        let landed = {
            let mut inbox = self.inbox.lock().unwrap_or_else(PoisonError::into_inner);
            (!inbox.is_empty()).then(|| inbox.remove(0))
        };
        if self.down {
            // A read started before the daemon went reads it as it was.
            if landed.is_some() {
                self.in_flight = false;
            }
            return;
        }
        if let Some(outcome) = landed {
            self.in_flight = false;
            self.last_poll_ms = Some(now_ms);
            match outcome {
                StatusOutcome::Read(rows) => {
                    state.apply_agent_status_read(rows, now_ms);
                }
                StatusOutcome::Failed(reason) => {
                    state.agent_status_read_failed(reason, now_ms);
                }
            }
        }
        let due = self.last_poll_ms.is_none_or(|last| now_ms - last >= POLL_MS);
        if due && !self.in_flight {
            self.in_flight = true;
            read(Arc::clone(&self.inbox));
        }
    }
}

/// Put `outcome` in `inbox`, through a poisoned lock as `tick` reads it: a
/// dropped report would leave the read in flight for good.
fn report(inbox: &Mutex<Vec<StatusOutcome>>, outcome: StatusOutcome) {
    inbox.lock().unwrap_or_else(PoisonError::into_inner).push(outcome);
}

/// Read the roster on a worker thread and report it into `inbox`.
pub fn read_on_worker(inbox: Reports) {
    let worker_inbox = Arc::clone(&inbox);
    let spawned = std::thread::Builder::new()
        .name("ainb-agent-status".into())
        .spawn(move || report(&worker_inbox, read_once()));
    // A read that never starts must still land, or it stays in flight and the
    // board is never read again.
    if let Err(error) = spawned {
        report(
            &inbox,
            StatusOutcome::Failed(format!("the agent status read could not start: {error}")),
        );
    }
}

fn read_once() -> StatusOutcome {
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
        return StatusOutcome::Failed("no runtime for the agent status read".to_string());
    };
    runtime.block_on(async {
        // As the desktop: each read is a connection, and the daemon's surface
        // trail must not show a terminal this process never ran.
        let client = ainb_app::fleet::bridge::daemon::surface_client(
            ainb_hangar_proto::connections::SurfaceKind::Desktop,
        );
        match client {
            Ok(client) => match client.fleet_roster_status().await {
                Ok(rows) => StatusOutcome::Read(rows),
                Err(error) => StatusOutcome::Failed(format!("fleet/roster_status: {error}")),
            },
            Err(error) => {
                StatusOutcome::Failed(format!("fleet/roster_status unavailable: {error}"))
            }
        }
    })
}
