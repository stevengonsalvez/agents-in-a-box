//! The usage reader: the task that keeps section 21 (usage) current from the
//! daemon's `fleet/usage_summary` (D3p-e).
//!
//! `ainb-app` reduces and the host owns the socket, so the host spawns this
//! task and folds what it reports on its own loop, as it does the agent status
//! reader. The counters have one producer, the daemon's usage projection, and
//! this task only carries its reply: it reads the trailing thirty days, on the
//! cadence the daemon refreshes at, since asking faster returns the same bytes.
//!
//! ```text
//!  daemon ──usage_summary──▶ task ──Read(reply)──▶ mpsc ──▶ drain_into ──▶ section 21
//!  no fleet.usage.read      ──Absent(reason)──▶
//!  read or dial failed      ──Failed(reason)──▶ (numbers kept, and why)
//! ```
//!
//! A summary still `scanning` is asked again on a short cadence, so the first
//! numbers appear once the daemon's first scan lands rather than fifteen
//! minutes later. Failures never become a state: a failed read keeps the last
//! numbers and says why, and the task retries on a bounded backoff. It is
//! panic-free, because the TUI's panic handler tears the terminal down.

use std::time::Duration;

use ainb_hangar_proto::fleet::{
    FLEET_CAPABILITY_USAGE_READ, FleetUsagePeriod, FleetUsageSummaryParams,
    FleetUsageSummaryResult, FleetUsageSummaryState,
};
use ainb_hangar_proto::methods::FLEET_USAGE_SUMMARY;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::app::state::AppState;
use crate::fleet::agent_status_reader::Dialer;
use crate::fleet::bridge::daemon::{DaemonClient, DaemonError};

/// JSON-RPC "method not found": a daemon older than the read.
const METHOD_NOT_FOUND: i32 = -32601;

/// The absent reason when the daemon does not advertise the capability.
pub const NOT_SERVED: &str = "the daemon does not serve fleet.usage.read";

/// The failure reason section 21 renders once the reader task has died.
pub const READER_STOPPED: &str = "usage reader stopped";

/// How often the task reads. Shortened only by the tests.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// Between reads of a ready, partial or unavailable summary: the daemon's
    /// own refresh (`fleet_usage.rs`, fifteen minutes).
    pub ready_every: Duration,
    /// Between reads while the daemon is still scanning.
    pub scanning_every: Duration,
    /// First retry wait after a failure.
    pub backoff_initial: Duration,
    /// Longest retry wait.
    pub backoff_max: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            ready_every: Duration::from_mins(15),
            scanning_every: Duration::from_secs(10),
            backoff_initial: Duration::from_millis(500),
            backoff_max: Duration::from_mins(1),
        }
    }
}

/// One thing the task learned, for the host loop to fold into section 21.
#[derive(Debug, Clone, PartialEq)]
pub enum UsageUpdate {
    /// A reply landed, received at this local epoch-ms clock. Boxed: the
    /// reply is the one large variant.
    Read(Box<FleetUsageSummaryResult>, i64),
    /// A read or the dial failed, for this reason.
    Failed(String),
    /// The daemon cannot serve the read.
    Absent(String),
}

/// The running reader task and the channel its updates arrive on. Dropping it
/// stops the task.
pub struct UsageReader {
    updates: mpsc::UnboundedReceiver<UsageUpdate>,
    task: JoinHandle<()>,
    /// The task was found finished and section 21 was told so.
    stop_reported: bool,
}

impl UsageReader {
    /// Start the task. Must be called inside a tokio runtime.
    #[must_use]
    pub fn spawn(dialer: Dialer) -> Self {
        Self::spawn_timed(dialer, Timing::default())
    }

    /// [`Self::spawn`] on a cadence of the caller's. For tests.
    #[must_use]
    pub fn spawn_timed(dialer: Dialer, timing: Timing) -> Self {
        let (tx, updates) = mpsc::unbounded_channel();
        let task = tokio::spawn(run(dialer, tx, timing));
        Self {
            updates,
            task,
            stop_reported: false,
        }
    }

    /// Fold every update that has arrived into section 21. Returns whether
    /// section 21's version moved.
    ///
    /// Supervises the task too: it ends only when this handle drops, so a
    /// finished task means it died, and section 21 says so rather than
    /// presenting its last numbers as current forever.
    pub fn drain_into(&mut self, state: &mut AppState) -> bool {
        let mut changed = false;
        while let Ok(update) = self.updates.try_recv() {
            changed |= apply(state, update);
        }
        if !self.stop_reported && self.task.is_finished() {
            self.stop_reported = true;
            tracing::warn!("usage: the reader task stopped");
            changed |= state.usage_read_failed(READER_STOPPED);
        }
        changed
    }
}

impl Drop for UsageReader {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Fold one update into section 21 through its reducer entry points.
pub fn apply(state: &mut AppState, update: UsageUpdate) -> bool {
    match update {
        UsageUpdate::Read(reply, received_at_ms) => state.apply_usage_read(*reply, received_at_ms),
        UsageUpdate::Failed(reason) => state.usage_read_failed(reason),
        UsageUpdate::Absent(reason) => state.usage_absent(reason),
    }
}

async fn run(dialer: Dialer, tx: mpsc::UnboundedSender<UsageUpdate>, timing: Timing) {
    let mut backoff = timing.backoff_initial;
    loop {
        let (update, wait) = match dialer() {
            Ok(client) => read(&client, timing).await,
            Err(error) => (UsageUpdate::Failed(error.to_string()), None),
        };
        let wait = wait.unwrap_or_else(|| {
            let wait = backoff;
            backoff = (backoff * 2).min(timing.backoff_max);
            wait
        });
        if !matches!(update, UsageUpdate::Failed(_)) {
            backoff = timing.backoff_initial;
        }
        if tx.send(update).is_err() {
            return;
        }
        tokio::time::sleep(wait).await;
    }
}

/// One read, and how long to wait before the next; `None` means back off.
async fn read(client: &DaemonClient, timing: Timing) -> (UsageUpdate, Option<Duration>) {
    match client.hello().await {
        Ok(hello) if !hello.advertises(FLEET_CAPABILITY_USAGE_READ) => {
            return (
                UsageUpdate::Absent(NOT_SERVED.to_string()),
                Some(timing.ready_every),
            );
        }
        Ok(_) => {}
        Err(error) => return (UsageUpdate::Failed(error.to_string()), None),
    }
    let params = FleetUsageSummaryParams {
        period: FleetUsagePeriod::Trailing30Days,
    };
    match client
        .call_typed::<_, FleetUsageSummaryResult>(FLEET_USAGE_SUMMARY, &params)
        .await
    {
        Ok(reply) => {
            let wait = if reply.state == FleetUsageSummaryState::Scanning {
                timing.scanning_every
            } else {
                timing.ready_every
            };
            (UsageUpdate::Read(Box::new(reply), now_ms()), Some(wait))
        }
        Err(DaemonError::Rpc {
            code: METHOD_NOT_FOUND,
            ..
        }) => (
            UsageUpdate::Absent(NOT_SERVED.to_string()),
            Some(timing.ready_every),
        ),
        Err(error) => (UsageUpdate::Failed(error.to_string()), None),
    }
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| {
        i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
    })
}
