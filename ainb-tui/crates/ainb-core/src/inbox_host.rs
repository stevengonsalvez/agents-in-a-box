//! The terminal's inbox reader host (D3-prime): section 16 is read for the
//! TUI's whole life, slowly while the inbox screen is not the one open and
//! at the reader's normal cadence while it is, so the unread badge on the
//! session-list legend is live and the open screen is fresh.
//!
//! ```text
//!  launch ──▶ reader at OFF_SCREEN_POLL ──drain──▶ section 16 (badge)
//!  screen opened ──▶ reader at Timing::default().poll, fresh read now
//!  screen left ────▶ back to OFF_SCREEN_POLL; the section is kept
//! ```
//!
//! "Once the daemon is connected" is the reader's own dial: each read dials a
//! fresh client and backs off while the daemon is not there, so the first
//! read that lands is the connection, and until then the section carries the
//! dial's failure as its reason.

use std::time::Duration;

use ainb_app::AppState;
use ainb_app::app::screens::ids;
use ainb_app::fleet::inbox_reader::{Dialer, InboxReader, Timing};

/// The wait between reads while the inbox screen is not open.
pub const OFF_SCREEN_POLL: Duration = Duration::from_secs(30);

/// The reader, at the cadence the current screen asks for.
pub struct InboxHost {
    dialer: std::sync::Arc<Dialer>,
    reader: InboxReader,
    on_screen: bool,
}

impl InboxHost {
    /// Start reading through `dialer` at the off-screen cadence. Must be
    /// called inside a tokio runtime, which the TUI's main is.
    #[must_use]
    pub fn new(dialer: Dialer) -> Self {
        let dialer = std::sync::Arc::new(dialer);
        let reader = Self::spawn(&dialer, false);
        Self {
            dialer,
            reader,
            on_screen: false,
        }
    }

    /// Whether the reader is at the open screen's cadence.
    #[must_use]
    pub const fn on_screen(&self) -> bool {
        self.on_screen
    }

    /// The wait between reads at the current cadence.
    #[must_use]
    pub fn poll(&self) -> Duration {
        Self::timing(self.on_screen).poll
    }

    fn timing(on_screen: bool) -> Timing {
        let normal = Timing::default();
        if on_screen {
            normal
        } else {
            Timing {
                poll: OFF_SCREEN_POLL,
                ..normal
            }
        }
    }

    fn spawn(dialer: &std::sync::Arc<Dialer>, on_screen: bool) -> InboxReader {
        let dialer = std::sync::Arc::clone(dialer);
        InboxReader::spawn_timed(Box::new(move || dialer()), Self::timing(on_screen))
    }

    /// Match the cadence to the current screen, then fold what arrived. A
    /// cadence change restarts the reader, whose first read is immediate, so
    /// opening the screen reads at once. The section is never reset here:
    /// what the badge counts is what the screen will show.
    pub fn tick(&mut self, state: &mut AppState) -> bool {
        let wanted = state.shell.current_screen == ids::INBOX;
        if wanted != self.on_screen {
            self.on_screen = wanted;
            self.reader = Self::spawn(&self.dialer, wanted);
        }
        self.reader.drain_into(state)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use ainb_app::fleet::bridge::daemon::DaemonError;

    fn counting_dialer() -> (Dialer, Arc<AtomicUsize>) {
        let dials = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&dials);
        let dialer: Dialer = Box::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            Err(DaemonError::NoHome)
        });
        (dialer, dials)
    }

    async fn settle(host: &mut InboxHost, state: &mut AppState, ticks: usize) {
        for _ in 0..ticks {
            tokio::time::sleep(Duration::from_millis(20)).await;
            host.tick(state);
        }
    }

    #[tokio::test]
    async fn the_reader_runs_from_launch_and_the_screen_only_sets_the_cadence() {
        let (dialer, dials) = counting_dialer();
        let mut host = InboxHost::new(dialer);
        let mut state = AppState::new();
        settle(&mut host, &mut state, 5).await;
        assert!(!host.on_screen());
        assert_eq!(
            host.poll(),
            OFF_SCREEN_POLL,
            "slow while the screen is closed"
        );
        assert!(
            dials.load(Ordering::SeqCst) >= 1,
            "home reads too: the badge needs the count"
        );
        assert!(
            state.inbox.get().absent.is_some(),
            "a failed dial lands as the absent reason"
        );

        state.shell.current_screen = ids::INBOX.to_string();
        host.tick(&mut state);
        assert!(host.on_screen());
        assert_eq!(
            host.poll(),
            Timing::default().poll,
            "normal cadence on the screen"
        );
        let before = dials.load(Ordering::SeqCst);
        settle(&mut host, &mut state, 5).await;
        assert!(
            dials.load(Ordering::SeqCst) > before,
            "opening the screen reads at once"
        );

        state.shell.current_screen = ids::HOME.to_string();
        host.tick(&mut state);
        assert!(!host.on_screen());
        assert_eq!(host.poll(), OFF_SCREEN_POLL);
        assert!(
            state.inbox.get().absent.is_some(),
            "leaving the screen keeps the section"
        );
    }
}
