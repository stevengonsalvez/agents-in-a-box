//! A settings row whose value the host runs is refused from the window
//! (#1224), by name and by the key sequence, at the seam the webview's
//! intents cross: `DesktopHost::refused_from_renderer`.

use std::cell::RefCell;
use std::rc::Rc;

use ainb_app::config::AppConfig;
use ainb_app::config::registry::registry_key;
use ainb_app::config::renderer_edit::{DENIED, DENIED_REASON, NOT_DRAWN_REASON};
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
        serde_json::json!({ "key": key, "value": { "Text": "evil" } }),
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

    let key = on_row(&mut host, "usage.plan.id").expect("a default row");
    let refusal = host.refused_from_renderer(&set_row(&key)).expect("an undrawn row");
    assert_eq!(refusal.reason, NOT_DRAWN_REASON);
    let refusal = host
        .refused_from_renderer(&Intent::Key(Chord::parse("enter").expect("chord")))
        .expect("an undrawn row by Enter");
    assert_eq!(refusal.reason, NOT_DRAWN_REASON);
}
