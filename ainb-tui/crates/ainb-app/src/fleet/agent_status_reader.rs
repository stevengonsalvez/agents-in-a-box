//! The one agent-status reader both hosts run (#1188): the task that keeps
//! section 20 (agent status) current from the daemon.
//!
//! `ainb-app` reduces and the host owns the socket, so each host spawns this
//! task and folds what it reports on its own loop, as it does the attention
//! poller beside it. The task holds a Fleet subscription and for every revision
//! it is told about pays ONE daemon read, the joined `fleet/roster_status`.
//! The terminal and the desktop both run it; neither polls.
//!
//! ```text
//!  daemon ──fleet/event──▶ task ──Head(rev)──▶ mpsc ──▶ drain_into ──▶ section 20
//!         ◀─roster_status─      ──Read(rows)─▶
//!  refused / gone        ──Failed / Absent──▶ (rows frozen, or absent, and why)
//! ```
//!
//! The read path is chosen once per connection from the daemon's advertised
//! catalogue. A daemon without `fleet.roster_status.read` (N-1), or a host with
//! `[fleet.status] legacy_panel` set, gets the pre-section two reads,
//! `fleet/snapshot` and `fleet/status`, joined with the proto's one `join`, so
//! every surface downstream sees the same reply shape either way.
//!
//! Failures never become a state: a read that fails freezes the section's rows
//! as unreachable, a daemon that serves neither path leaves it absent, and the
//! task retries with a bounded backoff. A dropped subscription reads
//! unreachable at once, and a reconnect resets the section before its next
//! read. It is panic-free, because the TUI's panic handler tears the terminal
//! down.

use std::time::Duration;

use ainb_hangar_proto::agent_status::{RosterStatusResult, join};
use ainb_hangar_proto::fleet::FLEET_CAPABILITY_ROSTER_STATUS_READ;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::app::state::AppState;
use crate::fleet::bridge::daemon::{ConnectionState, DaemonClient, DaemonError, FleetStreamEvent};

/// JSON-RPC "method not found": a daemon older than the read it was asked for.
const METHOD_NOT_FOUND: i32 = -32601;

/// The unreachable reason section 20 renders once the reader task has died.
pub const READER_STOPPED: &str = "agent status reader stopped";

/// Which daemon read the task pays per revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadPath {
    /// One `fleet/roster_status`.
    Joined,
    /// `fleet/snapshot` and `fleet/status`, joined here.
    TwoReads,
}

/// Retry and backoff bounds. Shortened only by the tests.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// First retry wait after a failure.
    backoff_initial: Duration,
    /// Longest retry wait, and the wait after a daemon without the method.
    backoff_max: Duration,
    /// A connection must stay up this long before a later failure restarts
    /// the backoff at `backoff_initial`, so a daemon that accepts and drops
    /// cannot drive a tight reconnect loop (the `presence.rs` rule).
    min_uptime: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            backoff_initial: Duration::from_millis(500),
            backoff_max: Duration::from_secs(30),
            min_uptime: Duration::from_secs(5),
        }
    }
}

/// One thing the task learned, for the TUI loop to fold into section 20.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentStatusUpdate {
    /// A joined read landed, received at this local epoch-ms clock.
    Read(RosterStatusResult, i64),
    /// A newer Fleet revision was observed.
    Head(i64),
    /// The task reconnected: section 20 drops its view and keeps its head.
    Reset,
    /// A read or the subscription failed, for this reason, at this local clock.
    Failed(String, i64),
    /// The daemon cannot serve the joined read.
    Absent(String),
}

/// Resolves a fresh daemon client for every connection attempt.
pub type Dialer = Box<dyn Fn() -> Result<DaemonClient, DaemonError> + Send + Sync>;

/// The running reader task and the channel its updates arrive on. Dropping it
/// stops the task.
pub struct AgentStatusReader {
    updates: mpsc::UnboundedReceiver<AgentStatusUpdate>,
    task: JoinHandle<()>,
    /// The task was found finished and section 20 was told so.
    stop_reported: bool,
}

