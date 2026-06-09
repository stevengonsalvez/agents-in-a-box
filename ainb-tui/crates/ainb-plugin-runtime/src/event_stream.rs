//! Host-side event-stream registry for `host/event_stream_subscribe`.
//!
//! An *event stream* is a cap-gated, host-allocated subscription on a
//! topic. Unlike the [`crate::snapshot::SnapshotStore`] subscriber set
//! (which is keyed by topic and fans the raw topic back to the plugin),
//! an event stream is keyed by a **host-minted opaque `stream_id`** and
//! re-emits every matching publish under topic `stream:<stream_id>`.
//!
//! Two reasons for the indirection:
//!
//! - **Anti-forgery.** The `stream_id` is allocated by the host (ULID),
//!   so a plugin can't fabricate one to read another plugin's stream.
//! - **Multiplexing.** A plugin can hold many concurrent streams on the
//!   same or different topics; each gets its own id and its own
//!   `stream:<id>` delivery topic, so the plugin can demux without extra
//!   per-event bookkeeping.
//!
//! ## Cancellation / lifecycle
//!
//! Streams are dropped when the owning plugin leaves the `Running`
//! state — explicit `host/event_stream_cancel`, plugin shutdown, crash,
//! or quarantine. [`EventStreamRegistry::drop_plugin`] is the teardown
//! hook the per-plugin task calls from `kill_child`.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::RwLock;

use crate::types::{PluginId, Topic};

/// Host-minted, unforgeable stream identifier.
///
/// Backed by a ULID so it is monotonic, time-sortable, and opaque to
/// the plugin. The plugin echoes it back on `host/event_stream_cancel`
/// and observes events under topic `stream:<id>`.
pub type StreamId = String;

/// Allocator for [`StreamId`]s.
///
/// Wraps a ULID generator behind a monotonic guard so two allocations
/// in the same millisecond still produce strictly increasing,
/// distinct ids (ULID's monotonic mode). Cheap to clone — shares one
/// `Arc`.
#[derive(Clone)]
pub struct StreamIdGen {
    /// Tie-breaker bumped per allocation so collisions are impossible
    /// even if the system clock is frozen in tests.
    seq: Arc<AtomicU64>,
}

impl std::fmt::Debug for StreamIdGen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamIdGen").finish_non_exhaustive()
    }
}

impl Default for StreamIdGen {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamIdGen {
    /// Construct a fresh allocator.
    #[must_use]
    pub fn new() -> Self {
        Self {
            seq: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Allocate the next opaque, unforgeable stream id.
    ///
    /// The ULID supplies the host-minted, time-sortable identity; a
    /// per-allocator sequence suffix guarantees uniqueness even under a
    /// frozen clock (the property the anti-forgery + multiplexing tests
    /// rely on).
    pub fn allocate(&self) -> StreamId {
        let n = self.seq.fetch_add(1, Ordering::Relaxed);
        format!("{}-{n:08x}", ulid::Ulid::new())
    }
}

/// One live event stream: which plugin owns it and which topic it reads.
#[derive(Debug, Clone)]
struct Stream {
    plugin: PluginId,
    topic: Topic,
}

/// Concurrent registry of live event streams.
///
/// Cheap to clone (single `Arc`). Shared between the [`crate::Runtime`]
/// (for host-side `publish_stream_event` fan-out) and each per-plugin
/// task (for subscribe / cancel / teardown).
#[derive(Clone, Default)]
pub struct EventStreamRegistry {
    inner: Arc<RwLock<HashMap<StreamId, Stream>>>,
    ids: StreamIdGen,
}

impl std::fmt::Debug for EventStreamRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventStreamRegistry")
            .field("live_streams", &self.inner.read().len())
            .finish_non_exhaustive()
    }
}

impl EventStreamRegistry {
    /// Construct an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Open a new stream for `plugin` on `topic`. Returns the
    /// host-minted [`StreamId`] the plugin will cancel with and observe
    /// events under (`stream:<id>`).
    pub fn subscribe(&self, plugin: PluginId, topic: Topic) -> StreamId {
        let id = self.ids.allocate();
        self.inner.write().insert(id.clone(), Stream { plugin, topic });
        id
    }

    /// Cancel a single stream by id. Returns `true` if a stream was
    /// removed (i.e. the id was live and owned).
    ///
    /// Ownership is enforced: a plugin can only cancel a stream it owns,
    /// defending against one plugin tearing down another's stream by
    /// guessing an id.
    pub fn cancel(&self, plugin: &PluginId, id: &str) -> bool {
        let mut map = self.inner.write();
        match map.get(id) {
            Some(s) if &s.plugin == plugin => {
                map.remove(id);
                true
            }
            _ => false,
        }
    }

    /// Drop every stream owned by `plugin`. Called on plugin teardown
    /// (shutdown / crash / quarantine) so no events leak to a dead
    /// process. Returns the number of streams dropped.
    pub fn drop_plugin(&self, plugin: &PluginId) -> usize {
        let mut map = self.inner.write();
        let before = map.len();
        map.retain(|_, s| &s.plugin != plugin);
        before - map.len()
    }

