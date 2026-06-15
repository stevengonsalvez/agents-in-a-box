//! Time injection.
//!
//! All Hangar timestamps are epoch milliseconds (`INTEGER` columns). Production
//! code reads the wall clock via [`SystemClock`]; tests inject [`FixedClock`]
//! so `created_at` / `started_at` / `finished_at` assertions are deterministic.
//! The trait is the single seam the persistence layer takes a clock through.

/// A source of "now" in epoch milliseconds.
pub trait HangarClock: Send + Sync {
    /// The current time as epoch milliseconds (UTC).
    fn now_ms(&self) -> i64;
}

/// The production clock: reads the system wall clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl HangarClock for SystemClock {
    fn now_ms(&self) -> i64 {
        chrono::Utc::now().timestamp_millis()
    }
}

/// A deterministic clock that always returns a frozen epoch-ms value.
///
/// Available under `#[cfg(test)]` within this crate and via the `test-support`
/// feature to downstream crates' tests.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Clone, Copy)]
pub struct FixedClock(pub i64);

#[cfg(any(test, feature = "test-support"))]
impl HangarClock for FixedClock {
    fn now_ms(&self) -> i64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_clock_returns_frozen_value() {
        let clock = FixedClock(1_700_000_000_000);
        assert_eq!(clock.now_ms(), 1_700_000_000_000);
        // Stable across calls.
        assert_eq!(clock.now_ms(), clock.now_ms());
    }

    #[test]
    fn system_clock_is_nondecreasing_and_nonzero() {
        let clock = SystemClock;
        let first = clock.now_ms();
        assert!(first > 0);
        let second = clock.now_ms();
        // Wall-clock reads taken in sequence must not move backwards.
        assert!(second >= first);
    }
}
