//! A settings row whose value the host runs is refused from the window
//! (#1224), by name and by the key sequence, at the seam the webview's
//! intents cross: `DesktopHost::refused_from_renderer`.

use std::cell::RefCell;
use std::rc::Rc;

use ainb_app::config::AppConfig;
use ainb_app::config::registry::{self, registry_key};
use ainb_app::config::renderer_edit::{DENIED, DENIED_REASON, NOT_DRAWN_REASON, SECRET_REASON};
use ainb_app::wire::frame::{FrameBatch, HostId, Subscription};
use ainb_app::{Chord, CommandId, Intent, Keymap, SectionId};
use ainb_desktop::host::DesktopHost;

mod support;

type Log = Rc<RefCell<Vec<String>>>;

fn host(log: &Log) -> DesktopHost<impl FnMut(FrameBatch)> {
    support::isolated_home();
    let log = Rc::clone(log);
    DesktopHost::new(
        AppConfig::default(),
        Keymap::defaults(),
        HostId::local(),
        Subscription::only(&[SectionId::Config]),
        move |batch: FrameBatch| {
            for frame in batch.frames {
                log.borrow_mut().push(format!("frame {}", frame.section));
            }
        },
    )
}

/// `config.set_row` naming `key`.
fn set_row(key: &str) -> Intent {
    Intent::Command(
        CommandId::new("config.set_row"),
        serde_json::json!({ "key": key, "value": { "Text": "evil" }, "revision": 0 }),
    )
}

/// Walk the reducer onto the Config screen the way the settings page does,
/// then filter its rows to `pattern` through the screen's own `/` search, so
/// the first match is under the cursor. `None` when no row of the default
/// config matches the pattern (a map with no entries).
fn on_row(host: &mut DesktopHost<impl FnMut(FrameBatch)>, pattern: &str) -> Option<String> {
    for step in [
        "config.search.cancel",
        "global.go_home",
        "home.config",
        "config.search",
    ] {
        let _ = host.dispatch(Intent::Command(
            CommandId::new(step),
            serde_json::Value::Null,
        ));
    }
    let filter = pattern.rsplit("*.").next().unwrap_or(pattern);
    let _ = host.dispatch(Intent::Text(filter.to_string()));
    let screen = &host.state().config.config_screen_state;
    let key = screen.current_setting().map(|row| row.key.clone())?;
    (registry_key(&key) == pattern).then_some(key)
}

#[test]
fn a_spawn_row_is_refused_by_name_and_by_key_sequence() {
    let log = Log::default();
    let mut host = host(&log);
    let mut by_key = 0;
    for (pattern, _) in DENIED {
        // A secret row on the deny list is refused as a secret first; the
        // secret test below covers it.
        if registry::row(pattern).is_some_and(|row| matches!(row.kind, registry::RowKind::Secret)) {
            continue;
        }
        // By name, whether or not the default config has such a row: the
        // key is judged, not the row.
        let named = pattern.replace('*', "sample");
        let refusal = host
            .refused_from_renderer(&set_row(&named))
            .unwrap_or_else(|| panic!("{named} by name"));
        assert_eq!(refusal.reason, DENIED_REASON, "{named}");
        assert_eq!(refusal.command.as_str(), "config.set_row");

        // By key sequence, on the real row when the default config has one.
        let Some(key) = on_row(&mut host, pattern) else {
            continue;
        };
        let refusal = host
            .refused_from_renderer(&Intent::Key(Chord::parse("enter").expect("chord")))
            .unwrap_or_else(|| panic!("{key} by Enter"));
        assert_eq!(refusal.reason, DENIED_REASON, "{key}");
        assert!(
            refusal.command.as_str().starts_with("config."),
            "{key}: {refusal:?}"
        );
        by_key += 1;
    }
    assert!(
        by_key >= 8,
        "the default config carries rows for the key path: {by_key}"
    );
}

