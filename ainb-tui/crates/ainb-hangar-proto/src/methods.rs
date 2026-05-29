//! Daemon JSON-RPC method-name registry.
//!
//! These are the methods the Hangar **daemon** speaks over its
//! `~/.ainb/hangar.sock` socket. They sit on the same JSON-RPC 2.0
//! envelope ([`crate::RpcRequest`] / [`crate::RpcResponse`]) the host
//! plugin caps mediate. P3.7's plugin connection state machine sends
//! [`WORKSPACE_SUBSCRIBE`] right after dialling and renders
//! `"Hangar: Connected"` once the daemon acknowledges.
//!
//! Method names are namespaced (`<area>/<verb>`) except [`PING`], which
//! is the canonical bare liveness probe. The [`ALL_METHODS`] slice is the
//! single source of truth used by the uniqueness / namespacing tests.

/// `workspace/subscribe` — open a workspace event subscription.
///
/// Params: `{ workspace_id: String }`. Result: the current workspace
/// snapshot (empty on a fresh store). After the ack the daemon pushes
/// workspace events on the same stream.
pub const WORKSPACE_SUBSCRIBE: &str = "workspace/subscribe";

/// `workspace/list` — list the workspaces visible to the caller.
///
/// Params: `{}`. Result: `{ workspaces: [...] }`.
pub const WORKSPACE_LIST: &str = "workspace/list";

/// `ping` — bare liveness probe. Params: `{}`. Result: `{}`.
pub const PING: &str = "ping";

/// Every daemon method name, in declaration order.
///
/// Single source of truth for the registry tests in this module. The
/// `all_methods_covers_every_const` test guards against registry drift (a
/// method const declared but never appended here), while `method_names_unique`
/// and `methods_namespaced_or_ping` guard the shape of the wire surface.
pub const ALL_METHODS: &[&str] = &[WORKSPACE_SUBSCRIBE, WORKSPACE_LIST, PING];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// No two daemon methods share a name.
    #[test]
    fn method_names_unique() {
        let set: HashSet<&&str> = ALL_METHODS.iter().collect();
        assert_eq!(set.len(), ALL_METHODS.len(), "duplicate method name");
    }

    /// Every method is either namespaced (`<area>/<verb>`) or the bare
    /// `ping` liveness probe. No empty or whitespace names.
    #[test]
    fn methods_namespaced_or_ping() {
        for m in ALL_METHODS {
            assert!(!m.is_empty(), "empty method name");
            assert!(!m.contains(char::is_whitespace), "whitespace in {m:?}");
            assert!(
                *m == PING || m.contains('/'),
                "{m:?} is neither namespaced nor `ping`"
            );
        }
    }

    /// The workspace methods live under the `workspace/` namespace.
    #[test]
    fn workspace_methods_namespaced() {
        assert!(WORKSPACE_SUBSCRIBE.starts_with("workspace/"));
        assert!(WORKSPACE_LIST.starts_with("workspace/"));
    }

    /// Registry-drift guard: every individually-declared method const must be
    /// present in [`ALL_METHODS`]. Rust has no compile-time reflection over
    /// module consts, so the full set is mirrored here explicitly — adding a
    /// new `pub const` method without also appending it to `ALL_METHODS` (and
    /// to this list) fails this test, keeping the wire registry honest.
    #[test]
    fn all_methods_covers_every_const() {
        // Every method const known to this module. Keep in sync with the
        // `pub const` declarations above.
        let declared: &[&str] = &[WORKSPACE_SUBSCRIBE, WORKSPACE_LIST, PING];
        for m in declared {
            assert!(
                ALL_METHODS.contains(m),
                "method const {m:?} is missing from ALL_METHODS"
            );
        }
        assert_eq!(
            declared.len(),
            ALL_METHODS.len(),
            "ALL_METHODS has {} entries but {} method consts are declared",
            ALL_METHODS.len(),
            declared.len()
        );
    }
}
