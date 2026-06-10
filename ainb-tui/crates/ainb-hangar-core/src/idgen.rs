//! Id generation.
//!
//! Hangar primary keys are ULIDs (lexicographically sortable, time-prefixed).
//! Production code mints them via [`SystemIdGen`]; tests inject [`FixedIdGen`]
//! with a pre-seeded queue so golden assertions over generated ids are
//! repeatable. Like [`crate::clock`], this trait is the single seam the
//! persistence layer takes id generation through.

/// A source of new entity ids.
pub trait IdGen: Send + Sync {
    /// Mint a fresh, unique id (a ULID string in production).
    fn new_ulid(&self) -> String;
}

/// The production id generator: mints a random, time-prefixed ULID per call.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemIdGen;

impl IdGen for SystemIdGen {
    fn new_ulid(&self) -> String {
        ulid::Ulid::new().to_string()
    }
}

/// A deterministic id generator that pops from a pre-seeded queue.
///
/// Each call to [`new_ulid`](IdGen::new_ulid) returns the next id in
/// declaration order; the generator **panics if the queue is exhausted** so a
/// test that mints more ids than it seeded fails loudly rather than silently
/// reusing a value. Available under `#[cfg(test)]` within this crate and via
/// the `test-support` feature to downstream crates' tests.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug)]
pub struct FixedIdGen {
    queue: std::sync::Mutex<std::collections::VecDeque<String>>,
}

#[cfg(any(test, feature = "test-support"))]
impl FixedIdGen {
    /// Seed the generator with the ids it will hand out, in order.
    #[must_use]
    pub fn new(ids: Vec<String>) -> Self {
        Self {
            queue: std::sync::Mutex::new(ids.into()),
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
impl IdGen for FixedIdGen {
    /// # Panics
    ///
    /// Panics when the pre-seeded queue is exhausted.
    fn new_ulid(&self) -> String {
        self.queue
            .lock()
            .expect("FixedIdGen mutex poisoned")
            .pop_front()
            .expect("FixedIdGen queue exhausted: seed more ids")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_idgen_produces_unique_ulids() {
        let gen = SystemIdGen;
        let a = gen.new_ulid();
        let b = gen.new_ulid();
        assert_ne!(a, b);
        assert_eq!(a.len(), 26, "ULID canonical length");
    }

    #[test]
    fn fixed_idgen_pops_in_order() {
        let gen = FixedIdGen::new(vec!["a".into(), "b".into()]);
        assert_eq!(gen.new_ulid(), "a");
        assert_eq!(gen.new_ulid(), "b");
    }

    #[test]
    #[should_panic(expected = "queue exhausted")]
    fn fixed_idgen_panics_when_exhausted() {
        let gen = FixedIdGen::new(vec![]);
        let _ = gen.new_ulid();
    }
}
