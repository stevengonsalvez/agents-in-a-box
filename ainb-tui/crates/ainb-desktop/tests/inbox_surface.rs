//! The desktop inbox page (D3p-c) drives the reducer, never its own state: it
//! opens the inbox screen with the rows the terminal uses, sends the
//! whole-inbox sweep as `inbox.mark_all_read`, which the reducer turns into the
//! one effect the executor runs, and closes back to where it came from. The
//! sweep does nothing off the inbox screen, as the terminal's key does not.

use ainb_app::app::Effect;
use ainb_app::config::AppConfig;
use ainb_app::wire::frame::{FrameBatch, HostId, Subscription};
use ainb_app::{CommandId, Intent, Keymap, SectionId};
use ainb_desktop::host::DesktopHost;
use ainb_desktop::intent::{RendererIntent, refused_from_webview};

mod support;

/// What the webview sends: the page's intents, through the seam every window
/// intent crosses.
fn send<S: ainb_desktop::host::FrameSink>(host: &mut DesktopHost<S>, id: &str) -> Vec<Effect> {
    let keymap = Keymap::defaults();
    let id = CommandId::new(id);
    assert!(
        !refused_from_webview(&keymap, &id),
        "`{}` is refused from the webview",
        id.as_str()
    );
    let intent = Intent::try_from(RendererIntent::Command(id, serde_json::Value::Null))
        .expect("the seam passes it");
    host.dispatch(intent)
}

#[test]
fn the_page_opens_the_inbox_sweeps_it_read_and_closes_back() {
    support::isolated_home();
    let mut host = DesktopHost::new(
        AppConfig::default(),
        Keymap::defaults(),
        HostId::local(),
        Subscription::only(&[SectionId::Shell]),
        |_: FrameBatch| {},
    )
    .without_attention_poll();

    // Off the inbox screen the sweep is not active: nothing is sent.
    let effects = send(&mut host, "inbox.mark_all_read");
    assert!(
        !effects.iter().any(|effect| matches!(effect, Effect::InboxMarkAllRead)),
        "the sweep ran off its screen: {effects:?}"
    );

    // OPEN_INBOX in inbox.ts.
    let _ = send(&mut host, "global.go_home");
    let _ = send(&mut host, "home.inbox");
    assert_eq!(host.state().shell.current_screen, "inbox");

    let effects = send(&mut host, "inbox.mark_all_read");
    assert_eq!(
        effects
            .iter()
            .filter(|effect| matches!(effect, Effect::InboxMarkAllRead))
            .count(),
        1,
        "one sweep, the effect the executor runs: {effects:?}"
    );
    let again = send(&mut host, "inbox.mark_all_read");
    assert!(
        !again.iter().any(|effect| matches!(effect, Effect::InboxMarkAllRead)),
        "a second press while one is in flight sends nothing: {again:?}"
    );

    // CLOSE_INBOX in inbox.ts: back to where it opened from, then on to the
    // session list the window sits on, as closing settings does.
    let _ = send(&mut host, "inbox.back");
    assert_eq!(host.state().shell.current_screen, "home");
    let _ = send(&mut host, "home.sessions");
    assert_eq!(host.state().shell.current_screen, "session_list");
}
