//! The host task that keeps section 20 (agent status) current (T0-section, #1015).
//!
//! `ainb-app` reduces; the host owns the socket. This task holds a Fleet
//! subscription, and for every revision it is told about it pays ONE daemon read,
//! the joined `fleet/roster_status`, and hands the reply to the TUI loop, which
//! folds it into section 20 through the section's reducer.
//!
//! It is the process's only agent-status reader (#1031): the TUI loop publishes
//! section 20 to the plugins as an envelope, so the Fleet panel costs no read of
//! its own.
//!
//! ```text
//!  daemon ──fleet/event──▶ task ──Head(rev)──▶ mpsc ──▶ drain_into ──▶ section 20
//!         ◀─roster_status─      ──Read(rows)─▶
//!  refused / gone        ──Failed / Absent──▶ (rows frozen, or absent, and why)
//! ```
//!
//! The read path is chosen once per connection from the daemon's advertised
//! catalogue. A daemon without `fleet.roster_status.read` (N-1), or a TUI with
//! `[fleet.status] legacy_panel` set, gets the pre-section two reads,
//! `fleet/snapshot` and `fleet/status`, joined with the proto's one `join`, so
//! every surface downstream sees the same reply shape either way.
//!
//! Failures never become a state: a read that fails freezes the section's rows
//! as unreachable, a daemon that serves neither path leaves it absent, and the
//! task retries with a bounded backoff. It is panic-free, because the TUI's
//! panic handler tears the terminal down.

use std::time::Duration;

use ainb_app::app::sections::AgentStatusSection;
use ainb_app::app::state::AppState;
use ainb_app::fleet::bridge::daemon::{
    ConnectionState, DaemonClient, DaemonError, FleetStreamEvent,
};
use ainb_hangar_proto::agent_status::{RosterStatusResult, join};
use ainb_hangar_proto::fleet::FLEET_CAPABILITY_ROSTER_STATUS_READ;
use ainb_hangar_proto::status_topic::{
    AGENT_STATUS_CLOCK_TOPIC, AGENT_STATUS_ENVELOPE_MAX_BYTES, AGENT_STATUS_TOPIC,
    AgentStatusClock, AgentStatusEnvelope, AgentStatusHealth,
};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// JSON-RPC "method not found": a daemon older than the read it was asked for.
const METHOD_NOT_FOUND: i32 = -32601;

/// The unreachable reason section 20 renders once the reader task has died.
const READER_STOPPED: &str = "agent status reader stopped";

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
struct Timing {
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

/// The running task and the channel its updates arrive on.
pub struct AgentStatusHost {
    updates: mpsc::UnboundedReceiver<AgentStatusUpdate>,
    task: JoinHandle<()>,
    /// The last envelope sequence handed to the plugin runtime.
    sequence: u64,
    /// Section 20 moved since the last publish.
    unpublished: bool,
    /// The task was found finished and section 20 was told so.
    stop_reported: bool,
    /// When the card clock was last published (#1054).
    last_tick: Option<std::time::Instant>,
}

impl AgentStatusHost {
    /// Start the task. Must be called inside a tokio runtime.
    ///
    /// `legacy_panel` is `[fleet.status] legacy_panel`: take the two reads even
    /// from a daemon that serves the joined one.
    #[must_use]
    pub fn spawn(dialer: Dialer, legacy_panel: bool) -> Self {
        Self::spawn_timed(dialer, legacy_panel, Timing::default())
    }

    fn spawn_timed(dialer: Dialer, legacy_panel: bool, timing: Timing) -> Self {
        let (tx, updates) = mpsc::unbounded_channel();
        let task = tokio::spawn(run(dialer, tx, legacy_panel, timing));
        Self {
            updates,
            task,
            sequence: 0,
            unpublished: false,
            stop_reported: false,
            last_tick: None,
        }
    }

    /// Fold every update that has arrived into section 20. Returns whether
    /// section 20's version moved.
    ///
    /// Supervises the owner too: the task is panic-free and ends only when this
    /// handle drops, so a finished task means it died (a panic, or an abort).
    /// Section 20 then renders unreachable with that reason instead of freezing
    /// its last read as live, because nothing else will ever read again.
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
        self.unpublished |= changed;
        changed
    }

