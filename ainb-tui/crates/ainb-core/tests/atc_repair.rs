//! End-to-end tests for `ainb fleet atc repair`.
//!
//! Separate from `atc_cli_lifecycle.rs` because these tests must actually
//! exercise unit installation, which means owning three env vars that the
//! lifecycle harness does not:
//!
//! * `HOME`: unit paths come from `dirs::home_dir()`, NOT `AINB_HOME`, so
//!   without this the suite would write into the developer's real
//!   `~/Library/LaunchAgents` (or `~/.config/systemd/user`).
//! * `AINB_HANGAR_HOME`: points daemon discovery at a dir with no token file
//!   so `DaemonClient::from_env` fails deterministically and repair takes the
//!   local-timer branch on every machine.
//! * `AINB_TIMER_SKIP_ACTIVATION`: never `launchctl load` / `systemctl enable`
//!   a real, valid, firing job into the developer's session.
//!
//! Setups use `--no-spawn --no-hooks` but deliberately NOT `--no-heartbeat`
//! (except where a disabled heartbeat is the thing under test), because repair
//! has nothing to look at without an enabled one.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn ainb_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ainb"))
}

fn ainb_bin_str() -> String {
    ainb_bin().display().to_string()
}

/// A home dir unique per test: the PID suffix alone is shared across every test
/// in this binary, and they run in parallel.
fn home_for(test: &str) -> PathBuf {
    let home = std::env::temp_dir().join(format!("atc-repair-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).expect("create test home");
    home
}

fn run(home: &Path, bin: &str, path: &str, args: &[&str]) -> Output {
    Command::new(ainb_bin())
        .env("AINB_HOME", home)
        // Unit paths resolve from dirs::home_dir(), not AINB_HOME.
        .env("HOME", home)
        // No daemon token here, so repair takes the local-timer branch.
        .env("AINB_HANGAR_HOME", home)
        // Never touch the real launchd/systemd.
        .env("AINB_TIMER_SKIP_ACTIVATION", "1")
        .env("AINB_BIN", bin)
        .env("PATH", path)
        .args(args)
        .output()
        .expect("invoke ainb")
}

/// The default PATH for a run whose binary is pinned absolutely by `AINB_BIN`.
fn real_path() -> String {
    "/usr/bin:/bin".to_string()
}

/// Every unit file `install` writes, in the order `timer::unit_paths` returns.
#[cfg(target_os = "macos")]
fn unit_files(home: &Path, name: &str) -> Vec<PathBuf> {
    vec![
        home.join("Library")
            .join("LaunchAgents")
            .join(format!("com.agentsinabox.atc.{name}.plist")),
    ]
}

#[cfg(not(target_os = "macos"))]
fn unit_files(home: &Path, name: &str) -> Vec<PathBuf> {
    let dir = home.join(".config").join("systemd").join("user");
    vec![
        dir.join(format!("com.agentsinabox.atc.{name}.service")),
        dir.join(format!("com.agentsinabox.atc.{name}.timer")),
    ]
}

/// The unit `installed_program_health` reads: the plist on macOS, the `.service`
/// (not the `.timer`) on Linux.
fn health_unit(home: &Path, name: &str) -> PathBuf {
    unit_files(home, name).into_iter().next().expect("at least one unit path")
}

fn json_of(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout was not JSON ({e}): {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

fn setup(home: &Path, bin: &str, name: &str, extra: &[&str]) -> Output {
    let mut args: Vec<&str> = vec!["fleet", "atc", "setup", name, "--no-spawn", "--no-hooks"];
    args.extend_from_slice(extra);
    run(home, bin, &real_path(), &args)
}

// --- T1 ---------------------------------------------------------------------

#[test]
fn repair_rejects_unknown_instance() {
    let home = home_for("unknown");

    let out = run(
        &home,
        &ainb_bin_str(),
        &real_path(),
        &["--format", "json", "fleet", "atc", "repair", "ghost"],
    );

    assert!(
        !out.status.success(),
        "repair on a missing instance must not exit 0"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no ATC instance named 'ghost'"),
        "unexpected stderr: {stderr}"
    );
    assert!(
        out.stdout.is_empty(),
        "a failed repair must emit no JSON: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        unit_files(&home, "ghost").iter().all(|p| !p.exists()),
        "repair installed a unit for a non-existent instance"
    );

    let _ = std::fs::remove_dir_all(&home);
}

// --- T2 ---------------------------------------------------------------------

#[test]
fn repair_installs_a_missing_timer() {
    let home = home_for("missing-timer");
    let bin = ainb_bin_str();

    let out = setup(&home, &bin, "tower", &[]);
    assert!(
        out.status.success(),
        "setup failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    for unit in unit_files(&home, "tower") {
        assert!(unit.is_file(), "setup did not install {}", unit.display());
        std::fs::remove_file(&unit).expect("delete unit");
    }

    let out = run(
        &home,
        &bin,
        &real_path(),
        &["--format", "json", "fleet", "atc", "repair", "tower"],
    );
    assert!(
        out.status.success(),
        "repair failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = json_of(&out);
    assert_eq!(v["result"], "repaired");
    assert_eq!(v["changed"], true, "a fresh install is a change: {v}");
    // `program_after` is the load-bearing field: it is only populated on the
    // Resolves arm, so its presence proves the post-install health check ran
    // and passed. (The old assertion here checked `problem_after` was null,
    // which was true by construction on every reachable path and so could
    // never fail.)
    assert!(
        v["program_after"].is_string(),
        "repair did not verify the installed program: {v}"
    );
    assert_eq!(
        v["activation_skipped"], true,
        "the harness sets AINB_TIMER_SKIP_ACTIVATION, so the JSON must say the \
         unit was written but not loaded: {v}"
    );
    for unit in unit_files(&home, "tower") {
        assert!(
            unit.is_file(),
            "repair did not reinstall {}",
            unit.display()
        );
    }

    let _ = std::fs::remove_dir_all(&home);
}

// --- T3 (the core case) -----------------------------------------------------

#[test]
fn repair_rewrites_a_broken_unit_against_the_current_path() {
    let home = home_for("rewrite");
    let good = ainb_bin_str();
    let bad = "/nonexistent/ainb";

    let out = setup(&home, bad, "tower", &[]);
    assert!(
        out.status.success(),
        "setup failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Assert the PRECONDITION: the timer really is broken before repair runs,
    // otherwise the post-condition below proves nothing.
    let before = run(
        &home,
        bad,
        &real_path(),
        &["--format", "json", "fleet", "atc", "status", "tower"],
    );
    let before = json_of(&before);
    assert!(
        before["timer_program_problem"].is_string(),
        "precondition failed, the timer was never broken: {before}"
    );

    // While it is still broken, pin the hint change: status must point at
    // repair, not at setup (which resets meta.json, re-enables a disabled
    // heartbeat, rewrites the hooks, and spawns a session).
    let hint = run(
        &home,
        bad,
        &real_path(),
        &["fleet", "atc", "status", "tower"],
    );
    let hint = String::from_utf8_lossy(&hint.stdout).to_string();
    assert!(
        hint.contains("ainb fleet atc repair"),
        "status does not point at repair: {hint}"
    );
    assert!(
        !hint.contains("atc setup tower` to repoint"),
        "status still recommends the destructive setup path: {hint}"
    );

    let out = run(
        &home,
        &good,
        &real_path(),
        &["--format", "json", "fleet", "atc", "repair", "tower"],
    );
    assert!(
        out.status.success(),
        "repair failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = json_of(&out);
    assert_eq!(v["result"], "repaired");
    assert_eq!(v["changed"], true, "a rewritten unit is a change: {v}");

    let after = run(
        &home,
        &good,
        &real_path(),
        &["--format", "json", "fleet", "atc", "status", "tower"],
    );
    let after = json_of(&after);
    assert!(
        after["timer_program_problem"].is_null(),
        "the timer is still broken after repair: {after}"
    );

    let unit = std::fs::read_to_string(health_unit(&home, "tower")).expect("read unit");
    assert!(
        unit.contains(&good),
        "unit does not name the repaired binary: {unit}"
    );
    assert!(
        !unit.contains(bad),
        "unit still names the stale binary: {unit}"
    );

    let _ = std::fs::remove_dir_all(&home);
}

// --- T4 (anti-lie) ----------------------------------------------------------

#[test]
fn repair_refuses_when_the_binary_is_nowhere() {
    let home = home_for("nowhere");
    let good = ainb_bin_str();

    let out = setup(&home, &good, "tower", &[]);
    assert!(
        out.status.success(),
        "setup failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let unit_path = health_unit(&home, "tower");
    let snapshot = std::fs::read(&unit_path).expect("snapshot unit");

    let out = run(
        &home,
        "/nonexistent/ainb",
        "/nonexistent",
        &["--format", "json", "fleet", "atc", "repair", "tower"],
    );

    assert!(
        !out.status.success(),
        "repair claimed success with an unrunnable binary: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("AINB_BIN"),
        "the refusal must say how to pin a path: {stderr}"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("\"result\""),
        "a refused repair must emit no result: {stdout}"
    );
    assert_eq!(
        std::fs::read(&unit_path).expect("re-read unit"),
        snapshot,
        "a refused repair overwrote a working unit with a dead one"
    );

    let _ = std::fs::remove_dir_all(&home);
}

// --- T5 (the reason the verb exists) ----------------------------------------

#[test]
fn repair_never_rewrites_meta() {
    let home = home_for("meta");
    let bin = ainb_bin_str();

    let out = setup(
        &home,
        &bin,
        "tower",
        &["--interval", "17", "--idle-pause", "5"],
    );
    assert!(
        out.status.success(),
        "setup failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let meta_path = home.join("atc").join("tower").join("meta.json");
    let snapshot = std::fs::read(&meta_path).expect("snapshot meta");

    let out = run(
        &home,
        &bin,
        &real_path(),
        &["--format", "json", "fleet", "atc", "repair", "tower"],
    );
    assert!(
        out.status.success(),
        "repair failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert_eq!(
        std::fs::read(&meta_path).expect("re-read meta"),
        snapshot,
        "repair rewrote meta.json (did it route through setup?)"
    );

    let status = run(
        &home,
        &bin,
        &real_path(),
        &["--format", "json", "fleet", "atc", "status", "tower"],
    );
    let status = json_of(&status);
    assert_eq!(
        status["heartbeat_interval_min"], 17,
        "custom interval reset: {status}"
    );
    assert_eq!(
        status["idle_pause_min"], 5,
        "custom idle-pause reset: {status}"
    );

    let _ = std::fs::remove_dir_all(&home);
}

// --- T6 ---------------------------------------------------------------------

#[test]
fn repair_removes_a_stray_unit_when_the_heartbeat_is_disabled() {
    let home = home_for("disabled");
    let bin = ainb_bin_str();

    assert!(setup(&home, &bin, "tower", &[]).status.success());
    for unit in unit_files(&home, "tower") {
        assert!(unit.is_file(), "setup did not install {}", unit.display());
    }

    // `setup --no-heartbeat` on a provisioned instance skips the whole install
    // block and leaves the old unit on disk, still firing. That stray unit is
    // exactly what repair is expected to remove.
    assert!(setup(&home, &bin, "tower", &["--no-heartbeat"]).status.success());
    let expected_removed = unit_files(&home, "tower").iter().filter(|p| p.exists()).count();
    assert!(
        expected_removed > 0,
        "premise broken: --no-heartbeat cleaned the unit up on its own"
    );

    let out = run(
        &home,
        &bin,
        &real_path(),
        &["--format", "json", "fleet", "atc", "repair", "tower"],
    );
    assert!(
        out.status.success(),
        "repair failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = json_of(&out);
    assert_eq!(v["result"], "disabled");
    assert_eq!(v["scheduler"], "none");
    assert_eq!(
        v["timer_units_removed"].as_array().map(Vec::len),
        Some(expected_removed),
        "wrong number of units removed: {v}"
    );
    assert!(
        v["timer_units"].as_array().is_some_and(Vec::is_empty),
        "repair installed a unit for a disabled heartbeat: {v}"
    );
    for unit in unit_files(&home, "tower") {
        assert!(
            !unit.exists(),
            "stray unit survived repair: {}",
            unit.display()
        );
    }

    let _ = std::fs::remove_dir_all(&home);
}

// --- T7 ---------------------------------------------------------------------

#[test]
fn repair_is_idempotent() {
    let home = home_for("idempotent");
    let bin = ainb_bin_str();

    assert!(setup(&home, &bin, "tower", &[]).status.success());

    let args = ["--format", "json", "fleet", "atc", "repair", "tower"];
    let first = run(&home, &bin, &real_path(), &args);
    assert!(
        first.status.success(),
        "first repair failed: {}",
        String::from_utf8_lossy(&first.stderr)
    );

    let second = run(&home, &bin, &real_path(), &args);
    assert!(
        second.status.success(),
        "second repair failed: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    let v = json_of(&second);
    assert_eq!(v["result"], "repaired");
    assert_eq!(
        v["changed"], false,
        "repair claimed a rewrite it did not make: {v}"
    );

    let _ = std::fs::remove_dir_all(&home);
}

// --- T8 ---------------------------------------------------------------------

#[test]
fn repair_dry_run_writes_nothing_and_keeps_exit_semantics() {
    let home = home_for("dry-run");
    let good = ainb_bin_str();
    let bad = "/nonexistent/ainb";

    // (a) A broken unit plus a findable binary: dry-run reports the repair it
    // would make, and changes nothing on disk.
    assert!(setup(&home, bad, "tower", &[]).status.success());
    let unit_path = health_unit(&home, "tower");
    let snapshot = std::fs::read(&unit_path).expect("snapshot unit");

    let out = run(
        &home,
        &good,
        &real_path(),
        &[
            "--format",
            "json",
            "fleet",
            "atc",
            "repair",
            "tower",
            "--dry-run",
        ],
    );
    assert!(
        out.status.success(),
        "dry-run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = json_of(&out);
    assert_eq!(v["dry_run"], true);
    assert_eq!(v["result"], "repaired");
    assert_eq!(v["changed"], true, "dry-run missed the pending change: {v}");
    assert!(
        v["program_after"].is_null(),
        "dry-run verified a unit it never wrote: {v}"
    );
    // Dry-run attempts no daemon call, so neither outcome is KNOWN. Reporting a
    // bool here would be a guess dressed as a fact, and it previously made the
    // preview disagree with the real run.
    assert!(
        v["daemon_registered"].is_null() && v["daemon_unregistered"].is_null(),
        "dry-run guessed a daemon outcome it never attempted: {v}"
    );
    assert_eq!(
        std::fs::read(&unit_path).expect("re-read unit"),
        snapshot,
        "dry-run mutated the unit"
    );

    let text = run(
        &home,
        &good,
        &real_path(),
        &["fleet", "atc", "repair", "tower", "--dry-run"],
    );
    let text = String::from_utf8_lossy(&text.stdout).to_string();
    assert!(
        text.contains("Re-run without --dry-run"),
        "dry-run did not say how to apply: {text}"
    );
    assert_eq!(
        std::fs::read(&unit_path).expect("re-read unit"),
        snapshot,
        "dry-run mutated the unit"
    );

    // (b) Same instance, unfindable binary: the exit code must match the real
    // run's, so `repair --dry-run` is usable as a CI / agent health gate.
    let out = run(
        &home,
        bad,
        "/nonexistent",
        &[
            "--format",
            "json",
            "fleet",
            "atc",
            "repair",
            "tower",
            "--dry-run",
        ],
    );
    assert!(
        !out.status.success(),
        "dry-run exited 0 on an unrepairable instance, so it cannot gate: {}",
        String::from_utf8_lossy(&out.stdout)
    );

    let _ = std::fs::remove_dir_all(&home);
}
