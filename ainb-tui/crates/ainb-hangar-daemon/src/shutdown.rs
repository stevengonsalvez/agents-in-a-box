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
/// re-registering a handler on every tick.
#[derive(Debug)]
pub struct Watch {
    /// `None` only when the SIGTERM handler could not be installed; the daemon
    /// then degrades to SIGINT rather than refusing to run.
    terminate: Option<tokio::signal::unix::Signal>,
    /// Resolves with the new owner's pid if this daemon ever stops owning its
    /// home. `None` for a daemon that holds no lock (a test harness driving the
    /// run loop directly).
    lock_lost: Option<tokio::sync::oneshot::Receiver<i32>>,
}

impl Watch {
    /// Install the signal handlers.
    #[must_use]
    pub fn new() -> Self {
        let terminate =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(signal) => Some(signal),
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        "could not install the SIGTERM handler; `daemon stop` will kill \
                         this process ungracefully"
                    );
                    None
                }
            };
        Self {
            terminate,
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
    /// branches: `Signal::recv` is cancel-safe by contract, and a dropped
    /// `ctrl_c()` future leaves the process-wide handler installed.
    pub async fn recv(&mut self) -> Cause {
        // `Option::as_mut` + `futures::future::OptionFuture` would read better,
        // but a never-resolving branch keeps this to one dependency-free
        // `select!` whose arms are all cancel-safe.
        let signalled = async {
            match self.terminate.as_mut() {
                Some(terminate) => {
                    tokio::select! {
                        _ = tokio::signal::ctrl_c() => Cause::Interrupt,
                        _ = terminate.recv() => Cause::Terminate,
                    }
                }
                None => {
                    let _ = tokio::signal::ctrl_c().await;
                    Cause::Interrupt
                }
            }
        };
        match self.lock_lost.as_mut() {
            Some(lock_lost) => {
                tokio::select! {
                    cause = signalled => cause,
                    // A dropped sender (the watchdog task died) is not a lost
                    // lock, so it must never masquerade as one: fall back to
                    // waiting on the signals alone.
                    owner = lock_lost => match owner {
                        Ok(pid) => Cause::LockLost(pid),
                        Err(_) => std::future::pending().await,
                    },
                }
            }
            None => signalled.await,
        }
    }
}

impl Default for Watch {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let (tx, rx) = tokio::sync::oneshot::channel();
        let mut watch = Watch::new().on_lock_lost(rx);
        tx.send(9001).expect("send new owner");
        assert_eq!(watch.recv().await, Cause::LockLost(9001));
    }

    /// A watchdog that died must not be read as a lost lock — that would shut
    /// the daemon down on a task panic.
    #[tokio::test]
    async fn a_dropped_watchdog_is_not_a_lost_lock() {
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
}
