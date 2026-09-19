// ABOUTME: The one lock that orders every test in a process against the two
// things a process shares whether the tests like it or not: its environment and
// the tunables snapshot read from it.

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
//! Prefer the scoped home guard in `tests/support/home.rs` for anything to do
//! with the home directory: it takes this lock, points the home somewhere
//! private and puts the environment back. Take the lock directly only for
//! variables that guard does not cover, and never take both, because the lock
//! does not nest.

/// The one lock every test that mutates the environment or the tunables
/// snapshot must hold. Exported as `config::tunables::TEST_ENV_LOCK` too, which
/// is the name the crate's older tests use.
pub static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