#[test]
fn a_drawn_row_passes_and_an_undrawn_row_is_refused() {
    let log = Log::default();
    let mut host = host(&log);
    let key = on_row(&mut host, "workspace_defaults.branch_prefix").expect("a default row");
    assert_eq!(host.refused_from_renderer(&set_row(&key)), None);
    assert_eq!(
        host.refused_from_renderer(&Intent::Key(Chord::parse("enter").expect("chord"))),
        None
    );

    // Every registry row is classified, so an unclassified row can only be
    // one the schema does not have yet: judged by name, deny by default.
    let refusal = host
        .refused_from_renderer(&set_row("new.row.nobody.classified"))
        .expect("an unclassified row");
    assert_eq!(refusal.reason, NOT_DRAWN_REASON);
    let key = on_row(&mut host, "usage.plan.id").expect("a default row");
    let refusal = host.refused_from_renderer(&set_row(&key)).expect("the plugin's row");
    assert_eq!(refusal.reason, DENIED_REASON);
}

/// A secret row is refused from the window by name and by Enter, with the
/// reason that says where a secret is set; so is the keychain prompt.
#[test]
fn a_secret_row_and_the_keychain_prompt_are_refused_from_the_window() {
    let log = Log::default();
    let mut host = host(&log);
    let key = on_row(&mut host, "fleet.bridge.telegram.token").expect("a default secret row");
    let refusal = host.refused_from_renderer(&set_row(&key)).expect("by name");
    assert_eq!(refusal.reason, SECRET_REASON);
    let refusal = host
        .refused_from_renderer(&Intent::Key(Chord::parse("enter").expect("chord")))
        .expect("by Enter");
    assert_eq!(refusal.reason, SECRET_REASON);
    let refusal = host
        .refused_from_renderer(&Intent::Key(Chord::parse("ctrl+k").expect("chord")))
        .expect("the keychain prompt");
    assert_eq!(refusal.reason, SECRET_REASON);
}

/// A payload the row cannot parse is refused at the seam, not judged on the
/// row's placeholder and left for the reducer to drop.
#[test]
fn a_payload_that_does_not_fit_the_row_is_refused_closed() {
    let log = Log::default();
    let mut host = host(&log);
    on_row(&mut host, "workspace_defaults.branch_prefix").expect("a default row");
    let malformed = Intent::Command(
        CommandId::new("config.set_row"),
        serde_json::json!({ "key": "workspace_defaults.branch_prefix", "value": { "Text": "x" } }),
    );
    let refusal = host.refused_from_renderer(&malformed).expect("no revision");
    assert!(refusal.reason.contains("does not fit"), "{refusal:?}");
    let refusal = host
        .refused_from_renderer(&Intent::Command(
            CommandId::new("config.set_row"),
            serde_json::json!({ "bogus": true }),
        ))
        .expect("wrong fields");
    assert!(refusal.reason.contains("does not fit"), "{refusal:?}");
}

/// A printable key in a text-input context resolves to a synthesised text
/// action no row names. The gate cannot judge what it cannot name, so it
/// refuses rather than answering "not refused"; the window types with `Text`.
#[test]
fn a_key_the_gate_cannot_name_is_refused_closed() {
    let log = Log::default();
    let mut host = host(&log);
    for step in ["global.go_home", "home.config", "config.search"] {
        let _ = host.dispatch(Intent::Command(
            CommandId::new(step),
            serde_json::Value::Null,
        ));
    }
    assert!(host.state().config.config_screen_state.is_searching());

    let refusal = host
        .refused_from_renderer(&Intent::Key(Chord::parse("a").expect("chord")))
        .expect("a printable key in the search is refused");
    assert!(refusal.reason.contains("sends text instead"), "{refusal:?}");
    assert!(refusal.command.as_str().ends_with(".text"), "{refusal:?}");
    // The search's own bound rows are still judged by their row.
    assert_eq!(
        host.refused_from_renderer(&Intent::Key(Chord::parse("esc").expect("chord"))),
        None
    );
}
