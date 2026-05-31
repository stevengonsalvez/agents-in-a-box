//! In-memory daemon health stats: the rolling task-throughput ring buffer + the
//! bounded claim-slot cache figure (P8.5).
//!
//! The daemon-health pane (`hangar/daemon_health`, the `D` screen) needs two
//! pieces of state the persistent store does not hold: a **per-second rolling
//! window** of how many tasks finished (split success / failure), and the
//! current occupancy of the bounded claim-slot cache. Both are view-layer
//! figures — this is deliberately a tiny snapshot collector, **not** a metrics
//! system; nothing here is persisted.
//!
//! ## Throughput ring
//!
//! [`HealthStats`] holds a fixed [`THROUGHPUT_WINDOW`]-slot ring of
//! per-second `(completed, failed)` tallies. The FSM finalize path
//! ([`crate::run_loop`]) calls [`HealthStats::record_completed`] /
//! [`HealthStats::record_failed`] with the current epoch-second as each task
//! reaches a terminal state. The ring **rolls on the second boundary**: when a
//! record lands on a newer second than the ring's head, the head advances one
//! slot per elapsed second (zero-filling any idle seconds in between) so the
//! window always covers exactly the last minute relative to the latest
//! activity.
//!
//! [`HealthStats::snapshot`] renders the ring oldest-first against a caller-
//! supplied "now" second, so a snapshot taken in a quiet period still shows the
//! window sliding forward (the trailing buckets read zero).
//!
//! ## Concurrency
//!
//! The daemon runs the FSM finalize path and the RPC dispatcher on different
//! tasks, so the collector is shared as `Arc<HealthStats>` and guards its ring
//! behind a `Mutex`. The critical sections are O(window) and contention-free in
//! practice (finalize is not a hot loop).

use std::sync::Mutex;

pub use ainb_hangar_proto::settings::THROUGHPUT_WINDOW;
use ainb_hangar_proto::settings::{ClaimCache, ThroughputSample};

/// [`THROUGHPUT_WINDOW`] as an `i64`, for the second-arithmetic in the ring
/// (the window is a small fixed constant, so the widening never wraps).
#[allow(clippy::cast_possible_wrap)] // 60 -> i64 widening is exact.
const WINDOW_I64: i64 = THROUGHPUT_WINDOW as i64;

/// The fixed claim-slot cache capacity the health pane reports.
///
/// The claim service has no bounded cache to read an exact figure from, so the
/// pane reports this fixed ceiling (a sane view-layer constant, not a tuned
/// metric) — see the P8.5 plan's "report a sane fixed capacity" fallback.
pub const DEFAULT_CLAIM_CAPACITY: u32 = 64;

/// Sentinel `ts` for an as-yet-unused ring slot. `i64::MIN` can never collide
/// with a real epoch-second (or a small test timestamp like `0`), so a slot
/// carrying it is treated as empty during lookup.
const UNUSED_TS: i64 = i64::MIN;

/// One ring slot: the second it covers plus its success / failure tallies.
#[derive(Debug, Clone, Copy)]
struct Bucket {
    /// Epoch-second this bucket tallies ([`UNUSED_TS`] for an unused slot).
    ts:        i64,
    /// Successful (`done`) terminal tasks in this second.
    completed: u32,
    /// Unsuccessful (`failed` / `cancelled`) terminal tasks in this second.
    failed:    u32,
}

impl Bucket {
    /// An empty slot tallying `ts` (the snapshot zero-fill / seed shape).
    const fn empty(ts: i64) -> Self {
        Self {
            ts,
            completed: 0,
            failed: 0,
        }
    }
}

/// Mutable ring state, guarded by the [`HealthStats`] mutex.
#[derive(Debug)]
struct Ring {
    /// The per-second buckets; `head` indexes the most-recent second.
    buckets: [Bucket; THROUGHPUT_WINDOW],
    /// Index of the most-recent (current) second's bucket.
    head:    usize,
    /// The epoch-second `buckets[head]` currently tallies; `None` until the
    /// first record (so the very first record seeds the head without rolling).
    head_ts: Option<i64>,
}

impl Ring {
    const fn new() -> Self {
        Self {
            buckets: [Bucket::empty(UNUSED_TS); THROUGHPUT_WINDOW],
            head:    0,
            head_ts: None,
        }
    }

