// ABOUTME: The one lock that orders every test in a process against the two
// things a process shares whether the tests like it or not: its environment and
// the tunables snapshot read from it. It refuses to be taken twice on one
// thread, because that is a hang with no output rather than a failure.

//! The process's environment lock.
//!
//! The environment is process-global, so a test that writes one variable writes
//! it for every thread running at that moment. Ordering those writes needs one
//! lock, and one only: two mutexes around the same `setenv` still race, and a
//! snapshot installed under one of them is read by tests holding the other.
//!
//! It lives here, compiled into every build, rather than behind the
//! `test-support` feature, for one reason: the crate's integration tests need
//! the same lock as its unit tests, and a feature that half the test targets do
//! not ask for would give them a second one. Holding a mutex costs nothing in a
//! build that never locks it.
//!
//! The lock does not nest, and [`EnvLock::lock`] says so out loud rather than
//! blocking: a second take on the same thread panics naming both takes' job.
//! Without that, a helper that takes the lock and calls another helper that
//! takes the lock is a test binary that hangs until the runner is killed, with
//! no failing test to point at.
//!
//! Prefer the scoped home guard in `tests/support/home.rs` for anything to do
//! with the home directory: it takes this lock, points the home somewhere
//! private and puts the environment back. Take the lock directly only for
//! variables that guard does not cover, and never take both.

use std::sync::{LockResult, Mutex, MutexGuard};
use std::thread::ThreadId;

/// The one lock every test that mutates the environment or the tunables
/// snapshot must hold. Exported as `config::tunables::TEST_ENV_LOCK` and
/// `headroom::HEADROOM_ENV_LOCK` too, which are the names the crate's older
/// tests use.
pub static ENV_LOCK: EnvLock = EnvLock::new();

/// A mutex that knows which thread holds it.
pub struct EnvLock {
    inner: Mutex<()>,
    holder: Mutex<Option<ThreadId>>,
}

impl EnvLock {
    /// The lock, unheld.
    const fn new() -> Self {
        Self {
            inner: Mutex::new(()),
            holder: Mutex::new(None),
        }
    }

    /// Take the lock, blocking until the thread that holds it lets go.
    ///
    /// The result is always `Ok`: a test that panics while holding the lock
    /// poisons it, and the environment it was writing is already back, so the
    /// next test takes the lock as it stands rather than failing for a panic
    /// that was reported once. `LockResult` stays in the signature because every
    /// caller in this crate unwraps a poisoned lock by hand.
    ///
    /// # Panics
    ///
    /// If the calling thread already holds it. One take per thread: the second
    /// would wait for the first to be released, on the thread that is waiting.
    pub fn lock(&'static self) -> LockResult<EnvGuard> {
        let current = std::thread::current().id();
        assert!(
            *self.holder_slot() != Some(current),
            "this thread already holds the environment lock. Something that \
             takes it has called something else that takes it: the lock does \
             not nest, so take it once, at the outermost step that writes the \
             environment"
        );
        let inner = self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        *self.holder_slot() = Some(current);
        Ok(EnvGuard {
            held: Some(inner),
            lock: self,
        })
    }

    fn holder_slot(&self) -> MutexGuard<'_, Option<ThreadId>> {
        self.holder.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// The environment lock, held. Dropping it releases the lock.
pub struct EnvGuard {
    /// `None` only while dropping, so the holder is forgotten before the lock is
    /// released and the next thread never reads this thread's id.
    held: Option<MutexGuard<'static, ()>>,
    lock: &'static EnvLock,
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        *self.lock.holder_slot() = None;
        drop(self.held.take());
    }
}
