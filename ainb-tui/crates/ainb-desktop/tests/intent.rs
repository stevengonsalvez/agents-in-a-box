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
        assert_eq!(Intent::from(renderer), intent);
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