    /// Advance the ring head to `now_sec`, zero-filling any idle seconds, so
    /// the current bucket tallies `now_sec`. A record at the same second is
    /// a no-op; a record in the past (clock skew) clamps to the current
    /// head (never rolls backwards, never loses the existing tally).
    fn advance_to(&mut self, now_sec: i64) {
        let Some(head_ts) = self.head_ts else {
            // First ever record: seed the head bucket at `now_sec`.
            self.head_ts = Some(now_sec);
            self.buckets[self.head] = Bucket::empty(now_sec);
            return;
        };
        if now_sec <= head_ts {
            // Same second (or a backwards clock blip): keep the current bucket.
            return;
        }
        // Roll forward one slot per elapsed second. Cap the roll at the window
        // width — more than a full minute idle means the whole window is stale,
        // so a single full wipe suffices (and bounds the loop).
        let steps = (now_sec - head_ts).min(WINDOW_I64);
        for step in 1..=steps {
            self.head = (self.head + 1) % THROUGHPUT_WINDOW;
            self.buckets[self.head] = Bucket::empty(head_ts + step);
        }
        self.head_ts = Some(now_sec);
    }

    /// Render the ring oldest-first against `now_sec`, advancing the window
    /// first so a snapshot taken after a quiet stretch slides forward
    /// (trailing buckets read zero). Always returns exactly
    /// [`THROUGHPUT_WINDOW`] samples whose timestamps are the contiguous
    /// `now_sec - 59 ..= now_sec`.
    fn snapshot(&mut self, now_sec: i64) -> Vec<ThroughputSample> {
        self.advance_to(now_sec);
        let head_ts = self.head_ts.unwrap_or(now_sec);
        // Build a ts -> bucket lookup over the live ring, then emit the
        // contiguous trailing-minute window ending at `head_ts`.
        let mut out = Vec::with_capacity(THROUGHPUT_WINDOW);
        let oldest_ts = head_ts - (WINDOW_I64 - 1);
        for offset in 0..THROUGHPUT_WINDOW {
            let ts = oldest_ts + i64::try_from(offset).unwrap_or(0);
            let bucket = self
                .buckets
                .iter()
                .find(|b| b.ts == ts)
                .copied()
                .unwrap_or_else(|| Bucket::empty(ts));
            out.push(ThroughputSample {
                ts,
                completed: bucket.completed,
                failed: bucket.failed,
            });
        }
        out
    }
}

/// The daemon's in-memory health stats collector (P8.5).
///
/// Shared as `Arc<HealthStats>` across the FSM finalize path (which records
/// terminal outcomes) and the RPC dispatcher (which snapshots the window for
/// the `hangar/daemon_health` handler).
#[derive(Debug)]
pub struct HealthStats {
    ring:           Mutex<Ring>,
    claim_capacity: u32,
}

impl Default for HealthStats {
    fn default() -> Self {
        Self::new(DEFAULT_CLAIM_CAPACITY)
    }
}

impl HealthStats {
    /// A fresh collector with the given claim-slot `capacity`.
    #[must_use]
    pub const fn new(claim_capacity: u32) -> Self {
        Self {
            ring: Mutex::new(Ring::new()),
            claim_capacity,
        }
    }

    /// Record one successfully-completed task at epoch-second `now_sec`.
    pub fn record_completed(&self, now_sec: i64) {
        let mut ring = self.ring.lock().expect("health ring poisoned");
        ring.advance_to(now_sec);
        let head = ring.head;
        ring.buckets[head].completed = ring.buckets[head].completed.saturating_add(1);
    }

    /// Record one failed / cancelled task at epoch-second `now_sec`.
    pub fn record_failed(&self, now_sec: i64) {
        let mut ring = self.ring.lock().expect("health ring poisoned");
        ring.advance_to(now_sec);
        let head = ring.head;
        ring.buckets[head].failed = ring.buckets[head].failed.saturating_add(1);
    }

    /// Snapshot the rolling 60-second throughput window oldest-first against
    /// `now_sec`.
    #[must_use]
    pub fn throughput_window(&self, now_sec: i64) -> Vec<ThroughputSample> {
        self.ring.lock().expect("health ring poisoned").snapshot(now_sec)
    }

