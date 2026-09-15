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

#[test]
fn a_dispatch_during_ticks_returns() {
    let home = tempfile::tempdir().expect("scratch home");
    std::env::set_var("HOME", home.path());
    std::env::set_var("AINB_HOME", home.path());

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