impl AgentStatusReader {
    /// Start the task. Must be called inside a tokio runtime.
    ///
    /// `dialer` names the surface the reads are recorded under; `legacy_panel`
    /// is `[fleet.status] legacy_panel`: take the two reads even from a daemon
    /// that serves the joined one.
    #[must_use]
    pub fn spawn(dialer: Dialer, legacy_panel: bool) -> Self {
        Self::spawn_timed(dialer, legacy_panel, Timing::default())
    }

    /// [`Self::spawn`] with its retry bounds given. For tests.
    #[must_use]
    pub fn spawn_timed(dialer: Dialer, legacy_panel: bool, timing: Timing) -> Self {
        let (tx, updates) = mpsc::unbounded_channel();
        let task = tokio::spawn(run(dialer, tx, legacy_panel, timing));
        Self {
            updates,
            task,
            stop_reported: false,
        }
    }

    /// Fold every update that has arrived into section 20. Returns whether
    /// section 20's version moved.
    ///
    /// Supervises the task too: it is panic-free and ends only when this handle
    /// drops, so a finished task means it died (a panic, or an abort). Section
    /// 20 then renders unreachable with that reason instead of freezing its
    /// last read as live, because nothing else will ever read again.
    pub fn drain_into(&mut self, state: &mut AppState) -> bool {
        let mut changed = false;
        while let Ok(update) = self.updates.try_recv() {
            changed |= apply(state, update);
        }
        if !self.stop_reported && self.task.is_finished() {
            self.stop_reported = true;
            tracing::warn!("agent status: the reader task stopped");
            changed |= state.agent_status_read_failed(READER_STOPPED.to_string(), now_ms());
        }
        changed
    }
}

impl Drop for AgentStatusReader {
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
        AgentStatusUpdate::Reset => state.agent_status_reset(),
        AgentStatusUpdate::Failed(reason, now_ms) => state.agent_status_read_failed(reason, now_ms),
        AgentStatusUpdate::Absent(reason) => state.agent_status_absent(reason),
    }
}

pub(crate) fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| {
        i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
    })
}

/// Why a connection ended.
enum Ended {
    /// The daemon serves neither read path; wait long before asking again.
    Absent,
    /// Anything else: retry on the ordinary backoff.
    Failed,
    /// The update channel is gone: the TUI quit.
    Closed,
}

async fn run(
    dialer: Dialer,
    tx: mpsc::UnboundedSender<AgentStatusUpdate>,
    legacy_panel: bool,
    timing: Timing,
) {
    let mut backoff = timing.backoff_initial;
    let mut connected_before = false;
    loop {
        let started = tokio::time::Instant::now();
        let (ended, connected) = match dialer() {
            Ok(client) => serve(&client, &tx, connected_before, legacy_panel).await,
            Err(error) => (report(&tx, &error), false),
        };
        connected_before |= connected;
        // Only a connection that stayed up restarts the backoff.
        if connected && started.elapsed() >= timing.min_uptime {
            backoff = timing.backoff_initial;
        }
        let wait = match ended {
            Ended::Closed => return,
            Ended::Absent => timing.backoff_max,
            Ended::Failed => backoff,
        };
        tokio::time::sleep(wait).await;
        backoff = (backoff * 2).min(timing.backoff_max);
    }
}

