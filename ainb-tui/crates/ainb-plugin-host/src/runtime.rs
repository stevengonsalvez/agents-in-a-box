//! Per-plugin wasmi state + cross-plugin shared state.
//!
//! Each loaded plugin owns its own [`wasmi::Store`] parameterised over
//! [`HostState`]. The Store keeps the plugin's linear memory, table, and
//! per-call mutable state. Capability-gated host fns mutate `HostState`
//! directly; the cross-plugin event bus mutates [`HostShared`] through the
//! `Arc` each `HostState` holds.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use ainb_plugin_api::{CapabilitySet, RenderTarget, WireBuffer};

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
    /// Bytes the plugin wrote to fd 1 (stdout) via the wasi `fd_write`
    /// shim. Used by `PluginHost::dispatch_cli` to harvest the output of
    /// plugin-handled CLI commands. The CLI dispatch path drains this
    /// before each call; long-running renders never touch it.
    pub captured_stdout: Vec<u8>,
    /// Bytes the plugin wrote to fd 2 (stderr). Surfaces via the same
    /// dispatch_cli path; useful for diagnosing parse errors.
    pub captured_stderr: Vec<u8>,
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
            captured_stdout: Vec::new(),
            captured_stderr: Vec::new(),
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
/// load time. Each field is guarded by its own mutex so a publish doesn't
/// block a concurrent subscribe (and a render-buffer write doesn't block
/// either).
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
    /// Most recent `WireBuffer` painted by each plugin, keyed by
    /// `(plugin_id, render_target)`. Plugins write here through the
    /// `ainb_render_buffer` host-fn during their `_render` call; ainb-core's
    /// screen layer drains the entry it cares about right after triggering
    /// `PluginHost::render_plugin`. Older buffers are overwritten in place.
    pub pending_renders: Mutex<HashMap<(String, RenderTarget), WireBuffer>>,
    /// `correlation_id → reply bytes`. Populated by the cross-plugin
    /// pump when a subscriber publishes on `rep:<topic>` with a known
    /// correlation id; drained by [`Self::take_reply`] inside the
    /// `ainb_request_data` host-fn.
    ///
    /// Phase 6a foundation only: the host-fn primitive + this ledger
    /// land here so Phase 6c-cli can wire the actual pump-driven reply
    /// route without re-touching the wasmi linker.
    pub pending_replies: Mutex<HashMap<u64, Vec<u8>>>,
    /// Set of correlation ids currently being awaited by an in-flight
    /// `ainb_request_data` call. Inserted on request, removed on
    /// completion, timeout, or cancel.
    ///
    /// `publish_reply_impl` checks this before inserting into
    /// `pending_replies` so a late reply (whose requester has already
    /// timed out) is dropped instead of leaking memory in the ledger
    /// forever. Replies with `correlation_id == 0` are also dropped
    /// — registry.rs's `inject_session_reader_snapshot` broker uses 0
    /// as a sentinel meaning "this is a one-way push, no reply
    /// expected".
    pub inflight: Mutex<HashSet<u64>>,
    /// Monotonic counter for correlation ids handed out by
    /// [`Self::next_correlation_id`]. `AtomicU64` so the host-fn closure
    /// can advance it without locking.
    correlation_counter: AtomicU64,
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

    /// Drop a plugin's subscriptions and any pending renders. Called on
    /// `PluginHost::shutdown` so a re-load doesn't see stale buffers.
    pub fn drop_plugin(&self, plugin_id: &str) {
        let mut subs = self.subscriptions.lock().expect("subscriptions poisoned");
        for v in subs.values_mut() {
            v.retain(|p| p != plugin_id);
        }
        let mut renders = self.pending_renders.lock().expect("pending_renders poisoned");
        renders.retain(|(p, _), _| p != plugin_id);
    }

    /// Stash a freshly painted [`WireBuffer`] from a plugin. Called by the
    /// `ainb_render_buffer` host-fn; overwrites whatever was there before.
    pub fn store_render(&self, plugin_id: &str, target: RenderTarget, buf: WireBuffer) {
        let mut renders = self.pending_renders.lock().expect("pending_renders poisoned");
        renders.insert((plugin_id.to_string(), target), buf);
    }

    /// Take (and clear) the most recent render for a plugin/target pair.
    /// Returns `None` when the plugin hasn't painted to this target since the
    /// previous take.
    #[must_use]
    pub fn take_render(&self, plugin_id: &str, target: RenderTarget) -> Option<WireBuffer> {
        let mut renders = self.pending_renders.lock().expect("pending_renders poisoned");
        renders.remove(&(plugin_id.to_string(), target))
    }

    /// Hand out the next correlation id for a synchronous request. Counter
    /// starts at 1 so a 0 id can mean "no request" in plugin-side wire
    /// formats that need a sentinel.
    pub fn next_correlation_id(&self) -> u64 {
        self.correlation_counter.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Park a reply payload for the request-data host-fn to pick up.
    /// Called from [`crate::PluginHost::pump_events`] when a subscriber
    /// publishes on the `rep:<topic>` channel.
    ///
    /// Replies are dropped (not parked) when:
    /// - `correlation_id == 0`: sentinel used by the
    ///   `inject_session_reader_snapshot` broker for one-way pushes;
    ///   no requester is waiting.
    /// - the id is not in `inflight`: the requester already timed out
    ///   and gave up on this id, so parking the reply would leak it
    ///   in `pending_replies` forever.
    pub fn put_reply(&self, correlation_id: u64, payload: Vec<u8>) {
        if correlation_id == 0 {
            return;
        }
        let inflight = self.inflight.lock().expect("inflight poisoned");
        if !inflight.contains(&correlation_id) {
            return;
        }
        drop(inflight);
        let mut q = self.pending_replies.lock().expect("pending_replies poisoned");
        q.insert(correlation_id, payload);
    }

    /// Drain the reply for `correlation_id`, if any.
    #[must_use]
    pub fn take_reply(&self, correlation_id: u64) -> Option<Vec<u8>> {
        let mut q = self.pending_replies.lock().expect("pending_replies poisoned");
        q.remove(&correlation_id)
    }

    /// Mark a correlation id as in-flight. Call when emitting a
    /// `req:<topic>` event so subsequent `put_reply` calls accept the
    /// matching reply.
    pub fn mark_inflight(&self, correlation_id: u64) {
        let mut inflight = self.inflight.lock().expect("inflight poisoned");
        inflight.insert(correlation_id);
    }

    /// Forget a correlation id and discard any parked reply. Call on
    /// request completion (success or timeout) so a late reply for
    /// this id is dropped instead of poisoning the next request that
    /// reuses the slot (counter is monotonic, but defensive cleanup
    /// is still cheap).
    pub fn clear_inflight(&self, correlation_id: u64) {
        {
            let mut inflight = self.inflight.lock().expect("inflight poisoned");
            inflight.remove(&correlation_id);
        }
        let mut q = self.pending_replies.lock().expect("pending_replies poisoned");
        q.remove(&correlation_id);
    }
}

