//! The desktop puts its reducer on the session list at launch, and every host
//! test that reads a session-list surface starts the same way. That move must
//! not depend on how fast the host runs.
//!
//! It used to be two clicks on the home sidebar's Sessions item, which the
//! reducer opens only when they fall inside the double-click window, timed with
//! the wall clock. A loaded runner that spent the window on the first click's
//! dispatch left the host on the home screen, so a test of a session-list
//! surface failed at random (the Pal conversation and transcript tests in
//! `host_contract`). Its own test binary: it installs a process-wide tunables
//! snapshot.

use ainb_app::app::Effect;
use ainb_app::config::AppConfig;
use ainb_app::wire::frame::{FrameBatch, HostId, Subscription};
use ainb_app::{Intent, Keymap};
use ainb_desktop::host::{DesktopHost, Executor};

mod support;

struct NoEffects;

impl Executor for NoEffects {
    fn execute(&mut self, _effect: Effect) -> Vec<Intent> {
        Vec::new()
    }
}

/// With a double-click window of zero no two clicks are ever a double click,
/// which is what a runner slower than the window sees: the host still lands on
/// the session list.
#[test]
fn open_sessions_lands_on_the_session_list_however_slow_the_host() {
    support::isolated_home();
    let mut config = AppConfig::default();
    config.ui.double_click_ms = 0;
    ainb_app::config::tunables::install_snapshot(config);

    let mut host = DesktopHost::new(
        AppConfig::default(),
        Keymap::defaults(),
        HostId::local(),
        Subscription::none(),
        |_batch: FrameBatch| {},
    )
    .without_attention_poll();

    host.open_sessions(&mut NoEffects);

    assert_eq!(
        host.state().shell.current_screen,
        ainb_app::app::screens::ids::SESSION_LIST,
        "the host is still on {:?}",
        host.state().shell.current_screen
    );
}