/// One connection: pick the read path, read, subscribe, and read once per
/// revision the last read does not already cover, until it ends. Returns why
/// it ended and whether a read landed on it.
async fn serve(
    client: &DaemonClient,
    tx: &mpsc::UnboundedSender<AgentStatusUpdate>,
    reconnect: bool,
    legacy_panel: bool,
) -> (Ended, bool) {
    let mut path = match client.hello().await {
        Ok(hello) if !legacy_panel && hello.advertises(FLEET_CAPABILITY_ROSTER_STATUS_READ) => {
            ReadPath::Joined
        }
        Ok(_) => ReadPath::TwoReads,
        Err(error) => return (report(tx, &error), false),
    };
    let mut covered = match read(client, tx, reconnect, &mut path).await {
        Ok(read_revision) => read_revision,
        Err(ended) => return (ended, false),
    };
    let mut subscription = client.reconnecting_fleet_subscription(covered);
    let mut state_rx = subscription.state();
    let mut was_reconnecting = false;
    loop {
        tokio::select! {
            state_changed = state_rx.changed() => {
                if state_changed.is_err() {
                    return (Ended::Closed, true);
                }
                let current_state = state_rx.borrow().clone();
                match current_state {
                    ConnectionState::Reconnecting { error, .. } => {
                        was_reconnecting = true;
                        let reason = error.unwrap_or_else(|| "daemon not reachable".to_string());
                        if tx.send(AgentStatusUpdate::Failed(reason, now_ms())).is_err() {
                            return (Ended::Closed, true);
                        }
                    }
                    ConnectionState::Connected => {
                        if was_reconnecting {
                            was_reconnecting = false;
                            match read(client, tx, true, &mut path).await {
                                Ok(read_revision) => {
                                    covered = read_revision;
                                    subscription.set_after_revision(covered);
                                }
                                Err(ended) => return (ended, true),
                            }
                        }
                    }
                    ConnectionState::Closed => {
                        return (Ended::Closed, true);
                    }
                }
            }
            event_res = subscription.next_event() => {
                match event_res {
                    Ok(FleetStreamEvent::Revision(event)) => {
                        if tx.send(AgentStatusUpdate::Head(event.revision)).is_err() {
                            return (Ended::Closed, true);
                        }
                        // A burst: the last read already describes this revision.
                        if event.revision <= covered {
                            continue;
                        }
                    }
                    Ok(FleetStreamEvent::ResyncRequired) => {}
                    Err(error) => return (report(tx, &error), true),
                }
                match read(client, tx, false, &mut path).await {
                    Ok(read_revision) => {
                        covered = read_revision;
                        subscription.set_after_revision(covered);
                    }
                    Err(ended) => return (ended, true),
                }
            }
        }
    }
}

/// One read on `path`: sends the reply (preceded by a reset on a reconnect)
/// and returns its revision, or sends the failure and returns why the
/// connection ends.
///
/// A daemon that advertised the joined read and then refuses it as unknown
/// moves this connection to the two reads rather than leaving the panel
/// absent: the catalogue and the method table disagreeing is the daemon's
/// defect, not a reason to empty the roster.
async fn read(
    client: &DaemonClient,
    tx: &mpsc::UnboundedSender<AgentStatusUpdate>,
    reset_first: bool,
    path: &mut ReadPath,
) -> Result<i64, Ended> {
    let reply = match *path {
        ReadPath::Joined => match client.fleet_roster_status().await {
            Err(DaemonError::Rpc { code, .. }) if code == METHOD_NOT_FOUND => {
                tracing::warn!(
                    "agent status: daemon advertised fleet/roster_status but refused it"
                );
                *path = ReadPath::TwoReads;
                two_reads(client).await
            }
            other => other,
        },
        ReadPath::TwoReads => two_reads(client).await,
    };
    match reply {
        Ok(result) => {
            if reset_first {
                tx.send(AgentStatusUpdate::Reset).map_err(|_| Ended::Closed)?;
            }
            let revision = result.read_revision;
            tx.send(AgentStatusUpdate::Read(result, now_ms())).map_err(|_| Ended::Closed)?;
            Ok(revision)
        }
        Err(error) => Err(report(tx, &error)),
    }
}

/// The pre-section read: the roster and the status table, joined by the one
/// proto `join`.
async fn two_reads(client: &DaemonClient) -> Result<RosterStatusResult, DaemonError> {
    let snapshot = client.fleet_snapshot().await?;
    let status = client.fleet_status().await?;
    // Neither reply carries the daemon's clock, so the joined read has none and
    // cards age on this surface's own now, as they did before the section read.
    Ok(join(&snapshot, &status, 0))
}

