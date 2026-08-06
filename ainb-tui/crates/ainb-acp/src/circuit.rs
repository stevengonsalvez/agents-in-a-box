//! Crash breaker for one adapter PROCESS.
//!
//! Scope follows the multiplex decision (graft 6): one adapter process per
//! provider hosts many sessions, so the breaker is per PROCESS, not per scope.
//! A crash-looping adapter must not be respawned on every queued prompt; while
//! the breaker is open the pool fails deliveries fast with an enumerated
//! `breaker_open` detail instead of burning a spawn per message.
//!
//! Backoff is exponential with jitter. The jitter matters under the multiplex
//! decision: a process crash converges EVERY session it hosted, so without it
//! all of them would retry on the same tick and re-crash the adapter together.

use std::time::{Duration, Instant};

/// Breaker tuning.
#[derive(Debug, Clone, Copy)]
pub struct CircuitConfig {
    /// Consecutive crashes before the breaker opens.
    pub failure_threshold: u32,
    /// Backoff for the first crash past the threshold.
    pub base_backoff: Duration,
    /// Ceiling for the exponential growth.
    pub max_backoff: Duration,
    /// Jitter as a fraction of the computed backoff, applied downward
    /// (`[backoff * (1 - jitter), backoff]`).
    pub jitter: f64,
}

impl Default for CircuitConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 3,
            base_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(5 * 60),
            jitter: 0.25,
        }
    }
}

/// Per-process breaker state.
#[derive(Debug)]
pub struct SlotCircuit {
    config: CircuitConfig,
    consecutive_failures: u32,
    open_until: Option<Instant>,
}

impl SlotCircuit {
    /// A closed breaker with the given tuning.
    pub const fn new(config: CircuitConfig) -> Self {
        Self {
            config,
            consecutive_failures: 0,
            open_until: None,
        }
    }

    /// True while spawns must be refused.
    pub fn is_open(&self, now: Instant) -> bool {
        self.open_until.is_some_and(|until| now < until)
    }

    /// How long until a spawn may be attempted again, if the breaker is open.
    pub fn retry_after(&self, now: Instant) -> Option<Duration> {
        self.open_until.filter(|until| now < *until).map(|until| until - now)
    }

    /// Consecutive crashes observed since the last success.
    pub const fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }

    /// A process came up and served a turn: close the breaker.
    pub const fn record_success(&mut self) {
        self.consecutive_failures = 0;
        self.open_until = None;
    }

    /// A process died. Returns the backoff now in force, if the breaker opened.
    pub fn record_crash(&mut self, now: Instant) -> Option<Duration> {
        self.record_crash_with(now, rand::random::<f64>())
    }

    /// [`Self::record_crash`] with the jitter draw injected, so the ladder is
    /// asserted deterministically in tests.
    ///
    /// `jitter_unit` is in `[0, 1)`.
    pub fn record_crash_with(&mut self, now: Instant, jitter_unit: f64) -> Option<Duration> {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        if self.consecutive_failures < self.config.failure_threshold {
            return None;
        }
        let steps = self.consecutive_failures - self.config.failure_threshold;
        let backoff = self.backoff_for(steps, jitter_unit);
        self.open_until = Some(now + backoff);
        Some(backoff)
    }

    fn backoff_for(&self, steps: u32, jitter_unit: f64) -> Duration {
        let scaled = self
            .config
            .base_backoff
            .saturating_mul(1_u32.checked_shl(steps.min(31)).unwrap_or(u32::MAX))
            .min(self.config.max_backoff);
        let jitter = self.config.jitter.clamp(0.0, 1.0) * jitter_unit.clamp(0.0, 1.0);
        scaled.mul_f64(1.0 - jitter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> CircuitConfig {
        CircuitConfig {
            failure_threshold: 3,
            base_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(8),
            jitter: 0.0,
        }
    }

    #[test]
    fn the_breaker_stays_closed_below_the_threshold() {
        let now = Instant::now();
        let mut circuit = SlotCircuit::new(config());
        assert_eq!(circuit.record_crash_with(now, 0.0), None);
        assert_eq!(circuit.record_crash_with(now, 0.0), None);
        assert!(!circuit.is_open(now));
    }

    #[test]
    fn backoff_doubles_per_crash_past_the_threshold_and_caps() {
        let now = Instant::now();
        let mut circuit = SlotCircuit::new(config());
        circuit.record_crash_with(now, 0.0);
        circuit.record_crash_with(now, 0.0);
        let ladder: Vec<Duration> = (0..6)
            .map(|_| circuit.record_crash_with(now, 0.0).expect("open past the threshold"))
            .collect();
        assert_eq!(
            ladder,
            vec![
                Duration::from_millis(500),
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4),
                Duration::from_secs(8),
                Duration::from_secs(8),
            ]
        );
    }

    #[test]
    fn jitter_only_ever_shortens_the_backoff() {
        let now = Instant::now();
        let jittered = CircuitConfig {
            jitter: 0.25,
            ..config()
        };
        for draw in [0.0, 0.5, 0.999] {
            let mut circuit = SlotCircuit::new(jittered);
            circuit.record_crash_with(now, draw);
            circuit.record_crash_with(now, draw);
            let backoff = circuit.record_crash_with(now, draw).expect("open");
            assert!(backoff <= Duration::from_millis(500));
            assert!(backoff >= Duration::from_millis(375));
        }
    }

    #[test]
    fn an_open_breaker_reports_its_remaining_wait_then_closes_on_success() {
        let now = Instant::now();
        let mut circuit = SlotCircuit::new(config());
        for _ in 0..3 {
            circuit.record_crash_with(now, 0.0);
        }
        assert!(circuit.is_open(now));
        assert_eq!(circuit.retry_after(now), Some(Duration::from_millis(500)));
        assert!(!circuit.is_open(now + Duration::from_millis(501)));
        circuit.record_success();
        assert!(!circuit.is_open(now));
        assert_eq!(circuit.consecutive_failures(), 0);
        assert_eq!(circuit.retry_after(now), None);
    }
}
