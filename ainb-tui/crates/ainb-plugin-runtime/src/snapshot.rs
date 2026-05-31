//! Versioned snapshot store keyed by topic.
//!
//! Backed by `parking_lot::RwLock<HashMap<Topic, (Bytes, Version)>>`.
//! `parking_lot` chosen so the lock is poison-free and works from sync
//! context (TUI render thread) without needing a tokio guard.
//!
//! Subscriber notification is **not** implemented inside this struct —
//! the runtime task pulls subscribers from a sibling registry and
//! pushes `plugin/handle_event` notifications via the per-plugin
//! channel. Keeping the store dumb keeps tests trivial.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use bytes::Bytes;
use parking_lot::RwLock;

use crate::types::{PluginId, Topic};

/// Monotonic version stamped on each publish.
pub type Version = u64;

/// Latest snapshot entry stored in the [`SnapshotStore`].
#[derive(Debug, Clone)]
pub struct SnapshotEntry {
    /// Topic key.
    pub topic: Topic,
    /// Published bytes.
    pub payload: Bytes,
    /// Monotonic version. Starts at 1 on first publish.
    pub version: Version,
}

/// Concurrent snapshot store.
///
/// Cheap to clone (single `Arc`). All mutating ops take a write lock;
/// readers take a read lock and are wait-free relative to each other.
#[derive(Debug, Clone, Default)]
pub struct SnapshotStore {
    inner: Arc<RwLock<Inner>>,
}

#[derive(Debug, Default)]
struct Inner {
    entries: HashMap<Topic, (Bytes, Version)>,
    /// Subscribers per topic, keyed by topic. Stored here so the store
    /// can answer "who subscribes to X?" in one read lock.
    subscribers: HashMap<Topic, HashSet<PluginId>>,
    /// Monotonic global counter used to stamp every publish.
    counter: Version,
}

impl SnapshotStore {
    /// Construct an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Publish a payload under `topic`. Returns the new version.
    #[must_use]
    pub fn publish(&self, topic: Topic, payload: Bytes) -> Version {
        let mut inner = self.inner.write();
        inner.counter += 1;
        let version = inner.counter;
        inner.entries.insert(topic, (payload, version));
        version
    }

    /// Latest payload for `topic`, plus its version.
    #[must_use]
    pub fn get(&self, topic: &Topic) -> Option<(Bytes, Version)> {
        let inner = self.inner.read();
        inner.entries.get(topic).cloned()
    }

    /// Latest payload only (for the sync `RuntimeHandle::snapshot_get`).
    #[must_use]
    pub fn payload(&self, topic: &Topic) -> Option<Bytes> {
        self.get(topic).map(|(p, _)| p)
    }

    /// Register `plugin` as a subscriber to `topic`.
    pub fn subscribe(&self, topic: Topic, plugin: PluginId) {
        self.inner.write().subscribers.entry(topic).or_default().insert(plugin);
    }

    /// Drop a subscriber. Used by lifecycle teardown.
    pub fn unsubscribe_all(&self, plugin: &PluginId) {
        let mut inner = self.inner.write();
        for set in inner.subscribers.values_mut() {
            set.remove(plugin);
        }
    }

    /// Snapshot of subscribers for `topic`.
    #[must_use]
    pub fn subscribers(&self, topic: &Topic) -> Vec<PluginId> {
        self.inner
            .read()
            .subscribers
            .get(topic)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Snapshot of all `(topic, version)` pairs. Test helper.
    #[must_use]
    pub fn topics(&self) -> Vec<(Topic, Version)> {
        self.inner.read().entries.iter().map(|(t, (_, v))| (t.clone(), *v)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_then_get_round_trips() {
        let s = SnapshotStore::new();
        let v = s.publish(Topic::from("t"), Bytes::from_static(b"hello"));
        assert_eq!(v, 1);
        let (p, v2) = s.get(&Topic::from("t")).unwrap();
        assert_eq!(&p[..], b"hello");
        assert_eq!(v2, 1);
    }

    #[test]
    fn version_monotonic_across_topics() {
        let s = SnapshotStore::new();
        let v1 = s.publish(Topic::from("a"), Bytes::from_static(b"1"));
        let v2 = s.publish(Topic::from("b"), Bytes::from_static(b"2"));
        let v3 = s.publish(Topic::from("a"), Bytes::from_static(b"3"));
        assert!(v1 < v2 && v2 < v3);
    }

    #[test]
    fn subscribe_and_unsubscribe() {
        let s = SnapshotStore::new();
        let p = PluginId::from("burndown");
        s.subscribe(Topic::from("t"), p.clone());
        assert_eq!(s.subscribers(&Topic::from("t")), vec![p.clone()]);
        s.unsubscribe_all(&p);
        assert!(s.subscribers(&Topic::from("t")).is_empty());
    }

    #[test]
    fn missing_topic_returns_none() {
        let s = SnapshotStore::new();
        assert!(s.get(&Topic::from("nope")).is_none());
        assert!(s.payload(&Topic::from("nope")).is_none());
    }
}