    /// Every `(plugin, stream_id)` currently subscribed to `topic`
    /// (exact-match). The host's `publish_stream_event` walks this list
    /// and emits `plugin/handle_event` under `stream:<id>` for each.
    #[must_use]
    pub fn streams_for_topic(&self, topic: &Topic) -> Vec<(PluginId, StreamId)> {
        self.inner
            .read()
            .iter()
            .filter(|(_, s)| &s.topic == topic)
            .map(|(id, s)| (s.plugin.clone(), id.clone()))
            .collect()
    }

    /// Number of live streams. Test/diagnostic helper.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.read().len()
    }

    /// Whether the registry holds no streams.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.read().is_empty()
    }
}

/// The delivery topic a stream's events arrive under: `stream:<id>`.
#[must_use]
pub fn stream_topic(id: &str) -> String {
    format!("stream:{id}")
}

/// Check a requested `topic` against an `event_stream_subscribe`
/// capability allow-list.
///
/// Semantics (locked by P3.1/P3.2 contract):
/// - **bool-true grant** (`allow_list == None` after the caller has
///   already confirmed `is_granted()`): wildcard — any topic permitted.
/// - **list grant**: each entry is a prefix pattern. A trailing `*`
///   matches any suffix (`workspace:*` allows `workspace:default`); an
///   entry without `*` must match the topic exactly.
///
/// The caller is responsible for the prior `is_granted()` gate — this
/// function only answers "does the allow-list permit this topic?".
#[must_use]
pub fn topic_allowed(allow_list: Option<&[String]>, topic: &str) -> bool {
    // `None` = bool-true grant: wildcard across all topics.
    allow_list.is_none_or(|patterns| patterns.iter().any(|p| pattern_matches(p, topic)))
}

/// Match a single allow-list pattern against a topic.
///
/// `prefix*` matches any topic starting with `prefix`; an exact pattern
/// matches only itself. A bare `*` matches everything.
fn pattern_matches(pattern: &str, topic: &str) -> bool {
    pattern
        .strip_suffix('*')
        .map_or_else(|| pattern == topic, |prefix| topic.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pid(s: &str) -> PluginId {
        PluginId::from(s)
    }

    fn topic(s: &str) -> Topic {
        Topic::from(s)
    }

    #[test]
    fn subscribe_allocates_unique_unforgeable_ids() {
        let r = EventStreamRegistry::new();
        let a = r.subscribe(pid("p"), topic("workspace:default"));
        let b = r.subscribe(pid("p"), topic("workspace:default"));
        assert_ne!(a, b, "stream ids must be unique even for the same topic");
        // Host-minted: non-empty, opaque.
        assert!(!a.is_empty());
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn streams_for_topic_returns_only_matching() {
        let r = EventStreamRegistry::new();
        let s1 = r.subscribe(pid("p1"), topic("workspace:default"));
        let _s2 = r.subscribe(pid("p2"), topic("workspace:other"));
        let matches = r.streams_for_topic(&topic("workspace:default"));
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].0, pid("p1"));
        assert_eq!(matches[0].1, s1);
    }

    #[test]
    fn cancel_removes_only_owned_stream() {
        let r = EventStreamRegistry::new();
        let id = r.subscribe(pid("owner"), topic("t"));
        // A different plugin cannot cancel someone else's stream.
        assert!(!r.cancel(&pid("intruder"), &id));
        assert_eq!(r.len(), 1, "intruder must not be able to cancel");
        // The owner can.
        assert!(r.cancel(&pid("owner"), &id));
        assert_eq!(r.len(), 0);
        // Cancelling again is a no-op.
        assert!(!r.cancel(&pid("owner"), &id));
    }

    #[test]
    fn drop_plugin_clears_all_its_streams() {
        let r = EventStreamRegistry::new();
        r.subscribe(pid("p1"), topic("a"));
        r.subscribe(pid("p1"), topic("b"));
        r.subscribe(pid("p2"), topic("a"));
        let dropped = r.drop_plugin(&pid("p1"));
        assert_eq!(dropped, 2);
        assert_eq!(r.len(), 1);
        // p2's stream survives.
        assert_eq!(r.streams_for_topic(&topic("a")).len(), 1);
    }

    #[test]
    fn stream_topic_prefixes_id() {
        assert_eq!(stream_topic("01ABC"), "stream:01ABC");
    }

    #[test]
    fn topic_allowed_bool_true_is_wildcard() {
        assert!(topic_allowed(None, "anything:goes"));
    }

    #[test]
    fn topic_allowed_prefix_pattern() {
        let list = vec!["workspace:*".to_string(), "stream:*".to_string()];
        assert!(topic_allowed(Some(&list), "workspace:default"));
        assert!(topic_allowed(Some(&list), "stream:01ABC"));
        assert!(!topic_allowed(Some(&list), "secret:keys"));
    }

    #[test]
    fn topic_allowed_exact_pattern_no_wildcard() {
        let list = vec!["workspace:default".to_string()];
        assert!(topic_allowed(Some(&list), "workspace:default"));
        assert!(!topic_allowed(Some(&list), "workspace:other"));
        // Exact pattern must NOT prefix-match.
        assert!(!topic_allowed(Some(&list), "workspace:default:sub"));
    }

    #[test]
    fn topic_allowed_empty_list_denies() {
        let list: Vec<String> = vec![];
        assert!(!topic_allowed(Some(&list), "workspace:default"));
    }
}
