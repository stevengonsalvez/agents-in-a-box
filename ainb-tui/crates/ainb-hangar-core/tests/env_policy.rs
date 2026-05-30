//! P5.3 — env allowlist enforcement (pure policy, tests 1-5).
//!
//! These exercise [`ainb_hangar_core::env_policy::apply_policy`] against an
//! **input `HashMap`** — never the process environment — so the suite is
//! parallel-safe (no `std::env::set_var`; per `reference_env_lock_for_parallel_tests`).

use std::collections::HashMap;

use ainb_hangar_core::env_policy::{apply_policy, EnvPolicy, DENY};

/// Build a parent-env map from `(key, value)` pairs.
fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

/// Test 1 — the 12 defaults pass through; nothing else does.
#[test]
fn default_allowlist_passes_through_home_path_term() {
    let policy = EnvPolicy::default();
    let parent = env(&[
        ("HOME", "/home/stevie"),
        ("PATH", "/usr/bin"),
        ("TERM", "xterm-256color"),
        // Not on the allowlist — must be dropped.
        ("MULTICA_SECRET", "leak"),
        ("RUST_LOG", "debug"),
    ]);

    let out = apply_policy(&policy, &parent);

    assert_eq!(out.get("HOME").map(String::as_str), Some("/home/stevie"));
    assert_eq!(out.get("PATH").map(String::as_str), Some("/usr/bin"));
    assert_eq!(out.get("TERM").map(String::as_str), Some("xterm-256color"));
    assert!(
        !out.contains_key("MULTICA_SECRET"),
        "non-allowlisted MULTICA_* must not survive"
    );
    assert!(
        !out.contains_key("RUST_LOG"),
        "non-allowlisted RUST_LOG must not survive"
    );
}

/// Test 2 — a user who adds `LD_PRELOAD` to the allowlist is still denied.
#[test]
fn hardcoded_deny_overrides_user_allow_for_ld_preload() {
    let mut policy = EnvPolicy::default();
    // User explicitly (and dangerously) allowlists the injector.
    policy.allow.insert("LD_PRELOAD".to_string());
    let parent = env(&[("HOME", "/h"), ("LD_PRELOAD", "/tmp/evil.so")]);

    let out = apply_policy(&policy, &parent);

    assert!(
        !out.contains_key("LD_PRELOAD"),
        "hardcoded deny must override a user allow entry"
    );
    assert!(out.contains_key("HOME"), "unrelated allow entry survives");
}

/// Test 3 — the macOS `DYLD_INSERT_LIBRARIES` injector is denied even if allowed.
#[test]
fn hardcoded_deny_overrides_for_dyld_insert_libraries() {
    let mut policy = EnvPolicy::default();
    policy.allow.insert("DYLD_INSERT_LIBRARIES".to_string());
    let parent = env(&[("DYLD_INSERT_LIBRARIES", "/tmp/evil.dylib")]);

    let out = apply_policy(&policy, &parent);

    assert!(!out.contains_key("DYLD_INSERT_LIBRARIES"));
}

/// Test 4 — the full deny family is stripped even when a user allowlists it.
#[test]
fn hardcoded_deny_overrides_for_pythonpath_node_options_bash_env() {
    // Data-driven over the deny family (rstest is not a workspace dep; a plain
    // table gives equivalent parametrised coverage).
    for &denied in DENY {
        let mut policy = EnvPolicy::default();
        policy.allow.insert(denied.to_string());
        let parent = env(&[(denied, "anything"), ("HOME", "/h")]);

        let out = apply_policy(&policy, &parent);

        assert!(
            !out.contains_key(denied),
            "deny family member {denied} must be stripped even when allowlisted"
        );
        assert!(
            out.contains_key("HOME"),
            "stripping {denied} must not affect HOME"
        );
    }
}

/// Test 5 — the `LC_*` glob matches every `LC_`-prefixed key.
#[test]
fn glob_pattern_lc_underscore_star_matches() {
    let policy = EnvPolicy::default();
    let parent = env(&[
        ("LC_ALL", "en_US.UTF-8"),
        ("LC_CTYPE", "en_US.UTF-8"),
        ("LC_MESSAGES", "C"),
        // A key that merely starts with `LC` but not `LC_` must NOT match.
        ("LCD", "nope"),
    ]);

    let out = apply_policy(&policy, &parent);

    assert!(out.contains_key("LC_ALL"), "LC_* must match LC_ALL");
    assert!(out.contains_key("LC_CTYPE"), "LC_* must match LC_CTYPE");
    assert!(out.contains_key("LC_MESSAGES"), "LC_* must match LC_MESSAGES");
    assert!(
        !out.contains_key("LCD"),
        "LC_* glob must require the underscore, not match bare LC prefix"
    );
}
