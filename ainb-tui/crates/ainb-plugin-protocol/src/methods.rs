//! JSON-RPC method-name constants.
//!
//! Method strings live here so the SDK, the runtime, and the testkit
//! all dispatch on byte-identical names. Names follow the JSON-RPC
//! convention `<namespace>/<verb_or_object>`. Two namespaces:
//!
//! - `plugin/*` — host calls into the plugin (request or notification).
//! - `host/*`   — plugin calls into the host (request or notification).

// ---------------------------------------------------------------------
// plugin/* — host -> plugin
// ---------------------------------------------------------------------

/// Host informs plugin of capabilities granted + manifest path. First call after spawn.
pub const PLUGIN_INIT: &str = "plugin/init";

/// Host requests graceful shutdown. Plugin should flush, exit 0.
pub const PLUGIN_SHUTDOWN: &str = "plugin/shutdown";

/// Host requests a render for a viewport; plugin replies with a `WireBuffer`.
pub const PLUGIN_RENDER: &str = "plugin/render";

/// Host pushes a snapshot/event update. Notification — no response expected.
pub const PLUGIN_HANDLE_EVENT: &str = "plugin/handle_event";

/// Host forwards a single key event to the plugin owning the focused screen.
/// Notification — no response expected. Ordering is preserved across the same
/// transport as `plugin/handle_event` so key sequences arrive in send order.
pub const PLUGIN_HANDLE_KEY: &str = "plugin/handle_key";

/// Host dispatches a CLI namespace + argv to the plugin; plugin replies with stdout/stderr/exit.
pub const PLUGIN_CLI_DISPATCH: &str = "plugin/cli_dispatch";

// ---------------------------------------------------------------------
// host/* — plugin -> host
// ---------------------------------------------------------------------

/// Plugin asks host for the latest snapshot for a topic.
pub const HOST_SNAPSHOT_GET: &str = "host/snapshot/get";

/// Plugin publishes a snapshot for a topic. Notification.
pub const HOST_SNAPSHOT_PUBLISH: &str = "host/snapshot/publish";

/// Plugin subscribes to snapshot updates on a topic.
///
/// Host will subsequently send `plugin/handle_event` notifications
/// each time the topic's snapshot changes.
pub const HOST_SNAPSHOT_SUBSCRIBE: &str = "host/snapshot/subscribe";

/// Plugin invokes an action on another plugin (or the host) with a timeout.
pub const HOST_ACTION_INVOKE: &str = "host/action/invoke";

/// Plugin emits a structured log line. Notification.
pub const HOST_LOG: &str = "host/log";

/// Plugin reads a directory through the host (capability-gated).
pub const HOST_FS_READ_DIR: &str = "host/fs/read_dir";

/// Plugin reads a file through the host (capability-gated).
pub const HOST_FS_READ_FILE: &str = "host/fs/read_file";

/// Plugin fetches a URL through the host (capability-gated).
pub const HOST_NETWORK_FETCH: &str = "host/network/fetch";

/// Every method name registered by the protocol, in stable order.
///
/// Used by the runtime's static method-existence check and by the CTS
/// to assert the dispatcher knows every spec'd method.
pub const ALL_METHODS: &[&str] = &[
    PLUGIN_INIT,
    PLUGIN_SHUTDOWN,
    PLUGIN_RENDER,
    PLUGIN_HANDLE_EVENT,
    PLUGIN_HANDLE_KEY,
    PLUGIN_CLI_DISPATCH,
    HOST_SNAPSHOT_GET,
    HOST_SNAPSHOT_PUBLISH,
    HOST_SNAPSHOT_SUBSCRIBE,
    HOST_ACTION_INVOKE,
    HOST_LOG,
    HOST_FS_READ_DIR,
    HOST_FS_READ_FILE,
    HOST_NETWORK_FETCH,
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn method_names_unique() {
        let set: HashSet<&&str> = ALL_METHODS.iter().collect();
        assert_eq!(set.len(), ALL_METHODS.len(), "duplicate method name");
    }

    #[test]
    fn plugin_methods_namespaced() {
        for m in [
            PLUGIN_INIT,
            PLUGIN_SHUTDOWN,
            PLUGIN_RENDER,
            PLUGIN_HANDLE_EVENT,
            PLUGIN_HANDLE_KEY,
            PLUGIN_CLI_DISPATCH,
        ] {
            assert!(m.starts_with("plugin/"), "{m} missing plugin/ namespace");
        }
    }

    #[test]
    fn all_methods_contains_plugin_handle_key() {
        assert!(
            ALL_METHODS.contains(&PLUGIN_HANDLE_KEY),
            "PLUGIN_HANDLE_KEY missing from ALL_METHODS registry"
        );
        assert_eq!(PLUGIN_HANDLE_KEY, "plugin/handle_key");
    }

    #[test]
    fn host_methods_namespaced() {
        for m in [
            HOST_SNAPSHOT_GET,
            HOST_SNAPSHOT_PUBLISH,
            HOST_SNAPSHOT_SUBSCRIBE,
            HOST_ACTION_INVOKE,
            HOST_LOG,
            HOST_FS_READ_DIR,
            HOST_FS_READ_FILE,
            HOST_NETWORK_FETCH,
        ] {
            assert!(m.starts_with("host/"), "{m} missing host/ namespace");
        }
    }
}
