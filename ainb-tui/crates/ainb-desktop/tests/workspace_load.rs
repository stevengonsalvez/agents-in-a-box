//! The workspace load the window starts at launch lands through the host's
//! tick. Its own test binary: the load reads and may write under HOME, which
//! the host contract asserts nothing touches.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ainb_app::config::AppConfig;
use ainb_app::wire::frame::{FrameBatch, Subscription};
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
