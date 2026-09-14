//! Reserved snapshot-topic names.
//!
//! Topics are free-form strings on the wire (`host/snapshot/publish`
//! takes any `topic`), but a few names carry host-level semantics.
//! Those live here so the host, the SDK, and in-tree plugins dispatch
//! on byte-identical strings — same rationale as [`crate::methods`].
//!
//! Data-plane topics owned by specific plugins (e.g. the session-reader's
//! `sessions.usage_data`) are NOT listed here; they're plugin contracts,
//! not host contracts.

/// Plugin asks the host to close its focused screen.
///
/// Published by a plugin when it receives an `Esc` it has no internal
/// state left to consume (no zoom, no overlay, no filter chip — its
/// root view). The host's event loop polls this topic by version and
/// navigates back to the screen the panel was opened from.
///
/// Payload: JSON `{"screen_id": "<screen the plugin wants closed>"}`.
/// The host ignores requests whose `screen_id` doesn't match the
/// currently-focused screen, so a stale publish can't close a screen
/// the user has since navigated to.
pub const UI_CLOSE_REQUEST: &str = "ui.close_request";

/// A plugin's view state, for a host that draws the plugin's screen itself.
///
/// Published by a plugin whenever the state its screen shows changes. The
/// payload is the plugin's own JSON view of that state, a contract between
/// the plugin and the renderers that draw it; the host reads it by version
/// and never interprets it.
pub const UI_STATE: &str = "ui.state";

/// JSON payload for [`UI_CLOSE_REQUEST`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UiCloseRequest {
    /// Screen id the plugin wants closed (must match the host's
    /// currently-focused screen for the request to be honoured).
    pub screen_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_request_round_trips() {
        let req = UiCloseRequest {
            screen_id: "analytics".to_string(),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(json, r#"{"screen_id":"analytics"}"#);
        let back: UiCloseRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back, req);
    }
}
