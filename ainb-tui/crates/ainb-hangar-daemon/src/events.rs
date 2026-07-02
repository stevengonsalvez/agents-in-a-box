//! The daemon's in-process event broker — the producer half of the
//! dual-channel design (e38.2).
//!
//! [`ainb_hangar_proto::events`] has carried the typed [`HangarEvent`] wire
//! contract since P3, and the plugin's `StreamClient` fully decodes
//! `hangar/event` notification frames — but until e38.2 the daemon had zero
//! emission sites, so live UI was snapshot-pull only. This module closes the
//! gap:
//!
//! - Mutation paths (the claim loop's FSM finalize, the RPC `task_transition`
//!   handler, the autopilot scheduler / fire-now path) hold an [`EventSink`]
//!   and call [`EventSink::emit`] with the owning workspace's **real row id**
//!   after each state change commits.
//! - The RPC server registers each authenticated, subscribed connection as a
//!   workspace-scoped consumer ([`EventBroker::subscribe`]); a per-connection
//!   forwarder filters [`ScopedEvent`]s to the subscription's workspace and
//!   frames matching events as JSON-RPC *notifications* (`{jsonrpc, method,
//!   params}`, no `id`) in the same Content-Length envelope responses use.
//!
//! ## Delivery semantics
//!
//! Best-effort, at-most-once: the broker is a [`tokio::sync::broadcast`]
//! channel, so emission never blocks a mutation path (no subscribers → the
//! send is dropped) and a slow consumer that lags past the channel capacity
//! loses the oldest events rather than back-pressuring the daemon. That is the
//! designed contract — the plugin treats events as instant feedback and the
//! next snapshot pull reconciles authoritatively, so a dropped event
//! self-heals.
//!
//! ## Scoping invariant
//!
//! Every [`ScopedEvent::workspace_id`] and every subscription's workspace are
//! the **resolved row id** (never the wire slug), so the equality filter in the
//! connection forwarder is exact: events for workspace A can never reach a
//! connection subscribed to workspace B.

use ainb_hangar_proto::events::{EVENT_METHOD, HangarEvent};
use tokio::sync::{broadcast, mpsc};

/// Broadcast channel capacity. Events are tiny and consumers drain fast; a
/// burst beyond this lags the slowest consumer (dropping its oldest events)
/// instead of growing memory.
const CHANNEL_CAPACITY: usize = 256;

/// A domain event tagged with the owning workspace's resolved row id.
///
/// The workspace id rides *beside* the event (not inside it) because most
/// [`HangarEvent`] variants deliberately omit it from the wire payload — the
/// plugin already knows which workspace it subscribed.
#[derive(Debug, Clone)]
pub struct ScopedEvent {
    /// The owning workspace's real row id (never the slug).
    pub workspace_id: String,
    /// The typed wire event.
    pub event: HangarEvent,
}

/// The daemon-global event broker: mutation paths publish through a cloned
/// [`EventSink`]; the RPC server taps per-connection receivers.
///
/// Two fan-out channels ride under one sink:
///
/// - a [`broadcast`] channel for the LIVE consumers (per-connection RPC
///   forwarders, the inbox aggregator). Lossy by design — a slow consumer that
///   lags past the buffer drops its oldest events; the next snapshot pull
///   reconciles.
/// - an optional lossless [`mpsc`] channel feeding the DURABLE outbox drain
///   ([`crate::event_outbox`]). Unbounded so a burst never back-pressures a
///   mutation path AND never drops an event — the outbox must be a gapless log
///   for `seq`-cursor replay to be correct. Present only when the broker was
///   built with [`EventBroker::with_outbox`]; [`EventBroker::new`] (unit tests,
///   pool-less harnesses) is broadcast-only.
#[derive(Debug, Clone)]
pub struct EventBroker {
    tx: broadcast::Sender<ScopedEvent>,
    outbox_tx: Option<mpsc::UnboundedSender<ScopedEvent>>,
    /// The FLEET-WIDE attention channel (spec P2). Attention events are NOT
    /// workspace-partitioned — the control centre answers for the whole host, and
    /// a hand-started session has no workspace to scope by — so `AttentionRaised`
    /// / `AttentionAnswered` ride their own broadcast, unfiltered by the
    /// workspace forwarder, with the `attention` table (not the event-log outbox)
    /// as their durable source. Lossy like the workspace broadcast: a resuming
    /// surface catches up via `attention/list`, so a dropped nudge self-heals.
    attention_tx: broadcast::Sender<HangarEvent>,
}

