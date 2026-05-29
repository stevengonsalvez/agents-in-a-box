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
///
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

/// Plugin opens a cancellable streaming subscription on a topic.
///
/// Capability-gated. Host allocates a `stream_id` and thereafter emits
/// `plugin/handle_event` notifications under topic `stream:<stream_id>` until
/// the plugin cancels or the plugin process leaves the `Running` state.
pub const HOST_EVENT_STREAM_SUBSCRIBE: &str = "host/event_stream_subscribe";

/// Plugin cancels a previously opened event stream.
///
/// Notification — no response expected. The host also implicitly cancels
/// every stream a plugin holds when that plugin shuts down, crashes, or is
/// quarantined.
pub const HOST_EVENT_STREAM_CANCEL: &str = "host/event_stream_cancel";

/// Plugin asks the host to spawn a host-supervised child process.
///
/// Capability-gated by the plugin's `spawn_managed_subprocess` grant,
/// which MUST be list-form (an allow-list of binary names/paths) — a
/// bool-true grant is rejected with `-32003 MANIFEST_VALIDATION`. The
/// host owns the child's lifecycle: it spawns under the same pgrp /
/// `PDEATHSIG` leak guard as plugin processes and kills every managed
/// child on plugin shutdown or `Runtime` drop. On success the plugin
/// receives an opaque `handle` it can compose with
/// `host/event_stream_subscribe` (topic `managed:<handle>:stdout`).
pub const HOST_SPAWN_MANAGED_SUBPROCESS: &str = "host/spawn_managed_subprocess";

/// Plugin asks the host to dial a whitelisted `AF_UNIX` socket path.
///
/// Capability-gated by the plugin's `unix_socket_dial` grant, which MUST
/// be list-form (an allow-list of socket paths) — a bool-true grant is
/// rejected with `-32003 MANIFEST_VALIDATION` so there is no way to
/// request an unrestricted "dial any socket" grant. The host
/// canonicalizes the requested path (resolving symlinks) before
/// comparing it against the allow-list, defending against a symlink that
/// resolves outside the whitelist. On success the plugin receives an
/// opaque, host-minted `stream_id`; thereafter the host emits
/// `plugin/handle_event` notifications under topic `socket:<stream_id>`
/// carrying `data` / `eof` / `error` frames, and the plugin writes via
/// `host/unix_socket_send` and tears down via `host/unix_socket_close`.
pub const HOST_UNIX_SOCKET_DIAL: &str = "host/unix_socket_dial";

/// Plugin writes bytes to a previously dialled unix socket. Notification.
///
/// The host looks up the `stream_id`'s live connection and writes the
/// supplied bytes to it. A `stream_id` the plugin does not own (or that
/// has already closed) is silently dropped.
pub const HOST_UNIX_SOCKET_SEND: &str = "host/unix_socket_send";

/// Plugin closes a previously dialled unix socket. Notification.
///
/// The host shuts down the connection and stops emitting
/// `socket:<stream_id>` events. Closure is also implicit when the plugin
/// shuts down, crashes, or is quarantined.
pub const HOST_UNIX_SOCKET_CLOSE: &str = "host/unix_socket_close";

/// Plugin asks the host to read a secret from the platform secret store.
///
/// Capability-gated by the plugin's `secret_store_get` grant. List form is
/// an allow-list of `service` strings (e.g. `["ainb-hangar"]`); a bool-true
/// grant is an unconditional read (allowed, but a danger surface — every
/// `service` becomes readable). On macOS the host reads the generic
/// password from the login Keychain; on linux the host returns
/// `-32005 NOT_IMPLEMENTED` (Secret Service deferred to a later phase) so
/// the plugin can degrade to dotenv. A missing entry returns
/// `-32004 SECRET_NOT_FOUND`. The secret is returned base64-encoded so it
/// never rides the wire as a raw byte array.
pub const HOST_SECRET_STORE_GET: &str = "host/secret_store_get";

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
    HOST_EVENT_STREAM_SUBSCRIBE,
    HOST_EVENT_STREAM_CANCEL,
    HOST_SPAWN_MANAGED_SUBPROCESS,
    HOST_UNIX_SOCKET_DIAL,
    HOST_UNIX_SOCKET_SEND,
    HOST_UNIX_SOCKET_CLOSE,
    HOST_SECRET_STORE_GET,
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
            HOST_EVENT_STREAM_SUBSCRIBE,
            HOST_EVENT_STREAM_CANCEL,
            HOST_SPAWN_MANAGED_SUBPROCESS,
            HOST_UNIX_SOCKET_DIAL,
            HOST_UNIX_SOCKET_SEND,
            HOST_UNIX_SOCKET_CLOSE,
            HOST_SECRET_STORE_GET,
        ] {
            assert!(m.starts_with("host/"), "{m} missing host/ namespace");
        }
    }

    #[test]
    fn all_methods_contains_unix_socket() {
        assert!(
            ALL_METHODS.contains(&HOST_UNIX_SOCKET_DIAL),
            "HOST_UNIX_SOCKET_DIAL missing from ALL_METHODS registry"
        );
        assert!(
            ALL_METHODS.contains(&HOST_UNIX_SOCKET_SEND),
            "HOST_UNIX_SOCKET_SEND missing from ALL_METHODS registry"
        );
        assert!(
            ALL_METHODS.contains(&HOST_UNIX_SOCKET_CLOSE),
            "HOST_UNIX_SOCKET_CLOSE missing from ALL_METHODS registry"
        );
        assert_eq!(HOST_UNIX_SOCKET_DIAL, "host/unix_socket_dial");
        assert_eq!(HOST_UNIX_SOCKET_SEND, "host/unix_socket_send");
        assert_eq!(HOST_UNIX_SOCKET_CLOSE, "host/unix_socket_close");
    }

    #[test]
    fn all_methods_contains_spawn_managed_subprocess() {
        assert!(
            ALL_METHODS.contains(&HOST_SPAWN_MANAGED_SUBPROCESS),
            "HOST_SPAWN_MANAGED_SUBPROCESS missing from ALL_METHODS registry"
        );
        assert_eq!(
            HOST_SPAWN_MANAGED_SUBPROCESS,
            "host/spawn_managed_subprocess"
        );
    }

    #[test]
    fn all_methods_contains_secret_store_get() {
        assert!(
            ALL_METHODS.contains(&HOST_SECRET_STORE_GET),
            "HOST_SECRET_STORE_GET missing from ALL_METHODS registry"
        );
        assert_eq!(HOST_SECRET_STORE_GET, "host/secret_store_get");
    }

    #[test]
    fn all_methods_contains_event_stream() {
        assert!(
            ALL_METHODS.contains(&HOST_EVENT_STREAM_SUBSCRIBE),
            "HOST_EVENT_STREAM_SUBSCRIBE missing from ALL_METHODS registry"
        );
        assert!(
            ALL_METHODS.contains(&HOST_EVENT_STREAM_CANCEL),
            "HOST_EVENT_STREAM_CANCEL missing from ALL_METHODS registry"
        );
        assert_eq!(HOST_EVENT_STREAM_SUBSCRIBE, "host/event_stream_subscribe");
        assert_eq!(HOST_EVENT_STREAM_CANCEL, "host/event_stream_cancel");
    }
}
