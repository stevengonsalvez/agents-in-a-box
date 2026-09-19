//! The plugins_host frame stays small whatever a plugin's render error says
//! (D3p-f review). A render error is scrubbed, then cut to a small cap with a
//! flag: an uncapped error could push the section past the frame ceiling, the
//! section would be withheld whole, and the desktop placeholder would then say
//! a registered, failing plugin is not loaded.

use ainb_app::app::sections::PluginPresence;
use ainb_app::wire::frame::{Frame, HostId, MAX_FRAME_BYTES};
use ainb_app::{AppState, SectionId};

fn failing_witr(error: String) -> AppState {
    let mut state = AppState::with_config(ainb_app::config::AppConfig::default());
    let host = &mut state.plugins_host;
    host.plugin_presence.insert(
        "witr".to_string(),
        PluginPresence {
            registered: true,
            ..Default::default()
        },
    );
    host.plugin_render_errors.insert("witr".to_string(), error);
    state
}

#[test]
fn a_huge_render_error_is_cut_and_the_section_still_frames() {
    let state = failing_witr("x".repeat(8 * 1024 * 1024));
    let frame = Frame::new(&state, SectionId::PluginsHost, HostId::local());
    let bytes = serde_json::to_vec(&frame).expect("encodes").len();
    assert!(
        bytes < MAX_FRAME_BYTES,
        "the plugins_host frame is {bytes} bytes"
    );

    let error = &frame.body()["plugin_render_errors"]["witr"];
    assert_eq!(error["cut"], true, "{error}");
    let text = error["text"].as_str().expect("the error text");
    assert!(
        text.chars().count() <= 512,
        "{} chars",
        text.chars().count()
    );
    assert_eq!(frame.body()["plugin_presence"]["witr"]["registered"], true);
}

#[test]
fn a_short_render_error_is_framed_whole_and_scrubbed() {
    let token = format!("ghp_{}", "a1B2c3D4e5".repeat(4));
    let state = failing_witr(format!("spawn failed with {token}"));
    let frame = Frame::new(&state, SectionId::PluginsHost, HostId::local());
    let error = &frame.body()["plugin_render_errors"]["witr"];
    assert_eq!(error["cut"], false, "{error}");
    let text = error["text"].as_str().expect("the error text");
    assert!(text.starts_with("spawn failed with "), "{text}");
    assert!(!text.contains(&token), "{text}");
}
