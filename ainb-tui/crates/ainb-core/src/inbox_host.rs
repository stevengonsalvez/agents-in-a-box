//! The terminal's inbox reader host (D3-prime): section 16 is read only while
//! the inbox screen is open, the rule the desktop applies to its `inbox`
//! subscription, so no daemon RPC is issued per tick for a screen nobody has
//! open.
//!
//! ```text
//!  tick ──current_screen == inbox?──▶ start reader ──drain──▶ section 16
//!       ──left the screen?─────────▶ drop reader, reset section
//! ```

use ainb_app::AppState;
use ainb_app::app::screens::ids;
use ainb_app::fleet::inbox_reader::{Dialer, InboxReader};

/// The reader, while the inbox screen is open, and the dialer it is started
/// with each time.
pub struct InboxHost {
    dialer: std::sync::Arc<Dialer>,
    reader: Option<InboxReader>,
}

impl InboxHost {
    /// A host that will dial through `dialer` whenever the screen opens.
    /// Starts nothing until [`Self::tick`] sees the screen.
    #[must_use]
    pub fn new(dialer: Dialer) -> Self {
        Self {
            dialer: std::sync::Arc::new(dialer),
            reader: None,
        }
    }

    /// Whether the reader is running, which it is exactly while the inbox
    /// screen is the current one.
    #[must_use]
    pub const fn running(&self) -> bool {
        self.reader.is_some()
    }

    /// Start or stop the reader to match the current screen, then fold what
    /// arrived. Must be called inside a tokio runtime, which the TUI loop is.
    pub fn tick(&mut self, state: &mut AppState) -> bool {
        let wanted = state.shell.current_screen == ids::INBOX;
        match (wanted, self.reader.is_some()) {
            (true, false) => {
                let dialer = std::sync::Arc::clone(&self.dialer);
                self.reader = Some(InboxReader::spawn(Box::new(move || dialer())));
            }
            (false, true) => {
                self.reader = None;
                return state.inbox_reset();
            }
            _ => {}
        }
        self.reader.as_mut().is_some_and(|reader| reader.drain_into(state))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::*;
    use ainb_hangar_client::DaemonError;

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
    async fn the_reader_runs_only_while_the_inbox_screen_is_open() {
        let (dialer, dials) = counting_dialer();
        let mut host = InboxHost::new(dialer);
        let mut state = AppState::new();
        settle(&mut host, &mut state, 5).await;
        assert!(!host.running());
        assert_eq!(dials.load(Ordering::SeqCst), 0, "home dials nothing");

        state.shell.current_screen = ids::INBOX.to_string();
        settle(&mut host, &mut state, 5).await;
        assert!(host.running());
        assert!(
            dials.load(Ordering::SeqCst) >= 1,
            "the screen starts the reads"
        );
        assert!(
            state.inbox.get().absent.is_some(),
            "a failed dial lands as the absent reason"
        );

        state.shell.current_screen = ids::HOME.to_string();
        host.tick(&mut state);
        assert!(!host.running());
        assert!(
            state.inbox.get().absent.is_none(),
            "leaving the screen resets the section"
        );
        let seen = dials.load(Ordering::SeqCst);
        settle(&mut host, &mut state, 30).await;
        assert_eq!(
            dials.load(Ordering::SeqCst),
            seen,
            "no read after the screen is left"
        );
    }
}