impl Default for EventBroker {
    fn default() -> Self {
        Self::new()
    }
}

impl EventBroker {
    /// A fresh broadcast-only broker (no durable outbox). Every emission fans
    /// out to live subscribers and is dropped once no one is listening — the
    /// pre-outbox behaviour, kept for the pool-less test harnesses.
    #[must_use]
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(CHANNEL_CAPACITY);
        let (attention_tx, _arx) = broadcast::channel(CHANNEL_CAPACITY);
        Self {
            tx,
            outbox_tx: None,
            attention_tx,
        }
    }

    /// A broker wired for durable persistence: alongside the live broadcast,
    /// every emission is also queued on a lossless unbounded channel whose
    /// receiver is returned for the outbox drain task ([`crate::event_outbox::spawn`])
    /// to persist with a monotonic `seq`.
    ///
    /// Split from [`Self::new`] so the durable path is opt-in: `boot()` uses this
    /// (and spawns the drain); the RPC/scheduler unit harnesses keep the
    /// broadcast-only [`Self::new`].
    #[must_use]
    pub fn with_outbox() -> (Self, mpsc::UnboundedReceiver<ScopedEvent>) {
        let (tx, _rx) = broadcast::channel(CHANNEL_CAPACITY);
        let (attention_tx, _arx) = broadcast::channel(CHANNEL_CAPACITY);
        let (outbox_tx, outbox_rx) = mpsc::unbounded_channel();
        (
            Self {
                tx,
                outbox_tx: Some(outbox_tx),
                attention_tx,
            },
            outbox_rx,
        )
    }

    /// A cheap-clone publishing handle for a mutation path.
    #[must_use]
    pub fn sink(&self) -> EventSink {
        EventSink {
            tx: self.tx.clone(),
            outbox_tx: self.outbox_tx.clone(),
            attention_tx: self.attention_tx.clone(),
        }
    }

    /// A fresh receiver seeing every event published from now on. The RPC
    /// server opens one per `workspace/subscribe`; dropping it (connection
    /// close) deregisters automatically.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<ScopedEvent> {
        self.tx.subscribe()
    }

    /// A fresh receiver onto the FLEET-WIDE attention stream (spec P2). The RPC
    /// server opens one per `attention/subscribe`; dropping it (connection close)
    /// deregisters automatically. Unlike [`Self::subscribe`] this carries the bare
    /// [`HangarEvent`] (attention is not workspace-scoped in the channel — the
    /// forwarder applies any optional workspace narrowing from the event's own
    /// `workspace_id` field).
    #[must_use]
    pub fn subscribe_attention(&self) -> broadcast::Receiver<HangarEvent> {
        self.attention_tx.subscribe()
    }
}

/// A publishing handle onto the broker, threaded through the daemon's mutation
/// paths (claim loop, RPC mutations, autopilot scheduler).
#[derive(Debug, Clone)]
pub struct EventSink {
    tx: broadcast::Sender<ScopedEvent>,
    outbox_tx: Option<mpsc::UnboundedSender<ScopedEvent>>,
    attention_tx: broadcast::Sender<HangarEvent>,
}

impl EventSink {
    /// Publish `event` scoped to `workspace_id` (the resolved row id).
    ///
    /// Best-effort and non-blocking on BOTH channels: with no live subscriber the
    /// broadcast is dropped silently, and the durable-outbox send onto the
    /// unbounded channel never blocks (nor fails a mutation path — a closed
    /// receiver during shutdown is ignored). A mutation path never stalls or
    /// errors on observability.
    pub fn emit(&self, workspace_id: &str, event: HangarEvent) {
        let scoped = ScopedEvent {
            workspace_id: workspace_id.to_string(),
            event,
        };
        // Durable outbox first (lossless): clone only when a drain is wired.
        if let Some(outbox) = &self.outbox_tx {
            let _ = outbox.send(scoped.clone());
        }
        // Live broadcast (lossy): the per-connection forwarders + inbox aggregator.
        let _ = self.tx.send(scoped);
    }

