//! The shell's host and executor are one lock: a dispatch arriving while ticks
//! run returns instead of deadlocking.

use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use ainb_app::config::AppConfig;
use ainb_app::wire::frame::{FrameBatch, HostId, Subscription};
use ainb_app::{Chord, Intent, Keymap, SectionId};
use ainb_desktop::executor::DesktopExecutor;
use ainb_desktop::host::DesktopHost;
use ainb_desktop::shell::Shell;

mod support;

#[test]
fn a_dispatch_during_ticks_returns() {
    support::isolated_home();

    // Frames are sent across threads in the window, so the sink here is Send.
    let (frames, received) = mpsc::channel::<FrameBatch>();
    let host = DesktopHost::new(
        AppConfig::default(),
        Keymap::defaults(),
        HostId::local(),
        Subscription::only(&[SectionId::Shell]),
        move |batch: FrameBatch| {
            let _ = frames.send(batch);
        },
    );
    let shell = Arc::new(Shell::new(host, DesktopExecutor::new(None)));

    let ticking = {
        let shell = Arc::clone(&shell);
        std::thread::spawn(move || {
            for _ in 0..500 {
                shell.tick();
            }
        })
    };
    let (done, finished) = mpsc::channel();
    let dispatching = {
        let shell = Arc::clone(&shell);
        std::thread::spawn(move || {
            for spelling in ["s", "q"].iter().cycle().take(200) {
                shell.dispatch(Intent::Key(Chord::parse(spelling).expect("valid chord")));
            }
            let _ = done.send(());
        })
    };

    finished
        .recv_timeout(Duration::from_secs(30))
        .expect("dispatch returned while the shell was ticking");
    dispatching.join().expect("dispatch thread");
    ticking.join().expect("tick thread");
    assert!(
        received.try_iter().count() > 0,
        "the shell framed its moves"
    );
}

/// P6e: the flush the app's exit paths call is bounded by its own argument,
/// the wait for the shell's lock included. A tick stuck holding that lock
/// would otherwise hold the whole exit, which is the hang the bound exists to
/// stop; the exit hears that the queue was never reached and goes.
#[test]
fn a_flush_gives_up_on_a_shell_that_stays_busy() {
    support::isolated_home();

    let host = DesktopHost::new(
        AppConfig::default(),
        Keymap::defaults(),
        HostId::local(),
        Subscription::only(&[SectionId::Shell]),
        |_batch: FrameBatch| {},
    );
    let shell = Shell::new(host, DesktopExecutor::new(None));

    let bound = Duration::from_millis(300);
    let busy = shell.hold_for_tests();
    let started = std::time::Instant::now();
    let outcome = shell.flush_session_store_writes(bound);
    let waited = started.elapsed();
    drop(busy);

    assert!(
        outcome.is_none(),
        "the flush claimed a queue it never reached"
    );
    assert!(
        waited < bound * 4,
        "the flush waited past its bound on the shell's lock: {waited:?}"
    );

    // The shell is free again, so the same call reaches the queue and finds
    // nothing in it.
    assert_eq!(shell.flush_session_store_writes(bound), Some(0));
}
