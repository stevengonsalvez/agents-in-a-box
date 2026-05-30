//! P5.3 tripwire (test 7) — the daemon's task-env builder strips `LD_PRELOAD`.
//!
//! Drives the real dispatch env-build path ([`build_task_env`]) the daemon uses
//! before spawning a provider subprocess and asserts the resulting child env map
//! does NOT carry `LD_PRELOAD`, even when the daemon's ambient environment does
//! and even when a (foot-gun) user allowlist added it.
//!
//! Pure-process + path-explicit: the builder takes an input parent-env map and
//! an injected policy, so this never touches the process environment and is
//! parallel-safe (per `reference_env_lock_for_parallel_tests`). This is cleaner
//! and more deterministic than a tmux UI capture for proving subprocess-env
//! contents.

use std::collections::HashMap;

use ainb_hangar_core::env_policy::EnvPolicy;
use ainb_hangar_daemon::dispatch::build_task_env;

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

#[test]
fn tripwire_env_allowlist_blocks_ld_preload() {
    // The daemon's ambient env carries the injector, and a user has (wrongly)
    // tried to allowlist it.
    let mut policy = EnvPolicy::default();
    policy.allow.insert("LD_PRELOAD".to_string());

    let parent = env(&[
        ("HOME", "/home/stevie"),
        ("PATH", "/usr/bin"),
        ("LD_PRELOAD", "/tmp/evil.dylib"),
    ]);

    // No keychain keys for this leg.
    let keychain: Vec<(String, String)> = Vec::new();

    let child_env = build_task_env(&parent, keychain, &policy);

    assert!(
        !child_env.contains_key("LD_PRELOAD"),
        "spawned-subprocess env must NOT contain LD_PRELOAD; got keys: {:?}",
        child_env.keys().collect::<Vec<_>>()
    );
}
