//! Topic-keyed pub/sub event bus — Phase 2c.
//!
//! Minimal surface for the Phase 3 burndown plugin to subscribe to host
//! lifecycle events (`session.started`, `session.closed`) without coupling
//! the host to the plugin's event names. Cross-plugin pub/sub now lives
//! in `ainb_plugin_runtime::SnapshotStore`; this in-core bus is the
//! complement that hosts use to publish *to* plugins.
//!
//! ## Wire format
//!
//! `payload` is `&[u8]` — by convention rmp-serde of the topic-specific
//! event struct, mirroring the format used by the plugin host. The bus
//! itself is payload-agnostic so non-plugin in-core subscribers can use any
//! encoding they like.
//!
//! ## Threading
//!
//! Single-threaded; the TUI dispatch loop owns the bus. Plugin handlers are
//! invoked synchronously during `publish`. Long-running work should be
//! offloaded by the handler.

use std::collections::HashMap;

/// Synchronous handler invoked once per matching `publish`.
pub type EventHandler = Box<dyn FnMut(&[u8]) + Send>;

/// Lookups are by `&'static str` so subscribers don't pay an allocation per
/// publish. Topics are constants like `"session.started"`.
#[derive(Default)]
pub struct EventBus {
    subscribers: HashMap<&'static str, Vec<EventHandler>>,
}

impl EventBus {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `handler` to be called for every `publish(topic, ...)`.
    /// Multiple handlers per topic are allowed and run in registration order.
    pub fn subscribe(&mut self, topic: &'static str, handler: EventHandler) {
        self.subscribers.entry(topic).or_default().push(handler);
    }

    /// Invoke every handler registered for `topic`. Returns the number of
    /// handlers that ran — `0` for unknown topics. Errors raised inside
    /// handlers are not propagated; handlers are expected to log internally.
    pub fn publish(&mut self, topic: &'static str, payload: &[u8]) -> usize {
        match self.subscribers.get_mut(topic) {
            Some(handlers) => {
                for h in handlers.iter_mut() {
                    h(payload);
                }
                handlers.len()
            }
            None => 0,
        }
    }

    /// `true` if at least one subscriber exists for `topic`.
    pub fn has_subscribers(&self, topic: &'static str) -> bool {
        self.subscribers.get(topic).is_some_and(|v| !v.is_empty())
    }

    /// Total topic count (useful for stats / debugging).
    #[must_use]
    pub fn topic_count(&self) -> usize {
        self.subscribers.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn event_bus_subscribe_and_publish() {
        let received = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
        let mut bus = EventBus::new();

        let cap = received.clone();
        bus.subscribe(
            "session.started",
            Box::new(move |payload: &[u8]| {
                cap.lock().unwrap().push(payload.to_vec());
            }),
        );

        let n = bus.publish("session.started", b"hello");
        assert_eq!(n, 1, "one subscriber should fire");
        assert_eq!(*received.lock().unwrap(), vec![b"hello".to_vec()]);
    }

    #[test]
    fn publish_to_unknown_topic_returns_zero() {
        let mut bus = EventBus::new();
        assert_eq!(bus.publish("nobody.cares", b""), 0);
    }

    #[test]
    fn multiple_subscribers_all_fire_in_order() {
        let order = Arc::new(Mutex::new(Vec::<u8>::new()));
        let mut bus = EventBus::new();

        for tag in [1u8, 2, 3] {
            let cap = order.clone();
            bus.subscribe(
                "tick",
                Box::new(move |_payload: &[u8]| {
                    cap.lock().unwrap().push(tag);
                }),
            );
        }

        let n = bus.publish("tick", &[]);
        assert_eq!(n, 3);
        assert_eq!(*order.lock().unwrap(), vec![1, 2, 3]);
    }

    #[test]
    fn has_subscribers_reports_topic_state() {
        let mut bus = EventBus::new();
        assert!(!bus.has_subscribers("foo"));
        bus.subscribe("foo", Box::new(|_| {}));
        assert!(bus.has_subscribers("foo"));
        assert_eq!(bus.topic_count(), 1);
    }
}