    /// Publish an attention `event` on the FLEET-WIDE attention stream (spec P2).
    ///
    /// Only `HangarEvent::AttentionRaised` / `AttentionAnswered` belong here. This
    /// is deliberately separate from [`Self::emit`]: attention is not
    /// workspace-scoped (no `workspace_id` beside the event, no event-log outbox
    /// write — the `attention` table is the durable source), so it never hits the
    /// workspace forwarder or the seq log. Best-effort and non-blocking: with no
    /// `attention/subscribe` connection the broadcast is dropped silently, and the
    /// durable row already landed in the store regardless.
    pub fn emit_attention(&self, event: HangarEvent) {
        let _ = self.attention_tx.send(event);
    }
}

/// Frame `event` as a `hangar/event` JSON-RPC notification in the daemon's
/// Content-Length envelope — the exact shape the plugin's `StreamClient`
/// decodes (`{jsonrpc, method, params}`, **no** `id`).
#[must_use]
pub fn encode_event_frame(event: &HangarEvent) -> Vec<u8> {
    let body = serde_json::to_vec(&serde_json::json!({
        "jsonrpc": "2.0",
        "method": EVENT_METHOD,
        "params": event,
    }))
    .unwrap_or_else(|_| b"{}".to_vec());
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    let mut out = Vec::with_capacity(header.len() + body.len());
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(&body);
    out
}

