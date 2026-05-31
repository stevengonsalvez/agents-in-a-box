//! Plugin state — lifecycle + cache + UI mode for the witr plugin.
//!
//! cfx.5 introduces three orthogonal pieces of state:
//!
//! 1. **Lifecycle** ([`Lifecycle`]) — whether witr was detected,
//!    found-but-too-old, or genuinely missing. Drives the empty-state
//!    painter (cfx.4) vs the tab UI (cfx.5).
//! 2. **UI mode** ([`UiMode`]) — which tab is focused, whether the
//!    user is currently typing a target, whether a detail overlay
//!    is open (overlay rendering itself lands in cfx.6).
//! 3. **Snapshot cache** ([`SnapshotCache`]) — LRU 8-slot, 60s TTL.
//!    Coalesces rapid `r` presses; keyed by the target string. Idle
//!    plugins drop nothing because the cache is bounded.
//!
//! No persistence — flushed at shutdown. Cfx.8 (event-bus publisher)
//! reads from the same cache when emitting `witr.snapshot`.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use semver::Version;

use crate::detect::DetectResult;
use crate::model::WitrSnapshot;

/// Whether the plugin can talk to witr right now. Driven by cfx.2's
/// detect; consumed by the render dispatcher (cfx.5 = paint tab UI
/// only when `Ready`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lifecycle {
    /// Detect hasn't run yet (or was abandoned). Plugin should render
    /// a transient "checking witr…" line.
    Unknown,
    /// Witr is on PATH and meets the minimum version. Tab UI active.
    Ready,
    /// Detect returned a `Missing` variant. Empty-state painter
    /// (cfx.4) shows install instructions.
    Missing(crate::detect::MissingReason),
    /// Detect returned `Outdated`. Empty-state painter shows upgrade
    /// instructions.
    Outdated {
        /// The version witr reported.
        found_version: Version,
        /// The minimum the plugin requires (== `detect::MIN_VERSION`).
        minimum: Version,
    },
}

impl Lifecycle {
    /// Construct a [`Lifecycle`] from a [`DetectResult`]. Pure
    /// translation — no side effects.
    #[must_use]
    pub fn from_detect(result: DetectResult) -> Self {
        match result {
            DetectResult::Ready { .. } => Self::Ready,
            DetectResult::Missing(reason) => Self::Missing(reason),
            DetectResult::Outdated {
                found_version,
                minimum,
            } => Self::Outdated {
                found_version,
                minimum,
            },
        }
    }
}

/// Which tab the user is currently focused on. Index lines up with
/// witr's CLI structure (Processes / Ports / Containers / Locks).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tab {
    /// Default tab — process ancestry chain.
    Processes,
    /// Sockets owned by the target process.
    Ports,
    /// Container the target lives inside, if any.
    Containers,
    /// File locks held / waited on.
    Locks,
}

impl Tab {
    /// Stable display label for the tab strip.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Processes => "Processes",
            Self::Ports => "Ports",
            Self::Containers => "Containers",
            Self::Locks => "Locks",
        }
    }

    /// 0-based index. Matches the `1/2/3/4` key handler.
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Processes => 0,
            Self::Ports => 1,
            Self::Containers => 2,
            Self::Locks => 3,
        }
    }

    /// Reverse of [`Tab::index`] — used by `1/2/3/4` key handling.
    /// Returns `None` for out-of-range indices so callers can ignore
    /// stray digit presses.
    #[must_use]
    pub const fn from_index(i: usize) -> Option<Self> {
        match i {
            0 => Some(Self::Processes),
            1 => Some(Self::Ports),
            2 => Some(Self::Containers),
            3 => Some(Self::Locks),
            _ => None,
        }
    }

    /// All four tabs in their canonical order. Used to render the
    /// tab strip without hand-listing them at the call site.
    pub const ALL: [Self; 4] = [Self::Processes, Self::Ports, Self::Containers, Self::Locks];
}

/// What the user is currently doing inside the witr screen. Drives
/// what `handle_key` does with non-tab keys.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum UiMode {
    /// Normal browsing — keys 1/2/3/4 switch tabs, r refreshes,
    /// t enters target-input mode, / opens detail.
    #[default]
    Browsing,
    /// User pressed `t` and is typing a new target. `buffer` is the
    /// in-progress string. Enter commits + triggers a scan; Esc
    /// cancels.
    EnteringTarget { buffer: String },
    /// Detail overlay open over the current tab. Q closes; the
    /// overlay itself is painted by cfx.6.
    DetailOpen,
}

