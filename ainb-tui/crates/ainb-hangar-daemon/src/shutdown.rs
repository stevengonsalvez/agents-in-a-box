//! The daemon's one shutdown seam: what asked it to stop, and how much to tear
//! down on the way out.
//!
//! The daemon used to handle `ctrl_c()` (SIGINT) alone, while the supported stop
//! command (`ainb hangar daemon stop`) sends SIGTERM. With no handler for it the
//! process died on the OS default disposition, so NONE of the teardown below ran:
//! headless provider children were reparented to init and kept mutating their
//! workspaces unsupervised, and the ownership lock was never released. Every
//! sibling daemon in this workspace (`mcp_pool`, `notifyd`) already handled
//! SIGTERM; the hangar daemon was the outlier.
//!
//! # Why the two causes differ
//!
//! [`Cause::Interrupt`] is a human in the foreground saying "tear it all down",
//! and takes in-flight interactive tmux sessions with it.
//!
//! [`Cause::Terminate`] is `daemon stop` / `daemon restart` — which runs after
//! every upgrade. Killing the operator's attached agent panes on an upgrade would
//! destroy work the daemon merely supervises, so interactive sessions are left
//! running; the boot tmux reconciler re-adopts them with exact pane identity.
//! Headless children are still killed in both cases (they are unattended, and
//! leaving them reparented to init is the leak this seam exists to stop).

/// What asked the daemon to shut down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    /// SIGINT — a human interrupting a foreground daemon.
    Interrupt,
    /// SIGTERM — `hangar daemon stop`, `restart`, launchd/systemd, or a
    /// system shutdown.
    Terminate,
    /// This daemon no longer owns its hangar home; the named pid does.
    ///
    /// Nobody signalled us — we noticed. Standing down is the only correct
    /// response: two daemons on one home race the same database and unlink each
    /// other's socket.
    LockLost(i32),
}

impl Cause {
    /// Should in-flight INTERACTIVE tmux sessions be reaped on the way out?
    ///
    /// See the module doc: only the foreground interrupt takes attached panes
    /// with it.
    #[must_use]
    pub const fn reaps_interactive_sessions(self) -> bool {
        matches!(self, Self::Interrupt)
    }

    /// The `signal` field for the shutdown log line.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Interrupt => "SIGINT",
            Self::Terminate => "SIGTERM",
            Self::LockLost(_) => "lock-lost",
        }
    }
}

/// A long-lived watcher for every signal that ends the daemon.
///
/// Built ONCE outside the claim loop and polled from its `select!`, rather than
/// re-registering a handler on every tick. Both signal streams are OWNED here,
/// so a signal delivered while another `select!` branch was winning is queued
/// rather than lost.
#[derive(Debug)]
pub struct Watch {
    /// `None` only when the handler could not be installed; the daemon then
    /// degrades to whichever signal it did manage to register rather than
    /// refusing to run.
    interrupt: Option<tokio::signal::unix::Signal>,
    /// `None` only when the SIGTERM handler could not be installed.
    terminate: Option<tokio::signal::unix::Signal>,
    /// Resolves with the new owner's pid if this daemon ever stops owning its
    /// home. `None` for a daemon that holds no lock (a test harness driving the
    /// run loop directly).
    lock_lost: Option<tokio::sync::oneshot::Receiver<i32>>,
}

/// Install one signal handler, logging and degrading rather than failing.
fn install_handler(
    kind: tokio::signal::unix::SignalKind,
    name: &str,
) -> Option<tokio::signal::unix::Signal> {
    match tokio::signal::unix::signal(kind) {
        Ok(signal) => Some(signal),
        Err(e) => {
            tracing::error!(
                error = %e,
                signal = name,
                "could not install a shutdown signal handler; this process will die on \
                 that signal's default disposition instead of shutting down cleanly"
            );
            None
        }
    }
}

/// Never resolves — the arm for a handler that could not be installed.
async fn never() {
    std::future::pending::<()>().await;
}

/// Wait for one more delivery of `signal`, or forever if it is absent.
async fn fires(signal: Option<&mut tokio::signal::unix::Signal>) {
    match signal {
        Some(signal) => {
            signal.recv().await;
        }
        None => never().await,
    }
}

impl Watch {
    /// Install the signal handlers.
    #[must_use]
    pub fn new() -> Self {
        use tokio::signal::unix::SignalKind;
        Self {
            interrupt: install_handler(SignalKind::interrupt(), "SIGINT"),
            terminate: install_handler(SignalKind::terminate(), "SIGTERM"),
            lock_lost: None,
        }
    }

    /// Also stand down when `lock_lost` resolves — see
    /// [`crate::single_instance::watch_ownership`].
    #[must_use]
    pub fn on_lock_lost(mut self, lock_lost: tokio::sync::oneshot::Receiver<i32>) -> Self {
        self.lock_lost = Some(lock_lost);
        self
    }

