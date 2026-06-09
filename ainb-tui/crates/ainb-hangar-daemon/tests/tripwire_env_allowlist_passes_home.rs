//! P5.3 tripwire (test 8) — the daemon's task-env builder passes `HOME` through
//! and injects keychain keys.
//!
//! The positive counterpart to `tripwire_env_allowlist_blocks_ld_preload`:
//! drives the real [`build_task_env`] path and asserts the resulting child env
//! DOES carry the allowlisted `HOME`, and that a keychain-injected API key is
//! present even though it is never on the ambient allowlist (keys are injected
//! after the policy pass, not sourced from the parent env).
//!
//! Pure-process + path-explicit; no `std::env::set_var`; parallel-safe.

use std::collections::HashMap;

use ainb_hangar_core::env_policy::EnvPolicy;
use ainb_hangar_daemon::dispatch::build_task_env;

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
}

#[test]
fn tripwire_env_allowlist_passes_home() {
    let policy = EnvPolicy::default();
    let parent = env(&[
        ("HOME", "/home/stevie"),
        ("LD_PRELOAD", "/tmp/evil.dylib"),
        ("RUST_LOG", "debug"),
    ]);

    // A keychain-resident API key injected at exec time. It is NOT on the
    // ambient allowlist, yet must reach the child because it is added after the
    // policy pass.
    let keychain = vec![("ANTHROPIC_API_KEY".to_string(), "sk-test-123".to_string())];

    let child_env = build_task_env(&parent, keychain, &policy);

    assert_eq!(
        child_env.get("HOME").map(String::as_str),
        Some("/home/stevie"),
        "allowlisted HOME must pass through"
    );
    assert_eq!(
        child_env.get("ANTHROPIC_API_KEY").map(String::as_str),
        Some("sk-test-123"),
        "keychain-injected key must be present even though it is not on the ambient allowlist"
    );
    assert!(
        !child_env.contains_key("LD_PRELOAD"),
        "deny family still stripped on the positive leg"
    );
    assert!(
        !child_env.contains_key("RUST_LOG"),
        "non-allowlisted ambient var still dropped"
    );
}