/// LRU snapshot cache. 8 slots, 60-second TTL per the spec
/// (`plans/witr-plugin-spec.md` § Performance).
///
/// Internal storage is `Vec<(target, when, Arc<snapshot>)>` ordered
/// from oldest to newest. On insert past capacity we drop the head
/// (oldest). On lookup we walk linearly — N=8 makes O(N) fine. TTL
/// is checked at lookup time, not eagerly; expired entries are
/// dropped lazily.
pub struct SnapshotCache {
    capacity: usize,
    ttl: Duration,
    entries: Vec<CacheEntry>,
}

struct CacheEntry {
    target: String,
    inserted_at: Instant,
    snapshot: std::sync::Arc<WitrSnapshot>,
}

impl SnapshotCache {
    /// Default per-spec: 8 slots, 60s TTL.
    #[must_use]
    pub fn with_default_capacity() -> Self {
        Self::new(8, Duration::from_secs(60))
    }

    /// Construct with arbitrary capacity + TTL. Exposed for tests so
    /// behaviour at the boundary (cap=2, ttl=0) can be pinned without
    /// real-time waits.
    #[must_use]
    pub fn new(capacity: usize, ttl: Duration) -> Self {
        Self {
            capacity,
            ttl,
            entries: Vec::with_capacity(capacity.max(1)),
        }
    }

    /// Insert (or refresh) a snapshot keyed by `target`. Boxed `Arc`
    /// so the publisher (cfx.8) can clone the snapshot without a
    /// full deep copy.
    pub fn insert(&mut self, target: String, snapshot: WitrSnapshot, now: Instant) {
        // Remove any existing entry for this target so insertion is
        // last-write-wins (refreshes the TTL clock).
        self.entries.retain(|e| e.target != target);
        if self.capacity == 0 {
            return;
        }
        while self.entries.len() >= self.capacity {
            self.entries.remove(0);
        }
        self.entries.push(CacheEntry {
            target,
            inserted_at: now,
            snapshot: std::sync::Arc::new(snapshot),
        });
    }

    /// Look up a fresh snapshot. Returns `None` for misses *and* for
    /// entries past their TTL relative to `now`. Expired entries are
    /// removed in place so the next lookup walks less.
    #[must_use]
    pub fn get(&mut self, target: &str, now: Instant) -> Option<std::sync::Arc<WitrSnapshot>> {
        self.entries.retain(|e| now.duration_since(e.inserted_at) < self.ttl);
        self.entries.iter().find(|e| e.target == target).map(|e| e.snapshot.clone())
    }

    /// Number of currently-resident (possibly stale until next
    /// `get`/`insert`) entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Convenience: `len() == 0`.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Coalescing gate around concurrent `r` presses. Holds the set of
/// targets currently being scanned. A second `r` for a target
/// already in flight is a no-op; the in-flight scan will publish
/// its result through the cache when it lands.
#[derive(Debug, Default)]
pub struct ScanGate {
    in_flight: HashSet<String>,
}

impl ScanGate {
    /// True if we acquired the slot (caller should start the scan);
    /// false if the target is already being scanned.
    pub fn try_acquire(&mut self, target: &str) -> bool {
        self.in_flight.insert(target.to_string())
    }

    /// Mark a scan as finished. Idempotent — releasing a target
    /// that was never acquired is fine.
    pub fn release(&mut self, target: &str) {
        self.in_flight.remove(target);
    }

