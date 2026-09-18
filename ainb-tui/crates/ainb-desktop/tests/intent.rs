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
