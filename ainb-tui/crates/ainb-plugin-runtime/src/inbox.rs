//! Bounded, drop-oldest input inbox (#1087).
//!
//! A plugin's key and mouse events travel on their own priority channels so a
//! backlog of `HandleEvent` chunks cannot queue ahead of an Esc. Those channels
//! used to be unbounded: a registered plugin that stops reading its stdin
//! blocks the task's frame write, the task stops draining, and every keystroke
//! after that sat in memory for as long as the plugin stayed wedged.
//!
//! This queue holds at most `capacity` events. When it is full a new event
//! pushes the OLDEST one out and bumps a counter: the newest input is the one
//! the user just pressed (often the Esc that is trying to leave), so it is the
//! one worth keeping. Bounded `mpsc` cannot do that from the sender side, since
//! only the receiver can pop.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use tokio::sync::Notify;

/// Events a plugin's key or mouse inbox holds before it drops the oldest.
///
/// Human key auto-repeat runs near 30 events a second and a paste reaches the
/// host as text rather than keys, so this is about two seconds of held key: far
/// more than a plugin that is keeping up ever has queued.
pub const INPUT_INBOX_CAPACITY: usize = 64;

struct Shared<T> {
    queue: parking_lot::Mutex<VecDeque<T>>,
    capacity: usize,
    dropped: AtomicU64,
    closed: AtomicBool,
    notify: Notify,
}

/// Build a connected sender and receiver holding at most `capacity` events.
///
/// # Panics
///
/// When `capacity` is zero: a queue that can hold nothing would drop every
/// event it is given.
#[must_use]
pub fn drop_oldest<T>(capacity: usize) -> (DropOldestSender<T>, DropOldestReceiver<T>) {
    assert!(capacity > 0, "a drop-oldest inbox needs room for one event");
    let shared = Arc::new(Shared {
        queue: parking_lot::Mutex::new(VecDeque::with_capacity(capacity)),
        capacity,
        dropped: AtomicU64::new(0),
        closed: AtomicBool::new(false),
        notify: Notify::new(),
    });
    (
        DropOldestSender {
            shared: Arc::clone(&shared),
        },
        DropOldestReceiver { shared },
    )
}

/// The sending half. Cheap to clone; every clone feeds the same queue.
pub struct DropOldestSender<T> {
    shared: Arc<Shared<T>>,
}

impl<T> Clone for DropOldestSender<T> {
    fn clone(&self) -> Self {
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl<T> std::fmt::Debug for DropOldestSender<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DropOldestSender")
            .field("capacity", &self.shared.capacity)
            .field("dropped", &self.dropped())
            .finish_non_exhaustive()
    }
}

impl<T> DropOldestSender<T> {
    /// Queue `item`, pushing out the oldest queued event when the inbox is full.
    ///
    /// # Errors
    ///
    /// Hands `item` back when the receiver is gone: nothing will ever read it,
    /// the same answer an `mpsc` sender gives for a closed channel.
    pub fn send(&self, item: T) -> Result<(), T> {
        {
            // Checked under the lock the receiver's drop takes, so a send
            // cannot slip in after the receiver has gone and still answer `Ok`.
            let mut queue = self.shared.queue.lock();
            if self.shared.closed.load(Ordering::Acquire) {
                return Err(item);
            }
            if queue.len() >= self.shared.capacity {
                queue.pop_front();
                self.shared.dropped.fetch_add(1, Ordering::Relaxed);
            }
            queue.push_back(item);
        }
        self.shared.notify.notify_one();
        Ok(())
    }

    /// Events pushed out of a full inbox so far.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.shared.dropped.load(Ordering::Relaxed)
    }

    /// Events queued and not yet read.
    #[must_use]
    pub fn len(&self) -> usize {
        self.shared.queue.lock().len()
    }

    /// Whether nothing is queued.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The receiving half. Dropping it closes the inbox.
pub struct DropOldestReceiver<T> {
    shared: Arc<Shared<T>>,
}

impl<T> std::fmt::Debug for DropOldestReceiver<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DropOldestReceiver")
            .field("capacity", &self.shared.capacity)
            .finish_non_exhaustive()
    }
}

impl<T> DropOldestReceiver<T> {
    /// The oldest queued event, waiting for one when the inbox is empty.
    ///
    /// Never resolves to `None`: the sending half lives in the runtime's plugin
    /// map for as long as the task runs, so an empty inbox simply pends, as an
    /// idle `mpsc` receiver with a live sender does.
    ///
    /// Cancel-safe for `tokio::select!`: an event is taken only in the same
    /// poll that returns it, and a wakeup is registered before the queue is
    /// checked, so a send between the check and the wait is not missed.
    pub async fn recv(&mut self) -> Option<T> {
        loop {
            let notified = self.shared.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(item) = self.shared.queue.lock().pop_front() {
                return Some(item);
            }
            notified.await;
        }
    }
}

impl<T> Drop for DropOldestReceiver<T> {
    fn drop(&mut self) {
        let mut queue = self.shared.queue.lock();
        self.shared.closed.store(true, Ordering::Release);
        queue.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_inbox_drops_the_oldest_and_counts_it() {
        let (tx, mut rx) = drop_oldest::<u32>(64);
        for n in 0..100 {
            tx.send(n).expect("receiver alive");
        }
        assert_eq!(tx.len(), 64);
        assert_eq!(tx.dropped(), 36);
        let rt = tokio::runtime::Builder::new_current_thread().build().expect("runtime");
        let first = rt.block_on(rx.recv());
        assert_eq!(first, Some(36), "the 36 oldest went, the newest 64 stayed");
    }

    #[test]
    fn a_send_after_the_receiver_is_gone_hands_the_event_back() {
        let (tx, rx) = drop_oldest::<u32>(4);
        drop(rx);
        assert_eq!(tx.send(7), Err(7));
        assert_eq!(tx.dropped(), 0);
    }

    #[tokio::test]
    async fn a_waiting_receiver_wakes_on_a_send() {
        let (tx, mut rx) = drop_oldest::<u32>(4);
        let reader = tokio::spawn(async move { rx.recv().await });
        tokio::task::yield_now().await;
        tx.send(5).expect("receiver alive");
        let got = tokio::time::timeout(std::time::Duration::from_secs(5), reader)
            .await
            .expect("woke within 5s")
            .expect("reader task");
        assert_eq!(got, Some(5));
    }

    #[tokio::test]
    async fn a_cancelled_recv_loses_nothing() {
        let (tx, mut rx) = drop_oldest::<u32>(4);
        // A select that picks another arm drops the pending recv future.
        tokio::select! {
            biased;
            () = std::future::ready(()) => {}
            _ = rx.recv() => panic!("nothing was queued"),
        }
        tx.send(9).expect("receiver alive");
        assert_eq!(rx.recv().await, Some(9));
    }
}