    /// True if any scan is currently in flight against `target`.
    pub fn is_in_flight(&self, target: &str) -> bool {
        self.in_flight.contains(target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::MissingReason;

    fn fixture_snapshot(pid: i64) -> WitrSnapshot {
        // Minimal viable snapshot — only the stable-6 fields the
        // serde defaults require. Cache tests don't read content,
        // they just shuffle Arcs.
        serde_json::from_value(serde_json::json!({
            "Target": {"Type": "pid", "Value": format!("{pid}")},
            "ResolvedTarget": format!("{pid}"),
            "Process": {"PID": pid, "PPID": 1, "Command": "x"},
            "Ancestry": [],
            "Source": {"Type": "unknown", "Name": ""},
            "Warnings": []
        }))
        .expect("fixture snapshot must parse")
    }

    #[test]
    fn tab_index_round_trip() {
        for (i, &t) in Tab::ALL.iter().enumerate() {
            assert_eq!(t.index(), i);
            assert_eq!(Tab::from_index(i), Some(t));
        }
        assert_eq!(Tab::from_index(4), None);
        assert_eq!(Tab::from_index(99), None);
    }

    #[test]
    fn tab_labels_are_unique_and_stable() {
        let labels: std::collections::HashSet<_> = Tab::ALL.iter().map(|t| t.label()).collect();
        assert_eq!(labels.len(), Tab::ALL.len(), "labels must be unique");
        assert_eq!(Tab::Processes.label(), "Processes");
    }

    #[test]
    fn lifecycle_from_detect_maps_ready() {
        let result = DetectResult::Ready {
            path: std::path::PathBuf::from("/usr/local/bin/witr"),
            version: Version::parse("0.3.2").unwrap(),
            commit: None,
            built_at: None,
        };
        assert_eq!(Lifecycle::from_detect(result), Lifecycle::Ready);
    }

    #[test]
    fn lifecycle_from_detect_maps_missing() {
        let result = DetectResult::Missing(MissingReason::NotOnPath);
        assert!(matches!(
            Lifecycle::from_detect(result),
            Lifecycle::Missing(MissingReason::NotOnPath)
        ));
    }

    #[test]
    fn cache_insert_and_get_round_trip() {
        let mut cache = SnapshotCache::with_default_capacity();
        let now = Instant::now();
        cache.insert("nginx".into(), fixture_snapshot(1234), now);
        let hit = cache.get("nginx", now).expect("hit");
        assert_eq!(hit.process.pid, 1234);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn cache_evicts_oldest_past_capacity() {
        let mut cache = SnapshotCache::new(3, Duration::from_secs(60));
        let t0 = Instant::now();
        cache.insert("a".into(), fixture_snapshot(1), t0);
        cache.insert("b".into(), fixture_snapshot(2), t0);
        cache.insert("c".into(), fixture_snapshot(3), t0);
        cache.insert("d".into(), fixture_snapshot(4), t0);
        assert_eq!(cache.len(), 3);
        assert!(cache.get("a", t0).is_none(), "oldest evicted");
        assert!(cache.get("d", t0).is_some(), "newest retained");
    }

    #[test]
    fn cache_refresh_on_same_key_moves_to_newest() {
        let mut cache = SnapshotCache::new(2, Duration::from_secs(60));
        let t0 = Instant::now();
        cache.insert("a".into(), fixture_snapshot(1), t0);
        cache.insert("b".into(), fixture_snapshot(2), t0);
        // Touch "a" again — should evict "b" next, not "a".
        cache.insert("a".into(), fixture_snapshot(99), t0);
        cache.insert("c".into(), fixture_snapshot(3), t0);
        assert!(cache.get("a", t0).is_some(), "refreshed key retained");
        assert!(cache.get("b", t0).is_none(), "untouched older key evicted");
        let a = cache.get("a", t0).unwrap();
        assert_eq!(a.process.pid, 99, "refresh overwrites snapshot");
    }

    #[test]
    fn cache_drops_expired_entries_on_get() {
        let mut cache = SnapshotCache::new(2, Duration::from_secs(1));
        let t0 = Instant::now();
        cache.insert("a".into(), fixture_snapshot(1), t0);
        // 2s later — past 1s TTL.
        let t1 = t0 + Duration::from_secs(2);
        assert!(cache.get("a", t1).is_none(), "expired entry not returned");
        assert_eq!(cache.len(), 0, "expired entry pruned");
    }

    #[test]
    fn cache_zero_capacity_never_stores() {
        let mut cache = SnapshotCache::new(0, Duration::from_secs(60));
        let t0 = Instant::now();
        cache.insert("a".into(), fixture_snapshot(1), t0);
        assert!(cache.is_empty());
        assert!(cache.get("a", t0).is_none());
    }

    #[test]
    fn scan_gate_acquires_once_per_target() {
        let mut gate = ScanGate::default();
        assert!(gate.try_acquire("nginx"));
        assert!(
            !gate.try_acquire("nginx"),
            "second acquire on same target fails"
        );
        assert!(gate.is_in_flight("nginx"));
        // Different target unaffected.
        assert!(gate.try_acquire("postgres"));
    }

    #[test]
    fn scan_gate_release_makes_target_acquirable_again() {
        let mut gate = ScanGate::default();
        gate.try_acquire("nginx");
        gate.release("nginx");
        assert!(!gate.is_in_flight("nginx"));
        assert!(gate.try_acquire("nginx"), "re-acquire after release works");
    }

    #[test]
    fn ui_mode_default_is_browsing() {
        assert_eq!(UiMode::default(), UiMode::Browsing);
    }
}