    /// Publish section 20 to the plugins on [`AGENT_STATUS_TOPIC`] when it
    /// moved since the last publish (#1031). Every update drained in one loop
    /// iteration lands as one envelope, and the task pays at most one read per
    /// revision, so a burst of events yields one publish per revision.
    ///
    /// With no plugin runtime yet the change is held and published once one
    /// exists, so a runtime that starts after the first read still gets it.
    ///
    /// This task is also the tick source for the cards' ages (#1054): while
    /// section 20 holds cards it publishes the card clock on
    /// [`AGENT_STATUS_CLOCK_TOPIC`] once a second, and right after every
    /// envelope. Each publish marks the subscribing panel for a repaint, so an
    /// idle card's age advances without a key press or a daemon event.
    ///
    /// Returns whether an envelope was published.
    pub fn publish(
        &mut self,
        state: &AppState,
        runtime: Option<&ainb_plugin_runtime::RuntimeHandle>,
    ) -> bool {
        let Some(runtime) = runtime else {
            return false;
        };
        self.publish_with(
            state,
            std::time::Instant::now(),
            now_ms(),
            |topic, payload| {
                runtime.publish_snapshot(topic, payload.into());
            },
        )
    }

    /// [`Self::publish`] through `send`, at `now` and the local wall clock
    /// `local_now_ms`. The change stays unpublished until an envelope actually
    /// goes out: a section mid-reset, with nothing to encode yet, is published
    /// by a later iteration instead of being forgotten.
    fn publish_with(
        &mut self,
        state: &AppState,
        now: std::time::Instant,
        local_now_ms: i64,
        mut send: impl FnMut(&str, Vec<u8>),
    ) -> bool {
        let mut published = false;
        if self.unpublished {
            if let Some(payload) = encode(&state.agent_status, self.sequence + 1) {
                self.sequence += 1;
                send(AGENT_STATUS_TOPIC, payload);
                self.unpublished = false;
                published = true;
            }
        }
        let tick_due = self.last_tick.is_none_or(|at| now.duration_since(at) >= CLOCK_TICK);
        if published || tick_due {
            if let Some(payload) = encode_clock(&state.agent_status, local_now_ms) {
                send(AGENT_STATUS_CLOCK_TOPIC, payload);
                self.last_tick = Some(now);
            }
        }
        published
    }
}

/// How often the card clock is published while section 20 holds cards: ages
/// render in whole seconds.
const CLOCK_TICK: Duration = Duration::from_secs(1);

/// The local clock the tick carries.
///
/// The pane maps it onto the daemon's clock itself: since W0-mirror,
/// `FleetPaneState::evidence_clock_ms` is `view.daemon_now_ms(now)`, the
/// daemon's clock at its last read (`read_at_ms`) plus the local time held
/// since. So the tick must stay LOCAL: sending the daemon estimate here would
/// apply the skew twice. What the tick adds is the "since": without it the
/// pane's `now` never moves and every age freezes at the read (#1054).
fn card_clock_ms(_section: &AgentStatusSection, local_now_ms: i64) -> i64 {
    local_now_ms
}

/// The card-clock tick the plugins fold, encoded, or `None` while section 20
/// holds no cards (nothing on screen has an age to advance).
#[must_use]
pub fn encode_clock(section: &AgentStatusSection, local_now_ms: i64) -> Option<Vec<u8>> {
    if section.view.as_ref().is_none_or(|view| view.cards.is_empty()) {
        return None;
    }
    serde_json::to_vec(&AgentStatusClock {
        clock_ms: card_clock_ms(section, local_now_ms),
    })
    .ok()
}

