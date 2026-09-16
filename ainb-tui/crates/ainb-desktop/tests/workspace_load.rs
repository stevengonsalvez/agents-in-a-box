//! The workspace load the window starts at launch lands through the host's
//! tick. Its own test binary: the load reads and may write under HOME, which
//! the host contract asserts nothing touches.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ainb_app::config::AppConfig;
use ainb_app::wire::frame::{FrameBatch, HostId, Subscription};
use ainb_app::{Keymap, SectionId};
use ainb_desktop::host::DesktopHost;

mod support;

#[test]
fn a_started_workspace_load_is_applied_on_a_later_tick() {
    support::isolated_home();
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    let _runtime = runtime.enter();

    // `is_loading_workspaces` from each framed WorkspaceLoad body, in order.
    let loading = Arc::new(Mutex::new(Vec::<bool>::new()));
    let seen = Arc::clone(&loading);
    let mut host = DesktopHost::new(
        AppConfig::default(),
        Keymap::defaults(),
        HostId::local(),
        Subscription::only(&[SectionId::WorkspaceLoad]),
        move |batch: FrameBatch| {
            for frame in batch.frames {
                if let Some(flag) = frame.body()["is_loading_workspaces"].as_bool() {
                    seen.lock().expect("frame log").push(flag);
                }
            }
        },
    );

    host.start_workspace_load();
    // Checked before any tick: the runtime is multi-threaded, so the load can
    // finish before the first tick frames anything.
    assert!(
        host.state().workspace_load.is_loading_workspaces,
        "the host reports the load running as soon as it starts"
    );

    // Only a tick applies the result, so the last frame reads `false` only
    // once one has. The load is bounded by the state's 10 s Docker budget.
    let deadline = Instant::now() + Duration::from_secs(30);
    while loading.lock().expect("frame log").last() != Some(&false) {
        assert!(
            Instant::now() < deadline,
            "the load never landed: {:?}",
            loading.lock()
        );
        std::thread::sleep(Duration::from_millis(50));
        let _ = host.tick();
    }
}

/// A session another process creates reaches the sidebar only because the tick
/// keeps asking for a fresh scan, so the tick starts a second one on its own.
/// The sidebar's loading flag is the first load's alone, so the second scan is
/// seen through the host's own "a scan is running" answer.
#[test]
fn the_tick_starts_another_scan_once_the_cadence_has_passed() {
    support::isolated_home();
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    let _runtime = runtime.enter();

    let mut host = DesktopHost::new(
        AppConfig::default(),
        Keymap::defaults(),
        HostId::local(),
        Subscription::only(&[SectionId::WorkspaceLoad]),
        |_batch: FrameBatch| {},
    )
    // Long enough to be a cadence rather than a loop, short enough for a test.
    .rescanning_every(Duration::from_secs(1));

    // Nothing is started by hand here: the first scan is the tick's too.
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut scans = 0_u8;
    let mut running = false;
    while scans < 2 {
        assert!(
            Instant::now() < deadline,
            "the tick started {scans} scan(s), not two"
        );
        std::thread::sleep(Duration::from_millis(50));
        let _ = host.tick();
        let now = host.state().workspace_scan_running();
        if now && !running {
            scans += 1;
        }
        running = now;
    }
}

/// The daemon already knows when something happened, so its publish counter
/// starts a scan rather than the window waiting out the cadence (#1156).
#[test]
fn daemon_news_starts_a_scan_before_the_cadence_would() {
    use std::sync::atomic::Ordering;

    support::isolated_home();
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    let _runtime = runtime.enter();

    let mut host = DesktopHost::new(
        AppConfig::default(),
        Keymap::defaults(),
        HostId::local(),
        Subscription::only(&[SectionId::WorkspaceLoad]),
        |_batch: FrameBatch| {},
    )
    // Far longer than this test runs: a scan here is the news's, not the
    // cadence's.
    .rescanning_every(Duration::from_secs(600));
    // Held off, so the poller thread does not move the counter under the test.
    host.state().host.attention_poll_running.store(true, Ordering::Release);

    let _ = host.tick();
    assert!(
        !host.state().workspace_scan_running(),
        "nothing has happened yet, and the cadence is 10 minutes away"
    );

    host.state().host.daemon_attention_generation.fetch_add(1, Ordering::Release);
    let _ = host.tick();

    assert!(
        host.state().workspace_scan_running(),
        "the daemon reported news, so the window looked at once"
    );

    // Wait the scan out, so the test leaves no loader thread behind.
    let deadline = Instant::now() + Duration::from_secs(30);
    while host.state().workspace_scan_running() {
        assert!(Instant::now() < deadline, "the scan never finished");
        std::thread::sleep(Duration::from_millis(50));
        let _ = host.tick();
    }
}
