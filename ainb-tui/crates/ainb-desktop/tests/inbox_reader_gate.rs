//! The inbox reader runs exactly while a renderer subscribes to `inbox`
//! (D3-prime): no daemon read is issued for a screen nobody has open, the
//! first subscription starts the reads, and dropping the subscription stops
//! them and clears the section.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use ainb_app::config::AppConfig;
use ainb_app::fleet::inbox_reader::Dialer;
use ainb_app::wire::frame::{FrameBatch, HostId, Subscription};
use ainb_app::{Keymap, SectionId};
use ainb_desktop::host::DesktopHost;
use ainb_hangar_client::DaemonError;

mod support;

/// A dialer that counts every attempt and never connects.
fn counting_dialer() -> (Dialer, Arc<AtomicUsize>) {
    let dials = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&dials);
    let dialer: Dialer = Box::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
        Err(DaemonError::NoHome)
    });
    (dialer, dials)
}

fn host(sections: &[SectionId], dialer: Dialer) -> DesktopHost<impl FnMut(FrameBatch)> {
    support::isolated_home();
    DesktopHost::new(
        AppConfig::default(),
        Keymap::defaults(),
        HostId::local(),
        Subscription::only(sections),
        |_batch: FrameBatch| {},
    )
    .without_attention_poll()
    .reading_inbox_with(dialer)
}

async fn settle(host: &mut DesktopHost<impl FnMut(FrameBatch)>, ticks: usize) {
    for _ in 0..ticks {
        tokio::time::sleep(Duration::from_millis(20)).await;
        let _ = host.tick();
    }
}

#[tokio::test]
async fn no_read_is_issued_until_a_renderer_subscribes_to_inbox() {
    let (dialer, dials) = counting_dialer();
    let mut host = host(&[SectionId::Sessions], dialer);
    settle(&mut host, 5).await;
    assert_eq!(dials.load(Ordering::SeqCst), 0, "a host with no inbox reader dials nothing");
    assert!(!host.inbox_reader_running());

    host.subscribe(Subscription::only(&[SectionId::Sessions, SectionId::Inbox]));
    assert!(host.inbox_reader_running());
    settle(&mut host, 5).await;
    assert!(dials.load(Ordering::SeqCst) >= 1, "the first inbox subscription starts the reads");
    // The failed dial reaches the section as its absent reason.
    assert!(
        host.state().inbox.get().absent.is_some(),
        "a dial that fails leaves the section absent with the reason"
    );
}

#[tokio::test]
async fn dropping_the_inbox_subscription_stops_the_reads_and_clears_the_section() {
    let (dialer, dials) = counting_dialer();
    let mut host = host(&[SectionId::Sessions, SectionId::Inbox], dialer);
    host.subscribe(Subscription::only(&[SectionId::Sessions, SectionId::Inbox]));
    settle(&mut host, 5).await;
    assert!(dials.load(Ordering::SeqCst) >= 1);

    host.resubscribe(Subscription::only(&[SectionId::Sessions]));
    assert!(!host.inbox_reader_running());
    assert!(host.state().inbox.get().absent.is_none(), "the section is reset, not absent");
    // The task is aborted, so a wait long past the backoff sees no new dial.
    let seen = dials.load(Ordering::SeqCst);
    settle(&mut host, 30).await;
    assert_eq!(dials.load(Ordering::SeqCst), seen, "no read after the subscription went");
}