/// Send a failure update and say how the connection ends.
///
/// The rendered reason is generic for a dial or token failure: those errors
/// carry the absolute socket path, which belongs in the log, not on screen.
fn report(tx: &mpsc::UnboundedSender<AgentStatusUpdate>, error: &DaemonError) -> Ended {
    let (update, ended) = match error {
        DaemonError::Rpc { code, .. } if *code == METHOD_NOT_FOUND => (
            AgentStatusUpdate::Absent("daemon serves no agent status read".to_string()),
            Ended::Absent,
        ),
        DaemonError::Connect { .. } => {
            tracing::debug!(error = %error, "agent status: daemon not reachable");
            (
                AgentStatusUpdate::Failed("daemon not reachable".to_string(), now_ms()),
                Ended::Failed,
            )
        }
        DaemonError::Token(_) | DaemonError::NoHome => {
            tracing::debug!(error = %error, "agent status: daemon credentials unavailable");
            (
                AgentStatusUpdate::Failed("daemon credentials unavailable".to_string(), now_ms()),
                Ended::Failed,
            )
        }
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

/// A fake daemon on a unix socket, for the reader's tests and a host's (#1188).
///
/// It answers `auth/hello` with a catalogue, each read method through an
/// answer function, and `fleet/subscribe` as configured, and records every call
/// and hello it saw.
#[cfg(any(test, feature = "test-support"))]
pub mod fake_daemon {
    #![allow(missing_docs, clippy::missing_panics_doc)]

    use ainb_hangar_proto::fleet::FLEET_CAPABILITY_ROSTER_STATUS_READ;
    use serde_json::{Value, json};
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;
    use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

    async fn frame(reader: &mut BufReader<OwnedReadHalf>) -> Option<Value> {
        let mut length = None;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).await.ok()? == 0 {
                return None;
            }
            let line = line.trim_end();
            if line.is_empty() {
                let mut body = vec![0_u8; length?];
                reader.read_exact(&mut body).await.ok()?;
                return serde_json::from_slice(&body).ok();
            }
            if let Some((name, value)) = line.split_once(':') {
                if name.eq_ignore_ascii_case("Content-Length") {
                    length = value.trim().parse().ok();
                }
            }
        }
    }

    async fn send(writer: &mut OwnedWriteHalf, value: &Value) {
        let body = serde_json::to_vec(value).unwrap();
        let head = format!("Content-Length: {}\r\n\r\n", body.len());
        let _ = writer.write_all(head.as_bytes()).await;
        let _ = writer.write_all(&body).await;
        let _ = writer.flush().await;
    }

    pub fn joined(revision: i64) -> Value {
        json!({ "rows": [], "read_revision": revision })
    }

    pub fn fleet_event(revision: i64) -> Value {
        json!({
            "method": "fleet/event",
            "params": {
                "revision": revision, "event_id": format!("e-{revision}"),
                "session_key": "claude:x", "observed_at": 1, "provenance": "authoritative",
                "event_type": "turn_started", "payload": {}, "session_version": 1, "applied": true
            }
        })
    }

    /// A fake daemon's reply body for a read method and the call's index.
    pub type Answer = dyn Fn(&str, usize) -> Value + Send + Sync;

    /// What a fake daemon advertises and answers.
    #[derive(Clone)]
    pub struct Fake {
        /// Capabilities in the hello reply.
        pub capabilities: Vec<&'static str>,
        /// Every read method the task called, in order.
        pub calls: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        /// The params of every `auth/hello`, in order.
        pub hellos: std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
        /// The reply body (`result` or `error`) for a read method.
        pub answer: std::sync::Arc<Answer>,
        /// Pushed after the subscription is acked.
        pub after_subscribe: Vec<Value>,
        /// Close the connection after acking the subscription.
        pub close_after_subscribe: bool,
        /// Hold the subscription open until this is notified, then close it,
        /// as a daemon that dies mid-stream does.
        pub hang_up: Option<std::sync::Arc<tokio::sync::Notify>>,
    }

    impl Fake {
        pub fn joined(answer: impl Fn(&str, usize) -> Value + Send + Sync + 'static) -> Self {
            Self {
                capabilities: vec![FLEET_CAPABILITY_ROSTER_STATUS_READ],
                calls: std::sync::Arc::default(),
                hellos: std::sync::Arc::default(),
                answer: std::sync::Arc::new(answer),
                after_subscribe: Vec::new(),
                close_after_subscribe: false,
                hang_up: None,
            }
        }

        pub fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }

        pub fn reads_of(&self, method: &str) -> usize {
            self.calls().iter().filter(|call| *call == method).count()
        }
    }

    /// One fake daemon connection: answers hello with the fake's catalogue,
    /// each read method through `answer`, and the subscription as configured.
    async fn serve_connection(stream: tokio::net::UnixStream, fake: Fake) {
        let (read_half, mut writer) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        while let Some(request) = frame(&mut reader).await {
            let id = request["id"].clone();
            let method = request["method"].as_str().unwrap_or_default().to_string();
            match method.as_str() {
                "auth/hello" => {
                    fake.hellos.lock().unwrap().push(request["params"].clone());
                    send(
                        &mut writer,
                        &json!({"jsonrpc": "2.0", "id": id, "result": {
                            "capabilities": fake.capabilities
                        }}),
                    )
                    .await;
                }
                // `fleet/usage_summary` too: the usage reader's tests (D3p-e)
                // share this fake rather than growing a second one.
                "fleet/roster_status"
                | "fleet/snapshot"
                | "fleet/status"
                | "fleet/usage_summary" => {
                    let index = {
                        let mut calls = fake.calls.lock().unwrap();
                        calls.push(method.clone());
                        calls.len()
                    };
                    let mut reply = (fake.answer)(&method, index);
                    reply["id"] = id;
                    reply["jsonrpc"] = json!("2.0");
                    send(&mut writer, &reply).await;
                }
                "fleet/subscribe" => {
                    send(
                        &mut writer,
                        &json!({"jsonrpc": "2.0", "id": id, "result": {
                            "snapshot": {"head_revision": 0, "sessions": []},
                            "replay": [], "replay_state": {"state": "complete"}
                        }}),
                    )
                    .await;
                    if fake.close_after_subscribe {
                        return;
                    }
                    for event in &fake.after_subscribe {
                        send(&mut writer, event).await;
                    }
                    if let Some(hang_up) = &fake.hang_up {
                        hang_up.notified().await;
                        return;
                    }
                }
                _ => {}
            }
        }
    }

    /// Serve `fake` on a fresh socket; `per_connection` may vary it by the
    /// connection's index.
    pub fn listen(
        socket: &std::path::Path,
        per_connection: impl Fn(usize) -> Fake + Send + 'static,
    ) -> std::path::PathBuf {
        let socket = socket.to_path_buf();
        let listener = UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            let mut index = 0;
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(serve_connection(stream, per_connection(index)));
                index += 1;
            }
        });
        socket
    }

    pub fn method_not_found() -> Value {
        json!({"error": {"code": -32601, "message": "method not found"}})
    }

    pub fn snapshot(revision: i64) -> Value {
        json!({"result": {"head_revision": revision, "sessions": []}})
    }

    pub fn status(revision: i64) -> Value {
        json!({"result": {"rows": [], "head_revision": revision}})
    }
}

