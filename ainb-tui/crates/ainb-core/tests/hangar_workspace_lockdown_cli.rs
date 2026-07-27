//! The acceptance proof for the per-instance workspace-creation lockdown:
//! **with the flag set, workspace create is refused (sqlite/config + CLI).**
//!
//! Drives the REAL `ainb` binary against an isolated `$AINB_HANGAR_HOME`, so the
//! whole chain is exercised end to end: `daemon config set` writes the
//! `daemon_config` row → a separate process's `workspace create` reads it back
//! out of sqlite and refuses → `workspace list` proves nothing was written.
//!
//! Decoy discipline: the refusal step asserts the refused slug is ABSENT from a
//! subsequent `list`, not merely that some error text appeared. A "some error
//! present" assertion would stay green if `create` failed for an unrelated reason
//! (bad flag parsing, a locked DB), which is exactly the vacuous-green this test
//! exists to rule out.

use std::path::PathBuf;
use std::process::Command;

fn ainb_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ainb"))
}

/// Run `ainb <args>` against an isolated hangar home. Returns (exit-success,
/// combined stdout+stderr).
fn run(home: &std::path::Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(ainb_bin())
        .args(args)
        .env("AINB_HANGAR_HOME", home)
        .env("HOME", home)
        // The env override is one-way ON, so a value inherited from the developer's
        // shell would silently lock every step down. Clear it so this test is about
        // the sqlite row only.
        .env_remove("HANGAR_DISABLE_WORKSPACE_CREATION")
        .output()
        .expect("spawn ainb");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    (out.status.success(), format!("{stdout}{stderr}"))
}

/// The full operator journey, ordered because each step depends on the previous
/// one's persisted state.
#[test]
fn lockdown_flag_refuses_workspace_create_end_to_end() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();

    // 1. With the knob unset (the shipped default), create works.
    let (ok, out) = run(
        home,
        &[
            "hangar",
            "workspace",
            "create",
            "--slug",
            "acme",
            "--name",
            "Acme",
        ],
    );
    assert!(
        ok,
        "create with the lockdown unset should exit 0; out={out}"
    );
    assert!(
        out.contains("created workspace acme"),
        "missing the create ack:\n{out}"
    );

    // 2. …and it shows up alongside the bootstrapped default.
    let (ok, out) = run(home, &["hangar", "workspace", "list"]);
    assert!(ok, "workspace list should exit 0; out={out}");
    assert!(out.contains("acme"), "created workspace not listed:\n{out}");
    assert!(
        out.contains("default"),
        "bootstrap workspace missing:\n{out}"
    );

    // 3. Engage the lockdown through the ordinary config surface.
    let (ok, out) = run(
        home,
        &[
            "hangar",
            "daemon",
            "config",
            "set",
            "workspace.creation_disabled",
            "true",
        ],
    );
    assert!(ok, "config set should exit 0; out={out}");
    assert!(
        out.contains("set workspace.creation_disabled = true"),
        "missing the set ack:\n{out}"
    );

    // 4. It round-tripped through sqlite (a fresh process reads it back).
    let (ok, out) = run(
        home,
        &[
            "hangar",
            "daemon",
            "config",
            "get",
            "workspace.creation_disabled",
        ],
    );
    assert!(ok, "config get should exit 0; out={out}");
    assert_eq!(
        out.trim(),
        "true",
        "the flag must read back as true from the store"
    );

    // 5. THE ACCEPTANCE: create is now refused, non-zero, with the lockdown
    //    message (not a generic failure).
    let (ok, out) = run(
        home,
        &[
            "hangar",
            "workspace",
            "create",
            "--slug",
            "beta",
            "--name",
            "Beta",
        ],
    );
    assert!(!ok, "create under lockdown must exit non-zero; out={out}");
    assert!(
        out.contains("workspace creation is disabled"),
        "refusal must name the lockdown, got:\n{out}"
    );

    // 6. Nothing was written, and nothing was lost.
    let (ok, out) = run(home, &["hangar", "workspace", "list"]);
    assert!(ok, "workspace list should exit 0; out={out}");
    assert!(
        !out.contains("beta"),
        "the refused workspace must NOT exist:\n{out}"
    );
    assert!(out.contains("acme"), "pre-existing workspace lost:\n{out}");
    assert!(out.contains("default"), "bootstrap workspace lost:\n{out}");

    // 7. Clearing the flag restores create — the gate is the flag, not a break.
    let (ok, out) = run(
        home,
        &[
            "hangar",
            "daemon",
            "config",
            "set",
            "workspace.creation_disabled",
            "false",
        ],
    );
    assert!(ok, "config set false should exit 0; out={out}");

    let (ok, out) = run(
        home,
        &[
            "hangar",
            "workspace",
            "create",
            "--slug",
            "gamma",
            "--name",
            "Gamma",
        ],
    );
    assert!(
        ok,
        "create after clearing the lockdown should exit 0; out={out}"
    );

    let (ok, out) = run(home, &["hangar", "workspace", "list"]);
    assert!(ok, "workspace list should exit 0; out={out}");
    assert!(
        out.contains("gamma"),
        "post-unlock workspace missing:\n{out}"
    );
    assert!(
        !out.contains("beta"),
        "the workspace refused under lockdown must still not exist:\n{out}"
    );
}

/// The knob is registry-driven, so it must appear on `daemon config list`
/// without any per-surface wiring — that is the property that keeps the CLI,
/// the RPC and the TUI Settings pane from drifting apart.
#[test]
fn lockdown_knob_is_listed_by_daemon_config_list() {
    let tmp = tempfile::tempdir().unwrap();
    let (ok, out) = run(tmp.path(), &["hangar", "daemon", "config", "list"]);
    assert!(ok, "config list should exit 0; out={out}");
    assert!(
        out.contains("workspace.creation_disabled"),
        "the lockdown knob must be listed:\n{out}"
    );
}
