//! The host and its executor as one lockable unit, for the window's commands
//! and its tick to share.
//!
//! Both run an intent's effects with the executor while holding the host, so
//! they live behind one `Mutex`: there is no second lock to take in a different
//! order, and a dispatch arriving during a tick waits for it rather than
//! deadlocking against it.

use std::sync::{Mutex, MutexGuard, PoisonError};

use ainb_app::Intent;
use ainb_app::wire::frame::{HostId, Subscription};

use crate::executor::DesktopExecutor;
use crate::host::{DesktopHost, Executor, FrameSink};
use crate::intent::Refusal;

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

    /// Apply an intent the webview sent, unless
    /// [`DesktopHost::refused_from_renderer`] refuses what it would run; the
    /// refusal is returned for the webview to show. The check runs under the
    /// same lock that would apply the intent, so the state cannot move between
    /// the check and the dispatch.
    #[must_use = "a refusal the webview never hears about looks like a dead key"]
    pub fn dispatch_renderer(&self, intent: Intent) -> Option<Refusal> {
        let mut core = self.core();
        let Core { host, executor } = &mut *core;
        if let Some(refusal) = host.refused_from_renderer(&intent) {
            tracing::warn!(
                "`{}` refused from the webview: {}",
                refusal.command,
                refusal.reason
            );
            return Some(refusal);
        }
        host.run(intent, executor);
        None
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

    /// Every command the palette may offer; see [`DesktopHost::palette`].
    #[must_use]
    pub fn palette(&self) -> Vec<crate::host::PaletteEntry> {
        self.core().host.palette()
    }

    /// Put the reducer on the session list; see [`DesktopHost::open_sessions`].
    pub fn open_sessions(&self) {
        let mut core = self.core();
        let Core { host, executor } = &mut *core;
        host.open_sessions(executor);
    }

    /// Frame every section in `subscription`, and only those, for a renderer
    /// that just attached, and answer the host those frames name. One lock, so
    /// the answer is the host of the frames this call sent.
    pub fn subscribe(&self, subscription: Subscription) -> HostId {
        let mut core = self.core();
        core.host.subscribe(subscription);
        core.host.host_id().clone()
    }

    /// The host every frame names.
    pub fn host_id(&self) -> HostId {
        self.core().host.host_id().clone()
    }

    /// The sidecar lost its daemon; see [`DesktopHost::daemon_lost`].
    pub fn daemon_lost(&self, reason: &str) {
        self.core().host.daemon_lost(reason);
    }

    /// The sidecar has its daemon; see [`DesktopHost::daemon_connected`].
    pub fn daemon_connected(&self) {
        self.core().host.daemon_connected();
    }

    /// Re-pin the host every frame names; see [`DesktopHost::set_host`].
    pub fn set_host(&self, host_id: HostId) -> bool {
        self.core().host.set_host(host_id)
    }
}
