// ABOUTME: One home directory at a time, for every test that needs the home
// directory to be somewhere other than the developer's own: a guard that takes
// a lock for the test's life, points HOME and AINB_HOME at a fresh temporary
// directory, and puts the process's environment back when it drops.

//! A scoped home directory for tests.
//!
//! The environment belongs to the process, not to a test. `cargo test` runs the
//! tests in one binary on many threads, so a test that points `HOME` at its own
//! scratch directory moves it under every other test running at that moment:
//! one test reads the home another test is halfway through replacing, and the
//! failure lands on whichever test looked at the wrong moment. Running with
//! `--test-threads=1` hides it, which is how this reads as a flake rather than
//! as the race it is.
//!
//! So a home directory is taken, not set. [`ScopedHome::new`] holds a lock
//! until the guard drops, which orders every test that needs a home against
//! every other one, and leaves the tests that do not need one running in
//! parallel as before.
//!
//! Declare it once per test binary, because the lock has to be the same lock
//! for every test in that binary:
//!
//! ```ignore
//! #[path = "support/home.rs"]
//! mod home;
//! use home::ScopedHome;
//!
//! #[test]
//! fn a_saved_setting_lands_under_the_scratch_home() {
//!     let home = ScopedHome::new();
//!     assert!(home.path().is_dir());
//! }
//! ```
//!
//! The crate's own unit tests reach it through `crate::test_home`, declared in
//! `lib.rs` from this same file, so the lib's test binary has one lock too.

#![allow(dead_code)]

use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use std::thread::ThreadId;

/// One home directory for every test in this binary, taken once and never
/// given back.
///
/// For the tests that only need home to be somewhere other than the developer's
/// own: they never read what another test wrote, so they do not need to be
/// ordered against each other, only kept off the real home. The first caller
/// takes the lock, points `HOME` and `AINB_HOME` at a temporary directory and
/// keeps both for the rest of the binary, so no test can move home under
/// another one. A [`ScopedHome`] taken later in the same binary still works: it
/// puts this directory back when it drops.
///
/// Prefer [`ScopedHome`] when a test reads back what it wrote, because then a
/// home of its own is the point.
pub fn shared() -> &'static Path {
    static SHARED: std::sync::OnceLock<ScopedHome> = std::sync::OnceLock::new();
    SHARED
        .get_or_init(|| {
            let mut home = ScopedHome::new();
            // The lock is dropped and the guard kept: the environment stays
            // pointed at this directory for the rest of the binary, and the
            // tests that take a `ScopedHome` can still take the lock.
            drop(home.lock.take());
            home
        })
        .path()
}

/// Held by whichever test currently owns the home directory.
static HOME: Mutex<()> = Mutex::new(());

/// The thread holding [`HOME`], so a second guard on the same thread says what
/// happened instead of hanging on the lock forever.
static HOLDER: Mutex<Option<ThreadId>> = Mutex::new(None);

/// A home directory this test owns, and the environment it borrowed to say so.
///
/// `HOME` and `AINB_HOME` both point at a fresh temporary directory for as long
/// as the guard lives. On drop the variables go back to exactly what they were,
/// including back to unset, and the directory is removed.
pub struct ScopedHome {
    /// The lock, held for the guard's life. Named, not `_lock`, only because
    /// dropping it early would hand the home to another test mid-test.
    lock: Option<MutexGuard<'static, ()>>,
    /// Every variable this guard changed, oldest first, with what it held
    /// before. Restored in reverse.
    borrowed: Vec<(OsString, Option<OsString>)>,
    dir: tempfile::TempDir,
}

impl ScopedHome {
    /// Take the home directory: block until no other test holds it, then point
    /// `HOME` and `AINB_HOME` at a fresh temporary directory.
    ///
    /// Panics if the calling thread already holds one. Two guards on one thread
    /// would deadlock, and a deadlock in a test suite reads as a hang with no
    /// output at all.
    pub fn new() -> Self {
        let current = std::thread::current().id();
        {
            let holder = HOLDER.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            assert!(
                *holder != Some(current),
                "this thread already holds the scoped home; one test takes it once"
            );
        }
        // A test that panics while holding the home poisons the lock. The home
        // itself is fine: the guard's drop ran and put the environment back, so
        // the next test takes the lock as it stands rather than failing for a
        // panic that was already reported.
        let lock = HOME.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        *HOLDER.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(current);

        let dir = tempfile::tempdir().expect("a temporary home directory");
        let mut home = Self {
            lock: Some(lock),
            borrowed: Vec::new(),
            dir,
        };
        let path = home.dir.path().to_path_buf();
        home.set("HOME", &path);
        home.set("AINB_HOME", &path);
        home
    }

    /// The temporary directory `HOME` points at.
    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Point another variable somewhere for this test only, restored on drop.
    ///
    /// For the variables that travel with a home directory: a plugin root, a
    /// hangar home, a config path. Setting one outside the guard puts it back
    /// in the race the guard exists to end.
    pub fn set(&mut self, name: impl AsRef<OsStr>, value: impl AsRef<OsStr>) {
        let name = name.as_ref().to_os_string();
        self.borrow(&name);
        std::env::set_var(&name, value.as_ref());
    }

    /// Unset a variable for this test only, restored on drop.
    pub fn unset(&mut self, name: impl AsRef<OsStr>) {
        let name = name.as_ref().to_os_string();
        self.borrow(&name);
        std::env::remove_var(&name);
    }

    /// Record what a variable held before this guard first touched it. Only the
    /// first value is kept: a test that sets one variable twice still restores
    /// what the process had, not what the test wrote in between.
    fn borrow(&mut self, name: &OsString) {
        if self.borrowed.iter().any(|(seen, _)| seen == name) {
            return;
        }
        self.borrowed.push((name.clone(), std::env::var_os(name)));
    }
}

impl Default for ScopedHome {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ScopedHome {
    fn drop(&mut self) {
        // Reverse, so a variable set twice ends at the value the process had.
        for (name, previous) in self.borrowed.drain(..).rev() {
            match previous {
                Some(value) => std::env::set_var(&name, value),
                None => std::env::remove_var(&name),
            }
        }
        *HOLDER.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        // The lock goes last: the environment is already back, so the next test
        // to take the home never sees this one's.
        drop(self.lock.take());
    }
}
