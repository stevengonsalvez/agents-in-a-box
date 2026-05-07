//! Per-plugin wasmi state + cross-plugin shared state.
//!
//! Each loaded plugin owns its own [`wasmi::Store`] parameterised over
//! [`HostState`]. The Store keeps the plugin's linear memory, table, and
//! per-call mutable state. Capability-gated host fns mutate `HostState`
//! directly; the cross-plugin event bus mutates [`HostShared`] through the
//! `Arc` each `HostState` holds.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use ainb_plugin_api::CapabilitySet;

/// Per-plugin mutable state carried inside its wasmi `Store`.
#[derive(Debug)]
pub struct HostState {
    pub plugin_id: String,
    pub capabilities: CapabilitySet,
    /// Captured `ainb_log` payloads; bounded to `LOG_RING_CAPACITY` entries to
    /// keep tests deterministic and avoid unbounded growth in long-running
    /// hosts.
    pub log_ring: Vec<LoggedLine>,
    /// Set by any host-fn that returned a negative status; cleared by the
    /// caller before each plugin call.
    pub last_error: Option<String>,
    /// Shared cross-plugin state — event bus subscriptions and outbound
    /// queue. Cloning the `Arc` keeps host-fns cheap; the `Mutex` is held
    /// only briefly inside `ainb_event_subscribe` / `ainb_event_publish`.
    pub shared: Arc<HostShared>,
}

const LOG_RING_CAPACITY: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggedLine {
    pub level: i32,
    pub msg: String,
}

impl HostState {
    #[must_use]
    pub fn new(
        plugin_id: impl Into<String>,
        caps: CapabilitySet,
        shared: Arc<HostShared>,
    ) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            capabilities: caps,
            log_ring: Vec::new(),
            last_error: None,
            shared,
        }
    }

    pub fn push_log(&mut self, level: i32, msg: String) {
        if self.log_ring.len() >= LOG_RING_CAPACITY {
            self.log_ring.remove(0);
        }
        self.log_ring.push(LoggedLine { level, msg });
    }
}

/// State shared across every plugin loaded into the same `PluginHost`.
///
/// The host clones one `Arc<HostShared>` into each plugin's [`HostState`] at
/// load time. Both fields are guarded by separate mutexes to keep contention
/// low — a publish doesn't block a concurrent subscribe.
#[derive(Debug, Default)]
pub struct HostShared {
    /// `topic → list-of-subscriber-plugin-ids`. Subscriptions are append-only
    /// for the lifetime of the host; unsubscribing on plugin shutdown happens
    /// in [`crate::PluginHost::shutdown`].
    pub subscriptions: Mutex<HashMap<String, Vec<String>>>,
    /// Outbound queue of events waiting to be dispatched. Each entry is
    /// `(publisher_plugin_id, topic, payload)`. The host drains the queue in
    /// `pump_events`, calling each subscriber's `_handle_event` export.
    pub event_queue: Mutex<Vec<QueuedEvent>>,
}

#[derive(Debug, Clone)]
pub struct QueuedEvent {
    pub publisher: String,
    pub topic: String,
    pub payload: Vec<u8>,
}

impl HostShared {
    pub fn subscribe(&self, plugin_id: &str, topic: String) {
        let mut subs = self.subscriptions.lock().expect("subscriptions poisoned");
        let list = subs.entry(topic).or_default();
        if !list.iter().any(|p| p == plugin_id) {
            list.push(plugin_id.to_string());
        }
    }

    pub fn publish(&self, publisher: &str, topic: String, payload: Vec<u8>) {
        let mut q = self.event_queue.lock().expect("event_queue poisoned");
        q.push(QueuedEvent {
            publisher: publisher.to_string(),
            topic,
            payload,
        });
    }

    /// Drain queued events. Returns `(events, subscriptions-snapshot)` so the
    /// caller can dispatch without holding either mutex while invoking wasmi.
    pub fn drain_events(&self) -> (Vec<QueuedEvent>, HashMap<String, Vec<String>>) {
        let events = std::mem::take(
            &mut *self
                .event_queue
                .lock()
                .expect("event_queue poisoned"),
        );
        let subs = self
            .subscriptions
            .lock()
            .expect("subscriptions poisoned")
            .clone();
        (events, subs)
    }

    /// Drop a plugin's subscriptions. Called on `PluginHost::shutdown`.
    pub fn drop_plugin(&self, plugin_id: &str) {
        let mut subs = self.subscriptions.lock().expect("subscriptions poisoned");
        for v in subs.values_mut() {
            v.retain(|p| p != plugin_id);
        }
    }
}
