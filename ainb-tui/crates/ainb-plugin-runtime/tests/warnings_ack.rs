//! P5.6 — danger-full-access warning ack persistence (state.toml).
//!
//! Tests 1 + 2 of the P5.6 RED set, driving the host-side ack store directly
//! (path-explicit, no `std::env::set_var`, so parallel-test-safe per
//! `reference_env_lock_for_parallel_tests`):
//!
//! 1. `first_run_warning_persists_ack_to_state_toml` — show once, accept,
//!    `state.toml` grows `warnings_ack = ["first_run"]`.
//! 2. `acked_warning_does_not_re_render` — re-read, the decision is now "skip".
//!
//! Plus foreign-section preservation: the warnings writer must not clobber the
//! workspace switch keys that share `state.toml`.

use ainb_plugin_runtime::warnings::{
    FIRST_RUN_KEY, ack_at, is_provider_ack, provider_session_key, read_acks_at, reset_at,
    should_warn_first_run, should_warn_provider,
};

/// A `state.toml` path inside a fresh tempdir (never the real `~/.ainb`).
fn temp_state() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("hangar").join("state.toml");
    (dir, path)
}

#[test]
fn first_run_warning_persists_ack_to_state_toml() {
    let (_dir, path) = temp_state();

    // First launch: no acks yet → the first-run warning must be shown.
    assert!(
        should_warn_first_run(&read_acks_at(&path).unwrap()),
        "fresh state must warn on first run"
    );

    // User accepts → record the ack.
    let wrote = ack_at(&path, FIRST_RUN_KEY).unwrap();
    assert!(wrote, "first ack must write");

    // state.toml now carries warnings_ack = ["first_run"].
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(
        raw.contains("warnings_ack"),
        "state.toml missing warnings_ack:\n{raw}"
    );
    assert!(
        raw.contains("first_run"),
        "state.toml missing first_run ack:\n{raw}"
    );
    assert_eq!(
        read_acks_at(&path).unwrap(),
        vec![FIRST_RUN_KEY.to_string()]
    );
}

#[test]
fn acked_warning_does_not_re_render() {
    let (_dir, path) = temp_state();
    ack_at(&path, FIRST_RUN_KEY).unwrap();

    // Re-launch: the recorded ack suppresses the warning.
    let acks = read_acks_at(&path).unwrap();
    assert!(
        !should_warn_first_run(&acks),
        "an acked first-run warning must not re-render"
    );

    // Re-acking is idempotent (no duplicate, no second write).
    let wrote_again = ack_at(&path, FIRST_RUN_KEY).unwrap();
    assert!(!wrote_again, "re-acking must be a no-op");
    assert_eq!(read_acks_at(&path).unwrap().len(), 1);
}

#[test]
fn warnings_writer_preserves_foreign_state_sections() {
    let (_dir, path) = temp_state();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    // Seed the file with the workspace switch keys + a foreign plugin section,
    // exactly as the workspace store + another plugin would leave them.
    std::fs::write(
        &path,
        "active_workspace = \"01ID_ACME\"\ndefault_workspace = \"01ID_DEFAULT\"\n\n[other_plugin]\nfoo = \"bar\"\n",
    )
    .unwrap();

    ack_at(&path, FIRST_RUN_KEY).unwrap();

    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(
        raw.contains("active_workspace"),
        "lost active_workspace:\n{raw}"
    );
    assert!(raw.contains("01ID_ACME"), "lost active id:\n{raw}");
    assert!(
        raw.contains("default_workspace"),
        "lost default_workspace:\n{raw}"
    );
    assert!(
        raw.contains("[other_plugin]"),
        "lost foreign section:\n{raw}"
    );
    assert!(
        raw.contains("warnings_ack"),
        "did not add warnings_ack:\n{raw}"
    );
}

#[test]
fn reset_provider_wipes_only_that_provider_session_acks() {
    let (_dir, path) = temp_state();
    ack_at(&path, FIRST_RUN_KEY).unwrap();
    ack_at(&path, &provider_session_key("claude", "s1")).unwrap();
    ack_at(&path, &provider_session_key("claude", "s2")).unwrap();
    ack_at(&path, &provider_session_key("codex", "s1")).unwrap();

    let removed = reset_at(&path, |k| is_provider_ack(k, "claude")).unwrap();
    assert_eq!(removed, 2, "must remove both claude session acks");

    let acks = read_acks_at(&path).unwrap();
    // first_run + codex survive; both claude acks gone → next claude dispatch re-warns.
    assert!(acks.iter().any(|a| a == FIRST_RUN_KEY));
    assert!(acks.contains(&provider_session_key("codex", "s1")));
    assert!(should_warn_provider(&acks, "claude", "s1"));
    assert!(should_warn_provider(&acks, "claude", "s2"));
    assert!(!should_warn_provider(&acks, "codex", "s1"));
}