/// Section 20 as the envelope the plugins fold, encoded.
///
/// `None` while the section holds neither a view nor an absent reason (a reset
/// with its read still in flight): the plugins keep the last envelope until
/// the read lands. A roster too large for the plugin framer is published as an
/// absent view with the reason, never cut short.
#[must_use]
pub fn encode(section: &AgentStatusSection, sequence: u64) -> Option<Vec<u8>> {
    let mut envelope = match (&section.view, &section.absent) {
        (Some(view), _) => AgentStatusEnvelope::from_view(sequence, view),
        (None, Some(reason)) => {
            AgentStatusEnvelope::absent(sequence, reason.clone(), section.head_revision)
        }
        (None, None) => return None,
    };
    // A failure reason can carry daemon error text verbatim (an RPC, IO or
    // decode message), so it is scrubbed exactly as the section 20 frame
    // scrubs it (`wire/mod.rs`). Here rather than in the proto, which has no
    // redactor, so every publish passes through it.
    match &mut envelope.health {
        AgentStatusHealth::Unreachable { reason, .. } | AgentStatusHealth::Absent { reason } => {
            *reason = ainb_app::fleet::bridge::redact::scrub(reason);
        }
        AgentStatusHealth::Live | AgentStatusHealth::Stale { .. } => {}
    }
    let bytes = match serde_json::to_vec(&envelope) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(%error, "agent status: envelope failed to encode");
            return None;
        }
    };
    if bytes.len() <= AGENT_STATUS_ENVELOPE_MAX_BYTES {
        return Some(bytes);
    }
    tracing::warn!(
        bytes = bytes.len(),
        rows = envelope.rows.len(),
        "agent status: roster too large to publish to plugins"
    );
    serde_json::to_vec(&AgentStatusEnvelope::absent(
        sequence,
        "roster too large to publish",
        section.head_revision,
    ))
    .ok()
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
        AgentStatusUpdate::Reset => state.agent_status_reset(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;
    use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

    fn fast() -> Timing {
        Timing {
            backoff_initial: Duration::from_millis(20),
            backoff_max: Duration::from_millis(160),
            min_uptime: Duration::from_secs(5),
        }
    }

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

    fn joined(revision: i64) -> Value {
        json!({ "rows": [], "read_revision": revision })
    }

    fn fleet_event(revision: i64) -> Value {
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
    type Answer = dyn Fn(&str, usize) -> Value + Send + Sync;

    /// What a fake daemon advertises and answers.
    #[derive(Clone)]
    struct Fake {
        /// Capabilities in the hello reply.
        capabilities: Vec<&'static str>,
        /// Every read method the task called, in order.
        calls: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        /// The reply body (`result` or `error`) for a read method.
        answer: std::sync::Arc<Answer>,
        /// Pushed after the subscription is acked.
        after_subscribe: Vec<Value>,
        /// Close the connection after acking the subscription.
        close_after_subscribe: bool,
    }

    impl Fake {
        fn joined(answer: impl Fn(&str, usize) -> Value + Send + Sync + 'static) -> Self {
            Self {
                capabilities: vec![FLEET_CAPABILITY_ROSTER_STATUS_READ],
                calls: std::sync::Arc::default(),
                answer: std::sync::Arc::new(answer),
                after_subscribe: Vec::new(),
                close_after_subscribe: false,
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }

        fn reads_of(&self, method: &str) -> usize {
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
                    send(
                        &mut writer,
                        &json!({"jsonrpc": "2.0", "id": id, "result": {
                            "capabilities": fake.capabilities
                        }}),
                    )
                    .await;
                }
                "fleet/roster_status" | "fleet/snapshot" | "fleet/status" => {
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
                }
                _ => {}
            }
        }
    }

    /// Serve `fake` on a fresh socket; `per_connection` may vary it by the
    /// connection's index.
    fn listen(
        dir: &tempfile::TempDir,
        per_connection: impl Fn(usize) -> Fake + Send + 'static,
    ) -> std::path::PathBuf {
        let socket = dir.path().join("hangar.sock");
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

    fn method_not_found() -> Value {
        json!({"error": {"code": -32601, "message": "method not found"}})
    }

    fn snapshot(revision: i64) -> Value {
        json!({"result": {"head_revision": revision, "sessions": []}})
    }

    fn status(revision: i64) -> Value {
        json!({"result": {"rows": [], "head_revision": revision}})
    }

    async fn drain_until(
        host: &mut AgentStatusHost,
        seen: &mut Vec<AgentStatusUpdate>,
        done: impl Fn(&[AgentStatusUpdate]) -> bool,
    ) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !done(seen) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "updates so far: {seen:?}"
            );
            while let Ok(update) = host.updates.try_recv() {
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
        let socket = listen(&dir, move |_| fake.clone());
        let mut host = AgentStatusHost::spawn_timed(dialer(socket), false, fast());
        let mut seen = Vec::new();
        drain_until(&mut host, &mut seen, |seen| {
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
        let socket = listen(&dir, move |_| fake.clone());
        let mut host = AgentStatusHost::spawn_timed(dialer(socket), false, fast());
        let mut seen = Vec::new();
        drain_until(&mut host, &mut seen, |seen| {
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
        let socket = listen(&dir, move |_| fake.clone());
        let mut host = AgentStatusHost::spawn_timed(dialer(socket), true, fast());
        let mut seen = Vec::new();
        drain_until(&mut host, &mut seen, |seen| {
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
        let socket = listen(&dir, move |_| fake.clone());
        let mut host = AgentStatusHost::spawn_timed(dialer(socket), false, fast());
        let mut seen = Vec::new();
        drain_until(&mut host, &mut seen, |seen| {
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
        let mut host = AgentStatusHost::spawn_timed(dialer, false, fast());
        let mut seen = Vec::new();
        drain_until(&mut host, &mut seen, |seen| seen.len() >= 4).await;
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
        let socket = listen(&dir, |index| {
            let first_incarnation = index < 3;
            let revision = if first_incarnation { 40 } else { 3 };
            let mut fake = Fake::joined(move |_, _| json!({"result": joined(revision)}));
            fake.close_after_subscribe = first_incarnation;
            fake
        });
        let mut host = AgentStatusHost::spawn_timed(dialer(socket), false, fast());
        let mut seen = Vec::new();
        drain_until(&mut host, &mut seen, |seen| {
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
        let socket = listen(&dir, move |_| fake.clone());
        let mut host = AgentStatusHost::spawn_timed(dialer(socket), false, fast());
        let mut seen = Vec::new();
        drain_until(&mut host, &mut seen, |seen| {
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

    /// #1031: the published envelope is section 20 exactly, the absent reason
    /// when there is no view, and nothing mid-reset.
    #[test]
    fn the_envelope_is_section_20_or_its_absent_reason() {
        let mut state = AppState::default();
        assert_eq!(
            encode(&state.agent_status, 1),
            None,
            "nothing to publish yet"
        );

        apply(
            &mut state,
            AgentStatusUpdate::Absent("daemon serves no agent status read".into()),
        );
        let absent: AgentStatusEnvelope =
            serde_json::from_slice(&encode(&state.agent_status, 1).unwrap()).unwrap();
        assert_eq!(
            absent.into_view(),
            Err("daemon serves no agent status read".to_string())
        );

        apply(
            &mut state,
            AgentStatusUpdate::Read(
                RosterStatusResult {
                    rows: Vec::new(),
                    read_revision: 4,
                    read_at_ms: 0,
                    unknown_events: Vec::new(),
                },
                10,
            ),
        );
        apply(&mut state, AgentStatusUpdate::Head(6));
        let envelope: AgentStatusEnvelope =
            serde_json::from_slice(&encode(&state.agent_status, 2).unwrap()).unwrap();
        assert_eq!(envelope.sequence, 2);
        assert_eq!(
            &envelope.into_view().expect("a view"),
            state.agent_status.view.as_ref().unwrap()
        );

        apply(&mut state, AgentStatusUpdate::Reset);
        assert_eq!(
            encode(&state.agent_status, 3),
            None,
            "a reset publishes nothing until its read lands"
        );
    }

    /// #1038 review item 6: a change stays unpublished while there is no
    /// runtime or nothing to encode, and is cleared only once an envelope goes
    /// out.
    #[tokio::test]
    async fn a_change_stays_unpublished_until_an_envelope_goes_out() {
        let dir = tempfile::tempdir().unwrap();
        let mut host =
            AgentStatusHost::spawn_timed(dialer(dir.path().join("missing.sock")), false, fast());
        let mut state = AppState::default();
        host.unpublished = true;

        assert!(!host.publish(&state, None), "no runtime: held");
        assert!(host.unpublished);
        let mut sent = Vec::new();
        assert!(
            !host.publish_with(&state, std::time::Instant::now(), 1, |topic, payload| sent
                .push((topic.to_string(), payload))),
            "nothing to encode mid-reset: held"
        );
        assert!(
            host.unpublished,
            "a failed encode does not clear the change"
        );

        apply(
            &mut state,
            AgentStatusUpdate::Read(
                RosterStatusResult {
                    rows: Vec::new(),
                    read_revision: 2,
                    read_at_ms: 0,
                    unknown_events: Vec::new(),
                },
                5,
            ),
        );
        assert!(
            host.publish_with(&state, std::time::Instant::now(), 1, |topic, payload| {
                sent.push((topic.to_string(), payload))
            })
        );
        assert!(!host.unpublished);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, AGENT_STATUS_TOPIC);
        assert!(
            !host.publish_with(&state, std::time::Instant::now(), 1, |topic, payload| sent
                .push((topic.to_string(), payload))),
            "published once"
        );
    }

    /// #1038 review item 7: a reader task that dies turns section 20
    /// unreachable, once, instead of leaving the panel live on its last read.
    #[tokio::test]
    async fn a_dead_reader_marks_section_20_unreachable() {
        let dir = tempfile::tempdir().unwrap();
        let mut host =
            AgentStatusHost::spawn_timed(dialer(dir.path().join("missing.sock")), false, fast());
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
        host.task.abort();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !host.task.is_finished() {
            assert!(tokio::time::Instant::now() < deadline, "the abort lands");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(host.drain_into(&mut state), "the stop is a change");
        let health = &state.agent_status.view.as_ref().expect("view").health;
        assert!(
            matches!(
                health,
                ainb_hangar_proto::status_view::ViewHealth::Unreachable { reason, .. }
                    if reason == READER_STOPPED
            ),
            "{health:?}"
        );
        assert!(host.unpublished, "and it is published");
        let version = state.agent_status.version();
        assert!(!host.drain_into(&mut state), "reported once");
        assert_eq!(state.agent_status.version(), version);
    }

    /// #1038 review item 4: a daemon RPC error carrying a token-shaped string
    /// reaches the plugins redacted.
    #[test]
    fn a_token_in_a_daemon_error_is_published_redacted() {
        let token = format!("ghp_{}", "a1B2c3D4e5".repeat(4));
        let (tx, mut rx) = mpsc::unbounded_channel();
        report(
            &tx,
            &DaemonError::Rpc {
                code: -32000,
                message: format!("store refused credential {token}"),
            },
        );
        let mut state = AppState::default();
        apply(
            &mut state,
            AgentStatusUpdate::Read(
                RosterStatusResult {
                    rows: Vec::new(),
                    read_revision: 1,
                    read_at_ms: 0,
                    unknown_events: Vec::new(),
                },
                1,
            ),
        );
        apply(&mut state, rx.try_recv().expect("a failure update"));
        let envelope: AgentStatusEnvelope =
            serde_json::from_slice(&encode(&state.agent_status, 1).unwrap()).unwrap();
        let AgentStatusHealth::Unreachable { reason, .. } = envelope.health else {
            panic!("unreachable: {:?}", envelope.health);
        };
        assert!(!reason.contains(&token), "{reason}");
        assert!(
            reason.contains(ainb_app::fleet::bridge::redact::REDACTED),
            "{reason}"
        );
    }

    /// #1054: the host task is the tick source. Holding cards, it publishes the
    /// card clock right after an envelope and then once a second, never faster,
    /// and never while section 20 is empty.
    #[tokio::test]
    async fn the_host_publishes_the_card_clock_once_a_second_while_it_holds_cards() {
        use ainb_hangar_proto::agent_status::{RosterStatusRow, status_row};
        use ainb_hangar_proto::fleet::{
            AttentionState, FleetCapabilities, FleetConfidence, FleetProvenance, FleetProvider,
            FleetSession, LifecycleState, ManagementState, PaneBinding, TransportHealth,
        };
        let dir = tempfile::tempdir().unwrap();
        let mut host =
            AgentStatusHost::spawn_timed(dialer(dir.path().join("missing.sock")), false, fast());
        let mut state = AppState::default();
        let start = std::time::Instant::now();
        let mut sent: Vec<(String, Vec<u8>)> = Vec::new();

        apply(
            &mut state,
            AgentStatusUpdate::Read(
                RosterStatusResult {
                    rows: Vec::new(),
                    read_revision: 1,
                    unknown_events: Vec::new(),
                    read_at_ms: 0,
                },
                1,
            ),
        );
        host.unpublished = true;
        host.publish_with(&state, start, 10_000, |topic, payload| {
            sent.push((topic.into(), payload))
        });
        assert_eq!(
            sent.iter().map(|(topic, _)| topic.as_str()).collect::<Vec<_>>(),
            [AGENT_STATUS_TOPIC],
            "no cards: no clock"
        );

        let session = FleetSession {
            session_key: "claude:a".into(),
            provider: FleetProvider::Claude,
            provider_session_id: Some("a".into()),
            tmux_target: None,
            pane_binding: PaneBinding::Bound,
            process_start_fingerprint: None,
            cwd: "/w".into(),
            display_name: None,
            lifecycle: LifecycleState::Idle,
            active_work_count: 0,
            attention: AttentionState::Ask,
            current_request_fingerprint: None,
            current_request: None,
            management: ManagementState::Managed,
            transport_health: TransportHealth::Healthy,
            capabilities: FleetCapabilities::default(),
            provenance: FleetProvenance::Authoritative,
            confidence: FleetConfidence::High,
            discovered_at: 1,
            last_observed_at: 1,
            lifecycle_updated_at: 1,
            attention_updated_at: 1,
            model: None,
            reasoning_effort: None,
            model_updated_at: 0,
            version: 1,
            updated_revision: 2,
        };
        apply(
            &mut state,
            AgentStatusUpdate::Read(
                RosterStatusResult {
                    rows: vec![RosterStatusRow {
                        status: status_row(&session, true),
                        session,
                        read_revision: 2,
                    }],
                    read_revision: 2,
                    unknown_events: Vec::new(),
                    read_at_ms: 0,
                },
                2,
            ),
        );
        host.unpublished = true;
        sent.clear();
        host.publish_with(&state, start, 10_000, |topic, payload| {
            sent.push((topic.into(), payload))
        });
        assert_eq!(
            sent.iter().map(|(topic, _)| topic.as_str()).collect::<Vec<_>>(),
            [AGENT_STATUS_TOPIC, AGENT_STATUS_CLOCK_TOPIC],
            "an envelope with cards is followed by the clock"
        );
        let tick: AgentStatusClock = serde_json::from_slice(&sent[1].1).unwrap();
        assert_eq!(tick.clock_ms, 10_000);

        sent.clear();
        host.publish_with(
            &state,
            start + Duration::from_millis(400),
            10_400,
            |topic, payload| {
                sent.push((topic.into(), payload));
            },
        );
        assert!(sent.is_empty(), "not faster than once a second: {sent:?}");
        host.publish_with(
            &state,
            start + Duration::from_millis(1_000),
            11_000,
            |topic, payload| {
                sent.push((topic.into(), payload));
            },
        );
        assert_eq!(
            sent.iter().map(|(topic, _)| topic.as_str()).collect::<Vec<_>>(),
            [AGENT_STATUS_CLOCK_TOPIC],
            "an idle second later, the clock alone"
        );
        let tick: AgentStatusClock = serde_json::from_slice(&sent[0].1).unwrap();
        assert_eq!(tick.clock_ms, 11_000);
    }
}
