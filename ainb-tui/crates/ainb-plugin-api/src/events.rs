//! Plugin event payloads.
//!
//! Events are msgpack-serialised on the wire (host → plugin via the
//! `_handle_event` export). Custom events carry an opaque
//! `Vec<u8>` payload — most often a msgpack body matching a topic-specific
//! wire schema (e.g. `ainb_plugin_types_sessions::UsageDataEvent` for
//! `sessions.usage_data`). Producers pick their own encoding and
//! consumers decode against the same schema crate. This keeps the
//! envelope opaque to the host so binary payloads (rmp-serde,
//! flatbuffers, raw bytes) survive the bus untouched — historic
//! `serde_json::Value` wrap mangled msgpack via utf8-lossy.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "lifecycle", rename_all = "snake_case")]
pub enum LifecycleEvent {
    SessionStarted { session_id: String },
    SessionClosed { session_id: String },
    FocusChanged { from: Option<String>, to: Option<String> },
    HostShuttingDown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PluginEvent {
    Lifecycle(LifecycleEvent),
    /// Coarse tick; host emits roughly once per ~250ms.
    Tick { elapsed_ms: u64 },
    /// Plugin-scoped custom event. Payload is opaque bytes (typically
    /// msgpack) — the topic identifies the schema. See module docs.
    Custom {
        topic: String,
        #[serde(with = "serde_bytes")]
        payload: Vec<u8>,
    },
    /// CLI/slash command dispatched to a plugin command.
    Command { name: String, args: Vec<String> },
    /// Key event forwarded when the plugin's screen owns focus.
    Key { code: String, modifiers: u8 },
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
}