/// Frame an already-serialised event `params` (a durable-log row's stored
/// payload) as a `hangar/event` notification — byte-identical to the live
/// [`encode_event_frame`], but taking the raw JSON so the subscribe-replay path
/// re-emits stored events verbatim without deserialising to [`HangarEvent`] and
/// back. A replayed frame is indistinguishable on the wire from the live one it
/// mirrors.
#[must_use]
pub fn encode_event_frame_payload(params: &serde_json::Value) -> Vec<u8> {
    let body = serde_json::to_vec(&serde_json::json!({
        "jsonrpc": "2.0",
        "method": EVENT_METHOD,
        "params": params,
    }))
    .unwrap_or_else(|_| b"{}".to_vec());
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    let mut out = Vec::with_capacity(header.len() + body.len());
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(&body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_hangar_core::ids::TaskId;
    use ainb_hangar_proto::events::TaskResult;

    fn finished(task: &str) -> HangarEvent {
        HangarEvent::TaskFinished {
            task_id: TaskId::from_str(task.to_string()).unwrap(),
            result: TaskResult::Success,
            ended_at: chrono::DateTime::from_timestamp_millis(1_700_000_000_000).unwrap(),
        }
    }

    /// A subscriber receives an emitted event with its workspace scope intact.
    #[tokio::test]
    async fn emitted_event_reaches_subscriber_with_scope() {
        let broker = EventBroker::new();
        let mut rx = broker.subscribe();
        broker.sink().emit("ws-a", finished("task-1"));
        let scoped = rx.recv().await.unwrap();
        assert_eq!(scoped.workspace_id, "ws-a");
        assert!(matches!(scoped.event, HangarEvent::TaskFinished { .. }));
    }

    /// Emission with no subscriber is a silent no-op (never an error/panic).
    #[test]
    fn emit_without_subscribers_is_noop() {
        let broker = EventBroker::new();
        broker.sink().emit("ws-a", finished("task-1"));
    }

    /// A broker built [`with_outbox`](EventBroker::with_outbox) fans each
    /// emission to BOTH the durable outbox channel and the live broadcast — the
    /// outbox drain sees a gapless stream while live subscribers get instant
    /// feedback.
    #[tokio::test]
    async fn emit_with_outbox_reaches_both_the_durable_channel_and_live_subscribers() {
        let (broker, mut outbox_rx) = EventBroker::with_outbox();
        let mut live = broker.subscribe();
        broker.sink().emit("ws-a", finished("task-7"));

        let durable = outbox_rx.recv().await.expect("durable outbox receives the event");
        assert_eq!(durable.workspace_id, "ws-a");
        assert!(matches!(durable.event, HangarEvent::TaskFinished { .. }));

        let broadcast = live.recv().await.expect("live subscriber receives the event");
        assert_eq!(broadcast.workspace_id, "ws-a");
    }

    /// The broadcast channel is lossy (a lagged subscriber drops events), but the
    /// durable outbox channel is lossless: a burst far beyond the broadcast
    /// capacity is still delivered in full and in order to the outbox drain, so
    /// the `seq` log can never gap.
    #[tokio::test]
    async fn durable_outbox_channel_never_drops_under_a_burst() {
        let (broker, mut outbox_rx) = EventBroker::with_outbox();
        let sink = broker.sink();
        let burst = CHANNEL_CAPACITY * 4;
        for i in 0..burst {
            sink.emit("ws-a", finished(&format!("task-{i}")));
        }
        drop(sink);
        drop(broker);
        let mut received = 0;
        while let Some(_e) = outbox_rx.recv().await {
            received += 1;
        }
        assert_eq!(received, burst, "the durable channel delivers every event");
    }

    fn attention_raised(id: &str) -> HangarEvent {
        HangarEvent::AttentionRaised {
            attention_id: id.to_string(),
            session_id: "sess".into(),
            workspace_id: None,
            kind: "ask_user_question".into(),
            degraded: false,
            created_at: 0,
        }
    }

    /// An attention event reaches an `attention/subscribe` receiver but NOT the
    /// workspace broadcast or the durable outbox — the two streams are isolated
    /// (attention is fleet-wide + table-durable, not workspace-scoped/log-durable).
    #[tokio::test]
    async fn attention_stream_is_isolated_from_the_workspace_stream_and_outbox() {
        let (broker, mut outbox_rx) = EventBroker::with_outbox();
        let mut attention = broker.subscribe_attention();
        let mut workspace = broker.subscribe();

        broker.sink().emit_attention(attention_raised("att-1"));

        // The attention subscriber sees it.
        let got = attention.recv().await.expect("attention subscriber receives it");
        assert!(matches!(got, HangarEvent::AttentionRaised { .. }));

        // Neither the workspace broadcast nor the durable outbox does. Emit a
        // sentinel workspace event so the workspace channels have SOMETHING to
        // deliver — proving the attention event was absent, not merely pending.
        broker.sink().emit("ws-a", finished("t1"));
        let ws = workspace.recv().await.expect("workspace subscriber receives the ws event");
        assert!(
            matches!(ws.event, HangarEvent::TaskFinished { .. }),
            "the first workspace event is the ws one, not the attention one"
        );
        let durable = outbox_rx.recv().await.expect("outbox receives the ws event");
        assert!(
            matches!(durable.event, HangarEvent::TaskFinished { .. }),
            "the outbox never carries an attention event"
        );
    }

    /// The encoded frame is a Content-Length envelope around a notification
    /// (no `id`) whose method is `hangar/event` and whose params carry the
    /// internally-tagged event — byte-decodable by the plugin's frame shape.
    #[test]
    fn encoded_frame_is_a_decodable_notification() {
        let frame = encode_event_frame(&finished("task-9"));
        let text = String::from_utf8(frame).unwrap();
        let (header, body) = text.split_once("\r\n\r\n").unwrap();
        let len: usize = header.strip_prefix("Content-Length: ").unwrap().parse().unwrap();
        assert_eq!(len, body.len(), "header length must match the body");

        let v: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["method"], EVENT_METHOD);
        assert!(v.get("id").is_none(), "a notification must carry no id");
        assert_eq!(v["params"]["event"], "task_finished");
        assert_eq!(v["params"]["task_id"], "task-9");
        assert_eq!(v["params"]["result"], "success");
    }

    /// A replayed frame built from a stored payload is byte-identical to the live
    /// frame for the same event: a resuming subscriber cannot tell a catch-up
    /// event from a live one.
    #[test]
    fn replay_frame_is_byte_identical_to_the_live_frame() {
        let event = finished("task-9");
        let live = encode_event_frame(&event);
        let params = serde_json::to_value(&event).unwrap();
        let replayed = encode_event_frame_payload(&params);
        assert_eq!(live, replayed);
    }
}