#[cfg(test)]
mod tests {
    use super::fake_daemon::{
        Fake, fleet_event, joined, listen, method_not_found, snapshot, status,
    };
    use super::*;
    use serde_json::json;

    fn fast() -> Timing {
        Timing {
            backoff_initial: Duration::from_millis(20),
            backoff_max: Duration::from_millis(160),
            min_uptime: Duration::from_secs(5),
        }
    }

    async fn drain_until(
        reader: &mut AgentStatusReader,
        seen: &mut Vec<AgentStatusUpdate>,
        done: impl Fn(&[AgentStatusUpdate]) -> bool,
    ) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !done(seen) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "updates so far: {seen:?}"
            );
            while let Ok(update) = reader.updates.try_recv() {
                seen.push(update);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn dialer(socket: std::path::PathBuf) -> Dialer {
        Box::new(move || Ok(DaemonClient::with_parts(socket.clone(), "t".to_string())))
    }

    /// The loop folds updates through section 20's reducers, and the version
    /// moves only for a change.
    #[test]
    fn updates_fold_into_section_20_and_only_changes_bump_it() {
        let mut state = AppState::default();
        let before = state.agent_status.version();
        assert!(apply(
            &mut state,
            AgentStatusUpdate::Absent("daemon serves no agent status read".into())
        ));
        assert!(!apply(
            &mut state,
            AgentStatusUpdate::Absent("daemon serves no agent status read".into())
        ));
        assert_eq!(state.agent_status.version(), before + 1);
        let empty = RosterStatusResult {
            rows: Vec::new(),
            read_revision: 4,
            unknown_events: Vec::new(),
            read_at_ms: 0,
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
            AgentStatusUpdate::Failed("daemon not reachable".into(), 12)
        ));
    }

    /// #1019 review, absent: a daemon that serves neither read yields Absent,
    /// and the task waits the long bound before asking again.
    #[tokio::test]
    async fn a_daemon_serving_no_read_leaves_section_20_absent() {
        let dir = tempfile::tempdir().unwrap();
        let mut fake = Fake::joined(|_, _| method_not_found());
        fake.capabilities.clear();
        let observed = fake.clone();
        let socket = listen(&dir.path().join("hangar.sock"), move |_| fake.clone());
        let mut reader = AgentStatusReader::spawn_timed(dialer(socket), false, fast());
        let mut seen = Vec::new();
        drain_until(&mut reader, &mut seen, |seen| {
            seen.iter().any(|update| matches!(update, AgentStatusUpdate::Absent(_)))
        })
        .await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            observed.calls(),
            vec!["fleet/snapshot".to_string()],
            "an absent daemon is not re-asked inside the long bound"
        );
    }

    /// #1031, N-1: a daemon that does not advertise the joined read gets the
    /// two reads, joined, and never a `fleet/roster_status` it would refuse.
    #[tokio::test]
    async fn an_n_minus_one_daemon_is_read_with_the_two_reads() {
        let dir = tempfile::tempdir().unwrap();
        let mut fake = Fake::joined(|method, _| match method {
            "fleet/snapshot" => snapshot(6),
            "fleet/status" => status(6),
            _ => method_not_found(),
        });
        fake.capabilities.clear();
        let observed = fake.clone();
        let socket = listen(&dir.path().join("hangar.sock"), move |_| fake.clone());
        let mut reader = AgentStatusReader::spawn_timed(dialer(socket), false, fast());
        let mut seen = Vec::new();
        drain_until(&mut reader, &mut seen, |seen| {
            seen.iter()
                .any(|update| matches!(update, AgentStatusUpdate::Read(read, _) if read.read_revision == 6))
        })
        .await;
        assert_eq!(observed.reads_of("fleet/roster_status"), 0);
        assert!(
            !seen.iter().any(|update| matches!(
                update,
                AgentStatusUpdate::Absent(_) | AgentStatusUpdate::Failed(..)
            )),
            "the N-1 path renders rows, not a failure: {seen:?}"
        );
    }

    /// #1031, legacy flag: `[fleet.status] legacy_panel` takes the two reads
    /// even from a daemon that serves the joined one.
    #[tokio::test]
    async fn the_legacy_panel_flag_takes_the_two_reads() {
        let dir = tempfile::tempdir().unwrap();
        let fake = Fake::joined(|method, _| match method {
            "fleet/snapshot" => snapshot(2),
            "fleet/status" => status(2),
            _ => json!({"result": joined(2)}),
        });
        let observed = fake.clone();
        let socket = listen(&dir.path().join("hangar.sock"), move |_| fake.clone());
        let mut reader = AgentStatusReader::spawn_timed(dialer(socket), true, fast());
        let mut seen = Vec::new();
        drain_until(&mut reader, &mut seen, |seen| {
            seen.iter().any(|update| matches!(update, AgentStatusUpdate::Read(..)))
        })
        .await;
        assert_eq!(observed.reads_of("fleet/roster_status"), 0);
        assert_eq!(observed.reads_of("fleet/snapshot"), 1);
        assert_eq!(observed.reads_of("fleet/status"), 1);
    }

    /// #1031: a daemon whose catalogue claims the joined read but whose method
    /// table refuses it moves to the two reads instead of rendering absent.
    #[tokio::test]
    async fn a_refused_joined_read_falls_back_on_the_same_connection() {
        let dir = tempfile::tempdir().unwrap();
        let fake = Fake::joined(|method, _| match method {
            "fleet/snapshot" => snapshot(3),
            "fleet/status" => status(3),
            _ => method_not_found(),
        });
        let socket = listen(&dir.path().join("hangar.sock"), move |_| fake.clone());
        let mut reader = AgentStatusReader::spawn_timed(dialer(socket), false, fast());
        let mut seen = Vec::new();
        drain_until(&mut reader, &mut seen, |seen| {
            seen.iter().any(|update| matches!(update, AgentStatusUpdate::Read(..)))
        })
        .await;
        assert!(
            !seen.iter().any(|update| matches!(update, AgentStatusUpdate::Absent(_))),
            "{seen:?}"
        );
    }

    /// #1019 review, backoff and item 8: a dial failure renders a generic
    /// reason (no socket path) and retries on a growing, bounded backoff.
    #[tokio::test]
    async fn a_dead_socket_backs_off_and_never_renders_its_path() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("missing.sock");
        let attempts = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen_attempts = attempts.clone();
        let path = socket.clone();
        let dialer: Dialer = Box::new(move || {
            seen_attempts.lock().unwrap().push(std::time::Instant::now());
            Ok(DaemonClient::with_parts(path.clone(), "t".to_string()))
        });
        let mut reader = AgentStatusReader::spawn_timed(dialer, false, fast());
        let mut seen = Vec::new();
        drain_until(&mut reader, &mut seen, |seen| seen.len() >= 4).await;
        for update in &seen {
            let AgentStatusUpdate::Failed(reason, _) = update else {
                panic!("only failures from a dead socket: {update:?}");
            };
            assert_eq!(reason, "daemon not reachable");
            assert!(!reason.contains(&socket.display().to_string()));
        }
        let times = attempts.lock().unwrap().clone();
        let gaps: Vec<_> = times.windows(2).map(|pair| pair[1] - pair[0]).collect();
        assert!(gaps.len() >= 3, "{gaps:?}");
        assert!(
            gaps[2] > gaps[0],
            "the wait grows between attempts: {gaps:?}"
        );
        assert!(
            gaps.iter().all(|gap| *gap < Duration::from_secs(1)),
            "and stays bounded: {gaps:?}"
        );
    }

    /// #1019 review, reconnect: after a connection drops, the task resets
    /// section 20 before the next read, and a read that lands below the head
    /// the section was told renders stale, not live.
    #[tokio::test]
    async fn a_reconnect_resets_the_section_and_a_lower_read_renders_stale() {
        let dir = tempfile::tempdir().unwrap();
        // Connections 0 to 2 are the first incarnation (every call dials its
        // own): the hello, a read at 40, then a subscription it closes, as a
        // killed daemon does. After that the store is rebuilt and its counter
        // restarted at 3.
        let socket = listen(&dir.path().join("hangar.sock"), |index| {
            let first_incarnation = index < 3;
            let revision = if first_incarnation { 40 } else { 3 };
            let mut fake = Fake::joined(move |_, _| json!({"result": joined(revision)}));
            fake.close_after_subscribe = first_incarnation;
            fake
        });
        let mut reader = AgentStatusReader::spawn_timed(dialer(socket), false, fast());
        let mut seen = Vec::new();
        drain_until(&mut reader, &mut seen, |seen| {
            seen.iter().any(|update| matches!(update, AgentStatusUpdate::Read(read, _) if read.read_revision == 3))
        })
        .await;
        let reset = seen
            .iter()
            .position(|update| *update == AgentStatusUpdate::Reset)
            .expect("a reset");
        let lower = seen
            .iter()
            .position(|update| matches!(update, AgentStatusUpdate::Read(read, _) if read.read_revision == 3))
            .unwrap();
        assert!(
            reset < lower,
            "the reset precedes the first read after reconnect: {seen:?}"
        );

        let mut state = AppState::default();
        for update in seen {
            apply(&mut state, update);
        }
        state.observe_agent_status_head(40);
        let health = &state.agent_status.view.as_ref().expect("view").health;
        assert!(
            matches!(
                health,
                ainb_hangar_proto::status_view::ViewHealth::Stale {
                    read_revision: 3,
                    ..
                }
            ),
            "a lower read after a reconnect renders stale: {health:?}"
        );
    }

    /// #1019 review, coalescing: events the last read already covers cost no
    /// read; only a revision past it does.
    #[tokio::test]
    async fn a_burst_of_covered_revisions_costs_no_extra_read() {
        let dir = tempfile::tempdir().unwrap();
        let mut fake = Fake::joined(|_, index| {
            // The first read answers 10; any later read answers 11.
            let revision = if index <= 1 { 10 } else { 11 };
            json!({"result": joined(revision)})
        });
        fake.after_subscribe = vec![
            fleet_event(8),
            fleet_event(9),
            fleet_event(10),
            fleet_event(11),
        ];
        let observed = fake.clone();
        let socket = listen(&dir.path().join("hangar.sock"), move |_| fake.clone());
        let mut reader = AgentStatusReader::spawn_timed(dialer(socket), false, fast());
        let mut seen = Vec::new();
        drain_until(&mut reader, &mut seen, |seen| {
            seen.iter().any(|update| matches!(update, AgentStatusUpdate::Head(11)))
                && seen
                    .iter()
                    .filter(|update| matches!(update, AgentStatusUpdate::Read(..)))
                    .count()
                    >= 2
        })
        .await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            observed.reads_of("fleet/roster_status"),
            2,
            "revisions 8, 9 and 10 are covered by the read at 10; only 11 costs a read"
        );
    }

    /// #1038 review item 7: a reader task that dies turns section 20
    /// unreachable, once, instead of leaving the panel live on its last read.
    #[tokio::test]
    async fn a_dead_reader_marks_section_20_unreachable() {
        let dir = tempfile::tempdir().unwrap();
        let mut reader =
            AgentStatusReader::spawn_timed(dialer(dir.path().join("missing.sock")), false, fast());
        let mut state = AppState::default();
        apply(
            &mut state,
            AgentStatusUpdate::Read(
                RosterStatusResult {
                    rows: Vec::new(),
                    read_revision: 3,
                    read_at_ms: 0,
                    unknown_events: Vec::new(),
                },
                5,
            ),
        );
        reader.task.abort();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !reader.task.is_finished() {
            assert!(tokio::time::Instant::now() < deadline, "the abort lands");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(reader.drain_into(&mut state), "the stop is a change");
        let health = &state.agent_status.view.as_ref().expect("view").health;
        assert!(
            matches!(
                health,
                ainb_hangar_proto::status_view::ViewHealth::Unreachable { reason, .. }
                    if reason == READER_STOPPED
            ),
            "{health:?}"
        );
        let version = state.agent_status.version();
        assert!(!reader.drain_into(&mut state), "reported once");
        assert_eq!(state.agent_status.version(), version);
    }
}
