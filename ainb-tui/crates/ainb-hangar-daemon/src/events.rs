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
use tokio::sync::broadcast;

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
#[derive(Debug, Clone)]
pub struct EventBroker {
    tx: broadcast::Sender<ScopedEvent>,
}

impl Default for EventBroker {
    fn default() -> Self {
        Self::new()
    }
}

impl EventBroker {
    /// A fresh broker with no subscribers.
    #[must_use]
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(CHANNEL_CAPACITY);
        Self { tx }
    }

    /// A cheap-clone publishing handle for a mutation path.
    #[must_use]
    pub fn sink(&self) -> EventSink {
        EventSink {
            tx: self.tx.clone(),
        }
    }

    /// A fresh receiver seeing every event published from now on. The RPC
    /// server opens one per `workspace/subscribe`; dropping it (connection
    /// close) deregisters automatically.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<ScopedEvent> {
        self.tx.subscribe()
    }
}

/// A publishing handle onto the broker, threaded through the daemon's mutation
/// paths (claim loop, RPC mutations, autopilot scheduler).
#[derive(Debug, Clone)]
pub struct EventSink {
    tx: broadcast::Sender<ScopedEvent>,
}

impl EventSink {
    /// Publish `event` scoped to `workspace_id` (the resolved row id).
    ///
    /// Best-effort and non-blocking: with no live subscriber the event is
    /// dropped silently — a mutation path never fails or stalls on
    /// observability.
    pub fn emit(&self, workspace_id: &str, event: HangarEvent) {
        let _ = self.tx.send(ScopedEvent {
            workspace_id: workspace_id.to_string(),
            event,
        });
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
}