    /// Resolve on the first signal asking the daemon to stop.
    ///
    /// Cancel-safe, so it can live in a `select!` arm that loses to other
    /// branches: `Signal::recv` is cancel-safe by contract, and the streams
    /// themselves are owned by this struct, so nothing is deregistered when a
    /// losing branch's future is dropped.
    pub async fn recv(&mut self) -> Cause {
        loop {
            // Destructured so the three sources are disjoint borrows and can be
            // polled in one `select!`.
            let Self {
                interrupt,
                terminate,
                lock_lost,
            } = self;
            let cause = match lock_lost {
                Some(lock_lost) => {
                    tokio::select! {
                        () = fires(interrupt.as_mut()) => Some(Cause::Interrupt),
                        () = fires(terminate.as_mut()) => Some(Cause::Terminate),
                        // `Err` means the watchdog ended without reporting a
                        // loss (its task panicked, or a refactor let it return),
                        // which is NOT a lost lock — it maps to `None` and the
                        // loop re-enters the select.
                        //
                        // It must not be answered inside this arm: `select!` has
                        // already DROPPED the other branches' futures by the
                        // time an arm body runs, so awaiting `pending()` here —
                        // the previous shape — stranded the signal streams and
                        // the daemon stopped answering SIGINT and SIGTERM.
                        owner = lock_lost => owner.ok().map(Cause::LockLost),
                    }
                }
                None => {
                    tokio::select! {
                        () = fires(interrupt.as_mut()) => Some(Cause::Interrupt),
                        () = fires(terminate.as_mut()) => Some(Cause::Terminate),
                    }
                }
            };
            if let Some(cause) = cause {
                return cause;
            }
            tracing::warn!(
                "the hangar ownership watchdog ended without reporting a loss; \
                 continuing to serve and watching signals only"
            );
            self.lock_lost = None;
        }
    }
}

/// An observer of the shutdown seam, clonable so the boot phase and the run
/// loop can both wait on the SAME first cause.
///
/// [`install`] is what boot calls: it registers the signal handlers immediately
/// — before the store, the migrations and the socket bind — so a SIGTERM
/// arriving mid-boot is queued and answered rather than killing the process on
/// the OS default disposition with the ownership lock still on disk.
#[derive(Debug, Clone)]
pub struct Handle {
    cause: tokio::sync::watch::Receiver<Option<Cause>>,
}

impl Handle {
    /// Resolve with the first cause that asked the daemon to stop.
    pub async fn recv(&mut self) -> Cause {
        loop {
            // Bound the borrow to this statement: a `watch::Ref` held across the
            // `changed().await` below would block every publisher.
            let seen = *self.cause.borrow_and_update();
            if let Some(cause) = seen {
                return cause;
            }
            if self.cause.changed().await.is_err() {
                // The publisher task is gone, so no cause can ever arrive. Park
                // rather than inventing one: a dropped channel is not a
                // shutdown request, and resolving here would tear the daemon
                // down on a task fault.
                never().await;
            }
        }
    }
}

/// Install the signal handlers NOW and publish the first shutdown cause.
///
/// Registration happens synchronously in this call, so the handlers are live
/// before the caller does any further work; the waiting happens on a spawned
/// task feeding every [`Handle`].
#[must_use]
pub fn install(lock_lost: Option<tokio::sync::oneshot::Receiver<i32>>) -> Handle {
    let mut watch = Watch::new();
    if let Some(lock_lost) = lock_lost {
        watch = watch.on_lock_lost(lock_lost);
    }
    let (tx, rx) = tokio::sync::watch::channel(None);
    tokio::spawn(async move {
        let cause = watch.recv().await;
        let _ = tx.send(Some(cause));

        // Keep OWNING the signal streams after publishing. tokio never
        // unregisters a handler, so a task that returned here would leave
        // SIGTERM caught and then silently discarded — and a daemon whose
        // teardown wedges (a hung `tmux` call, a run that will not abort) could
        // no longer be stopped by the supported command at all, while still
        // holding the home's lock.
        //
        // A second signal escalates instead: the operator asked twice, and the
        // graceful path has demonstrably not finished. Exiting here skips the
        // remaining teardown deliberately — that is what "again" means — and is
        // still better than an unkillable process holding a home.
        let second = watch.recv().await;
        tracing::warn!(
            signal = second.as_str(),
            "second shutdown signal during teardown; exiting immediately"
        );
        std::process::exit(1);
    });
    Handle { cause: rx }
}

