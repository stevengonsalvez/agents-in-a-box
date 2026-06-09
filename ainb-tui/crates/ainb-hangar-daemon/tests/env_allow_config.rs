//! P5.3 — env allowlist TOML loader (test 6).
//!
//! Path-explicit (operates on a tempfile), so the suite is parallel-safe: no
//! reliance on `$HOME` resolution or the process environment.

use std::collections::BTreeSet;

use ainb_hangar_daemon::dispatch::{load_allow_at, save_allow_at};

/// Test 6 — `add` writes atomically, round-trips, and preserves a pre-existing
/// `[other_plugin]` foreign section.
#[test]
fn cli_add_writes_atomically_and_roundtrips_toml() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("env.allow.toml");

    // Seed a file that already carries a foreign section owned by another tool,
    // plus a partial `[env]` allow list.
    std::fs::write(
        &path,
        "[other_plugin]\nkeep = \"me\"\nnested = { a = 1, b = 2 }\n\n[env]\nallow = [\"FOO_BAR\"]\n",
    )
    .expect("seed file");

    // Load, add a key, save (the CLI `add` path).
    let mut cfg = load_allow_at(&path).expect("load");
    assert!(cfg.allow.contains("FOO_BAR"), "existing allow entry loaded");
    cfg.allow.insert("BAZ_QUX".to_string());
    save_allow_at(&path, &cfg).expect("save");

    // Round-trip: the new key is present.
    let reloaded = load_allow_at(&path).expect("reload");
    let mut expected: BTreeSet<String> = BTreeSet::new();
    expected.insert("FOO_BAR".to_string());
    expected.insert("BAZ_QUX".to_string());
    assert_eq!(
        reloaded.allow, expected,
        "allow set round-trips with the add"
    );

    // Foreign-section preservation: the raw document still carries
    // `[other_plugin]` with its original contents.
    let raw = std::fs::read_to_string(&path).expect("read raw");
    let doc: toml::Value = toml::from_str(&raw).expect("parse raw");
    let other = doc.get("other_plugin").expect("[other_plugin] section survived the save");
    assert_eq!(
        other.get("keep").and_then(toml::Value::as_str),
        Some("me"),
        "foreign scalar preserved"
    );
    assert_eq!(
        other.get("nested").and_then(|n| n.get("a")).and_then(toml::Value::as_integer),
        Some(1),
        "foreign nested table preserved"
    );
}

/// A missing file loads as the operator-default policy (the 12 built-ins), not
/// an error — a first run has no config yet.
#[test]
fn missing_file_loads_default_allowlist() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("does-not-exist.toml");

    let cfg = load_allow_at(&path).expect("load missing -> default");

    assert!(cfg.allow.contains("HOME"), "default includes HOME");
    assert!(cfg.allow.contains("LC_*"), "default includes LC_* glob");
    assert_eq!(cfg.allow.len(), 12, "exactly the 12 built-in defaults");
}

/// `EnvAllowConfig::into_policy` carries the user allow set into an `EnvPolicy`
/// whose deny family is always the hardcoded one.
#[test]
fn into_policy_carries_allow_and_hardcoded_deny() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("env.allow.toml");
    std::fs::write(&path, "[env]\nallow = [\"HOME\", \"CUSTOM_VAR\"]\n").expect("seed");

    let cfg = load_allow_at(&path).expect("load");
    let policy = cfg.into_policy();

    assert!(policy.is_allowed("HOME"));
    assert!(policy.is_allowed("CUSTOM_VAR"));
    assert!(
        policy.is_denied("LD_PRELOAD"),
        "deny family is always the hardcoded set"
    );
}