    /// The claim-slot cache occupancy. `used` is derived from the live
    /// concurrent task count (the daemon has no separately-tracked cache);
    /// `capacity` is the fixed ceiling this collector was built with.
    #[must_use]
    pub const fn claim_cache(&self, used: u32) -> ClaimCache {
        ClaimCache {
            used,
            capacity: self.claim_capacity,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A snapshot of a fresh collector is the full window, all-zero,
    /// contiguous.
    #[test]
    fn empty_window_is_full_and_zero() {
        let stats = HealthStats::default();
        let window = stats.throughput_window(1_700_000_100);
        assert_eq!(window.len(), THROUGHPUT_WINDOW);
        assert!(window.iter().all(|s| s.completed == 0 && s.failed == 0));
        // Contiguous, ending at `now_sec`.
        assert_eq!(window[THROUGHPUT_WINDOW - 1].ts, 1_700_000_100);
        assert_eq!(window[0].ts, 1_700_000_100 - 59);
    }

    /// Records within the same second accumulate into one bucket.
    #[test]
    fn records_in_same_second_accumulate() {
        let stats = HealthStats::default();
        stats.record_completed(1_000);
        stats.record_completed(1_000);
        stats.record_failed(1_000);
        let window = stats.throughput_window(1_000);
        let last = window[THROUGHPUT_WINDOW - 1];
        assert_eq!(last.ts, 1_000);
        assert_eq!(last.completed, 2);
        assert_eq!(last.failed, 1);
    }

    /// The ring rolls correctly across the minute boundary: a record 60 seconds
    /// after an earlier one fully evicts the earlier second from the window.
    #[test]
    fn ring_rolls_across_minute_boundary() {
        let stats = HealthStats::default();
        // t=0: one completion.
        stats.record_completed(0);
        // A snapshot at t=0 sees it at the head.
        let w0 = stats.throughput_window(0);
        assert_eq!(w0[THROUGHPUT_WINDOW - 1].completed, 1);
        // t=60: a full minute later — the t=0 second has rolled off the window.
        stats.record_completed(60);
        let w60 = stats.throughput_window(60);
        assert_eq!(w60.len(), THROUGHPUT_WINDOW);
        // The new record is at the head (t=60).
        assert_eq!(w60[THROUGHPUT_WINDOW - 1].ts, 60);
        assert_eq!(w60[THROUGHPUT_WINDOW - 1].completed, 1);
        // The t=0 bucket is gone — no sample in the window carries its tally.
        assert!(
            !w60.iter().any(|s| s.ts == 0 && s.completed == 1),
            "t=0 second must have rolled off the 60s window"
        );
        // Window timestamps are contiguous 1..=60 (oldest 1, newest 60).
        assert_eq!(w60[0].ts, 1);
    }

    /// A record one second after another keeps both within the window, in their
    /// own buckets (no off-by-one merge / skip).
    #[test]
    fn adjacent_seconds_land_in_distinct_buckets() {
        let stats = HealthStats::default();
        stats.record_completed(100);
        stats.record_failed(101);
        let window = stats.throughput_window(101);
        // t=100 completed=1, t=101 failed=1, distinct buckets.
        let at100 = window.iter().find(|s| s.ts == 100).unwrap();
        let at101 = window.iter().find(|s| s.ts == 101).unwrap();
        assert_eq!(at100.completed, 1);
        assert_eq!(at100.failed, 0);
        assert_eq!(at101.completed, 0);
        assert_eq!(at101.failed, 1);
    }

    /// An idle gap zero-fills the skipped seconds rather than smearing the
    /// tally forward.
    #[test]
    fn idle_gap_zero_fills() {
        let stats = HealthStats::default();
        stats.record_completed(10);
        stats.record_completed(15); // 4-second gap (11,12,13,14 idle).
        let window = stats.throughput_window(15);
        for ts in 11..15 {
            let s = window.iter().find(|s| s.ts == ts).unwrap();
            assert_eq!(s.completed, 0, "idle second {ts} must be zero");
            assert_eq!(s.failed, 0);
        }
        assert_eq!(window.iter().find(|s| s.ts == 10).unwrap().completed, 1);
        assert_eq!(window.iter().find(|s| s.ts == 15).unwrap().completed, 1);
    }

    /// The claim-cache figure carries the configured capacity and the supplied
    /// used count.
    #[test]
    fn claim_cache_reports_capacity_and_used() {
        let stats = HealthStats::new(64);
        let cache = stats.claim_cache(12);
        assert_eq!(cache.used, 12);
        assert_eq!(cache.capacity, 64);
    }
}