impl Default for Watch {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Signals are process-wide: two tests raising SIGTERM concurrently would
    /// race for one delivery, and either could observe the other's.
    ///
    /// Async-aware, because every holder keeps it across an `await`.
    static SIGNAL_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// The behavioural difference between the two causes, stated once so the
    /// run loop's teardown cannot drift from the documented contract.
    #[test]
    fn only_a_foreground_interrupt_reaps_attached_sessions() {
        assert!(Cause::Interrupt.reaps_interactive_sessions());
        assert!(
            !Cause::Terminate.reaps_interactive_sessions(),
            "daemon stop/restart must not kill the operator's attached panes"
        );
    }

    #[test]
    fn each_cause_names_its_signal() {
        assert_eq!(Cause::Interrupt.as_str(), "SIGINT");
        assert_eq!(Cause::Terminate.as_str(), "SIGTERM");
        assert_eq!(Cause::LockLost(7).as_str(), "lock-lost");
    }

    /// Losing the home is not a reason to kill the operator's attached panes:
    /// the daemon that now owns the home adopts them.
    #[test]
    fn a_lost_lock_leaves_attached_sessions_alone() {
        assert!(!Cause::LockLost(7).reaps_interactive_sessions());
    }

    #[tokio::test]
    async fn a_resolved_watchdog_stands_the_daemon_down() {
        let _serialised = SIGNAL_LOCK.lock().await;
        let (tx, rx) = tokio::sync::oneshot::channel();
        let mut watch = Watch::new().on_lock_lost(rx);
        tx.send(9001).expect("send new owner");
        assert_eq!(watch.recv().await, Cause::LockLost(9001));
    }

    /// A watchdog that died must not be read as a lost lock — that would shut
    /// the daemon down on a task panic.
    #[tokio::test]
    async fn a_dropped_watchdog_is_not_a_lost_lock() {
        // Any installed `Watch` sees process-wide signals, so this must not
        // overlap a sibling test that raises one — it would observe that
        // delivery and read as "the seam resolved".
        let _serialised = SIGNAL_LOCK.lock().await;
        let (tx, rx) = tokio::sync::oneshot::channel::<i32>();
        let mut watch = Watch::new().on_lock_lost(rx);
        drop(tx);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(200), watch.recv())
                .await
                .is_err(),
            "a dropped watchdog resolved the shutdown seam"
        );
    }

    /// ...and the daemon must still ANSWER signals afterwards.
    ///
    /// The regression this pins: handling the dropped watchdog inside a
    /// `select!` arm stranded the signal streams, because `select!` drops the
    /// losing branches' futures before an arm body runs. The daemon then
    /// ignored SIGTERM forever — `daemon stop` would burn its whole budget and
    /// only SIGKILL could end the process, skipping every guard's release.
    ///
    /// `a_dropped_watchdog_is_not_a_lost_lock` cannot see that: "still waiting"
    /// and "hung forever" look identical to a timeout. This one delivers a real
    /// signal, which is the only way to tell them apart.
    ///
    /// Safe to raise in-process: `Watch::new()` installs tokio's handler for
    /// SIGTERM before the raise, so the default terminate disposition is already
    /// replaced. Serialised against the sibling raise test by `SIGNAL_LOCK` —
    /// signals are process-wide, and two watches would race for one delivery.
    #[tokio::test]
    async fn signals_still_arrive_after_the_watchdog_is_dropped() {
        let _serialised = SIGNAL_LOCK.lock().await;
        let (tx, rx) = tokio::sync::oneshot::channel::<i32>();
        let mut watch = Watch::new().on_lock_lost(rx);
        drop(tx);

        tokio::task::spawn_blocking(|| {
            std::thread::sleep(std::time::Duration::from_millis(50));
            nix::sys::signal::raise(nix::sys::signal::Signal::SIGTERM).expect("raise SIGTERM");
        });

        let cause = tokio::time::timeout(std::time::Duration::from_secs(5), watch.recv())
            .await
            .expect("a dropped watchdog must not strand the signal handlers");
        assert_eq!(cause, Cause::Terminate);
    }

    /// The plain path, so the raise-based proof above is anchored: a SIGTERM
    /// with a live watchdog attached still reads as a terminate.
    #[tokio::test]
    async fn sigterm_resolves_the_seam() {
        let _serialised = SIGNAL_LOCK.lock().await;
        let (_tx, rx) = tokio::sync::oneshot::channel::<i32>();
        let mut watch = Watch::new().on_lock_lost(rx);

        tokio::task::spawn_blocking(|| {
            std::thread::sleep(std::time::Duration::from_millis(50));
            nix::sys::signal::raise(nix::sys::signal::Signal::SIGTERM).expect("raise SIGTERM");
        });

        let cause = tokio::time::timeout(std::time::Duration::from_secs(5), watch.recv())
            .await
            .expect("SIGTERM must resolve the shutdown seam");
        assert_eq!(cause, Cause::Terminate);
    }
}
