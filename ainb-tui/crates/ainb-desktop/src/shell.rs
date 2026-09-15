//! The host and its executor as one lockable unit, for the window's commands
//! and its tick to share.
//!
//! Both run an intent's effects with the executor while holding the host, so
//! they live behind one `Mutex`: there is no second lock to take in a different
//! order, and a dispatch arriving during a tick waits for it rather than
//! deadlocking against it.

use std::sync::{Mutex, MutexGuard, PoisonError};

use ainb_app::Intent;

use crate::executor::DesktopExecutor;
use crate::host::{DesktopHost, Executor, FrameSink};

struct Core<S: FrameSink> {
    host: DesktopHost<S>,
    executor: DesktopExecutor,
}

/// The shell's host and executor, locked together.
pub struct Shell<S: FrameSink> {
    core: Mutex<Core<S>>,
}

impl<S: FrameSink> Shell<S> {
    #[must_use]
    pub fn new(host: DesktopHost<S>, executor: DesktopExecutor) -> Self {
        Self {
            core: Mutex::new(Core { host, executor }),
        }
    }

    fn core(&self) -> MutexGuard<'_, Core<S>> {
        self.core.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Apply an intent from the renderer and run what it queued.
    pub fn dispatch(&self, intent: Intent) {
        let mut core = self.core();
        let Core { host, executor } = &mut *core;
        host.run(intent, executor);
    }

    /// Frame what moved since the last tick, and run the effects and deferred
    /// reports that work produced.
    pub fn tick(&self) {
        let mut core = self.core();
        let Core { host, executor } = &mut *core;
        let mut reports = Vec::new();
        for effect in host.tick() {
            reports.extend(executor.execute(effect));
        }
        reports.extend(executor.take_deferred());
        for report in reports {
            host.run(report, executor);
        }
    }

    /// Frame every subscribed section again for a renderer that just attached.
    pub fn reframe(&self) {
        self.core().host.reframe();
    }
}
