//! End-to-end tests for the ATC supervisor mode — the one-controller-per-fleet
//! rule, as an operator actually exercises it.
//!
//! Drives the real `ainb` binary against a tempdir `AINB_HOME`, with
//! `--no-spawn --no-heartbeat` so nothing touches launchd/systemd, tmux, or a
//! provider CLI. The lite scanner is always driven `--once --dry-run`: a real
//! scan reads the HOST's live sessions, and a test that sends `continue` into
//! whatever the developer happens to be running is not a test.
//!
//! What is asserted is deliberately the OBSERVABLE contract, not the internals
//! the unit tests already cover:
//!
//! * exclusivity — the losing controller refuses to act, from the CLI, after a
//!   switch it did not participate in
//! * persistence — the mode survives the process, and a re-`setup` (which is
//!   what `daemon atc start` runs) does not silently reset it
//! * toggle — switching reports the transition and is idempotent
//! * provider capability gating — Claude and Codex are accepted, a provider
//!   ainb cannot drive is REFUSED rather than faked, and the refusal leaves the
//!   instance untouched

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
fn a_fresh_instance_is_a_full_claude_atc() {
    // The compatibility contract: nothing about the default changed, so an
    // upgrade cannot quietly downgrade a fleet to the no-LLM scanner.
    let home = home_for("default");
    let out = setup(&home, "tower", &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    let m = meta(&home, "tower");
    assert_eq!(m["mode"], "full");
    assert_eq!(m["provider"], "claude");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_mode_survives_the_process_that_set_it() {
    let home = home_for("persist");
    assert!(setup(&home, "tower", &[]).status.success());

    let out = run(
        &home,
        &[
            "--format", "json", "fleet", "atc", "mode", "tower", "--set", "lite",
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));

    // On disk, and readable by a NEW process — the mode is not in-memory state.
    assert_eq!(meta(&home, "tower")["mode"], "lite");
    let read_back = run(
        &home,
        &["--format", "json", "fleet", "atc", "mode", "tower"],
    );
    assert!(read_back.status.success(), "{}", stderr(&read_back));
    assert_eq!(json(&read_back)["mode"], "lite");
    assert_eq!(json(&read_back)["owner"], "lite scanner");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn re_running_setup_does_not_reset_the_mode() {
    // `ainb daemon atc start` re-runs `setup` to respawn a dead session. If that
    // rebuilt meta from defaults it would flip a deliberately-lite fleet back to
    // a token-spending brain, which is the bug this test exists to hold shut.
    let home = home_for("resetup");
    assert!(setup(&home, "tower", &["--mode", "lite"]).status.success());
    assert_eq!(meta(&home, "tower")["mode"], "lite");

    let again = setup(&home, "tower", &["--interval", "9"]);
    assert!(again.status.success(), "{}", stderr(&again));
    let m = meta(&home, "tower");
    assert_eq!(
        m["mode"], "lite",
        "a bare re-setup must not change the mode"
    );
    assert_eq!(m["heartbeat_interval_min"], 9, "explicit knobs still apply");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn an_instance_written_before_modes_existed_reads_as_full_claude() {
    // Simulates an on-disk instance from an older ainb: no `mode`, no
    // `provider`. It must keep behaving as the full LLM ATC it was.
    let home = home_for("legacy");
    let dir = home.join("atc").join("tower");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("meta.json"),
        r#"{"name":"tower","heartbeat_enabled":true,"heartbeat_interval_min":15,"idle_pause_min":60}"#,
    )
    .unwrap();

    let out = run(
        &home,
        &["--format", "json", "fleet", "atc", "mode", "tower"],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let v = json(&out);
    assert_eq!(v["mode"], "full");
    assert_eq!(v["provider"], "claude");
    let _ = std::fs::remove_dir_all(&home);
}

// ── Exclusivity ─────────────────────────────────────────────────────────────

#[test]
fn the_full_heartbeat_stands_down_on_a_lite_fleet() {
    // THE exclusivity property, driven the way it actually happens: a scheduler
    // outlives a switch and fires anyway. Both the local timer and the daemon
    // cron reach the full controller through this verb, so a refusal here is a
    // refusal on every full-mode action path.
    let home = home_for("standdown");
    assert!(setup(&home, "tower", &["--mode", "lite"]).status.success());

    let beat = run(
        &home,
        &["--format", "json", "fleet", "atc", "heartbeat", "tower"],
    );
    assert!(
        beat.status.success(),
        "a stood-down beat must exit 0, not spam a scheduler with failures: {}",
        stderr(&beat)
    );
    let v = json(&beat);
    assert_eq!(v["stood_down"], true);
    assert_eq!(v["delivered"], false, "nothing may be sent");
    assert_eq!(v["owner"], "lite scanner", "the refusal names the owner");
    // The daemon's ledger gates are fail-closed on exactly these two fields; a
    // stood-down beat must not read as "the whole fleet recovered".
    assert_eq!(v["roster_valid"], false);
    assert_eq!(v["ledger_owner"], "none");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_lite_scanner_refuses_to_start_on_a_full_fleet() {
    // The mirror image: the other controller, refused from its own entry point.
    let home = home_for("literefuse");
    assert!(setup(&home, "tower", &[]).status.success());

    let out = run(
        &home,
        &["fleet", "atc", "supervise", "tower", "--once", "--dry-run"],
    );
    assert!(
        !out.status.success(),
        "the lite scanner must refuse to run on a full fleet"
    );
    let err = stderr(&out);
    assert!(err.contains("full heartbeat"), "names the owner: {err}");
    assert!(err.contains("--set lite"), "names the fix: {err}");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn exactly_one_controller_is_permitted_in_either_mode() {
    // Drive both controllers in both modes and count who is allowed. The count
    // must be one, always — never two (concurrent sends) and never zero (an
    // unowned fleet nothing is watching).
    let home = home_for("exclusive");
    assert!(setup(&home, "tower", &[]).status.success());

    for mode in ["full", "lite"] {
        assert!(
            run(
                &home,
                &[
                    "--format", "json", "fleet", "atc", "mode", "tower", "--set", mode
                ],
            )
            .status
            .success()
        );

        let beat = run(
            &home,
            &["--format", "json", "fleet", "atc", "heartbeat", "tower"],
        );
        let full_permitted = !json(&beat)["stood_down"].as_bool().unwrap_or(false);

        let scan = run(
            &home,
            &["fleet", "atc", "supervise", "tower", "--once", "--dry-run"],
        );
        let lite_permitted = scan.status.success();

        assert_ne!(
            full_permitted, lite_permitted,
            "{mode} mode permitted full={full_permitted} lite={lite_permitted}; \
exactly one controller must be allowed"
        );
        assert_eq!(
            full_permitted,
            mode == "full",
            "{mode} mode must be owned by its own controller"
        );
    }
    let _ = std::fs::remove_dir_all(&home);
}

// ── Toggle behaviour ────────────────────────────────────────────────────────

#[test]
fn switching_reports_the_transition_and_is_idempotent() {
    let home = home_for("toggle");
    assert!(setup(&home, "tower", &[]).status.success());

    let first = run(
        &home,
        &[
            "--format", "json", "fleet", "atc", "mode", "tower", "--set", "lite",
        ],
    );
    assert!(first.status.success(), "{}", stderr(&first));
    let v = json(&first);
    assert_eq!(v["previous_mode"], "full");
    assert_eq!(v["mode"], "lite");
    assert_eq!(v["changed"], true);

    // Setting the same mode again is a no-op that says so, rather than tearing
    // down and restarting the controller that is already correct.
    let again = run(
        &home,
        &[
            "--format", "json", "fleet", "atc", "mode", "tower", "--set", "lite",
        ],
    );
    assert!(again.status.success(), "{}", stderr(&again));
    assert_eq!(json(&again)["changed"], false);

    // And back.
    let back = run(
        &home,
        &[
            "--format", "json", "fleet", "atc", "mode", "tower", "--set", "full",
        ],
    );
    assert!(back.status.success(), "{}", stderr(&back));
    assert_eq!(json(&back)["mode"], "full");
    assert_eq!(meta(&home, "tower")["mode"], "full");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn reading_the_mode_never_changes_it() {
    // Switching the thing that drives a whole fleet must take an explicit
    // --set; looking at it must not be enough.
    let home = home_for("readonly");
    assert!(setup(&home, "tower", &["--mode", "lite"]).status.success());
    let before = meta(&home, "tower");

    let out = run(
        &home,
        &["--format", "json", "fleet", "atc", "mode", "tower"],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(meta(&home, "tower"), before, "a read must not mutate");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_mode_report_explains_both_modes_and_names_the_owner() {
    let home = home_for("help");
    assert!(setup(&home, "tower", &[]).status.success());
    let out = run(&home, &["fleet", "atc", "mode", "tower"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("full heartbeat"), "current owner: {text}");
    assert!(text.contains("no LLM"), "lite behaviour: {text}");
    assert!(text.contains("never answers an ASK"), "lite limits: {text}");
    assert!(text.contains("spends tokens"), "full limits: {text}");
    assert!(text.contains("--set lite"), "how to switch: {text}");
    let _ = std::fs::remove_dir_all(&home);
}

// ── Provider capability gating ──────────────────────────────────────────────

#[test]
fn codex_is_a_real_full_mode_brain_and_gets_the_file_it_reads() {
    let home = home_for("codex");
    let out = setup(&home, "tower", &["--provider", "codex"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(meta(&home, "tower")["provider"], "codex");

    let dir = home.join("atc").join("tower");
    // Codex reads AGENTS.md. A CLAUDE.md-only instance would boot and then
    // ignore its entire playbook, while looking perfectly provisioned.
    assert!(dir.join("AGENTS.md").is_file(), "codex policy not written");
    let policy = std::fs::read_to_string(dir.join("AGENTS.md")).unwrap();
    assert!(policy.contains("# ATC — Air Traffic Control (tower)"));
    // CLAUDE.md stays too, so switching providers never leaves the instance
    // without the file the previous brain read.
    assert!(dir.join("CLAUDE.md").is_file());
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_provider_ainb_cannot_drive_is_refused_not_faked() {
    let home = home_for("unsupported");
    for provider in ["copilot", "antigravity", "gemini"] {
        let out = setup(&home, "tower", &["--provider", provider]);
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
            !home.join("atc").join("tower").join("meta.json").exists(),
            "{provider} refusal left a meta.json behind"
        );
    }
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn an_unsupported_provider_cannot_be_switched_into_full_mode() {
    // The other door into the same state: provision fine, then try to switch.
    let home = home_for("switchgate");
    assert!(setup(&home, "tower", &[]).status.success());

    let out = run(
        &home,
        &[
            "fleet",
            "atc",
            "mode",
            "tower",
            "--set",
            "full",
            "--provider",
            "copilot",
        ],
    );
    assert!(!out.status.success(), "must be refused");
    assert!(stderr(&out).contains("copilot"), "{}", stderr(&out));
    // Refused BEFORE the write: the instance keeps the provider it had.
    assert_eq!(meta(&home, "tower")["provider"], "claude");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn lite_mode_is_available_regardless_of_provider_support() {
    // Lite runs no brain, so a provider ainb cannot drive is irrelevant to it.
    // What lite must NOT do is accept a provider that could never serve a later
    // switch back to full — that would defer the refusal to the worst moment.
    let home = home_for("literovider");
    assert!(setup(&home, "tower", &["--mode", "lite"]).status.success());
    assert_eq!(meta(&home, "tower")["mode"], "lite");

    let ok = run(
        &home,
        &[
            "--format",
            "json",
            "fleet",
            "atc",
            "mode",
            "tower",
            "--set",
            "lite",
            "--provider",
            "codex",
        ],
    );
    assert!(ok.status.success(), "{}", stderr(&ok));
    assert_eq!(meta(&home, "tower")["provider"], "codex");

    let bad = run(
        &home,
        &[
            "fleet",
            "atc",
            "mode",
            "tower",
            "--set",
            "lite",
            "--provider",
            "copilot",
        ],
    );
    assert!(!bad.status.success(), "must not bank an unusable provider");
    assert_eq!(meta(&home, "tower")["provider"], "codex");
    let _ = std::fs::remove_dir_all(&home);
}

// ── Lite provisioning ───────────────────────────────────────────────────────

#[test]
fn a_lite_instance_provisions_a_policy_but_schedules_no_heartbeat() {
    let home = home_for("liteprov");
    let out = setup(&home, "tower", &["--mode", "lite"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let v = json(&out);
    assert_eq!(v["mode"], "lite");
    assert_eq!(v["owner"], "lite scanner");
    assert_eq!(
        v["session_spawned"], false,
        "lite mode must not spawn a brain that is never nudged"
    );
    assert_eq!(v["daemon_registered"], false, "no full-mode cron in lite");
    assert_eq!(v["timer_units"].as_array().map(Vec::len), Some(0));
    let _ = std::fs::remove_dir_all(&home);
}
