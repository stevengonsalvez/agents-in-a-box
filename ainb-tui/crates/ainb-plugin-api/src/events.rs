//! Plugin event payloads.
//!
//! Events are msgpack-serialised on the wire (host → plugin via the
//! `_handle_event` export). Custom events use `serde_json::Value` so plugins can
//! evolve their own scoped payloads without an ABI change.

use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "lifecycle", rename_all = "snake_case")]
pub enum LifecycleEvent {
    SessionStarted { session_id: String },
    SessionClosed { session_id: String },
    FocusChanged { from: Option<String>, to: Option<String> },
    HostShuttingDown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PluginEvent {
    Lifecycle(LifecycleEvent),
    /// Coarse tick; host emits roughly once per ~250ms.
    Tick { elapsed_ms: u64 },
    /// Plugin-scoped custom event (host treats payload as opaque).
    Custom { topic: String, payload: serde_json::Value },
    /// CLI/slash command dispatched to a plugin command.
    Command { name: String, args: Vec<String> },
    /// Key event forwarded when the plugin's screen owns focus.
    Key { code: String, modifiers: u8 },
    /// Synchronous request from another plugin via `ainb_request_data`.
    /// The handler must reply by calling `ainb_publish_reply(correlation_id, ...)`
    /// before returning. Payload is opaque bytes (the host preserves binary
    /// content via msgpack `ByteBuf` rather than the lossy utf-8 path used
    /// for `Custom`).
    Request {
        topic: String,
        correlation_id: u64,
        payload: ByteBuf,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_session_started_roundtrip_msgpack() {
        let ev = PluginEvent::Lifecycle(LifecycleEvent::SessionStarted {
            session_id: "abc".to_string(),
        });
        let bytes = rmp_serde::to_vec_named(&ev).unwrap();
        let back: PluginEvent = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(ev, back);
    }

    #[test]
    fn tick_event_roundtrip() {
        let ev = PluginEvent::Tick { elapsed_ms: 250 };
        let bytes = rmp_serde::to_vec_named(&ev).unwrap();
        let back: PluginEvent = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(ev, back);
    }

    #[test]
    fn request_event_preserves_binary_payload() {
        // Includes invalid-utf8 bytes that would have been mangled by the
        // legacy `Custom { payload: serde_json::Value::String(...) }` path.
        let ev = PluginEvent::Request {
            topic: "sessions.usage_data".into(),
            correlation_id: 42,
            payload: ByteBuf::from(vec![0xFF, 0x00, 0xC0, 0x80, 0x42]),
        };
        let bytes = rmp_serde::to_vec_named(&ev).unwrap();
        let back: PluginEvent = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(ev, back);
    }
}