#[cfg(test)]
mod render_buffer_tests {
    use super::*;
    use ainb_plugin_api::{RenderTarget, WireBuffer};

    #[test]
    fn store_then_take_returns_buffer_once() {
        let shared = HostShared::default();
        let buf = WireBuffer::empty(2, 1);
        shared.store_render("burndown", RenderTarget::Screen, buf.clone());

        // First take returns the buffer; second returns None (drained).
        assert_eq!(shared.take_render("burndown", RenderTarget::Screen), Some(buf));
        assert_eq!(shared.take_render("burndown", RenderTarget::Screen), None);
    }

    #[test]
    fn store_distinguishes_target_and_plugin() {
        let shared = HostShared::default();
        let screen = WireBuffer::empty(80, 24);
        let sidebar = WireBuffer::empty(20, 24);
        shared.store_render("burndown", RenderTarget::Screen, screen.clone());
        shared.store_render("burndown", RenderTarget::Sidebar, sidebar.clone());
        shared.store_render("kanban", RenderTarget::Screen, WireBuffer::empty(1, 1));

        assert_eq!(
            shared.take_render("burndown", RenderTarget::Sidebar),
            Some(sidebar)
        );
        // Other (plugin, target) pairs are unaffected.
        assert_eq!(
            shared.take_render("burndown", RenderTarget::Screen),
            Some(screen)
        );
        assert!(shared.take_render("kanban", RenderTarget::Screen).is_some());
    }

    #[test]
    fn drop_plugin_clears_only_its_renders() {
        let shared = HostShared::default();
        shared.store_render("burndown", RenderTarget::Screen, WireBuffer::empty(1, 1));
        shared.store_render("kanban", RenderTarget::Screen, WireBuffer::empty(1, 1));
        shared.drop_plugin("burndown");

        assert!(shared.take_render("burndown", RenderTarget::Screen).is_none());
        assert!(shared.take_render("kanban", RenderTarget::Screen).is_some());
    }
}
