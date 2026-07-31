// ABOUTME: Fleet-core module root — the extracted lower layers of the fleet
// stack (types, enrich cache, read/classify, send, discover).
//
// The module is deliberately named `fleet` so the files moved out of
// `ainb-core` keep their original `crate::fleet::…` self-references and needed
// no path rewrites. `ainb-core::fleet` re-exports these back so the TUI/CLI is
// unchanged; the hangar daemon consumes them directly.

#![allow(missing_docs)]

pub mod discover;
pub mod enrich_cache;
pub mod provider;
pub mod read;
pub mod repo_clone;
pub mod repo_roster;
pub mod send;
pub mod session_registry;
pub mod types;

pub use types::{
    AinbSession, AttentionState, Block, BrokerPeer, Capabilities, Capability, Confidence,
    FleetSession, LifecycleState, Liveness, ManagementState, Provenance, Provider, SendOutcome,
    Session, SessionKey, SessionSource, SessionState, Signal, TransportHealth,
};

/// The tmux binary this process drives, honouring `AINB_TMUX_BIN`.
///
/// Launch, discovery, liveness validation and send MUST resolve tmux the same
/// way. The managed-Codex launch path already reads `AINB_TMUX_BIN`; when the
/// fleet paths hardcoded `tmux` instead, a custom binary meant sessions were
/// launched against one tmux server and then looked up against another — so
/// they were created successfully and were immediately invisible to discovery
/// and unreachable by send.
#[must_use]
pub fn tmux_bin() -> std::ffi::OsString {
    resolve_tmux_bin(std::env::var_os("AINB_TMUX_BIN"))
}

/// Env-free core of [`tmux_bin`], so the contract is testable without mutating
/// process-global state (this crate forbids `unsafe`, so tests cannot call
/// `set_var`, and env-mutating tests race under a parallel runner anyway).
///
/// An EMPTY override falls back rather than spawning `""`. `var_os` returns
/// `Some("")` for `AINB_TMUX_BIN=`, which an `unwrap_or_else` alone would
/// happily pass to `Command::new`, turning an unset-looking variable into a
/// spawn failure on every tmux call.
fn resolve_tmux_bin(configured: Option<std::ffi::OsString>) -> std::ffi::OsString {
    configured.filter(|value| !value.is_empty()).unwrap_or_else(|| "tmux".into())
}

#[cfg(test)]
mod tmux_bin_tests {
    use super::resolve_tmux_bin;

    #[test]
    fn unset_falls_back_to_path_tmux() {
        assert_eq!(resolve_tmux_bin(None), "tmux");
    }

    #[test]
    fn a_configured_binary_wins() {
        assert_eq!(
            resolve_tmux_bin(Some("/opt/custom/bin/tmux".into())),
            "/opt/custom/bin/tmux"
        );
    }

    #[test]
    fn an_empty_override_falls_back_instead_of_spawning_nothing() {
        assert_eq!(resolve_tmux_bin(Some(String::new().into())), "tmux");
    }
}
