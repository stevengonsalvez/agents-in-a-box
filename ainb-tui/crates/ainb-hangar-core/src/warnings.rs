//! P5.6 — danger-full-access warning ack keys + pure show/skip decision.
//!
//! ainb agents run with `danger-full-access`.
//!
//! That means full filesystem + network and no container sandbox at v1 — the
//! build-plan "trust-the-user-with-warnings" decision. The user is warned
//! twice:
//!
//! 1. **first run ever** — once, the first time the Hangar TUI launches;
//! 2. **first provider invocation per session per provider** — once per
//!    `(provider, session)` pair: a fresh session re-warns, a second dispatch
//!    in the same session stays quiet.
//!
//! A dismissed warning is recorded as a string key in `warnings_ack` of
//! `~/.agents-in-a-box/hangar/state.toml`. This module owns the **pure** half — the ack-key
//! shape and the show/skip predicates — so both the host (`ainb-plugin-runtime`,
//! which renders the modal + persists the ack) and the daemon (which decides
//! whether a provider invocation warrants a warning) share one source of truth
//! without either depending on the other. The `state.toml` IO lives on each
//! side.
//!
//! | warning                          | ack key                       |
//! |----------------------------------|-------------------------------|
//! | first run ever                   | `first_run`                   |
//! | provider `<p>` in session `<s>`  | `provider:<p>:session:<s>`    |

/// The ack key recorded once the first-run warning is dismissed.
pub const FIRST_RUN_KEY: &str = "first_run";

/// Build the per-session-per-provider ack key: `provider:<name>:session:<sid>`.
///
/// The exact wire form recorded in `warnings_ack` and matched on the next
/// dispatch, so a second invocation of the same provider in the same session is
/// suppressed while a new session (different `sid`) re-warns.
#[must_use]
pub fn provider_session_key(provider: &str, session_id: &str) -> String {
    format!("provider:{provider}:session:{session_id}")
}

/// `true` when the first-run warning has not yet been acknowledged.
///
/// The modal is shown exactly when [`FIRST_RUN_KEY`] is absent from the
/// recorded acks.
#[must_use]
pub fn should_warn_first_run(acks: &[String]) -> bool {
    !acks.iter().any(|a| a == FIRST_RUN_KEY)
}

/// `true` when `provider` has not yet been warned about in `session_id`.
#[must_use]
pub fn should_warn_provider(acks: &[String], provider: &str, session_id: &str) -> bool {
    let key = provider_session_key(provider, session_id);
    !acks.contains(&key)
}

/// `true` when `key` is a per-session ack for `provider`.
///
/// (i.e. it starts with `provider:<name>:session:`). Used by the
/// `--provider <name>` reset to wipe only that provider's session acks, leaving
/// `first_run` and other providers untouched.
#[must_use]
pub fn is_provider_ack(key: &str, provider: &str) -> bool {
    key.starts_with(&format!("provider:{provider}:session:"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_session_key_shape() {
        assert_eq!(
            provider_session_key("claude", "sess-1"),
            "provider:claude:session:sess-1"
        );
    }

    #[test]
    fn first_run_decision() {
        assert!(should_warn_first_run(&[]));
        assert!(!should_warn_first_run(&[FIRST_RUN_KEY.to_string()]));
        assert!(should_warn_first_run(&[
            "provider:claude:session:s".to_string()
        ]));
    }

    #[test]
    fn provider_decision_keyed_by_session() {
        let acks = vec![provider_session_key("claude", "s1")];
        assert!(!should_warn_provider(&acks, "claude", "s1"));
        assert!(should_warn_provider(&acks, "claude", "s2"));
        assert!(should_warn_provider(&acks, "codex", "s1"));
    }

    #[test]
    fn is_provider_ack_matches_only_that_provider() {
        let k = provider_session_key("claude", "s1");
        assert!(is_provider_ack(&k, "claude"));
        assert!(!is_provider_ack(&k, "codex"));
        assert!(!is_provider_ack(FIRST_RUN_KEY, "claude"));
    }
}
