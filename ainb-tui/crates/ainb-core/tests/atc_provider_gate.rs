//! End-to-end tests for the ATC provider gate, as an operator exercises it.
//!
//! Drives the real `ainb` binary against a tempdir `AINB_HOME`, with
//! `--no-spawn --no-heartbeat` so nothing touches launchd/systemd, tmux, or a
//! provider CLI.
//!
//! This file used to test the SUPERVISOR MODE: `lite` versus `full`, the
//! one-controller-per-fleet rule, and the exclusivity gate that kept the lite
//! scanner and the LLM heartbeat off each other's panes. Lite mode is gone, so
//! there is one controller and nothing to exclude, and what lite did is now the
//! hangar daemon's own retry sweep (`ainb fleet atc retries` reads its ledger).
//!
//! What survives is the half that was never about modes: ainb refuses to
//! provision an instance on a provider it cannot actually drive, and it writes
//! the policy file each provider really reads. Both are still load-bearing —
//! an instance that boots, ignores its playbook and is never woken looks
//! perfectly healthy on every surface.

use std::path::PathBuf;
use std::process::{Command, Output};

fn ainb_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ainb"))
}

/// A home per test, so the tests are independent and can run in parallel.
fn home_for(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("atc-mode-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// A per-test instance NAME, unique to this process.
///
/// `AINB_HOME` isolates the instance dir but NOT the OS scheduler: launchd
/// plists live in `~/Library/LaunchAgents/<label>.plist`, and the daemon's
/// `atc_instance` rows are keyed by name in the real store. A test that used a
/// plausible name like `tower` and then ran a verb which reconciles schedulers
/// would unload a developer's actual ATC timer. A pid-derived name collides
/// with nothing.
fn instance(tag: &str) -> String {
    format!("t{}-{}", std::process::id(), tag)
}

fn run(home: &std::path::Path, args: &[&str]) -> Output {
    Command::new(ainb_bin())
        .env("AINB_HOME", home)
        .env("AINB_BIN", ainb_bin())
        .args(args)
        .output()
        .expect("invoke ainb")
}

fn json(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "not JSON ({e}):\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// Provision an instance without any OS side effects.
fn setup(home: &std::path::Path, name: &str, extra: &[&str]) -> Output {
    let mut args = vec![
        "--format",
        "json",
        "fleet",
        "atc",
        "setup",
        name,
        "--no-spawn",
        "--no-heartbeat",
        "--no-hooks",
    ];
    args.extend_from_slice(extra);
    run(home, &args)
}

fn meta(home: &std::path::Path, name: &str) -> serde_json::Value {
    let raw = std::fs::read_to_string(home.join("atc").join(name).join("meta.json"))
        .expect("meta.json must exist");
    serde_json::from_str(&raw).expect("meta.json must parse")
}

// ── Persistence ─────────────────────────────────────────────────────────────

#[test]
fn codex_is_a_real_full_mode_brain_and_gets_the_file_it_reads() {
    let home = home_for("codex");
    let name = instance("codex");
    let out = setup(&home, &name, &["--provider", "codex"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(meta(&home, &name)["provider"], "codex");

    let dir = home.join("atc").join(&name);
    // Codex reads AGENTS.md. A CLAUDE.md-only instance would boot and then
    // ignore its entire playbook, while looking perfectly provisioned.
    assert!(dir.join("AGENTS.md").is_file(), "codex policy not written");
    let policy = std::fs::read_to_string(dir.join("AGENTS.md")).unwrap();
    assert!(policy.contains(&format!("# ATC — Air Traffic Control ({name})")));
    // CLAUDE.md stays too, so switching providers never leaves the instance
    // without the file the previous brain read.
    assert!(dir.join("CLAUDE.md").is_file());
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_provider_ainb_cannot_drive_is_refused_not_faked() {
    let home = home_for("unsupported");
    let name = instance("unsupported");
    for provider in ["copilot", "antigravity", "gemini"] {
        let out = setup(&home, &name, &["--provider", provider]);
        assert!(
            !out.status.success(),
            "{provider} must be refused for full mode, not provisioned"
        );
        let err = stderr(&out);
        assert!(err.contains(provider), "the refusal names it: {err}");
        assert!(
            err.contains("claude") && err.contains("codex"),
            "the refusal lists what does work: {err}"
        );
        // A refusal must leave nothing behind — a half-provisioned instance
        // whose brain can never be woken is worse than none.
        assert!(
            !home.join("atc").join(&name).join("meta.json").exists(),
            "{provider} refusal left a meta.json behind"
        );
    }
    let _ = std::fs::remove_dir_all(&home);
}
