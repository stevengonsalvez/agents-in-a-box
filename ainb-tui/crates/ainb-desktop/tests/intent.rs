//! The webview's intents share `Intent`'s spelling, minus the pointer.

use ainb_app::{Chord, CommandId, Intent};
use ainb_desktop::intent::RendererIntent;

#[test]
fn a_renderer_intent_reads_the_intent_wire_spelling() {
    for intent in [
        Intent::Key(Chord::parse("ctrl+k").expect("valid chord")),
        Intent::Command(
            CommandId::new("global.open_learnings"),
            serde_json::Value::Null,
        ),
        Intent::Text("pasted".to_string()),
    ] {
        let wire = serde_json::to_value(&intent).expect("serialises");
        let renderer: RendererIntent =
            serde_json::from_value(wire).expect("the renderer subset reads it");
        assert_eq!(Intent::try_from(renderer), Ok(intent));
    }
}

#[test]
fn a_host_authored_command_is_refused() {
    for id in ainb_app::app::reports::ids::ALL
        .iter()
        .chain(ainb_app::app::plugin_action::ids::ALL)
    {
        let renderer = RendererIntent::Command(CommandId::new(*id), serde_json::Value::Null);
        assert_eq!(
            Intent::try_from(renderer).map_err(|refusal| refusal.command),
            Err(CommandId::new(*id)),
            "`{id}` came from the webview"
        );
    }
}

#[test]
fn a_pointer_intent_is_refused() {
    let wire = serde_json::to_value(Intent::Mouse(
        ainb_app::Pos { x: 1, y: 1 },
        ainb_app::Btn::Left,
    ))
    .expect("serialises");
    assert!(serde_json::from_value::<RendererIntent>(wire).is_err());
}

/// The window reads a refusal as `{ command, reason }` to toast it.
#[test]
fn a_refusal_reads_as_the_row_and_the_reason() {
    let id = ainb_app::app::reports::ids::ALL[0];
    let renderer = RendererIntent::Command(CommandId::new(id), serde_json::Value::Null);
    let refusal = Intent::try_from(renderer).expect_err("host-authored");
    assert_eq!(
        serde_json::to_value(&refusal).expect("serialises"),
        serde_json::json!({ "command": id, "reason": refusal.reason })
    );
}

/// The updater from the webview: the window may run the update the host
/// resolved (check, apply, roll back, discard the previous, read the
/// settings); the channel and the tag are the terminal's and the config
/// file's, never the window's. One list, shared with the palette gate.
#[test]
fn the_window_may_run_the_update_but_never_choose_where_it_comes_from() {
    use ainb_desktop::intent::{refused_from_webview, update, update_refusal};
    let keymap = ainb_app::Keymap::defaults();
    for id in update::FROM_WEBVIEW {
        assert!(update_refusal(id).is_none(), "{id} refused");
        assert!(
            !refused_from_webview(&keymap, &CommandId::new(id)),
            "{id} refused by the one list"
        );
    }
    let refusal =
        update_refusal(update::SET_SETTINGS).expect("the channel is not the window's to set");
    assert_eq!(refusal.command, CommandId::new(update::SET_SETTINGS));
    assert!(refused_from_webview(
        &keymap,
        &CommandId::new(update::SET_SETTINGS)
    ));
    assert_eq!(update::ALL.len(), update::FROM_WEBVIEW.len() + 1);
    assert!(update::ALL.iter().all(|id| id.starts_with("update.")));
}

/// A toast is scrubbed (control and format characters, paths) and then cut,
/// in that order, so what is cut never counts toward the length.
#[test]
fn a_toast_is_scrubbed_then_cut() {
    use ainb_desktop::intent::{MAX_TOAST_CHARS, toast_text};
    let long = format!(
        "Update not installed: \u{202E}{}",
        "x".repeat(MAX_TOAST_CHARS * 2)
    );
    let shown = toast_text(&long);
    assert!(!shown.contains('\u{202E}'));
    assert_eq!(shown.chars().count(), MAX_TOAST_CHARS);
    assert_eq!(
        toast_text("removing /Users/x/Applications/A.app failed"),
        "removing <path> failed"
    );
    let controlled = format!("{}ok", "\u{0007}".repeat(MAX_TOAST_CHARS));
    assert_eq!(toast_text(&controlled), "ok");
}

/// The desktop's watchable plugin screens are `PLUGIN_SCREENS` less
/// `analytics`: the stats tab draws burndown's counters from the daemon's
/// projection, so a cell painting burndown beside it would show them twice
/// (D3p-f).
#[test]
fn the_desktop_watches_every_plugin_screen_but_analytics() {
    let all: Vec<&str> = ainb_app::app::screens::builtin::PLUGIN_SCREENS
        .iter()
        .map(|(screen, _)| *screen)
        .filter(|screen| *screen != ainb_app::app::screens::ids::ANALYTICS)
        .collect();
    assert_eq!(
        ainb_desktop::intent::DESKTOP_WATCHABLE_SCREENS,
        all.as_slice()
    );
    assert_eq!(all, ["witr", "learnings", "abtop", "hangar"]);
}

/// A watch for `analytics` is refused at the seam with its own reason, ahead
/// of the host-authored refusal every plugin action gets from the window.
#[test]
fn a_watch_for_analytics_is_refused_with_its_reason() {
    let watch = |screen: &str| {
        let Intent::Command(id, args) = ainb_app::app::plugin_action::watch_screen(
            screen,
            &ainb_app::wire::frame::HostId::local(),
            true,
            80,
            24,
        ) else {
            panic!("a watch is a command");
        };
        Intent::try_from(RendererIntent::Command(id, args)).expect_err("a watch is refused")
    };

    let analytics = watch("analytics");
    assert!(
        analytics.reason.contains("stats tab"),
        "{:?}",
        analytics.reason
    );
    assert_eq!(watch("witr").reason, "a host sends it, not the window");
}
